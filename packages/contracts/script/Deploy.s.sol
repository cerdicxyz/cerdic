// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {Script, console} from "forge-std/Script.sol";

import {Account as AccountContract} from "../src/clearing/Account.sol";
import {CollateralEngine} from "../src/clearing/CollateralEngine.sol";
import {RiskMonitor} from "../src/clearing/RiskMonitor.sol";
import {AttestationRouter} from "../src/clearing/AttestationRouter.sol";
import {TeeAttestationVerifier} from "../src/clearing/TeeAttestationVerifier.sol";
import {PositionEngine} from "../src/clearing/PositionEngine.sol";
import {OracleHub} from "../src/oracle/OracleHub.sol";
import {PythConsumer} from "../src/oracle/PythConsumer.sol";
import {ChainlinkConsumer} from "../src/oracle/ChainlinkConsumer.sol";
import {FxPerpMarket} from "../src/markets/FxPerpMarket.sol";
import {ProtocolConstants} from "../src/lib/ProtocolConstants.sol";

/// @title  Deploy
/// @notice MVP deploy sequence: the kernel (Account/CollateralEngine/RiskMonitor),
///         oracle plumbing (OracleHub + Pyth/Chainlink consumers), attestation
///         (AttestationRouter, admin-allowlist path — still the production path;
///         TeeAttestationVerifier deploys alongside it but isn't wired as the
///         active gate yet, see that contract's own module doc for why), and the
///         one MVP market (FxPerpMarket, EURC/USDC), per ARCHITECTURE.md's Build
///         Milestones section.
/// @dev    Dry-run locally first (`forge script script/Deploy.s.sol:Deploy`, no
///         --broadcast) before ever adding --broadcast against a real network —
///         that's how the deployer/admin key mismatch below was actually caught.
///         Required env vars: PRIVATE_KEY (deployer key, also becomes every
///         contract's admin — see the comment in run()), ARC_USDC_ADDRESS,
///         ARC_EURC_ADDRESS, ARC_PYTH_CONTRACT, EURC_USDC_PYTH_FEED_ID. These are
///         real, network-specific values this script does not fabricate — unset,
///         the run reverts with a clear error instead of silently deploying
///         against zero addresses.
contract Deploy is Script {
    function run() external {
        // The deploying key doubles as `admin` on every contract below — every
        // post-deploy wiring call (setPythContract, registerAsset, ...) is
        // admin-gated, so broadcasting as anyone else would revert NotAdmin() on
        // the very first wiring call. Confirmed live: an earlier version of this
        // script took a separate DEPLOY_ADMIN address and broadcast as Foundry's
        // default sender instead, which reverted exactly that way in a local
        // dry run (`forge script` without --broadcast) before this was fixed.
        uint256 deployerKey = vm.envUint("PRIVATE_KEY");
        address admin = vm.addr(deployerKey);
        address usdcToken = vm.envAddress("ARC_USDC_ADDRESS");
        address eurcToken = vm.envAddress("ARC_EURC_ADDRESS");
        address pythContract = vm.envAddress("ARC_PYTH_CONTRACT");
        bytes32 eurcUsdcFeedId = vm.envBytes32("EURC_USDC_PYTH_FEED_ID");

        vm.startBroadcast(deployerKey);

        // --- Kernel ---
        AccountContract account = new AccountContract(admin);
        CollateralEngine collateralEngine = new CollateralEngine(admin, usdcToken);
        RiskMonitor riskMonitor = new RiskMonitor(admin, address(0)); // markPriceOracle wired below

        // --- Oracle plumbing ---
        OracleHub oracleHub = new OracleHub(admin);
        PythConsumer pythConsumer = new PythConsumer(admin);
        ChainlinkConsumer chainlinkConsumer = new ChainlinkConsumer(admin);
        pythConsumer.setPythContract(pythContract);
        oracleHub.setPythConsumer(address(pythConsumer));
        oracleHub.setChainlinkConsumer(address(chainlinkConsumer));

        // --- Attestation ---
        AttestationRouter attestationRouter = new AttestationRouter(admin);
        // Deployed but not wired as the active gate — see this contract's own
        // module doc. AttestationRouter's admin-allowlist stays the production
        // path until a real enclave token has been tested against this.
        TeeAttestationVerifier attestationVerifier = new TeeAttestationVerifier(admin);

        // --- The one MVP market: FxPerpMarket (EURC/USDC) ---
        // 50x: FX majors' lower realized vol vs crypto/commodities, see
        // SettlementEngine.LEVERAGE_CEILING's own doc.
        FxPerpMarket fxMarket = new FxPerpMarket(admin, address(oracleHub), eurcUsdcFeedId, 50);

        // --- Wiring ---
        fxMarket.registerDecoder(eurcUsdcFeedId, address(fxMarket));
        fxMarket.setAttestationRouter(address(attestationRouter));
        riskMonitor.setMarkPriceOracle(address(oracleHub));
        riskMonitor.setCollateralEngine(address(collateralEngine));
        riskMonitor.setAttestationRouter(address(attestationRouter));
        riskMonitor.registerMarket(eurcUsdcFeedId);
        account.setRiskMonitor(address(riskMonitor));

        // Previously missing entirely: without this, CollateralEngine.effectiveCollateral
        // reverts BalanceSourceNotSet on every call, which means RiskMonitor.isWithdrawSafe
        // (and therefore Account.withdraw() itself, since account.riskMonitor is set above)
        // reverted unconditionally on every withdraw attempt on this deployment — a real,
        // deploy-blocking bug independent of the security-audit-tee-contracts.md findings.
        collateralEngine.setBalanceSource(address(account));
        // RiskMonitor.positionEngine backs the LEGACY plaintext fallback
        // (currentMarginRequirement, only consulted when no fresh TEE portfolio-margin
        // attestation exists) — the live sealed-TEE trading path never writes to a
        // PositionEngine at all, so a dedicated, permanently-empty instance is the correct
        // target: every lookup legitimately returns "no position" without reverting
        // PositionEngineNotSet the way leaving this wire missing did.
        riskMonitor.setPositionEngine(address(new PositionEngine(admin)));

        ProtocolConstants constants = new ProtocolConstants();
        collateralEngine.registerAsset(usdcToken, 1, uint16(constants.t1HaircutBps()));
        collateralEngine.registerAsset(eurcToken, 1, uint16(constants.t1HaircutBps()));

        vm.stopBroadcast();

        console.log("Account:", address(account));
        console.log("CollateralEngine:", address(collateralEngine));
        console.log("RiskMonitor:", address(riskMonitor));
        console.log("OracleHub:", address(oracleHub));
        console.log("PythConsumer:", address(pythConsumer));
        console.log("ChainlinkConsumer:", address(chainlinkConsumer));
        console.log("AttestationRouter:", address(attestationRouter));
        console.log("TeeAttestationVerifier (deployed, not yet wired as active gate):", address(attestationVerifier));
        console.log("FxPerpMarket (EURC/USDC):", address(fxMarket));
    }
}
