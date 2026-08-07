#!/usr/bin/env bash
# Real EUR/USD funding keeper for FxPerpMarket.setRateDifferential.
#
# Blends two real, independently-sourced terms, matching trade[XYZ]'s own
# funding formula shape (docs/trade-xyz-research.md section 4):
#   F = 0.5 * (premium_bps + clamp(rate_diff_bps, -50, +50))
#
# - rate_diff_bps: EFFR (Fed effective funds rate, FRED) minus the ECB
#   deposit facility rate (ECB SDW), the same interest-rate differential
#   this script always pushed. Real central-bank rates only move on
#   policy-meeting cadence, daily polling is more than sufficient.
# - premium_bps: this market's own order-book mid (from the matcher's
#   real /orderbook endpoint) versus the live Pyth EUR/USD oracle price,
#   the same "does the book trade above/below spot" concept
#   PerpMarket.sol's own funding model already uses for BTC/USDC, applied
#   here off-chain since FxPerpMarket's funding is keeper-pushed rather
#   than on-chain-computed. NOT "average entry price" (an earlier version
#   of this script's own doc claimed that, which turned out to be
#   unimplementable: entry prices are sealed/encrypted, there is no
#   public aggregate to read). Needs a tighter cadence than the rate
#   term, hourly not daily, since it tracks live market pricing.
#
# Usage (same script for local Anvil and testnet, only the env vars change):
#   RPC_URL=http://127.0.0.1:8545 \
#   MATCHER_URL=http://127.0.0.1:8787 \
#   FX_MARKET_ADDRESS=0x... \
#   FX_MARKET_ID="EURC/USDC" \
#   PRIVATE_KEY=0x... \
#   ./scripts/keeper-fx-rate.sh
set -euo pipefail

: "${RPC_URL:?set RPC_URL (e.g. http://127.0.0.1:8545 for local Anvil)}"
: "${MATCHER_URL:?set MATCHER_URL (e.g. http://127.0.0.1:8787)}"
: "${FX_MARKET_ADDRESS:?set FX_MARKET_ADDRESS (deployed FxPerpMarket address)}"
: "${FX_MARKET_ID:?set FX_MARKET_ID (the matcher own market_id string, e.g. EURC/USDC)}"
: "${PRIVATE_KEY:?set PRIVATE_KEY (must hold RATE_KEEPER_ROLE on FxPerpMarket)}"

fed_rate=$(curl -sf "https://fred.stlouisfed.org/graph/fredgraph.csv?id=EFFR" | tail -1 | cut -d, -f2)
ecb_rate=$(curl -sf "https://data-api.ecb.europa.eu/service/data/FM/D.U2.EUR.4F.KR.DFR.LEV?format=csvdata&lastNObservations=1" \
  | tail -1 | cut -d, -f10)

if [[ -z "$fed_rate" || -z "$ecb_rate" ]]; then
  echo "keeper: failed to fetch a real rate from FRED or ECB, refusing to push a fabricated value" >&2
  exit 1
fi

# Both sources quote in percent (e.g. "3.63"); bps = percent * 100.
rate_diff_bps=$(python3 -c "print(round(($fed_rate - $ecb_rate) * 100))")
# Clamp to +-50bps, matching XYZ's own formula update
# (docs/trade-xyz-research.md section 4).
rate_diff_bps=$(python3 -c "print(max(-50, min(50, $rate_diff_bps)))")

encoded_market_id=$(python3 -c "import urllib.parse,sys; print(urllib.parse.quote(sys.argv[1], safe=''))" "$FX_MARKET_ID")
orderbook=$(curl -sf "${MATCHER_URL}/orderbook/${encoded_market_id}")
oracle_price=$(curl -sf "https://hermes.pyth.network/v2/updates/price/latest?ids[]=a995d00bb36a63cef7fd2c287dc105fc8f3d93779f062f09551b0af3e81ec30b" \
  | python3 -c "
import json, sys
d = json.load(sys.stdin)['parsed'][0]['price']
print(int(d['price']) * (10 ** int(d['expo'])))
")

# Market-hours gate (docs/trade-xyz-research.md sections 1 and 9): FX trades
# on-chain 24/7 but the order book has no real external liquidity feeding it
# Friday 21:00 UTC - Sunday 21:00 UTC (approximating 5pm ET, see
# crates/cerdic-tee-matcher/src/market_hours.rs's own DST caveat). A thin
# weekend book's mid can gap arbitrarily far from the oracle price without
# reflecting any real premium/discount, so the premium term is forced to 0
# during that window instead of trusting it — the same "no fabricated
# signal" principle the no-two-sided-book branch below already used.
weekday=$(date -u +%u)   # 1=Mon .. 7=Sun
hour=$(date -u +%H)
fx_closed=0
if [[ "$weekday" == "6" ]]; then
  fx_closed=1
elif [[ "$weekday" == "5" && "$hour" -ge 21 ]]; then
  fx_closed=1
elif [[ "$weekday" == "7" && "$hour" -lt 21 ]]; then
  fx_closed=1
fi

if [[ "$fx_closed" == "1" ]]; then
  premium_bps=0
  echo "keeper: FX market closed (weekend), forcing premium_bps=0 rather than trusting a thin weekend book" >&2
else
  premium_bps=$(python3 -c "
import json
book = json.loads('''$orderbook''')
oracle = $oracle_price
bid = book.get('best_bid')
ask = book.get('best_ask')
if bid is None or ask is None or oracle <= 0:
    print(0)  # no two-sided book yet, or bad oracle read: no premium signal, not a fabricated one
else:
    mid = (bid + ask) / 2
    print(round((mid - oracle) / oracle * 10000))
")
fi

blended_bps=$(python3 -c "print(round(0.5 * ($premium_bps + $rate_diff_bps)))")

echo "keeper: EFFR=${fed_rate}% ECB_DFR=${ecb_rate}% rate_diff=${rate_diff_bps}bps premium=${premium_bps}bps blended=${blended_bps}bps"

cast send "$FX_MARKET_ADDRESS" "setRateDifferential(int256)" "$blended_bps" \
  --rpc-url "$RPC_URL" \
  --private-key "$PRIVATE_KEY"
