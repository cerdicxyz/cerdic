//! Rust replacement for scripts/local-dev-up.sh, local-dev-down.sh, and
//! seed_trades_loop.sh — same orchestration, same external tools (anvil,
//! forge, the already-built matcher/market_maker/keeper_liquidator/
//! demo_client binaries), just typed and without bash's quoting/array
//! footguns (the private-key truncation and `set --` word-splitting bugs
//! that actually broke this setup twice were both bash-shaped problems).
//!
//! This still shells out to anvil/forge/cargo — those are real external
//! tools, not something to reimplement — but every parsing step (anvil's
//! dev keys, forge's deploy addresses, JSON responses) is done with real
//! types instead of awk/grep/python3 one-liners.
//!
//! Usage:
//!   cargo run --release --bin local_dev -- up
//!   cargo run --release --bin local_dev -- down
//!   cargo run --release --bin local_dev -- seed-loop   (usually launched
//!     BY `up`, not run directly — see cmd_seed_loop's own doc)

// Every `Command::spawn()` in this file is a deliberately long-lived
// background process (anvil, the matcher, market makers, keeper_liquidator,
// the oracle-push/seed loops) — managed via PID files and `cmd_down`'s own
// kill sweep, not something this process is ever meant to `.wait()` on
// (that would block `up` forever, since these processes run until
// explicitly killed). Newer clippy's `zombie_processes` lint doesn't know
// that shape is intentional here.
#![allow(clippy::zombie_processes)]

