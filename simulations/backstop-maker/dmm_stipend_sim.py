"""Does paying a designated market maker (DMM) to show up on day one beat
waiting for one to arrive organically (liquidity_bootstrap_sim.py's
with_backstop scenario, which only reacts to volume it has already
generated)?

Real FX/CLOB venues don't leave this to chance: EBS and similar venues run
Designated Market Maker programs -- a provider commits to measurable
quoting obligations (min size, max spread, min uptime) on specific
instruments, often deliberately the THIN ones, in exchange for a stipend
paid whether or not they get filled. The academic literature on optimal
rebate sizing (e.g. arXiv 2501.12591) solves this as a high-dimensional
HJB equation via a neural PDE solver, not reproducible here -- so instead
this uses the more fundamental principle every one of those models rests
on: a break-even subsidy. Pay the DMM exactly enough to cover its net
losses over a period, nothing more. If the pair is naturally profitable to
quote, the stipend is zero.

Adds one thing the earlier sims didn't model: real, non-adversarial order
flow is not purely random. A fraction of real traders are mildly
INFORMED (their side correlates with the next tick's true price move),
the standard Glosten-Milgrom assumption and the actual reason a market
maker needs compensation at all beyond pure noise-flow spread capture.

Three scenarios:

    cold_start   - nobody quotes until a real MM organically arrives
                   (same passive arrival model as liquidity_bootstrap_sim).
    with_backstop - the kernel's own passive backstop maker rests from
                   tick 0 (wide spread, no obligation, no stipend).
    with_dmm     - a designated maker rests from tick 0 at a TIGHTER
                   spread (a real committed provider, not a last-resort
                   default), reimbursed for net losses each period via a
                   break-even stipend.

Run directly: `python3 dmm_stipend_sim.py`
"""

from __future__ import annotations

from dataclasses import dataclass, replace

import numpy as np

from backstop_maker_sim import FULLY_GUARDED, MakerConfig, OracleConfig, attempt_fill, simulate_oracle
from liquidity_bootstrap_sim import DemandConfig, MmArrivalConfig

# A real, committed designated maker: same guards (TWAP, cap, last-look) as
# the passive backstop, but a tighter spread -- it's actually trying to win
# flow, not just providing a last-resort floor.
DMM = replace(FULLY_GUARDED, name="dmm", spread_bps=5.0)

STIPEND_PERIOD_TICKS = 2_000


@dataclass
class InformedDemandConfig(DemandConfig):
    informed_fraction: float = 0.15  # share of real traders whose side correlates with the next tick's move


@dataclass
class ScenarioResult:
    scenario: str
    total_demand_attempts: int
    total_served: int
    total_served_notional: float
    maker_trading_pnl: float  # spread income minus adverse-selection losses, BEFORE any stipend
    total_stipend_paid: float
    effective_spread_bps: float
    organic_mm_arrival_tick: int | None


def _trade_side_and_pnl_sign(rng, informed_fraction: float, true_price: np.ndarray, t: int) -> str:
    """Returns the taker's side. With probability informed_fraction, the
    taker is mildly informed: it trades on the side that will be profitable
    given the NEXT tick's true price move (buys before an up-move, sells
    before a down-move) -- this is what makes serving real flow cost a
    maker something beyond pure noise, same as it does in reality."""
    if rng.random() < informed_fraction:
        moves_up = true_price[t + 1] > true_price[t]
        return "sell_to_maker" if moves_up else "buy_from_maker"
    return rng.choice(["buy_from_maker", "sell_to_maker"])


