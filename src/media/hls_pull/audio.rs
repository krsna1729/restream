//! What the chosen variant says about audio, and what the pull does about it.
//!
//! The puller follows one rendition, so a master whose audio is a separate
//! `EXT-X-MEDIA` rendition — packaged live HLS, and Restream's own preview —
//! is pulled video-only. That is a known gap (Q-077), and it is stated once
//! per pull instead of being dropped silently.
//!
//! The decision is separate from the pull loop so it can be read and tested
//! on its own: the loop only calls [`report_audio_rendition`] where a master
//! is resolved.

use tracing::warn;

use crate::media::hls_ingest::playlist::{MasterPlaylist, Variant};
use crate::media::ingest_auth::AuthenticatedPipeline;
use crate::secret_display::redact_url;

/// What the chosen variant says about audio, and what this puller does about
/// it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum AudioPlan {
    /// The variant carries its own audio: it names no `AUDIO` group, or the
    /// member the group selects has no `URI` (RFC 8216 section 4.3.4.1).
    /// Nothing is lost, so nothing is logged.
    Muxed,
    /// The group's selected member is a separate media playlist.
    Separate {
        group: String,
        name: String,
        url: String,
    },
    /// The variant names a group the master does not define: a broken master,
    /// stated as the reason the pull is video-only.
    MissingGroup { group: String },
}

impl AudioPlan {
    /// The one line a pull logs about this plan: the stated reason the audio
    /// is not followed, or `None` when the variant carries its own audio.
    pub(super) fn reason(&self) -> Option<String> {
        match self {
            Self::Muxed => None,
            Self::Separate { group, name, .. } => Some(format!(
                "HLS pull follows the video variant only: the audio is the separate \
                 EXT-X-MEDIA rendition {name:?} of group {group:?}, which this puller does \
                 not follow yet; the pulled stream is video-only"
            )),
            Self::MissingGroup { group } => Some(format!(
                "HLS pull follows the video variant only: the master defines no audio \
                 rendition for the group {group:?} the chosen variant names"
            )),
        }
    }
}

/// The audio plan of the chosen variant. The member is chosen by
/// [`MasterPlaylist::audio_rendition`] (`DEFAULT`, else `AUTOSELECT`, else the
/// first of the group).
pub(super) fn audio_plan(master: &MasterPlaylist, variant: &Variant) -> AudioPlan {
    let Some(group) = variant.audio_group.as_deref() else {
        // No `AUDIO` attribute: the variant's own segments carry its audio.
        return AudioPlan::Muxed;
    };
    let Some(rendition) = master.audio_rendition(variant) else {
        return AudioPlan::MissingGroup {
            group: group.to_string(),
        };
    };
    match &rendition.path {
        // A member with no `URI` is muxed into the group's variants.
        None => AudioPlan::Muxed,
        Some(url) => AudioPlan::Separate {
            group: group.to_string(),
            // A member may omit `NAME`; its URI then names it.
            name: match rendition.name.as_str() {
                "" => redact_url(url),
                name => name.to_string(),
            },
            url: url.clone(),
        },
    }
}

/// State what the pull does with the audio the chosen variant does not carry
/// itself. Called once, where a master playlist is resolved, so a pull logs at
/// most one line about it; the pull continues either way.
pub(super) fn report_audio_rendition(
    master: &MasterPlaylist,
    variant: &Variant,
    pipeline: &AuthenticatedPipeline,
) {
    let plan = audio_plan(master, variant);
    let Some(reason) = plan.reason() else {
        return;
    };
    match &plan {
        AudioPlan::Separate { group, name, url } => warn!(
            pipeline = %pipeline.id,
            input = %pipeline.input_id,
            group = %group,
            name = %name,
            uri = %redact_url(url),
            "{}", reason
        ),
        AudioPlan::MissingGroup { group } => warn!(
            pipeline = %pipeline.id,
            input = %pipeline.input_id,
            group = %group,
            "{}", reason
        ),
        AudioPlan::Muxed => {}
    }
}
