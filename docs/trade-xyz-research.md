# trade[XYZ] research: what's worth carrying over

Date: 2026-08-05
Source: docs.trade.xyz, trade.xyz (public documentation, fetched live)

## What XYZ actually is

XYZ is a HIP-3 market deployment on Hyperliquid, not a standalone clearinghouse: it
"defines market listings, oracle sources, leverage limits, and other parameters," and
Hyperliquid's own validators do the matching, margining, and liquidation. This matters
for how much of their design is transferable: their **keeper/liquidator/market-maker
infrastructure is Hyperliquid's, not something XYZ built or documents** — there is
nothing to carry over on that front, it doesn't exist as a separate thing on their side.
What *is* transferable is their **market design decisions**: how they price FX/TradFi
assets 24/7, their funding formula, their margin tiers, and their liquidation sequencing.
Synchra owns its full stack (TEE matcher, on-chain settlement, its own keepers), so these
translate into real implementation choices here, not just configuration.

## 1. FX/TradFi-hours 24/7 pricing (directly relevant — Synchra trades FX)

XYZ lists EUR/USD and USD/JPY. FX markets are only open Sunday 5pm ET through Friday 5pm
ET. Their solution:

- **External coverage**: a relayer sources executable institutional quotes while the
  market is open.
- **Internal fallback**: from Friday 5pm to Sunday 5pm ET, *or any time there's a gap
  of more than 30 seconds between external oracle datapoints*, they fall back to an
  internal price computed as a continuous-time EWMA that adjusts the previous price by a
  fraction of the order-book impact-price difference (time constant τ=30min, capped at
  9.5% weight per update, time-delta capped at 3min to avoid a stale gap causing a huge
  jump when data resumes).

