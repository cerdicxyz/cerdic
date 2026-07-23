# synchra-accelerator-mvp - Work Plan

## TL;DR (For humans)

**What you'll get:** A fully working privacy-preserving BTC/USDC perpetual futures market deployed on Circle's Arc Testnet. A trader can deposit yield-bearing USYC as collateral, open a leveraged BTC/USDC perpetual position through a public order book, submit encrypted block-trade RFQs matched inside a Phala TEE enclave, close their position, and withdraw — all settled in USDC gas with deterministic sub-second finality. The deliverable validates the three competitive moats from the protocol paper: leverage (clearing kernel + isolated margin + liquidation waterfall), privacy (encrypted RFQ + TEE matching), and programmable yield (USYC earning while backing positions).

**Why this approach:** The protocol paper specifies an exact MVP scope (§roadmap §Accelerator MVP, lines 1138-1158) and a fixed technology stack (Solidity/Foundry, Rust, Next.js, Intel TDX/Phala/Marlin, Arc EVM). The plan pins Phala Network as the TEE provider because it has production-grade TDX confidential VMs at $0.06-$0.23/hour, native on-chain EVM attestation governance (DstackApp), and a deployed production order-matching precedent (Turbine/Fynd.io). The plan pins Circle Developer-Controlled Wallets + App Kit for the frontend because the server-side REST custody model avoids per-user MPC friction on a privacy-sensitive MVP, and the SDK has a confirmed Arc Testnet quickstart. The clearing kernel uses isolated (not portfolio) margin per the paper's explicit MVP scope — cross-market offsets and the f_S/f_C/f_L/f_K formula components are computed but not granted to traders until Phase 1.

**What it will NOT do:** Cross-market portfolio margin offsets (isolated margin only). FX, RWA, strategy vault, or repo modules. Agent accounts, session keys, or Nanopayments. Any ZK proofs (solvency, batch verification, or attestation compression). Batch auctions. Mainnet deployment. Backstop liquidation, auto-deleveraging, or contract unwind. Formal verification of the clearing kernel. Risk scenario backtesting report.

**Effort:** XL
**Risk:** Medium - accelerator timeline pins to Phala Cloud provisioning maturity and Arc Testnet oracle/feed availability.

**Decisions to sanity-check:** (1) Phala Network (dstack) selected as the TEE provider over Marlin Oyster and raw Intel TDX — locks in Phala Cloud as external dependency. (2) Circle Developer-Controlled Wallets + App Kit selected over WaaS Web SDK — server-side custody model, no per-user MPC PIN flow. (3) MVP margin parameters: IMR 5%, MMR 3% (60% of IMR), liquidation threshold γ=0.85, max leverage 20×. (4) Arc Testnet oracle feeds may require mocks if Pyth/Chainlink are not yet deployed on Arc Testnet — smoke test will flag mock mode.

Your next move: start execution with `/start-work`. Full execution detail follows below.

---

> TL;DR (machine): XL | Medium | 37 todos across 5 kanban waves — clearing kernel contracts (Solidity/Foundry), BTC/USDC perp + CLOB (Rust+Solidity), risk monitor (Rust+Solidity), Phala TEE encrypted RFQ, Next.js+Circle Dev Wallets+App Kit frontend, Arc Testnet E2E.

## Scope
### Must have
- Solidity clearing kernel smart contracts (Solidity, Foundry, Arc EVM): accounts, collateral engine with 4-tier haircut, position engine (opaque-bytes per-market storage), settlement batch processor with isolated-margin enforcement — no cross-market offsets in MVP.
- BTC/USDC perpetual market extension implementing `IMarket` and the 7 lifecycle callbacks from `alg:hooks`: continuous funding index update with the paper formula `ΔF_t = clamp((P_mark − P_index) / P_index, −max_rate, +max_rate) · Δt`, lazy PnL = `Q · (F_current − F_entry) · P_index`, oracle-tethered mark price as median of Pyth primary + Chainlink secondary + on-chain impact TWAP, leverage ceiling 20×, IMR 5%, MMR = 60% of IMR, γ = 0.85.
- Rust off-chain CLOB matching engine: price-time priority, gas-aware on-chain order footprint storage (single-transaction modify-as-move per `paper/synchra.tex:627`), recycle fee for expired orders per `paper/synchra.tex:954`, emits settlement batches to the kernel.
- Rust portfolio margin risk monitor: pulls account state, computes isolated-margin liquidation when `C_eff < M · γ`, emits standard-liquidation calls to the kernel; backstop + ADL framework stubs in scope-OUT.
- Encrypted RFQ module with TEE quote matching using **Phala Network (dstack)** — `DstackApp` Solidity contract deployed on Arc Testnet verifies enclave identity keep-list + signature per quote; commit-reveal fallback for offline TEE; settlement only after on-chain attestation verification passes.
- Next.js + TypeScript trading frontend using **Circle Developer-Controlled Wallets (REST, server-side custody)** + **`@circle-fin/app-kit`** for crosschain/USDC gas UX: wallet connect, USYC approve/deposit, RFQ submission flow, position viewer with margin health bar, liquidation state display, withdrawal flow.
- Arc Testnet deployment with all smart contracts registered, USDC native gas configured, USYC ERC-20 (`0xe9185F0c5F296Ed1797AaE4238D26CCaBEadb86C`) collateral tier-2 registered, and a single end-to-end smoke test: deposit USYC → open BTC/USDC perp → RFQ block trade via Phala TEE → close → withdraw.

### Must NOT have (guardrails, anti-slop, scope boundaries)
- **NOT** portfolio-margin **cross-market offsets** — MVP margin is isolated per-position; the f_S / f_C / f_L / f_K formulas are computed and read but the engine does NOT grant cross-market hedging credits to the trader. Paper line 1143 explicitly says "isolated margin, liquidation waterfall".
- **NOT** the FX perpetual, RWA, strategy vault, repo/basis modules — Phase 1/2.
- **NOT** agent account system, programmable session keys, or Nanopayments integration — Phase 3.
- **NOT** ZK solvency proofs, ZK-compressed TEE attestation, or hourly batch ZK verification — paper §ZK is explicitly out of MVP scope.
- **NOT** frequent batch auctions / FBAs — MVP deliverables at `paper/synchra.tex:1144,1151` specify CLOB + encrypted RFQ only; batch auctions are Phase 1.
- **NOT** mainnet deployment — Testnet only per paper line 1155.
- **NOT** backstop-liquidation-pool, auto-deleveraging (ADL), or contract-unwind execution paths — framework stubs only.
- **NOT** formal verification of the clearing kernel — Phase 1 per paper line 1163.
- **NOT** risk scenario backtesting report — paper §economics defers to mainnet pre-launch.
- **NOT** a WaaS Web SDK / user-controlled MPC PIN flow on the frontend — only Developer-Controlled Wallets + App Kit per owner decision.
- **NOT** Marlin Oyster or raw Intel TDX bare metal — Phala Network (dstack) is the picked TEE provider.
- **NOT** slogging — see the separate remove-ai-slops recipe for the cleanup pass once work starts.

## Verification strategy
> Zero human intervention — all verification is agent-executed.
- Test decision: **TDD across all three languages** — forge unit + fuzz + invariant tests in `packages/contracts`; Rust unit + `proptest` + `tokio` integration tests in `packages/engine`; vitest + Playwright E2E in `packages/frontend`.
- Per-todo evidence path: `<attemptDir>/task-<N>-synchra-accelerator-mvp.<ext>` where `attemptDir = .omo/evidence/ulw/<session>/<goalId>/a<attempt>` (or `.omo/evidence/` outside ulw-loop).
- Smoke test contract: Arc Testnet end-to-end script in `packages/contracts/script/Smoke.s.sol` — deposit USYC → open perp → TEE RFQ → close → withdraw; asserts final balance matches expected PnL within `5 bps` tolerance (for testnet).
- Gas budgets: every public kernel function and perp module function must declare a `forge test --gas-report` ceiling in `packages/contracts/gas_benchmarks.txt`; ceilings committed alongside the contract code.

## Execution strategy
### Parallel execution waves
> Target 5-8 todos per wave. Fewer than 3 (except the final) means under-split.

The plan is organized as a **Kanban board**: five progressive waves that map to MVP release gates. Each wave is its own column with cards that can each succeed or fail independently within the wave's dependency context.

- **Wave 0 — Scaffold & Foundation** (7 cards): monorepo skeleton, tooling, CI, oracle + USYC stubs, paper-cited constants.
- **Wave 1 — Clearing Kernel** (8 cards): account model, collateral engine, position engine, settlement engine, IMarket interface, isolated-margin enforcement, liquidation entry, gas benchmarks.
- **Wave 2 — BTC/USDC Perpetual Extension + Risk Monitor + CLOB** (8 cards): perp extension impl, funding index, oracle mark price, leverage/margin guards, risk monitor wiring, CLOB matching engine, on-chain batch settlement, recycle-fee cleanup.
- **Wave 3 — Encrypted RFQ + Phala TEE** (7 cards): RFQ data types, `DstackApp` Solidity verifier, Phala Cloud account, Rust RFQ matcher crate in TDX CVM, on-chain attestation gate, commit-reveal fallback, RFQ integration test.
- **Wave 4 — Frontend + Arc Testnet E2E** (7 cards): Next.js + Circle Dev Wallets REST backend, App Kit integration, USYC approve/deposit flow, RFQ submission UI, position viewer + margin health bar, Arc Testnet deployment scripts, end-to-end smoke test.

Total: **37 implementation todos + 4 final verification tasks = 41 tracked work items.**

### Dependency matrix
| Todo | Depends on | Blocks | Can parallelize with |
| --- | --- | --- | --- |
| 1 | — | 2-37 | 2 |
| 2 | 1 | 8, 11, 13, 18, 25 | — |
| 3 | 1 | 6, 8, 18, 25 | 2 |
| 4 | 2 | 6, 8, 14, 18 | 3, 5, 7 |
| 5 | 1 | 6, 25, 33 | 2 \\
| 6 | 4, 5 | 8, 14, 18, 22 | 7 |
| 7 | 1 | 8, 25, 37 | 2, 4 |
| 8 | 2-7 | 14, 18, 22 | 9, 10 |
| 9 | 1 | 14, 22, 37 | 8 |
| 10 | 8 | 14, 22 | 9 |
| 11 | 8 | 17-22, 31-32 | 9, 10, 12-15 |
| 12 | 8, 11 | 22, 37 | 13-15 |
| 13 | 11 | 18, 22, 25 | 14, 15 |
| 14 | 8, 9, 10, 13 | 18, 22 | 15-17 |
| 15 | 10, 11 | 18, 22 | 14, 16-17 |
| 16 | 13 | 22 | 17-19 |
| 17 | 11-16 | 22, 36 | 18-21 |
| 18 | 13-17 | 22 | 19-21 |
| 19 | 17 | 28 | 20-22 |
| 20 | 17 | 28 | 19, 21, 22 |
| 21 | 11 | 22 | 19-20, 22 |
| 22 | 18-21 | 30, 37 | 23-25 |
| 23 | 1 | 25, 31-32 | 24, 22 |
| 24 | 11 | 25, 27 | 23, 22 |
| 25 | 23, 24 | 27, 37 | 26 |
| 26 | 25 | 27, 37 | — |
| 27 | 22, 25, 26 | 30, 37 | 28 |
| 28 | 17, 19, 20, 27 | 30 | 29 |
| 29 | 5, 25 | 30, 37 | 28 |
| 30 | 28, 29 | 37 | 31-36 |
| 31 | 25, 30 | 37 | 32-36 |
| 32 | 5, 25, 30 | 37 | 31, 33-36 |
| 33 | 6, 30 | 37 | 31-32, 34-36 |
| 34 | 30 | 37 | 31-33, 35-36 |
| 35 | 30 | 37 | 31-34, 36 |
| 36 | 30 | 37 | 31-35 |
| 37 | 22, 30-36 | F1-F4 | — |

