"""Simulation: how much does position privacy extend a trading strategy's life?

Context: Cerdic's privacy moat for agents (docs/agentic-finance-research.md)
rests on a specific claim: a strategy traded with sealed positions keeps its
edge longer because copycats can't see the trades, only the coarse PnL
attestations the agent chooses to publish. A strategy traded on a transparent
venue broadcasts its entries and exits in real time; copycat capital piles in
and the edge decays through crowding.

Model: a strategy earns a per-period edge that decays exponentially in total
copycat capital (crowding impact):

    edge_t = edge_0 * exp(-lambda * copycat_capital_t)

Copycat capital follows observed profitability, but what copycats can OBSERVE
differs by venue:

    transparent - every trade is visible in real time. Copycats see the true
                  current edge and pile in logistically, up to the venue's
                  copyable capacity.
    sealed      - only periodic (weekly) PnL attestations are visible.
                  Copycats must infer the edge from a noisy, delayed signal,
                  and they can't confirm WHICH trades made the money, so their
                  deployment is slower and discounted by inference uncertainty.

Swept across initial edge sizes, because the honest expectation is that
privacy is NOT uniformly valuable: a huge edge exhausts its own capacity
regardless, and a dead strategy has nothing worth hiding.

Reported: edge half-life (weeks until edge_0/2) and cumulative strategy PnL
over the horizon, per venue, per regime.

Run directly: `python3 alpha_decay_sim.py`
"""

from __future__ import annotations

from dataclasses import dataclass

import numpy as np
import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt


@dataclass
class Config:
    weeks: int = 104                  # 2-year horizon
    n_trials: int = 200
    strategy_capital: float = 5e6     # the strategy's own deployed capital
    copy_capacity: float = 60e6       # max copycat capital the venue absorbs
    decay_lambda: float = 1 / 30e6    # edge halving per ~$21M of crowding
    copy_growth: float = 0.08         # fraction of the gap closed per week;
                                       # ~3 months for copycats to substantially
                                       # crowd even a fully visible strategy
                                       # (observe -> allocate -> deploy)
    attestation_noise: float = 0.35   # relative std of the weekly attested edge
    inference_discount: float = 0.55  # copycats deploy this fraction under inference
    obs_window: int = 4               # copycats react to a 4-week mean of
                                       # observed edge, not one print
    salience_per_week: float = 6_000 # $/week of visible profit at which a
                                       # strategy is unmissable. Discovery
                                       # scales with ABSOLUTE visible profit:
                                       # a 40bp/wk edge on $5M ($20k/wk) is
                                       # found fast anywhere; an 8bp/wk edge
                                       # ($4k/wk) is subtle even in the open.
    seed: int = 31


REGIMES = {
    "mid edge (8 bp/wk)": 0.0008,
    "high edge (40 bp/wk)": 0.0040,
}


def run(rng: np.random.Generator, cfg: Config, edge0: float,
        venue: str) -> tuple[np.ndarray, np.ndarray]:
    """Returns (edge path, cumulative strategy PnL path), length cfg.weeks."""
    edges = np.zeros(cfg.weeks)
    pnl = np.zeros(cfg.weeks)
    obs_hist: list[float] = []
    copy_cap = 0.0
    for t in range(cfg.weeks):
        edge = edge0 * np.exp(-cfg.decay_lambda * copy_cap)
        edges[t] = edge
        pnl[t] = (pnl[t - 1] if t else 0.0) + edge * cfg.strategy_capital

        if venue == "transparent":
            obs_hist.append(edge)  # real-time, exact
            growth = cfg.copy_growth
        else:
            # Weekly attestation: noisy, and copycats discount the inference.
            obs_hist.append(max(0.0, edge * (1 + cfg.attestation_noise
                                             * rng.standard_normal())))
            growth = cfg.copy_growth * cfg.inference_discount

        observed = float(np.mean(obs_hist[-cfg.obs_window:]))
        salience = min(1.0, observed * cfg.strategy_capital / cfg.salience_per_week)

        # Copycat capital chases OBSERVED profit only: zero observed edge means
        # zero target. Discovery speed scales with absolute visible profit, so
        # a big edge gets mobbed on any venue while a small one stays subtle —
        # the regime in which privacy buys the most time.
        target = cfg.copy_capacity * salience
        copy_cap += growth * salience * (target - copy_cap)
        copy_cap = max(0.0, min(copy_cap, cfg.copy_capacity))

    return edges, pnl


def main() -> None:
    cfg = Config()
    rng = np.random.default_rng(cfg.seed)

    fig, axes = plt.subplots(2, 2, figsize=(11, 7))
    print(f"{'regime':<22}{'venue':<14}{'edge half-life (wk)':>20}{'2y PnL ($M)':>14}")

    for col, (regime, edge0) in enumerate(REGIMES.items()):
        for venue, color in (("transparent", "#c0392b"), ("sealed", "#27ae60")):
            half_lives, finals = [], []
            edge_paths, pnl_paths = [], []
            for _ in range(cfg.n_trials):
                e, p = run(rng, cfg, edge0, venue)
                edge_paths.append(e)
                pnl_paths.append(p)
                below = np.where(e <= edge0 / 2)[0]
                half_lives.append(below[0] if len(below) else cfg.weeks)
                finals.append(p[-1])
            edge_paths = np.array(edge_paths)
            pnl_paths = np.array(pnl_paths)
            hl = np.median(half_lives)
            hl_str = f">{cfg.weeks}" if hl >= cfg.weeks else f"{hl:.0f}"
            print(f"{regime:<22}{venue:<14}{hl_str:>20}"
                  f"{np.median(finals) / 1e6:>13.2f}")

            ax_e = axes[0, col]
            ax_p = axes[1, col]
            wk = np.arange(cfg.weeks)
            ax_e.plot(wk, np.median(edge_paths, axis=0) * 1e4, color=color,
                      label=venue, lw=2)
            ax_p.plot(wk, np.median(pnl_paths, axis=0) / 1e6, color=color,
                      label=venue, lw=2)

        axes[0, col].axhline(edge0 * 1e4 / 2, color="k", ls=":", lw=1, alpha=0.5)
        axes[0, col].set_title(f"edge decay — {regime}")
        axes[0, col].set_ylabel("edge (bp/week)")
        axes[0, col].legend(fontsize=8)
        axes[0, col].grid(alpha=0.3)
        axes[1, col].set_title(f"cumulative strategy PnL — {regime}")
        axes[1, col].set_ylabel("PnL ($M)")
        axes[1, col].set_xlabel("week")
        axes[1, col].legend(fontsize=8)
        axes[1, col].grid(alpha=0.3)

    fig.suptitle("Copycat crowding: transparent order flow vs sealed positions + weekly attestations")
    fig.tight_layout()
    fig.savefig("alpha_decay.png", dpi=150, bbox_inches="tight")
    print("\nwrote alpha_decay.png")


if __name__ == "__main__":
    main()
