//! Domain-level audio-routing grammar shared by planner and media backends.
//!
//! These types describe what an encoding string means for audio selection or
//! transformation. Backend modules can then decide how to execute the routing
//! without owning the grammar themselves.

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "mode", rename_all = "camelCase")]
pub enum AudioRouting {
    /// Pass all audio streams through unchanged.
    #[serde(rename = "all")]
    Passthrough,
    /// Select specific audio tracks by 0-based index.
    SelectTracks { tracks: Vec<usize> },
    /// Remap stereo channels: (left_channel, right_channel, optional_track).
    Remap {
        #[serde(default)]
        track: usize,
        #[serde(rename = "leftChannel")]
        left: usize,
        #[serde(rename = "rightChannel")]
        right: usize,
    },
    /// Downmix a specific audio track to stereo.
    Downmix { track: usize },
}

impl AudioRouting {
    pub fn operation_string(&self) -> Option<String> {
        match self {
            Self::Passthrough => None,
            Self::SelectTracks { tracks } if !tracks.is_empty() => Some(format!(
                "atrack:{}",
                tracks
                    .iter()
                    .map(|track| track.to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            )),
            Self::SelectTracks { .. } => None,
            Self::Remap { left, right, track } if *track == 0 => {
                Some(format!("remap:{left}:{right}"))
            }
            Self::Remap { left, right, track } => Some(format!("remap:{left}:{right}:{track}")),
            Self::Downmix { track } => Some(format!("downmix:{track}")),
        }
    }
}

pub fn is_audio_operation(value: &str) -> bool {
    value.starts_with("atrack:") || value.starts_with("remap:") || value.starts_with("downmix:")
}

pub fn parse_audio_operation(operation: &str) -> AudioRouting {
    if let Some(rest) = operation.strip_prefix("remap:") {
        let parts: Vec<&str> = rest.split(':').collect();
        if parts.len() >= 2 {
            let left = parts[0].parse().unwrap_or(0);
            let right = parts[1].parse().unwrap_or(1);
            let track = parts.get(2).and_then(|t| t.parse().ok()).unwrap_or(0);
            return AudioRouting::Remap { left, right, track };
        }
    } else if let Some(rest) = operation.strip_prefix("atrack:") {
        let tracks: Vec<usize> = rest.split(',').filter_map(|t| t.parse().ok()).collect();
        if !tracks.is_empty() {
            return AudioRouting::SelectTracks { tracks };
        }
    } else if let Some(rest) = operation.strip_prefix("downmix:")
        && let Ok(track) = rest.parse()
    {
        return AudioRouting::Downmix { track };
    }

    AudioRouting::Passthrough
}

pub fn parse_audio_routing(encoding: &str) -> AudioRouting {
    let audio_part = if let Some(pos) = encoding.find('+') {
        &encoding[pos + 1..]
    } else if is_audio_operation(encoding) {
        encoding
    } else {
        return AudioRouting::Passthrough;
    };

    parse_audio_operation(audio_part)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routing_passthrough_for_plain_video_preset() {
        assert!(matches!(
            parse_audio_routing("720p"),
            AudioRouting::Passthrough
        ));
        assert!(matches!(
            parse_audio_routing("source"),
            AudioRouting::Passthrough
        ));
        assert!(matches!(
            parse_audio_routing("1080p"),
            AudioRouting::Passthrough
        ));
    }

    #[test]
    fn routing_select_tracks_single() {
        let routing = parse_audio_routing("720p+atrack:0");
        assert!(matches!(routing, AudioRouting::SelectTracks { ref tracks } if tracks == &[0]));
    }

    #[test]
    fn routing_select_tracks_multiple() {
        let routing = parse_audio_routing("source+atrack:0,2,5");
        assert!(
            matches!(routing, AudioRouting::SelectTracks { ref tracks } if tracks == &[0, 2, 5])
        );
    }

    #[test]
    fn routing_select_tracks_invalid_falls_back_to_passthrough() {
        assert!(matches!(
            parse_audio_routing("720p+atrack:abc"),
            AudioRouting::Passthrough
        ));
        assert!(matches!(
            parse_audio_routing("720p+atrack:"),
            AudioRouting::Passthrough
        ));
    }

    #[test]
    fn routing_remap_two_channel() {
        let routing = parse_audio_routing("720p+remap:0:1");
        assert!(matches!(
            routing,
            AudioRouting::Remap {
                left: 0,
                right: 1,
                track: 0
            }
        ));
    }

    #[test]
    fn routing_remap_with_track_index() {
        let routing = parse_audio_routing("source+remap:0:1:3");
        assert!(matches!(
            routing,
            AudioRouting::Remap {
                left: 0,
                right: 1,
                track: 3
            }
        ));
    }

    #[test]
    fn routing_remap_default_fallback() {
        let routing = parse_audio_routing("720p+remap:0");
        assert!(matches!(routing, AudioRouting::Passthrough));
    }

    #[test]
    fn routing_downmix_single_track() {
        let routing = parse_audio_routing("source+downmix:0");
        assert!(matches!(routing, AudioRouting::Downmix { track: 0 }));
        let routing = parse_audio_routing("720p+downmix:3");
        assert!(matches!(routing, AudioRouting::Downmix { track: 3 }));
    }

    #[test]
    fn routing_downmix_invalid_falls_back_to_passthrough() {
        assert!(matches!(
            parse_audio_routing("720p+downmix:abc"),
            AudioRouting::Passthrough
        ));
        assert!(matches!(
            parse_audio_routing("720p+downmix:"),
            AudioRouting::Passthrough
        ));
    }

    #[test]
    fn routing_atrack_standalone() {
        let routing = parse_audio_routing("atrack:0,1");
        assert!(matches!(routing, AudioRouting::SelectTracks { ref tracks } if tracks == &[0, 1]));
    }

    #[test]
    fn routing_remap_standalone() {
        let routing = parse_audio_routing("remap:0:1");
        assert!(matches!(
            routing,
            AudioRouting::Remap {
                left: 0,
                right: 1,
                track: 0
            }
        ));
    }

    #[test]
    fn routing_downmix_standalone() {
        let routing = parse_audio_routing("downmix:0");
        assert!(matches!(routing, AudioRouting::Downmix { track: 0 }));
    }

    #[test]
    fn parse_audio_operation_supports_stage_owned_operations() {
        assert!(matches!(
            parse_audio_operation("atrack:0,1"),
            AudioRouting::SelectTracks { ref tracks } if tracks == &[0, 1]
        ));
        assert!(matches!(
            parse_audio_operation("downmix:2"),
            AudioRouting::Downmix { track: 2 }
        ));
    }

    #[test]
    fn parse_passthrough() {
        assert!(matches!(
            parse_audio_routing("source"),
            AudioRouting::Passthrough
        ));
        assert!(matches!(
            parse_audio_routing("720p"),
            AudioRouting::Passthrough
        ));
        assert!(matches!(parse_audio_routing(""), AudioRouting::Passthrough));
    }

    #[test]
    fn parse_atrack() {
        match parse_audio_routing("720p+atrack:0,1") {
            AudioRouting::SelectTracks { tracks } => assert_eq!(tracks, vec![0, 1]),
            other => panic!("expected SelectTracks, got {:?}", other),
        }
        match parse_audio_routing("source+atrack:2") {
            AudioRouting::SelectTracks { tracks } => assert_eq!(tracks, vec![2]),
            other => panic!("expected SelectTracks, got {:?}", other),
        }
    }

    #[test]
    fn parse_remap() {
        match parse_audio_routing("source+remap:0:1") {
            AudioRouting::Remap { left, right, track } => {
                assert_eq!((left, right, track), (0, 1, 0));
            }
            other => panic!("expected Remap, got {:?}", other),
        }
        match parse_audio_routing("720p+remap:1:0:2") {
            AudioRouting::Remap { left, right, track } => {
                assert_eq!((left, right, track), (1, 0, 2));
            }
            other => panic!("expected Remap, got {:?}", other),
        }
    }

    #[test]
    fn parse_downmix() {
        match parse_audio_routing("source+downmix:1") {
            AudioRouting::Downmix { track } => assert_eq!(track, 1),
            other => panic!("expected Downmix, got {:?}", other),
        }
    }

    #[test]
    fn parse_legacy_remap() {
        match parse_audio_routing("remap:0:1") {
            AudioRouting::Remap { left, right, track } => {
                assert_eq!((left, right, track), (0, 1, 0));
            }
            other => panic!("expected Remap, got {:?}", other),
        }
    }
}
