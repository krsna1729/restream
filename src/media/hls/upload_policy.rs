//! What an HLS PUT output uploads, in what order, and what it does on each
//! response. Pure: no I/O, no clock, no channel. Each transport (the Reqwest
//! uploader on Tokio, the egress-shard backend) feeds it publishes, results
//! and the current time, and performs the request it returns, so every
//! transport follows the same ingest rules:
//!
//! - Output-local numbering from 0 (YouTube: the first Media Playlist starts
//!   at sequence 0), names `{session}-{n}.ts` with a per-attempt session
//!   token, so names never repeat across restarts (YouTube, Akamai).
//! - A late-starting output begins at the newest segment, not the store's
//!   backlog.
//! - Segment first, then a playlist of acknowledged segments only, once per
//!   segment (YouTube, Akamai), so no listed segment is ever outstanding
//!   (YouTube allows at most five).
//! - 2xx acknowledges. 400/401/403/404/405 and other client errors stop the
//!   output (Akamai: do not retry; YouTube 401: expired `cid`). 5xx, 408,
//!   425, 429 and transport failures back off exponentially; a segment is
//!   retried for one segment duration, then dropped (Akamai), and the
//!   playlist window restarts after the gap; the playlist is retried without
//!   limit, always as its newest version.
//! - An intentional stop sends one final playlist with `EXT-X-ENDLIST`
//!   (Akamai).
use std::collections::VecDeque;
use std::fmt::Write as _;
use std::time::{Duration, Instant};

use bytes::Bytes;

use super::HlsStoreSnapshot;

pub(crate) const HLS_PLAYLIST_CONTENT_TYPE: &str = "application/vnd.apple.mpegurl";
pub(crate) const HLS_SEGMENT_CONTENT_TYPE: &str = "video/mp2t";

