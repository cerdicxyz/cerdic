// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {ECDSA} from "openzeppelin-contracts/contracts/utils/cryptography/ECDSA.sol";
import {MessageHashUtils} from "openzeppelin-contracts/contracts/utils/cryptography/MessageHashUtils.sol";

import {Account as ClearingAccount} from "./Account.sol";

/// @title  CapabilityRegistry
/// @notice Agent-account capability tokens (paper/cerdic-propdesk.tex, Definition: Prop
///         account): a firm signs a capability once, at account creation, pinning
///         execution limits to a trader address. The kernel enforces those limits without
///         the firm ever needing to see the trader's live positions, replacing
///         surveillance with enforcement.
/// @dev    capability = Sign_sk_firm(H(trader, limits, expiry)). Non-retroactive by
///         construction: `grantCapability` reverts while an active capability already
///         exists for the trader, and there is no update function, only `revokeCapability`.
///         The firm's only on-chain action against an open account is ending it; it can
///         never tighten or loosen the pinned terms mid-flight.
///
///         MVP scope (propdesk paper sec:relation): one account, one capability, no
///         sub-account hierarchy.
contract CapabilityRegistry {
    using ECDSA for bytes32;
    using MessageHashUtils for bytes32;

    /// @notice Kernel-enforced execution-policy fields (propdesk paper, table:composition).
    struct Limits {
        /// @notice Per-market absolute position size cap, 1e18-scaled base units.
        uint256 maxPositionSize;
        /// @notice Max leverage, basis points (500 = 5x).
        uint256 maxLeverageBps;
        /// @notice Max realized loss in a rolling day, 1e18-scaled USD.
        uint256 dailyLossLimitUsd;
        /// @notice Max drawdown off the account's high-water mark, basis points.
        uint256 maxDrawdownBps;
        /// @notice Minimum seconds between position opens.
        uint64 cooldownSeconds;
    }

    struct Capability {
        Limits limits;
        uint64 expiry;
        bool revoked;
    }

    /// @notice One active capability per trader.
    mapping(address => Capability) public capabilities;

    /// @notice The firm's signing key. Immutable per registry: a key rotation deploys a
    ///         new registry rather than mutating trust in place for open accounts.
    address public immutable firmSigner;

    address public immutable admin;

    /// @notice The clearing account this registry can freeze on a limit breach.
    ClearingAccount public immutable account;

    event CapabilityGranted(
        address indexed trader,
        uint256 maxPositionSize,
        uint256 maxLeverageBps,
        uint256 dailyLossLimitUsd,
        uint256 maxDrawdownBps,
        uint64 cooldownSeconds,
        uint64 expiry
    );
    event CapabilityRevoked(address indexed trader);
    event LimitBreached(address indexed trader, uint256 realizedLossTodayUsd, uint256 drawdownBps);

    error ZeroAddress();
    error InvalidSignature();
    error CapabilityAlreadyActive(address trader);
    error CapabilityNotActive(address trader);
    error ExpiryInPast();
    error NotAdmin();

    modifier onlyAdmin() {
        if (msg.sender != admin) revert NotAdmin();
        _;
    }

    constructor(address adminAccount, address firmSigner_, address account_) {
        if (adminAccount == address(0) || firmSigner_ == address(0) || account_ == address(0)) {
            revert ZeroAddress();
        }
        admin = adminAccount;
        firmSigner = firmSigner_;
        account = ClearingAccount(account_);
    }

    /// @notice The digest the firm signs off-chain: `H(trader, limits, expiry)`.
    function capabilityHash(address trader, Limits calldata limits, uint64 expiry) public pure returns (bytes32) {
        return keccak256(
            abi.encode(
                trader,
                limits.maxPositionSize,
                limits.maxLeverageBps,
                limits.dailyLossLimitUsd,
                limits.maxDrawdownBps,
                limits.cooldownSeconds,
                expiry
            )
        );
    }

    /// @notice Registers a capability signed once by the firm's key. Reverts if the trader
    ///         already holds an active one: a firm that wants different terms must revoke
    ///         first, it cannot silently overwrite an open account's pinned limits.
    function grantCapability(address trader, Limits calldata limits, uint64 expiry, bytes calldata signature) external {
        if (trader == address(0)) revert ZeroAddress();
        if (expiry <= block.timestamp) revert ExpiryInPast();
        if (isActive(trader)) revert CapabilityAlreadyActive(trader);

        bytes32 digest = capabilityHash(trader, limits, expiry).toEthSignedMessageHash();
        if (digest.recover(signature) != firmSigner) revert InvalidSignature();

        capabilities[trader] = Capability({limits: limits, expiry: expiry, revoked: false});
        emit CapabilityGranted(
            trader,
            limits.maxPositionSize,
            limits.maxLeverageBps,
            limits.dailyLossLimitUsd,
            limits.maxDrawdownBps,
            limits.cooldownSeconds,
            expiry
        );
    }

    /// @notice Closes the account's capability. The only on-chain action the firm has
    ///         against an open account: it can end the relationship, it cannot edit it.
    function revokeCapability(address trader) external onlyAdmin {
        if (!isActive(trader)) revert CapabilityNotActive(trader);
        capabilities[trader].revoked = true;
        emit CapabilityRevoked(trader);
    }

    /// @notice True while a capability exists, is unexpired, and is unrevoked.
    function isActive(address trader) public view returns (bool) {
        Capability storage cap = capabilities[trader];
        return cap.expiry > block.timestamp && !cap.revoked;
    }

    function limitsOf(address trader) external view returns (Limits memory) {
        return capabilities[trader].limits;
    }

    /// @notice Position-size gate an execution hook checks before opening or extending a
    ///         position. Fails closed: an inactive capability rejects every size, not just
    ///         sizes over a stale limit.
    function checkPositionSize(address trader, uint256 absoluteSize) external view returns (bool allowed) {
        return isActive(trader) && absoluteSize <= capabilities[trader].limits.maxPositionSize;
    }

    /// @notice Leverage gate, same fail-closed shape as `checkPositionSize`.
    function checkLeverage(address trader, uint256 leverageBps) external view returns (bool allowed) {
        return isActive(trader) && leverageBps <= capabilities[trader].limits.maxLeverageBps;
    }

    /// @notice Daily-loss / drawdown breach check, evaluated the same way a liquidation
    ///         threshold is (propdesk paper sec:lifecycle): a comparison against the
    ///         pinned limits, no discretion, no dispute process. An inactive capability
    ///         reports breached, since there is no valid account to check against.
    function checkBreach(address trader, uint256 realizedLossTodayUsd, uint256 drawdownBps)
        public
        view
        returns (bool breached)
    {
        if (!isActive(trader)) return true;
        Limits storage limits = capabilities[trader].limits;
        return realizedLossTodayUsd > limits.dailyLossLimitUsd || drawdownBps > limits.maxDrawdownBps;
    }

    /// @notice Evaluates the breach check and, if breached, revokes the capability and
    ///         freezes the clearing account in the same transaction. Caller must have
    ///         `CLEARING_ADMIN_ROLE` on `account` for the freeze to succeed, the same
    ///         admin wiring `LiquidationEntry` uses for margin breaches.
    /// @dev    `security-audit-tee-contracts.md` finding C1, fixed: previously callable by
    ///         anyone with caller-supplied loss/drawdown figures (`type(uint256).max` always
    ///         breached), and froze accounts with no capability at all (`checkBreach`
    ///         reports breached for those by design, see that function's own doc — correct
    ///         for a general breach query, wrong to act on here). `onlyAdmin` is the interim
    ///         fix until a real kernel-sourced risk feed replaces the caller-supplied
    ///         figures; the `isActive` guard means a capability-less address can never be
    ///         frozen through this path, full stop.
    function checkAndFreezeOnBreach(address trader, uint256 realizedLossTodayUsd, uint256 drawdownBps)
        external
        onlyAdmin
        returns (bool breached)
    {
        if (!isActive(trader)) {
            return false;
        }
        breached = checkBreach(trader, realizedLossTodayUsd, drawdownBps);
        if (!breached) {
            return false;
        }
        capabilities[trader].revoked = true;
        emit CapabilityRevoked(trader);
        account.freezeAccount(trader);
        emit LimitBreached(trader, realizedLossTodayUsd, drawdownBps);
    }
}
