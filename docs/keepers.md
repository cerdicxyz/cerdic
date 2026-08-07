# Keeper infrastructure

Four real keeper/bot binaries, all in `crates/cerdic-tee-matcher/src/bin/`,
all built and verified against a real local Anvil + matcher deployment
this session (see each binary's own module doc for exact verification
notes and known limitations — this doc is deployment instructions, not a
repeat of that detail).

## 1. `keeper_price_pusher` — required, not optional

Pushes real, guardian-signed Pyth price updates on-chain. Without this
running continuously, every sealed-market funding checkpoint reverts
`StalePrice` once the on-chain Pyth price is more than 60 seconds old —
hit repeatedly and directly this session, the single most common failure
mode in every local test that didn't have a fresh manual price push right
before it. On a real testnet this is not cosmetic: no trade settles past
its first minute without it.

Can only be end-to-end tested against a real Pyth receiver contract
(Arc testnet/mainnet), not local Anvil's `MockPyth` — see the binary's
own module doc for why.

```bash
SETTLEMENT_RPC_URL=<arc-testnet-rpc> \
PYTH_CONTRACT_ADDRESS=<real Pyth contract on Arc testnet> \
KEEPER_PRIVATE_KEY=<funded keeper key> \
PYTH_FEED_IDS=a995d00bb36a63cef7fd2c287dc105fc8f3d93779f062f09551b0af3e81ec30b,e62df6c8b4a85fe1a67db44dc12de5db330f7ac66b72dc658afedf0f4a415b43 \
KEEPER_POLL_INTERVAL_SECS=20 \
FX_MARKET_IDS=a995d00bb36a63cef7fd2c287dc105fc8f3d93779f062f09551b0af3e81ec30b \
ORACLE_HUB_ADDRESS=<deployed OracleHub address> \
cargo run --release --bin keeper_price_pusher
```

Run one instance per (RPC, feed set) pair; feed ids should cover every
market's Pyth feed at once, not one process per market.

`FX_MARKET_IDS`/`ORACLE_HUB_ADDRESS` are optional, and needed only for
markets with `OracleHub.setDiscoveryBounds` enabled (see the discovery-bounds
section below): every id in `FX_MARKET_IDS` must also be listed in
`PYTH_FEED_IDS`. Two effects (`crates/cerdic-tee-matcher/src/market_hours.rs`,
`docs/trade-xyz-research.md` sections 1/9): a large move on a closed FX
weekend logs at `info`, not `warn` (an expected gap, not an alarm), and while
the FX week is open this keeper also calls `OracleHub.refreshDiscoveryReference`
for each configured id, keeping that market's discovery-bounds reference
walking forward with real price moves instead of going stale.

## 2. `keeper_liquidator` — the spec's own liquidation-keeper role

Discovers portfolios via the `SealedPositionTouched` event (added this
session — before it, there was no way for an external keeper to learn a
portfolioKey existed at all, `docs/spec-contracts-tee.md` section 2.4
named this design but no contract event provided it), then polls
`/liquidation-check` and calls `/liquidate` when underwater. Verified
end to end against real chain events and a real running matcher.

```bash
MATCHER_URL=http://<matcher-host>:8787 \
SETTLEMENT_RPC_URL=<arc-testnet-rpc> \
KEEPER_LIQUIDATOR_ADDRESS=<address to receive keeperReward> \
KEEPER_MARKET_CONTRACTS="EURC/USDC=<FxPerpMarket address>,BTC/USDC=<PerpMarket address>" \
KEEPER_START_BLOCK=<deployment block, not 0, on a long-lived chain> \
KEEPER_POLL_INTERVAL_SECS=15 \
cargo run --release --bin keeper_liquidator
```

`KEEPER_MARKET_CONTRACTS` must list the SAME market_id strings the
matcher itself was configured with (`CERDIC_SETTLEMENT_CONTRACTS`) —
on-chain events only carry `keccak256(marketId)`, a one-way hash, so the
keeper needs this mapping independently, it can't be recovered from
chain data alone.

## 3. `market_maker` — real standing-quote bot via `/offer`

Quotes both sides of one market around the live Pyth mid-price, using
the spec's own "Market Maker Offers" surface (`/offer`, post-only, no
collateral pre-lock at placement). Verified live: real portfolio-key
self-lookup, real oracle-tracked quoting, real inventory inference from
public collateral deltas (a real bug in the inference math — a
percentage tolerance that was wider than the actual bid/ask spread,
caught live and fixed with real captured numbers, see the binary's own
test module).

```bash
MATCHER_URL=http://<matcher-host>:8787 \
SETTLEMENT_RPC_URL=<arc-testnet-rpc> \
MARKET_ID="BTC/USDC" \
MARKET_CONTRACT_ADDRESS=<PerpMarket address> \
PYTH_FEED_ID=e62df6c8b4a85fe1a67db44dc12de5db330f7ac66b72dc658afedf0f4a415b43 \
MM_PRIVATE_KEY=<the maker's own trading key> \
MM_SPREAD_BPS=20 \
MM_QUOTE_SIZE=10 \
MM_REQUOTE_INTERVAL_SECS=15 \
cargo run --release --bin market_maker
```

