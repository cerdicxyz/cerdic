"""Does Ostium's dynamic-spread idea (spread widens with utilization/
inventory imbalance, not a flat constant) actually improve the backstop
maker, or is a flat spread already good enough?

The backstop maker built so far (backstop_maker_sim.py's `fully_guarded`
config, now implemented in crates/risk::backstop_maker) uses a FLAT
spread_bps regardless of how much inventory it has already accumulated.
Ostium's pooled-LP model instead widens its spread as utilization/open-
interest imbalance grows -- the more directional exposure a pool is
already carrying, the more expensive it gets to push further in that same
direction. The point isn't to adopt Ostium's pooled-counterparty
architecture (a separate, larger discussion, and a worse fit for a
portfolio-margined kernel), just this one pricing idea: does spread
widening as a function of the MAKER'S OWN inventory reduce its risk
without materially hurting service quality?

    flat_spread     - spread_bps is a constant (the current implementation).
    dynamic_spread  - spread_bps = base + k * (|inventory| / inventory_ref),
                       widening symmetrically as the maker's own net
                       position grows, capped at a max.

Tested against BOTH stress scenarios already validated separately:
    1. The one-tick manipulation attacker (backstop_maker_sim.py's scenario)
    2. Ordinary informed order flow (dmm_stipend_sim.py's scenario)

because a change that helps against one and hurts the other would be a
wash, not an improvement.

Run directly: `python3 dynamic_spread_sim.py`
"""

from __future__ import annotations

from dataclasses import dataclass, replace

import numpy as np

from backstop_maker_sim import FULLY_GUARDED, MakerConfig, OracleConfig, fair_value, simulate_oracle
from dmm_stipend_sim import InformedDemandConfig, _trade_side_and_pnl_sign
from liquidity_bootstrap_sim import MmArrivalConfig


@dataclass
class DynamicSpreadConfig:
    base_spread_bps: float = 8.0
    max_spread_bps: float = 24.0
    widen_per_unit_utilization_bps: float = 16.0  # extra bps at 100% utilization
    inventory_reference: float = 20_000.0  # inventory (base units) that counts as "100% utilized"
    # Price elasticity of demand: without this, taker size is fixed regardless
    # of the quoted spread, which makes a wider spread look like a pure win
    # (more revenue on the SAME executed trades) rather than testing the
    # actual claimed mechanism (discouraging further one-directional flow).
    # A trade's accept probability decays as spread widens past the base:
    # accept_prob = exp(-elasticity_k * max(0, spread_bps - base_spread_bps)).
    elasticity_k: float = 0.02


def dynamic_spread_bps(cfg: DynamicSpreadConfig, inventory: float) -> float:
    utilization = min(1.0, abs(inventory) / cfg.inventory_reference)
    return min(cfg.max_spread_bps, cfg.base_spread_bps + cfg.widen_per_unit_utilization_bps * utilization)


def taker_accepts(rng: np.random.Generator, dynamic_cfg: DynamicSpreadConfig | None, spread_bps: float) -> bool:
    """Whether a price-sensitive taker proceeds at the quoted spread. Flat
    spread has no elasticity applied (nothing to be sensitive TO beyond the
    constant it already sees every time); only meaningful once the spread
    itself moves in response to inventory."""
    if dynamic_cfg is None:
        return True
    excess = max(0.0, spread_bps - dynamic_cfg.base_spread_bps)
    accept_prob = np.exp(-dynamic_cfg.elasticity_k * excess)
    return rng.random() < accept_prob


def attempt_fill_with_inventory(
    maker_cfg: MakerConfig,
    dynamic_cfg: DynamicSpreadConfig | None,  # None = flat spread (maker_cfg.spread_bps)
    oracle_price: np.ndarray,
    t: int,
    side: str,
    requested_size: float,
    inventory: float,
) -> tuple[float, float] | None:
    """Same guard logic as backstop_maker_sim.attempt_fill (TWAP center,
    notional cap, last-look), but the spread is either flat or a function
    of the maker's current inventory."""
    center = fair_value(maker_cfg, oracle_price, t)
    spread_bps = maker_cfg.spread_bps if dynamic_cfg is None else dynamic_spread_bps(dynamic_cfg, inventory)
    half_spread = center * spread_bps / 10_000
    quote_price = center - half_spread if side == "buy_from_maker" else center + half_spread

    size = requested_size
    if maker_cfg.use_notional_cap:
        size = min(size, maker_cfg.notional_cap)

    if maker_cfg.use_last_look:
        window = oracle_price[max(0, t - maker_cfg.last_look_window + 1) : t + 1]
        reference = float(window.mean())
        deviation_bps = abs(oracle_price[t] - reference) / reference * 10_000
        if deviation_bps > maker_cfg.last_look_threshold_bps:
            return None

    return quote_price, size


def _update_inventory(inventory: float, side: str, size: float) -> float:
    # "buy_from_maker": taker buys, maker sells -> maker inventory decreases.
    # "sell_to_maker": taker sells, maker buys -> maker inventory increases.
    return inventory - size if side == "buy_from_maker" else inventory + size


