---
slug: synchra-accelerator-mvp
status: awaiting-approval
intent: clear
review_required: false
pending-action: write .omo/plans/synchra-accelerator-mvp.md
approach: 6 component topology (clearing kernel contracts, BTC/USDC perp extension with CLOB, risk monitor, encrypted RFQ + Phala TEE matcher, Next.js frontend using Developer-Controlled Wallets + App Kit, Arc Testnet deployment) executed in 5 kanban waves with the Foundry+Cargo+Next.js monorepo scaffold as Wave 0; deps; TDD in all three langs; isolated margin only for MVP; ZK, multi-market PM offsets, FX, RWA, strategy vaults, agents all scope-out.
---

# Draft: synchra-accelerator-mvp

## Components (topology ledger)
<!-- Lock the SHAPE before depth. One row per top-level component that can succeed or fail independently. -->
<!-- id | outcome (one line) | status: active|deferred | evidence path -->

| id | outcome (one line) | status | evidence path |
| --- | --- | --- | --- |
| C1 | Clearing kernel smart contracts (Solidity/Foundry on Arc EVM) — accounts, collateral engine, position engine, settlement engine, isolated-margin enforcement. | active | paper/synchra.tex:339-421 (§Clearing Kernel) |
| C2 | BTC/USDC perpetual market extension with CLOB matching — Rust off-chain matching, on-chain settlement via IMarket callbacks. | active | paper/synchra.tex:561-575 (§Privacy-preserving perpetual module), 624-629 (§Public CLOB), 541-557 (§Extension interface) |
| C3 | Basic portfolio margin risk monitor — Rust off-chain risk computation, on-chain enforcement of margin/MR/liquidation triggers with the staged default waterfall. | active | paper/synchra.tex:424-531 (§Portfolio Margin Engine) |
| C4 | Encrypted RFQ module with TEE quote matching — encrypted RFQ flow, TEE enclave matcher, attestation-verified settlement. | active | paper/synchra.tex:631-638 (§RFQ), 705-748 (§Trusted Execution Environment), 1138-1156 (MVP deliverable) |
| C5 | Frontend (Next.js + TypeScript) for trading UI — wallet connect, RFQ submission, position viewer, on Arc Testnet with USYC collateral integration. | active | paper/synchra.tex:1154 (MVP frontend deliverable), Table~\ref{tab:stack} at 1200-1219 |
| C6 | Arc Testnet deployment — deploy all smart contracts, configure USDC gas, register USYC ERC-20 collateral tier, end-to-end smoke. | active | paper/synchra.tex:1155 (MVP deployment deliverable) |

## Open assumptions (announced defaults)
<!-- Record any default you adopt instead of asking, so the user can veto it at the gate. -->
<!-- assumption | adopted default | rationale | reversible? -->

| assumption | adopted default | rationale | reversible? |
| --- | --- | --- | --- |
| Repo layout (greenfield) | pnpm workspaces at repo root: `packages/contracts` (Foundry project), `packages/engine` (Cargo workspace), `packages/frontend` (Next.js), `packages/shared` (TS types shared across), plus top-level `paper/` retained. | Cross-language monorepo with shared types and one-command install/build is standard for Solidity+Rust+TS stacks. | Yes (low cost) |
| Test strategy | TDD across all three languages: forge unit + fuzz + invariant tests; Rust unit + proptest + tokio integration; Next.js vitest + Playwright E2E. | Smart-contract correctness is safety-critical and proptest/foundry fuzz are best practice; perp risk math is non-deterministic and needs property tests. | No (safety-critical) |
| BTC/USDC funding formula | Use exactly the formula in §markets (line 567): ΔF_t = clamp((P_mark − P_index) / P_index, −max_rate, +max_rate) · Δt; lazy PnL = Q · (F_current − F_entry) · P_index. | Paper-specced, do not deviate. | No |
| Outstanding oracle choice | Pyth primary (PythNetwork price feeds) + Chainlink secondary + on-chain CLOB TWAP tertiary with circuit breaker on 1%–5% divergence. | Paper §Security Analysis, lines 1055-1063, specifies the multi-oracle architecture. | No |
| Margin tier values for MVP | Initial margin ratio (IMR) = 5% for BTC/USDC (i.e., 20x ceiling), maintenance margin ratio (MMR) = 60% of IMR (= 3% of notional), liquidation threshold γ = 0.85. | Paper §markets says maintenance at 50%–75% of IM; midpoint 60%, standard practice for crypto perps MVP. | Yes (parameters in risk config — adjustable) |
| Liquidation waterfall for MVP | Standard liquidation only; backstop + ADL + contract unwind ship as stubs/enabled-but-not-triggered. | Paper §margin says the staged waterfall is the design, but for MVP "isolated margin, liquidation waterfall" (line 1143) — standard liquidation is the only active stage; backstop/ADL are framework stubs. | No (per MVP spec) |
| Insurance fund for MVP | Stub fund seeded with protocol treasury USDC (testnet credit); per-market and global fund accounting in-place but no LP capital. | MVP does not require live LP market — insurance accounting exists but is seeded from testnet treasury, matching "basic portfolio margin risk monitor". | No |
| Arc Testnet access | Use Circle's public Arc Testnet endpoint with developer-supplied USDC test tokens (faucet) and USYC test ERC-20 (deployed at 0xe9185F0c5F296Ed1797AaE4238D26CCaBEadb86C per librarian finding). | Arc mainnet expected summer 2026 still — Testnet is the only MVP target per the paper. | No |
| RFQ matching engine language | Rust (TDX compute runs Rust crates fully — dstack/oyster all support Rust; matching engine shares crates with the CLOB) | Paper §roadmap line 1152 + Table at 1209 specify Rust off-chain engine. | No |
| Output format | Kanban board layout INSIDE the standard ulw-plan template — i.e., the `## Todos` section is organized into kanban lanes (Backlog → In Progress by Wave → Done) where columns are release waves and rows are work items. | User explicitly asked "kanban board style plan". | No |

