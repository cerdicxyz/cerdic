// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {AccessControl} from "openzeppelin-contracts/contracts/access/AccessControl.sol";
import {IERC20} from "openzeppelin-contracts/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "openzeppelin-contracts/contracts/token/ERC20/utils/SafeERC20.sol";
import {IERC20Permit} from "openzeppelin-contracts/contracts/token/ERC20/extensions/IERC20Permit.sol";

import {ICollateralBalanceSource} from "./ICollateralEngine.sol";
import {IRiskMonitor} from "./RiskMonitor.sol";
import {AttestationRouter} from "./AttestationRouter.sol";

/// @title  Account
/// @notice Owns collateral custody, positions, and credit records per trader.
/// @dev    Position/credit-record writes belong to PositionEngine/SettlementEngine, not here.
///         deposit/withdraw follow checks-effects-interactions, no ReentrancyGuard needed.
contract Account is AccessControl, ICollateralBalanceSource {
    using SafeERC20 for IERC20;

    /// @notice Gates freezeAccount; granted to LiquidationEntry.
    bytes32 public constant CLEARING_ADMIN_ROLE = keccak256("CLEARING_ADMIN_ROLE");

    /// @dev `frozen` is the only scalar field: a public mapping to a struct of only mappings
    ///      can't generate a getter, so the auto getter returns `frozen`.
    struct AccountData {
        mapping(address => uint256) collateralBalances;
        mapping(bytes32 => bytes) positions;
        mapping(bytes32 => int256) creditRecords;
        bool frozen;
    }

    mapping(address => AccountData) public accounts;

    /// @notice Zero address = unwired, withdrawals skip the margin check.
    IRiskMonitor public riskMonitor;

    /// @notice Zero until wired; while zero, `settleRealizedPnl` is unreachable
    ///         (every call reverts `AttestationRouterNotSet`), same
    ///         fail-closed posture `SettlementEngine.settleMatch` already
    ///         uses for the same dependency.
    AttestationRouter public attestationRouter;

    event CollateralDeposited(address indexed trader, address indexed asset, uint256 amount);
    event CollateralWithdrawn(address indexed trader, address indexed asset, uint256 amount);
    event AccountFrozen(address indexed trader);
    event AccountUnfrozen(address indexed trader);
    event CollateralSeized(address indexed trader, address indexed asset, uint256 amount, address indexed recipient);
    event RiskMonitorUpdated(address indexed monitor);
    event AttestationRouterUpdated(address indexed router);
    /// @notice Deliberately carries only a count, no addresses or amounts:
    ///         those are in calldata already (unavoidable, the contract
    ///         needs them to apply real balance changes verifiably), but
    ///         an event with indexed per-trader topics would hand an
    ///         observer a free, pre-filtered surveillance feed on top of
    ///         that — see `settleRealizedPnlBatch`'s own doc on why this
    ///         function is batched at all.
    event RealizedPnlBatchSettled(uint256 count);

    error InsufficientCollateral(address trader, address asset, uint256 requested, uint256 available);
    error InsufficientMarginForWithdraw(address trader, address asset, uint256 amount);
    error AccountIsFrozen(address trader);
    error AccountNotFrozen(address trader);
    error ZeroAmount();
    error ZeroAddress();
    error AttestationRouterNotSet();
    error NotAuthorizedTEE(address caller);
    error BatchLengthMismatch(uint256 tradersLen, uint256 assetsLen, uint256 pnlDeltasLen);

    constructor(address admin) {
        if (admin == address(0)) revert ZeroAddress();
        _grantRole(DEFAULT_ADMIN_ROLE, admin);
        _grantRole(CLEARING_ADMIN_ROLE, admin);
    }

    /// @notice Pulls `amount` of `asset` via transferFrom; caller must have approved this contract.
    function deposit(address asset, uint256 amount) external {
        _deposit(asset, amount);
    }

    /// @notice Same deposit, minus the separate approve transaction: signs
    ///         one EIP-2612 permit off-chain (free, no gas, no separate tx)
    ///         and this call spends it and deposits in the same
    ///         transaction. Real, confirmed UX cost this replaces: the
    ///         plain `deposit()` path needs an `approve` transaction AND a
    ///         `deposit` transaction — two separate wallet prompts, two
    ///         separate waits for confirmation, just to move collateral in
    ///         once. `asset` must support EIP-2612 (`TestUSDC.sol` does);
    ///         a real deployment's real collateral asset would need the
    ///         same, or fall back to plain `deposit()`.
    /// @dev    `permit` wrapped in try/catch: the signature is public the
    ///         moment this transaction is broadcast (visible in the
    ///         mempool), so anyone could front-run and submit it first —
    ///         harmless on its own (it sets the exact same allowance this
    ///         call already wants), but without the try/catch that
    ///         front-run would make THIS call's own `permit` revert
    ///         (already-consumed nonce) and fail the whole deposit even
    ///         though the allowance it needed already exists.
    ///         `transferFrom` right after is the real backstop either way
    ///         — it reverts on its own if the allowance genuinely isn't
    ///         there, permit succeeded or not.
    function depositWithPermit(address asset, uint256 amount, uint256 deadline, uint8 v, bytes32 r, bytes32 s)
        external
    {
        try IERC20Permit(asset).permit(msg.sender, address(this), amount, deadline, v, r, s) {} catch {}
        _deposit(asset, amount);
    }

    function _deposit(address asset, uint256 amount) internal {
        if (asset == address(0)) revert ZeroAddress();
        if (amount == 0) revert ZeroAmount();
        if (accounts[msg.sender].frozen) revert AccountIsFrozen(msg.sender);

        accounts[msg.sender].collateralBalances[asset] += amount;

        IERC20(asset).safeTransferFrom(msg.sender, address(this), amount);

        emit CollateralDeposited(msg.sender, asset, amount);
    }

    function withdraw(address asset, uint256 amount) external {
        if (amount == 0) revert ZeroAmount();
        if (accounts[msg.sender].frozen) revert AccountIsFrozen(msg.sender);

        AccountData storage account = accounts[msg.sender];
        uint256 balance = account.collateralBalances[asset];
        if (balance < amount) {
            revert InsufficientCollateral(msg.sender, asset, amount, balance);
        }

        IRiskMonitor monitor = riskMonitor;
        if (address(monitor) != address(0) && !monitor.isWithdrawSafe(msg.sender, asset, amount)) {
            revert InsufficientMarginForWithdraw(msg.sender, asset, amount);
        }

        account.collateralBalances[asset] = balance - amount;

        IERC20(asset).safeTransfer(msg.sender, amount);

        emit CollateralWithdrawn(msg.sender, asset, amount);
    }

    /// @notice Liquidation penalty path: moves collateral between two in-kernel balances,
    ///         never leaves custody. Only callable on a frozen account.
    function seizeCollateral(address trader, address asset, uint256 amount, address recipient)
        external
        onlyRole(CLEARING_ADMIN_ROLE)
    {
        if (asset == address(0) || recipient == address(0)) revert ZeroAddress();
        if (amount == 0) revert ZeroAmount();
        if (!accounts[trader].frozen) revert AccountNotFrozen(trader);

        AccountData storage account = accounts[trader];
        uint256 balance = account.collateralBalances[asset];
        if (balance < amount) {
            revert InsufficientCollateral(trader, asset, amount, balance);
        }

        account.collateralBalances[asset] = balance - amount;
        accounts[recipient].collateralBalances[asset] += amount;

        emit CollateralSeized(trader, asset, amount, recipient);
    }

    function setRiskMonitor(address monitor) external onlyRole(CLEARING_ADMIN_ROLE) {
        riskMonitor = IRiskMonitor(monitor);
        emit RiskMonitorUpdated(monitor);
    }

    function setAttestationRouter(address router) external onlyRole(CLEARING_ADMIN_ROLE) {
        attestationRouter = AttestationRouter(router);
        emit AttestationRouterUpdated(router);
    }

    /// @notice MVP interim bridge, not the final privacy model. The real
    ///         fix — real trader address never appearing on-chain at all,
    ///         balances as commitments, settlement via a ZK proof of
    ///         correctness instead of plaintext deltas (Renegade's own
    ///         published design is the reference point) — is a separate,
    ///         much larger build (new circuits, client-side note
    ///         management, nullifiers, a withdrawal proof flow), tracked
    ///         as deferred-not-cut ZK work, not attempted here. This
    ///         function is what makes realized PnL move real money AT
    ///         ALL in the meantime; `settle.rs`'s batched, jittered flush
    ///         is a real but partial mitigation on top of it, not a
    ///         substitute for the eventual redesign.
    /// @notice Moves realized close/liquidation PnL between real custody
    ///         balances — the bridge `SettlementEngine.sol`'s own
    ///         `SealedPosition.collateral` doc says doesn't exist:
    ///         everything settled there is virtual accounting, this is
    ///         what actually makes a losing close cost the trader real
    ///         money and a winning close pay real money, out of the same
    ///         shared custody pool every deposit already sits in.
    ///
    ///         Callable only by an attested TEE (same trust boundary
    ///         `SettlementEngine.settleMatch` uses), never by a trader
    ///         directly — each `pnlDeltas[i]` is the TEE's own sealed PnL
    ///         computation (`realized_close_delta` in the matcher),
    ///         applied here without re-derivation, exactly like
    ///         `collateralDelta` in `settleMatch`. Takes real trader
    ///         addresses, not `portfolioKey`s: unlike `SettlementEngine`
    ///         (deliberately portfolioKey-only, for unlinkability), this
    ///         contract's balances are already keyed by real address —
    ///         only the TEE itself ever holds the portfolioKey-to-address
    ///         mapping (it derives one from the other), so it's the only
    ///         party that can correctly attribute a realized delta here.
    ///
    ///         BATCHED deliberately, not one call per fill: a single-item
    ///         version was the first draft, then reverted before ever
    ///         being wired up. `collateralBalanceOf` is already a public,
    ///         permissionless view — anyone can already see a trader's
    ///         real balance change block to block, batching was never
    ///         going to hide THAT. What it hides is WHICH fill caused it:
    ///         one settlement call per close ties a real address, a real
    ///         dollar amount, and a timestamp directly to whichever trade
    ///         printed on the public tape in that same instant — trivial
    ///         deanonymization of a supposedly sealed match. Netting
    ///         several traders' (or several fills') deltas into one
    ///         on-chain call (`settle.rs`'s `realized_pnl_flush_loop`,
    ///         jittered, not a fixed cadence) means an observer can no
    ///         longer cleanly attribute one settlement to one visible
    ///         trade. Not perfect unlinkability (this is still a
    ///         transparent EVM contract, not a shielded pool — full
    ///         privacy here is the deferred ZK work's job), but a real,
    ///         meaningful reduction versus one call per close.
    ///
    ///         Floors each trader at zero rather than reverting on a loss
    ///         larger than their real balance (mirrors the sealed
    ///         ledger's own floor, `realized_close_delta`'s doc) — a real
    ///         deployment's liquidation keeper is what's supposed to
    ///         prevent this case from ever being reached, not this
    ///         function; flooring instead of reverting keeps one
    ///         late/missed liquidation from bricking every OTHER trader's
    ///         settlement batched into the same call.
    function settleRealizedPnlBatch(address[] calldata traders, address[] calldata assets, int256[] calldata pnlDeltas)
        external
    {
        AttestationRouter router = attestationRouter;
        if (address(router) == address(0)) revert AttestationRouterNotSet();
        if (!router.isAuthorizedTEE(msg.sender)) revert NotAuthorizedTEE(msg.sender);
        if (traders.length != assets.length || traders.length != pnlDeltas.length) {
            revert BatchLengthMismatch(traders.length, assets.length, pnlDeltas.length);
        }

        for (uint256 i; i < traders.length; ++i) {
            address trader = traders[i];
            address asset = assets[i];
            if (trader == address(0) || asset == address(0)) revert ZeroAddress();

            AccountData storage account = accounts[trader];
            uint256 balance = account.collateralBalances[asset];
            int256 pnlDelta = pnlDeltas[i];
            uint256 newBalance;
            if (pnlDelta >= 0) {
                newBalance = balance + uint256(pnlDelta);
            } else {
                uint256 loss = uint256(-pnlDelta);
                newBalance = loss >= balance ? 0 : balance - loss;
            }
            account.collateralBalances[asset] = newBalance;
        }

        emit RealizedPnlBatchSettled(traders.length);
    }

    /// @dev Idempotent: re-freezing is a no-op, no event.
    function freezeAccount(address trader) external onlyRole(CLEARING_ADMIN_ROLE) {
        if (trader == address(0)) revert ZeroAddress();
        if (accounts[trader].frozen) return;

        accounts[trader].frozen = true;

        emit AccountFrozen(trader);
    }

    /// @notice `security-audit-tee-contracts.md` finding C1: `freezeAccount` previously had
    ///         no inverse at all, so a freeze (permissionless-triggerable pre-fix, via
    ///         `CapabilityRegistry.checkAndFreezeOnBreach`) was permanent by construction.
    ///         Admin-gated like `freezeAccount` itself, not a trader self-service unfreeze.
    function unfreezeAccount(address trader) external onlyRole(CLEARING_ADMIN_ROLE) {
        if (trader == address(0)) revert ZeroAddress();
        if (!accounts[trader].frozen) revert AccountNotFrozen(trader);

        accounts[trader].frozen = false;

        emit AccountUnfrozen(trader);
    }

    function getPosition(bytes32 marketId) external view returns (bytes memory) {
        return accounts[msg.sender].positions[marketId];
    }

    function getCollateralBalance(address asset) external view returns (uint256) {
        return accounts[msg.sender].collateralBalances[asset];
    }

    function collateralBalanceOf(address trader, address asset) external view returns (uint256) {
        return accounts[trader].collateralBalances[asset];
    }
}