def quote_and_maybe_fill(
    rng: np.random.Generator,
    maker_cfg: MakerConfig,
    dynamic_cfg: DynamicSpreadConfig | None,
    oracle_price: np.ndarray,
    t: int,
    side: str,
    requested_size: float,
    inventory: float,
) -> tuple[float, float] | None:
    """Elasticity check first (a price-sensitive taker may walk away from a
    wide dynamic quote before ever reaching the maker's own guards), then
    the maker's guards (TWAP pricing, notional cap, last-look)."""
    if dynamic_cfg is not None:
        spread_bps = dynamic_spread_bps(dynamic_cfg, inventory)
        if not taker_accepts(rng, dynamic_cfg, spread_bps):
            return None
    return attempt_fill_with_inventory(maker_cfg, dynamic_cfg, oracle_price, t, side, requested_size, inventory)


# ---------------------------------------------------------------------------
# Scenario 1: manipulation attacker (same shape as backstop_maker_sim.py).
# ---------------------------------------------------------------------------


@dataclass
class AttackRunResult:
    config_name: str
    cumulative_pnl: float
    worst_trade_pnl: float
    max_abs_inventory: float
    noise_served_pct: float


def run_attack_scenario(use_dynamic: bool, oracle_cfg: OracleConfig, seed: int) -> AttackRunResult:
    rng = np.random.default_rng(seed)
    true_price, oracle_price, is_manipulated = simulate_oracle(oracle_cfg, rng)
    dynamic_cfg = DynamicSpreadConfig() if use_dynamic else None

    inventory = 0.0
    max_abs_inventory = 0.0
    cumulative_pnl = 0.0
    worst_trade_pnl = 0.0
    noise_attempts = noise_fills = 0
    attacker_size = 50_000.0
    noise_prob = 0.02

    for t in range(oracle_cfg.n_ticks - 1):
        if is_manipulated[t]:
            direction_up = oracle_price[t] > true_price[t]
            side = "buy_from_maker" if direction_up else "sell_to_maker"
            result = quote_and_maybe_fill(rng, FULLY_GUARDED, dynamic_cfg, oracle_price, t, side, attacker_size, inventory)
        elif rng.random() < noise_prob:
            side = rng.choice(["buy_from_maker", "sell_to_maker"])
            size = rng.uniform(200.0, 2_000.0)
            noise_attempts += 1
            result = quote_and_maybe_fill(rng, FULLY_GUARDED, dynamic_cfg, oracle_price, t, side, size, inventory)
            if result is not None:
                noise_fills += 1
        else:
            result = None

        if result is None:
            continue
        fill_price, fill_size = result
        inventory = _update_inventory(inventory, side, fill_size)
        max_abs_inventory = max(max_abs_inventory, abs(inventory))

        true_next = true_price[t + 1]
        trade_pnl = (true_next - fill_price) * fill_size if side == "buy_from_maker" else (fill_price - true_next) * fill_size
        cumulative_pnl += trade_pnl
        worst_trade_pnl = min(worst_trade_pnl, trade_pnl)

    return AttackRunResult(
        config_name="dynamic_spread" if use_dynamic else "flat_spread",
        cumulative_pnl=cumulative_pnl,
        worst_trade_pnl=worst_trade_pnl,
        max_abs_inventory=max_abs_inventory,
        noise_served_pct=100 * noise_fills / noise_attempts if noise_attempts else 0.0,
    )


# ---------------------------------------------------------------------------
# Scenario 2: ordinary informed flow (same shape as dmm_stipend_sim.py).
# ---------------------------------------------------------------------------


@dataclass
class InformedFlowResult:
    config_name: str
    maker_trading_pnl: float
    breakeven_subsidy_needed: float
    max_abs_inventory: float
    fill_rate_pct: float
    avg_effective_spread_bps: float


def run_informed_flow_scenario(use_dynamic: bool, oracle_cfg: OracleConfig, demand_cfg: InformedDemandConfig, seed: int) -> InformedFlowResult:
    rng = np.random.default_rng(seed)
    true_price, oracle_price, _ = simulate_oracle(oracle_cfg, rng)
    dynamic_cfg = DynamicSpreadConfig() if use_dynamic else None

    inventory = 0.0
    max_abs_inventory = 0.0
    maker_pnl = 0.0
    total_attempts = total_served = 0
    spreads_paid: list[float] = []

    for t in range(oracle_cfg.n_ticks - 1):
        if rng.random() >= demand_cfg.trader_arrival_prob:
            continue
        total_attempts += 1
        side = _trade_side_and_pnl_sign(rng, demand_cfg.informed_fraction, true_price, t)
        size = rng.uniform(*demand_cfg.size_range)
        result = quote_and_maybe_fill(rng, FULLY_GUARDED, dynamic_cfg, oracle_price, t, side, size, inventory)
        if result is None:
            continue
        fill_price, fill_size = result
        total_served += 1
        inventory = _update_inventory(inventory, side, fill_size)
        max_abs_inventory = max(max_abs_inventory, abs(inventory))
        spreads_paid.append(abs(fill_price - oracle_price[t]) / oracle_price[t] * 10_000)

        true_next = true_price[t + 1]
        trade_pnl = (true_next - fill_price) * fill_size if side == "buy_from_maker" else (fill_price - true_next) * fill_size
        maker_pnl += trade_pnl

    return InformedFlowResult(
        config_name="dynamic_spread" if use_dynamic else "flat_spread",
        maker_trading_pnl=maker_pnl,
        breakeven_subsidy_needed=max(0.0, -maker_pnl),
        max_abs_inventory=max_abs_inventory,
        fill_rate_pct=100 * total_served / total_attempts if total_attempts else 0.0,
        avg_effective_spread_bps=float(np.mean(spreads_paid)) if spreads_paid else float("nan"),
    )


