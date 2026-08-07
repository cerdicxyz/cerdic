"""Simulation: how fast does a TEE-attested PnL track record separate trading
skill from luck — and does a self-reported track record ever converge at all?

Context: docs/agentic-finance-research.md argues Cerdic's TEE can sign
cumulative-PnL attestations for an agent without revealing its positions, and
that feeding those into ERC-8004's ReputationRegistry produces verifiable
track records with zero strategy leakage. The obvious objection: PnL over any
finite window is mostly luck. This sim measures how much truth there is in
that, under two reporting regimes:

    attested      - the funder sees the agent's true cumulative PnL, signed by
                     the TEE, bound to one portfolioKey. Ungameable.
    self_reported - the agent reports its own numbers, modeled in the shape
                     this actually takes (fund incubation, CTA marketing):
                     k parallel accounts, report the best. Legal today, and
                     the de-facto standard of "verified" web2 track records.

Population: true daily Sharpe centered at zero; a real minority is skilled.

The metric that matters is OUT-OF-SAMPLE: a funder buying a track record is
buying future performance. So we measure, vs track length T:
    - Spearman(observed track, realized PnL over the FOLLOWING 90 days)
    - precision@10%: fraction of the observed top decile that lands in the
      top decile of out-of-sample PnL
A best-of-k self-reported track embeds one-time max-noise that does NOT
repeat out of sample — the funded agent trades one account going forward,
not a fresh lottery. That non-persistence is the bias.

The honest finding to look for: attested PnL converges SLOWLY (skill is hard
to see through a year of noise), but it converges. Self-reported tracks
predict the future strictly worse at every T — the gap IS the selection bias.

Run directly: `python3 reputation_convergence_sim.py`
"""

from __future__ import annotations

from dataclasses import dataclass

import numpy as np
import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt
from scipy.stats import rankdata


@dataclass
class Config:
    n_agents: int = 1_000
    n_trials: int = 40           # cohort resamples per T, for percentile bands
    max_days: int = 360
    t_marks: tuple = (7, 14, 30, 60, 90, 180, 360)
    daily_vol: float = 0.02      # 2% equity vol/day, leveraged-book shaped
    skill_spread: float = 0.08   # true daily Sharpe ~ N(0, 0.08)
    skilled_frac: float = 0.10
    skilled_sharpe: float = 0.20
    # Self-reported agents each run k_i parallel accounts and report the best.
    # k is HETEROGENEOUS and undisclosed — that's the realistic shape: honest
    # agents run one book; gamers run incubation lotteries. Uniform-k is the
    # special case where best-of-k is actually a tighter skill estimator
    # (max of k iid normals has lower variance than one draw), which is NOT
    # the world funders live in.
    k_choices: tuple = (1, 3, 5, 10, 20)
    k_probs: tuple = (0.4, 0.2, 0.2, 0.1, 0.1)
    seed: int = 11


FUTURE_DAYS = 90  # out-of-sample window the track record is judged against


def cohort_skills(rng: np.random.Generator, cfg: Config) -> np.ndarray:
    skills = rng.normal(0.0, cfg.skill_spread, cfg.n_agents)
    skilled = rng.random(cfg.n_agents) < cfg.skilled_frac
    skills[skilled] = np.abs(skills[skilled]) + cfg.skilled_sharpe
    return skills


