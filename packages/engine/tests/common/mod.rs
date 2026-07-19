//! Shared test runtime helpers for the engine workspace.
//!
//! This file is the forward-looking scaffold for the `mod common;`
//! integration-test pattern that the engine crates will adopt in later
//! todos (CLOB matching in #16, risk equivalence in #15, encrypted
//! RFQ in #27).
//!
//! Usage from a crate's `tests/<name>.rs`:
//! ```ignore
//! mod common;
//! use common::{block_on, runtime, DEFAULT_TEST_TIMEOUT};
//!
//! #[test]
//! fn my_test() {
//!     let rt = runtime();
//!     rt.block_on(async {
//!         // ... async work
//!     });
//! }
//! ```
//!
//! The helper wraps a 1-tick/second tokio current-thread runtime sized
//! for the engine's per-batch budget (matching the paper's 1-second
//! matching tick at `paper/synchra.tex:646`) and a 5-second per-test
//! timeout so a wedged future fails fast instead of stalling CI.
//!
//! The actual `#[tokio::test]` macro is re-exported from `tokio` once
//! each crate adds `tokio = { workspace = true, features = ["macros",
//! "rt-multi-thread"] }` to its `Cargo.toml`; the functions below are
//! the feature-flag-free fallback for crates that want to keep their
//! test runtime minimal (e.g. the `clob` pure-Rust matching tests in
//! #16).
//!
//! NOTE: this file is a workspace-level scaffold. It is not a cargo
//! target itself (the engine workspace has no `[package]`); the
//! functions are `include!`-able or `mod`-able from any crate's
//! `tests/common/mod.rs` once the crate opts in to using them.

#![allow(dead_code)]

use std::time::Duration;

/// Tokio test runtime configured for engine test suites.
///
/// Wraps `tokio::runtime::Builder::new_current_thread` with `enable_all`
/// so the runtime has timers + IO available for the TEE matching
/// engine and the alloy RPC mock fixtures. A new runtime is built per
/// test (cheap) so each test starts from a clean scheduler.
pub fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime for engine tests")
}

/// Convenience: run an async future on a fresh shared runtime and
/// block the test thread until it completes.
///
/// Prefer this over `runtime().block_on(future)` at call sites where
/// a one-shot future is the entire test body.
pub fn block_on<F: std::future::Future>(future: F) -> F::Output {
    runtime().block_on(future)
}

/// Default per-test timeout.
///
/// 5 seconds matches the engine's worst-case batch tick (1 s) plus
/// generous headroom for alloy RPC round-trips and the TEE mock
/// attestation handshake. Tests that need a longer budget should
/// override locally rather than bumping this constant.
pub const DEFAULT_TEST_TIMEOUT: Duration = Duration::from_secs(5);