/// Acknowledged segments each playlist lists (YouTube's example lists 3).
const PLAYLIST_WINDOW: usize = 3;
const BACKOFF_FIRST: Duration = Duration::from_millis(200);
const BACKOFF_MAX: Duration = Duration::from_secs(5);
/// A segment is retried at least this long even when it is shorter.
const MIN_SEGMENT_RETRY: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum UploadTarget {
    Segment {
        sequence: u64,
    },
    Playlist {
        last_sequence: Option<u64>,
        end: bool,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct UploadRequest {
    pub(crate) target: UploadTarget,
    /// `file=` value or path suffix for this object.
    pub(crate) file_name: String,
    pub(crate) content_type: &'static str,
    pub(crate) body: Bytes,
    /// The previous attempt failed: use a new connection, resolving the host
    /// again (Akamai re-resolves DNS on every retry).
    pub(crate) fresh_connection: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UploadOutcome {
    Status(u16),
    /// Connect, send, receive or timeout failure: no status was received.
    Transport,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Next {
    Put(UploadRequest),
    /// Nothing to send before this instant (a backoff), or until the next
    /// publish when `None`.
    Wait(Option<Instant>),
    /// The output is finished: rejected by the server, or ended.
    Stop(Stopped),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stopped {
    Rejected { status: u16 },
    Ended,
}

/// What a result did, for the transport's status reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResultEffect {
    Acknowledged {
        bytes: u64,
    },
    WillRetry {
        attempts: u32,
    },
    Rejected {
        status: u16,
    },
    /// The end playlist failed; it is not retried and the output ends.
    EndNotDelivered,
}

struct PendingSegment {
    sequence: u64,
    duration: f64,
    data: Bytes,
    first_attempt: Option<Instant>,
}

struct AckedSegment {
    sequence: u64,
    duration: f64,
}

/// The request awaiting its result, with what acknowledging it needs (the
/// segment may have left `pending` through `finish`).
struct InFlight {
    target: UploadTarget,
    duration: f64,
    bytes: u64,
}

pub(crate) struct UploadPolicy {
    session: String,
    /// Highest store index seen; `None` before the first publish.
    last_store_index: Option<u64>,
    next_sequence: u64,
    pending: VecDeque<PendingSegment>,
    window: VecDeque<AckedSegment>,
    /// Never decreases (HLS: the target duration must not change).
    target_duration: u64,
    playlist_dirty: bool,
    in_flight: Option<InFlight>,
    attempts: u32,
    backoff_until: Option<Instant>,
    ending: bool,
    stopped: Option<Stopped>,
    dropped: u64,
}

impl UploadPolicy {
    /// `session` must be unique per output attempt, also across process
    /// restarts, and may hold only `[A-Za-z0-9_-]`.
    pub(crate) fn new(session: String) -> Self {
        Self {
            session,
            last_store_index: None,
            next_sequence: 0,
            pending: VecDeque::new(),
            window: VecDeque::new(),
            target_duration: 1,
            playlist_dirty: false,
            in_flight: None,
            attempts: 0,
            backoff_until: None,
            ending: false,
            stopped: None,
            dropped: 0,
        }
    }

    /// `finish` was called: only the end playlist remains.
    pub(crate) fn is_finishing(&self) -> bool {
        self.ending
    }

    pub(crate) fn dropped_segments(&self) -> u64 {
        self.dropped
    }

    /// Take the segments this output has not seen. The first publish yields
    /// only its newest segment. A store that was cleared (its indices went
    /// backwards) yields all of its segments.
    pub(crate) fn on_publish(&mut self, snapshot: &HlsStoreSnapshot) {
        if self.ending || self.stopped.is_some() {
            return;
        }
        let Some(newest) = snapshot.segments.iter().map(|segment| segment.index).max() else {
            return;
        };
        let fresh: Vec<_> = match self.last_store_index {
            None => snapshot
                .segments
                .iter()
                .filter(|segment| segment.index == newest)
                .collect(),
            Some(last) if newest < last => snapshot.segments.iter().collect(),
            Some(last) => snapshot
                .segments
                .iter()
                .filter(|segment| segment.index > last)
                .collect(),
        };
        self.last_store_index = Some(newest);
        for segment in fresh {
            self.pending.push_back(PendingSegment {
                sequence: self.next_sequence,
                duration: segment.duration,
                data: segment.data.clone(),
                first_attempt: None,
            });
            self.next_sequence = self.next_sequence.saturating_add(1);
        }
    }

    /// Stop after one final playlist with `EXT-X-ENDLIST`; nothing more is
    /// uploaded.
    pub(crate) fn finish(&mut self) {
        self.ending = true;
        self.pending.clear();
        self.backoff_until = None;
    }

    /// The next request, or why there is none. One request is in flight at
    /// a time (one persistent connection); while one is, this waits.
    pub(crate) fn next(&mut self, now: Instant) -> Next {
        if let Some(stopped) = self.stopped {
            return Next::Stop(stopped);
        }
        if self.in_flight.is_some() {
            return Next::Wait(None);
        }
        if let Some(until) = self.backoff_until.filter(|until| *until > now) {
            return Next::Wait(Some(until));
        }
        // YouTube: a playlist after every segment, also when catching up.
        if self.playlist_dirty || (self.ending && !self.window.is_empty()) {
            return Next::Put(self.playlist_request());
        }
        while let Some(front) = self.pending.front_mut() {
            let deadline = front
                .first_attempt
                .map(|first| first + retry_window(front.duration));
            if deadline.is_some_and(|deadline| now >= deadline) {
                // Akamai: past one segment duration, drop it and move on.
                // The playlist window restarts after the gap, so it never
                // lists the missing segment.
                self.pending.pop_front();
                self.window.clear();
                self.attempts = 0;
                self.dropped = self.dropped.saturating_add(1);
                continue;
            }
            // The retry window counts from the first send.
            front.first_attempt.get_or_insert(now);
            let target = UploadTarget::Segment {
                sequence: front.sequence,
            };
            let (sequence, duration, body) = (front.sequence, front.duration, front.data.clone());
            self.in_flight = Some(InFlight {
                target: target.clone(),
                duration,
                bytes: body.len() as u64,
            });
            return Next::Put(UploadRequest {
                target,
                file_name: self.segment_name(sequence),
                content_type: HLS_SEGMENT_CONTENT_TYPE,
                body,
                fresh_connection: self.attempts > 0,
            });
        }
        if self.ending {
            self.stopped = Some(Stopped::Ended);
            return Next::Stop(Stopped::Ended);
        }
        Next::Wait(None)
    }

    fn playlist_request(&mut self) -> UploadRequest {
        let end = self.ending;
        let target = UploadTarget::Playlist {
            last_sequence: self.window.back().map(|segment| segment.sequence),
            end,
        };
        let body = Bytes::from(self.render_playlist(end));
        self.in_flight = Some(InFlight {
            target: target.clone(),
            duration: 0.0,
            bytes: body.len() as u64,
        });
        UploadRequest {
            target,
            file_name: String::new(),
            content_type: HLS_PLAYLIST_CONTENT_TYPE,
            body,
            fresh_connection: self.attempts > 0,
        }
    }

    /// Apply the result of the request in flight; `None` when there is none.
    pub(crate) fn on_result(
        &mut self,
        outcome: UploadOutcome,
        now: Instant,
    ) -> Option<ResultEffect> {
        let InFlight {
            target,
            duration,
            bytes,
        } = self.in_flight.take()?;
        Some(match classify(outcome) {
            Class::Acknowledged => {
                self.attempts = 0;
                self.backoff_until = None;
                match target {
                    UploadTarget::Segment { sequence } => {
                        if self
                            .pending
                            .front()
                            .is_some_and(|front| front.sequence == sequence)
                        {
                            self.pending.pop_front();
                        }
                        self.target_duration = self.target_duration.max(duration.ceil() as u64);
                        self.window.push_back(AckedSegment { sequence, duration });
                        while self.window.len() > PLAYLIST_WINDOW {
                            self.window.pop_front();
                        }
                        self.playlist_dirty = true;
                        ResultEffect::Acknowledged { bytes }
                    }
                    UploadTarget::Playlist { end, .. } => {
                        self.playlist_dirty = false;
                        if end {
                            self.stopped = Some(Stopped::Ended);
                        }
                        ResultEffect::Acknowledged { bytes }
                    }
                }
            }
            Class::Rejected(status) => {
                self.stopped = Some(Stopped::Rejected { status });
                ResultEffect::Rejected { status }
            }
            Class::Retry => {
                if matches!(target, UploadTarget::Playlist { end: true, .. }) {
                    // The end marker is best effort: one attempt.
                    self.stopped = Some(Stopped::Ended);
                    return Some(ResultEffect::EndNotDelivered);
                }
                self.attempts = self.attempts.saturating_add(1);
                self.backoff_until = Some(now + backoff(self.attempts));
                ResultEffect::WillRetry {
                    attempts: self.attempts,
                }
            }
        })
    }

    fn segment_name(&self, sequence: u64) -> String {
        format!("{}-{sequence}.ts", self.session)
    }

    fn render_playlist(&self, end: bool) -> String {
        let first = self.window.front().map_or(0, |segment| segment.sequence);
        let mut playlist = format!(
            "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:{}\n#EXT-X-MEDIA-SEQUENCE:{first}\n",
            self.target_duration
        );
        for segment in &self.window {
            let _ = write!(
                playlist,
                "#EXTINF:{:.3},\n{}\n",
                segment.duration,
                self.segment_name(segment.sequence)
            );
        }
        if end {
            playlist.push_str("#EXT-X-ENDLIST\n");
        }
        playlist
    }
}

enum Class {
    Acknowledged,
    Rejected(u16),
    Retry,
}

fn classify(outcome: UploadOutcome) -> Class {
    match outcome {
        UploadOutcome::Status(200..=299) => Class::Acknowledged,
        UploadOutcome::Status(408 | 425 | 429) => Class::Retry,
        UploadOutcome::Status(status @ 400..=499) => Class::Rejected(status),
        UploadOutcome::Status(_) | UploadOutcome::Transport => Class::Retry,
    }
}

pub(crate) fn backoff(attempts: u32) -> Duration {
    let doublings = attempts.saturating_sub(1).min(16);
    BACKOFF_FIRST
        .saturating_mul(1u32 << doublings)
        .min(BACKOFF_MAX)
}

fn retry_window(duration: f64) -> Duration {
    Duration::try_from_secs_f64(duration)
        .unwrap_or(MIN_SEGMENT_RETRY)
        .max(MIN_SEGMENT_RETRY)
}

#[cfg(test)]
#[path = "upload_policy_tests.rs"]
mod tests;
