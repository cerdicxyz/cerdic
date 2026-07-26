// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;

/// @title  RingBuffer
/// @notice Rolling 60-block window of per-block trade observations, used
///         by `MarketImpactTwap` (plan todo #21) to compute the on-chain
///         impact TWAP cited as the tertiary mark-price input
///         (paper/cerdic.tex:570 "on-chain impact mid-price",
///         paper/cerdic.tex:1059 "TWAP from CLOB trades").
/// @dev    Storage layout per market (61 slots):
///           - `latest` — the accumulator for the block currently being
///             written. Trades landing in the same block aggregate here;
///             the observation is only moved into the ring when the first
///             trade of a NEWER block arrives.
///           - `ring` — 60 slots addressed by `blockNumber % 60`, each
///             holding the most recently persisted observation for that
///             block congruence class. A slot is overwritten only when a
///             block exactly 60 (or 120, ...) newer persists, so a stale
///             slot can survive longer than the window; readers therefore
///             ALWAYS re-check the observation's block number against the
///             current block (`currentBlock - obs.blockNumber <
///             WINDOW_BLOCKS`) instead of trusting the slot position.
///
///         Gas posture: `record` costs exactly 1 SLOAD + 1 SSTORE on the
///         same-block accumulate path and 1 SLOAD + 2 SSTOREs on the
///         new-block advance path (the persist write plus the fresh
///         accumulator), keeping `recordTrade` inside its 30k budget.
///         `meanBlockVwap` is a view scan over all 61 slots — acceptable
///         because the reader (`OracleHub.markPrice`) is itself a view
///         function with a 200k budget.
///
///         Time weighting: each populated block in the window contributes
///         ONE observation — that block's volume-weighted average price —
///         and the window mean weights every such block equally. Volume
///         weighting is applied WITHIN a block, time weighting ACROSS
///         blocks, which is what makes the aggregate a TWAP rather than a
///         VWAP.
library RingBuffer {
    /// @notice Rolling-window length in blocks.
    uint256 internal constant WINDOW_BLOCKS = 60;

    /// @notice 1e18 scaling shared with the price/size encodings.
    uint256 internal constant SCALE = 1e18;

    /// @notice One block's aggregated trade print, packed into a single
    ///         storage slot (48 + 104 + 104 = 256 bits).
    /// @param  blockNumber Block the observation aggregates.
    /// @param  priceVolume Sum of `price * size / 1e18` over the block's
    ///         trades (1e18-scaled USD notional). Bounded by
    ///         `type(uint104).max` ~ 2.07e31 — billions of USD per block.
    /// @param  size        Sum of trade sizes over the block (1e18-scaled
    ///         base units).
    struct Observation {
        uint48 blockNumber;
        uint104 priceVolume;
        uint104 size;
    }

    /// @notice Per-market rolling window state.
    /// @param  latest Accumulator for the block currently being written.
    ///         `latest.size == 0` iff the market has never recorded a
    ///         trade (the zero observation is never persisted into the
    ///         ring, so an empty ring stays all-zero).
    /// @param  ring   Persisted per-block observations, addressed by
    ///         `blockNumber % WINDOW_BLOCKS`.
    struct Buffer {
        Observation latest;
        Observation[WINDOW_BLOCKS] ring;
    }

    /// @notice Aggregates one trade into the buffer: accumulates into
    ///         `latest` when the trade lands in the same block, otherwise
    ///         persists `latest` into its ring slot (evicting the
    ///         60-blocks-older observation, if any) and starts a fresh
    ///         accumulator for the new block.
    /// @dev    Trades must arrive in non-decreasing block order — the EVM
    ///         guarantees this for on-chain callers. Callers must cast to
    ///         the packed widths (checked, so pathological values revert).
    function record(RingBuffer.Buffer storage self, uint48 blockNumber, uint104 priceVolume, uint104 size) internal {
        Observation memory latest = self.latest;
        if (latest.blockNumber == blockNumber) {
            latest.priceVolume += priceVolume;
            latest.size += size;
            self.latest = latest;
            return;
        }
        // Advance: persist the finished block's observation (skip the
        // never-used zero accumulator so the ring stays all-zero until the
        // second block with trades) and open the new block's accumulator.
        if (latest.size != 0) {
            self.ring[latest.blockNumber % WINDOW_BLOCKS] = latest;
        }
        self.latest = Observation({blockNumber: blockNumber, priceVolume: priceVolume, size: size});
    }

    /// @notice Arithmetic mean of the per-block volume-weighted average
    ///         prices over the populated observations inside the trailing
    ///         `WINDOW_BLOCKS`-block window (inclusive of the current
    ///         block). Returns `(0, 0)` when no observation is in-window —
    ///         either the market never traded or every observation is
    ///         stale — so the caller can distinguish "no data" from a
    ///         legitimate zero price.
    /// @dev    Staleness is re-checked per slot (`currentBlock -
    ///         obs.blockNumber >= WINDOW_BLOCKS` excludes) because a
    ///         congruence slot is only overwritten when a block exactly
    ///         N*60 newer persists; gaps in trading leave stale entries
    ///         behind.
    function meanBlockVwap(RingBuffer.Buffer storage self, uint256 currentBlock)
        internal
        view
        returns (uint256 mean, uint256 count)
    {
        uint256 sum;
        Observation memory latest = self.latest;
        if (latest.size != 0 && currentBlock - uint256(latest.blockNumber) < WINDOW_BLOCKS) {
            sum = uint256(latest.priceVolume) * SCALE / uint256(latest.size);
            count = 1;
        }
        for (uint256 i; i < WINDOW_BLOCKS; ++i) {
            Observation memory obs = self.ring[i];
            if (obs.size == 0) continue;
            if (currentBlock - uint256(obs.blockNumber) >= WINDOW_BLOCKS) continue;
            sum += uint256(obs.priceVolume) * SCALE / uint256(obs.size);
            ++count;
        }
        if (count == 0) return (0, 0);
        mean = sum / count;
    }
}