Run one instance per market (a different `MARKET_ID`/`MARKET_CONTRACT_ADDRESS`/
`PYTH_FEED_ID`/`MM_PRIVATE_KEY` each). Known, documented limitations
worth reading before sizing `MM_QUOTE_SIZE` for a real deployment:

- **No cancel endpoint exists anywhere in the matcher's API.** Each
  requote cycle stacks a NEW resting order rather than replacing the old
  one; short GTT expiry (`ttl.as_secs() * 2`) is what eventually clears
  stale ones, not this bot. Outstanding size genuinely grows across a
  run of stable-price cycles before settling into a rolling window.
- **Inventory tracking is a best-effort inference, not ground truth.**
  `sealedParams` is sealed under the ENCLAVE's key, not the trader's — a
  maker cannot decrypt its own resting fills' side/size from chain data,
  by design. This bot infers a fill from a public `loadSealed` collateral
  delta and attributes it to whichever outstanding quote's own
  `required_margin` matches within a couple of rounding units. If that
  attribution is ever ambiguous, it fails safe into "unknown" (quotes
  symmetrically, no inventory skew) rather than guessing.
- **A reverted on-chain settlement does not undo an off-chain fill.**
  See `api.rs`'s own doc at the `tokio::spawn` inside `post_order`: the
  match already happened and the trader's HTTP response already said
  "filled" before the settlement broadcast even starts, so a broadcast
  failure (most commonly `StalePrice`, see keeper #1's whole reason for
  existing) silently leaves the maker's liquidity consumed with no
  on-chain record and no retry. Hit directly this session while testing
  this bot's inventory inference. A real deployment needs a
  reconciliation keeper (diff "matches the book thinks happened" against
  "`SealedPositionTouched` events actually emitted," re-drive the gap) —
  not built this pass, a real follow-up.

## 4. `scripts/keeper-fx-rate.sh` — FX funding rate keeper

Enhanced this pass to match trade[XYZ]'s real funding formula shape
(`docs/trade-xyz-research.md` section 4): `F = 0.5 × (premium + clamp(rate_diff, ±50bps))`,
where `rate_diff` is the live EFFR-vs-ECB-DFR differential (daily cadence
is fine, real central-bank rates only move on policy schedule) and
`premium` is `FxPerpMarket`'s own live mark-vs-average-entry-price basis
(needs hourly-ish cadence, it tracks live market pricing not policy).

```bash
RPC_URL=<arc-testnet-rpc> \
MATCHER_URL=http://<matcher-host>:8787 \
FX_MARKET_ADDRESS=<FxPerpMarket address> \
FX_MARKET_ID="EURC/USDC" \
PRIVATE_KEY=<key holding RATE_KEEPER_ROLE> \
./scripts/keeper-fx-rate.sh
```

Market-hours aware (`docs/trade-xyz-research.md` section 9): the premium
term is forced to 0 during the closed FX week (Friday 21:00 UTC - Sunday
21:00 UTC, approximating 5pm ET) rather than trusting a thin weekend
order-book mid — the rate-differential term still pushes on the same
schedule regardless, since central-bank rates don't stop mattering over a
weekend.

## 5. Discovery bounds — `OracleHub.setDiscoveryBounds`

Not a standing keeper process, a one-time (or as-needed) admin call, but
listed here since it's what `keeper_price_pusher`'s `FX_MARKET_IDS` above
actually feeds. Bounds mark price to `referencePrice ± boundBps` for a
market whenever the live Pyth/Chainlink feed is unavailable, instead of
reverting — see `docs/trade-xyz-research.md` section 2 for the full
mechanism and its one real simplification versus trade[XYZ]'s own design
(no order-flow-driven EWMA, since the TEE matcher has no RPC client to read
chain state).

```bash
cast send <OracleHub address> \
  "setDiscoveryBounds(bytes32,bool,uint256,uint16,uint8)" \
  <marketId> true <initial reference price, 1e18-scaled> 500 2 \
  --rpc-url <arc-testnet-rpc> --private-key <admin key>
```

`boundBps=500` is ±5% (matching the existing 20x leverage every market
runs at today); `maxResets=2` caps how many times the reference can walk to
a bound edge before it stops moving until an admin resets it. Liquidation is
refused (`LiquidationEntry.checkAndFlag`/`executeStandardLiquidation`) for a
bounds-enabled market whenever its live feed is unavailable — `keeper_liquidator`
needs no changes for this, the gate lives entirely in the contracts.

## Operational notes common to all four

- All four are plain long-running processes (`tokio::time::interval`
  loops), no external scheduler dependency — a systemd unit or a
  supervisor process per binary is the expected wrapper for a real
  deployment, none is checked in here since the exact process manager is
  a deployment-environment choice, not something this repo should
  dictate.
- None of the four hold the matcher's own settlement-signing key — they
  either read public chain state, call the matcher's own HTTP API, or
  (price pusher, FX rate keeper) hold a SEPARATE, narrowly-scoped key
  whose only privilege is pushing a price/rate update, matching the
  existing design principle of keeping the settlement signer isolated
  from every other credential (`settle.rs`'s own doc on `SettlementSigner`
  vs `Keystore`).