(F1-F4 depend on all implementation todos per the contract.)

## Todos
> Implementation + Test = ONE todo. Never separate.
<!-- APPEND TASK BATCHES BELOW THIS LINE WITH edit/apply_patch - never rewrite the headers above. -->

### Wave 0 — Scaffold & Foundation

- [ ] 1. Pnpm-workspaces monorepo + tooling bootstrap
  What to do / Must NOT do: Initialize a pnpm workspaces monorepo at `/Users/nuel/oss/pjs/synchra-monorepo` with four workspaces: `packages/contracts` (Foundry project), `packages/engine` (Cargo workspace), `packages/frontend` (Next.js app), `packages/shared` (TS types shared across TS-only code paths). Root `package.json` exposes `dev`, `test`, `lint`, `build` scripts that fan out to each workspace. Root `pnpm-workspace.yaml` lists the four packages; root `Cargo.toml` declares a virtual workspace with `members = ["packages/engine/*"]`. Install working versions: pnpm ≥ 9.x, solc 0.8.24 (lock via `foundryup`), Rust ≥ 1.78 (via `rustup`), Node ≥ 20. Add root `.gitignore` covering `node_modules/`, `target/`, `out/`, `cache/`, `lib/`, `next-env.d.ts`, `*.log`, `.omo/evidence/` (so agent-exec evidence doesn't pollute the repo). Add `.tool-versions` pinning node, rust, and solc to the locked versions. **Must NOT have:** commit `paper/synchra.tex` history changes (paper keeps living in `paper/`); must NOT pin specific Phala or Circle SDK versions at this layer — those go in their dependent todos.
  Parallelization: Wave 0 | Blocked by: — | Blocks: 2-37
  References (executor has NO interview context — be exhaustive): `README.md:1-3` (current repo state); `paper/synchra.tex:1198-1219` (Stack Table — Solidity+Foundry / Rust / TypeScript+Node.js / Next.js); Pnpm workspaces docs https://pnpm.io/workspaces; Foundry book at Context7 ID `/foundry-rs/book`.
  Acceptance criteria (agent-executable): run `pnpm install && pnpm -r build` at repo root and all four workspaces report success. Run `forge --version` and confirm `forge 0.2.0+` or newer. Run `cargo --version` and confirm `cargo 1.78.0` or newer. Run `node --version` and confirm `v20.x` or newer. Assert `ls packages/contracts packages/engine packages/frontend packages/shared` each returns a directory with its `package.json` or `Cargo.toml`. Assert `.tool-versions` exists at repo root.
  QA scenarios (name the exact tool + invocation): happy = `pnpm test` at root exits 0 with no test failures; failure = remove one workspace from `pnpm-workspace.yaml` and assert `pnpm -r build` errors with "no packages found" — restore afterwards. Evidence `<attemptDir>/task-1-synchra-accelerator-mvp.log`
  Commit: Y | chore(monorepo): bootstrap pnpm workspaces + tooling versions

- [ ] 2. Shared TS type definitions for protocol constants
  What to do / Must NOT do: Create `packages/shared/src/types.ts` with TypeScript `type` and `interface` declarations for the data shapes that bridge the frontend, the engine, and on-chain event decoders: `MarketId` (bytes32 surface), `MarketPosition` struct mirror (matching `paper/synchra.tex:396-407` IMarket (simplified)) including `marketId: string`, `size: bigint`, `entryPrice: bigint`, `margin: bigint`, `leverage: bigint`; `CollateralTier` enum (TIER_1=1, TIER_2=2, TIER_3=3, TIER_4=4) per `paper/synchra.tex:366-380`; `Rfq` struct (taker / market / side / size / maxPrice? / expiryBlock); `RfqQuote` struct (maker / price / size); `FundingIndex` (marketId / cumulative / lastUpdateBlock); `LiquidationState` enum `Healthy | Warning | Liquidation | Closed` per `paper/synchra.tex:498-505`. Add `packages/shared/src/constants.ts` exporting haircuts per tier (Table at 367-380: `T1_HAIRCUT = 0`, `T2_HAIRCUT_BPS = [200, 500]`, `T3_HAIRCUT_BPS = [1000, 2000]`, `T4_HAIRCUT_BPS = [1500, 3500]`), MVP margin constants (`IMR_BPS = 500`, `MMR_BPS = 300` (60% of IMR per the decision), `LIQUIDATION_GAMMA = 85` percent), and `USYC_ARC_TESTNET = "0xe9185F0c5F296Ed1797AaE4238D26CCaBEadb86C"` sourced from Circle's USYC smart-contract docs. Add `packages/shared/src/index.ts` re-exporting both modules. Add `vitest` config + a smoke test that loops haircuts and asserts each Tier 1-4 haircut range is monotonically increasing. **Must NOT have:** any runtime logic (this package is pure types + constants — no settlement code, no oracle code, no wallet).
  Parallelization: Wave 0 | Blocked by: 1 | Blocks: 8, 11, 13, 18, 25
  References: `paper/synchra.tex:357-380` (collateral tiers); `paper/synchra.tex:396-407` (IMarket struct); `paper/synchra.tex:498-505` (liquidation states); `paper/synchra.tex:549-556` (lifecycle callbacks); librarian sourced USYC address `0xe9185F0c5F296Ed1797AaE4238D26CCaBEadb86C`.
  Acceptance criteria (agent-executable): `pnpm --filter @synchra/shared test` exits 0. `pnpm --filter @synchra/shared build` emits `dist/index.js` with zero TS errors `tsc --noEmit -p packages/shared/tsconfig.json`. `grep -E "T1_HAIRCUT|T2_HAIRCUT_BPS|USYC_ARC_TESTNET" packages/shared/src/constants.ts` returns 3 matches.
  QA scenarios: happy = `vitest run` shows 1 passing test; failure = swap T2 and T3 haircut ranges, assert the monotonicity test fails; restore. Evidence `<attemptDir>/task-2-synchra-accelerator-mvp.log`
  Commit: Y | feat(shared): add protocol types + constants for collateral tiers, margin thresholds, USYC testnet address

- [ ] 3. Foundry + Solc 0.8.24 project in `packages/contracts`
  What to do / Must NOT do: Inside `packages/contracts`, run `forge init --no-commit` to produce the canonical Foundry project layout (`src/`, `test/`, `script/`, `lib/`). Configure `foundry.toml`'s `[profile.default]` with `solc = "0.8.24"`, `evm_version = "cancun"`, `optimizer_runs = 200`, `fs_permissions = [{ access = "read", path = "./" }]`. Add `[profile.testnet]` extending default with `eth_rpc_url = "${ARC_TESTNET_RPC}"`, `eth_rpc_url` placeholder pointing to the user's Arc Testnet endpoint via `.env`. Install `@pythnetwork/pyth-sdk-solidity` as a Foundry library via `forge install PythNetwork/pyth-sdk-solidity --no-commit` (consumes Pyth's `IPyth` and `Bytes32` price feed interfaces per `/pyth-network/pyth-crosschain` Context7 docs). Install `@chainlink/contracts` via `forge install smartcontractkit/chainlink --no-commit`. Pin both libraries in `.gitmodules` with the SHAs resolved by `forge install`. Write `packages/contracts/test/Setup.t.sol` that imports both libraries and verifies each returns `address(0)` if unset. **Must NOT have:** mainnet RPC URLs hardcoded; must NOT commit `.env`.
  Parallelization: Wave 0 | Blocked by: 1 | Blocks: 6, 8, 18, 25
  References: Context7 `/foundry-rs/book` for `forge init`, `forge install`, `forge test` mechanics; Context7 `/pyth-network/pyth-crosschain` for Solidity consumer + EVM price-feed APIs; `paper/synchra.tex:1058-1061` (oracle stack requirement).
  Acceptance criteria (agent-executable): `cd packages/contracts && forge build` exits 0. `forge test --match-path test/Setup.t.sol -vvv` passes. `forge config | grep solc` reports `0.8.24`. `.gitmodules` has 2 submodule entries.
  QA scenarios: happy = `forge build` produces artifacts; failure = intentionally mis-set `solc = "0.8.99"` in `foundry.toml` and assert `forge build` errors — restore. Evidence `<attemptDir>/task-3-synchra-accelerator-mvp.log`
  Commit: Y | chore(contracts): init Foundry project, pin solc 0.8.24, install Pyth + Chainlink libraries

- [ ] 4. Cargo workspace in `packages/engine`
  What to do / Must NOT do: Create `packages/engine/Cargo.toml` (virtual workspace with `members = ["crates/clob", "crates/risk", "crates/rfq-matcher", "crates/common"]` and a `[workspace.dependencies]` block reserving `tokio = "1"`, `serde = "1"`, `serde_json = "1"`, `thiserror = "1"`, `alloy = "0.3"` for EVM RPC + ABI bindings, `proptest = "1"` for property tests, `dstack-sdk = "0.1"` for Phala). Initialize each crate with `cargo new --lib packages/engine/crates/<NAME>` and empty `lib.rs`. Add a `tests/common/mod.rs` that bootstraps a 1-second-tick tokio runtime and an optional Anvil fixture via `alloy`'s `anvil` provider. Add `[profile.release] lto = "fat"` and `codegen-units = 1`. **Must NOT have:** duplicate definitions of protocol types — use FFI against `packages/shared`'s emitted `dist/types.d.ts` baked into a `crates/common/types.rs` via a build.rs that runs `pnpm --filter @synchra/shared build` once and parses `dist/types.d.ts` to derive Rust mirror types; alternatively hand-write `crates/common/types.rs` mirroring the TS types and pin a smoke test that asserts the bytes32/uint256 encodings match (lhs vs rhs). Implementation choice (hand-write + test) is **RECOMMENDED** — FFI-from-TS is fragile and beyond MVP scope.
  Parallelization: Wave 0 | Blocked by: 1 | Blocks: 6, 8, 14, 18
  References: `paper/synchra.tex:1207-1210` (engine crates = Rust); Rust workspace book at https://doc.rust-lang.org/book/ch14-03-cargo-workspaces.html.
  Acceptance criteria (agent-executable): `cargo check --workspace` exits 0 at `packages/engine`. `cargo test --workspace --lib` exits 0 with all 4 crate stub tests passing. `grep -E "alloy|dstack-sdk" packages/engine/Cargo.toml` returns 2 matches under `[workspace.dependencies]`.
  QA scenarios: happy = `cargo build --workspace` produces 4 rlibs; failure = rename one crate member and assert `cargo check` errors with "failed to load manifest" — restore. Evidence `<attemptDir>/task-4-synchra-accelerator-mvp.log`
  Commit: Y | chore(engine): init Cargo workspace with 4 crates + workspace deps

- [ ] 5. Next.js 14 + TypeScript app in `packages/frontend`
  What to do / Must NOT do: Run `npx create-next-app@14 packages/frontend --typescript --app --no-src-dir --import-alias "@/*" --use-pnpm` (Next 14 stable to match Circle's App Kit docs which list Next.js as first-class). Add `@circle-fin/app-kit@1.10.0` and `viem@2.x` as dependencies (App Kit adapters). Add `axios` for talking to the dev-controlled-wallets REST backend. Configure `next.config.mjs` with `transpilePackages: ["@synchra/shared"]` and a `publicRuntimeConfig` slot for `ARC_TESTNET_RPC`, `BACKEND_API_URL`, `USYC_ADDRESS`. Add `vitest` + `@playwright/test` as devDependencies. Add `.env.local.example` documenting `CIRCLE_API_KEY=`, `CIRCLE_ENTITY_SECRET=`, `ARC_TESTNET_RPC=`, `BACKEND_API_URL=`. **Must NOT have:** WaaS Web SDK (`@circle-fin/w3s-pw-web-sdk`) — explicitly scope-OUT per owner decision; must NOT commit `.env.local`.
  Parallelization: Wave 0 | Blocked by: 1 | Blocks: 6, 25, 33
  References: Context7 `/websites/getfoundry_sh` for Arc + next setup; Circle App Kit docs at https://docs.arc.io/app-kit and https://www.npmjs.com/package/@circle-fin/app-kit.
  Acceptance criteria (agent-executable): `pnpm --filter @synchra/frontend dev` runs and `curl --head http://localhost:3000` returns `200 OK`. `pnpm --filter @synchra/frontend test` exits 0. `grep -E "@circle-fin/app-kit|viem" packages/frontend/package.json` returns 2 matches in `dependencies`. `.env.local.example` exists and contains the 4 documented keys.
  QA scenarios: happy = `next build` produces `.next/` and reports "Compiled successfully"; failure = unset NEXT_PUBLIC_API_URL and assert the Playwright smoke test fails on wallet-connect — restore. Evidence `<attemptDir>/task-5-synchra-accelerator-mvp.log`
  Commit: Y | chore(frontend): init Next.js 14 + app kit + viem + test runners

- [ ] 6. CI pipeline (GitHub Actions) — build, test, lint across all three stacks
  What to do / Must NOT do: Add `.github/workflows/ci.yml` with three jobs that fan out in parallel: (1) `contracts` — actions/setup-node@v4, `foundryup`, `forge fmt --check`, `forge build`, `forge test -vvv`; (2) `engine` — `dtolnay/rust-toolchain@1.78`, `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`; (3) `frontend` — actions/setup-node@v4, `pnpm install --frozen-lockfile`, `pnpm --filter @synchra/frontend lint`, `pnpm --filter @synchra/frontend test`, `npx playwright test --project=chromium`. The root job runs `pnpm install --frozen-lockfile && pnpm lint`. Trigger on push to `main` and any PR. Add caching for `~/.cache/foundry`, `~/.cargo/registry`, `~/.cargo/git`, `node_modules` via `actions/cache@v4`. **Must NOT have:** mainnet deploy step, secret key material beyond `CIRCLE_API_KEY` and `ARC_TESTNET_RPC` placeholders in `secrets:`.
  Parallelization: Wave 0 | Blocked by: 2, 3, 4 | Blocks: 14, 18
  References: `.github/workflows/` patterns from Foundry docs at `/foundry-rs/book`; rust toolchain action https://github.com/dtolnay/rust-toolchain; pnpm CI patterns https://pnpm.io/continuous-integration.
  Acceptance criteria (agent-executable): Push a trivial commit `chore(ci): no-op` to a feature branch and confirm on `gh run list` that 3 jobs run and all 3 exit 0. `grep -E "forge test|cargo test|playwright test" .github/workflows/ci.yml` returns 3 matches.
  QA scenarios: happy = green CI on the trivial commit; failure = intentionally break a TS type and assert the `frontend` job fails — restore. Evidence `<attemptDir>/task-6-synchra-accelerator-mvp.log`
  Commit: Y | ci: add parallel contracts+engine+frontend build/test/lint pipeline

- [ ] 7. Protocol constants reference snapshot in repo
  What to do / Must NOT do: Write `packages/contracts/src/lib/ProtocolConstants.sol` exposing constant values matching `packages/shared/src/constants.ts`: `IMR_BPS`, `MMR_BPS`, `LIQUIDATION_GAMMA_PERCENT`, `MAX_LEVERAGE_BPS` (2000 for 20×), `FUNDING_MAX_RATE_BPS_PER_SEC` per paper line 567 somewhere in the 5-50 bps/sec industry range (use `30` — conservative for MVP), `TIER_*_HAIRCUT_*_BPS_*` ranges mirroring `paper/synchra.tex:367-380`, `USYC_ARC_TESTNET` (= `address(0xe9185F0c5F296Ed1797AaE4238D26CCaBEadb86C)`). Add `ProtocolConstants.t.sol` that asserts each immutable matches the paper-cited value. The intent is a **single source of truth** between TS and Solidity — the test guards against the two drifting apart. **Must NOT have:** live-haircut adjustment logic — that is Phase 1 (`dynamic haircuts` per `paper/synchra.tex:1166`); MVP uses static MVP-tiered haircuts.
  Parallelization: Wave 0 | Blocked by: 1 | Blocks: 8, 25, 37
  References: `paper/synchra.tex:367-380` (collateral tiers), `paper/synchra.tex:422-425` (settlement), `paper/synchra.tex:567` (funding clamp), `paper/synchra.tex:571` (1×-50× ceiling; MVP trims to 20× = `2000` bps), `packages/shared/src/constants.ts` (TS source of truth, todo #2).
  Acceptance criteria (agent-executable): `forge test --match-contract ProtocolConstantsTest -vvv` passes. `forge coverage --match-contract ProtocolConstants --lcov` reports 100% line coverage. `grep -E "IMR_BPS|MMR_BPS|LIQUIDATION_GAMMA_PERCENT|USYC_ARC_TESTNET" packages/contracts/src/lib/ProtocolConstants.sol` returns each key at least once.
  QA scenarios: happy = unit test green; failure = change `IMR_BPS` in constants.ts without updating the .sol and assert ProtocolConstants.t.sol fails — restore. Evidence `<attemptDir>/task-7-synchra-accelerator-mvp.log`
  Commit: Y | feat(contracts): add ProtocolConstants single-source-of-truth + drift test

### Wave 1 — Clearing Kernel

- [ ] 8. Clearing account contract — Account.sol
  What to do / Must NOT do: Implement `packages/contracts/src/clearing/Account.sol` exposing a `mapping(address => AccountData) public accounts` keyed by trader wallet address, where `AccountData` is a struct with `mapping(address => uint256) collateralBalances`, `mapping(bytes32 => bytes) positions` (opaque bytes per `paper/synchra.tex:409`), and `mapping(bytes32 => int256) creditRecords`. Each balance and position maps into the formal model `A = (B, P, H)` per `paper/synchra.tex:357-360`. Add `deposit(address asset, uint256 amount)`, `withdraw(address asset, uint256 amount)`, `getPosition(bytes32 marketId) view returns (bytes)`, `getCollateralBalance(address asset) view returns (uint256)`. Deposit emits `CollateralDeposited(trader, asset, amount)`; withdraw reverts unless the isolated-margin check passes (delegated to `RiskMonitor.isWithdrawSafe(trader, asset, amount)` call — pure read, stubbed until #10). Deposit/withdraw calls must `require(accountState == AccountState.Healthy)` if the account is in liquidation — but for the MVP the state machine reduces to `Healthy` only (liquidation state transitions live in #13 via `LiquidationEntry.freezeAccount`). Add initializer access control via OpenZeppelin's `AccessControl` with `CLEARING_ADMIN_ROLE`. Add `forge test` for: happy deposit emits event and increases balance; withdraw to zero succeeds; withdraw that would breach isolated margin reverts (mock the risk monitor to return false). **Must NOT have:** institution hierarchy / sub-account split — Phase 3 (`paper/synchra.tex:1192`); must NOT manage account abstraction inside this contract — that's settlement-engine job (#11).
  Parallelization: Wave 1 | Blocked by: 2-7 | Blocks: 14, 18, 22 | can-parallelize-with: 9, 10
  References: `paper/synchra.tex:339-421` (§Clearing Kernel), `paper/synchra.tex:348-360` (account model), `paper/synchra.tex:493-510` (liquidation state machine).
  Acceptance criteria (agent-executable): `forge test --match-contract AccountTest -vvv` passes with ≥4 cases (deposit happy, withdraw happy, withdraw-breach reverts, frozen-account reverts). `forge coverage --match-contract Account --lcov` shows ≥90% lines covered. `forge test --gas-report --match-contract AccountTest` reports `deposit` ≤ 80k gas, `withdraw` ≤ 100k gas — record in `packages/contracts/gas_benchmarks.txt`.
  QA scenarios: happy = deposit+withdraw round-trip leaves balance unchanged; failure = withdraw more than deposited reverts with `InsufficientCollateral()`. Evidence `<attemptDir>/task-8-synchra-accelerator-mvp.log`
  Commit: Y | feat(clearing): add Account contract with deposit/withdraw + position storage

- [ ] 9. Collateral engine contract — CollateralEngine.sol
  What to do / Must NOT do: Implement `packages/contracts/src/clearing/CollateralEngine.sol` exposing `tierOf(address asset) view returns (uint8)`, `haircutBpsOf(address asset) view returns (uint16)`, `oraclePriceOf(address asset) view returns (uint256)` (delegates to `PythConsumer.sol` from #12), and `effectiveCollateral(address trader) view returns (uint256)` per the formula at `paper/synchra.tex:384-386`: `C_eff = Σ b_a · (1 − h_a) · p_a`. Register the four tier classifications using static arrays per `paper/synchra.tex:367-380`: Tier 1 assets (USDC, EURC) at 0% haircut, Tier 2 (USYC at 200 bps MVP haircut) at 2-5% range lower bound 2%, Tier 3 (ETH/BTCe liquid staking tokens) at 10%-20% MVP midpoint 15%, Tier 4 (tokenized RWAs) at 15%-35%. Use `haircutBpsOf` returning the MVP constant from `ProtocolConstants.sol`. Add Oracle price reading via an `OracleConsumer` interface defined here and implemented by `PythConsumer` in #12. Add `assetValueUsd(address asset, uint256 amount) view returns (uint256)` returning `amount · haircutAdjustedPrice / 1e18` for use by the risk monitor. Add invariant tests for tier monotonicity (Tier1 < Tier2 < Tier3 < Tier4 in haircut). **Must NOT have:** dynamic haircut adjustments — Phase 1 per `paper/synchra.tex:378`.
  Parallelization: Wave 1 | Blocked by: 1 | Blocks: 14, 22, 37 | can-parallelize-with: 8
  References: `paper/synchra.tex:362-386` (collateral engine + effective collateral formula).
  Acceptance criteria (agent-executable): `forge test --match-contract CollateralEngineTest -vvv` passes with tier-monotonicity invariant passing 100 fuzz iterations. `forge coverage --match-contract CollateralEngine` shows ≥90%. Gas budget for `effectiveCollateral(trader)` with 4 collateral assets ≤ 120k gas.
  QA scenarios: happy = effective collateral for 1000 USDC at 0% haircut + 1000 USYC at 2% = `1000 + 980 = 1980` USD; failure = register USDC at Tier 4 haircut and assert effective collateral drops. Evidence `<attemptDir>/task-9-synchra-accelerator-mvp.log`
  Commit: Y | feat(clearing): add CollateralEngine with tier classification + effective-collateral formula

- [ ] 10. Position engine contract — PositionEngine.sol
  What to do / Must NOT do: Implement `packages/contracts/src/clearing/PositionEngine.sol` exposing `_store(address trader, bytes32 marketId, bytes positionData) internal`, `load(address trader, bytes32 marketId) view returns (bytes)`, `_clear(address trader, bytes32 marketId) internal`. Position is stored as opaque `bytes` per `paper/synchra.tex:409`; market-extension authors encode their own struct into bytes. Expose `IImarket` ABI hooks (defined in #11) that call `_store` after validation. Add `getPositionMetadata(address trader, bytes32 marketId) view returns (uint256 size, uint256 entryPrice, uint256 margin, uint256 leverage)` — this decodes the opaque bytes via a per-market decoder contract registered via `registerDecoder(bytes32 marketId, address decoderAddress)` (the decoder is the market extension itself implementing `IMarket.getMetadata`). **Must NOT have:** funding accrual logic (`paper/synchra.tex:419-420` — that lives in the perp module, todo #14); must NOT have portfolio-margin across positions — that's #15 with isolated-margin semantics.
  Parallelization: Wave 1 | Blocked by: 8 | Blocks: 14, 22 | can-parallelize-with: 9
  References: `paper/synchra.tex:388-411` (position engine and opaque storage).
  Acceptance criteria (agent-executable): `forge test --match-contract PositionEngineTest` passes with 5+ cases: store/load round-trips bytes; clear zeros the slot; load-unknown returns empty; registration of a mock decoder produces correct metadata bytes. Gas for `_store` ≤ 50k (SSTORE + emit).
  QA scenarios: happy = store + load preserves bytes; failure = attempt to read after `_clear` and assert empty. Evidence `<attemptDir>/task-10-synchra-accelerator-mvp.log`
  Commit: Y | feat(clearing): add PositionEngine with opaque-bytes storage + per-market decoder registry

- [ ] 11. Settlement engine contract + IMarket interface — SettlementEngine.sol
  What to do / Must NOT do: Implement `packages/contracts/src/clearing/IMarket.sol` (Solidity interface) matching `paper/synchra.tex:396-407` (simplified IMarket): `struct MarketPosition { bytes32 marketId; int256 size; uint256 entryPrice; uint256 margin; uint256 leverage; }` + 4 functions `getPnL`, `getFunding`, `validateOpen`, `validateClose`. Implement `packages/contracts/src/clearing/IMarketLifecycle.sol` matching the 7 callbacks at `paper/synchra.tex:549-556`: `beforeOpenPosition`, `afterOpenPosition`, `beforeClosePosition`, `afterClosePosition`, `beforeSettleFunding`, `onLiquidation`, `onOracleUpdate`. Implement `packages/contracts/src/clearing/SettlementEngine.sol` exposing `settleTrade(bytes32 marketId, address longTrader, address shortTrader, int256 size, uint256 price, uint256 premium)` that (1) calls the market extension's `IMarketLifecycle.beforeOpenPosition` for each side; (2) validates margin sufficiency by calling the registered market extension's `IMarket.validateOpen`; (3) atomically updates `PositionEngine._store` for both sides; (4) calls `afterOpenPosition`. For `StructuredProductLimit` (premium-bearing instruments): only on the upfront-premium transfer per `paper/synchra.tex:419` — wired as a transfer of `msg.value` (or its asset equivalent) at trade time. `settleTrade` must revert on insufficient margin or `validateOpen == false`. Settlement must be **atomic** per `paper/synchra.tex:419` ("collateral changes atomically"). Add event `TradeSettled(marketId, longTrader, shortTrader, size, price, premium)`. **Must NOT have:** funding settlement in this contract — that comes via `settleFunding` in the perp extension (#14); must NOT compute liquidation — that's #15.
  Parallelization: Wave 1 | Blocked by: 8 | Blocks: 17-22, 31-32 | can-parallelize-with: 9, 10, 12-15
  References: `paper/synchra.tex:388-407` (IMarket interface); `paper/synchra.tex:537-557` (Extension interface + callbacks); `paper/synchra.tex:413-420` (settlement engine).
  Acceptance criteria (agent-executable): `forge test --match-contract SettlementEngineTest` passes with ≥6 cases including beforeOpen reverts, validateOpen=false reverts, both-sides position storage, premium transfer path, atomic revert on either side revert. `forge invariant settle-trade-preserves-collateral` runs with `runs = 1000` and asserts total collateral preserved across fuzzed trades.
  QA scenarios: happy = trade settles and both parties' positions are created; failure = long trader has 1 wei insufficient margin — assert `InsufficientMargin()` revert and no state mutation. Evidence `<attemptDir>/task-11-synchra-accelerator-mvp.log`
  Commit: Y | feat(clearing): add IMarket + IMarketLifecycle interfaces + atomic SettlementEngine

- [ ] 12. Oracle consumer — PythConsumer.sol + ChainlinkConsumer.sol
  What to do / Must NOT do: Implement `packages/contracts/src/oracle/PythConsumer.sol` that wraps `@pythnetwork/pyth-sdk-solidity`'s `PythContract` on Arc Testnet (deploy address resolved per Pyth docs for Arc Testnet — the deploy addresses endpoint at https://docs.pyth.network/price-feeds/contract-addresses lists all EVM chains; if Arc isn't there, fall back to deploying a mock Pyth for testing). Exposes `fetchPrice(bytes32 priceFeedId) returns (PythStructs.Price)` with staleness check + circuit breaker integration (deferred). Implement `packages/contracts/src/oracle/ChainlinkConsumer.sol` exposing `fetchPrice(address aggregator) view returns (int256, uint256 updatedAt)` delegating to `@chainlink/contracts`' `AggregatorV3Interface.latestRoundData`. Implement `packages/contracts/src/oracle/OracleHub.sol` exposing `markPrice(bytes32 marketId) view returns (uint256)` that takes the median of (1) Pyth primary via `PythConsumer.fetchPrice`, (2) Chainlink secondary via `ChainlinkConsumer.fetchPrice`, and (3) on-chain impact TWAP from the CLOB (deferred to #14 since CLOB doesn't exist yet — MVP sets tertiary = primary for now, doc'd in code as a TODO). Circuit breaker per `paper/synchra.tex:1063`: if Pyth vs Chainlink diverge by > `δ_bps` (default 500 bps = 5%) — emit `CircuitBreakerTripped` and revert subsequent markPrice calls until `unpause` by admin. **Must NOT have:** ZK proofs for oracle — out of MVP scope (`paper/synchra.tex:1095-1100`).
  Parallelization: Wave 1 | Blocked by: 8, 11 | Blocks: 22, 37 | can-parallelize-with: 13-15
  References: Context7 `/pyth-network/pyth-crosschain` for Solidity consumer pattern; `paper/synchra.tex:1055-1063` (multi-oracle + circuit breaker).
  Acceptance criteria (agent-executable): `forge test --match-contract OracleHubTest` passes with ≥5 cases: Pyth returns valid price; Chainlink returns valid; circuit breaker trips on 6% divergence; mark price = median of 2 sources (TWAP stub); resume after unpause. Gas budget for `markPrice` ≤ 200k.
  QA scenarios: happy = median returns within 1 bps of expected for mocked feeds; failure = inject 6% divergence, assert markPrice reverts with `CircuitBreakerTripped()`. Evidence `<attemptDir>/task-12-synchra-accelerator-mvp.log`
  Commit: Y | feat(oracle): add Pyth + Chainlink consumers + OracleHub mark-price median with circuit breaker

- [ ] 13. Liquidation entry contract — LiquidationEntry.sol (isolated-margin version)
  What to do / Must NOT do: Implement `packages/contracts/src/clearing/LiquidationEntry.sol` exposing `checkAndFlag(address trader, bytes32 marketId) returns (bool)`: read `OracleHub.markPrice(marketId)` + `CollateralEngine.effectiveCollateral(trader)` + `PositionEngine.getPositionMetadata` to compute isolated-margin utilisation = `notionalValue / effectiveCollateral`. If utilisation ≥ `LIQUIDATION_GAMMA_PERCENT` (85%), call `Account.freezeAccount(trader)` and emit `AccountFrozen`. Add `executeStandardLiquidation(address trader, bytes32 marketId, uint256 maxNotional)` — closes the trader's position by issuing a market sell back to the SettlementEngine with liquidator as counterparty up to `maxNotional`, with the liquidation penalty (1%-5% per `paper/synchra.tex:947`) routed to the liquidator and the insurance fund (stub). Add invariant test: standard liquidation preserves total collateral. **Must NOT have:** backstop liquidation pool, auto-deleveraging, contract unwind, multi-stage cascade — all scope-OUT per the Approval brief.
  Parallelization: Wave 1 | Blocked by: 8, 11 | Blocks: 18, 22, 25 | can-parallelize-with: 14, 15
  References: `paper/synchra.tex:488-520` (liquidation mechanism + waterfall); `paper/synchra.tex:945-948` (liquidation penalty fee).
  Acceptance criteria (agent-executable): `forge test --match-contract LiquidationEntryTest -vvv` passes with ≥4 cases: healthy account stays Healthy; margin-breach freezes; standard liquidation transfers liquidation penalty; collateral preservation invariant holds across 200 fuzz runs.
  QA scenarios: happy = account under-margined gets flagged, liquidator closes position, balances settle; failure = liquidation call on Healthy account reverts with `NoLiquidation()`. Evidence `<attemptDir>/task-13-synchra-accelerator-mvp.log`
  Commit: Y | feat(clearing): add LiquidationEntry for isolated-margin standard liquidation + insurance-fund stub

- [ ] 14. BTC/USDC perpetual market extension — BtcPerpMarket.sol (with funding index)
  What to do / Must NOT do: Implement `packages/contracts/src/markets/BtcPerpMarket.sol` implementing `IMarket` and `IMarketLifecycle` (the perp market contract itself does NOT toggle private/TEE mode — the public CLOB is the only execution path for this contract; TEE-based private matching is handled by the separate RFQ flow in todos #23-#27). State: `fundingIndex` (mapping bytes32 marketId → int256 cumulative) per `paper/synchra.tex:419` lazy model; `lastIndexUpdateBlock`. Implement `updateFundingIndex(bytes32 marketId)` using the formula at `paper/synchra.tex:567`: `ΔF_t = clamp((P_mark − P_index) / P_index, −max_rate, +max_rate) · Δt`, where `max_rate = FUNDING_MAX_RATE_BPS_PER_SEC` (30 bps/s, per #7), `Δt = block.number − lastIndexUpdateBlock`. `P_mark = OracleHub.markPrice(MBTC_USDC_FEED)`, `P_index = OracleHub.ethPythPrimary(MBTC_USDC_FEED)`. Set `fundingIndex += ΔF_t`. Implement `getPnL(position, oraclePrice) returns (int256)` that combines spot PnL + lazy funding PnL = `Q · (F_current − F_entry) · P_index` per `paper/synchra.tex:569`. Implement `validateOpen`: requires `|size| · oraclePrice · IMR_BPS / 1e4 ≤ effectiveCollateral` (`IMR_BPS = 500` = 5%); leverage cap = `MAX_LEVERAGE_BPS` (2000 = 20×). Implement `validateClose(position) returns bool`: true if position size = 0 after close. Implement all 7 lifecycle hooks. Emit `FundingIndexUpdated(marketId, newIndex, block.number)` on each update. **Must NOT have:** private/TEE mode toggling inside this contract — TEE-based private matching is handled by the separate RFQ flow (todos #23-#27); the perp market extension uses public CLOB only per MVP deliverables at `paper/synchra.tex:1149-1156`.
  Parallelization: Wave 2 | Blocked by: 8, 9, 10, 13 | Blocks: 18, 22 | can-parallelize-with: 15
  References: `paper/synchra.tex:561-575` (perp module spec); `paper/synchra.tex:567` (funding formula); `paper/synchra.tex:569` (lazy PnL); `paper/synchra.tex:1063` (oracle circuit breaker).
  Acceptance criteria (agent-executable): `forge test --match-contract BtcPerpMarketTest -vvv` passes with ≥8 cases: happy open/close under margin; open above 20× reverts; open with insufficient margin reverts; funding index monotonic in stable market; getPnL formula matches expectation within 1 wei for fuzzed prices (10 fuzz runs via `forge test --gas-report`); lazy funding settlement correct when position held across 1000 blocks.
  QA scenarios: happy = funding index updates, position opens, settles; failure = attempt open at 21× leverage, assert `LeverageCeiling()`. Evidence `<attemptDir>/task-14-synchra-accelerator-mvp.log`
  Commit: Y | feat(markets): add BTC/USDC perp market extension with continuous funding index + lazy PnL

- [ ] 15. Risk monitor contract + Rust component — RiskMonitor.sol + crates/risk
  What to do / Must NOT do: Implement `packages/contracts/src/clearing/RiskMonitor.sol` exposing `isWithdrawSafe(trader, asset, amount) view returns (bool)` (called by `Account.withdraw`): returns true if `effectiveCollateral − amount_in_usd ≥ currentMarginRequirement`. `currentMarginRequirement` is the isolated margin requirement: `Σ |positionSize| · markPrice · MMR_BPS / 1e4` — only the MMR is enforced here (initial margin checked at open in `IMarket.validateOpen`). Add `checkLiquidation(trader) returns (bool)` that compares isolated-margin requirement vs effective collateral; calls `LiquidationEntry.checkAndFlag` if breached. Implement `packages/engine/crates/risk/src/lib.rs` with the off-chain mirror (`RiskMonitor::compute_margin(ship_state: AccountState, oracle_mark_prices: HashMap<MarketId, U256>) -> MarginResult`) using the same formulas — this Rust mirror is used by the off-chain CLOB and TEE matching engine to pre-validate orders before broadcasting on-chain. Add `proptest` cases in `crates/risk/tests/` asserting Rust and Solidity yield identical `current_margin_requirement` for 1000 fuzzed account states. **Must NOT have:** `f_S`, `f_C`, `f_L`, `f_K` portfolio-margin components — scope-OUT.
  Parallelization: Wave 2 | Blocked by: 8, 9, 10, 11, 13 | Blocks: 18, 22 | can-parallelize-with: 14
  References: `paper/synchra.tex:422-474` (margin model formulas — but see scope-OUT for portfolio offsets in MVP); `paper/synchra.tex:511-518` (liquidation waterfall — standard stage only).
  Acceptance criteria (agent-executable): `forge test --match-contract RiskMonitorTest -vvv` passes with ≥5 cases including failed withdraw when margin would breach; `cargo test --package risk` passes incl. `proptest` equivalence with Solidity computation. `forge coverage --match-contract RiskMonitor` ≥90%.
  QA scenarios: happy = withdraw 1 wei under MR succeeds; failure = withdraw that tips account below MMR reverts `InsufficientMarginForWithdraw()`. Evidence `<attemptDir>/task-15-synchra-accelerator-mvp.log`
  Commit: Y | feat(risk): add on-chain + Rust mirror RiskMonitor with isolated-margin requirements

### Wave 2 — BTC/USDC Perp + CLOB Matching + Settlement

- [ ] 16. Rust CLOB matching engine — crates/clob
  What to do / Must NOT do: Implement `packages/engine/crates/clob/src/lib.rs` with: `OrderBook` struct (sorted price-levels via `BTreeMap<Price, VecDeque<Order>>` for asks, mirror for bids with reverse comparator); `process_order(order: Order) -> Vec<MatchResult>` running price-time priority matching; emits a `Vec<MatchedTrade>` that is later flattened to `Vec<Settlement>` and submitted to the on-chain `SettlementEngine.settleTrade`. Order fields: `id: u64`, `side: Side`, `price: U256`, `size: i128`, `expiry_block: u64`, `signature: Vec<u8>` (EIP-712 over the on-chain typed data). Add `modify_order(orderId, newPrice, newSize)` that moves the slot rather than delete/re-create to match the gas-aware design at `paper/synchra.tex:627`. Add `cancel_order(orderId)`. Add `cleanup_expired(currentBlock: u64) -> Vec<OrderId>` that returns expired order IDs and the owed recycle fee per #17. Add a `proptest` case fuzzing random order streams (limit-orders with random price/size within ±5% of mid) and asserting the matching engine fulfills conservation-of-shares and bounded-slippage invariants. **Must NOT have:** encrypted order mode — encrypted RFQ through TEE is separate (#19-#21).
  Parallelization: Wave 2 | Blocked by: 13 | Blocks: 22 | can-parallelize-with: 17-19
  References: `paper/synchra.tex:619-651` (§Execution Layer public CLOB section); `paper/synchra.tex:954` (order recycle fee).
  Acceptance criteria (agent-executable): `cargo test --package clob --lib -- --nocapture` passes with ≥10 unit cases + 1 `proptest` over 1000 fuzz iterations. `cargo bench --package clob` shows matching throughput at ≥5000 matches/s on test machine.
  QA scenarios: happy = inserted buy 105×3 matched against sell 105×3 — they clear at 105 with conservation-of-size; failure = sell at higher price than best bid remains in book, no match emitted. Evidence `<attemptDir>/task-16-synchra-accelerator-mvp.log`
  Commit: Y | feat(clob): add Rust matching engine with price-time + gas-aware modify + cleanup

- [ ] 17. On-chain order book storage contract for CLOB — OrderBook.sol
  What to do / Must NOT do: Implement `packages/contracts/src/execution/OrderBook.sol` exposing `placeOrder(bytes32 marketId, Side side, uint256 price, int256 size, uint256 expiryBlock, bytes signature)` that validates EIP-712 signature against the trader's account owner key, verifies margin sufficiency via the perp module's `IMarket.validateOpen`, stores order metadata (price/size/owner/expiry/sigHash) in a gas-efficient struct, emits `OrderPlaced`. Add `modifyOrder(orderId, newPrice, newSize)` that emits `OrderModified` and replaces the slot (not delete/recrete). Add `cancelOrder(orderId)`. Add `claimRecycleFee(expiredOrderId)` callable by anyone — if the order is past expiry, refund the bonded recycle fee to the caller per `paper/synchra.tex:954`. Bonded fee is sent as `msg.value` in `placeOrder` (USDC native gas on Arc per `paper/synchra.tex:1027`). Add event `OrderRecycled(orderId, claimer, feeRefunded)`. **Must NOT have:** matching in on-chain code — matching is the Rust engine's role; this contract just stores and validates orders + produces the `MatchedOrder`s that the CLOB emits as settlement batches.
  Parallelization: Wave 2 | Blocked by: 11-16 | Blocks: 22
  References: `paper/synchra.tex:619-629` (CLOB); `paper/synchra.tex:627` (modify-as-move); `paper/synchra.tex:954` (recycle fee).
  Acceptance criteria (agent-executable): `forge test --match-contract OrderBookTest -vvv` passes with ≥7 cases: place happy, place-out-of-margin reverts, modify happy, cancel happy, expired-order recycle claimable, invalid signature reverts, atomic revert on insufficient bond. Gas budget ≤ 150k for `placeOrder`, ≤ 80k for `modifyOrder`.
  QA scenarios: happy = order placed, book contains it; failure = claim recycle on unexpired order reverts `NotYetExpired()`. Evidence `<attemptDir>/task-17-synchra-accelerator-mvp.log`
  Commit: Y | feat(execution): add on-chain OrderBook with EIP-712 sig verify + gas-aware modify + recycle fee

- [ ] 18. Batch settlement submission — SettlementBatcher.sol
  What to do / Must NOT do: Implement `packages/contracts/src/clearing/SettlementBatcher.sol` exposing `submitBatch(bytes32 marketId, MatchedTrade[] trades, bytes[] signatures) external payable` that validates each `MatchedTrade`'s counter-party signatures against the `OrderBook`'s stored order hashes, calls `SettlementEngine.settleTrade` for each, rolls back the entire batch if any trade reverts, and emits `BatchSettled`. `MatchedTrade` struct: `longTrader, shortTrader, size, price, premium, longOrderId, shortOrderId`. Used by the Rust CLOB engine after each matching tick. Add invariant test asserting that `BatchSettled` is followed by per-trade `TradeSettled` events from `SettlementEngine` (atomic batch-to-trade event correlation). Add a `forge invariant batch-preserves-collateral` for fuzzed batches asserting total collateral unchanged. **Must NOT have:** per-order on-chain settlement — the Rust CLOB batches calls for gas efficiency matching the bulk-settlement pattern at `paper/synchra.tex:628` ("settlement batches").
  Parallelization: Wave 2 | Blocked by: 13-17 | Blocks: 22 | can-parallelize-with: 19-21
  References: `paper/synchra.tex:628` (CLOB submits matched trades as batches); `paper/synchra.tex:419` (settlement engine).
  Acceptance criteria (agent-executable): `forge test --match-contract SettlementBatcherTest -vvv` passes with ≥5 cases including batch-all-valid happy, 1-invalid reverts entire batch, signature-mismatch reverts. Gas budget ≤ 80k per trade in a batch of 10, ≤ 110k for a batch of 1.
  QA scenarios: happy = submit 3-item batch, all settle; failure = submit batch with longOrderId not in OrderBook, revert `UnknownOrder()` and no state changes. Evidence `<attemptDir>/task-18-synchra-accelerator-mvp.log`
  Commit: Y | feat(clearing): add batch settlement with per-trade signature verify + atomic revert

- [ ] 19. Rust risk-monitor wire into CLOB engine — pre-flight order validation hook
  What to do / Must NOT do: Wire `packages/engine/crates/clob/src/lib.rs` to call `RiskMonitor::compute_margin` (from #15) **before** emitting a `MatchedTrade` — reject the trade if either side would breach the MMR post-trade. Emit a `MarginRejectedTrade{ orderId, trader, reason }` event that gets logged for visibility but does NOT call the on-chain contracts. Add `proptest` case: random order streams with adversarial margin states (some traders insisting on opening just before liquidation) and assert (i) no `MatchedTrade` puts a trader below MMR, (ii) the matching engine throughput drops by ≤20% with the guard (bench in `cargo bench --package clob`).
  Parallelization: Wave 2 | Blocked by: 17 | Blocks: 28 | can-parallelize-with: 20-22
  References: `paper/synchra.tex:1076-1084` (invariants — position modifications atomic with collateral updates).
  Acceptance criteria (agent-executable): `cargo test --package clob --lib -- pre_flight_margin_guard` passes with 5 fuzz scenarios. `cargo bench --package clob` shows pre-flight guard throughput penalty ≤20%.
  QA scenarios: happy = trade that would breach one party is not emitted, logged as `MarginRejectedTrade`; failure = disable guard, assert trade would otherwise pass to on-chain. Evidence `<attemptDir>/task-19-synchra-accelerator-mvp.log`
  Commit: Y | feat(clob): pre-flight margin guard in matching engine

- [ ] 20. CLOB→settlement RPC adapter in Rust — crates/engine/rpc
  What to do / Must NOT do: Implement `packages/engine/crates/engine-rpc/src/lib.rs` (new sub-crate added to `Cargo.toml`) using `alloy` to (1) read/write the on-chain `OrderBook` and `SettlementBatcher` via `<contract>::new(address, provider)`, (2) submit batches via `provider.send_transaction(...)` paying Arc USDC native gas, (3) sign with the matching-engine's EOA. Add `setup_backoff` `reqwest` retry for transient RPC errors. Add a `tokio` integration test in `crates/engine-rpc/tests/end_to_end_local_anvil.rs` that spins up `anvil --block-time 1`, deploys all deploys via the script from #37, submits a 3-item batch round-trip, and asserts `OrderBook` no longer holds the spent order IDs. **Must NOT have:** mainnet RPC URL hardcoded — read from `ARC_TESTNET_RPC` env.
  Parallelization: Wave 2 | Blocked by: 17, 18, 19 | Blocks: 28 | can-parallelize-with: 21, 22
  References: Context7 `/pyth-network/pyth-crosschain` for EVM consumer patterns.
  Acceptance criteria (agent-executable): `cargo test --package engine-rpc --test end_to_end_local_anvil -- --nocapture` passes 1 test. `cargo clippy --package engine-rpc` reports 0 warnings.
  QA scenarios: happy = full round trip on local anvil; failure = submit with RPC pointing to non-responsive endpoint, assert retries then errors. Evidence `<attemptDir>/task-20-synchra-accelerator-mvp.log`
  Commit: Y | feat(engine): on-chain CLOB→settlement RPC adapter with anvil E2E

- [ ] 21. Mark-price on-chain impact TWAP — MarketImpactTwap.sol
  What to do / Must NOT do: Implement `packages/contracts/src/oracle/MarketImpactTwap.sol` exposing `recordTrade(bytes32 marketId, uint256 price, uint256 size)` called by `SettlementEngine` after each `settleTrade`. Track a 60-block rolling TWAP using `RingBuffer` storage — undocumented in MVP but memoized in `gas_benchmarks.txt` as `MarketImpactTwap.gasPricePerRecordTrade = 30k`. `OracleHub.markPrice` now uses this TWAP as the tertiary input per #12's deferred TODO. Add probe invariant: TWAP of uniform-constant trades equals the constant within 1 wei.
  Parallelization: Wave 2 | Blocked by: 11 | Blocks: 22 | can-parallelize-with: 19-20
  References: `paper/synchra.tex:570` ("median ... on-chain impact mid-price, basis-adjusted fair value, and best bid/ask/last trade").
  Acceptance criteria (agent-executable): `forge test --match-contract MarketImpactTwapTest -vvv` passes; `MarketImpactTwap.gasPricePerRecordTrade = 30k` recorded in `gas_benchmarks.txt`.
  QA scenarios: happy = insert 100 uniform trades, TWAP ≈ price; failure = inject 1 outlier 10× price, assert TWAP pulls toward outlier but bounded. Evidence `<attemptDir>/task-21-synchra-accelerator-mvp.log`
  Commit: Y | feat(oracle): add on-chain impact TWAP ring buffer fed into mark-price median

- [ ] 22. Arc Testnet Pyth + Chainlink feed wiring
  What to do / Must NOT do: Wire `packages/contracts/script/WireFeeds.s.sol` Foundry script that (1) on Arc Testnet, reads or deploys a mock Pyth contract if Arc Testnet isn't in `pyth-sdk-solidity`'s deploy URLs (per Arc Testnet maturity — see librarian finding); (2) sets up `PythConsumer` with `BTC_USD_FEED_ID = 0x...` (Pyth mainnet feed ID per Pyth docs); (3) sets up `ChainlinkConsumer` with the BTC/USD Chainlink aggregator address on Arc Testnet (or a mock if no BTC/USD aggregator exists). Document fallback in `packages/contracts/script/README.md`. Resulting `OracleHub` addresses persisted in `packages/contracts/deployments/arc-testnet.json`. **Must NOT have:** mainnet deployment step.
  Parallelization: Wave 2 | Blocked by: 18-21 | Blocks: 30, 37 | can-parallelize-with: 23
  References: Context7 `/pyth-network/pyth-crosschain` for deploy address endpoint.
  Acceptance criteria (agent-executable): `forge script WireFeeds --rpc-url ${ARC_TESTNET_RPC} --broadcast` exits 0. `cat packages/contracts/deployments/arc-testnet.json | jq -r '.pythConsumerAddress, .chainlinkConsumerAddress, .oracleHubAddress'` returns 3 non-zero addresses. If mocks are deployed, `deployments/arc-testnet.json` must contain `"oracleMode": "mock"` and `Smoke.s.sol` must require `--mock-oracle` flag to pass in mock mode.
  QA scenarios: happy = `cast call <oracleHub> "markPrice(bytes32)" <BTC_FEED_ID> --rpc-url $ARC_TESTNET_RPC` returns a non-zero price; failure = unset `ARC_TESTNET_RPC` and assert script errors. Evidence `<attemptDir>/task-22-synchra-accelerator-mvp.log`
  Commit: Y | feat(oracle): wire Arc Testnet Pyth + Chainlink feeds + deployment JSON record

### Wave 3 — Encrypted RFQ + Phala TEE

- [ ] 23. RFQ on-chain data structures — RfqRegistry.sol
  What to do / Must NOT do: Implement `packages/contracts/src/execution/RfqRegistry.sol` exposing `requestRfq(bytes32 marketId, Side side, int256 size, uint256 maxPrice, uint256 expiryBlock) external returns (bytes32 rfqId)` that emits `RfqRequested(rfqId, msg.sender, marketId, side, size, maxPrice, expiryBlock)`; `submitQuote(bytes32 rfqId, uint256 price, bytes encryptedQuoteReminderCiphertext) external returns (bytes32 quoteId)` emits `QuoteSubmitted` — the `encryptedQuoteReminderCiphertext` is opaque bytes storing encrypted order intent metadata per `paper/synchra.tex:666-671` (the buyer's price+size+expiry encapsulated so the matching layer sees only ciphertext until TEE decrypts). The TEE matcher (per #25) decrypts ciphertext inside the enclave and selects the best quote. `acceptQuote(quoteId) external` triggers `SettlementEngine.settleTrade` with the taker/price/size from the registered quote — emits `RfqSettled`. **Must NOT have:** encrypted order intent decryption in this contract — that's the TEE's role in #25.
  Parallelization: Wave 3 | Blocked by: 1 | Blocks: 25, 37 | can-parallelize-with: 22, 24
  References: `paper/synchra.tex:631-638` (§RFQ); `paper/synchra.tex:666-671` (privacy dimensions — order intent encrypted).
  Acceptance criteria (agent-executable): `forge test --match-contract RfqRegistryTest -vvv` passes ≥5 cases: request+quote+accept happy, expired RFQ reverts accept, unknown quote id reverts, signature-required on quote submit, encrypted blob emitted verbatim.
  QA scenarios: happy = RFQ→quote→settlement flow completes; failure = quote submitted after RFQ expiry reverts `RfqExpired()`. Evidence `<attemptDir>/task-23-synchra-accelerator-mvp.log`
  Commit: Y | feat(rfq): add RfqRegistry with encrypted quote blob for TEE matching

- [ ] 24. Solidity `DstackApp` sibling verifier contract — DstackAppArc.sol
  What to do / Must NOT do: Deploy a local `.omo`-governance clone of Phala's `DstackApp` contract as `packages/contracts/src/tee/DstackAppArc.sol` exposing `registerComposeHash(bytes32 composeHash, string deviceId)` (admin-gated), `isAppAllowed(bytes32 composeHash, bytes32 deviceId) view returns (bool)`, and `verifySignature(bytes32 messageHash, bytes signature, bytes32 composeHash, string deviceId) view returns (bool)` — verify the secp256k1 signature against the enclave's attestation-derived public key (only signatures whose key was `isAppAllowed`-governed pass). Mirror Phala's `DstackApp` ABI per the dstack on-chain docs cited in the librarian finding (https://docs.phala.com/phala-cloud/key-management/understanding-onchain-kms). Add `forge test` covering: register happy, duplicate register reverts, signature verify happy (with mock key derived from secp256k1), signature verify with wrong composeHash reverts. **Must NOT have:** ZK-compressed attestation — Phase 3 (`paper/synchra.tex:777-779`).
  Parallelization: Wave 3 | Blocked by: 11 | Blocks: 25, 27 | can-parallelize-with: 23, 22
  References: librarian finding — https://docs.phala.com/phala-cloud/key-management/understanding-onchain-kms, https://github.com/Dstack-TEE/dstack.
  Acceptance criteria (agent-executable): `forge test --match-contract DstackAppArcTest -vvv` passes ≥4 cases. Gas budget for `verifySignature` ≤ 50k.
  QA scenarios: happy = register composeHash, verify signed quote with allowed enclave key; failure = verify signature with wrong composeHash reverts `EnclaveNotAllowed()`. Evidence `<attemptDir>/task-24-synchra-accelerator-mvp.log`
  Commit: Y | feat(tee): add DstackAppArc verifier inspired by Phala DstackApp on-chain governance

- [ ] 25. Phala Cloud account + `dstack-sdk` Rust crate
  What to do / Must NOT do: Onboard the team to Phala Cloud per https://docs.phala.com — create account, fund wallet with USDC testnet credit, create a TDX CVM with the chosen compose hash that runs the Rust RFQ matcher crate from #26. Add `packages/engine/crates/rfq-matcher/Cargo.toml` `dstack-sdk = "0.1"` per Phala's open-source SDK on https://github.com/Dstack-TEE/dstack. Add `packages/engine/crates/rfq-matcher/src/attestation.rs` exposing `get_attestation_report() -> AttestationReport` returning the `composeHash + deviceId + tcbStatus` from the running CVM. Add a `tests/smoke_attestation.rs` that runs in a unit-test context (NOT real TEE) and asserts the attestation API correctly serializes a mock report. Document Phala Cloud deployment steps + budget in `packages/engine/crates/rfq-matcher/README.md`: (1) `docker build` the matcher container, (2) `dstack-cloud deploy --compose docker-compose.yml`, (3) `addComposeHash` tx on Arc Testnet `DstackAppArc` registry. **Must NOT have:** persistent attestation caching in MVP — re-fetch on each request.
  Parallelization: Wave 3 | Blocked by: 23, 24 | Blocks: 27, 37 | can-parallelize-with: 26
  References: librarian finding on Phala Cloud pricing $0.06–$0.23/hr TDX CVM; `paper/synchra.tex:711-740` (TEE architecture).
  Acceptance criteria (agent-executable): `cargo test --package rfq-matcher --test smoke_attestation` passes. `packages/engine/crates/rfq-matcher/README.md` contains `docker`, `dstack-cloud deploy`, `addComposeHash` strings. After real `dstack-cloud deploy`, `cast call <DstackAppArc> "isAppAllowed(bytes32,bytes32)" <composeHash> <deviceId> --rpc-url $ARC_TESTNET_RPC` returns `true`.
  QA scenarios: happy = mock attestation report serializes with valid `composeHash` 0x... and `deviceId` non-empty; failure = unset TEE attestation env var and assert test errors — restore. Evidence `<attemptDir>/task-25-synchra-accelerator-mvp.log`
  Commit: Y | feat(tee): onboard Phala Cloud, add dstack-sdk Rust crate with attestation report API

- [ ] 26. Rust RFQ matcher crate — crates/rfq-matcher
  What to do / Must NOT do: Implement `packages/engine/crates/rfq-matcher/src/lib.rs` exposing `match_rfq(rfq: Rfq, quotes: Vec<Quote>) -> Option<MatchedQuote>` that selects the best quote (best price-for-side), decrypts the encrypted ` ciphertext` using the TEE-internal key (obtained via `dstack-sdk`'s `get_secret` API), signs the resulting `MatchedQuote` with the enclave's KMS-derived signing key, and emits a `SignedSettlement` struct ready for on-chain submission. Wire `accept_quote` so that the on-chain `RfqRegistry.acceptQuote(quoteId)` triggers, with the enclave signature passed as data — `DstackAppArc.verifySignature` runs inside `acceptQuote` to gate the settlement. Add unit test cases for happy matching, no-quotes-returned fallback, merge two RFQs with stale quotes (expiry). Add a `tokio` integration test `tests/end_to_end_local.rs` that spins up the matcher in a mock CVM (no real TEE), submits RFQs, verifies signature compatibility with `DstackAppArc`. **Must NOT have:** ZK-proof generation — Phase 3 (`paper/synchra.tex:777`).
  Parallelization: Wave 3 | Blocked by: 25 | Blocks: 27, 37 | can-parallelize-with: 22
  References: `paper/synchra.tex:691-703` (private order flow / RFQ + commit-reveal); `paper/synchra.tex:718-740` (TEE matching).
  Acceptance criteria (agent-executable): `cargo test --package rfq-matcher` passes with ≥6 unit + 1 integration test. `cargo clippy --package rfq-matcher` reports no warnings.
  QA scenarios: happy = RFQ matches best quote, enclave signs settlement; failure = quote signed with non-governed key, on-chain acceptQuote reverts. Evidence `<attemptDir>/task-26-synchra-accelerator-mvp.log`
  Commit: Y | feat(rfq-matcher): TEE RFQ best-quote selection with enclave signature gate

- [ ] 27. On-chain TEE attestation gate — RfqRegistry settlement with DstackAppArc verify
  What to do / Must NOT do: Modify `RfqRegistry.acceptQuote` to (1) extract `(quoteId, price, size, enclaveSignature, composeHash, deviceId)` from the taker's calldata; (2) call `DstackAppArc.verifySignature(keccak256(quoteId|price|size), enclaveSignature, composeHash, deviceId)`; (3) require return value true; (4) settle via `SettlementEngine.settleTrade`. Add `forge test --match-contract RfqRegistryTeeGateTest -vvv` with cases: happy valid enclave signature; non-governed enclave reverts; expired RFQ reverts; large notional success. Add invariant: total collateral preserved across RFQ settlement including liquidation penalty.
  Parallelization: Wave 3 | Blocked by: 22, 25, 26 | Blocks: 30, 37 | can-parallelize-with: 28
  References: `paper/synchra.tex:738-740` (attestation verified on-chain before settlement).
  Acceptance criteria (agent-executable): `forge test --match-contract RfqRegistryTeeGateTest -vvv` passes ≥4 cases; invariant `rfq-settlement-preserves-collateral` passes 500 fuzz iterations.
  QA scenarios: happy = accept with governed enclave signature settles; failure = accept with non-governed signature reverts `AttestationFailed()`. Evidence `<attemptDir>/task-27-synchra-accelerator-mvp.log`
  Commit: Y | feat(rfq): gate RFQ settlement on DstackAppArc attestation verify

### Wave 4 — Frontend + Arc Testnet E2E

- [ ] 28. Commit-reveal fallback contract — CommitReveal.sol
  What to do / Must NOT do: Implement `packages/contracts/src/execution/CommitReveal.sol` exposing `commit(bytes32 commitmentHash) external payable` (requires bond = `REVEAL_BOND_AMOUNT` USDC), `reveal(bytes32 orderId, bytes32 nonce) external` — if the commitment `H(order, r)` matches the stored commitment per `paper/synchra.tex:696`, release bond + trigger matching; if not, bond forfeited per the bonding requirement at `paper/synchra.tex:1090`. Bond amount configurable. Add fallback flow: if TEE is offline (per `paper/synchra.tex:740` — system falls back to on-chain settlement with public execution), traders submit through commit-reveal instead. Add `forge test` covering commit→reveal happy, commitment hash mismatch reverts with bond slashed, expired commitment (no reveal) slash by liquidator claim. **Must NOT have:** ZK reveal — Phase 3.
  Parallelization: Wave 4 | Blocked by: 17, 19, 20, 27 | Blocks: 30 | can-parallelize-with: 29
  References: `paper/synchra.tex:693-700` (commit-reveal flow); `paper/synchra.tex:740` (TEE fallback to on-chain).
  Acceptance criteria (agent-executable): `forge test --match-contract CommitRevealTest -vvv` passes ≥5 cases incl. bond slashing.
  QA scenarios: happy = commit+reveal success, bond released; failure = mismatched nonce reverts `InvalidReveal()` with bond slashed. Evidence `<attemptDir>/task-28-synchra-accelerator-mvp.log`
  Commit: Y | feat(rfq): commit-reveal fallback path with bond slashing

- [ ] 29. Circle Developer-Controlled Wallets REST backend — Node/TS service
  What to do / Must NOT do: Implement `packages/frontend/backend/` as a Node.js + Express service exposing REST endpoints: `POST /wallet/create` (uses Circle's Developer-Controlled Wallets API with the server-held entity secret per https://developers.circle.com/wallets/dev-controlled), `GET /wallet/:walletId/balances`, `POST /wallet/:walletId/approve` (USYC-approve via `contractExecution`), `POST /wallet/:walletId/deposit_usyc` (transfer to clearing kernel `Account.deposit`), `GET /rfq/:rfqId/quotes`, `POST /rfq/submit`, `GET /positions/:walletId`. Use `@circle-fin/developer-controlled-wallets` REST API via axios — NOT the user-controlled WaaS Web SDK per owner decision. Store entity secret in OS keystore (macOS `security` or Linux `secret-service`). Add `vitest` tests with mocked Circle API responses covering all 8 endpoints. Add env-var validation on startup (`CIRCLE_API_KEY`, `CIRCLE_ENTITY_SECRET_PATH`, `ARC_TESTNET_RPC`, `BACKEND_API_URL`). **Must NOT have:** WaaS Web SDK integration, browser-held PIN/MPC flows; per owner decision the custody stays server-side.
  Parallelization: Wave 4 | Blocked by: 5, 25 | Blocks: 30, 37 | can-parallelize-with: 28
  References: librarian finding — https://developers.circle.com/wallets/dev-controlled, https://developers.circle.com/api-reference/wallets.
  Acceptance criteria (agent-executable): `pnpm --filter @synchra/backend test` exits 0 with ≥8 endpoint tests. `pnpm --filter @synchra/backend build` produces `dist/server.js`. `curl http://localhost:4001/health` returns 200 after `pnpm --filter @synchra/backend dev`.
  QA scenarios: happy = mock create wallet returns `walletId`, balance fetched; failure = mock Circle API returns 401, endpoint returns 503 with `CIRCLE_AUTH_FAILED`. Evidence `<attemptDir>/task-29-synchra-accelerator-mvp.log`
  Commit: Y | feat(backend): Circle Dev-Controlled Wallets REST with USYC approve/deposit flow

- [ ] 30. Arc Testnet full deployment script — DeployArc.s.sol
  What to do / Must NOT do: Implement `packages/contracts/script/DeployArc.s.sol` Foundry script that deploys ALL contracts in correct order: `ProtocolConstants` (already deployed by #7 as `immutable`, but values already exist there), `DstackAppArc`, `PythConsumer`, `ChainlinkConsumer`, `MarketImpactTwap`, `OracleHub`, `Account`, `CollateralEngine`, `PositionEngine`, `SettlementEngine`, `BtcPerpMarket`, `RiskMonitor`, `LiquidationEntry`, `OrderBook`, `SettlementBatcher`, `RfqRegistry`, `CommitReveal`. Wire cross-contract references via the run's broadcast transactions. Register the perp market in `SettlementEngine.markets[MBTC_USDC]` mapping. Set initial `DstackAppArc.registerComposeHash(<compose-hash-from-#25>, "phala-test-cluster")`. Save all deployed addresses to `packages/contracts/deployments/arc-testnet.json` keyed by contract name. Document the smoke-test sequence in `packages/contracts/script/README.md`. **Must NOT have:** mainnet deployment, real USDC beyond testnet faucet.
  Parallelization: Wave 4 | Blocked by: 28, 29 | Blocks: 37 | can-parallelize-with: 31-36
  References: `paper/synchra.tex:1138-1156` (MVP spec); Foundry scripts docs at `/foundry-rs/book`.
  Acceptance criteria (agent-executable): `forge script script/DeployArc.s.sol --rpc-url ${ARC_TESTNET_RPC} --broadcast` exits 0. `cat packages/contracts/deployments/arc-testnet.json | jq -r 'keys | length'` returns ≥17 deployed contract addresses. **Prerequisite: fund deployer EOA from Arc Testnet USDC faucet; assert `cast balance <deployer> --rpc-url $ARC_TESTNET_RPC > 0` before broadcasting.** Also deploy the Next.js frontend to a hosting target (Vercel or Arc static host) and assert `curl -I https://<frontend-url>` returns 200 — record the URL in `packages/frontend/deployments/arc-testnet.json`.
  QA scenarios: happy = all 17 contracts deployed and JSON persisted; failure = unset RPC, assert script fails with `RPC_URL_MISSING`. Evidence `<attemptDir>/task-30-synchra-accelerator-mvp.log`
  Commit: Y | feat(deploy): Arc Testnet all-contracts deploy script with persisted addresses JSON

- [ ] 31. Frontend wallet connect + USYC approve flow — Next.js pages
  What to do / Must NOT do: Implement `packages/frontend/app/wallet/page.tsx` — shows wallet connect button that calls `POST /wallet/create` (via the dev-controlled backend endpoint). After wallet is created/loaded, shows USYC + USDC balances via `GET /wallet/:walletId/balances`. Adds "Approve USYC for deposit" button that calls `POST /wallet/:walletId/approve { spender: <Account.sol address from arc-testnet.json>, amount }`. Shows USYC approval status. Uses `@circle-fin/app-kit` SvelteKit-style Web Components for any crosschain USDC deposit modal (App Kit is configured with Arc Testnet adapter). Stores `walletId` in localStorage. **Must NOT have:** WaaS Web SDK MPC PIN modal flow — only dev-controlled per owner decision.
  Parallelization: Wave 4 | Blocked by: 25, 30 | Blocks: 37 | can-parallelize-with: 32-36
  References: librarian finding — App Kit list of Arc-supported chains at https://docs.arc.io/app-kit/references/supported-blockchains.
  Acceptance criteria (agent-executable): `pnpm --filter @synchra/frontend test -- --run wallet` exits 0. `grep -E "POST /wallet/create|GET /wallet/.*/balances|POST /wallet/.*/approve" packages/frontend/app/wallet/page.tsx` returns 3 matches.
  QA scenarios: happy = Playwright script clicks Connect → balances load → Approve USYC button appears and POST hits the backend mock; failure = Kill backend, click Approve, assert error toast appears. Evidence `<attemptDir>/task-31-synchra-accelerator-mvp.log`
  Commit: Y | feat(frontend): wallet connect + USYC approve flow via Dev-Controlled Wallets + App Kit

- [ ] 32. USYC deposit flow page + position viewer
  What to do / Must NOT do: Implement `packages/frontend/app/account/page.tsx` — "Deposit USYC" button (with input for amount), then calls `POST /wallet/:walletId/deposit_usyc { amount }` to the backend. Shows USYC balance held in `Account.sol` after deposit. Position viewer table: reads `GET /positions/:walletId` and renders open positions in a table with marketId, size, entryPrice, unrealisedPnL, margin usability. Adds a margin-health bar component (green ≥ 1.5× MMR, yellow 1×–1.5× MMR, red below MMR) showing `effectiveCollateral / currentMarginRequirement`. **Must NOT have:** position adjustment tools — closing positions is via the trading page (#34).
  Parallelization: Wave 4 | Blocked by: 5, 25, 30 | Blocks: 37 | can-parallelize-with: 31, 33-36
  References: `paper/synchra.tex:503-507` (liquidation state machine visual states); `packages/shared/src/types.ts` types `MarketPosition`, `CollateralTier`, `LiquidationState`.
  Acceptance criteria (agent-executable): `pnpm --filter @synchra/frontend test -- --run account` exits 0. `grep -E "USYC|deposit_usyc|marginHealth" packages/frontend/app/account/page.tsx` returns ≥4 matches. Visual assertion via Playwright screenshot diff within 1% tolerance of a baseline — baseline generated on first run via `npx playwright screenshot --output packages/frontend/tests/baselines/account.png http://localhost:3000/account`, committed to the repo; subsequent runs diff against it.
  QA scenarios: happy = deposit USYC, balance updates; failure = Insufficient USYC approval assert toast appears. Evidence `<attemptDir>/task-32-synchra-accelerator-mvp.log`
  Commit: Y | feat(frontend): USYC deposit flow + margin-health position viewer

- [ ] 33. RFQ submission page with TEE matching flow
  What to do / Must NOT do: Implement `packages/frontend/app/trade/page.tsx` — RFQ submission form: market (BTC/USDC), side (long/short), size (in BTC), max price (input). Submit button calls `POST /rfq/submit` backend. After submission, shows quotes panel polling `GET /rfq/:rfqId/quotes` until a quote is returned. "Accept Quote" button calls `POST /rfq/accept` to trigger on-chain settlement via enclave-signed calldata. Shows the attestation status (verified-pending-failed) streamed from the backend. Shows the executed trade in a "Recent Trades" sidebar. **Must NOT have:** direct on-chain wallet transactions — all RFQ flows go through the backend which signs and broadcasts via the dev-controlled wallet.
  Parallelization: Wave 4 | Blocked by: 6, 30 | Blocks: 37 | can-parallelize-with: 31-32, 34-36
  References: `paper/synchra.tex:631-638` (RFQ flow).
  Acceptance criteria (agent-executable): `pnpm --filter @synchra/frontend test -- --run trade` exits 0. Playwright E2E screenshot diff < 1% from baseline — baseline generated on first run via `npx playwright screenshot --output packages/frontend/tests/baselines/trade.png http://localhost:3000/trade`, committed to the repo; subsequent runs diff against it.
  QA scenarios: happy = RFQ submitted, quote returned in 30s, accept triggers settlement; failure = No quote returned within timeout (mock TEE), UI shows "No quotes received — try smaller size". Evidence `<attemptDir>/task-33-synchra-accelerator-mvp.log`
  Commit: Y | feat(frontend): RFQ submission page with TEE matching + attestation status

- [ ] 34. Arc Testnet README + dev-run guide
  What to do / Must NOT do: Add `packages/frontend/README.md` and `packages/contracts/script/README.md` and update root `README.md` with: env vars required (`ARC_TESTNET_RPC`, `CIRCLE_API_KEY`, `CIRCLE_ENTITY_SECRET_PATH`, `BACKEND_API_URL`, `PHALA_COMPOSE_HASH`), commands to run locally (`pnpm --filter @synchra/backend dev`, `pnpm --filter @synchra/frontend dev`, `cargo run --package rfq-matcher -- serve`, `forge script script/DeployArc.s.sol --rpc-url $ARC_TESTNET_RPC --broadcast`), Phala Cloud deployment steps, Arc Testnet faucet URL, USYC test ERC-20 address (`0xe9185F0c5F296Ed1797AaE4238D26CCaBEadb86C`), and the smoke-test sequence from #37. **Must NOT have:** mainnet steps.
  Parallelization: Wave 4 | Blocked by: 30 | Blocks: 37 | can-parallelize-with: 31-33, 35-36
  References: librarian findings on Circle Dev-Controlled Wallets and USYC addresses.
  Acceptance criteria (agent-executable): `grep -E "ARC_TESTNET_RPC|CIRCLE_API_KEY|PHALA_COMPOSE_HASH|USYC" README.md` returns 4 key matches. An automated script `packages/contracts/script/verify-readme.sh` clones the repo to a temp dir, exports env vars from the README catalog, runs `forge script script/Smoke.s.sol`, and asserts exit 0 — zero human intervention.
  QA scenarios: happy = a fresh-clone-and-follow-README run passes the smoke script; failure = remove one env var from README, follow-assert smoke script errors. Evidence `<attemptDir>/task-34-synchra-accelerator-mvp.log`
  Commit: Y | docs: Arc Testnet run guide + env var catalog + USA address

- [ ] 35. End-to-end smoke test script — Smoke.s.sol
  What to do / Must NOT do: Implement `packages/contracts/script/Smoke.s.sol` Foundry script that runs after `DeployArc.s.sol`: (1) assume running against Arc Testnet via `--rpc-url`; (2) create two dev-controlled wallets via the backend; (3) approve + deposit USYC into `Account.sol` from each; (4) open a BTC/USDC perp position long 1 BTC at $100k with each wallet on opposite sides via `RfqRegistry.requestRfq` + `submitQuote` (simulated maker) + `acceptQuote` (TEE-gated); (5) wait for funding index update tick; (6) close both positions via `BtcPerpMarket.close`; (7) withdraw USYC; (8) assert final balances equal initial + realized PnL within 5 bps tolerance (testnet acceptance). Script emits `SMOKE_TEST_PASSED` event on success.
  Parallelization: Wave 4 | Blocked by: 30 | Blocks: 37 | can-parallelize-with: 31-34, 36
  References: `paper/synchra.tex:1156-1158` (MVP milestone success criterion — "working privacy-preserving perpetual futures market on Arc Testnet").
  Acceptance criteria (agent-executable): `forge script script/Smoke.s.sol --rpc-url ${ARC_TESTNET_RPC} --broadcast` exits 0 and final logs include `SMOKE_TEST_PASSED`. Balance assertions hold within 5 bps tolerance.
  QA scenarios: happy = full deposit→open→RFQ→close→withdraw cycle in <2 min; failure = intentionally revert at one step, assert script emits `SMOKE_TEST_FAILED at <step>`. Evidence `<attemptDir>/task-35-synchra-accelerator-mvp.log`
  Commit: Y | test(smoke): end-to-end Arc Testnet smoke from deposit to withdraw

- [ ] 36. Perp module hardening — invariant tests + fuzz
  What to do / Must NOT do: Implement `packages/contracts/test/BtcPerpMarketInvariant.t.sol` with `forge invariant funding-index-never-overflows` (assert funding index doesn't overflow for 10^6 iterations), `forge invariant margin-monotonic` (assert margin utilisation is monotonically non-decreasing only inside the position lifetime), and `forge invariant collateral-preserved` (assert total collateral in Account + locked-in-positions equals depositor pool). All run with `runs = 1000, depth = 5` per Foundry invariant docs. Add fuzz tests with `vm.assume` realistic boundaries (price 50k–150k, size 0.001–10 BTC). **Must NOT have:** property tests for portfolio-margin — scope-OUT.
  Parallelization: Wave 4 | Blocked by: 14, 22, 30 | Blocks: 37 | can-parallelize-with: 31-35
  References: `paper/synchra.tex:1078-1084` (smart-contract risk invariants — collateral cannot be created/destroyed outside authorized operations; settlement deterministic given same input).
  Acceptance criteria (agent-executable): `forge test --match-contract BtcPerpMarketInvariant -vvv` exits 0 with 3 invariant properties passing 1000 runs each. Test runtime ≤ 60s on CI.
  QA scenarios: happy = all 3 invariants green; failure = artificially break one invariant (skip leverage check), assert property fails. Evidence `<attemptDir>/task-36-synchra-accelerator-mvp.log`
  Commit: Y | test(perp): add invariant + fuzz property suite for market extension

- [ ] 37. E2E frontend Playwright suite — full flow on local anvil
  What to do / Must NOT do: Implement `packages/frontend/tests/e2e/full-flow.spec.ts` that runs against (1) local `anvil` with `DeployArc.s.sol` deployed, (2) the dev-controlled backend, (3) a mock Phala TEE service stub returning canned enclave signatures. Steps: visit `/wallet`, click Connect, wait for balances panel; visit `/account`, click Approve USYC, wait for confirmation, click Deposit 1000 USYC; visit `/trade`, submit an RFQ long 0.1 BTC max $100k, wait for a mock quote, click Accept; assert Recent Trades shows the position; visit `/account` and assert position appears with positive margin health. Assert the Row-of-Trade appears on the Arc Testnet landing via the e2e stub. After-test cleanup tears down the anvil node and the backend process.
  Parallelization: Wave 4 | Blocked by: 22, 30-36 | Blocks: F1-F4
  References: All previous todos; full integration snapshot.
  Acceptance criteria (agent-executable): `pnpm --filter @synchra/frontend test:e2e` exits 0 within 5 minutes. Screenshots dumped to `<attemptDir>/task-37-synchra-accelerator-mvp/screenshots/`.
  QA scenarios: happy = full flow from connect to position display within 5 min; failure = mock TEE returns bad signature, assert the trade UI surfaces "Settlement failed — attestation invalid". Evidence `<attemptDir>/task-37-synchra-accelerator-mvp.log`
  Commit: Y | test(e2e): Playwright full-flow suite against local anvil + mock TEE

## Final verification wave
> Runs in parallel after ALL todos. ALL must APPROVE. Surface results and wait for the user's explicit okay before declaring complete.
- [ ] F1. Plan compliance audit
- [ ] F2. Code quality review
- [ ] F3. Real manual QA
- [ ] F4. Scope fidelity

## Commit strategy

- One commit per todo, conventional-commits style (`feat(scope):`, `fix(scope):`, `chore(scope):`, `test(scope):`, `docs(scope):`, `ci:`).
- Scopes: `monorepo`, `shared`, `contracts`, `engine`, `clob`, `risk`, `rfq-matcher`, `oracle`, `clearing`, `markets`, `execution`, `tee`, `rfq`, `frontend`, `backend`, `deploy`, `smoke`.
- Each commit log closes with a trailer `Refs: synchra.tex:<lines>` citing the paper section the work fulfills.
- Atomic commits — each commit builds and `pnpm -r build` passes; tests pass via `pnpm -r test`.
- Wave boundaries tagged: `git tag wave-0-scaffold`, `wave-1-clearing`, `wave-2-perp-clob`, `wave-3-rfq-tee`, `wave-4-frontend-e2e`.
- Final MVP submission: tag `synchra-accelerator-mvp` once the smoke test on Arc Testnet passes.

## Success criteria

1. Arc Testnet deployment at the addresses saved in `packages/contracts/deployments/arc-testnet.json` is live and queryable.
2. `forge script script/Smoke.s.sol --rpc-url $ARC_TESTNET_RPC --broadcast` exits 0 with `SMOKE_TEST_PASSED` log.
3. `forge test` in `packages/contracts` exits 0 with ≥80 unit + fuzz + invariant tests passing.
4. `cargo test --workspace` in `packages/engine` exits 0.
5. `pnpm -r test` in root exits 0.
6. `pnpm --filter @synchra/frontend test:e2e` exits 0 within 5 minutes.
7. Gas benchmarks in `packages/contracts/gas_benchmarks.txt` recorded for each public + external contract function.
8. Phala DstackAppArc contract on Arc Testnet shows the registered composeHash and is queryable.
9. Frontend deployed to the hosting target recorded in `packages/frontend/deployments/arc-testnet.json` — wallet connect + USYC deposit + RFQ trade visible from a browser at the deployed URL.
10. `README.md` enable a fresh clone-and-follow run to execute a full deposit→open→RFQ→close→withdraw cycle on Arc Testnet.