//! Low-level Media over QUIC (MoQ) wire protocol, catalog, and packaging.

#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]

pub mod budget;
pub mod media;
pub mod tls;
pub mod token;
pub mod url;

pub use budget::{CacheBudget, origin};
pub use media::{
    AUDIO_TRACK, AudioConfig, CATALOG_TRACK, CatalogTracks, LOC_HEADROOM, LocBuffer, MediaError,
    TrackContainer, TrackKind, VIDEO_TRACK, VideoCodec, VideoConfig, WireError,
    annexb_to_length_prefixed, from_hang_catalog, hang_catalog, parse_legacy_frame,
    parse_loc_frame,
};
pub use tls::{MAX_INCOMING_UNI_STREAMS, client_config, leaf_sha256_hex, server_config};
pub use token::{TOKEN_TTL, TokenError, TokenKey, mint, process_key, verify};
pub use url::{DEFAULT_MOQ_PORT, MOQ_ALPNS, MoqUrl, MoqUrlError, parse_moq_url};