## Findings (cited - path:lines)

- `paper/synchra.tex:1138-1158` — Accelerator MVP definition (§roadmap §subsection 1): the three moats (leverage, privacy, programmable yield) and the explicit MVP deliverables list.
- `paper/synchra.tex:1200-1219` (Table stack) — fixed technology stack mapping per component.
- `paper/synchra.tex:339-421` (§Clearing Kernel) — kernel account model, collateral engine, position engine, settlement engine scope.
- `paper/synchra.tex:424-531` (§Portfolio Margin Engine) — margin model formulas (scenario-based margin f_S, concentration charge f_C, liquidity charge f_L, signed correlation adjustment f_K), insurance fund target.
- `paper/synchra.tex:533-616` (§Programmable Market Extensions) — IMarket interface, lifecycle callbacks, perpetual module, FX module, RWA module, strategy vault module, repo module.
- `paper/synchra.tex:619-651` (§Execution Layer) — public CLOB specs, RFQ flow, batch auctions.
- `paper/synchra.tex:706-748` (§Trusted Execution Environment) — TEE architecture, attestation flow, fallback path on attestation failure.
- `paper/synchra.tex:1055-1093` (§Security Analysis) — oracle anti-manipulation, liquidation cascade mitigations, kernel invariants.
- Repo state: greenfield — only `paper/synchra.tex`, `paper/*` build artifacts, `README.md`. No code, no `.omo/` artifacts prior to this draft.
- Librarian finding — TEE options brief (sourced): Phala (dstack) has mainnet TDX CVM $0.06–$0.23/hr + `DstackApp` on-chain governance + Turbine production order matcher; Marlin Oyster runs on AWS Nitro NSM with `AttestationVerifier.sol` deployed by caller and has the Kalypso orderbook in production; raw Intel TDX bare metal has no out-of-the-box on-chain governance and $9k–$30k capex.
- Librarian finding — Circle wallet SDKs (sourced): Developer-Controlled Wallets (REST, server-side keys, Arc Testnet quickstart confirmed) vs WaaS Web SDK (MPC, embedded UI with Next.js + TS quickstart, Arc support unverified for Web SDK); App Kit (`@circle-fin/app-kit@1.10.0`) is a payments-orchestration layer that adds Arc Testnet adapters and sits on top of either wallet type.
- Context7 resolve — Pyth best-match ID `/pyth-network/pyth-crosschain` (2431 snippets, high reputation, 78 benchmark) for Solidity consumer + SDK patterns; Arc-compatible since Arc is EVM-compatible and Pyth has EVM consumers.
- Context7 resolve — Foundry IDs `/foundry-rs/book` and `/websites/getfoundry_sh` (high reputation, 80+ benchmark) for Foundry setup, forge test, fuzz and invariant testing.

## Decisions (with rationale)

- **Decided internally (not asked):** adopt pnpm-workspaces monorepo layout, TDD, paper-specified BTC/USDC funding formula, paper-specified Pyth+Chainlink+TWAP oracle stack, MVP margin parameters (IMR 5%, MMR 60% of IMR, γ 0.85), Rust for both CLOB and TEE RFQ matcher, and the kanban-board layout inside the standard template. Rationale recorded under `Open assumptions`.
- **Owner decision RESOLVED — TEE provider:** **Phala Network (dstack)** chosen.
  - Rationale: mainnet-grade TDX CVMs at $0.06–$0.23/hr via Phala Cloud pay-as-you-go; native on-chain `DstackApp` enclave-identity governance on EVM (Arc is EVM-compatible so it deploys without modification); production precedent at Turbine/Fynd.io for an on-enclave orderbook/matching engine; open-source fallback (Dstack-TEE/dstack) for self-host if Cloud is outgrown; lowest ops-burden of the three options for an MVP timeline.
  - Plan must pin: Phala Cloud account setup, Rust `dstack-sdk` crate, `DstackApp` Solidity contract deployment on Arc Testnet, ZK-compressed attestation verifier deferred out of MVP scope (raw `DstackApp` view-call + secp256k1 signature verify on Arc is gas-cheap enough for MVP).
