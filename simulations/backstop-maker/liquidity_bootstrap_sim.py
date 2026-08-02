"""Does the synthetic backstop maker actually solve liquidity bootstrap,
not just survive manipulation (backstop_maker_sim.py already validated
that part)?

The real bootstrap problem is a chicken-and-egg deadlock: real market
makers won't quote a pair with no visible flow (there's nothing to earn
spread on), and a pair with no quotes gets no flow (traders can't fill, so
they don't try). Nothing breaks that loop on its own.

Two scenarios, same underlying demand and same real-market-maker-arrival
model, the only difference is whether a backstop maker exists to serve
demand while the pair is otherwise empty:

    cold_start  - no backstop. Real traders show up wanting to trade; if
                  no market maker is quoting yet, the trade simply doesn't
                  happen. Zero flow is visible, so nothing attracts a real
                  MM either -- the deadlock, modeled directly.
    with_backstop - the kernel's own fully_guarded maker (from
                  backstop_maker_sim.py) always has SOME quote resting,
                  at a wider spread than a real MM would offer. Every
                  trader gets served, at a worse price, and that served
                  volume is itself the signal a real MM's entry decision
                  depends on.

A real market maker's entry probability each tick is modeled as increasing
with trailing served volume (a proxy for "MMs quote where they see
flow"). Once one enters, it quotes tighter than the backstop and wins
price-time priority, so the backstop's fill share should collapse toward
zero once the real MM is active -- it's a floor, not a competitor.

Run directly: `python3 liquidity_bootstrap_sim.py`
"""

from __future__ import annotations

from dataclasses import dataclass

import numpy as np

from backstop_maker_sim import FULLY_GUARDED, MakerConfig, OracleConfig, attempt_fill, simulate_oracle


@dataclass
class DemandConfig:
    trader_arrival_prob: float = 0.05  # probability a real taker shows up wanting to trade, per tick
    size_range: tuple[float, float] = (500.0, 8_000.0)


@dataclass
class MmArrivalConfig:
    base_prob_per_tick: float = 0.00005  # MMs occasionally enter even with no visible flow (cold discovery)
    volume_sensitivity: float = 0.002  # additional entry probability per $1,000 of trailing served volume
    trailing_window: int = 200
    real_mm_spread_bps: float = 2.5  # tighter than the backstop's 8bps once active


@dataclass
class BootstrapResult:
    scenario: str
    total_demand_attempts: int
    total_served: int
    total_unserved: int
    total_served_notional: float
    time_to_first_real_mm: int | None  # tick index, or None if never arrived
    backstop_fill_share_before_mm: float
    backstop_fill_share_after_mm: float
    effective_spread_bps_before_mm: float
    effective_spread_bps_after_mm: float


