// Typed contract binding generated at compile time from this inline ABI —
// ethers-rs's abigen! macro, the Rust equivalent of what the old TypeScript
// version did by hand in abi.ts. Keep in sync with
// tiny/contracts/src/TinyShieldedVault.sol.
use ethers::prelude::abigen;

abigen!(
    TinyShieldedVault,
    r#"[
        function DENOMINATION() external view returns (uint256)
        function commitments(bytes32) external view returns (bool)
        function nullifiers(bytes32) external view returns (bool)
        function openPosition(bytes32 positionId, bytes32 commitment, bytes32 nullifier, bytes sealedParams) external
        function closePosition(bytes32 positionId, address payoutAddress, int256 settlementDelta) external
        function getPosition(bytes32 positionId) external view returns (bytes32 nullifier, uint256 collateral, uint8 status, bytes sealedParams)
        event Deposited(bytes32 indexed commitment)
        event PositionOpened(bytes32 indexed positionId)
        event PositionClosed(bytes32 indexed positionId)
    ]"#
);
