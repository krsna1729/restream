//! Shared drivers for the srt-interop caller/listener binaries. Not
//! production Drivers -- see each driver module's own doc comment, and
//! Cargo.toml's doc comment for why there are several (one per
//! docs/srt-pure-rust-plan.md Phase 4 driver-framework bake-off entry)
//! rather than one trait spanning all of them.

pub mod cpu_stats;
pub mod driver;
pub mod mio_driver;
pub mod smol_driver;
pub mod tokio_driver;

pub const INTEROP_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
