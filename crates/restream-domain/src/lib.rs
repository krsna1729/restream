//! Restream domain vocabulary, runtime-state contracts and graph planning.
//!
//! `domain` describes what *should* happen, `runtime` what *is* happening,
//! and `planner` turns desired stages into runtime decisions. None of it
//! depends on media execution, storage or the API.

#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]

pub mod domain;
pub mod planner;
pub mod runtime;