def run_experiment(n_trials: int = 200, base_seed: int = 4000):
    oracle_cfg_attack = OracleConfig()  # default manipulation_prob from backstop_maker_sim
    oracle_cfg_informed = OracleConfig(n_ticks=20_000, manipulation_prob=0.0)
    demand_cfg = InformedDemandConfig()

    attack_results = {"flat_spread": [], "dynamic_spread": []}
    informed_results = {"flat_spread": [], "dynamic_spread": []}

    for trial in range(n_trials):
        seed = base_seed + trial
        attack_results["flat_spread"].append(run_attack_scenario(False, oracle_cfg_attack, seed))
        attack_results["dynamic_spread"].append(run_attack_scenario(True, oracle_cfg_attack, seed))
        informed_results["flat_spread"].append(run_informed_flow_scenario(False, oracle_cfg_informed, demand_cfg, seed))
        informed_results["dynamic_spread"].append(run_informed_flow_scenario(True, oracle_cfg_informed, demand_cfg, seed))

    return attack_results, informed_results


def summarize(attack_results, informed_results):
    print("=== Manipulation-attacker scenario ===")
    print(f"{'config':<16} {'mean PnL':>12} {'worst PnL':>12} {'max |inventory|':>16} {'noise served%':>14}")
    print("-" * 75)
    for name, runs in attack_results.items():
        pnl = np.array([r.cumulative_pnl for r in runs])
        worst = np.array([r.worst_trade_pnl for r in runs])
        max_inv = np.array([r.max_abs_inventory for r in runs])
        noise_pct = np.mean([r.noise_served_pct for r in runs])
        print(f"{name:<16} {pnl.mean():>12.1f} {worst.min():>12.1f} {max_inv.mean():>16.1f} {noise_pct:>13.1f}%")

    print("\n=== Ordinary informed-flow scenario ===")
    print(f"{'config':<16} {'maker PnL':>12} {'subsidy needed':>15} {'max |inventory|':>16} {'fill%':>7} {'avg spread':>11}")
    print("-" * 90)
    for name, runs in informed_results.items():
        maker_pnl = sum(r.maker_trading_pnl for r in runs)
        subsidy = sum(r.breakeven_subsidy_needed for r in runs)
        max_inv = np.mean([r.max_abs_inventory for r in runs])
        fill_pct = np.mean([r.fill_rate_pct for r in runs])
        spread = np.nanmean([r.avg_effective_spread_bps for r in runs])
        print(f"{name:<16} {maker_pnl:>12,.0f} {subsidy:>15,.0f} {max_inv:>16.1f} {fill_pct:>6.1f}% {spread:>10.2f}bp")


def plot(attack_results, informed_results, out_path: str):
    import matplotlib.pyplot as plt

    fig, axes = plt.subplots(1, 2, figsize=(12, 5))

    ax = axes[0]
    data = [[r.cumulative_pnl for r in attack_results[name]] for name in ["flat_spread", "dynamic_spread"]]
    ax.boxplot(data, labels=["flat_spread", "dynamic_spread"], showmeans=True)
    ax.axhline(0, color="gray", linewidth=0.8, linestyle="--")
    ax.set_ylabel("Cumulative PnL per run (USD)")
    ax.set_title("Manipulation-attacker scenario")

    ax = axes[1]
    names = ["flat_spread", "dynamic_spread"]
    subsidies = [sum(r.breakeven_subsidy_needed for r in informed_results[n]) for n in names]
    bars = ax.bar(names, subsidies, color=["tab:orange", "tab:green"])
    ax.set_ylabel("Total break-even subsidy needed across all trials (USD)")
    ax.set_title("Ordinary informed-flow scenario")
    for bar, val in zip(bars, subsidies):
        ax.text(bar.get_x() + bar.get_width() / 2, val, f"${val:,.0f}", ha="center", va="bottom")

    fig.tight_layout()
    fig.savefig(out_path, dpi=150)
    print(f"\nSaved chart to {out_path}")


if __name__ == "__main__":
    attack_results, informed_results = run_experiment(n_trials=200)
    summarize(attack_results, informed_results)
    plot(attack_results, informed_results, out_path="dynamic_spread.png")
