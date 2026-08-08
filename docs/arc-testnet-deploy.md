# Arc testnet deployment

Everything needed to take the kernel from "works on local Anvil" to "works
on real Arc testnet." Written after fixing three real, previously-unknown
bugs that would each have silently broken a live deployment — every fix
below is verified (unit tests, or a live local dry-run against a fresh
Anvil instance), not just reasoned about. This doc is the single place
those fixes' operational consequences live.

## What was actually broken, and is now fixed

### 1. `Account.withdraw()` reverted unconditionally, on every deployment

Neither `Deploy.s.sol` nor `DeployLocal.s.sol` ever called
`CollateralEngine.setBalanceSource(account)`. `RiskMonitor.isWithdrawSafe`
calls `CollateralEngine.effectiveCollateral`, which reverts
`BalanceSourceNotSet` when unset — so `Account.withdraw()` (which checks
`isWithdrawSafe` whenever a `riskMonitor` is wired, which both scripts also
do) reverted on **every single call**, on every deployment including local
dev. This predates this session's other work; it was never actually
exercised end-to-end before now.

Also missing: `RiskMonitor.setPositionEngine(...)`. Without it,
`effectiveMarginRequirement`'s fallback path (`currentMarginRequirement`,
consulted whenever no fresh TEE portfolio-margin attestation exists) reverts
`PositionEngineNotSet` instead of correctly returning zero for a
sealed-only trader who's never traded.

**Fixed** in both `script/Deploy.s.sol` and `script/DeployLocal.s.sol`:
`collateralEngine.setBalanceSource(address(account))`, and a dedicated,
permanently-empty `PositionEngine` deployed solely so the legacy fallback
degrades to "no position" instead of reverting. Verified live: a fresh
`forge script script/Deploy.s.sol:Deploy --broadcast` against a throwaway
Anvil instance now shows `collateralEngine.balanceSource()` and
`riskMonitor.positionEngine()` both correctly non-zero (previously both
`address(0)`).

### 2. On-chain marketId would never match between the matcher and the deployed contracts

