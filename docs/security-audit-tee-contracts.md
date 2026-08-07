# Security audit: TEE + contracts flow

Date: 2026-08-06
Scope: `packages/contracts/src/` (full clearing kernel + markets) and
`crates/cerdic-tee-matcher/` (full TEE matcher). Method: direct code read of the
settle/attestation/liquidation paths, two parallel deep-reads of the remainder,
every finding below verified against the code before inclusion.

## Verdict

**Architecture: solid for MVP.** Layering is right — opaque position bytes with
market extensions, sealed vs plaintext settlement paths, sim-validated backstop
and funding design, and the trust model is honestly documented in the
docstrings themselves (each TEE-asserted input says so).

**Code: solid for a testnet MVP after fixing C1 and C2** (both are a few hours
of work). H1–H3 are acceptable on testnet where we operate the TEE and router
admin keys; they become must-fix before real capital.

**Privacy: not "truly private" yet.** The cryptography at rest and in transit
is correct, but the boundary leaks at the edges. Details in Part 1.

---

## Part 1 — Privacy: what actually leaks

### Visible on-chain per sealed match (verified `SettlementEngine.sol:246-312`)

| Item | Visible? | Consequence |
|---|---|---|
| `marketId` | plaintext | which market every position is in |
| `portfolioKeyA` + `portfolioKeyB` in one tx | plaintext | counterparty pairing of every trade |
| `collateralDeltaA/B` | signed plaintext | direction/magnitude inference (below) |
| `sealedParams` | variable-length ciphertext | blob length correlates with content |
| `SealedPositionTouched(portfolioKey, marketId)` | event, per leg | activity timeline per key per market |
| `PortfolioLiquidated` | event with plaintext `marginRequirement`, `collateralBefore`, `liquidator` | liquidated portfolio's risk size and total collateral — beyond the stated privacy model |
| side / size / price | not in calldata | the core claim holds on-chain |

### Structural leak 1: `loadSealed` is a public view (`SettlementEngine.sol:396`)

The full collateral trajectory of every portfolioKey is public state, forever.
Combined with deltas: at open, `|delta| ≈ margin = notional / leverage`; with
public mark price and `LEVERAGE_CEILING = 20`, position size is bounded within
the leverage-uncertainty band, tightening with repeated open/close cycles.
Sign/pairing patterns distinguish opens from closes. Not exact reconstruction —
but substantially more than "nothing leaks."

### Structural leak 2: the off-chain API is a public tape (verified `api.rs:1048-1061`)

`/orderbook`, `/ws/orderbook`, `/trades`, `/candles`, `/oi/:market` are
unauthenticated and expose per-fill price + quantity, plus per-market total
collateral and position count. Privacy is on-chain only; anyone watching the
matcher API sees the tape in plaintext. `/oi/:market` additionally serves as an
unauthenticated eth_call amplifier (uncached, unrate-limited).

### Identity linkage

- ✅ `portfolioKey` is a keyed hash derived in-enclave; `/portfolio-key`
  self-lookup is signature-gated and domain-separated. Sound.
- ⚠️ **Attestation binds the settlement key, not the encryption key**
  (verified `api.rs:1081-1087`): the OIDC nonce is the lowercased settlement
  address only. A client encrypting orders to `pubkey_b64` has no proof the
  X25519 key belongs to the attested enclave — a host-level MITM could swap
  the encryption pubkey and read order flow while settlement stays honest.
  Fix: commit both keys in the nonce.
- ⚠️ Deposit→portfolioKey funding linkage not fully traced; if a public deposit
  event precedes a fresh key's first settleMatch, timing/amount links the
  depositor to the key. Open verification item.
- ⚠️ The backstop key is recognizable by construction (derived from
  `Address::ZERO`, absorbs unserved flow by default with `notional_cap = MAX`).

---

## Part 2 — Correctness findings

### 🔴 C1 — `CapabilityRegistry.checkAndFreezeOnBreach`: anyone can permanently freeze any account

`CapabilityRegistry.sol:177-191`:

```solidity
function checkAndFreezeOnBreach(address trader, uint256 realizedLossTodayUsd, uint256 drawdownBps)
    external                       // no access control
    returns (bool breached)
{
    breached = checkBreach(trader, realizedLossTodayUsd, drawdownBps);  // caller-supplied
    ...
    account.freezeAccount(trader);   // no unfreeze exists in Account.sol
```

