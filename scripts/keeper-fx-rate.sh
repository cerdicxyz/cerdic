#!/usr/bin/env bash
# Real EUR/USD carry-rate keeper for FxPerpMarket.setRateDifferential.
#
# Pulls two genuine central-bank policy rates (not a fabricated/hardcoded
# spread) and pushes the differential on-chain:
#   - EFFR (Fed effective funds rate) from FRED's public CSV export
#   - ECB deposit facility rate from the ECB's public SDW API
# Convention matches FxPerpMarket.sol's own doc comment: rateDifferentialBps
# = r_quote - r_base, and EURC/USDC has base=EUR, quote=USD, so this is
# EFFR - ECB_DFR.
#
# Usage (same script for local Anvil and testnet, only the env vars change):
#   RPC_URL=http://127.0.0.1:8545 \
#   FX_MARKET_ADDRESS=0x... \
#   PRIVATE_KEY=0x... \
#   ./scripts/keeper-fx-rate.sh
#
# Intended to run on a schedule (cron/systemd timer) since real central-bank
# rates only change on policy-meeting cadence, not intraday  daily is
# more than sufficient.
set -euo pipefail

: "${RPC_URL:?set RPC_URL (e.g. http://127.0.0.1:8545 for local Anvil)}"
: "${FX_MARKET_ADDRESS:?set FX_MARKET_ADDRESS (deployed FxPerpMarket address)}"
: "${PRIVATE_KEY:?set PRIVATE_KEY (must hold RATE_KEEPER_ROLE on FxPerpMarket)}"

fed_rate=$(curl -sf "https://fred.stlouisfed.org/graph/fredgraph.csv?id=EFFR" | tail -1 | cut -d, -f2)
ecb_rate=$(curl -sf "https://data-api.ecb.europa.eu/service/data/FM/D.U2.EUR.4F.KR.DFR.LEV?format=csvdata&lastNObservations=1" \
  | tail -1 | cut -d, -f10)

if [[ -z "$fed_rate" || -z "$ecb_rate" ]]; then
  echo "keeper: failed to fetch a real rate from FRED or ECB, refusing to push a fabricated value" >&2
  exit 1
fi

# Both sources quote in percent (e.g. "3.63"); bps = percent * 100.
differential_bps=$(python3 -c "print(round(($fed_rate - $ecb_rate) * 100))")

echo "keeper: EFFR=${fed_rate}% ECB_DFR=${ecb_rate}% differential=${differential_bps}bps"

cast send "$FX_MARKET_ADDRESS" "setRateDifferential(int256)" "$differential_bps" \
  --rpc-url "$RPC_URL" \
  --private-key "$PRIVATE_KEY"
