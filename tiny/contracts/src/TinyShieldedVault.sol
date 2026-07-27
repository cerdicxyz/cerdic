// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

interface IERC20Minimal {
    function transfer(address to, uint256 amount) external returns (bool);
    function transferFrom(address from, address to, uint256 amount) external returns (bool);
}

/// @title TinyShieldedVault
/// @notice v2 of the tiny privacy MVP. TinyPrivacyVault.sol proved the
///         sealed-params / authorized-TEE-settler pattern but still stored
///         `address => balance` and a plaintext `trader` field per position
///         — anyone reading the chain could see exactly who held how much
///         collateral and which address a given position belonged to. This
///         contract removes that: deposits are commitments, positions and
///         closes reference a nullifier, and a close can pay out to any
///         address, not necessarily the one that deposited. Nothing on-chain
///         ties a specific deposit to a specific position or withdrawal.
///
/// @dev    What this does NOT hide, and cannot: the deposit transaction
///         itself still shows an address and (implicitly, since every note
///         is the same fixed size) an amount — that's inherent to bridging a
///         transparent ERC20 into any shielded pool on a public chain. What
///         it hides is the LINK from that deposit to whatever happens next.
///         See tiny/arkworks-prover/src/note_circuit.rs for the commitment/
///         nullifier derivation (MiMC-5, fixed denomination — deliberately,
///         to remove the amount-correlation side channel a variable-amount
///         design would otherwise leak).
///
///         Position privacy (sealedParams) is unchanged from
///         TinyPrivacyVault — see that contract's header and paper §7.1.
///         This contract is specifically the deposit/identity half.
contract TinyShieldedVault {
    enum PositionStatus {
        None,
        Open,
        Closed
    }

    struct Position {
        bytes32 nullifier;
        uint256 collateral;
        PositionStatus status;
        bytes sealedParams;
    }

    /// Every note is worth exactly this much — fixed denomination, matching
    /// note_circuit.rs's DENOMINATION constant. See the contract-level
    /// comment for why this isn't a variable amount.
    uint256 public constant DENOMINATION = 500_000_000;

    address public immutable collateralAsset;
    address public owner;
    address public authorizedTEE;

    /// Deposited note commitments. No address is stored against a
    /// commitment — the chain records only "a note with this commitment
    /// exists," never who created it.
    mapping(bytes32 => bool) public commitments;
    /// Spent notes. A nullifier being marked here proves *some* valid note
    /// was spent, without revealing which commitment it came from — the
    /// unlinkability is enforced by the NoteCircuit proof (off-chain today,
    /// TEE-checked; on-chain ZK verification is the same next step already
    /// named for the correctness proofs in ARCHITECTURE.md).
    mapping(bytes32 => bool) public nullifiers;

    mapping(bytes32 => Position) public positions;

    event AuthorizedTEEUpdated(address indexed previous, address indexed current);
    /// Deliberately no amount, no depositor — see contract header.
    event Deposited(bytes32 indexed commitment);
    event PositionOpened(bytes32 indexed positionId);
    event PositionClosed(bytes32 indexed positionId);

    error NotOwner();
    error NotAuthorizedTEE();
    error CommitmentAlreadyUsed();
    error UnknownCommitment();
    error NullifierAlreadySpent();
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

    function setAuthorizedTEE(address _authorizedTEE) external onlyOwner {
        emit AuthorizedTEEUpdated(authorizedTEE, _authorizedTEE);
        authorizedTEE = _authorizedTEE;
    }

    /// @notice Deposit a fixed-denomination note. `commitment` is computed
    ///         client-side (NoteCircuit::derive in the Rust prover, or the
    ///         equivalent off-chain) from a secret only the depositor knows
    ///         — the contract never sees that secret, only the commitment.
    function deposit(bytes32 commitment) external {
        if (commitments[commitment]) revert CommitmentAlreadyUsed();
        commitments[commitment] = true;
        bool ok = IERC20Minimal(collateralAsset).transferFrom(msg.sender, address(this), DENOMINATION);
        require(ok, "transferFrom failed");
        emit Deposited(commitment);
    }

    /// @notice Open a position against a deposited note. Only the TEE may
    ///         call this — it verified (today: checked directly; the
    ///         on-chain-verified version is the named next step) the
    ///         NoteCircuit proof that `nullifier` is the correct nullifier
    ///         for a note matching `commitment`, before ever submitting
    ///         this call.
    function openPosition(bytes32 positionId, bytes32 commitment, bytes32 nullifier, bytes calldata sealedParams)
        external
        onlyAuthorizedTEE
    {
        if (positions[positionId].status != PositionStatus.None) revert PositionAlreadyExists();
        if (!commitments[commitment]) revert UnknownCommitment();
        if (nullifiers[nullifier]) revert NullifierAlreadySpent();

        nullifiers[nullifier] = true;
        positions[positionId] = Position({
            nullifier: nullifier,
            collateral: DENOMINATION,
            status: PositionStatus.Open,
            sealedParams: sealedParams
        });

        emit PositionOpened(positionId);
    }

    /// @notice Close a position and pay out to `payoutAddress` — chosen at
    ///         close time, not required to be (and for real unlinkability,
    ///         should not be) the address that originally deposited the
    ///         note. This is the second half of the unlinkability property:
    ///         even watching every deposit and every close, you cannot pair
    ///         them up.
    function closePosition(bytes32 positionId, address payoutAddress, int256 settlementDelta)
        external
        onlyAuthorizedTEE
    {
        Position storage position = positions[positionId];
        if (position.status != PositionStatus.Open) revert PositionNotOpen();

        int256 finalAmount = int256(position.collateral) + settlementDelta;
        if (finalAmount < 0) revert SettlementUnderflow();

        position.status = PositionStatus.Closed;

        bool ok = IERC20Minimal(collateralAsset).transfer(payoutAddress, uint256(finalAmount));
        require(ok, "transfer failed");

        emit PositionClosed(positionId);
    }

    function getPosition(bytes32 positionId)
        external
        view
        returns (bytes32 nullifier, uint256 collateral, PositionStatus status, bytes memory sealedParams)
    {
        Position storage p = positions[positionId];
        return (p.nullifier, p.collateral, p.status, p.sealedParams);
    }
}
