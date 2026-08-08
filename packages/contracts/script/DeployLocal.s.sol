// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {Script, console} from "forge-std/Script.sol";
import {ERC20} from "openzeppelin-contracts/contracts/token/ERC20/ERC20.sol";
import {ERC20Permit} from "openzeppelin-contracts/contracts/token/ERC20/extensions/ERC20Permit.sol";
import {MockPyth} from "pyth-sdk-solidity/MockPyth.sol";

import {Account as AccountContract} from "../src/clearing/Account.sol";
import {CollateralEngine} from "../src/clearing/CollateralEngine.sol";
import {RiskMonitor} from "../src/clearing/RiskMonitor.sol";
import {AttestationRouter} from "../src/clearing/AttestationRouter.sol";
import {PositionEngine} from "../src/clearing/PositionEngine.sol";
import {TeeAttestationVerifier} from "../src/clearing/TeeAttestationVerifier.sol";
import {OracleHub} from "../src/oracle/OracleHub.sol";
import {PythConsumer} from "../src/oracle/PythConsumer.sol";
import {ChainlinkConsumer} from "../src/oracle/ChainlinkConsumer.sol";
import {FxPerpMarket} from "../src/markets/FxPerpMarket.sol";
import {ProtocolConstants} from "../src/lib/ProtocolConstants.sol";

/// @dev Minimal mintable 1e18-scaled ERC-20, same shape as
///      CollateralEngine.t.sol's MockStablecoin — a real local stand-in for
///      USDC/EURC, not a fabricated balance (every unit is actually minted
///      on this chain by this script, no numbers are just asserted).
/// @dev Also `ERC20Permit` — `Account.depositWithPermit`'s own doc on why:
///      collapses the old approve-then-deposit two-transaction flow into
///      one off-chain signature plus one on-chain call. Real, testnet
///      `TestUSDC.sol` has this too (separate contract, separate deploy
///      path via `DeployTestUSDC.s.sol`, not used by local_dev) — kept
///      as two contracts rather than merging them since this one is
///      also EURC's type (no faucet, no cooldown, admin mints freely),
///      a genuinely different shape than the self-serve testnet faucet.
contract LocalStablecoin is ERC20, ERC20Permit {
    constructor(string memory name_, string memory symbol_) ERC20(name_, symbol_) ERC20Permit(name_) {}

    function mint(address to, uint256 amount) external {
        _mint(to, amount);
    }
}

