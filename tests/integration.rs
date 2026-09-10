//! Integration suites (§14): end-to-end via the public library API only. Each
//! module here is one suite; shared fixtures live in this directory.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // §10: tests unwrap freely

#[path = "integration/eval.rs"]
#[cfg(feature = "ladybug")]
mod eval;
#[path = "integration/pipeline.rs"]
#[cfg(feature = "ladybug")]
mod pipeline;
#[path = "integration/store.rs"]
mod store;