**Carry-over**: `crates/cerdic-tee-matcher/src/oracle.rs`'s `fetch_price` currently has no
fallback at all — if Hermes 404s (which it did constantly in this session's local
testing), the caller just gets `Err` and falls back to the position's own sealed entry
price (see `compute_margin`'s `unwrap_or(m.entry_price)`). That's a much cruder fallback
than XYZ's EWMA — it means a stale position looks perpetually "fairly priced" against
itself and can never be flagged as underwater by a genuine price move (this was an actual
blocker hit while building the real liquidation test this session — see
`docs/gcp-attestation-test-report.md`'s sibling session notes). Worth a real follow-up:
give the backstop's own TWAP (`backstop.rs`, already EWMA-shaped) double duty as this
fallback when the primary feed goes stale, rather than falling all the way back to the
sealed entry price.

**Not carried over (yet)**: XYZ's explicit weekend/holiday calendar
(`consolidated-resources/holiday-closures.md`,`roll-schedules.md`) — Synchra's
`keeper-fx-rate.sh` and price keepers currently run on a fixed interval with no
market-hours awareness. A real FX venue eventually needs this; noted as a gap, not
fixed in this pass (see "Known gaps" below).

## 2. Discovery bounds (not carried over — real gap for weekend-open FX)

When a TradFi asset trades 24/7 on-chain but its home market is closed, XYZ bounds mark
price to ±(1/max_leverage) of a reference price (e.g. ±5% at 20x), with a re-anchoring
mechanism (reference price shifts to the bound edge once oracle price gets within ~90%
of it, consuming one of a capped number of "resets" per direction) — and critically,
**a position cannot be liquidated while its liquidation price sits outside the active
bound**. This is what makes 24/7 trading of a Sunday-Friday asset safe: a liquidity gap
over the weekend can't manufacture a liquidation nobody could have defended against.

Synchra's `FxPerpMarket` has no such protection today — `RiskMonitor`/`risk::compute_margin`
apply the same maintenance check around the clock, and per the finding above, the fallback
when live pricing is unavailable is "pretend nothing moved," not a bounded-but-live price.
This is a real, identified gap, not addressed in this pass — flagging clearly since Synchra
is an FX venue by design and will hit this exact problem on a live testnet the first weekend
it's running with real positions open.

## 3. Mark price (already structurally similar — validates existing design)

XYZ: mark price = median(oracle price, oracle price + 150s-EWMA(mid − oracle), median(best
bid, best ask, last trade)), with relayer updates clamped to ±50bps per tick.

Synchra's `OracleHub.markPrice` = median(Pyth, Chainlink, on-chain impact TWAP) — same
three-way-median shape, different third leg (impact TWAP vs. book median). No change
needed; this is a good sign the existing design already follows a proven pattern. The one
delta worth adopting: XYZ's ±50bps per-update clamp on the *relayer's own* price feed,
which Synchra's price-pusher keeper (built this pass) now also applies for the same
reason — a single bad tick from an upstream feed shouldn't be able to move the on-chain
price by more than a bounded step.

## 4. Funding rate formula (carried over into the FX keeper)

XYZ: `F = 0.5 × (avg_premium_index + clamp(interest_rate_diff, -5bps, +5bps))`, applied
hourly. The 0.5 scaling and the interest-rate clamp were an explicit later change to
dampen "aggressive funding during weekend price discovery" (`changelog/funding-rate-formula-update.md`).

Synchra's `FxPerpMarket` funding was pure interest-rate differential with no premium/basis
term at all (a keeper pushes `rateDifferentialBps` directly, no on-chain computation).
`scripts/keeper-fx-rate.sh` (this pass) now computes a blended rate matching XYZ's shape:
`0.5 × (premium_bps + clamp(interest_rate_diff_bps, -50, 50))`, where the premium term is
the FxPerpMarket's own live mark-vs-average-entry-price basis (queried from the deployed
contract), and the interest-rate differential term is the same live EFFR-vs-ECB-DFR figure
already being fetched (see the earlier session's real-rate keeper work). Real central-bank
rates only move on policy-meeting cadence so the differential term is still safe to poll
daily; the premium term needs to be pushed far more often (hourly, matching XYZ's cadence)
since it tracks live market pricing, not policy — the keeper now supports both cadences.

## 5. Margin tiers (not carried over — noted for later)

XYZ: maintenance margin = 0.5 × initial-margin-at-max-leverage, and max leverage varies
per asset (3-40x) rather than being one fixed number. Synchra hardcodes `IMR_BPS=500`
(5%, i.e. 20x) and `MMR_BPS=300` (3%) globally in `SettlementEngine`/`RiskMonitor`,
identical across every market including `FxPerpMarket` (which a prior session
deliberately kept at 20x rather than raising to the ~50x FX's lower realized volatility
would justify, specifically because `requiredMargin()` isn't per-market configurable
today — see that contract's own doc comment). XYZ's 0.5×-of-max-leverage relationship is
actually a clean, principled way to make maintenance margin scale sensibly if
per-market leverage config is ever added; worth keeping as the target formula for that
future work, not implemented now (real contract-level change, out of scope for a keeper
pass).

## 6. Liquidation sequencing (not carried over — noted for later)

XYZ liquidates in two stages: first, real market orders sized to fully close the
position; only if account equity craters to 2/3 of maintenance margin *without* that
succeeding does a backstop liquidation transfer the whole cross-margin position to the
liquidator. Synchra's `liquidateSealed` (as built and tested this session) is single-stage
— it closes every leg the portfolio holds in one shot, no partial-liquidation attempt
first. XYZ's docs are explicit that liquidation is meant to close only enough to restore
health, not the whole position (`docs/spec-contracts-tee.md` section 2.4 says the same:
"one `liquidate` call closes enough size to restore health, not the whole position, unless
LLTV-equivalent math says nothing smaller would work") — so this is actually a case of
Synchra's own spec already agreeing with XYZ's design, and the *implementation* not yet
matching either. Flagged as a real, pre-existing gap between spec and code, not something
new from this research; not fixed in this pass (a genuine partial-liquidation sizing
algorithm is a bigger, separate change from keeper infrastructure).

## 7. Fees (not carried over — Synchra has no fee engine yet)

XYZ runs seven volume tiers over a rolling 14-day window, continuous per-trade maker
rebates, and a "Growth Mode" (≥90% fee reduction) for non-crypto assets specifically —
worth noting for later since FX is exactly the asset class they discount hardest, a real
signal about price sensitivity in that segment. Not built here: Synchra has no fee engine
at all yet, adding one is a separate, larger piece of work than keeper infrastructure.

## Known gaps this research surfaced but this pass does not fix

- No market-hours/weekend awareness in any keeper (price pusher or FX rate keeper) —
  they run on a fixed interval regardless of whether FX markets are notionally "open."
- No discovery-bounds equivalent — a real, identified safety gap for weekend-open FX
  positions, see section 2.
- No per-market leverage/margin-tier configuration (needs a `SettlementEngine`/
  `RiskMonitor` change, not a keeper).
- No partial-liquidation sizing (needs a margin-engine change, not a keeper).
- No fee engine.

These are listed here so they're not silently lost — none of them are invented "nice to
haves," each ties back to a concrete mechanism XYZ actually ships and Synchra's own spec
already gestures at.

Sources: docs.trade.xyz/overview/xyz-architecture, /perp-mechanics/{overview,oracle-price,
mark-price,discovery-bounds,fees}, /asset-directory/fx, /risk-and-margining/{margin-modes,
liquidation-mechanics}, /changelog/funding-rate-formula-update, trade.xyz landing page.