- **Owner decision RESOLVED — Frontend wallet SDK:** **Developer-Controlled Wallets + App Kit layered on top** chosen.
  - Rationale: server-side REST custody avoids per-user MPC/PIN UX on a privacy-sensitive perps MVP; Circle's dev-controlled quickstart explicitly runs on Arc Testnet; USYC is a plain ERC-20 on Arc Testnet for approve/balanceOf; `@circle-fin/app-kit@1.10.0` adds Arc Testnet adapters for the crosschain USDC ingest UX (CCTP) if/when added during MVP. Wallet-custody stays with the developer treasury keyset — simplifies testnet demo, liquidation workflow, and TEE-settled RFQ drains.
  - Plan must pin: REST wallets API with developer-held entity secret on a Node backend; `@circle-fin/app-kit` integrated in the Next.js frontend; native USDC (18-dec Arc gas) + USYC (6-dec, address per librarian finding) flows wired into the deposit/withdraw path.

## Scope IN

- Solidity clearing kernel: account abstraction, collateral vault (4-tier classification haircut applied, USDC gas differentiation), position storage, settlement batch processor, isolated-margin check on every position mutation.
- BTC/USDC perpetual extension implementing IMarket + lifecycle callbacks: funding index update + lazy PnL evaluation, oracle-tethered mark price (median of Pyth primary + Chainlink secondary + on-chain impact TWAP), leverage ceiling up to 20x, maintenance margin = 60% of initial margin.
- Rust off-chain CLOB matching engine emitting on-chain settlement batches; price-time priority; gas-optimized order storage on-chain for matching metadata; recycle fee for expired orders.
- Rust risk monitor that pulls account state, computes Σ f_S + f_C + f_L + f_K across the account's positions, and triggers liquidation when C_eff < M·γ; emits standard-liquidation calls only (backstop + ADL stubs).
- Encrypted RFQ module with TEE enclave quote matching using the chosen TEE provider; commit-reveal fallback for offline TEE; on-chain attestation verification gate before each settlement.
- Next.js + TypeScript trading frontend: wallet connect, RFQ submission flow, position viewer, margin health bar, liquidation state display, USYC approve-and-deposit flow.
- Arc Testnet deployment of the full system with a documented deployment script, USDC faucet path, USYC collateral registration, and a single end-to-end smoke test (deposit USYC → open BTC/USDC perp → RFQ block trade via TEE → close → withdraw).

## Scope OUT (Must NOT have)

- **NOT in scope — portfolio margin across heterogeneous markets:** MVP margin is isolated (per-position) only; cross-market f_S / f_C / f_L / f_K components are computed and read but the engine does NOT grant cross-market offsets to the trader. Paper line 1143 explicitly says "clearing kernel with isolated margin, liquidation waterfall".
- **NOT in scope — FX perpetual module, RWA module, strategy vault module, repo module.** All deferred to Phase 1/2.
- **NOT in scope — agent account system, session keys, Nanopayments integration.** Deferred to Phase 3.
- **NOT in scope — ZK solvency proofs, ZK-compressed TEE attestation, batch ZK verification.** Paper §ZK is explicitly out of MVP scope.
- **NOT in scope — batch auctions / FBAs.** Paper §execution lists them but MVP deliverable (line 1151) specifies CLOB matching plus RFQ; batch auctions are for Phase 1.
- **NOT in scope — mainnet deployment.** Testnet only per paper line 1155.
- **NOT in scope — backstop liquidation pool, auto-deleveraging, contract unwind execution paths.** Framework stubs only.
- **NOT in scope — risk scenario backtesting or formal verification report.** Paper §economics defers to mainnet pre-launch.
- **NOT in scope — formal verification of the clearing kernel.** Phase 1 per the paper (line 1163).

## Open questions

1. **TEE provider** (owner-decision — external dependency, vendor lock-in, ops cost): Phala Network (dstack) vs Marlin Oyster vs raw Intel TDX bare metal.
   - **Why I explored first and could not resolve:** the paper (line 1152) lists all three as "Intel TDX / Phala / Marlin" with no preference. Each has materially different ops burden, on-chain governance, and cost profiles. Not reversible after the matching engine + settlement contracts are written.
2. **Frontend wallet SDK** (owner-decision — public API surface, custody model): Circle Developer-Controlled Wallets (REST, server-side key custody) vs WaaS Web SDK (MPC, embedded React UI) — with App Kit optionally layered on either for crosschain/USDC-on-Arc flows.
   - **Why I explored first and could not resolve:** the paper (line 1154) says "Circle Developer Controlled Wallets or App Kit". The two wallet products have different custody models and Arc Testnet support status. Not reversible after the frontend auth + treasury flow is built.

## Approval gate
status: awaiting-approval
<!-- Exploration exhausted, both forks resolved, draft complete. Awaiting explicit user okay to write .omo/plans/synchra-accelerator-mvp.md only — execution starts in a separate worker session. -->
next-action: on approval, run scaffold (without --draft-only) to create .omo/plans/synchra-accelerator-mvp.md, then APPEND kanban-laned todo batches to its ## Todos region, fill ## TL;DR (For humans) LAST.