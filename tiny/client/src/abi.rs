use ethers::prelude::abigen;

abigen!(
    TinyShieldedVault,
    r#"[
        function DENOMINATION() external view returns (uint256)
        function deposit(bytes32 commitment) external
        function getPosition(bytes32 positionId) external view returns (bytes32 nullifier, uint256 collateral, uint8 status, bytes sealedParams)
        event PositionOpened(bytes32 indexed positionId)
        event PositionClosed(bytes32 indexed positionId)
    ]"#
);

abigen!(
    MockUsdc,
    r#"[
        function mint(address to, uint256 amount) external
        function approve(address spender, uint256 amount) external returns (bool)
        function balanceOf(address account) external view returns (uint256)
    ]"#
);
