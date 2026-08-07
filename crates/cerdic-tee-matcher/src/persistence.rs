//! On-disk durability for this process's real-but-previously-ephemeral
//! state, via `redb` (pure Rust, embedded, ACID, copy-on-write B+tree —
//! same design lineage as `reth-db`'s MDBX, chosen instead of MDBX/RocksDB
//! specifically because both of those are C/C++ over FFI: an unnecessary
//! trusted-computing-base surface inside a Confidential Space enclave,
//! see this module's own doc below for the fuller reasoning).
//!
//! # What's persisted, and why exactly this set
//!
//! - `market_data` (`TradeTape` per market) — the public trade tape. Real
//!   ask this module exists to answer: candle history previously died on
//!   every restart, capped further by `market_data::WINDOW_SECONDS`
//!   anyway (see that constant's own doc).
//! - `last_nonce` — replay-protection state. Without this surviving a
//!   restart, the nonce floor resets to zero and a previously-valid
//!   signed order could be replayed post-restart. A real correctness
//!   gap this closes, not just a convenience.
//! - `portfolio_markets` — which markets a portfolioKey has ever settled
//!   into. Answers `security-audit-tee-contracts.md` finding T4 ("restart
//!   amnesia... `/liquidation-check` can false-negative on pre-restart
//!   positions"). Not privacy-sensitive: the exact same linkage is
//!   already public via `SealedPositionTouched` events (`settle.rs`'s
//!   `index_open_interest` already reads it back from chain), this is
//!   just a faster local cache of the same public fact.
//! - `trader_volume` — cumulative notional per trader, `fees.rs`'s tier
//!   input. Losing this on restart isn't unsafe, just an unfair,
//!   unexplained fee-tier reset for anyone mid-way to a better tier.
//! - `oi_index` — the keeper-facing open-interest scan's own resume
//!   checkpoint (`AppState::poll_open_interest`). Losing it just means
//!   re-scanning from block 0 next restart, expensive but not incorrect;
//!   persisting it is a real efficiency win, not a correctness one.
//!
//! # What's deliberately NOT persisted
//!
//! - **`position_cache`** — the ONE deliberate exclusion, and the
//!   important one. This is decrypted, unsealed, plaintext sealed
//!   position data (size, side, leverage, TP/SL) — exactly what
//!   `sealed.rs`'s AES-256-GCM sealing exists to keep out of anyone's
//!   hands but the TEE. It already lives in RAM only, gone on restart,
//!   by design. Writing it to a persistent file — especially one this
//!   module's own "migratable to another GCP account" framing implies
//!   might get copied/synced somewhere — would be a real privacy
//!   regression against the exact threat model `ARCHITECTURE.md`'s
//!   privacy table describes. `position_cache` is documented as a
//!   write-through cache that always falls back to a real on-chain
//!   unseal read (`load_single_market_state_cached`'s own doc) — losing
//!   it on restart costs a few extra RPC reads until it re-warms, not
//!   correctness, so there is no upside worth that regression.
//! - **`books`/`book_updates`** (resting orders) — active trading
//!   intent, not settled history. Silently resurrecting a trader's
//!   resting orders after a restart they didn't consent to (maybe they
//!   meant to cancel, maybe conditions changed) is a worse default than
//!   starting clean, the same posture most real venues take on restart.
//! - **`settlement_tx_hashes`** — short-lived polling status for
//!   `/settlement-status`; moot by the time any restart happens.
//! - **`backstop`** state — a real candidate for a future pass, not
//!   included here to keep this change's scope to the fields that were
//!   an explicit ask (candles) or a named audit finding (T4, nonce
//!   replay), not "persist everything reachable."
//!
//! # Migration model
//!
//! One versioned blob, not a table per field: `PersistedState` is
//! serialized whole (JSON, human-inspectable, no new binary-format
//! dependency) under one key. Every field is `#[serde(default)]`, so the
//! common case — adding a new field later — needs NO migration code at
//! all, an older blob just deserializes with that field defaulted. A
//! `schema_version` stamp plus an ordered `MIGRATIONS` slice exists for
//! the rarer case, a genuinely breaking change (a field renamed or
//! restructured, not just added) — each entry transforms the raw JSON
//! `serde_json::Value` from one version to the next; `load` applies
//! every migration between whatever's stamped and `CURRENT_SCHEMA_VERSION`
//! before deserializing into today's `PersistedState`.
//!
//! # Why this file alone is "migratable to another GCP account"
//!
//! It's one self-contained file, no server, no managed-service
//! dependency, no export/import ceremony — moving to a different GCP
//! project (or off GCP entirely) is copying this one file to the new
//! disk. That's the same portability property `kms.rs`'s GCS-stored
//! secrets blob already has, just for trading state instead of key
//! material — and it's a stronger property than any managed SQL service
//! would have given this codebase's "runs identically inside GCP
//! Confidential Space and AWS Nitro Enclaves" goal (this crate's own
//! `Cargo.toml` description).

