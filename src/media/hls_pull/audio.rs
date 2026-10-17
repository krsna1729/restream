//! What the chosen variant says about audio, and what the pull does about it.
//!
//! A master whose audio is a separate `EXT-X-MEDIA` rendition — packaged live
//! HLS, and Restream's own preview — has that rendition followed alongside
//! the variant. A variant that names an audio group the master does not
//! define is pulled video-only, and that is stated once per master resolution
//! — the first cycle, and again when an expired variant URL sends the pull
//! back to the master — instead of being dropped silently.
//!
//! The decision is separate from the pull loop so it can be read and tested
//! on its own: the loop only calls [`report_audio_rendition`] where a master
//! is resolved.

use tracing::{info, warn};

use super::fetch::log_url;
use crate::media::hls_ingest::playlist::{MasterPlaylist, Variant};
use crate::media::ingest_auth::AuthenticatedPipeline;

/// What the chosen variant says about audio, and what this puller does about
/// it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum AudioPlan {
    /// The variant carries its own audio: it names no `AUDIO` group, or the
    /// member the group selects has no `URI` (RFC 8216 section 4.3.4.1).
    /// Nothing is lost, so nothing is logged.
    Muxed,
    /// The group's selected member is a separate media playlist: followed.
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
    /// The reason the pulled stream lacks the variant's audio, or `None` when
    /// nothing is lost: the variant carries its own audio, or its separate
    /// rendition is followed.
    pub(super) fn reason(&self) -> Option<String> {
        match self {
            Self::Muxed | Self::Separate { .. } => None,
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
                "" => log_url(url),
                name => name.to_string(),
            },
            url: url.clone(),
        },
    }
}

/// State what the pull does with the audio the chosen variant does not carry
/// itself, and return the plan the pull follows. Called wherever a master
/// playlist is resolved — the first cycle, and again after an expired variant
/// URL sends the pull back to it — so a pull logs one line per resolution,
/// not one per pull; the pull continues either way.
pub(super) fn report_audio_rendition(
    master: &MasterPlaylist,
    variant: &Variant,
    pipeline: &AuthenticatedPipeline,
) -> AudioPlan {
    let plan = audio_plan(master, variant);
    match &plan {
        AudioPlan::Separate { group, name, url } => info!(
            pipeline = %pipeline.id,
            input = %pipeline.input_id,
            group = %group,
            name = %name,
            uri = %log_url(url),
            "HLS pull follows the separate EXT-X-MEDIA audio rendition {name:?} of group \
             {group:?} with the video variant"
        ),
        AudioPlan::MissingGroup { group } => warn!(
            pipeline = %pipeline.id,
            input = %pipeline.input_id,
            group = %group,
            "{}",
            plan.reason().unwrap_or_default()
        ),
        AudioPlan::Muxed => {}
    }
    plan
}