def run_scenario(
    scenario: str,
    oracle_cfg: OracleConfig,
    demand_cfg: InformedDemandConfig,
    mm_cfg: MmArrivalConfig,
    seed: int,
) -> ScenarioResult:
    rng = np.random.default_rng(seed)
    true_price, oracle_price, _ = simulate_oracle(oracle_cfg, rng)
    n = oracle_cfg.n_ticks

    use_backstop = scenario == "with_backstop"
    use_dmm = scenario == "with_dmm"
    resident_maker: MakerConfig | None = FULLY_GUARDED if use_backstop else (DMM if use_dmm else None)

    served_notional_history = np.zeros(n)
    organic_mm_active_from: int | None = None

    total_attempts = total_served = 0
    total_served_notional = 0.0
    spread_paid: list[float] = []

    period_pnl = 0.0
    total_stipend_paid = 0.0
    maker_trading_pnl = 0.0

    for t in range(n - 1):
        # Organic (unpaid) real-MM arrival, same reactive model as
        # liquidity_bootstrap_sim: it can ADDITIONALLY show up on top of
        # a resident backstop/DMM once it sees enough volume.
        if organic_mm_active_from is None:
            window_start = max(0, t - mm_cfg.trailing_window)
            trailing_volume = served_notional_history[window_start:t].sum()
            entry_prob = min(1.0, mm_cfg.base_prob_per_tick + mm_cfg.volume_sensitivity * (trailing_volume / 1_000))
            if rng.random() < entry_prob:
                organic_mm_active_from = t

        # Period-end stipend settlement: reimburse net losses only.
        if use_dmm and t > 0 and t % STIPEND_PERIOD_TICKS == 0:
            if period_pnl < 0:
                total_stipend_paid += -period_pnl
            period_pnl = 0.0

        if rng.random() >= demand_cfg.trader_arrival_prob:
            continue

        total_attempts += 1
        side = _trade_side_and_pnl_sign(rng, demand_cfg.informed_fraction, true_price, t)
        size = rng.uniform(*demand_cfg.size_range)
        center = oracle_price[t]

        resident_quote = None
        if resident_maker is not None:
            resident_quote = attempt_fill(resident_maker, oracle_price, t, side, size)

        organic_quote = None
        if organic_mm_active_from is not None:
            half_spread = center * mm_cfg.real_mm_spread_bps / 10_000
            organic_price = center - half_spread if side == "buy_from_maker" else center + half_spread
            organic_quote = (organic_price, size)

        chosen = None
        chosen_is_resident = False
        if resident_quote is not None and organic_quote is not None:
            better_resident = (
                resident_quote[0] < organic_quote[0] if side == "sell_to_maker" else resident_quote[0] > organic_quote[0]
            )
            chosen, chosen_is_resident = (resident_quote, True) if better_resident else (organic_quote, False)
        elif resident_quote is not None:
            chosen, chosen_is_resident = resident_quote, True
        elif organic_quote is not None:
            chosen, chosen_is_resident = organic_quote, False

        if chosen is None:
            continue

        fill_price, fill_size = chosen
        total_served += 1
        notional = fill_price * fill_size
        total_served_notional += notional
        served_notional_history[t] = notional
        spread_paid.append(abs(fill_price - center) / center * 10_000)

        if chosen_is_resident:
            true_next = true_price[t + 1]
            if side == "buy_from_maker":
                trade_pnl = (true_next - fill_price) * fill_size
            else:
                trade_pnl = (fill_price - true_next) * fill_size
            maker_trading_pnl += trade_pnl
            period_pnl += trade_pnl

    # Settle any partial final period.
    if use_dmm and period_pnl < 0:
        total_stipend_paid += -period_pnl

    return ScenarioResult(
        scenario=scenario,
        total_demand_attempts=total_attempts,
        total_served=total_served,
        total_served_notional=total_served_notional,
        maker_trading_pnl=maker_trading_pnl,
        total_stipend_paid=total_stipend_paid,
        effective_spread_bps=float(np.mean(spread_paid)) if spread_paid else float("nan"),
        organic_mm_arrival_tick=organic_mm_active_from,
    )


