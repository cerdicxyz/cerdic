"""Simulation: does kernel-enforced (pre-trade) daily-loss limiting actually
reduce funder losses versus post-hoc monitoring, and by how much?

Context: Cerdic's prop-desk design for agents (docs/agentic-finance-research.md)
enforces a funded account's daily loss limit inside the CapabilityRegistry /
settlement path: any trade that would push the account's loss past the limit
reverts, so the limit is a hard bound, not a Terms-of-Service clause. Web2 prop
firms (FTMO-shaped) enforce the same rule with monitoring dashboards and close
the account AFTER a breach is observed.

This models the difference honestly:

    post_hoc_eod    - breach detected only at end-of-day review. Intraday
                       losses run as far as they run (the real web2 baseline).
    post_hoc_4h     - checks every 4 hours; account frozen at the first check
                       that observes a breach (the optimistic web2 case).
    kernel_pretrade - any state transition that would push realized loss past
                       the limit reverts; loss capped at the limit plus one
                       crossing trade's overshoot.

And the honest counter-case, modeled separately:

    gap scenario    - an overnight/weekend gap jumps the loss past the limit
                       while no trades occur. Pre-trade enforcement CANNOT
                       prevent this; the sim quantifies how much of the
                       enforcement benefit survives gap risk.

Agent population per funded cohort:
    70% normal / 25% aggressive / 5% blow-up (negative drift, very high vol).

Run directly: `python3 drawdown_enforcement_sim.py`
"""

from __future__ import annotations

from dataclasses import dataclass

import numpy as np
import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt


@dataclass
class Config:
    n_agents: int = 2_000
    n_days: int = 250
    steps_per_day: int = 96  # 15-minute steps, 24h FX-style market
    daily_loss_limit_frac: float = 0.05  # 5% of equity, FTMO-shaped
    account_size: float = 10_000.0
    student_t_df: float = 5.0
    gap_prob: float = 0.01  # per account-day
    gap_mean_frac: float = 0.02
    gap_std_frac: float = 0.01
    seed: int = 7


# (daily drift, daily vol) as a fraction of ACCOUNT EQUITY — i.e. post-leverage,
# which is why these are large: a funded account at 20x on a 0.5%-daily-vol FX
# pair runs ~10% equity vol fully deployed. Prop firms' 5% daily limit is
# breached constantly in practice precisely because accounts are leveraged.
CLASSES = {
    "normal": (0.0015, 0.025),      # 2-sigma breach: a bad day every couple months
    "aggressive": (0.0, 0.05),      # 1-sigma breach: breaches are routine
    "blowup": (-0.005, 0.09),       # negative drift, breaching most weeks
}
CLASS_PROBS = (0.70, 0.25, 0.05)


# Post-breach behavior: the documented failure mode this rule actually exists
# for. A trader who has breached intraday and is still allowed to trade does not
# keep trading the same book — they size UP trying to recover before the daily
# reset ("revenge trading"; the reason web2 prop firms lose multiples of the
# limit on breached accounts rather than the limit itself). Modeled honestly:
# post-breach continuation runs at 2x vol with negative drift. Kernel pre-trade
# enforcement makes that continuation impossible — there is no behavior to model,
# the trade reverts.
TILT_DRIFT = -0.01   # fraction of equity per day while tilting
TILT_VOL_MULT = 2.0


def _continuation(rng: np.random.Generator, cfg: Config, n_steps: int,
                  tilt: bool) -> np.ndarray:
    """PnL of the remaining steps after a breach, under either the trader's
    normal book or the tilt book."""
    drift, vol = params_current[0], params_current[1]
    if tilt:
        drift, vol = TILT_DRIFT, vol * TILT_VOL_MULT
    shocks = rng.standard_t(cfg.student_t_df, size=n_steps)
    shocks /= np.sqrt(cfg.student_t_df / (cfg.student_t_df - 2))
    return (drift / cfg.steps_per_day
            + vol / np.sqrt(cfg.steps_per_day) * shocks).sum()


def simulate_breach_losses(cfg: Config) -> dict[str, np.ndarray]:
    """For every agent-day whose intraday path crosses the limit, record what
    each enforcement model actually ends up losing (fraction of equity)."""
    global params_current
    rng = np.random.default_rng(cfg.seed)
    classes = rng.choice(len(CLASSES), size=cfg.n_agents, p=CLASS_PROBS)
    params = list(CLASSES.values())
    out = {"post_hoc_eod": [], "post_hoc_4h": [], "kernel_pretrade": []}

    for i in range(cfg.n_agents):
        params_current = params[classes[i]]
        drift, vol = params_current
        shocks = rng.standard_t(cfg.student_t_df, size=(cfg.n_days, cfg.steps_per_day))
        shocks /= np.sqrt(cfg.student_t_df / (cfg.student_t_df - 2))
        cum = np.cumsum(
            drift / cfg.steps_per_day + vol / np.sqrt(cfg.steps_per_day) * shocks, axis=1
        )

        breach = cum <= -cfg.daily_loss_limit_frac
        for d in np.where(breach.any(axis=1))[0]:
            path = cum[d]
            first_cross = int(np.argmax(path <= -cfg.daily_loss_limit_frac))
            at_cross = -path[first_cross]
            remaining = cfg.steps_per_day - 1 - first_cross

            # Kernel: stops at the crossing trade. Nothing else happens.
            out["kernel_pretrade"].append(at_cross)
            if remaining == 0:
                out["post_hoc_eod"].append(at_cross)
                out["post_hoc_4h"].append(at_cross)
                continue

            # Post-hoc EOD: the trader keeps trading the tilt book all day.
            out["post_hoc_eod"].append(
                at_cross - _continuation(rng, cfg, remaining, tilt=True)
            )

            # Post-hoc 4h: tilt book runs only until the next checkpoint.
            steps_to_check = 24 - ((first_cross + 1) % 24)
            out["post_hoc_4h"].append(
                at_cross - _continuation(rng, cfg, steps_to_check, tilt=True)
            )

    return {k: np.array(v) for k, v in out.items()}


