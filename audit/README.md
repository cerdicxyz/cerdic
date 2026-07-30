# Cerdic Audit

## Status

| Stage | Status |
|---|---|
| Internal review | In progress |
| Invariant / fuzz tests | In progress |
| TEE attestation audit | In progress |
| Dependency audit | Not started |
| External audit | Not started |

## Scope

### Solidity Contracts

| Component | Path | Priority |
|---|---|---|
| Clearing Kernel | `packages/contracts/src/clearing/` | Critical |
| Order Book | `packages/contracts/src/execution/OrderBook.sol` | High |
| Oracle Hub | `packages/contracts/src/oracle/OracleHub.sol` | High |
| Perpetual Market | `packages/contracts/src/markets/BtcPerpMarket.sol` | High |
| Shielded Vault | `tiny/contracts/src/TinyShieldedVault.sol` | Medium |

### TEE Enclave

| Component | Path | Priority |
|---|---|---|
| TEE Matcher | `crates/cerdic-tee-matcher/` | Critical |
| ZK Circuits | `crates/zk-circuits/` | High |
| Demo Client | `crates/cerdic-tee-matcher/src/bin/` | Low |

### Infra

| Component | Path | Priority |
|---|---|---|
| Docker | `Dockerfile` | High |
| CI | `.github/workflows/ci.yml` | Medium |
| Cargo workspace | `Cargo.toml`, `Cargo.lock` | Medium |

## Process

1. **Manual review** — `checklist.md` per component
2. **Static analysis** — Slither for Solidity, cargo-deny for Rust deps
3. **TEE audit** — attestation quote verification, sealed-key lifecycle
4. **Fuzz + invariant tests** — `fuzz/` directory
5. **External audit** — reports in `reports/`
