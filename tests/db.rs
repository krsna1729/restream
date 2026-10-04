//! Integration tests for the persistence layer.
//! This file owns database-schema and query behavior so storage changes can be
//! verified independently from HTTP and runtime concerns.
#![allow(clippy::disallowed_methods)] // test code: raw std locks are fine

#[path = "db/core.rs"]
mod core;
#[path = "db/observability.rs"]
mod observability;