`FxPerpMarket`'s own `marketId` immutable "doubles as the Pyth feed ID"
(`OracleHub.sol`'s own doc on `_fetchPrices`) — so on a real deployment the
on-chain marketId **is** Pyth's real, externally-fixed feed ID for that
pair. But the matcher (`api.rs`, `market_maker.rs`, `keeper_liquidator.rs`)
always computed the on-chain marketId as `keccak256(market_id_string)` —
e.g. `keccak256("EURC/USDC")` — with no way to override it. Those two
values are never going to be equal on a real chain; only local dev worked,
because `DeployLocal.s.sol` deliberately sets its (mock) feed id to exactly
`keccak256("EURC/USDC")` so the two happen to agree there.

Left unfixed, this would have meant **every settlement call on Arc reverts
`MarketNotRegistered`**, silently, the first time anyone tried to trade —
a deploy that "succeeds" and then does nothing.

**Fixed**: added a configurable on-chain-marketId override, defaulting to
the old hash behavior (so local dev is unaffected — confirmed, all 172
matcher tests and 361 contract tests still pass):
- Matcher (`cerdic-tee-matcher`): `CERDIC_MARKET_ID_OVERRIDES` env var,
  `marketId=0xOnChainId` pairs, consumed via the new
  `AppState::onchain_market_id` (one function, six call sites converted to
  use it — was previously six separate inline hashes).
- `market_maker`: `ONCHAIN_MARKET_ID` env var (single market per process).
- `keeper_liquidator`: `KEEPER_MARKET_ONCHAIN_IDS`, same shape as
  `KEEPER_MARKET_CONTRACTS`.
- `keeper_price_pusher` needed no change — it already takes real feed ids
  directly (`PYTH_FEED_IDS`/`FX_MARKET_IDS`), never derived them from a
  string.

**You must set `CERDIC_MARKET_ID_OVERRIDES` (and the two keeper
equivalents) on Arc**, to the exact same value you pass as
`EURC_USDC_PYTH_FEED_ID` to `Deploy.s.sol`. See the deploy steps below —
skipping this silently reintroduces the bug.

### 3. Portfolio-margin attestation (fixed earlier this session, restated here)

`Account.withdraw()`'s margin check only sees a trader's real sealed
exposure when the TEE has recently submitted a fresh
`RiskMonitor.submitPortfolioMargin` attestation. This is wired
(`crates/cerdic-tee-matcher`, `attest_portfolio_margin` in `api.rs`, called
after every settled fill) but depends on `CERDIC_RISK_MONITOR_CONTRACT`
being set — see env vars below.

## What's still open (by design, not an oversight)

Per `docs/security-audit-tee-contracts.md`'s own verdict: **H1–H3 are
explicitly called "acceptable on testnet where we operate the TEE and
router admin keys"** — true for an Arc testnet deploy under your own keys.
Not fixed, and not blocking:

- **H1** — no on-chain conservation check across a sealed match/sweep's
  legs. A fix was attempted this session and reverted: the audit's proposed
  invariant (`deltaA + deltaB == 0`) doesn't hold for a real open, where
  each side independently locks its own `required_margin` — enforcing it
  would revert every real trade. The correct fix needs someone to define
  what "conserved" means given that, and hasn't been done.
- **H2** — `liquidateSealed` trusts the TEE's `marginRequirement` entirely.
- **H3** — trust root is `AttestationRouter`'s admin allowlist, not a real
  enclave attestation verifier.

C1/C2 (permissionless freeze, broken liquidation formula) **are** fixed,
but live in `LiquidationEntry`/`CapabilityRegistry`, which no deploy script
(including the ones here) deploys — they're not part of the live sealed
trading path at all today.

## Deploy sequence

### 0. Test collateral token

This deployment deliberately does NOT use Arc's real Circle-issued USDC as
the collateral asset (Arc's real USDC is the network's native gas currency
instead, unrelated to CollateralEngine's registered assets) — it uses a
self-deployed, self-serve-mintable `TestUSDC.sol`
(`packages/contracts/src/testnet/TestUSDC.sol`) instead, so onboarding a new
trader never depends on sourcing real testnet USDC from anywhere.

```bash
cd packages/contracts
PRIVATE_KEY=<deployer key, becomes admin/bulk-minter>
forge script script/DeployTestUSDC.s.sol:DeployTestUSDC --rpc-url <arc-testnet-rpc> --broadcast
```

Prints the `TestUSDC` address — that's what `ARC_USDC_ADDRESS` below means
for this deployment. New traders get their own starting balance via the
contract's own `claimFaucet()` (10,000 tUSDC, 24h cooldown, called directly
from their wallet — wired into the frontend's AccountPanel as "Get test
USDC"); `adminMint(to, amount)` bulk-seeds market makers/backstop liquidity.

### 1. Contracts

Required env vars for `Deploy.s.sol` (real, external, Arc-specific — this
script does not fabricate them, and neither can this doc):

```bash
PRIVATE_KEY=<deployer key, becomes admin on every contract>
ARC_USDC_ADDRESS=<TestUSDC address from step 0>
ARC_PYTH_CONTRACT=0x2880aB155794e7179c9eE2e38200202908C17B43
EURC_USDC_PYTH_FEED_ID=0xa995d00bb36a63cef7fd2c287dc105fc8f3d93779f062f09551b0af3e81ec30b
```

`ARC_PYTH_CONTRACT` above is Pyth's own documented Arc Testnet receiver
address (`docs.pyth.network/price-feeds/core/contract-addresses/evm`,
"Arc Network Testnet" row) — confirmed live, not just copied: `eth_getCode`
against Arc's own RPC returns real deployed bytecode at this address (an
EIP-1967-style proxy pattern, consistent with how Pyth deploys), not an
empty account.

No `ARC_EURC_ADDRESS`: this is cash-settled — EURC/USDC is the market's price
pair, not a token that ever moves. Collateral is USDC only.

`EURC_USDC_PYTH_FEED_ID` is not actually chain-specific and was never a real
unknown — Pyth feed IDs are one global identifier per price series (the value
above is confirmed live against Hermes' own public registry,
`https://hermes.pyth.network/v2/price_feeds?query=EUR/USD&asset_type=fx`),
identical across every chain a Pyth receiver reads it on. Only
`ARC_PYTH_CONTRACT` (the receiver contract's own deployed address) is
genuinely Arc-specific and still needs a real lookup.

Dry-run first (no `--broadcast`), exactly per the script's own doc:

```bash
cd packages/contracts
forge script script/Deploy.s.sol:Deploy --rpc-url <arc-testnet-rpc>
```

Then broadcast for real:

```bash
forge script script/Deploy.s.sol:Deploy --rpc-url <arc-testnet-rpc> --broadcast
```

Record every logged address — `Account`, `CollateralEngine`,
`RiskMonitor`, `OracleHub`, `AttestationRouter`, `FxPerpMarket
(EURC/USDC)`. You'll need all of them below.

There is currently no Arc-real equivalent of `DeployPerpMarketLocal.s.sol`
(the BTC/USDC market) — that script only exists in a local-mock-Pyth form.
`docs/keepers.md` already documents keeper configs assuming a real BTC/USDC
market exists on Arc; if you want BTC/USDC live too, write a
`DeployPerpMarket.s.sol` mirroring `DeployPerpMarketLocal.s.sol` but taking
a real Chainlink BTC/USD aggregator address and real Pyth feed id via env,
the same pattern `Deploy.s.sol` already uses for the FX leg. Not built here
— doing it blind risks fabricating a wrong aggregator/feed address for a
network this session has no live access to.

### 2. Authorize the TEE signer

The matcher generates its own settlement-signing key on first boot and
prints its address (`enclave keypair generated` log line, or
`pubkey_b64`). It must be authorized before it can settle anything:

```bash
cast send <AttestationRouter address> \
  "authorizeTEE(address)" <matcher's settlement signer address> \
  --rpc-url <arc-testnet-rpc> --private-key <admin key>
```

(`local_dev.rs`'s `fund_and_authorize` does exactly this for local dev —
same call, real network.)

### 3. Start the matcher

```bash
SETTLEMENT_RPC_URL=<arc-testnet-rpc>
CERDIC_SETTLEMENT_CONTRACTS="EURC/USDC=<FxPerpMarket address>"
CERDIC_ACCOUNT_CONTRACT=<Account address>
CERDIC_COLLATERAL_ASSET=<ARC_USDC_ADDRESS>
CERDIC_RISK_MONITOR_CONTRACT=<RiskMonitor address>
CERDIC_MARKET_ID_OVERRIDES="EURC/USDC=<EURC_USDC_PYTH_FEED_ID>"
CERDIC_ORACLE_FEEDS="EURC/USDC=<EURC_USDC_PYTH_FEED_ID>"
CERDIC_DB_PATH=/path/to/a/PERSISTENT/disk/cerdic-state.redb
CERDIC_LOG=info
# Do NOT set CERDIC_ENABLE_DEBUG_SEED on Arc — it lets /debug/seed-history
# rewrite the public trade tape (T8, security-audit-tee-contracts.md).
cargo run --release --bin cerdic-tee-matcher
```

`CERDIC_DB_PATH` (`persistence.rs`, `redb`-backed) is what makes candle
history, replay-protection nonces, and a few other previously-ephemeral
fields survive a **process** restart. It does NOT survive the underlying
disk/VM being replaced — same posture as `kms.rs`'s GCS-stored secrets
blob, just local instead of cloud-stored. On Confidential Space
specifically: point this at a real persistent disk attached to the VM, not
the default ephemeral boot disk, or a VM replacement (redeploy, autoscale
event) silently resets it to empty — no error, just a fresh start, exactly
`persistence::load`'s own "fresh database" behavior. Unset, it defaults to
`cerdic-state.redb` in the process's working directory, fine for local dev
only.

`CERDIC_MARKET_ID_OVERRIDES` and `CERDIC_ORACLE_FEEDS` both need
`EURC_USDC_PYTH_FEED_ID` — the same real Pyth feed id you passed to
`Deploy.s.sol` — but for two different reasons: the first makes settlement
calldata target the right on-chain position (fix #2 above), the second is
purely this process's own price-fetching config (`oracle.rs`, unrelated to
on-chain marketId).

### 4. Start the keepers (`docs/keepers.md` has full detail per binary)

`keeper_price_pusher` is not optional — without it, every funding
checkpoint reverts `StalePrice` within 60 seconds:

```bash
SETTLEMENT_RPC_URL=<arc-testnet-rpc>
PYTH_CONTRACT_ADDRESS=<ARC_PYTH_CONTRACT>
KEEPER_PRIVATE_KEY=<funded keeper key>
PYTH_FEED_IDS=<EURC_USDC_PYTH_FEED_ID>
FX_MARKET_IDS=<EURC_USDC_PYTH_FEED_ID>
ORACLE_HUB_ADDRESS=<OracleHub address>
cargo run --release --bin keeper_price_pusher
```

`keeper_liquidator`, with the new override wired in:

```bash
MATCHER_URL=http://<matcher-host>:8787
SETTLEMENT_RPC_URL=<arc-testnet-rpc>
KEEPER_LIQUIDATOR_ADDRESS=<address to receive keeperReward>
KEEPER_MARKET_CONTRACTS="EURC/USDC=<FxPerpMarket address>"
KEEPER_MARKET_ONCHAIN_IDS="EURC/USDC=<EURC_USDC_PYTH_FEED_ID>"
KEEPER_START_BLOCK=<Deploy.s.sol's broadcast block, not 0>
cargo run --release --bin keeper_liquidator
```

`keeper-fx-rate.sh` for funding, per `docs/keepers.md`'s own section —
unaffected by the marketId fix, it already used real feed ids directly.

### 5. Set discovery bounds — REQUIRED, not just weekend gap protection

`docs/keepers.md` frames this as weekend/closed-market FX gap protection,
which undersold it: confirmed live deploying to Arc — `OracleHub.markPrice`
(without bounds enabled) needs BOTH a live Pyth read AND a live Chainlink
read to succeed (`_fetchPrices`), and `Deploy.s.sol` never wires a Chainlink
FX aggregator (there is no real Arc-deployed Chainlink EUR/USD feed this
session had access to). Left unset, `markPrice` reverts `AggregatorNotSet`
on every single call — not degraded, completely unusable, which means every
funding checkpoint and every margin read through `RiskMonitor` breaks too.
Enabling discovery bounds is what makes `markPrice` fall back to the
reference price instead of reverting when the Chainlink leg is missing
(`_tryFetchPrices`'s non-reverting `ok=false` path) — do this immediately
after step 1, before starting the matcher, not as a later hardening pass:

```bash
cast send <OracleHub address> \
  "setDiscoveryBounds(bytes32,bool,uint256,uint16,uint8)" \
  <EURC_USDC_PYTH_FEED_ID> true <initial reference price, 1e18-scaled> 500 2 \
  --rpc-url <arc-testnet-rpc> --private-key <admin key>
```

Get a real current reference price from Hermes directly, don't guess it:
`curl "https://hermes.pyth.network/v2/updates/price/latest?ids[]=<feed_id>&parsed=true"`,
then `price * 10^expo * 1e18`. Verify it actually worked before moving on —
`markPrice` should return a real, non-reverting value:

```bash
cast call <OracleHub address> "markPrice(bytes32)(uint256)" <EURC_USDC_PYTH_FEED_ID> --rpc-url <arc-testnet-rpc>
```

### 6. Frontend env vars (`app/.env`)

```bash
VITE_ACCOUNT_ADDRESS=<Account address>
VITE_USDC_ADDRESS=<ARC_USDC_ADDRESS>
VITE_MATCHER_URL=http://<matcher-host>:8787
VITE_ACTIVE_CHAIN=<Arc testnet chain config Privy/viem expects>
VITE_CHAIN_RPC_URL=<arc-testnet-rpc>
VITE_PRIVY_APP_ID=<your Privy app id>
VITE_PRIVY_CLIENT_ID=<your Privy client id>
```

`VITE_PIMLICO_API_KEY`/`VITE_CLIENT_KEY`/`VITE_CLIENT_URL`/`VITE_WEB` are
whichever account-abstraction/paymaster provider you're pairing with Privy
on Arc, not something this repo fixes a value for.

## Before calling this "prepared," still do

1. **Restart against this exact fixed stack and run one real deposit → open
   → wait for a fill → withdraw cycle**, on Arc or a fresh local dry run.
   Nothing above has been exercised end-to-end with a live matcher process
   yet — the fixes are verified by unit test and one-off `cast call`
   checks, not a full user-facing flow. This is the single highest-value
   next step.
2. Decide on H1 for real, before any deployment holds value worth
   protecting — testnet-acceptable per the audit is not the same as safe
   indefinitely.
3. If BTC/USDC (or any second market) matters for the testnet launch,
   write the real `DeployPerpMarket.s.sol` — see the note under step 1
   above.
