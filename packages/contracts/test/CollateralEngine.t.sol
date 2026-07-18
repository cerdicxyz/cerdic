// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {Test} from "forge-std/Test.sol";
import {ERC20} from "openzeppelin-contracts/contracts/token/ERC20/ERC20.sol";

import {Account as ClearingAccount} from "../src/clearing/Account.sol";
import {CollateralEngine} from "../src/clearing/CollateralEngine.sol";
import {ICollateralEngine, IOracleConsumer} from "../src/clearing/ICollateralEngine.sol";

/// @dev Minimal mintable 1e18-scaled ERC-20 used as a stablecoin stand-in
///      for the MVP collateral basket (USDC/USYC are both 1e18-scaled).
contract MockStablecoin is ERC20 {
    constructor(string memory name_, string memory symbol_) ERC20(name_, symbol_) {}

    function mint(address to, uint256 amount) external {
        _mint(to, amount);
    }
}

/// @dev Stand-in for `PythConsumer.sol` (todo #12): an `IOracleConsumer`
///      with an externally settable 1e18-scaled USD price.
contract MockOracle is IOracleConsumer {
    uint256 public price;

    constructor(uint256 initialPrice) {
        price = initialPrice;
    }

    function setPrice(uint256 newPrice) external {
        price = newPrice;
    }

    function priceOf(address) external view returns (uint256) {
        return price;
    }
}