def run_experiment(n_trials: int = 200, base_seed: int = 2000):
    oracle_cfg = OracleConfig(n_ticks=20_000, manipulation_prob=0.0)
    demand_cfg = InformedDemandConfig()
    mm_cfg = MmArrivalConfig()

    scenarios = ["cold_start", "with_backstop", "with_dmm"]
    results: dict[str, list[ScenarioResult]] = {s: [] for s in scenarios}
    for trial in range(n_trials):
        seed = base_seed + trial
        for s in scenarios:
            results[s].append(run_scenario(s, oracle_cfg, demand_cfg, mm_cfg, seed))
    return results, oracle_cfg


def summarize(results: dict[str, list[ScenarioResult]], oracle_cfg: OracleConfig):
    print(
        f"{'scenario':<14} {'fill%':>7} {'avg spread':>11} {'maker PnL':>12} "
        f"{'stipend paid':>13} {'organic MM median tick':>23} {'never':>7}"
    )
    print("-" * 95)
    for scenario, runs in results.items():
        attempts = sum(r.total_demand_attempts for r in runs)
        served = sum(r.total_served for r in runs)
        maker_pnl = sum(r.maker_trading_pnl for r in runs)
        stipend = sum(r.total_stipend_paid for r in runs)
        spreads = [r.effective_spread_bps for r in runs if not np.isnan(r.effective_spread_bps)]
        arrivals = [r.organic_mm_arrival_tick for r in runs if r.organic_mm_arrival_tick is not None]
        never = len(runs) - len(arrivals)
        median_tick = int(np.median(arrivals)) if arrivals else -1
        print(
            f"{scenario:<14} {100 * served / attempts:>6.1f}% {np.mean(spreads):>10.2f}bp "
            f"{maker_pnl:>12,.0f} {stipend:>13,.0f} {median_tick if arrivals else 'n/a':>23} "
            f"{never:>6d}/{len(runs)}"
        )
    print(
        "\nNote: 'maker PnL' is the resident maker's own trading result (spread income minus\n"
        "adverse-selection losses to informed flow) BEFORE any stipend. 'stipend paid' is the\n"
        "break-even subsidy actually needed on top of that, summed across all trials and\n"
        "stipend periods. cold_start has no resident maker, so both are blank/zero by construction."
    )


def plot(results: dict[str, list[ScenarioResult]], out_path: str):
    import matplotlib.pyplot as plt

    fig, axes = plt.subplots(1, 2, figsize=(12, 5))

    ax = axes[0]
    scenarios = list(results.keys())
    fill_rates = [
        100 * sum(r.total_served for r in results[s]) / sum(r.total_demand_attempts for r in results[s])
        for s in scenarios
    ]
    bars = ax.bar(scenarios, fill_rates, color=["tab:red", "tab:orange", "tab:green"])
    ax.set_ylabel("% of real demand served")
    ax.set_title("Fill rate by scenario")
    ax.set_ylim(0, 100)
    for bar, rate in zip(bars, fill_rates):
        ax.text(bar.get_x() + bar.get_width() / 2, rate + 2, f"{rate:.1f}%", ha="center")

    ax = axes[1]
    colors = {"cold_start": "tab:red", "with_backstop": "tab:orange", "with_dmm": "tab:green"}
    for s in scenarios:
        arrivals = [r.organic_mm_arrival_tick for r in results[s] if r.organic_mm_arrival_tick is not None]
        never = len(results[s]) - len(arrivals)
        if arrivals:
            ax.hist(arrivals, bins=30, alpha=0.6, color=colors[s], label=f"{s} ({never} never)")
    ax.set_xlabel("Tick an ADDITIONAL organic market maker first entered")
    ax.set_ylabel("Number of runs")
    ax.set_title("Does a resident maker crowd out or attract organic competition?")
    ax.legend(fontsize=8)

    fig.tight_layout()
    fig.savefig(out_path, dpi=150)
    print(f"\nSaved chart to {out_path}")


if __name__ == "__main__":
    results, oracle_cfg = run_experiment(n_trials=200)
    summarize(results, oracle_cfg)
    plot(results, out_path="dmm_stipend.png")