def run_bootstrap_scenario(
    use_backstop: bool,
    oracle_cfg: OracleConfig,
    demand_cfg: DemandConfig,
    mm_cfg: MmArrivalConfig,
    backstop_cfg: MakerConfig,
    seed: int,
) -> BootstrapResult:
    rng = np.random.default_rng(seed)
    true_price, oracle_price, _is_manipulated = simulate_oracle(oracle_cfg, rng)
    n = oracle_cfg.n_ticks

    served_notional_history = np.zeros(n)
    real_mm_active_from: int | None = None

    total_attempts = total_served = total_unserved = 0
    total_served_notional = 0.0
    backstop_fills_before = backstop_fills_after = 0
    total_fills_before = total_fills_after = 0
    spread_paid_before: list[float] = []
    spread_paid_after: list[float] = []

    for t in range(n - 1):
        # Real MM entry check: probability increases with trailing served volume.
        if real_mm_active_from is None:
            window_start = max(0, t - mm_cfg.trailing_window)
            trailing_volume = served_notional_history[window_start:t].sum()
            entry_prob = mm_cfg.base_prob_per_tick + mm_cfg.volume_sensitivity * (trailing_volume / 1_000)
            entry_prob = min(entry_prob, 1.0)
            if rng.random() < entry_prob:
                real_mm_active_from = t

        mm_is_active = real_mm_active_from is not None

        if rng.random() >= demand_cfg.trader_arrival_prob:
            continue

        total_attempts += 1
        side = rng.choice(["buy_from_maker", "sell_to_maker"])
        size = rng.uniform(*demand_cfg.size_range)
        center = oracle_price[t]

        # Determine the best available quote this tick.
        backstop_quote = None
        if use_backstop:
            backstop_quote = attempt_fill(backstop_cfg, oracle_price, t, side, size)

        real_mm_quote = None
        if mm_is_active:
            half_spread = center * mm_cfg.real_mm_spread_bps / 10_000
            real_mm_price = center - half_spread if side == "buy_from_maker" else center + half_spread
            real_mm_quote = (real_mm_price, size)  # real MM has no cap in this model, just a tighter price

        # Price-time priority: whichever quote is better for the taker wins.
        chosen = None
        chosen_is_backstop = False
        if backstop_quote is not None and real_mm_quote is not None:
            better_backstop = (
                backstop_quote[0] < real_mm_quote[0] if side == "sell_to_maker" else backstop_quote[0] > real_mm_quote[0]
            )
            chosen, chosen_is_backstop = (backstop_quote, True) if better_backstop else (real_mm_quote, False)
        elif backstop_quote is not None:
            chosen, chosen_is_backstop = backstop_quote, True
        elif real_mm_quote is not None:
            chosen, chosen_is_backstop = real_mm_quote, False

        if chosen is None:
            total_unserved += 1
            continue

        fill_price, fill_size = chosen
        total_served += 1
        notional = fill_price * fill_size
        total_served_notional += notional
        served_notional_history[t] = notional

        spread_bps = abs(fill_price - center) / center * 10_000
        if mm_is_active:
            total_fills_after += 1
            spread_paid_after.append(spread_bps)
            if chosen_is_backstop:
                backstop_fills_after += 1
        else:
            total_fills_before += 1
            spread_paid_before.append(spread_bps)
            if chosen_is_backstop:
                backstop_fills_before += 1

    return BootstrapResult(
        scenario="with_backstop" if use_backstop else "cold_start",
        total_demand_attempts=total_attempts,
        total_served=total_served,
        total_unserved=total_unserved,
        total_served_notional=total_served_notional,
        time_to_first_real_mm=real_mm_active_from,
        backstop_fill_share_before_mm=(backstop_fills_before / total_fills_before) if total_fills_before else float("nan"),
        backstop_fill_share_after_mm=(backstop_fills_after / total_fills_after) if total_fills_after else float("nan"),
        effective_spread_bps_before_mm=float(np.mean(spread_paid_before)) if spread_paid_before else float("nan"),
        effective_spread_bps_after_mm=float(np.mean(spread_paid_after)) if spread_paid_after else float("nan"),
    )


def run_experiment(n_trials: int = 200, base_seed: int = 1000):
    oracle_cfg = OracleConfig(n_ticks=20_000, manipulation_prob=0.0)  # bootstrap question, not the attack one
    demand_cfg = DemandConfig()
    mm_cfg = MmArrivalConfig()

    results = {"cold_start": [], "with_backstop": []}
    for trial in range(n_trials):
        seed = base_seed + trial
        results["cold_start"].append(
            run_bootstrap_scenario(False, oracle_cfg, demand_cfg, mm_cfg, FULLY_GUARDED, seed)
        )
        results["with_backstop"].append(
            run_bootstrap_scenario(True, oracle_cfg, demand_cfg, mm_cfg, FULLY_GUARDED, seed)
        )
    return results, oracle_cfg