use crate::market_data::TradeTape;
use alloy::primitives::{Address, FixedBytes};
use common::types::MarketId;
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use std::collections::{HashMap, HashSet};
use std::path::Path;

const STATE_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("state");
const STATE_KEY: &str = "snapshot";
const META_TABLE: TableDefinition<&str, u32> = TableDefinition::new("meta");
const SCHEMA_VERSION_KEY: &str = "schema_version";

/// Bump when `PersistedState`'s shape changes in a way `#[serde(default)]`
/// alone can't absorb (a rename, a restructure — not a plain addition),
/// and push the matching transform onto `MIGRATIONS` below.
const CURRENT_SCHEMA_VERSION: u32 = 1;

/// One JSON-value-to-JSON-value transform, applied to raw parsed JSON
/// before this process's current `PersistedState` ever tries to
/// deserialize it — see this module's own "Migration model" doc.
/// `MIGRATIONS[0]` transforms version 1 -> 2, `MIGRATIONS[1]` transforms
/// 2 -> 3, and so on; empty today because nothing has needed a breaking
/// change yet, not because the mechanism is unused.
type Migration = fn(serde_json::Value) -> serde_json::Value;
const MIGRATIONS: &[Migration] = &[];

#[derive(Debug, thiserror::Error)]
pub enum PersistenceError {
    #[error("failed to open state database at {path}: {source}")]
    Open { path: String, source: redb::DatabaseError },
    #[error("redb transaction error: {0}")]
    Transaction(#[from] redb::TransactionError),
    #[error("redb table error: {0}")]
    Table(#[from] redb::TableError),
    #[error("redb storage error: {0}")]
    Storage(#[from] redb::StorageError),
    #[error("redb commit error: {0}")]
    Commit(#[from] redb::CommitError),
    #[error("state blob is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
}

/// Everything this process persists across a restart. Every field
/// `#[serde(default)]` — see this module's own "Migration model" doc for
/// why that matters. `MarketId`/`Address`/`FixedBytes` all serialize as
/// plain JSON strings (alloy's own `serde` feature, already a dependency
/// throughout this crate), so every map here round-trips through
/// `serde_json` with no custom (de)serialization code needed.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct PersistedState {
    #[serde(default)]
    pub market_data: HashMap<MarketId, TradeTape>,
    #[serde(default)]
    pub last_nonce: HashMap<Address, u64>,
    #[serde(default)]
    pub portfolio_markets: HashMap<FixedBytes<32>, HashSet<MarketId>>,
    #[serde(default)]
    pub trader_volume: HashMap<Address, u128>,
    #[serde(default)]
    pub oi_index: HashMap<MarketId, (HashSet<FixedBytes<32>>, u64)>,
}

/// Opens (creating if absent) the state database at `path` and brings its
/// schema up to `CURRENT_SCHEMA_VERSION`. Call once at startup, before
/// `load`.
pub fn open(path: &Path) -> Result<Database, PersistenceError> {
    let db = Database::create(path)
        .map_err(|source| PersistenceError::Open { path: path.display().to_string(), source })?;
    run_migrations(&db)?;
    Ok(db)
}

fn run_migrations(db: &Database) -> Result<(), PersistenceError> {
    let write_txn = db.begin_write()?;
    {
        let mut meta = write_txn.open_table(META_TABLE)?;
        let stored_version = meta.get(SCHEMA_VERSION_KEY)?.map(|v| v.value()).unwrap_or(0);

        if stored_version < CURRENT_SCHEMA_VERSION && stored_version > 0 {
            // A version 0 (unversioned/fresh) database has nothing to
            // migrate — this branch only ever runs for a REAL prior
            // version, once one exists.
            let mut state_table = write_txn.open_table(STATE_TABLE)?;
            let existing: Option<Vec<u8>> = state_table.get(STATE_KEY)?.map(|v| v.value().to_vec());
            if let Some(raw) = existing {
                let mut value: serde_json::Value = serde_json::from_slice(&raw)?;
                for migration in &MIGRATIONS[(stored_version as usize).saturating_sub(1)..] {
                    value = migration(value);
                }
                let encoded = serde_json::to_vec(&value)?;
                state_table.insert(STATE_KEY, encoded.as_slice())?;
            }
        }
        meta.insert(SCHEMA_VERSION_KEY, CURRENT_SCHEMA_VERSION)?;
    }
    write_txn.commit()?;
    Ok(())
}

/// Loads the last-persisted state, or `PersistedState::default()` on a
/// fresh database (never seen a save yet) — same "no data yet, don't
/// fabricate any" posture the rest of this crate already uses
/// (`market_data.rs`'s own `MarketSnapshot` doc). A corrupt/unreadable
/// blob degrades to the same empty default rather than panicking the
/// whole process on boot: losing this process's OWN prior cache is
/// recoverable (on-chain reads/live trade flow repopulate it), refusing
/// to start at all is not.
pub fn load(db: &Database) -> PersistedState {
    match try_load(db) {
        Ok(state) => state,
        Err(e) => {
            tracing::warn!(error = %e, "failed to load persisted state, starting from empty state");
            PersistedState::default()
        }
    }
}

fn try_load(db: &Database) -> Result<PersistedState, PersistenceError> {
    let read_txn = db.begin_read()?;
    let table = match read_txn.open_table(STATE_TABLE) {
        Ok(table) => table,
        Err(redb::TableError::TableDoesNotExist(_)) => return Ok(PersistedState::default()),
        Err(e) => return Err(e.into()),
    };
    match table.get(STATE_KEY)? {
        Some(raw) => Ok(serde_json::from_slice(raw.value())?),
        None => Ok(PersistedState::default()),
    }
}

/// Overwrites the persisted snapshot with `state`, in one transaction —
/// either the whole write lands or none of it does, never a half-written
/// blob a future `load` could choke on.
pub fn save(db: &Database, state: &PersistedState) -> Result<(), PersistenceError> {
    let bytes = serde_json::to_vec(state)?;
    let write_txn = db.begin_write()?;
    {
        let mut table = write_txn.open_table(STATE_TABLE)?;
        table.insert(STATE_KEY, bytes.as_slice())?;
    }
    write_txn.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db_path(name: &str) -> std::path::PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("cerdic-persistence-test-{name}-{}.redb", std::process::id()));
        let _ = std::fs::remove_file(&path);
        path
    }

    #[test]
    fn fresh_database_loads_the_default_empty_state() {
        let path = temp_db_path("fresh");
        let db = open(&path).expect("open");
        let state = load(&db);
        assert!(state.market_data.is_empty());
        assert!(state.last_nonce.is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn saved_state_round_trips_through_a_fresh_open() {
        let path = temp_db_path("roundtrip");
        {
            let db = open(&path).expect("open");
            let mut state = PersistedState::default();
            state.market_data.insert("BTC/USDC".to_string(), {
                let mut tape = TradeTape::default();
                tape.record(1_000, 65_000, 5);
                tape
            });
            state.last_nonce.insert(Address::ZERO, 42);
            state.trader_volume.insert(Address::ZERO, 1_000_000u128);
            save(&db, &state).expect("save");
        }
        // Re-open as a fresh Database handle, same file — proves this
        // survives a real process restart, not just an in-memory clone.
        let db = open(&path).expect("re-open");
        let loaded = load(&db);
        assert_eq!(loaded.last_nonce.get(&Address::ZERO), Some(&42));
        assert_eq!(loaded.trader_volume.get(&Address::ZERO), Some(&1_000_000u128));
        assert!(loaded.market_data.contains_key("BTC/USDC"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn schema_version_is_stamped_on_first_open() {
        let path = temp_db_path("version");
        {
            let db = open(&path).expect("open");
            let read_txn = db.begin_read().expect("read txn");
            let meta = read_txn.open_table(META_TABLE).expect("meta table");
            assert_eq!(meta.get(SCHEMA_VERSION_KEY).unwrap().unwrap().value(), CURRENT_SCHEMA_VERSION);
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn reopening_an_existing_database_does_not_lose_data() {
        let path = temp_db_path("reopen-preserves");
        {
            let db = open(&path).expect("open");
            let mut state = PersistedState::default();
            state.last_nonce.insert(Address::ZERO, 7);
            save(&db, &state).expect("save");
        }
        {
            // Simulates a restart on an already-versioned database:
            // run_migrations must not wipe existing data.
            let db = open(&path).expect("re-open");
            let state = load(&db);
            assert_eq!(state.last_nonce.get(&Address::ZERO), Some(&7));
        }
        let _ = std::fs::remove_file(&path);
    }
}
