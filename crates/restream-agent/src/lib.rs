//! Restream agent plane: redacted context, investigation, planning,
//! validation, graph and topology previews and verification gates (`plane`),
//! and the approval-gated operation store (`execution`, with the
//! `agent-execution` feature).
//!
//! The product links this crate only with its `agent-plane` feature; the
//! HTTP handlers, persistence and runtime mutations stay in the product.

#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]
// Raw std lock access is disallowed in shipped code (clippy.toml, `restream_sync::lock`);
// tests may poison and inspect locks directly.
#![cfg_attr(test, allow(clippy::disallowed_methods))]

#[cfg(feature = "agent-execution")]
pub mod execution;
pub mod plane;