def summarize(results: dict[str, list[BootstrapResult]], oracle_cfg: OracleConfig):
    for scenario, runs in results.items():
        n = len(runs)
        attempts = sum(r.total_demand_attempts for r in runs)
        served = sum(r.total_served for r in runs)
        unserved = sum(r.total_unserved for r in runs)
        served_notional = sum(r.total_served_notional for r in runs)
        arrivals = [r.time_to_first_real_mm for r in runs if r.time_to_first_real_mm is not None]
        never_arrived = n - len(arrivals)

        print(f"\n=== {scenario} ===")
        print(f"  demand attempts:          {attempts:>10d}")
        print(f"  served:                   {served:>10d}  ({100 * served / attempts:.1f}% fill rate)")
        print(f"  unserved (lost demand):   {unserved:>10d}  ({100 * unserved / attempts:.1f}%)")
        print(f"  total served notional:    ${served_notional:>14,.0f}")
        if arrivals:
            print(
                f"  real MM arrived:          {len(arrivals)}/{n} runs, "
                f"median tick {int(np.median(arrivals))} of {oracle_cfg.n_ticks} "
                f"({100 * np.median(arrivals) / oracle_cfg.n_ticks:.1f}% of the run)"
            )
        else:
            print("  real MM arrived:          never, in any run")
        if never_arrived:
            print(f"  real MM NEVER arrived in: {never_arrived}/{n} runs")

        before = [r.effective_spread_bps_before_mm for r in runs if not np.isnan(r.effective_spread_bps_before_mm)]
        after = [r.effective_spread_bps_after_mm for r in runs if not np.isnan(r.effective_spread_bps_after_mm)]
        if before:
            print(f"  avg effective spread paid BEFORE real MM: {np.mean(before):.2f} bps")
        if after:
            print(f"  avg effective spread paid AFTER real MM:  {np.mean(after):.2f} bps")

        backstop_share_after = [
            r.backstop_fill_share_after_mm for r in runs if not np.isnan(r.backstop_fill_share_after_mm)
        ]
        if backstop_share_after:
            print(f"  backstop's fill share AFTER a real MM arrives: {100 * np.mean(backstop_share_after):.1f}%")


def plot(results: dict[str, list[BootstrapResult]], oracle_cfg: OracleConfig, out_path: str):
    import matplotlib.pyplot as plt

    fig, axes = plt.subplots(1, 2, figsize=(12, 5))

    # Left: distribution of time-to-first-real-MM.
    ax = axes[0]
    for scenario, color in [("cold_start", "tab:red"), ("with_backstop", "tab:green")]:
        arrivals = [r.time_to_first_real_mm for r in results[scenario] if r.time_to_first_real_mm is not None]
        never = len(results[scenario]) - len(arrivals)
        label = f"{scenario} ({never} of {len(results[scenario])} never got a real MM)"
        if arrivals:
            ax.hist(arrivals, bins=30, alpha=0.6, color=color, label=label)
    ax.set_xlabel("Tick a real market maker first entered")
    ax.set_ylabel("Number of simulation runs")
    ax.set_title("Time to bootstrap a real market maker")
    ax.legend(fontsize=8)

    # Right: fill rate comparison.
    ax = axes[1]
    scenarios = list(results.keys())
    fill_rates = [
        100 * sum(r.total_served for r in results[s]) / sum(r.total_demand_attempts for r in results[s])
        for s in scenarios
    ]
    bars = ax.bar(scenarios, fill_rates, color=["tab:red", "tab:green"])
    ax.set_ylabel("% of real demand actually served")
    ax.set_title("Served vs. lost demand")
    ax.set_ylim(0, 100)
    for bar, rate in zip(bars, fill_rates):
        ax.text(bar.get_x() + bar.get_width() / 2, rate + 2, f"{rate:.1f}%", ha="center")

    fig.tight_layout()
    fig.savefig(out_path, dpi=150)
    print(f"\nSaved chart to {out_path}")


if __name__ == "__main__":
    results, oracle_cfg = run_experiment(n_trials=200)
    summarize(results, oracle_cfg)
    plot(results, oracle_cfg, out_path="liquidity_bootstrap.png")