def run_trial(rng: np.random.Generator, cfg: Config) -> dict:
    skills = cohort_skills(rng, cfg)
    daily_pnl = (skills[None, :] * cfg.daily_vol
                 + cfg.daily_vol * rng.standard_normal((cfg.max_days, cfg.n_agents)))
    track_attested = np.cumsum(daily_pnl, axis=0)

    k_max = max(cfg.k_choices)
    k_draw = rng.choice(cfg.k_choices, size=cfg.n_agents, p=cfg.k_probs)
    alt = (skills[None, :, None] * cfg.daily_vol
           + cfg.daily_vol
           * rng.standard_normal((cfg.max_days, cfg.n_agents, k_max)))
    # Mask accounts beyond each agent's k_i before taking the max.
    valid = np.arange(k_max)[None, :] < k_draw[:, None]      # (agents, k_max)
    alt_cum = np.cumsum(alt, axis=0)                          # (days, agents, k_max)
    alt_cum = np.where(valid[None, :, :], alt_cum, -np.inf)
    track_self = alt_cum.max(axis=2)                          # best-of-k_i at each T

    # Out-of-sample: the next 90 days, ONE account per agent. The best-of-k
    # lottery embedded in the self-reported track does not repeat here.
    future = (skills * cfg.daily_vol * FUTURE_DAYS
              + cfg.daily_vol * np.sqrt(FUTURE_DAYS)
              * rng.standard_normal(cfg.n_agents))
    future_rank = rankdata(future)
    top_future = future_rank > 0.9 * cfg.n_agents

    out = {}
    for T in cfg.t_marks:
        row_att = rankdata(track_attested[T - 1])
        row_self = rankdata(track_self[T - 1])
        top_att = row_att > 0.9 * cfg.n_agents
        top_self = row_self > 0.9 * cfg.n_agents
        # Disappointment gap: for the observed top decile, the daily PnL the
        # reported track IMPLIES vs what those agents actually did out-of-sample.
        imp_att = (track_attested[T - 1][top_att] / T).mean()
        imp_self = (track_self[T - 1][top_self] / T).mean()
        real_att = (future[top_att] / FUTURE_DAYS).mean()
        real_self = (future[top_self] / FUTURE_DAYS).mean()
        out[T] = (
            np.corrcoef(row_att, future_rank)[0, 1],
            top_future[top_att].mean(),
            np.corrcoef(row_self, future_rank)[0, 1],
            top_future[top_self].mean(),
            imp_att / real_att if abs(real_att) > 1e-12 else np.nan,
            imp_self / real_self if abs(real_self) > 1e-12 else np.nan,
        )
    return out


def main() -> None:
    cfg = Config()
    rng = np.random.default_rng(cfg.seed)
    trials = [run_trial(rng, cfg) for _ in range(cfg.n_trials)]

    print(f"{'T(days)':>8} {'sp_att':>8} {'sp_self':>8} {'prec@10_att':>12} "
          f"{'prec@10_self':>13} {'gap_att':>8} {'gap_self':>9}")
    agg = {}
    for T in cfg.t_marks:
        cols = [np.array([t[T][j] for t in trials]) for j in range(6)]
        agg[T] = tuple(cols[:4])
        print(f"{T:>8} {cols[0].mean():>8.3f} {cols[2].mean():>8.3f} "
              f"{cols[1].mean():>11.1%} {cols[3].mean():>12.1%} "
              f"{np.nanmean(cols[4]):>8.2f} {np.nanmean(cols[5]):>9.2f}")
    print("\n(gap = implied-vs-realized daily PnL ratio for the observed top decile; "
          "1.0 is honest, >1 is over-promise)")

    Ts = list(cfg.t_marks)
    fig, (ax1, ax2) = plt.subplots(1, 2, figsize=(11, 4.2))

    for ax, idx, title, ylabel in (
        (ax1, 0, "out-of-sample predictive power", "Spearman ρ(track, next-90d PnL)"),
        (ax2, 1, "precision of the observed top decile, out-of-sample", "fraction in top decile out-of-sample"),
    ):
        att = np.array([agg[T][idx] for T in Ts])
        selfr = np.array([agg[T][idx + 2] for T in Ts])
        ax.plot(Ts, att.mean(axis=1), color="#27ae60", lw=2, label="TEE-attested PnL")
        ax.fill_between(Ts, np.percentile(att, 10, axis=1), np.percentile(att, 90, axis=1),
                        color="#27ae60", alpha=0.2)
        ax.plot(Ts, selfr.mean(axis=1), color="#c0392b", lw=2,
                label="self-reported (best of undisclosed k)")
        ax.fill_between(Ts, np.percentile(selfr, 10, axis=1),
                        np.percentile(selfr, 90, axis=1), color="#c0392b", alpha=0.2)
        ax.set_xlabel("track record length (days)")
        ax.set_ylabel(ylabel)
        ax.set_title(title)
        ax.set_xscale("log")
        ax.set_xticks(Ts)
        ax.set_xticklabels(Ts)
        ax.grid(alpha=0.3)
        ax.legend(fontsize=8, loc="lower right")

    ax2.axhline(cfg.skilled_frac, color="k", ls=":", lw=1, alpha=0.5)
    ax2.text(8, cfg.skilled_frac + 0.01, "chance level", fontsize=8)

    fig.suptitle("How fast a track record becomes evidence: attested vs self-reported", y=1.02)
    fig.tight_layout()
    fig.savefig("reputation_convergence.png", dpi=150, bbox_inches="tight")
    print("\nwrote reputation_convergence.png")


if __name__ == "__main__":
    main()
