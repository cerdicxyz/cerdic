"""Simulation of Cerdic's proposed synthetic backstop maker: a kernel-owned
resting quote in the shared CLOB, priced off the mark oracle, meant to
guarantee a fillable two-sided quote on thin FX pairs before real market
makers show up.

This validates the three guards discussed against the design's central
risk (the same vulnerability class GMX's V1 exploit actually hit: a resting
quote priced off an instantaneous oracle print, with no smoothing and no
size cap, is free money for anyone who can move that print for one tick):

    1. TWAP pricing   - quote off a trailing average, not the latest print,
                         so a one-tick manipulation barely moves the quote.
    2. Notional cap   - bound the worst-case size any single fill can hit
                         the backstop maker for.
    3. Last-look      - re-validate the oracle print against the TWAP at
                         the moment of match, before confirming the fill;
                         reject if it has moved past a threshold. Cheap and
                         mechanical to do here specifically because Cerdic
                         already settles everything inside a TEE.

Four configurations are run so each guard's individual contribution is
visible, not just the end state:

    naive           - no guards (the GMX-shaped vulnerability, as a baseline)
    twap_only       - guard 1 only
    twap_and_cap    - guards 1+2
    fully_guarded   - guards 1+2+3 (the proposed design)

Two trader populations exist:

    noise traders   - unbiased small random flow, representing normal usage.
                       This is what proves the guards don't kill the
                       maker's ability to earn its ordinary spread.
    the attacker    - only trades DURING a manipulation event, sized to the
                       maximum the current config allows, always on the
                       side the manipulation makes profitable. This is what
                       proves (or disproves) each guard's protection.

Run directly: `python3 backstop_maker_sim.py`
"""

from __future__ import annotations

import dataclasses
from dataclasses import dataclass, field

import numpy as np

# ---------------------------------------------------------------------------
# Oracle price process.
# ---------------------------------------------------------------------------


@dataclass
class OracleConfig:
    n_ticks: int = 20_000
    start_price: float = 1.085  # EURC/USDC-shaped
    vol_per_tick: float = 0.0006  # true-price GBM volatility per tick
    oracle_noise: float = 0.00005  # normal oracle reporting noise (non-manipulated ticks)
    manipulation_prob: float = 0.003  # probability any given tick is a manipulation event
    manipulation_pct_range: tuple[float, float] = (0.006, 0.03)  # 0.6%-3% one-tick spike


def simulate_oracle(cfg: OracleConfig, rng: np.random.Generator):
    """Returns (true_price, oracle_price, is_manipulated) arrays of length n_ticks."""
    true_price = np.empty(cfg.n_ticks)
    oracle_price = np.empty(cfg.n_ticks)
    is_manipulated = np.zeros(cfg.n_ticks, dtype=bool)

    true_price[0] = cfg.start_price
    for t in range(1, cfg.n_ticks):
        shock = rng.normal(0, cfg.vol_per_tick)
        true_price[t] = true_price[t - 1] * (1 + shock)

    manipulated_ticks = rng.random(cfg.n_ticks) < cfg.manipulation_prob
    manipulated_ticks[0] = False  # no history to manipulate against on tick 0

    for t in range(cfg.n_ticks):
        base = true_price[t] * (1 + rng.normal(0, cfg.oracle_noise))
        if manipulated_ticks[t]:
            direction = rng.choice([-1, 1])
            magnitude = rng.uniform(*cfg.manipulation_pct_range)
            oracle_price[t] = base * (1 + direction * magnitude)
            is_manipulated[t] = True
        else:
            oracle_price[t] = base

    return true_price, oracle_price, is_manipulated


# ---------------------------------------------------------------------------
# The synthetic backstop maker.
# ---------------------------------------------------------------------------


@dataclass
class MakerConfig:
    name: str
    spread_bps: float = 8.0  # crates/risk liquidity_charge-shaped half-spread, in bps
    use_twap: bool = False
    twap_window: int = 20
    use_notional_cap: bool = False
    notional_cap: float = 5_000.0
    use_last_look: bool = False
    last_look_threshold_bps: float = 15.0  # reject if |oracle - reference| exceeds this
    # Deliberately SHORTER than twap_window: last-look must compare the live
    # print against a FAST reference, not the same slow window used to
    # center the quote. Checking against the quoting TWAP itself conflates
    # ordinary multi-tick trend drift with a genuine one-tick manipulation
    # spike (found by running this simulation the first time -- it rejected
    # ~32% of legitimate noise flow, not just the attacker). A short
    # reference tracks true price closely enough that only an actual
    # single-tick spike trips it.
    last_look_window: int = 3