/// @title  CollateralEngineTest
/// @notice Unit tests for the clearing kernel's `CollateralEngine.sol`
///         (paper/synchra.tex:362-386, plan todo #9). Covers the static
///         MVP tier basket (T1 USDC at 0 bps, T2 USYC at 200 bps), the
///         tier-haircut monotonicity invariant (fuzzed), the
///         `C_eff = Σ b_a · (1 − h_a) · p_a` valuation against
///         `Account.sol` balances, the stub/wired oracle paths, the
///         per-tier haircut-range guard, the admin gates, and the 120k
///         gas budget for a 4-asset `effectiveCollateral` call.
contract CollateralEngineTest is Test {
    CollateralEngine internal engine;
    ClearingAccount internal account;
    MockStablecoin internal usdc;
    MockStablecoin internal usyc;

    address internal admin = makeAddr("admin");
    address internal stranger = makeAddr("stranger");
    address internal trader = makeAddr("trader");

    /// @dev USYC on Arc Testnet — pre-registered by the engine constructor
    ///      (mirrors `ProtocolConstants.USYC_ARC_TESTNET`).
    address internal constant USYC_ARC = 0xe9185F0c5F296Ed1797AaE4238D26CCaBEadb86C;

    uint256 internal constant MINT_AMOUNT = 1_000_000e18;
    uint256 internal constant DEPOSIT_AMOUNT = 1_000e18;

    function setUp() public {
        account = new ClearingAccount(admin);
        usdc = new MockStablecoin("USD Coin", "USDC");
        usyc = new MockStablecoin("Hashnote US Yield Coin", "USYC");

        engine = new CollateralEngine(admin, address(usdc));

        // The constructor registers USDC (T1) and the Arc USYC address (T2);
        // the local USYC mock is registered at the same MVP tier-2 haircut
        // so test deposits of the mock are valued.
        vm.prank(admin);
        engine.registerAsset(address(usyc), 2, 200);

        vm.prank(admin);
        engine.setBalanceSource(address(account));

        usdc.mint(trader, MINT_AMOUNT);
        usyc.mint(trader, MINT_AMOUNT);
        vm.startPrank(trader);
        usdc.approve(address(account), type(uint256).max);
        usyc.approve(address(account), type(uint256).max);
        vm.stopPrank();
    }

    /// @dev Paper Table 1 haircut ranges (paper/synchra.tex:367-380),
    ///      hardcoded so the test encodes the paper spec independently of
    ///      the constants the engine consumes.
    function _rangeOf(uint8 tier) internal pure returns (uint256 minBps, uint256 maxBps) {
        if (tier == 1) return (0, 0);
        if (tier == 2) return (200, 500);
        if (tier == 3) return (1000, 2000);
        return (1500, 3500);
    }

    // ---------------------------------------------------------------------
    // Constructor: static MVP basket.
    // ---------------------------------------------------------------------

    /// @notice The constructor pre-registers exactly the MVP basket: USDC
    ///         as Tier 1 at 0% and USYC (Arc Testnet) as Tier 2 at 200 bps
    ///         (plan todo #9, paper Table 1).
    function test_ConstructorRegistersStaticTierBasket() public view {
        assertEq(engine.tierOf(address(usdc)), 1);
        assertEq(engine.haircutBpsOf(address(usdc)), 0);

        assertEq(engine.tierOf(USYC_ARC), 2);
        assertEq(engine.haircutBpsOf(USYC_ARC), 200);
    }

    /// @notice Both static registrations emit `AssetRegistered` during
    ///         construction.
    function test_ConstructorEmitsAssetRegistered() public {
        vm.expectEmit(true, false, false, true);
        emit ICollateralEngine.AssetRegistered(address(usdc), 1, 0);
        vm.expectEmit(true, false, false, true);
        emit ICollateralEngine.AssetRegistered(USYC_ARC, 2, 200);

        new CollateralEngine(admin, address(usdc));
    }

    /// @notice Zero-address constructor args are rejected.
    function test_ConstructorZeroAdminReverts() public {
        vm.expectRevert(ICollateralEngine.ZeroAddress.selector);
        new CollateralEngine(address(0), address(usdc));
    }

    /// @notice Zero-address USDC is rejected.
    function test_ConstructorZeroUsdcReverts() public {
        vm.expectRevert(ICollateralEngine.ZeroAddress.selector);
        new CollateralEngine(admin, address(0));
    }

    // ---------------------------------------------------------------------
    // Tier monotonicity invariant.
    // ---------------------------------------------------------------------

    /// @notice Tier haircut floors strictly increase T1 < T2 < T3 < T4
    ///         (0 < 200 < 1000 < 1500). Floors, not full ranges: the paper's
    ///         T3/T4 ranges deliberately overlap (see notepad learning #2),
    ///         so the invariant is asserted at the floor.
    function test_TierHaircutFloorsAreMonotonic() public {
        address t3Asset = makeAddr("t3Asset");
        address t4Asset = makeAddr("t4Asset");

        vm.startPrank(admin);
        engine.registerAsset(t3Asset, 3, 1000);
        engine.registerAsset(t4Asset, 4, 1500);
        vm.stopPrank();

        uint16 h1 = engine.haircutBpsOf(address(usdc));
        uint16 h2 = engine.haircutBpsOf(USYC_ARC);
        uint16 h3 = engine.haircutBpsOf(t3Asset);
        uint16 h4 = engine.haircutBpsOf(t4Asset);

        assertLt(h1, h2, "T1 floor must be below T2 floor");
        assertLt(h2, h3, "T2 floor must be below T3 floor");
        assertLt(h3, h4, "T3 floor must be below T4 floor");
    }

    /// @notice Fuzz invariant: for ANY in-range haircuts, a lower-numbered
    ///         non-overlapping tier always prices a strictly smaller
    ///         haircut than a higher one — T1 < T2 < T3 holds pointwise
    ///         (T2 ceiling 500 < T3 floor 1000), and the tier discriminant
    ///         ordering holds for every registration.
    function testFuzz_TierHaircutsStrictlyIncreaseAcrossNonOverlappingTiers(uint16 h2Raw, uint16 h3Raw) public {
        uint16 h2 = uint16(bound(h2Raw, 200, 500));
        uint16 h3 = uint16(bound(h3Raw, 1000, 2000));
        address t2Asset = makeAddr("fuzzT2");
        address t3Asset = makeAddr("fuzzT3");

        vm.startPrank(admin);
        engine.registerAsset(t2Asset, 2, h2);
        engine.registerAsset(t3Asset, 3, h3);
        vm.stopPrank();

        assertEq(engine.tierOf(t2Asset), 2);
        assertEq(engine.tierOf(t3Asset), 3);
        assertLt(engine.haircutBpsOf(address(usdc)), engine.haircutBpsOf(t2Asset));
        assertLt(engine.haircutBpsOf(t2Asset), engine.haircutBpsOf(t3Asset));
    }

    // ---------------------------------------------------------------------
    // registerAsset: validation + access control.
    // ---------------------------------------------------------------------

    /// @notice Happy path: registration round-trips tier and haircut and
    ///         emits `AssetRegistered`.
    function test_RegisterAssetRoundTripsTierAndHaircut() public {
        address rwa = makeAddr("rwa");

        vm.expectEmit(true, false, false, true, address(engine));
        emit ICollateralEngine.AssetRegistered(rwa, 4, 2500);
        vm.prank(admin);
        engine.registerAsset(rwa, 4, 2500);

        assertEq(engine.tierOf(rwa), 4);
        assertEq(engine.haircutBpsOf(rwa), 2500);
    }

    /// @notice Fuzz: any (tier, haircut) pair inside the paper's Table 1
    ///         ranges registers and round-trips.
    function testFuzz_RegisterAssetWithinTierRangeRoundTrips(uint8 tierRaw, uint16 haircutRaw) public {
        uint8 tier = uint8(bound(tierRaw, 1, 4));
        (uint256 minBps, uint256 maxBps) = _rangeOf(tier);
        uint16 haircut = uint16(bound(haircutRaw, minBps, maxBps));
        address asset = makeAddr("fuzzAsset");

        vm.prank(admin);
        engine.registerAsset(asset, tier, haircut);

        assertEq(engine.tierOf(asset), tier);
        assertEq(engine.haircutBpsOf(asset), haircut);
    }

    /// @notice Fuzz: any haircut outside its tier's range reverts with
    ///         `HaircutOutOfRange` (both sides of the range; T1 can only
    ///         exceed, its floor and ceiling are both 0).
    function testFuzz_RegisterAssetOutsideTierRangeReverts(uint8 tierRaw, uint16 haircutRaw) public {
        uint8 tier = uint8(bound(tierRaw, 1, 4));
        (uint256 minBps, uint256 maxBps) = _rangeOf(tier);
        uint16 haircut;
        if (minBps == 0 || haircutRaw % 2 == 1) {
            haircut = uint16(bound(haircutRaw, maxBps + 1, type(uint16).max));
        } else {
            haircut = uint16(bound(haircutRaw, 0, minBps - 1));
        }

        vm.expectRevert(abi.encodeWithSelector(ICollateralEngine.HaircutOutOfRange.selector, tier, haircut));
        vm.prank(admin);
        engine.registerAsset(makeAddr("fuzzBadAsset"), tier, haircut);
    }

    /// @notice Tiers outside the paper's 1-4 taxonomy revert with
    ///         `InvalidTier`.
    function test_RegisterAssetInvalidTierReverts() public {
        vm.expectRevert(abi.encodeWithSelector(ICollateralEngine.InvalidTier.selector, 0));
        vm.prank(admin);
        engine.registerAsset(makeAddr("tierZero"), 0, 0);

        vm.expectRevert(abi.encodeWithSelector(ICollateralEngine.InvalidTier.selector, 5));
        vm.prank(admin);
        engine.registerAsset(makeAddr("tierFive"), 5, 0);
    }

    /// @notice Boundary check: T1 accepts only exactly 0 bps; T2 rejects
    ///         199 (below floor) and 501 (above ceiling).
    function test_RegisterAssetHaircutRangeBoundaries() public {
        address t1 = makeAddr("t1");
        vm.expectRevert(abi.encodeWithSelector(ICollateralEngine.HaircutOutOfRange.selector, 1, 1));
        vm.prank(admin);
        engine.registerAsset(t1, 1, 1);

        vm.expectRevert(abi.encodeWithSelector(ICollateralEngine.HaircutOutOfRange.selector, 2, 199));
        vm.prank(admin);
        engine.registerAsset(t1, 2, 199);

        vm.expectRevert(abi.encodeWithSelector(ICollateralEngine.HaircutOutOfRange.selector, 2, 501));
        vm.prank(admin);
        engine.registerAsset(t1, 2, 501);
    }

    /// @notice Only the admin may register assets.
    function test_RegisterAssetRevertsForNonAdmin() public {
        vm.prank(stranger);
        vm.expectRevert(ICollateralEngine.NotAdmin.selector);
        engine.registerAsset(makeAddr("nope"), 3, 1500);
    }

    /// @notice Zero-address assets cannot be registered.
    function test_RegisterAssetZeroAddressReverts() public {
        vm.prank(admin);
        vm.expectRevert(ICollateralEngine.ZeroAddress.selector);
        engine.registerAsset(address(0), 1, 0);
    }

    /// @notice Re-registration updates the haircut in place and does NOT
    ///         duplicate the asset in the `C_eff` summation domain: the
    ///         trader's effective collateral reflects the new haircut
    ///         exactly once.
    function test_ReRegistrationUpdatesWithoutDoubleCounting() public {
        vm.prank(trader);
        account.deposit(address(usyc), DEPOSIT_AMOUNT);
        assertEq(engine.effectiveCollateral(trader), 980e18);

        // 200 bps -> 500 bps, still inside the T2 range.
        vm.prank(admin);
        engine.registerAsset(address(usyc), 2, 500);

        assertEq(engine.haircutBpsOf(address(usyc)), 500);
        // Single entry at the new haircut: 1000 * (1 - 0.05) = 950, not a
        // 980+950 double-count.
        assertEq(engine.effectiveCollateral(trader), 950e18);
    }

    // ---------------------------------------------------------------------
    // Unregistered-asset reads.
    // ---------------------------------------------------------------------

    /// @notice Tier reads for unknown assets revert with
    ///         `AssetNotRegistered` instead of returning a zero tier.
    function test_TierOfRevertsForUnregisteredAsset() public {
        address unknown = makeAddr("unknown");
        vm.expectRevert(abi.encodeWithSelector(ICollateralEngine.AssetNotRegistered.selector, unknown));
        engine.tierOf(unknown);
    }

    /// @notice Haircut reads for unknown assets revert.
    function test_HaircutBpsOfRevertsForUnregisteredAsset() public {
        address unknown = makeAddr("unknown");
        vm.expectRevert(abi.encodeWithSelector(ICollateralEngine.AssetNotRegistered.selector, unknown));
        engine.haircutBpsOf(unknown);
    }

    /// @notice Valuing an unknown asset reverts.
    function test_AssetValueUsdRevertsForUnregisteredAsset() public {
        address unknown = makeAddr("unknown");
        vm.expectRevert(abi.encodeWithSelector(ICollateralEngine.AssetNotRegistered.selector, unknown));
        engine.assetValueUsd(unknown, 1e18);
    }

    // ---------------------------------------------------------------------
    // Oracle: stub mode and wired mode.
    // ---------------------------------------------------------------------

    /// @notice With no oracle wired, `oraclePriceOf` is the $1.00 stub for
    ///         any asset (todo #12 wires the real one).
    function test_OraclePriceStubReturnsOneDollar() public {
        assertEq(engine.oraclePriceOf(address(usdc)), 1e18);
        assertEq(engine.oraclePriceOf(makeAddr("anything")), 1e18);
    }

    /// @notice Wiring an oracle delegates pricing to it, flows through
    ///         `assetValueUsd`, and clearing the oracle restores stub mode.
    function test_WiredOracleDelegatesPricing() public {
        MockOracle mockOracle = new MockOracle(2e18);

        vm.expectEmit(true, false, false, false, address(engine));
        emit ICollateralEngine.OracleUpdated(address(mockOracle));
        vm.prank(admin);
        engine.setOracle(address(mockOracle));

        assertEq(engine.oraclePriceOf(address(usdc)), 2e18);
        // 1000 USDC, 0% haircut, $2.00 price -> $2000.
        assertEq(engine.assetValueUsd(address(usdc), DEPOSIT_AMOUNT), 2000e18);

        mockOracle.setPrice(3e18);
        assertEq(engine.oraclePriceOf(address(usdc)), 3e18);

        // Clearing the oracle returns the engine to stub mode.
        vm.prank(admin);
        engine.setOracle(address(0));
        assertEq(engine.oraclePriceOf(address(usdc)), 1e18);
    }

    /// @notice Only the admin may wire the oracle.
    function test_SetOracleRevertsForNonAdmin() public {
        vm.prank(stranger);
        vm.expectRevert(ICollateralEngine.NotAdmin.selector);
        engine.setOracle(makeAddr("oracle"));
    }

    // ---------------------------------------------------------------------
    // Balance source wiring.
    // ---------------------------------------------------------------------

    /// @notice `setBalanceSource` stores the source and emits
    ///         `BalanceSourceUpdated`.
    function test_SetBalanceSourceEmitsEvent() public {
        vm.expectEmit(true, false, false, false, address(engine));
        emit ICollateralEngine.BalanceSourceUpdated(address(account));
        vm.prank(admin);
        engine.setBalanceSource(address(account));

        assertEq(address(engine.balanceSource()), address(account));
    }

    /// @notice Only the admin may wire the balance source.
    function test_SetBalanceSourceRevertsForNonAdmin() public {
        vm.prank(stranger);
        vm.expectRevert(ICollateralEngine.NotAdmin.selector);
        engine.setBalanceSource(address(account));
    }

    /// @notice The zero address is not a valid balance source.
    function test_SetBalanceSourceZeroAddressReverts() public {
        vm.prank(admin);
        vm.expectRevert(ICollateralEngine.ZeroAddress.selector);
        engine.setBalanceSource(address(0));
    }

    /// @notice `effectiveCollateral` before wiring reverts with
    ///         `BalanceSourceNotSet` rather than returning a silent zero.
    function test_EffectiveCollateralRevertsWithoutBalanceSource() public {
        CollateralEngine unwired = new CollateralEngine(admin, address(usdc));
        vm.expectRevert(ICollateralEngine.BalanceSourceNotSet.selector);
        unwired.effectiveCollateral(trader);
    }

    // ---------------------------------------------------------------------
    // assetValueUsd.
    // ---------------------------------------------------------------------

    /// @notice Per-asset valuation: `amount · (1 − h_a) · p_a` with the
    ///         $1.00 stub — 1000 USDC at 0% = $1000, 1000 USYC at 2% = $980.
    function test_AssetValueUsdAppliesHaircut() public view {
        assertEq(engine.assetValueUsd(address(usdc), DEPOSIT_AMOUNT), 1000e18);
        assertEq(engine.assetValueUsd(address(usyc), DEPOSIT_AMOUNT), 980e18);
    }

    // ---------------------------------------------------------------------
    // effectiveCollateral.
    // ---------------------------------------------------------------------

    /// @notice Paper example (plan todo #9): 1000 USDC at 0% haircut plus
    ///         1000 USYC at 2% haircut values to $1980 of effective
    ///         collateral — `C_eff = Σ b_a · (1 − h_a) · p_a`
    ///         (paper/synchra.tex:384-386).
    function test_EffectiveCollateralMatchesPaperExample() public {
        vm.startPrank(trader);
        account.deposit(address(usdc), DEPOSIT_AMOUNT);
        account.deposit(address(usyc), DEPOSIT_AMOUNT);
        vm.stopPrank();

        assertEq(engine.effectiveCollateral(trader), 1980e18);
    }

    /// @notice Zero balances contribute nothing: a trader holding only USDC
    ///         is valued at exactly the USDC deposit even though USYC (mock
    ///         and Arc) are also in the summation domain.
    function test_EffectiveCollateralSkipsZeroBalances() public {
        vm.prank(trader);
        account.deposit(address(usdc), DEPOSIT_AMOUNT);

        assertEq(engine.effectiveCollateral(trader), 1000e18);
        // A stranger with no deposits at all sums to zero.
        assertEq(engine.effectiveCollateral(stranger), 0);
    }

    /// @notice Gas budget (plan todo #9): `effectiveCollateral` over a
    ///         4-asset funded account stays under 120k gas. The registered
    ///         list also carries the zero-balance Arc USYC entry, so the
    ///         loop iterates 5 entries while valuing 4 — the conservative
    ///         case for the budget.
    function test_EffectiveCollateralGasBudgetFourAssets() public {
        MockStablecoin wstEth = new MockStablecoin("Wrapped stETH", "wstETH");
        MockStablecoin rwa = new MockStablecoin("Tokenized RWA", "RWA");

        vm.startPrank(admin);
        engine.registerAsset(address(wstEth), 3, 1000);
        engine.registerAsset(address(rwa), 4, 1500);
        vm.stopPrank();

        wstEth.mint(trader, MINT_AMOUNT);
        rwa.mint(trader, MINT_AMOUNT);
        vm.startPrank(trader);
        wstEth.approve(address(account), type(uint256).max);
        rwa.approve(address(account), type(uint256).max);
        account.deposit(address(usdc), DEPOSIT_AMOUNT);
        account.deposit(address(usyc), DEPOSIT_AMOUNT);
        account.deposit(address(wstEth), DEPOSIT_AMOUNT);
        account.deposit(address(rwa), DEPOSIT_AMOUNT);
        vm.stopPrank();

        uint256 gasBefore = gasleft();
        uint256 value = engine.effectiveCollateral(trader);
        uint256 gasUsed = gasBefore - gasleft();

        // 1000 + 980 + 900 + 850 = 3730 USD effective.
        assertEq(value, 3730e18);
        emit log_named_uint("effectiveCollateral(4 assets)", gasUsed);
        assertLe(gasUsed, 120_000, "effectiveCollateral exceeds 120k gas budget");
    }
}
