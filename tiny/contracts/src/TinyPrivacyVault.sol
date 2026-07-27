// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

/// @title IERC20Minimal
/// @notice Just enough of ERC20 to move collateral. Avoids pulling in a
///         dependency for this proof-of-concept.
interface IERC20Minimal {
    function transfer(address to, uint256 amount) external returns (bool);
    function transferFrom(address from, address to, uint256 amount) external returns (bool);
}

/// @title TinyPrivacyVault
/// @notice Minimal, end-to-end proof of the sealed-params / authorized-TEE-settler
///         privacy pattern described in ARCHITECTURE.md ("On-chain footprint of a
///         private position") and paper/synchra.tex Section 7.1 — scoped down from
///         PositionEngine + SettlementEngine to a single collateral asset, a single
///         position per trader, and no portfolio margin, market extension, or oracle
///         integration. The point of this contract is to prove the privacy mechanism
///         end-to-end against a real TEE process, not to be a usable market.
///
/// @dev    What stays sealed: side, size, entry price — collapsed into `sealedParams`,
///         an opaque bytes blob only the TEE can decrypt. What stays plain: collateral
///         amount and position status, because the kernel needs them to do solvency
///         accounting without trusting the TEE's arithmetic for anything but the
///         settlement delta itself.
///
///         What this contract deliberately does NOT do (all real gaps vs. the design
///         in ARCHITECTURE.md, not oversights): no attestation verification — the
///         authorized TEE signer is a single admin-set address, matching cer-perp's
///         "local dev mode" pattern, not GcpAttestationVerifier/NitroAttestationRegistry.
///         No portfolio margin, no liquidation, no ZK correctness proofs. Hardening
///         these is the Phase 1 roadmap item this contract is a stepping stone toward.
contract TinyPrivacyVault {
    enum PositionStatus {
        None,
        Open,
        Closed
    }

    struct Position {
        address trader;
        uint256 collateral;
        PositionStatus status;
        bytes sealedParams; // AES/NaCl-sealed blob — only the TEE holds the key
    }

    address public immutable collateralAsset;
    address public owner;
    address public authorizedTEE;

    mapping(address => uint256) public availableBalance; // deposited, not locked in a position
    mapping(bytes32 => Position) public positions;

    event Deposited(address indexed trader, uint256 amount);
    event Withdrawn(address indexed trader, uint256 amount);
    event AuthorizedTEEUpdated(address indexed previous, address indexed current);

    // Stripped events, matching the design's event-minimality rule: no side, size,
    // price, or PnL ever leaves the contract in an event — only the opaque position id
    // and the collateral figure the kernel already needs for solvency accounting.
    event PositionOpened(bytes32 indexed positionId, uint256 collateral);
    event PositionClosed(bytes32 indexed positionId);

    error NotOwner();
    error NotAuthorizedTEE();
    error InsufficientBalance();
    error PositionNotOpen();
    error PositionAlreadyExists();
    error SettlementUnderflow();

    modifier onlyOwner() {
        if (msg.sender != owner) revert NotOwner();
        _;
    }

    modifier onlyAuthorizedTEE() {
        if (msg.sender != authorizedTEE) revert NotAuthorizedTEE();
        _;
    }

    constructor(address _collateralAsset, address _authorizedTEE) {
        collateralAsset = _collateralAsset;
        owner = msg.sender;
        authorizedTEE = _authorizedTEE;
        emit AuthorizedTEEUpdated(address(0), _authorizedTEE);
    }

    /// @notice Admin bootstrap of the TEE's signing address. In the hardened design
    ///         (ARCHITECTURE.md, GcpAttestationVerifier / NitroAttestationRegistry) this
    ///         is replaced by attestation-gated registration, not an owner-set address.
    ///         Kept here as an explicit, named simplification rather than a silent one.
    function setAuthorizedTEE(address _authorizedTEE) external onlyOwner {
        emit AuthorizedTEEUpdated(authorizedTEE, _authorizedTEE);
        authorizedTEE = _authorizedTEE;
    }

    /// @notice Deposit collateral. Deposits themselves are plain — this contract seals
    ///         position parameters, not depositor identity or balance (see paper §7.1
    ///         for the distinction; shielded deposits are a separate, heavier mechanism
    ///         this proof-of-concept does not implement).
    function deposit(uint256 amount) external {
        bool ok = IERC20Minimal(collateralAsset).transferFrom(msg.sender, address(this), amount);
        require(ok, "transferFrom failed");
        availableBalance[msg.sender] += amount;
        emit Deposited(msg.sender, amount);
    }

    /// @notice Withdraw un-locked collateral.
    function withdraw(uint256 amount) external {
        if (availableBalance[msg.sender] < amount) revert InsufficientBalance();
        availableBalance[msg.sender] -= amount;
        bool ok = IERC20Minimal(collateralAsset).transfer(msg.sender, amount);
        require(ok, "transfer failed");
        emit Withdrawn(msg.sender, amount);
    }

    /// @notice Open a position on a trader's behalf. Only the attested (here:
    ///         admin-designated) TEE may call this — the contract never sees side,
    ///         size, or entry price in plaintext, only the sealed blob and the
    ///         collateral amount it needs for balance accounting.
    /// @param  positionId     Opaque id chosen by the TEE (e.g. hash of trader+nonce).
    /// @param  trader         The account the position belongs to.
    /// @param  collateral     Collateral to lock from `trader`'s available balance.
    /// @param  sealedParams   TEE-encrypted blob of {side, size, entryPrice, ...}.
    function openPosition(bytes32 positionId, address trader, uint256 collateral, bytes calldata sealedParams)
        external
        onlyAuthorizedTEE
    {
        if (positions[positionId].status != PositionStatus.None) revert PositionAlreadyExists();
        if (availableBalance[trader] < collateral) revert InsufficientBalance();

        availableBalance[trader] -= collateral;
        positions[positionId] = Position({
            trader: trader,
            collateral: collateral,
            status: PositionStatus.Open,
            sealedParams: sealedParams
        });

        emit PositionOpened(positionId, collateral);
    }

    /// @notice Close a position. `settlementDelta` is the TEE's attested PnL — computed
    ///         off-chain inside the enclave against the decrypted sealedParams and an
    ///         oracle price the TEE fetched. The contract does not recompute or verify
    ///         this number against plaintext; it trusts the attested caller and only
    ///         enforces its own invariant (a trader's balance cannot go negative).
    function closePosition(bytes32 positionId, int256 settlementDelta) external onlyAuthorizedTEE {
        Position storage position = positions[positionId];
        if (position.status != PositionStatus.Open) revert PositionNotOpen();

        int256 finalBalance = int256(position.collateral) + settlementDelta;
        if (finalBalance < 0) revert SettlementUnderflow();

        position.status = PositionStatus.Closed;
        availableBalance[position.trader] += uint256(finalBalance);

        emit PositionClosed(positionId);
    }

    /// @notice Read back a position, including its sealed blob — only the TEE can
    ///         meaningfully decrypt `sealedParams`; everyone else just sees ciphertext.
    function getPosition(bytes32 positionId)
        external
        view
        returns (address trader, uint256 collateral, PositionStatus status, bytes memory sealedParams)
    {
        Position storage p = positions[positionId];
        return (p.trader, p.collateral, p.status, p.sealedParams);
    }
}
