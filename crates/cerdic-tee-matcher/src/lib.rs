//! `cerdic-tee-matcher`'s modules, exposed as a library so `main.rs` (the
//! real server) and `src/bin/demo_client.rs` (a manual-testing client)
//! share exactly one compiled copy of the request/response types and
//! signing logic, not two definitions that could silently drift apart.
//! See `docs/spec-contracts-tee.md` for the full module layout and HTTP
//! API this binary implements.

pub mod api;
pub mod attestation;
pub mod book;
pub mod decrypt;
pub mod keystore;
pub mod kms;
pub mod logging;
pub mod market_data;
pub mod proof;
pub mod sealed;
pub mod settle;