NAIVE = MakerConfig(name="naive", use_twap=False, use_notional_cap=False, use_last_look=False)
TWAP_ONLY = MakerConfig(name="twap_only", use_twap=True, use_notional_cap=False, use_last_look=False)
TWAP_AND_CAP = MakerConfig(name="twap_and_cap", use_twap=True, use_notional_cap=True, use_last_look=False)
FULLY_GUARDED = MakerConfig(name="fully_guarded", use_twap=True, use_notional_cap=True, use_last_look=True)

ALL_CONFIGS = [NAIVE, TWAP_ONLY, TWAP_AND_CAP, FULLY_GUARDED]


def fair_value(cfg: MakerConfig, oracle_price: np.ndarray, t: int) -> float:
    """The price the maker's quote is centered on at tick t."""
    if cfg.use_twap:
        window = oracle_price[max(0, t - cfg.twap_window + 1) : t + 1]
        return float(window.mean())
    return float(oracle_price[t])


def attempt_fill(
    cfg: MakerConfig,
    oracle_price: np.ndarray,
    t: int,
    side: str,  # "buy_from_maker" (attacker/noise sells to maker's bid) or "sell_to_maker" (buys maker's ask)
    requested_size: float,
) -> tuple[float, float] | None:
    """Returns (fill_price, fill_size) if the trade is accepted, else None."""
    center = fair_value(cfg, oracle_price, t)
    half_spread = center * cfg.spread_bps / 10_000
    quote_price = center - half_spread if side == "buy_from_maker" else center + half_spread

    size = requested_size
    if cfg.use_notional_cap:
        size = min(size, cfg.notional_cap)

    if cfg.use_last_look:
        window = oracle_price[max(0, t - cfg.last_look_window + 1) : t + 1]
        reference = float(window.mean())
        deviation_bps = abs(oracle_price[t] - reference) / reference * 10_000
        if deviation_bps > cfg.last_look_threshold_bps:
            return None  # rejected: the live print just moved sharply vs. its own recent trend

    return quote_price, size


# ---------------------------------------------------------------------------
# Trader populations.
# ---------------------------------------------------------------------------


@dataclass
class TraderConfig:
    noise_trade_prob: float = 0.02  # probability of a noise trade on a non-manipulated tick
    noise_size_range: tuple[float, float] = (200.0, 2_000.0)
    attacker_size: float = 50_000.0  # what the attacker WANTS to trade (before any cap)


# ---------------------------------------------------------------------------
# One simulation run for one maker configuration.
# ---------------------------------------------------------------------------


@dataclass
class RunResult:
    maker_name: str
    cumulative_pnl: float
    worst_single_trade_pnl: float
    attacker_attempts: int
    attacker_fills: int
    attacker_rejected: int
    attacker_pnl_extracted: float
    noise_fills: int
    noise_attempts: int
    noise_pnl_earned: float