Two independent fatals: the loss figures are caller-supplied (pass
`type(uint256).max` and anything breaches), and `checkBreach` returns `true`
for accounts with no capability at all (`:168`). Any address — trader, Vault,
random — can be permanently frozen by anyone.

**Fix:** restrict the caller (kernel/TEE only), source loss figures from kernel
state rather than calldata, never freeze capability-less accounts, add an
unfreeze path.

### 🔴 C2 — `LiquidationEntry.checkAndFlag`: flags every levered position, healthy or not

`LiquidationEntry.sol:129`:

```solidity
flagged = notionalValue * PERCENT_DENOMINATOR >= effectiveCollateral * LIQUIDATION_GAMMA_PERCENT;
// notional × 100 ≥ collateral × 85
```

This compares notional to collateral, not equity to maintenance margin. A 20x
position has notional = 20× collateral, so the predicate is *always* true
regardless of PnL. The flag calls `freezeAccount` (permanent), and
`executeStandardLiquidation` (`:143`) checks only the frozen flag — no health
re-check — then pays the caller 50% of the 1% penalty. Result: permissionless,
profitable liquidation of perfectly healthy levered accounts on the plaintext
path. The discovery-bounds fallback guard is good work, but the core formula
is not a margin check.

**Fix:** flag when `effectiveCollateral + unrealizedPnL < maintenanceMargin`;

### 🟠 H1 — No conservation check in sealed settlement (verified)

`_settleOneMatch` (`SettlementEngine.sol:368`) applies each leg independently;
`_applySealedLeg` (`:375`) checks only `newCollateral >= 0`. Nothing requires
`deltaA + deltaB == -fee`. An authorized TEE key can submit
`deltaA = +X, deltaB = 0` and mint collateral. The kernel's on-chain invariants
today are exactly three: attested caller, matchId replay, per-leg
non-negativity. Necessary, not sufficient.

**Fix:** `deltaA + deltaB + fee == 0` per match (one line), sum check across
sweep legs. Long-term: the ZK margin-correctness verifier the design already
reserves (`IZkVerifier`) — note its current public inputs include
`match_price, match_size`, which would leak on-chain; commitments-only first.

### 🟠 H2 — `liquidateSealed` trusts the TEE's number entirely (verified)