def main() -> None:
    cfg = Config()
    losses = simulate_breach_losses(cfg)
    n_breach = len(losses["post_hoc_eod"])
    limit = cfg.daily_loss_limit_frac

    print(f"breach days: {n_breach:,} ({n_breach / (cfg.n_agents * cfg.n_days):.2%} of account-days)")
    print(f"{'model':<18}{'mean loss':>12}{'p95 loss':>12}{'max loss':>12}")
    for name, arr in losses.items():
        print(f"{name:<18}{arr.mean():>11.2%}{np.percentile(arr, 95):>11.2%}{arr.max():>11.2%}")

    breach_freq = n_breach / (cfg.n_agents * cfg.n_days)
    accounts, year_days = 1_000, 250
    annual = {
        k: breach_freq * accounts * year_days * v.mean() * cfg.account_size
        for k, v in losses.items()
    }
    print("\nannual desk capital lost (1,000 funded accounts, no gap risk):")
    for k, v in annual.items():
        print(f"  {k:<16}${v:>12,.0f}")

    # Gap risk: unpreventable by any pre-trade enforcement; hits all models equally.
    rng = np.random.default_rng(cfg.seed + 1)
    gaps = np.abs(rng.normal(
        cfg.gap_mean_frac, cfg.gap_std_frac, size=int(cfg.gap_prob * accounts * year_days)
    ))
    gap_cost = gaps.sum() * cfg.account_size
    print(f"\ngap risk: {len(gaps):,} events/yr, mean {gaps.mean():.2%}, annual ${gap_cost:,.0f}")
    print(f"kernel total incl. gaps: ${annual['kernel_pretrade'] + gap_cost:,.0f}")
    surviving = 1 - (annual["kernel_pretrade"] + gap_cost) / (annual["post_hoc_eod"] + gap_cost)
    print(f"enforcement benefit surviving gap risk: {surviving:.1%}")

    fig, (ax1, ax2) = plt.subplots(1, 2, figsize=(11, 4.2))
    colors = {"post_hoc_eod": "#c0392b", "post_hoc_4h": "#e67e22", "kernel_pretrade": "#27ae60"}
    labels = {"post_hoc_eod": "post-hoc (EOD review)", "post_hoc_4h": "post-hoc (4h checks)",
              "kernel_pretrade": "kernel pre-trade"}
    for name, arr in losses.items():
        s = np.sort(arr)
        ax1.plot(s * 100, np.arange(1, len(arr) + 1) / len(arr),
                 color=colors[name], label=labels[name], lw=2)
    ax1.axvline(limit * 100, color="k", ls="--", lw=1, alpha=0.6)
    ax1.text(limit * 100 + 0.1, 0.05, "5% daily limit", rotation=90, fontsize=8)
    ax1.set_xlabel("loss per breach day (% of account equity)")
    ax1.set_ylabel("CDF")
    ax1.set_title("What a breached daily-loss limit actually costs")
    ax1.legend(fontsize=8)
    ax1.grid(alpha=0.3)
    ax1.set_xlim(0, 25)

    models = list(annual.keys())
    x = np.arange(len(models))
    ax2.bar(x - 0.18, [annual[m] / 1e6 for m in models], 0.36, label="no gap risk",
            color=[colors[m] for m in models], alpha=0.85)
    ax2.bar(x + 0.18, [(annual[m] + gap_cost) / 1e6 for m in models], 0.36,
            label="incl. gap risk", color=[colors[m] for m in models], alpha=0.45, hatch="//")
    ax2.set_xticks(x)
    ax2.set_xticklabels([labels[m].replace(" (", "\n(") for m in models], fontsize=8)
    ax2.set_ylabel("annual desk capital lost ($M)")
    ax2.set_title("1,000 funded accounts, one year")
    ax2.legend(fontsize=8)
    ax2.grid(alpha=0.3, axis="y")

    fig.tight_layout()
    fig.savefig("drawdown_enforcement.png", dpi=150)
    print("\nwrote drawdown_enforcement.png")


if __name__ == "__main__":
    main()