def run_once(
    maker_cfg: MakerConfig,
    oracle_cfg: OracleConfig,
    trader_cfg: TraderConfig,
    seed: int,
) -> RunResult:
    rng = np.random.default_rng(seed)
    true_price, oracle_price, is_manipulated = simulate_oracle(oracle_cfg, rng)

    cumulative_pnl = 0.0
    worst_trade_pnl = 0.0
    attacker_attempts = attacker_fills = attacker_rejected = 0
    attacker_pnl_extracted = 0.0
    noise_fills = 0
    noise_attempts = 0
    noise_pnl_earned = 0.0

    for t in range(oracle_cfg.n_ticks - 1):
        if is_manipulated[t]:
            # The manipulation direction determines which side is profitable
            # to attack: an upward oracle spike makes selling INTO the
            # maker's (inflated) bid profitable, a downward spike makes
            # buying the (deflated) ask profitable.
            direction_up = oracle_price[t] > true_price[t]
            side = "buy_from_maker" if direction_up else "sell_to_maker"
            attacker_attempts += 1
            result = attempt_fill(maker_cfg, oracle_price, t, side, trader_cfg.attacker_size)
            if result is None:
                attacker_rejected += 1
                continue
            fill_price, fill_size = result
            attacker_fills += 1
            true_next = true_price[t + 1]
            # Maker took the OTHER side of the attacker's trade.
            if side == "buy_from_maker":  # maker bought (bid hit) at fill_price
                trade_pnl = (true_next - fill_price) * fill_size
            else:  # maker sold (ask lifted) at fill_price
                trade_pnl = (fill_price - true_next) * fill_size
            cumulative_pnl += trade_pnl
            attacker_pnl_extracted += -trade_pnl
            worst_trade_pnl = min(worst_trade_pnl, trade_pnl)
        elif rng.random() < trader_cfg.noise_trade_prob:
            side = rng.choice(["buy_from_maker", "sell_to_maker"])
            size = rng.uniform(*trader_cfg.noise_size_range)
            noise_attempts += 1
            result = attempt_fill(maker_cfg, oracle_price, t, side, size)
            if result is None:
                continue
            fill_price, fill_size = result
            noise_fills += 1
            true_next = true_price[t + 1]
            if side == "buy_from_maker":
                trade_pnl = (true_next - fill_price) * fill_size
            else:
                trade_pnl = (fill_price - true_next) * fill_size
            cumulative_pnl += trade_pnl
            noise_pnl_earned += trade_pnl
            worst_trade_pnl = min(worst_trade_pnl, trade_pnl)

    return RunResult(
        maker_name=maker_cfg.name,
        cumulative_pnl=cumulative_pnl,
        worst_single_trade_pnl=worst_trade_pnl,
        attacker_attempts=attacker_attempts,
        attacker_fills=attacker_fills,
        attacker_rejected=attacker_rejected,
        attacker_pnl_extracted=attacker_pnl_extracted,
        noise_fills=noise_fills,
        noise_attempts=noise_attempts,
        noise_pnl_earned=noise_pnl_earned,
    )


# ---------------------------------------------------------------------------
# Experiment: many trials per configuration.
# ---------------------------------------------------------------------------


def run_experiment(n_trials: int = 300, base_seed: int = 0):
    oracle_cfg = OracleConfig()
    trader_cfg = TraderConfig()

    results: dict[str, list[RunResult]] = {cfg.name: [] for cfg in ALL_CONFIGS}
    for trial in range(n_trials):
        for cfg in ALL_CONFIGS:
            # Same seed across configs for a given trial: identical price
            # path and identical attacker/noise-trade timing, so the ONLY
            # thing that differs between configs is the maker's own guards.
            results[cfg.name].append(run_once(cfg, oracle_cfg, trader_cfg, seed=base_seed + trial))
    return results


def summarize(results: dict[str, list[RunResult]]):
    print(f"{'config':<14} {'mean PnL':>12} {'worst PnL':>12} {'p5 PnL':>12} "
          f"{'attack fill%':>13} {'noise served%':>14} {'noise PnL':>12}")
    print("-" * 100)
    for name, runs in results.items():
        pnl = np.array([r.cumulative_pnl for r in runs])
        worst = np.array([r.worst_single_trade_pnl for r in runs])
        attempts = sum(r.attacker_attempts for r in runs)
        fills = sum(r.attacker_fills for r in runs)
        noise_attempts = sum(r.noise_attempts for r in runs)
        noise_fills = sum(r.noise_fills for r in runs)
        noise_pnl = sum(r.noise_pnl_earned for r in runs)
        fill_pct = 100 * fills / attempts if attempts else 0.0
        noise_served_pct = 100 * noise_fills / noise_attempts if noise_attempts else 0.0
        print(
            f"{name:<14} {pnl.mean():>12.1f} {worst.min():>12.1f} {np.percentile(pnl, 5):>12.1f} "
            f"{fill_pct:>12.1f}% {noise_served_pct:>13.1f}% {noise_pnl:>12.1f}"
        )


def plot(results: dict[str, list[RunResult]], out_path: str):
    import matplotlib.pyplot as plt

    fig, ax = plt.subplots(figsize=(9, 5))
    data = [[r.cumulative_pnl for r in results[cfg.name]] for cfg in ALL_CONFIGS]
    labels = [cfg.name for cfg in ALL_CONFIGS]
    ax.boxplot(data, labels=labels, showmeans=True)
    ax.axhline(0, color="gray", linewidth=0.8, linestyle="--")
    ax.set_ylabel("Backstop maker cumulative PnL per run (USD)")
    ax.set_title("Synthetic backstop maker PnL by guard configuration\n(300 trials, identical price paths per trial)")
    fig.tight_layout()
    fig.savefig(out_path, dpi=150)
    print(f"\nSaved chart to {out_path}")


if __name__ == "__main__":
    results = run_experiment(n_trials=300)
    summarize(results)
    plot(results, out_path="backstop_maker_pnl.png")
