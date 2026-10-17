//! One media playlist a pull follows: the chosen variant's, and the separate
//! audio rendition's when the variant's audio is one. Each has its own
//! sequence cursor, retries and map, and its own namespace in the stream key
//! (`pull/` and `pull/a/`), so the assembler sees two renditions as it would
//! from an encoder that pushes them.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use reqwest::{Client, Url};
use tracing::{debug, warn};

use super::namespace::{live_start, map_name, object_kind, segment_name};
use super::{PullError, PullState, SEGMENT_ATTEMPTS};
use crate::media::hls_ingest::playlist::{MediaPlaylist, SegmentEntry};
use crate::media::hls_ingest::{HlsIngest, HlsIngestLimits, ObjectKind, ObjectOutcome};
use crate::media::hls_pull::fetch::log_url;

/// Where a followed playlist and its objects go in the stream key's
/// namespace, and how far it has been fetched.
pub(super) struct Follow {
    /// The prefix of every object dispatched for it.
    pub(super) dir: &'static str,
    /// The path its rewritten playlist is dispatched under.
    pub(super) playlist: &'static str,
    /// First sequence not yet fetched, and the first sequence the previous
    /// playlist listed (to notice a restarted encoder).
    pub(super) next_sequence: Option<u64>,
    pub(super) last_media_sequence: Option<u64>,
    /// The map identity in force: the object name of the `EXT-X-MAP` whose
    /// init this task already handed to the session. Only the current one is
    /// kept, because a map *change* is what the assembler must see; the
    /// history of maps is worth nothing and would grow one `String` per
    /// playlist reload.
    pub(super) current_map: Option<String>,
    /// Failed attempts per sequence not yet skipped, so one persistently
    /// missing object cannot stall the pull until it leaves the window.
    pub(super) segment_tries: HashMap<u64, u32>,
}

impl Follow {
    pub(super) fn new(dir: &'static str, playlist: &'static str) -> Self {
        Self {
            dir,
            playlist,
            next_sequence: None,
            last_media_sequence: None,
            current_map: None,
            segment_tries: HashMap::new(),
        }
    }
}

/// The separate audio rendition the pull follows, and its playlist URL (a
/// signed origin re-signs it on every master resolution).
pub(super) struct AudioFollow {
    pub(super) url: Url,
    pub(super) follow: Follow,
    /// Cycles in a row whose audio part failed.
    pub(super) failures: u32,
}

/// What one followed playlist's cycle found.
pub(super) struct Followed {
    pub(super) had_new: bool,
    pub(super) ended: bool,
    pub(super) target: Duration,
}

impl PullState {
    /// Apply a fetched media playlist: dispatch its rewritten text, then fetch
    /// and hand over its new segments in order.
    pub(super) async fn follow_media(
        &mut self,
        follow: &mut Follow,
        client: &Client,
        ingest: &Arc<HlsIngest>,
        limits: &HlsIngestLimits,
        media: MediaPlaylist,
        text: String,
    ) -> Result<Followed, PullError> {
        self.dispatch_playlist(ingest, text.into_bytes(), follow.playlist)
            .await?;

        if media.media_sequence < follow.last_media_sequence.unwrap_or(media.media_sequence) {
            // The origin restarted its numbering; follow the new run.
            debug!(
                pipeline = %self.pipeline.id,
                input = %self.pipeline.input_id,
                sequence = media.media_sequence,
                "HLS pull media sequence restarted"
            );
            follow.next_sequence = Some(media.media_sequence);
            follow.segment_tries.clear();
        }
        follow.last_media_sequence = Some(media.media_sequence);

        // The first cycle of a live playlist starts near the end: RFC 8216
        // section 6.3.3 has a client enter no less than three target
        // durations from it, so a DVR or EVENT window is not read from its
        // start and paced in real time — a whole window behind live, with the
        // backfill loaded onto the origin.
        let mut next = follow.next_sequence.unwrap_or_else(|| {
            if media.ended {
                media.media_sequence
            } else {
                live_start(&media)
            }
        });
        let first_new = media
            .segments
            .iter()
            .find(|segment| segment.sequence >= next);
        if let Some(first) = first_new
            && first.sequence > next
        {
            // The window slid past sequences this task never saw.
            debug!(
                pipeline = %self.pipeline.id,
                input = %self.pipeline.input_id,
                skipped = first.sequence - next,
                "HLS pull playlist moved past segments"
            );
            next = first.sequence;
            follow.next_sequence = Some(next);
            follow.segment_tries.retain(|sequence, _| *sequence >= next);
        }

        let mut had_new = false;
        for segment in media
            .segments
            .iter()
            .filter(|segment| segment.sequence >= next)
        {
            match self
                .fetch_segment(follow, client, ingest, limits, segment)
                .await
            {
                Ok(()) => {
                    follow.segment_tries.remove(&segment.sequence);
                    follow.next_sequence = Some(segment.sequence.saturating_add(1));
                    had_new = true;
                }
                // One object that will not fetch must not stall the pull
                // until it leaves the window: after a few tries it is
                // skipped, and the gap it leaves is the assembler's to
                // account for, as a push upload that never arrives is.
                Err(PullError::Retry(reason)) => {
                    let tries = {
                        let tries = follow.segment_tries.entry(segment.sequence).or_insert(0);
                        *tries = tries.saturating_add(1);
                        *tries
                    };
                    if tries < SEGMENT_ATTEMPTS {
                        return Err(PullError::Retry(reason));
                    }
                    warn!(
                        pipeline = %self.pipeline.id,
                        input = %self.pipeline.input_id,
                        sequence = segment.sequence,
                        url = %log_url(&segment.path),
                        tries,
                        %reason,
                        "HLS pull skipped a segment that would not fetch"
                    );
                    follow.segment_tries.remove(&segment.sequence);
                    follow.next_sequence = Some(segment.sequence.saturating_add(1));
                }
                Err(error) => return Err(error),
            }
        }

        Ok(Followed {
            had_new,
            ended: media.ended,
            target: media.target_duration,
        })
    }

