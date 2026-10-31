//! Health assessment: typed alerts derived from the engine's health
//! snapshot, the per-output diagnosis they share with the runtime state, and
//! per-destination baselines that flag a destination off its usual.
//!
//! All of it is pure derivation over snapshots and readings: no I/O, storage
//! or HTTP.

#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]
// Raw std lock access is disallowed in shipped code (clippy.toml, `restream_sync::lock`);
// tests may poison and inspect locks directly.
#![cfg_attr(test, allow(clippy::disallowed_methods))]

pub mod alerts;
pub mod destination_baselines;
pub mod diagnosis;