/// @title  DeployLocal
/// @notice Same deploy sequence as Deploy.s.sol, but stands up local mock
///         USDC/EURC tokens and a MockPyth feed instead of reading real Arc
///         addresses from env, so it can run end to end against a bare
///         local Anvil chain with no external dependencies. Seeds the
///         EUR/USD feed with a real rate (1.151121, fetched live from
///         open.er-api.com at the time this script was written), not a
///         fabricated placeholder like 1.10 or 1.00.
/// @dev    Required env var: PRIVATE_KEY (deployer key, also becomes every
///         contract's admin, see Deploy.s.sol's comment on why).
contract DeployLocal is Script {
    uint256 internal constant EUR_USD_PRICE = 1_151_121; // 6 decimals: 1.151121
    int32 internal constant EUR_USD_EXPO = -6;

    // Split across two structs + two internal functions purely to dodge
    // "stack too deep" — run() alone had too many locals for the legacy
    // codegen path (no --via-ir here, keep the build fast/plain).
    struct Tokens {
        LocalStablecoin usdc;
        LocalStablecoin eurc;
        MockPyth pyth;
        bytes32 feedId;
    }

    struct Core {
        AccountContract account;
        CollateralEngine collateralEngine;
        RiskMonitor riskMonitor;
        OracleHub oracleHub;
        PythConsumer pythConsumer;
        ChainlinkConsumer chainlinkConsumer;
        AttestationRouter attestationRouter;
        TeeAttestationVerifier attestationVerifier;
        FxPerpMarket fxMarket;
    }

    function run() external {
        uint256 deployerKey = vm.envUint("PRIVATE_KEY");
        address admin = vm.addr(deployerKey);

        vm.startBroadcast(deployerKey);
        Tokens memory t = _deployTokens(admin);
        Core memory c = _deployCore(admin, t);
        vm.stopBroadcast();

        _logAddresses(t, c);
    }

    function _deployTokens(address admin) internal returns (Tokens memory t) {
        t.usdc = new LocalStablecoin("USD Coin", "USDC");
        t.eurc = new LocalStablecoin("Euro Coin", "EURC");
        t.usdc.mint(admin, 10_000_000e18);
        t.eurc.mint(admin, 10_000_000e18);

        t.pyth = new MockPyth(60, 1);
        t.feedId = keccak256("EURC/USDC");
        bytes memory updateData = t.pyth
            .createPriceFeedUpdateData(
                t.feedId,
                int64(uint64(EUR_USD_PRICE)),
                500,
                EUR_USD_EXPO,
                int64(uint64(EUR_USD_PRICE)),
                500,
                uint64(block.timestamp)
            );
        bytes[] memory updates = new bytes[](1);
        updates[0] = updateData;
        t.pyth.updatePriceFeeds{value: t.pyth.getUpdateFee(updates)}(updates);
    }

    function _deployCore(address admin, Tokens memory t) internal returns (Core memory c) {
        c.account = new AccountContract(admin);
        c.collateralEngine = new CollateralEngine(admin, address(t.usdc));
        c.riskMonitor = new RiskMonitor(admin, address(0));

        c.oracleHub = new OracleHub(admin);
        c.pythConsumer = new PythConsumer(admin);
        c.chainlinkConsumer = new ChainlinkConsumer(admin);
        c.pythConsumer.setPythContract(address(t.pyth));
        c.oracleHub.setPythConsumer(address(c.pythConsumer));
        c.oracleHub.setChainlinkConsumer(address(c.chainlinkConsumer));

        c.attestationRouter = new AttestationRouter(admin);
        c.attestationVerifier = new TeeAttestationVerifier(admin);

        c.fxMarket = new FxPerpMarket(admin, address(c.oracleHub), t.feedId, 50);

        c.fxMarket.registerDecoder(t.feedId, address(c.fxMarket));
        c.fxMarket.setAttestationRouter(address(c.attestationRouter));
        c.riskMonitor.setMarkPriceOracle(address(c.oracleHub));
        c.riskMonitor.setCollateralEngine(address(c.collateralEngine));
        c.riskMonitor.setAttestationRouter(address(c.attestationRouter));
        c.riskMonitor.registerMarket(t.feedId);
        c.account.setRiskMonitor(address(c.riskMonitor));
        // Lets the TEE settle realized close/liquidation PnL into real
        // custody balances via Account.settleRealizedPnl — see that
        // function's own doc for why this is the bridge SettlementEngine's
        // sealed collateral never had.
        c.account.setAttestationRouter(address(c.attestationRouter));

        // Previously missing entirely: without this, CollateralEngine.effectiveCollateral
        // reverts BalanceSourceNotSet on every call, which means RiskMonitor.isWithdrawSafe
        // (and therefore Account.withdraw() itself, since account.riskMonitor is set above)
        // reverted unconditionally on every withdraw attempt, on every deployment from this
        // script including local dev — a real, deploy-blocking bug independent of the
        // security-audit-tee-contracts.md findings.
        c.collateralEngine.setBalanceSource(address(c.account));
        // RiskMonitor.positionEngine backs the LEGACY plaintext fallback
        // (currentMarginRequirement, only consulted when no fresh TEE portfolio-margin
        // attestation exists, see RiskMonitor.sol's own doc) — the live sealed-TEE trading
        // path never writes to a PositionEngine at all, so a dedicated, permanently-empty
        // instance is the correct target: every lookup legitimately returns "no position",
        // same as an unset field would mean by intent, but without reverting
        // PositionEngineNotSet the way leaving this wire missing did.
        PositionEngine emptyPositionEngine = new PositionEngine(admin);
        c.riskMonitor.setPositionEngine(address(emptyPositionEngine));

        ProtocolConstants constants = new ProtocolConstants();
        c.collateralEngine.registerAsset(address(t.usdc), 1, uint16(constants.t1HaircutBps()));
        c.collateralEngine.registerAsset(address(t.eurc), 1, uint16(constants.t1HaircutBps()));
    }

    function _logAddresses(Tokens memory t, Core memory c) internal pure {
        console.log("USDC (mock):", address(t.usdc));
        console.log("EURC (mock):", address(t.eurc));
        console.log("MockPyth:", address(t.pyth));
        console.log("Account:", address(c.account));
        console.log("CollateralEngine:", address(c.collateralEngine));
        console.log("RiskMonitor:", address(c.riskMonitor));
        console.log("OracleHub:", address(c.oracleHub));
        console.log("PythConsumer:", address(c.pythConsumer));
        console.log("ChainlinkConsumer:", address(c.chainlinkConsumer));
        console.log("AttestationRouter:", address(c.attestationRouter));
        console.log("TeeAttestationVerifier:", address(c.attestationVerifier));
        console.log("FxPerpMarket (EURC/USDC):", address(c.fxMarket));
        console.logBytes32(t.feedId);
    }
}