use alloy::{
    network::{EthereumWallet, TransactionBuilder},
    primitives::{Address, U256},
    providers::{Provider, ProviderBuilder},
    rpc::types::TransactionRequest,
    signers::local::PrivateKeySigner,
    sol,
    sol_types::SolCall,
};
use rand::Rng;
use serde::Deserialize;
use std::{
    fs,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

// Every long-lived child this binary spawns (anvil, matcher, market
// makers, keeper, seed-loop) calls `.process_group(0)` before spawning,
// or it stays attached to whatever terminal/session launched
// `local_dev up` — bash's `nohup ... & disown` combo was doing real
// detachment work that a plain `Command::spawn()` does NOT replicate on
// its own. Confirmed live: the matcher died silently mid-stream (no
// panic, no error in its own log) the first time this binary's `up`
// command was run from a shell session that later closed — a
// session-level SIGHUP, not an application bug. `process_group(0)` puts
// the child in its own new process group (the real Rust equivalent of
// `setsid`), so signals delivered to the launching session's process
// group never reach it.

sol! {
    interface IAttestationRouter {
        function authorizeTEE(address tee) external;
    }

    interface IMockPyth {
        function createPriceFeedUpdateData(bytes32 id, int64 price, uint64 conf, int32 expo, int64 emaPrice, uint64 emaConf, uint64 publishTime) external pure returns (bytes memory);
        function updatePriceFeeds(bytes[] calldata updateData) external payable;
        function getUpdateFee(bytes[] calldata updateData) external view returns (uint256 feeAmount);
    }

    interface IMockV3Aggregator {
        function updateAnswer(int256 answer) external;
    }

    /// `LocalStablecoin`'s own `mint` (DeployLocal.s.sol) — unrestricted,
    /// any caller can mint any amount to any address, see that contract's
    /// own doc: a real local stand-in for USDC/EURC, not a fabricated
    /// balance, every unit genuinely minted on this chain.
    interface ILocalStablecoin {
        function mint(address to, uint256 amount) external;
    }

    /// Standard ERC20 `approve` — needed before `IAccountContract.deposit`
    /// below, which pulls funds via `transferFrom` (Account.sol's own doc).
    interface IErc20Approve {
        function approve(address spender, uint256 amount) external returns (bool);
    }

    /// `Account.sol`'s own `deposit` — real custody, real transferFrom,
    /// see `deposit_as_trader`'s own doc on why this binary calls it (not
    /// just mints) for the demo trader specifically.
    interface IAccountContract {
        function deposit(address asset, uint256 amount) external;
    }
}

const ANVIL_RPC: &str = "http://127.0.0.1:8545";
// A fixed, deterministic `CERDIC_DEV_SECRETS_SEED` (`kms.rs`'s own doc on
// why this env var exists at all) rather than leaving the matcher on its
// ephemeral-random default. Real, confirmed bug this fixes: restarting
// just the matcher binary (not a full `up`, e.g. to deploy a Rust-only
// fix against an already-live chain, exactly what `CERDIC_DB_PATH`'s own
// doc above says this setup is for) used to mint a brand new random
// sealing key every time, silently breaking decryption of every
// already-sealed position from before the restart — every market's
// positions, both real traders' and the market makers' own inventory,
// went "AEAD open failed: wrong key or corrupted blob" and got skipped
// as if they no longer existed. That corrupted close/margin math (a
// close miscounted as a fresh open, since the existing position vanished
// from the matcher's view) and market maker inventory tracking (skewed
// quoting from a maker that thinks its own position reset to flat)
// simultaneously, confirmed live across EURC/USDC, AUD/USD, GBP/USD,
// HYPE/USD, USD/JPY, and XAU/USD after one such restart. A fixed seed
// means the sealing key — and therefore every previously-sealed
// position — survives any matcher-only restart, exactly the "ordinary
// local multi-session testing" case `CERDIC_DEV_SECRETS_SEED` was built
// for. Never valid outside local dev (kms.rs's own doc), and this value
// is not a secret worth protecting: real, both are true and unrelated.
const DEV_SECRETS_SEED: &str = "0x1c4e3a8f295d6b71092ea4c8f0d3b657a1928e4dcf30b67158a9d2c4e6f1a3b7";

// Real, confirmed bug this fixes: `BackstopConfig::default().notional_cap`
// is `Qty::MAX` (`backstop.rs`'s own doc: "unset... keeps the backstop's
// own unbounded default"), and local_dev never overrode it, so the
// synthetic backstop counterparty had literally no limit on how much of
// any single order it would silently absorb. A trader with enough real
// deposited collateral could submit (and did, live: a 100,000-unit
// EURC/USDC market order) an order dwarfing the real resting book — each
// maker wave's own ladder here totals roughly `MM_QUOTE_SIZE * ladder
// levels` per side (200*14 + 150*12 = 4,600) — and have nearly all of it
// filled by the backstop at a fair TWAP price with zero real price
// impact, not the "did not fill, skipping" a real thin book should have
// produced. 2,000 (well under one side's real ladder total, above any
// ordinary single trade) caps how much of one order the backstop alone
// will ever cover; the rest is real resting depth or goes unfilled
// (`TimeInForce::ImmediateOrCancel`'s own semantics), same as any order
// genuinely too large for the book that exists.
const BACKSTOP_NOTIONAL_CAP: &str = "2000";
const MATCHER_URL: &str = "http://127.0.0.1:8787";
const MARKETS: [&str; 9] = [
    "EURC/USDC",
    "BTC/USDC",
    "GBP/USD",
    "AUD/USD",
    "USD/JPY",
    "XAU/USD",
    "KR200/USD",
    "BRENT/USD",
    "HYPE/USD",
];
// Real Pyth Hermes feed ids, same provenance as
// packages/contracts/script/DeployMoreMarketsLocal.s.sol's own doc
// comment (checked live against Hermes the day that script was written).
const FEED_IDS: [&str; 9] = [
    "a995d00bb36a63cef7fd2c287dc105fc8f3d93779f062f09551b0af3e81ec30b",
    "e62df6c8b4a85fe1a67db44dc12de5db330f7ac66b72dc658afedf0f4a415b43",
    "84c2dde9633d93d1bcad84e7dc41c9d56578b7ec52fabedc1f335d673df0a7c1",
    "67a6f93030420c1c9e3fe37c1ab6b77966af82f995944a9fefce357a22854a80",
    "ef2c98c804ba503c6a707e38be4dfbb16683775f195b091252bf24693042fd52",
    "765d2ba906dbc32ca17cc11f5310a89e9ee1f6420508c63861f2f8ba4ee34bb2",
    "7be2b3f9f9d02b1ffcf61fc26ad5cc6aff4dd02044f9abc22ee57f37b3b5d2e5",
    "6e3607735df0f027dc63890cc48055cccf1551003cc7a7c934cabe04485d1193",
    "4279e31cc369bbcc2faf022b382b080e32a8e689ff20fbc530d2a603eb6cd98b",
];

fn state_dir() -> PathBuf {
    PathBuf::from("/tmp/cerdic-local")
}

fn repo_root() -> PathBuf {
    // This binary lives at crates/cerdic-tee-matcher/src/bin/local_dev.rs;
    // CARGO_MANIFEST_DIR is crates/cerdic-tee-matcher.
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("repo root resolves")
}

fn log(msg: impl AsRef<str>) {
    println!("==> {}", msg.as_ref());
}

fn die(msg: impl AsRef<str>) -> ! {
    eprintln!("FATAL: {}", msg.as_ref());
    std::process::exit(1);
}

#[tokio::main]
async fn main() {
    let cmd = std::env::args().nth(1).unwrap_or_default();
    match cmd.as_str() {
        "up" => cmd_up().await,
        "down" => cmd_down(),
        "seed-loop" => cmd_seed_loop().await,
        "oracle-push-loop" => cmd_oracle_push_loop().await,
        _ => die("usage: local_dev <up|down|seed-loop|oracle-push-loop>"),
    }
}

// ---------------------------------------------------------------------------
// down
// ---------------------------------------------------------------------------

/// Kills everything `up` starts. PID files are the primary path; a
/// `pkill -f` sweep afterwards catches anything a stale/missing PID file
/// missed (e.g. a run started before this binary existed, or a process
/// that outlived its recorded PID some other way) — same belt-and-
/// suspenders posture the bash version had, kept because it's genuinely
/// useful, not because Rust needs the OS-level fallback any less.
fn cmd_down() {
    let dir = state_dir();
    for pid_file in [
        "anvil.pid",
        "matcher.pid",
        "keeper_liquidator.pid",
        "seed_loop.pid",
        "oracle_push_loop.pid",
        "market_maker.pids",
    ] {
        let path = dir.join(pid_file);
        let Ok(contents) = fs::read_to_string(&path) else { continue };
        for line in contents.lines() {
            if let Ok(pid) = line.trim().parse::<i32>() {
                let _ = Command::new("kill")
                    .arg(pid.to_string())
                    .stderr(Stdio::null())
                    .stdout(Stdio::null())
                    .status();
            }
        }
    }
    for pattern in [
        "target/release/cerdic-tee-matcher",
        "target/release/market_maker",
        "target/release/keeper_liquidator",
        "target/release/keeper_price_pusher",
        "target/release/local_dev.*seed-loop",
        "target/release/local_dev.*oracle-push-loop",
        "^anvil$",
    ] {
        let _ =
            Command::new("pkill").args(["-f", pattern]).stderr(Stdio::null()).stdout(Stdio::null()).status();
    }
    log("stopped");
}

// ---------------------------------------------------------------------------
// up
// ---------------------------------------------------------------------------

async fn cmd_up() {
    cmd_down();
    let dir = state_dir();
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create state dir");
    let root = repo_root();

    // --- 1. anvil ---------------------------------------------------------
    log("starting anvil (--accounts 20: 1 deployer + 9 wave-a makers + 9 wave-b makers + 1 demo trader, all distinct)");
    let anvil_log = fs::File::create(dir.join("anvil.log")).unwrap();
    let anvil_child = Command::new("anvil")
        .args(["--accounts", "20"])
        .stdout(Stdio::from(anvil_log.try_clone().unwrap()))
        .stderr(Stdio::from(anvil_log))
        .process_group(0)
        .spawn()
        .unwrap_or_else(|e| die(format!("failed to spawn anvil: {e}")));
    fs::write(dir.join("anvil.pid"), anvil_child.id().to_string()).unwrap();

    wait_http_ok(ANVIL_RPC, 30).await.unwrap_or_else(|| die("anvil never came up, see anvil.log"));

    // Parse anvil's own deterministic dev keys straight out of its log
    // instead of hand-copying them — a dropped hex digit in a copy-pasted
    // key is exactly what broke a market maker and a keeper earlier in
    // this same setup, twice, back when this was done by hand.
    let mm_keys = parse_anvil_keys(&dir.join("anvil.log"));
    if mm_keys.len() < 20 {
        die(format!("expected 20 anvil dev keys, parsed {} — anvil log format changed?", mm_keys.len()));
    }
    let deployer_key = &mm_keys[0];

    // --- 2. deploy contracts: kernel + all 9 markets -----------------------
    let contracts_dir = root.join("packages/contracts");

    log("deploying kernel + EURC/USDC market (DeployLocal)");
    let deploy_local = run_forge_script(
        &contracts_dir,
        "script/DeployLocal.s.sol:DeployLocal",
        &[("PRIVATE_KEY", deployer_key)],
    )
    .unwrap_or_else(|e| die(format!("DeployLocal failed: {e}")));
    fs::write(dir.join("deploy_local.log"), &deploy_local).unwrap();

    let mock_pyth = get_addr(&deploy_local, "MockPyth:").unwrap_or_else(|| die("failed to parse MockPyth"));
    let oracle_hub =
        get_addr(&deploy_local, "OracleHub:").unwrap_or_else(|| die("failed to parse OracleHub"));
    let risk_monitor =
        get_addr(&deploy_local, "RiskMonitor:").unwrap_or_else(|| die("failed to parse RiskMonitor"));
    let attestation_router = get_addr(&deploy_local, "AttestationRouter:")
        .unwrap_or_else(|| die("failed to parse AttestationRouter"));
    let eurc_usdc_market = get_addr(&deploy_local, "FxPerpMarket (EURC/USDC):")
        .unwrap_or_else(|| die("failed to parse EURC/USDC market"));
    let usdc_token =
        get_addr(&deploy_local, "USDC (mock):").unwrap_or_else(|| die("failed to parse USDC (mock)"));
    let account_contract =
        get_addr(&deploy_local, "Account:").unwrap_or_else(|| die("failed to parse Account"));

    log("deploying BTC/USDC market (DeployPerpMarketLocal)");
    let deploy_btc = run_forge_script(
        &contracts_dir,
        "script/DeployPerpMarketLocal.s.sol:DeployPerpMarketLocal",
        &[
            ("PRIVATE_KEY", deployer_key),
            ("ORACLE_HUB", &oracle_hub),
            ("RISK_MONITOR", &risk_monitor),
            ("ATTESTATION_ROUTER", &attestation_router),
            ("MOCK_PYTH", &mock_pyth),
        ],
    )
    .unwrap_or_else(|e| die(format!("DeployPerpMarketLocal failed: {e}")));
    fs::write(dir.join("deploy_btc.log"), &deploy_btc).unwrap();
    let btc_usdc_market = get_addr(&deploy_btc, "PerpMarket (BTC/USDC):")
        .unwrap_or_else(|| die("failed to parse BTC/USDC market"));
    let btc_aggregator = get_addr(&deploy_btc, "BTCAggregator (mock):")
        .unwrap_or_else(|| die("failed to parse BTC aggregator"));

    log("deploying remaining 7 markets (DeployMoreMarketsLocal)");
    let deploy_more = run_forge_script(
        &contracts_dir,
        "script/DeployMoreMarketsLocal.s.sol:DeployMoreMarketsLocal",
        &[
            ("PRIVATE_KEY", deployer_key),
            ("ORACLE_HUB", &oracle_hub),
            ("RISK_MONITOR", &risk_monitor),
            ("ATTESTATION_ROUTER", &attestation_router),
            ("MOCK_PYTH", &mock_pyth),
        ],
    )
    .unwrap_or_else(|e| die(format!("DeployMoreMarketsLocal failed: {e}")));
    fs::write(dir.join("deploy_more.log"), &deploy_more).unwrap();

    let market_contracts: Vec<String> = [
        eurc_usdc_market,
        btc_usdc_market,
        get_addr(&deploy_more, "GBP/USD (FxPerpMarket)").unwrap_or_else(|| die("failed to parse GBP/USD")),
        get_addr(&deploy_more, "AUD/USD (FxPerpMarket)").unwrap_or_else(|| die("failed to parse AUD/USD")),
        get_addr(&deploy_more, "USD/JPY (FxPerpMarket)").unwrap_or_else(|| die("failed to parse USD/JPY")),
        get_addr(&deploy_more, "XAU/USD (PerpMarket)").unwrap_or_else(|| die("failed to parse XAU/USD")),
        get_addr(&deploy_more, "KR200/USD (PerpMarket, EWY proxy)")
            .unwrap_or_else(|| die("failed to parse KR200/USD")),
        get_addr(&deploy_more, "BRENT/USD (PerpMarket, front-month proxy)")
            .unwrap_or_else(|| die("failed to parse BRENT/USD")),
        get_addr(&deploy_more, "HYPE/USD (PerpMarket)").unwrap_or_else(|| die("failed to parse HYPE/USD")),
    ]
    .to_vec();

    // DeployMoreMarketsLocal's own _log helper prints an "  aggregator:"
    // line right after each market's own line, in the exact declaration
    // order: GBP, AUD, USD/JPY, XAU, KR200, BRENT, HYPE — 7 total, one
    // per market that script deploys (FX markets get one too, even
    // though FX funding doesn't use it, see FxPerpMarket's own doc on
    // why pythPrimary needs an answering aggregator regardless).
    let more_aggregators = get_addrs_all(&deploy_more, "aggregator:");
    if more_aggregators.len() != 7 {
        die(format!(
            "expected 7 aggregator addresses from DeployMoreMarketsLocal, parsed {}",
            more_aggregators.len()
        ));
    }

    for (m, c) in MARKETS.iter().zip(market_contracts.iter()) {
        log(format!("  {m} -> {c}"));
    }

    // Funding no longer needs seeding here at all: the matcher now computes
    // and charges its own funding rate in-process (`AppState::poll_funding_native`,
    // book mid vs live Hermes oracle, no external central-bank fetch), and
    // charges it directly against `collateral_delta` at close/liquidation
    // instead of through this contract's separate, on-chain `fundingIndex`
    // (which this binary used to seed via `RATE_KEEPER_ROLE` calls here).
    // That on-chain value is still real and still accrues on its own via
    // the oracle-price-push loop below, but it's no longer what's actually
    // charged, so there's nothing left for local dev to seed into it.

    let settlement_contracts = MARKETS
        .iter()
        .zip(market_contracts.iter())
        .map(|(m, c)| format!("{m}={c}"))
        .collect::<Vec<_>>()
        .join(",");

    // --- 3. matcher ---------------------------------------------------------
    log("starting cerdic-tee-matcher (all 9 markets wired)");
    let bin_dir = root.join("crates/target/release");
    let matcher_log = fs::File::create(dir.join("matcher.log")).unwrap();
    // Without this, `AppState::oracle_feed_mapping` stays empty and every
    // live-price-dependent path (`poll_oracle_prices`'s backstop TWAP feed,
    // `compute_margin`'s live mark price, and now `poll_funding_native`)
    // silently no-ops forever — real, but a gap that predates this specific
    // change: local dev never set this before, it just happened not to
    // matter until funding needed a real live price to compute against.
    let oracle_feeds =
        MARKETS.iter().zip(FEED_IDS.iter()).map(|(m, f)| format!("{m}={f}")).collect::<Vec<_>>().join(",");
    let matcher_child = Command::new(bin_dir.join("cerdic-tee-matcher"))
        .env("SETTLEMENT_RPC_URL", ANVIL_RPC)
        .env("CERDIC_SETTLEMENT_CONTRACTS", &settlement_contracts)
        .env("CERDIC_ORACLE_FEEDS", &oracle_feeds)
        .env("CERDIC_ACCOUNT_CONTRACT", &account_contract)
        .env("CERDIC_COLLATERAL_ASSET", &usdc_token)
        .env("CERDIC_RISK_MONITOR_CONTRACT", &risk_monitor)
        .env("CERDIC_ENABLE_DEBUG_SEED", "1")
        .env("CERDIC_LOG", "info")
        .env("CERDIC_DEV_SECRETS_SEED", DEV_SECRETS_SEED)
        .env("CERDIC_BACKSTOP_NOTIONAL_CAP", BACKSTOP_NOTIONAL_CAP)
        // Same state dir every other local_dev artifact (logs, pids) lives
        // in — a real file, survives this one matcher process restarting.
        // NOT survived by a fresh `local_dev up`, which always starts
        // anvil and every contract from scratch (a new `dir` per run, see
        // `state_dir`'s own doc), so pre-restart candles/nonces wouldn't
        // line up with a brand new chain anyway; this matters for
        // restarting just the matcher binary against an already-live chain.
        .env("CERDIC_DB_PATH", dir.join("cerdic-state.redb"))
        .stdout(Stdio::from(matcher_log.try_clone().unwrap()))
        .stderr(Stdio::from(matcher_log))
        .process_group(0)
        .spawn()
        .unwrap_or_else(|e| {
            die(format!(
                "matcher binary not built? cargo build --release --bin cerdic-tee-matcher first ({e})"
            ))
        });
    fs::write(dir.join("matcher.pid"), matcher_child.id().to_string()).unwrap();

    log(format!("waiting for matcher on {MATCHER_URL}"));
    let pubkey_resp =
        wait_pubkey(MATCHER_URL, 180).await.unwrap_or_else(|| die("matcher never came up, see matcher.log"));
    log(format!(
        "matcher settlement signer: {} (stable across matcher-only restarts now, via DEV_SECRETS_SEED — only a full `local_dev up` mints a new one)",
        pubkey_resp.settlement_address
    ));

    // --- 4. fund + TEE-authorize the ephemeral settlement signer -----------
    log("funding + TEE-authorizing the settlement signer");
    fund_and_authorize(deployer_key, &pubkey_resp.settlement_address, &attestation_router)
        .await
        .unwrap_or_else(|e| die(format!("fund/authorize failed: {e}")));

    // --- 4b. optional real test-wallet funding (ETH + mock USDC) ---------
    // `LOCAL_DEV_FUND_ADDRESS`, if set, gets 10 ETH and 100,000 real
    // minted USDC — the same manual `cast send`/`cast call` sequence this
    // repo's own testing kept needing after every redeploy (USDC/Account
    // addresses regenerate each run), done here once instead of by hand.
    // Unset by default: this is a convenience for a specific person's own
    // wallet, not something every `up` should assume.
    if let Ok(fund_address) = std::env::var("LOCAL_DEV_FUND_ADDRESS") {
        log(format!("funding test wallet {fund_address} (10 ETH + 100,000 mock USDC)"));
        fund_test_wallet(deployer_key, &fund_address, &usdc_token)
            .await
            .unwrap_or_else(|e| die(format!("test wallet funding failed: {e}")));
    }

    // --- 5. two market makers per market ------------------------------------
    // Wave-a (index 1-9) and wave-b (index 10-18): different identity,
    // size, spread, and cadence each, so the combined book layers like
    // real participants instead of one bot's constant size repeated.
    log("starting 2 market_makers per market (18 total, RUST_LOG=info)");
    let mut mm_pids = Vec::new();
    for (i, (market, feed)) in MARKETS.iter().zip(FEED_IDS.iter()).enumerate() {
        let contract = &market_contracts[i];
        let safe_name = market.replace('/', "_");

        let key_a = &mm_keys[i + 1];
        let log_a = fs::File::create(dir.join(format!("mm_{safe_name}_a.log"))).unwrap();
        let child = Command::new(bin_dir.join("market_maker"))
            .env("RUST_LOG", "info")
            .env("MATCHER_URL", MATCHER_URL)
            .env("SETTLEMENT_RPC_URL", ANVIL_RPC)
            .env("MARKET_ID", market)
            .env("MARKET_CONTRACT_ADDRESS", contract)
            .env("PYTH_FEED_ID", feed)
            .env("MM_PRIVATE_KEY", key_a)
            // Bumped from 80/5 (size/levels): this is what was actually
            // capping real book depth regardless of market_maker.rs's
            // own tuning — these per-wave envs override that binary's
            // defaults outright, so raising the defaults there alone
            // never reached the running book. Testnet, no real capital
            // at risk, so "go big" is the actual fix, not a workaround.
            .env("MM_QUOTE_SIZE", "200")
            .env("MM_SPREAD_BPS", "20")
            .env("MM_LADDER_LEVELS", "14")
            .stdout(Stdio::from(log_a.try_clone().unwrap()))
            .stderr(Stdio::from(log_a))
            .process_group(0)
            .spawn()
            .unwrap_or_else(|e| die(format!("failed to spawn market_maker: {e}")));
        mm_pids.push(child.id());
        std::thread::sleep(Duration::from_secs(1));

        let key_b = &mm_keys[i + 10];
        let log_b = fs::File::create(dir.join(format!("mm_{safe_name}_b.log"))).unwrap();
        let child = Command::new(bin_dir.join("market_maker"))
            .env("RUST_LOG", "info")
            .env("MATCHER_URL", MATCHER_URL)
            .env("SETTLEMENT_RPC_URL", ANVIL_RPC)
            .env("MARKET_ID", market)
            .env("MARKET_CONTRACT_ADDRESS", contract)
            .env("PYTH_FEED_ID", feed)
            .env("MM_PRIVATE_KEY", key_b)
            // Same "these override market_maker.rs's own defaults, go
            // big since it's testnet" reasoning as wave-a above — kept
            // deliberately smaller/wider/slower than wave-a for real
            // layered variety (two bots that look identical isn't real
            // depth, it's one bot's book doubled), not scaled back.
            .env("MM_QUOTE_SIZE", "150")
            .env("MM_SPREAD_BPS", "35")
            .env("MM_REQUOTE_INTERVAL_SECS", "11")
            .env("MM_LADDER_LEVELS", "12")
            .stdout(Stdio::from(log_b.try_clone().unwrap()))
            .stderr(Stdio::from(log_b))
            .process_group(0)
            .spawn()
            .unwrap_or_else(|e| die(format!("failed to spawn market_maker: {e}")));
        mm_pids.push(child.id());
        std::thread::sleep(Duration::from_secs(1));
    }
    fs::write(
        dir.join("market_maker.pids"),
        mm_pids.iter().map(|p| p.to_string()).collect::<Vec<_>>().join("\n"),
    )
    .unwrap();

    // --- 6. liquidator keeper -------------------------------------------
    log("starting keeper_liquidator (all 9 markets)");
    let liquidator_addr = PrivateKeySigner::from_slice(&hex_decode_key(deployer_key)).unwrap().address();
    let keeper_log = fs::File::create(dir.join("keeper_liquidator.log")).unwrap();
    let keeper_child = Command::new(bin_dir.join("keeper_liquidator"))
        .env("MATCHER_URL", MATCHER_URL)
        .env("SETTLEMENT_RPC_URL", ANVIL_RPC)
        .env("KEEPER_LIQUIDATOR_ADDRESS", liquidator_addr.to_string())
        .env("KEEPER_MARKET_CONTRACTS", &settlement_contracts)
        .stdout(Stdio::from(keeper_log.try_clone().unwrap()))
        .stderr(Stdio::from(keeper_log))
        .process_group(0)
        .spawn()
        .unwrap_or_else(|e| die(format!("failed to spawn keeper_liquidator: {e}")));
    fs::write(dir.join("keeper_liquidator.pid"), keeper_child.id().to_string()).unwrap();

    // keeper_price_pusher is deliberately NOT started: it pushes real
    // Hermes-signed update data, which local MockPyth rejects (it expects
    // its own mock-format update data instead). Only works against a real
    // Pyth deployment.

    // --- 7. seed a handful of varied trades per market ----------------------
    // No hardcoded buy/sell/qty script here — every seed trade is decided
    // the same way `cmd_seed_loop` decides its own: read the market's
    // ACTUAL current book, pick a side with a mean-reversion bias off the
    // market's own live mid (see `pick_side` below), and cross at
    // whatever the real current best bid/ask is. A fixed table of sides
    // and sizes read as a scripted replay, not real trade flow, and
    // couldn't react to whatever the market makers actually quoted.
    log("waiting for makers to establish initial depth before seeding trades");
    tokio::time::sleep(Duration::from_secs(5)).await;
    let demo_trader_key = mm_keys[19].clone();
    // The demo trader submits real TAKER orders (`/order`, via
    // `demo_client`), which the real collateral gate (`api.rs`'s
    // `check_deposited_collateral`) now actually enforces — market maker
    // wallets never needed this (they only ever rest `/offer`s, exempt by
    // design, see that check's own doc), but this one does, or every
    // seed trade gets rejected with real $0 collateral. Confirmed live:
    // this exact failure, before this fund step existed.
    let demo_trader_address = demo_trader_key
        .parse::<PrivateKeySigner>()
        .unwrap_or_else(|e| die(format!("bad demo trader key: {e}")))
        .address()
        .to_string();
    fund_test_wallet(deployer_key, &demo_trader_address, &usdc_token)
        .await
        .unwrap_or_else(|e| die(format!("demo trader funding failed: {e}")));
    // Mint alone leaves `Account.collateralBalanceOf` at zero — see
    // `deposit_as_trader`'s own doc, this is the step that actually makes
    // the demo trader's own seed orders pass the real collateral gate.
    deposit_as_trader(&demo_trader_key, &usdc_token, &account_contract)
        .await
        .unwrap_or_else(|e| die(format!("demo trader deposit failed: {e}")));
    let client = reqwest::Client::new();
    let mut rng = rand::thread_rng();
    let mut mid_refs: std::collections::HashMap<&str, f64> = std::collections::HashMap::new();
    for &market in MARKETS.iter() {
        let mut seeded = 0;
        for _ in 0..3 {
            match fire_priced_trade(&client, &bin_dir, market, &demo_trader_key, &mut rng, &mut mid_refs)
                .await
            {
                Some(true) => seeded += 1,
                Some(false) => {}
                None => break, // no live depth yet, stop trying this market
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        if seeded > 0 {
            log(format!("  {market}: seeded {seeded}/3 real trades"));
        } else {
            log(format!("  {market}: no depth yet, skipping seed trades"));
        }
    }

    // --- 8. continuous REAL oracle price pusher for PerpMarket-based
    //    markets (BTC/XAU/KR200/BRENT/HYPE) -----------------------------
    // Pushes the REAL live Hermes price into both MockPyth and each
    // market's own Chainlink MockV3Aggregator every cycle. This is what
    // makes PerpMarket.sol's own on-chain funding formula (mark vs index
    // divergence, see that contract's own doc: pMark = median(pyth,
    // chainlink, TWAP-of-pyth), pIndex = raw instant pyth) produce a
    // REAL, non-zero rate over time: pushing the same real price to both
    // legs still leaves pMark pulled toward the TWAP (a genuinely lagging
    // indicator) while pIndex tracks the instant price, so real
    // divergence emerges naturally from real price movement — nothing
    // here is a synthetic offset. Also fixes the recurring StalePrice
    // reverts seen all session: nothing else in local dev ever refreshed
    // Pyth/Chainlink after their one-time deploy-time seed.
    let oracle_push_targets = [
        (MARKETS[1], btc_aggregator.as_str()),
        (MARKETS[5], more_aggregators[3].as_str()),
        (MARKETS[6], more_aggregators[4].as_str()),
        (MARKETS[7], more_aggregators[5].as_str()),
        (MARKETS[8], more_aggregators[6].as_str()),
    ]
    .iter()
    .map(|(m, a)| format!("{m}={a}"))
    .collect::<Vec<_>>()
    .join(",");
    log("starting continuous real oracle price pusher (BTC/XAU/KR200/BRENT/HYPE, live Hermes prices)");
    let oracle_push_log = fs::File::create(dir.join("oracle_push_loop.log")).unwrap();
    let this_exe = std::env::current_exe().expect("current_exe");
    let oracle_push_child = Command::new(&this_exe)
        .arg("oracle-push-loop")
        .env("ORACLE_PUSH_DEPLOYER_KEY", deployer_key)
        .env("ORACLE_PUSH_MOCK_PYTH", &mock_pyth)
        .env("ORACLE_PUSH_TARGETS", &oracle_push_targets)
        .stdout(Stdio::from(oracle_push_log.try_clone().unwrap()))
        .stderr(Stdio::from(oracle_push_log))
        .process_group(0)
        .spawn()
        .unwrap_or_else(|e| die(format!("failed to spawn oracle-push-loop: {e}")));
    fs::write(dir.join("oracle_push_loop.pid"), oracle_push_child.id().to_string()).unwrap();

    // --- 9. continuous trade seeder ----------------------------------------
    log("starting continuous trade seeder (real crossing trades every 15-45s)");
    let seed_log = fs::File::create(dir.join("seed_loop.log")).unwrap();
    let seed_child = Command::new(&this_exe)
        .arg("seed-loop")
        .env("SEED_TRADER_KEY", demo_trader_key)
        .stdout(Stdio::from(seed_log.try_clone().unwrap()))
        .stderr(Stdio::from(seed_log))
        .process_group(0)
        .spawn()
        .unwrap_or_else(|e| die(format!("failed to spawn seed-loop: {e}")));
    fs::write(dir.join("seed_loop.pid"), seed_child.id().to_string()).unwrap();

    log(format!("done. logs in {}/*.log, pids in {}/*.pid", dir.display(), dir.display()));
    log(format!("matcher:   {MATCHER_URL}"));
    log(format!("anvil rpc: {ANVIL_RPC}"));
    log("tear down with: cargo run --release --bin local_dev -- down");
}

// ---------------------------------------------------------------------------
// oracle-push-loop
// ---------------------------------------------------------------------------

/// Continuously pushes the REAL live Hermes price for each of the 5
/// PerpMarket-based markets into both MockPyth and that market's own
/// Chainlink MockV3Aggregator — see the spawn site's own doc (cmd_up,
/// step 8) for why this is what makes real, non-fabricated funding
/// emerge for those markets, and why it also fixes the StalePrice
/// reverts seen throughout local dev (nothing else ever refreshes
/// either oracle leg after their one-time deploy-time seed).
async fn cmd_oracle_push_loop() {
    let deployer_key = std::env::var("ORACLE_PUSH_DEPLOYER_KEY").expect("ORACLE_PUSH_DEPLOYER_KEY not set");
    let mock_pyth: Address = std::env::var("ORACLE_PUSH_MOCK_PYTH")
        .expect("ORACLE_PUSH_MOCK_PYTH not set")
        .parse()
        .expect("invalid mock pyth address");
    let targets_raw = std::env::var("ORACLE_PUSH_TARGETS").expect("ORACLE_PUSH_TARGETS not set");
    // (market_id, feed_id, aggregator_address) — feed_id looked up from
    // this binary's own FEED_IDS table by matching market_id against
    // MARKETS, same real Hermes feed ids used everywhere else here.
    let targets: Vec<(String, String, Address)> = targets_raw
        .split(',')
        .filter_map(|pair| {
            let (market_id, aggregator) = pair.split_once('=')?;
            let idx = MARKETS.iter().position(|&m| m == market_id)?;
            Some((market_id.to_string(), FEED_IDS[idx].to_string(), aggregator.parse().ok()?))
        })
        .collect();
    println!("==> oracle_push_loop starting, {} markets, interval 20s", targets.len());

    // Pyth updates every cycle; Chainlink only every 2nd (40s vs Pyth's
    // 20s) — matching how these two real oracle types actually differ
    // in update cadence (Pyth is pull-based and near-instant,
    // Chainlink's push-based aggregators update far less often in
    // practice). This isn't cosmetic: PerpMarket.sol's own markPrice()
    // is median(pythPrice, chainlinkPrice, twapPrice) — pushing the
    // SAME price to both legs every cycle makes chainlinkPrice ==
    // pythPrice exactly, which mathematically forces the median to
    // always equal pythPrice (two matching values out of three always
    // wins a median), so pMark==pIndex forever and funding can never
    // move no matter how long this runs. Letting Chainlink genuinely
    // lag is what lets real divergence emerge from real price movement.
    // Capped at every-2nd (not wider): `ChainlinkConsumer.sol`'s own
    // `MAX_STALENESS_SECONDS = 60` means a lag of 60s+ (e.g. the
    // originally-tried every-5th = 100s) makes markPrice() revert
    // `StalePrice` on literally every read, blocking funding entirely
    // instead of creating gentle divergence — confirmed live, this was
    // the actual reason BTC/XAU/HYPE funding stayed at 0 after the
    // first version of this loop.
    let mut cycle: u64 = 0;
    loop {
        let update_chainlink = cycle % 2 == 0;

        // Phase 1: fetch every market's live price CONCURRENTLY —
        // Hermes network I/O is the actual slow, sometimes-flaky part
        // (oracle.rs's own fetch_hermes_body doc on the retry budget
        // that can cost up to ~32s worst case), and this used to run it
        // sequentially, one market at a time: a single market stuck in
        // its full retry budget delayed every market queued behind it in
        // the SAME 20s cycle, even though their own fetches would have
        // succeeded immediately. `JoinSet` runs all 5 at once instead —
        // one slow fetch no longer blocks the others.
        let mut fetch_set = tokio::task::JoinSet::new();
        for (market_id, feed_id, _) in &targets {
            let market_id = market_id.clone();
            let feed_id = feed_id.clone();
            fetch_set.spawn(async move {
                let result = fetch_live_price(&feed_id).await;
                (market_id, result)
            });
        }
        let mut prices: std::collections::HashMap<String, Result<f64, String>> =
            std::collections::HashMap::new();
        while let Some(joined) = fetch_set.join_next().await {
            if let Ok((market_id, result)) = joined {
                prices.insert(market_id, result);
            }
        }

        // Phase 2: submit on-chain updates SEQUENTIALLY, not concurrently
        // — every market shares the SAME deployer signer, and concurrent
        // submission from one EOA races on nonces (the exact hazard
        // settle.rs's own module doc already names for settlement
        // broadcasts; same root cause, same fix, here). Purely local
        // anvil calls at this point, no network flakiness left to absorb,
        // so serializing this phase costs almost nothing.
        for (market_id, _feed_id, aggregator) in &targets {
            let Some(price_result) = prices.remove(market_id) else { continue };
            let real_price = match price_result {
                Ok(p) => p,
                Err(e) => {
                    println!("{} {market_id} push failed: {e}", chrono_like_time());
                    continue;
                }
            };
            match push_one_oracle_price(
                &deployer_key,
                mock_pyth,
                *aggregator,
                market_id,
                real_price,
                update_chainlink,
            )
            .await
            {
                Ok(price) => println!(
                    "{} {market_id} pushed real price {price} (chainlink: {update_chainlink})",
                    chrono_like_time()
                ),
                Err(e) => println!("{} {market_id} push failed: {e}", chrono_like_time()),
            }
        }
        cycle += 1;
        tokio::time::sleep(Duration::from_secs(20)).await;
    }
}

/// The fetch half of what `push_one_oracle_price` used to do inline —
/// split out so `cmd_oracle_push_loop` can run this concurrently across
/// every market while still submitting the resulting on-chain updates
/// sequentially (see that loop's own doc on why those two phases have
/// opposite concurrency needs).
async fn fetch_live_price(feed_id: &str) -> Result<f64, String> {
    let live = cerdic_tee_matcher::oracle::fetch_price(feed_id).await.map_err(|e| e.to_string())?;
    let real_price = live.price as f64 * 10f64.powi(live.expo);
    if real_price <= 0.0 {
        return Err("Hermes returned a non-positive price (feed has no live quote right now)".to_string());
    }
    Ok(real_price)
}

/// Builds its own short-lived provider per call (matching this file's
/// existing `fund_and_authorize`/`set_rate_differential_onchain`
/// pattern) rather than threading one generic provider type through —
/// negligible overhead against a local Anvil, and avoids alloy's
/// `Provider<Transport>` generic parameter leaking into every caller.
async fn push_one_oracle_price(
    deployer_key: &str,
    mock_pyth: Address,
    aggregator: Address,
    market_id: &str,
    real_price: f64,
    update_chainlink: bool,
) -> Result<f64, String> {
    let signer: PrivateKeySigner = deployer_key.parse().map_err(|e| format!("{e}"))?;
    let wallet = EthereumWallet::from(signer);
    let provider =
        ProviderBuilder::new().with_recommended_fillers().wallet(wallet).on_http(ANVIL_RPC.parse().unwrap());

    // Re-scale to a fixed 8-decimal fixed point, same convention every
    // deploy script here already seeds prices with, regardless of
    // Hermes's own (varying) expo for this feed.
    const EXPO: i32 = -8;
    let scaled = (real_price * 1e8).round() as i64;
    let conf = (scaled / 1000).max(1) as u64;
    let publish_time = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();

    let id = alloy::primitives::keccak256(market_id.as_bytes());
    let create_call = IMockPyth::createPriceFeedUpdateDataCall {
        id,
        price: scaled,
        conf,
        expo: EXPO,
        emaPrice: scaled,
        emaConf: conf,
        publishTime: publish_time,
    };
    let create_calldata = alloy::sol_types::SolCall::abi_encode(&create_call);
    let create_tx = TransactionRequest::default().with_to(mock_pyth).with_input(create_calldata);
    let raw =
        provider.call(&create_tx).await.map_err(|e| format!("createPriceFeedUpdateData call failed: {e}"))?;
    let update_data = IMockPyth::createPriceFeedUpdateDataCall::abi_decode_returns(&raw, true)
        .map_err(|e| format!("bad createPriceFeedUpdateData response: {e}"))?
        ._0;

    let update_data_bytes = vec![update_data];
    let fee_call = IMockPyth::getUpdateFeeCall { updateData: update_data_bytes.clone() };
    let fee_calldata = alloy::sol_types::SolCall::abi_encode(&fee_call);
    let fee_tx = TransactionRequest::default().with_to(mock_pyth).with_input(fee_calldata);
    let fee_raw = provider.call(&fee_tx).await.map_err(|e| format!("getUpdateFee call failed: {e}"))?;
    let fee = IMockPyth::getUpdateFeeCall::abi_decode_returns(&fee_raw, true)
        .map_err(|e| format!("bad getUpdateFee response: {e}"))?
        .feeAmount;

    let update_call = IMockPyth::updatePriceFeedsCall { updateData: update_data_bytes };
    let update_calldata = alloy::sol_types::SolCall::abi_encode(&update_call);
    let update_tx =
        TransactionRequest::default().with_to(mock_pyth).with_input(update_calldata).with_value(fee);
    provider
        .send_transaction(update_tx)
        .await
        .map_err(|e| e.to_string())?
        .get_receipt()
        .await
        .map_err(|e| e.to_string())?;

    if update_chainlink {
        let answer_call = IMockV3Aggregator::updateAnswerCall {
            answer: alloy::primitives::I256::try_from(scaled).unwrap(),
        };
        let answer_calldata = alloy::sol_types::SolCall::abi_encode(&answer_call);
        let answer_tx = TransactionRequest::default().with_to(aggregator).with_input(answer_calldata);
        provider
            .send_transaction(answer_tx)
            .await
            .map_err(|e| e.to_string())?
            .get_receipt()
            .await
            .map_err(|e| e.to_string())?;
    }

    Ok(real_price)
}

// ---------------------------------------------------------------------------
// seed-loop
// ---------------------------------------------------------------------------

/// Keeps real trade flow landing on every market with live depth,
/// forever (until killed). Two market_maker instances alone never trade
/// with each other (both quote symmetrically around the same real mid,
/// bid < mid < ask, so neither ever crosses the other) — without an
/// ongoing taker, TradeTape/candles/last-price flatline right after the
/// one-shot seed batch in `cmd_up` and never move again. Every trade
/// here is a real signed+encrypted order via the demo_client binary
/// against the real matcher, not a fabricated print.
async fn cmd_seed_loop() {
    let root = repo_root();
    let bin_dir = root.join("crates/target/release");
    let trader_key = std::env::var("SEED_TRADER_KEY").unwrap_or_else(|_| random_private_key());
    println!("==> seed_trades_loop starting, trader key set, cycling every market each round");

    let client = reqwest::Client::new();
    let mut rng = rand::thread_rng();
    let mut mid_refs: std::collections::HashMap<&str, f64> = std::collections::HashMap::new();
    loop {
        // One pass through EVERY market per round (shuffled, not fixed
        // order), not one random market per interval — picking randomly
        // meant whichever single market someone happened to be watching
        // could sit untouched for minutes even while everything else was
        // actively trading, which read as "broken" even though the
        // system was working. Cycling guarantees every market gets a
        // real trade roughly once per round.
        let mut order: Vec<usize> = (0..MARKETS.len()).collect();
        for i in (1..order.len()).rev() {
            order.swap(i, rng.gen_range(0..=i));
        }

        for idx in order {
            let market = MARKETS[idx];
            fire_priced_trade(&client, &bin_dir, market, &trader_key, &mut rng, &mut mid_refs).await;

            // Spread each market's trade out within the round rather than
            // firing all 9 back to back — still real, varied cadence, not
            // a burst that immediately goes quiet again.
            let gap = rng.gen_range(4..=9);
            tokio::time::sleep(Duration::from_secs(gap)).await;
        }
    }
}

/// Decides a side, crosses at the market's REAL current best bid/ask,
/// and fires one real trade via demo_client — no hardcoded buy/sell
/// script anywhere in this file, every trade this binary generates goes
/// through here. `mid_refs` is a slow-moving reference mid per market
/// (an exponential moving average, kept in the caller so it persists
/// across calls): `pick_side` biases toward whichever side would push
/// price back toward that reference once the live mid has drifted away
/// from it — a simple, real mean-reversion model, not a coin flip
/// pretending to be one. Returns `None` when the market has no live
/// depth to cross yet (e.g. KR200/BRENT outside real market hours),
/// `Some(filled)` otherwise.
async fn fire_priced_trade(
    client: &reqwest::Client,
    bin_dir: &Path,
    market: &'static str,
    trader_key: &str,
    rng: &mut impl Rng,
    mid_refs: &mut std::collections::HashMap<&'static str, f64>,
) -> Option<bool> {
    let book = fetch_orderbook(client, market).await?;
    let (bid, ask) = (book.best_bid?, book.best_ask?);
    let mid = (bid as f64 + ask as f64) / 2.0;

    let side = pick_side(market, mid, rng, mid_refs);
    let cross_tick = crossing_tick(&book, side)?;
    // Sized to actually sweep through the market makers' own 55-80 unit
    // quotes rather than nibbling a single tick — a trade this small
    // next to that depth reads as a rounding error, not real flow.
    let qty = rng.gen_range(20..=110);

    // IOC: this order only ever means to take whatever real depth sits
    // at/inside the crossing price (see crossing_tick's own doc — a
    // couple ticks past the touch, sized just to comfortably cross tick
    // granularity, not to guarantee the full requested qty fills). A
    // GTC order that only partially fills used to rest its remainder
    // at that off-market crossing price instead of being dropped — a
    // stale, artificially-priced resting order that a later opposing
    // seed trade could go on to cross, printing a trade far from the
    // real oracle mid. This is exactly what made candles "later on"
    // (once enough of these had accumulated) print wildly off from the
    // live mark price.
    let filled = run_demo_client(bin_dir, side, cross_tick, qty, market, trader_key, 5, "ioc").await;
    let now = chrono_like_time();
    if filled {
        println!("{now} {market} {side} {qty} @ {cross_tick} -> filled");
    } else {
        println!("{now} {market} {side} {qty} @ {cross_tick} -> did not fill (book moved), skipping");
    }
    Some(filled)
}

/// Mean-reversion bias: once mid has drifted more than 0.15% from the
/// slow reference, lean 70/30 toward the side that would push it back;
/// inside that band, no real signal either way, so it's an even
/// coin flip. The reference itself is a slow EMA (10% weight per
/// observation) of the market's own live mid — never a fabricated
/// price, always derived from what the book actually just reported.
fn pick_side(
    market: &'static str,
    mid: f64,
    rng: &mut impl Rng,
    mid_refs: &mut std::collections::HashMap<&'static str, f64>,
) -> &'static str {
    let reference = *mid_refs.entry(market).or_insert(mid);
    let drift = (mid - reference) / reference;
    let sell_probability = if drift > 0.0015 {
        0.7 // price ran up relative to reference -> lean toward selling into it
    } else if drift < -0.0015 {
        0.3 // price dropped relative to reference -> lean toward buying it back
    } else {
        0.5
    };
    mid_refs.insert(market, reference * 0.9 + mid * 0.1);
    if rng.gen_bool(sell_probability) {
        "sell"
    } else {
        "buy"
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// Parses anvil's stdout log for the "Private Keys" section it prints on
/// boot, in account order (0..N). Reading these out of anvil's own
/// output instead of hardcoding anvil's well-known deterministic dev
/// keys removes any chance of a transcription error — the exact failure
/// mode that broke this setup by hand twice before this binary existed.
fn parse_anvil_keys(log_path: &Path) -> Vec<String> {
    let content = fs::read_to_string(log_path).unwrap_or_default();
    let mut keys = Vec::new();
    let mut in_section = false;
    for line in content.lines() {
        if line.starts_with("Private Keys") {
            in_section = true;
            continue;
        }
        if in_section {
            if line.starts_with("Wallet") {
                break;
            }
            if let Some(rest) = line.trim().strip_prefix('(') {
                if let Some((_, key)) = rest.split_once(')') {
                    keys.push(key.trim().to_string());
                }
            }
        }
    }
    keys
}

/// Finds the first line containing `label` and returns its last
/// whitespace-separated token — matches how every deploy script here
/// logs an address (`console.log("Label:", address)` or
/// `console.log("Label", address)`, Foundry always renders both as
/// `label<...>0xADDRESS` on one line).
fn get_addr(output: &str, label: &str) -> Option<String> {
    output.lines().find(|l| l.contains(label))?.split_whitespace().last().map(|s| s.to_string())
}

/// Same as `get_addr`, but every matching line, in file order — for
/// scripts that log the same label once per market deployed (e.g.
/// DeployMoreMarketsLocal's "  aggregator:" line, once per market).
fn get_addrs_all(output: &str, label: &str) -> Vec<String> {
    output
        .lines()
        .filter(|l| l.contains(label))
        .filter_map(|l| l.split_whitespace().last())
        .map(|s| s.to_string())
        .collect()
}

fn run_forge_script(dir: &Path, target: &str, env: &[(&str, &str)]) -> std::io::Result<String> {
    let mut cmd = Command::new("forge");
    cmd.current_dir(dir).args(["script", target, "--rpc-url", ANVIL_RPC, "--broadcast"]);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let output = cmd.output()?;
    let combined =
        format!("{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    if !output.status.success() {
        return Err(std::io::Error::other(format!("forge script exited non-zero:\n{combined}")));
    }
    Ok(combined)
}

async fn wait_http_ok(url: &str, timeout_secs: u64) -> Option<()> {
    let client = reqwest::Client::new();
    for _ in 0..timeout_secs {
        if client.get(url).send().await.is_ok() {
            return Some(());
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    None
}

#[derive(Deserialize)]
struct PubkeyResponse {
    settlement_address: String,
}

async fn wait_pubkey(matcher_url: &str, timeout_secs: u64) -> Option<PubkeyResponse> {
    let client = reqwest::Client::new();
    let url = format!("{matcher_url}/pubkey");
    for _ in 0..timeout_secs {
        if let Ok(resp) = client.get(&url).send().await {
            if let Ok(parsed) = resp.json::<PubkeyResponse>().await {
                return Some(parsed);
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    None
}

fn hex_decode_key(key: &str) -> Vec<u8> {
    hex::decode(key.trim_start_matches("0x")).expect("valid hex private key")
}

/// Sends the ephemeral matcher signer 10 ETH of gas money, then calls
/// `AttestationRouter.authorizeTEE` so its settlement broadcasts stop
/// reverting with NotAuthorizedTEE. Both steps are real on-chain
/// transactions via alloy directly (not a `cast send` shell-out) —
/// idiomatic for this crate, which already builds providers/wallets this
/// same way in every keeper bin.
async fn fund_and_authorize(
    deployer_key: &str,
    signer_address: &str,
    attestation_router: &str,
) -> Result<(), String> {
    let signer: PrivateKeySigner = deployer_key.parse().map_err(|e| format!("{e}"))?;
    let wallet = EthereumWallet::from(signer);
    let provider =
        ProviderBuilder::new().with_recommended_fillers().wallet(wallet).on_http(ANVIL_RPC.parse().unwrap());

    let to: Address = signer_address.parse().map_err(|e| format!("{e}"))?;
    let fund_tx =
        TransactionRequest::default().with_to(to).with_value(U256::from(10_000_000_000_000_000_000u128));
    provider
        .send_transaction(fund_tx)
        .await
        .map_err(|e| e.to_string())?
        .get_receipt()
        .await
        .map_err(|e| e.to_string())?;

    let router: Address = attestation_router.parse().map_err(|e| format!("{e}"))?;
    let call = IAttestationRouter::authorizeTEECall { tee: to };
    let calldata = alloy::sol_types::SolCall::abi_encode(&call);
    let auth_tx = TransactionRequest::default().with_to(router).with_input(calldata);
    provider
        .send_transaction(auth_tx)
        .await
        .map_err(|e| e.to_string())?
        .get_receipt()
        .await
        .map_err(|e| e.to_string())?;

    Ok(())
}

/// Sends `LOCAL_DEV_FUND_ADDRESS` 10 ETH plus 100,000 real minted mock
/// USDC — see `LOCAL_DEV_FUND_ADDRESS`'s own doc at its call site on why
/// this exists (repeat manual `cast` calls after every redeploy).
async fn fund_test_wallet(deployer_key: &str, fund_address: &str, usdc_token: &str) -> Result<(), String> {
    let signer: PrivateKeySigner = deployer_key.parse().map_err(|e| format!("{e}"))?;
    let wallet = EthereumWallet::from(signer);
    let provider =
        ProviderBuilder::new().with_recommended_fillers().wallet(wallet).on_http(ANVIL_RPC.parse().unwrap());

    let to: Address = fund_address.parse().map_err(|e| format!("{e}"))?;
    let fund_tx =
        TransactionRequest::default().with_to(to).with_value(U256::from(10_000_000_000_000_000_000u128));
    provider
        .send_transaction(fund_tx)
        .await
        .map_err(|e| e.to_string())?
        .get_receipt()
        .await
        .map_err(|e| e.to_string())?;

    let usdc: Address = usdc_token.parse().map_err(|e| format!("{e}"))?;
    // 100,000 USDC at 18 decimals (LocalStablecoin's own ERC20 default).
    let call = ILocalStablecoin::mintCall { to, amount: U256::from(100_000_000_000_000_000_000_000u128) };
    let calldata = alloy::sol_types::SolCall::abi_encode(&call);
    let mint_tx = TransactionRequest::default().with_to(usdc).with_input(calldata);
    provider
        .send_transaction(mint_tx)
        .await
        .map_err(|e| e.to_string())?
        .get_receipt()
        .await
        .map_err(|e| e.to_string())?;

    Ok(())
}

/// Approves and deposits `amount` of `usdc_token` into `Account.sol`,
/// signed by `trader_key` itself — not the deployer. Minting real USDC
/// into a wallet (`fund_test_wallet`) is NOT the same as depositing it:
/// `Account.collateralBalanceOf` (what `check_deposited_collateral`
/// actually reads) stays zero until someone calls `approve` then
/// `deposit` FROM that wallet. Confirmed live: the demo trader had a
/// real 100,000 USDC wallet balance and every one of its own seed trades
/// still got rejected with real $0 collateral, because minting alone
/// never did this second step. For a real human's wallet
/// (`LOCAL_DEV_FUND_ADDRESS`), this binary deliberately does NOT do this
/// — it doesn't have that private key, and shouldn't: the real
/// approve/deposit there has to go through their own signature, which is
/// exactly what `DepositModal.tsx` now does. This function is only for
/// wallets this binary already controls the key for (anvil's own dev
/// accounts), like the demo/seed trader.
async fn deposit_as_trader(trader_key: &str, usdc_token: &str, account_contract: &str) -> Result<(), String> {
    let signer: PrivateKeySigner = trader_key.parse().map_err(|e| format!("{e}"))?;
    let wallet = EthereumWallet::from(signer);
    let provider =
        ProviderBuilder::new().with_recommended_fillers().wallet(wallet).on_http(ANVIL_RPC.parse().unwrap());

    let usdc: Address = usdc_token.parse().map_err(|e| format!("{e}"))?;
    let account: Address = account_contract.parse().map_err(|e| format!("{e}"))?;
    let amount = U256::from(100_000_000_000_000_000_000_000u128); // 100,000 USDC, 18 decimals

    let approve_call = IErc20Approve::approveCall { spender: account, amount };
    let approve_calldata = alloy::sol_types::SolCall::abi_encode(&approve_call);
    let approve_tx = TransactionRequest::default().with_to(usdc).with_input(approve_calldata);
    provider
        .send_transaction(approve_tx)
        .await
        .map_err(|e| e.to_string())?
        .get_receipt()
        .await
        .map_err(|e| e.to_string())?;

    let deposit_call = IAccountContract::depositCall { asset: usdc, amount };
    let deposit_calldata = alloy::sol_types::SolCall::abi_encode(&deposit_call);
    let deposit_tx = TransactionRequest::default().with_to(account).with_input(deposit_calldata);
    provider
        .send_transaction(deposit_tx)
        .await
        .map_err(|e| e.to_string())?
        .get_receipt()
        .await
        .map_err(|e| e.to_string())?;

    Ok(())
}

#[derive(Deserialize)]
struct OrderBookResponse {
    best_bid: Option<u64>,
    best_ask: Option<u64>,
}

async fn fetch_orderbook(client: &reqwest::Client, market: &str) -> Option<OrderBookResponse> {
    let encoded = urlencoding_encode(market);
    let url = format!("{MATCHER_URL}/orderbook/{encoded}");
    client.get(&url).send().await.ok()?.json::<OrderBookResponse>().await.ok()
}

/// A couple ticks past best_ask (buy) / best_bid (sell) to comfortably
/// cross regardless of the market's own tick granularity. `None` when
/// there's no live depth to cross yet.
fn crossing_tick(book: &OrderBookResponse, side: &str) -> Option<u64> {
    if side == "buy" {
        let ask = book.best_ask?;
        Some(ask + (ask / 2000 + 2))
    } else {
        let bid = book.best_bid?;
        Some(bid.saturating_sub(bid / 2000 + 2))
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_demo_client(
    bin_dir: &Path,
    side: &str,
    tick: u64,
    qty: u64,
    market: &str,
    key: &str,
    leverage: u64,
    tif: &str,
) -> bool {
    let output = tokio::process::Command::new(bin_dir.join("demo_client"))
        .args([side, &tick.to_string(), &qty.to_string(), market, key, &leverage.to_string(), tif])
        .output()
        .await;
    match output {
        Ok(o) => {
            let combined =
                format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
            combined.contains("\"status\":\"filled\"")
        }
        Err(_) => false,
    }
}

fn urlencoding_encode(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' || c == '~' {
                c.to_string()
            } else {
                format!("%{:02X}", c as u32)
            }
        })
        .collect()
}

fn random_private_key() -> String {
    // Any 0x-prefixed 32-byte hex key works as a trader identity for
    // demo_client — it only signs with it, never needs funding (the
    // matcher's own settlement_signer pays gas, not the trader).
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill(&mut bytes);
    format!("0x{}", hex::encode(bytes))
}

fn chrono_like_time() -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let secs_today = now % 86400;
    format!("{:02}:{:02}:{:02}", secs_today / 3600, (secs_today % 3600) / 60, secs_today % 60)
}
