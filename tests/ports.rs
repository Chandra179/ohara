//! Port contracts (§9): implementations are exercised through the public
//! traits and domain types, never through vendor handles.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "ports/engine.rs"]
mod engine;
#[path = "ports/knowledge.rs"]
mod knowledge;
#[path = "ports/retrieval.rs"]
mod retrieval;
