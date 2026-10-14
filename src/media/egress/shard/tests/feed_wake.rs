use super::super::*;
use super::support::{BlockingBackend, Gate, config, output_spec};
use crate::media::egress::feed::EgressFeed;
use crate::media::egress::journal::{FeedEpoch, TsFeed};
use crate::media::ts_chunk_ring::TsChunkRing;
use bytes::Bytes;
use std::sync::{Arc, mpsc};
use tokio_util::sync::CancellationToken;

/// Observe real feed heads at the backend's wake callback, pausing the
/// first callback through the existing blocked-backend test seam. The gate
/// and command-channel implementation under test is the production shard.
struct HeadObserver {
    feeds: [TsFeed; 2],
    observed: mpsc::Sender<[u64; 2]>,
    block: BlockingBackend,
    block_on_wake: bool,
    blocked: bool,
}

impl EgressShardBackend for HeadObserver {
    fn on_command(&mut self, command: EgressCommand) -> EgressShardCommandEffect {
        let is_wake = matches!(command, EgressCommand::FeedWake);
        if is_wake {
            self.observed
                .send(self.feeds.each_ref().map(EgressFeed::head_sequence))
                .unwrap();
        }
        if !self.blocked && is_wake == self.block_on_wake {
            self.blocked = true;
            self.block.on_command(command);
        }
        EgressShardCommandEffect::Continue
    }
}

fn fixture(
    block_on_wake: bool,
) -> (
    EgressShardHandle,
    [TsChunkRing; 2],
    Gate,
    mpsc::Receiver<[u64; 2]>,
) {
    let rings = std::array::from_fn(|_| TsChunkRing::new(8, CancellationToken::new()));
    let feeds = rings
        .each_ref()
        .map(|ring| TsFeed::new(ring, Arc::new(FeedEpoch::new())));
    let gate = Gate::default();
    let (observed, received) = mpsc::channel();
    let handle = EgressShardHandle::spawn(
        ShardId::new(0),
        config(1, 1),
        HeadObserver {
            feeds,
            observed,
            block: BlockingBackend { gate: gate.clone() },
            block_on_wake,
            blocked: false,
        },
    );
    (handle, rings, gate, received)
}

#[test]
fn publication_after_a_wake_head_check_rearms_the_shared_shard_gate() {
    let (handle, rings, gate, received) = fixture(true);
    rings[0].push(Bytes::from_static(b"first-feed"), true);
    handle.deliver_feed_wake().unwrap();
    gate.wait_until_entered();
    assert_eq!(
        received.recv_timeout(Duration::from_secs(2)).unwrap(),
        [1, 0]
    );

    // The first feed's head has already been checked. A publication on a
    // different feed must rearm a wake, not coalesce into that past check.
    rings[1].push(Bytes::from_static(b"second-feed"), true);
    handle.deliver_feed_wake().unwrap();
    gate.release();
    let next = received.recv_timeout(Duration::from_secs(2));
    handle.shutdown_and_join();
    assert_eq!(next.unwrap(), [1, 1]);
}

#[test]
fn a_full_command_queue_cannot_strand_a_feeds_final_publication() {
    let (handle, rings, gate, received) = fixture(false);
    handle
        .try_send(EgressCommand::Add(output_spec("block")))
        .unwrap();
    gate.wait_until_entered();
    handle
        .try_send(EgressCommand::Add(output_spec("fill")))
        .unwrap();

    rings[1].push(Bytes::from_static(b"final-publication"), true);
    assert_eq!(handle.deliver_feed_wake(), Err(EgressShardSendError::Full));
    gate.release();
    // There is no later publication to retry the hint. The active shard
    // must still observe this head after draining the full command queue.
    let next = received.recv_timeout(Duration::from_secs(2));
    handle.shutdown_and_join();
    assert_eq!(next.unwrap(), [0, 1]);
}

/// Pause a whole command batch so a queued wake is still outstanding when
/// the Full fallback runs. Reuse the existing blocked-backend seam at each
/// boundary rather than replacing the production command loop.
struct QueuedWakeObserver {
    heads: HeadObserver,
    startup: Option<BlockingBackend>,
    adds: std::array::IntoIter<BlockingBackend, 2>,
}

impl EgressShardBackend for QueuedWakeObserver {
    fn on_command(&mut self, command: EgressCommand) -> EgressShardCommandEffect {
        if matches!(command, EgressCommand::Add(_))
            && let Some(mut block) = self.adds.next()
        {
            return block.on_command(command);
        }
        self.heads.on_command(command)
    }

    fn on_media_tick(&mut self) -> EgressShardCommandEffect {
        if let Some(mut block) = self.startup.take() {
            block.on_command(EgressCommand::FeedWake);
        }
        EgressShardCommandEffect::Continue
    }
}

#[test]
fn full_fallback_preserves_an_already_queued_publication_wake() {
    let rings = std::array::from_fn(|_| TsChunkRing::new(8, CancellationToken::new()));
    let feeds = rings
        .each_ref()
        .map(|ring| TsFeed::new(ring, Arc::new(FeedEpoch::new())));
    let [startup, first_add, second_add, fallback] = std::array::from_fn(|_| Gate::default());
    let (observed, received) = mpsc::channel();
    let handle = EgressShardHandle::spawn(
        ShardId::new(0),
        config(1, 2),
        QueuedWakeObserver {
            heads: HeadObserver {
                feeds,
                observed,
                block: BlockingBackend {
                    gate: fallback.clone(),
                },
                block_on_wake: true,
                blocked: false,
            },
            startup: Some(BlockingBackend {
                gate: startup.clone(),
            }),
            adds: [first_add.clone(), second_add.clone()]
                .map(|gate| BlockingBackend { gate })
                .into_iter(),
        },
    );
    startup.wait_until_entered();
    handle
        .try_send(EgressCommand::Add(output_spec("first")))
        .unwrap();
    startup.release();
    first_add.wait_until_entered();
    handle
        .try_send(EgressCommand::Add(output_spec("second")))
        .unwrap();
    rings[0].push(Bytes::from_static(b"full-publication"), true);
    assert_eq!(handle.deliver_feed_wake(), Err(EgressShardSendError::Full));
    first_add.release();
    second_add.wait_until_entered();
    rings[1].push(Bytes::from_static(b"queued-publication"), true);
    handle.deliver_feed_wake().unwrap();
    second_add.release();
    fallback.wait_until_entered();
    let before = received.recv_timeout(Duration::from_secs(2));

    // The two-command batch is exhausted: the fallback must leave the queued
    // wake's gate set. This publish coalesces rather than trying the full queue.
    rings[0].push(Bytes::from_static(b"coalesced-publication"), true);
    let coalesced = handle.deliver_feed_wake();
    fallback.release();
    let after = received.recv_timeout(Duration::from_secs(2));
    handle.shutdown_and_join();
    assert_eq!(before.unwrap(), [1, 1]);
    assert_eq!(coalesced, Ok(()));
    assert_eq!(after.unwrap(), [2, 1]);
}
