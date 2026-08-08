// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {ERC20} from "openzeppelin-contracts/contracts/token/ERC20/ERC20.sol";
import {ERC20Permit} from "openzeppelin-contracts/contracts/token/ERC20/extensions/ERC20Permit.sol";

/// @title  TestUSDC
/// @notice A real, deployed-to-testnet ERC20 standing in for USDC as Cerdic's
///         one collateral asset, per this deployment's own choice not to use
///         Arc's real Circle-issued USDC for testing (Arc's real USDC is the
///         network's native gas currency instead, unrelated to this
///         contract). 18 decimals, matching every other mock stablecoin this
///         repo already uses (CollateralEngine's stub oracle prices at
///         exactly $1.00 per whole token, same "1e18 == $1.00" convention).
/// @dev    Two ways new balance enters the system, both real minting, no
///         fabricated numbers anywhere downstream:
///         - `claimFaucet`: permissionless, self-serve onboarding — a new
///           trader mints their own starting balance directly from their own
///           wallet, no backend minter key ever needs to exist or be trusted.
///         - `adminMint`: bulk seeding for market makers/backstop liquidity,
///           gated to whoever deployed this contract.
/// @dev    Also `ERC20Permit` (EIP-2612): lets `Account.depositWithPermit`
///         collapse the old approve-then-deposit two-transaction,
///         two-signature flow into one off-chain signature plus one
///         on-chain call — real UX cost confirmed live (a trader having
///         to sign and wait on two separate transactions just to move
///         collateral in). A permit signature never touches the chain on
///         its own; `depositWithPermit`'s own doc covers how it's spent.
contract TestUSDC is ERC20, ERC20Permit {
    /// @notice Minted per successful `claimFaucet` call.
    uint256 public constant FAUCET_AMOUNT = 10_000e18;

    /// @notice Minimum time between successful claims from the same address —
    ///         long enough to stop one address refreshing its way to
    ///         unlimited supply, short enough that an active tester never has
    ///         to spin up a fresh wallet just to keep testing.
    uint256 public constant FAUCET_COOLDOWN = 1 days;

    address public immutable admin;

    /// @notice Timestamp of each address's last successful claim, 0 meaning
    ///         "never claimed" — checked against, never assumed.
    mapping(address => uint256) public lastClaim;

    event FaucetClaimed(address indexed trader, uint256 amount);
    event AdminMinted(address indexed to, uint256 amount);

    error NotAdmin();
    error FaucetOnCooldown(address trader, uint256 claimableAt);
    error ZeroAddress();
    error ZeroAmount();

    modifier onlyAdmin() {
        if (msg.sender != admin) revert NotAdmin();
        _;
    }

    constructor(address admin_) ERC20("Cerdic Test USDC", "tUSDC") ERC20Permit("Cerdic Test USDC") {
        if (admin_ == address(0)) revert ZeroAddress();
        admin = admin_;
    }

    /// @notice Mints `FAUCET_AMOUNT` to the caller. Permissionless by design —
    ///         onboarding a new trader should never depend on a backend
    ///         service being up or a minter key being trusted; the only
    ///         guard is the per-address cooldown above.
    function claimFaucet() external {
        uint256 last = lastClaim[msg.sender];
        // `last == 0` means "never claimed" — must be immediately claimable,
        // not gated behind a full cooldown counted from the Unix epoch.
        // Confirmed live by this contract's own fuzz test: folding that case
        // into the additive `last + FAUCET_COOLDOWN` check made every fresh
        // address unclaimable until real chain time passed FAUCET_COOLDOWN
        // since genesis.
        if (last != 0) {
            uint256 claimableAt = last + FAUCET_COOLDOWN;
            if (block.timestamp < claimableAt) revert FaucetOnCooldown(msg.sender, claimableAt);
        }

        lastClaim[msg.sender] = block.timestamp;
        _mint(msg.sender, FAUCET_AMOUNT);
        emit FaucetClaimed(msg.sender, FAUCET_AMOUNT);
    }

    /// @notice True once `trader` could call `claimFaucet` without reverting.
    function canClaim(address trader) external view returns (bool) {
        uint256 last = lastClaim[trader];
        return last == 0 || block.timestamp >= last + FAUCET_COOLDOWN;
    }

    /// @notice Bulk mint for market makers / backstop liquidity seeding —
    ///         real supply, admin-gated, separate from the self-serve faucet
    ///         above so seeding a market maker's wallet doesn't compete with
    ///         or reset a real trader's own cooldown.
    function adminMint(address to, uint256 amount) external onlyAdmin {
        if (to == address(0)) revert ZeroAddress();
        if (amount == 0) revert ZeroAmount();
        _mint(to, amount);
        emit AdminMinted(to, amount);
    }
}