`SettlementEngine.sol:315-365`: `marginRequirement` is TEE-asserted;
`collateralBefore` is honestly summed from storage, but leg-list completeness
is trusted (the docstring itself flags that an incomplete list "could let a
healthy portfolio look underwater"), and full verification is "a tracked
follow-up." An authorized TEE key can liquidate any portfolio; the `liquidator`
reward address is caller-chosen.

**Fix (pre-ZK):** challenge window, or require the takeover leg to settle at
an oracle-checked price.

### 🟠 H3 — Trust root is an admin allowlist (verified)

`AttestationRouter.authorizeTEE` is `ROUTER_ADMIN_ROLE`-gated — one key
authorizes arbitrary "TEE" addresses. `TeeAttestationVerifier` implements real
RSA-via-modexp but is self-admittedly dead code ("NOT been tested against a
real attestation token… the admin-allowlist path stays the production path").
Reported subsidiary issue: its substring claim-checks may not bind
payload/expiry to the signed bytes — must be fixed before it gates anything.

**Fix:** wire the verifier (after binding review), keep the admin allowlist
only as a timelocked fallback.

### 🟡 TEE-side findings (parallel deep-read; endpoints verified, internals spot-checked)

| # | Finding | Note |
|---|---|---|
| T1 | Reverted settlement txs invisible above `broadcast()` | TEE can believe a match settled when it didn't — settlement black hole |
| T2 | Broadcast mutex can wedge all settlement on one stuck tx | availability |
| T3 | Unit divergence: matcher margin math in raw `u64` ticks vs contracts' `1e18` | also flagged in `backstop.rs` docs; resolve before margin correctness matters |
| T4 | Restart amnesia: portfolio→market index is memory-only | post-restart, `/liquidation-check` can false-negative on pre-restart positions — liquidation-safety gap |
| T5 | KMS race: two instances booting against an empty bucket diverge sealed keys | single-instance only until fixed |
| T6 | `/liquidate` unauthenticated with caller-chosen reward address | by design (permissionless), but the TEE's margin verdict is the only gate — fragile given T1/T3/T4 |
| T7 | `MAX_SANE_TICK` guard + unauthenticated `/liquidation-check` hint-folding | live DoS surfaces, per the deep-read |
| T8 | `/debug/seed-history` rewrites the public tape when env-enabled | never enable on a consumed deployment |

### 🟡 Contract lows

- `OrderBook.modifyOrder` skips margin re-validation (`OrderBook.sol:151-165`).
- `OrderBook.sol:140`: `expiryBlock` truncated to u64 while the signed digest
  commits to the full uint256 — signature/storage mismatch past 2⁶⁴.
- `BackstopLedger.drawSubsidy` anti-double-draw depends on off-chain behavior:
  re-attesting pre-draw PnL re-opens a paid draw. Admin-gated, bounded.
- `Account.getPosition` / `creditRecords` read storage nothing writes — dead
  getters, misleading.
- `settleTrade`/`validateOpen` never check that either side actually *holds*
  collateral — solvency rests on the matcher role + withdrawal gating +
  liquidation. Acceptable for MVP, worth stating.

---

## Part 3 — Is the flow right?

**Order intake crypto: correct.** NaCl box with fresh ephemeral keys and
nonces, inner EIP-191 signature as the sole auth, AES-256-GCM with fresh random
nonces per seal (no reuse), matchId replay guard on-chain, X25519 rotation per
boot, no plaintext order logging. Genuinely well done.

**Architectural issue:** the TEE computes margin and the kernel applies it
blindly. Solvency rests entirely on TEE honesty + matcher robustness, and the
matcher has the operational surfaces in T1–T8. The missing kernel invariant is
conservation (H1) — the single highest-value one-line fix in the system.

**Fix order:**

1. **C1, C2** — permissionless freeze / liquidation of healthy accounts. Hours.
   Block any demo with real users until done.
2. **H1** — settlement conservation check. One line, largest trust reduction.
3. **H3** — real attestation + commit the encryption pubkey in the nonce.
4. **H2** — liquidation challenge window or oracle-tied takeover.
5. Privacy hardening: trim `PortfolioLiquidated` fields, document the public
   tape honestly, trace the deposit-linkage path, rate-limit `/oi`.
6. T1–T5 — TEE operational correctness.


---

## Part 4 — Recommended fixes (code level)

### C1 — `CapabilityRegistry.checkAndFreezeOnBreach`

Gate the caller, source the numbers from the kernel instead of calldata, never
touch capability-less accounts, and add the missing unfreeze path:

```solidity
bytes32 public constant RISK_ROLE = keccak256("RISK_ROLE"); // kernel or attested TEE

function checkAndFreezeOnBreach(address trader) external returns (bool breached) {
    if (!hasRole(RISK_ROLE, msg.sender)) revert NotAuthorized();
    if (!isActive(trader)) return false;                    // capability-less: never freeze
    (uint256 lossToday, uint256 ddBps) = riskSource.lossMetrics(trader); // kernel-sourced
    if (!checkBreach(trader, lossToday, ddBps)) return false;
    capabilities[trader].revoked = true;
    account.freezeAccount(trader);
    emit LimitBreached(trader, lossToday, ddBps);
    return true;
}
```

```solidity
// Account.sol — pair every freeze with its inverse:
function unfreezeAccount(address trader) external onlyRole(CLEARING_ADMIN_ROLE) {
    accounts[trader] = true; // restore the flag however freeze stores it
    emit AccountUnfrozen(trader);
}
```

Until `riskSource` exists, the interim fix is the role gate alone — it removes
the permissionless attack even with caller-supplied numbers.

### C2 — `LiquidationEntry.checkAndFlag`

Flag on **equity vs maintenance margin**, and re-validate at execution:

```solidity
// flag: equity = effective collateral + unrealized PnL at mark
(int256 equity,) = _accountEquity(trader, marketId);   // uses entry price + mark
uint256 maintenance = notionalValue * MMR_BPS / BPS_DENOMINATOR;
flagged = equity < int256(maintenance);
```

```solidity
// executeStandardLiquidation, before any state change:
(int256 equity,) = _accountEquity(trader, marketId);
if (equity >= int256(_maintenance(position, markPrice))) revert NotLiquidatable(trader, marketId);
```

Plus an unflag path so a recovered account isn't permanently executable. The
plaintext path has everything on-chain to do this (entry price + size in the
position record, mark from the oracle) — no TEE dependency needed.

### H1 — settlement conservation (the one-line fix)

Collateral becomes a closed system: it only moves between keys, never created
at settlement. Route fees to an explicit protocol key as a third leg:

```solidity
// in _settleOneMatch:
if (m.collateralDeltaA + m.collateralDeltaB + feeDelta != 0) revert NonConservedDeltas();

// in settleTakerSweep:
int256 sum = collateralDeltaTaker + feeDelta;
for (uint256 i; i < makerLegs.length; ++i) sum += makerLegs[i].collateralDelta;
if (sum != 0) revert NonConservedDeltas();
```

Corollary to pin down in the same change: the deposit path is then the *only*
place positive deltas originate — the TEE's deposit-crediting must reference a
verifiable on-chain deposit (event + amount), or conservation is trivially
satisfied while deposits are fabricated.


### H2 — `liquidateSealed`

Two independent hardenings, both cheap:

1. **Complete legs by construction.** Index markets per key on-chain so the leg
   list can't understate collateral:

```solidity
mapping(bytes32 => bytes32[]) internal _portfolioMarkets; // key -> markets touched

// in _applySealedLeg, when a position transitions from empty to non-empty:
_portfolioMarkets[portfolioKey].push(marketId);

// in liquidateSealed:
bytes32[] storage touched = _portfolioMarkets[portfolioKey];
if (legs.length != touched.length) revert IncompleteLegs();
// + membership check per leg
```

2. **Challenge window for the requirement itself.** `liquidateSealed` freezes
   and starts a window; the owner (via the TEE) may post a counter-attestation
   showing `collateralBefore >= marginRequirement`; if none lands in N blocks,
   the liquidation completes. `marginRequirement` stays TEE-asserted until the
   ZK verifier lands, but a wrong assertion becomes contestable, not instant.

### H3 — attestation path

- Bind both keys in the OIDC nonce:
  `nonce = lowercase(settlementAddress) ++ ":" ++ hex(x25519Pubkey)` — closes
  the order-flow MITM.
- Before `TeeAttestationVerifier` gates anything: verify the signature over the
  exact `header.payload` bytes whose claims are substring-checked, and parse
  `exp` from those same bytes (no separate, unverified payload source).
- Keep `AttestationRouter`'s admin allowlist as fallback, behind a timelock.

### Privacy hardening (post-MVP)

- `PortfolioLiquidated`: drop `marginRequirement`/`collateralBefore` from the
  event (keep `portfolioKey` + outcome); the owner can query details via the
  TEE, the public doesn't need them.
- `loadSealed` collateral exposure is load-bearing for on-chain solvency —
  keep for MVP, but document the leak honestly in the paper's privacy section
  rather than claiming calldata-only visibility.
- `/oi/:market`: cache + rate-limit. Public tape (`/trades`, `/candles`):
  document as a design choice ("venue-visible tape, chain-private positions"),
  not a bug.
- Deposit linkage: credit sealed collateral in epochs (batch deposits), so a
  deposit event doesn't map 1:1 onto a fresh key's first settleMatch.

### TEE operational (T1–T8)

- **T1** after every broadcast, read the receipt; on revert, mark the match
  unsettled locally and raise. Add a reconciliation loop diffing local settled
  state vs on-chain `settledMatches`.
- **T2** replace the single global broadcast mutex with per-market queues or a
  nonce manager with retry/backoff.
- **T3** pin the tick↔1e18 conversion in the market-registration config as the
  single source of truth; add TEE↔contract margin equivalence tests.
- **T4** rebuild the portfolio→markets index on boot from the public
  `SealedPositionTouched` event stream (the keeper already does exactly this).
- **T5** GCS write preconditions (generation-match) for the KMS blob, or
  document single-instance deployment as a hard requirement.
- **T7** authenticate `/liquidation-check` hint folding, or validate hinted
  markets against the on-chain index (H2 fix #1 makes this free).
- **T8** compile `/debug/seed-history` out of production builds with a cargo
  feature, not an env var.

re-validate health inside `executeStandardLiquidation`; add unfreeze.
