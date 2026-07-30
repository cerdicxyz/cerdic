// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;

/// @title  RingBuffer
/// @notice Rolling 60-block window of per-block trade observations, feeding MarketImpactTwap.
/// @dev    A ring slot (blockNumber % 60) can go stale (only overwritten by a block exactly
///         N*60 newer), so readers always re-check `currentBlock - obs.blockNumber < WINDOW`
///         instead of trusting slot position. Each block contributes one VWAP observation;
///         averaging those across blocks (not across trades) is what makes this a TWAP.
library RingBuffer {
    uint256 internal constant WINDOW_BLOCKS = 60;
    uint256 internal constant SCALE = 1e18;

    /// @dev Packed into one slot: 48 + 104 + 104 = 256 bits. priceVolume = sum(price*size/1e18).
    struct Observation {
        uint48 blockNumber;
        uint104 priceVolume;
        uint104 size;
    }

    /// @dev latest.size == 0 iff the market never traded (zero observation is never persisted).
    struct Buffer {
        Observation latest;
        Observation[WINDOW_BLOCKS] ring;
    }

    /// @notice Accumulates same-block trades into `latest`; on a new block, persists
    ///         `latest` into its ring slot and starts a fresh accumulator.
    function record(RingBuffer.Buffer storage self, uint48 blockNumber, uint104 priceVolume, uint104 size) internal {
        Observation memory latest = self.latest;
        if (latest.blockNumber == blockNumber) {
            latest.priceVolume += priceVolume;
            latest.size += size;
            self.latest = latest;
            return;
        }
        if (latest.size != 0) {
            self.ring[latest.blockNumber % WINDOW_BLOCKS] = latest;
        }
        self.latest = Observation({blockNumber: blockNumber, priceVolume: priceVolume, size: size});
    }

    /// @notice Mean of per-block VWAPs over populated, in-window observations.
    ///         Returns (0, 0) when nothing is in-window, distinct from a real zero price.
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