    async fn fetch_segment(
        &mut self,
        follow: &mut Follow,
        client: &Client,
        ingest: &Arc<HlsIngest>,
        limits: &HlsIngestLimits,
        segment: &SegmentEntry,
    ) -> Result<(), PullError> {
        let kind = object_kind(segment);
        if kind == ObjectKind::Fmp4
            && let Some(map) = &segment.map_uri
        {
            // Only a *change* of map identity needs the init sent again: a
            // signed origin re-signs the same `EXT-X-MAP` URI on every
            // reload, and the query is the credential, not the identity.
            let name = map_name(map);
            if follow.current_map.as_deref() != Some(name.as_str()) {
                self.fetch_init(client, ingest, limits, map, follow.dir)
                    .await?;
                follow.current_map = Some(name);
            }
        }
        let url = Url::parse(&segment.path).map_err(|error| {
            PullError::Retry(format!("HLS segment URL does not parse: {error}"))
        })?;
        let path = format!("{}/{}", follow.dir, segment_name(segment.sequence, kind));
        let outcome = self
            .fetch_object(client, ingest, &url, kind.max_bytes(limits), kind, path)
            .await?;
        match outcome {
            ObjectOutcome::Stored | ObjectOutcome::Held => {
                // A refusal said its reason; a delivered segment means the
                // reason is old news, so the next one logs again.
                self.last_refused = None;
                Ok(())
            }
            ObjectOutcome::Dropped => {
                debug!(
                    pipeline = %self.pipeline.id,
                    input = %self.pipeline.input_id,
                    "HLS pull segment was already taken"
                );
                Ok(())
            }
            ObjectOutcome::Retry => Err(PullError::Retry(
                "HLS ingest ended under the pulled segment".to_string(),
            )),
            ObjectOutcome::Malformed(reason)
            | ObjectOutcome::Unsupported(reason)
            | ObjectOutcome::TooLarge(reason) => {
                // The same bytes cannot become valid on a retry; skip the
                // segment and let ordering count the gap. One reason repeats
                // for every segment of a playlist the assembler will not take
                // (a ladder whose top variant is HEVC, say), so it is logged
                // once, as the retry reason is; a segment that is delivered
                // clears it, so a later refusal logs again.
                if self.last_refused.as_deref() != Some(reason.as_str()) {
                    warn!(
                        pipeline = %self.pipeline.id,
                        input = %self.pipeline.input_id,
                        url = %log_url(url.as_str()),
                        %reason,
                        "HLS pull segment refused"
                    );
                    self.last_refused = Some(reason);
                }
                Ok(())
            }
        }
    }

    async fn fetch_init(
        &mut self,
        client: &Client,
        ingest: &Arc<HlsIngest>,
        limits: &HlsIngestLimits,
        map: &str,
        dir: &str,
    ) -> Result<(), PullError> {
        let url = Url::parse(map)
            .map_err(|error| PullError::Retry(format!("HLS map URL does not parse: {error}")))?;
        let path = format!("{dir}/{}", map_name(map));
        let outcome = self
            .fetch_object(
                client,
                ingest,
                &url,
                limits.max_init_bytes,
                ObjectKind::Fmp4,
                path,
            )
            .await?;
        match outcome {
            ObjectOutcome::Stored | ObjectOutcome::Held | ObjectOutcome::Dropped => Ok(()),
            ObjectOutcome::Retry => Err(PullError::Retry(
                "HLS ingest ended under the pulled init segment".to_string(),
            )),
            ObjectOutcome::Malformed(reason)
            | ObjectOutcome::Unsupported(reason)
            | ObjectOutcome::TooLarge(reason) => {
                warn!(
                    pipeline = %self.pipeline.id,
                    input = %self.pipeline.input_id,
                    url = %log_url(url.as_str()),
                    %reason,
                    "HLS pull init segment refused"
                );
                Ok(())
            }
        }
    }
}
