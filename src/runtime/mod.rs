//! Runtime-layer contracts for live media graph state.
//!
//! Types here describe the *observed* runtime state of stages, outputs, and
//! graph plans. Domain types describe what *should* happen; runtime types
//! describe what *is* happening.

pub mod capacity;
pub mod graph;
pub mod health;
pub mod output;
pub mod stage;
