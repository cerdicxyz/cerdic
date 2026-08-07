"""Simulation: is an agent prop desk on Cerdic a viable business, and what
does kernel-enforced drawdown do to the answer?

Context: docs/agentic-finance-research.md proposes a prop-desk model for
agents on Cerdic: agents pay an evaluation fee (x402 micropayment), pass a
challenge under kernel-enforced risk limits, then trade escrowed capital in a
kernel sub-account with an automatic profit split (ERC-8183 settlement).

The web2 shape of this business (FTMO et al.) is well documented: the money
is overwhelmingly EVALUATION FEES from failed challenges, not profit splits
from funded traders — because funded blow-ups eat the split revenue. The
interesting question for Cerdic: kernel enforcement caps blow-up losses at
the limit (see drawdown_enforcement_sim.py: the crossing trade settles, the
next one reverts), which changes the shape of the loss side of the ledger.
Does the split business become a real revenue line, or is this still a
fee-collection shop?

Model:
    applicant pool    - skill mixture: a small fraction genuinely skilled,
                        most flat, some negative. Daily equity returns with
                        fat tails, post-leverage vol as in the drawdown sim.
    challenge         - 30 days, pass if: final PnL >= +8%, no day <= -5%
                        (kernel-enforced, so this floor is a hard bound),
                        running drawdown <= 10%.
    funded            - $10k sub-account. Trades until blown (10% drawdown)
                        or the 12-month horizon. Profits split agent/desk.
    desk P&L          - fees + split share of funded profits
                        - capped blow-up losses (enforced) or uncapped
                        (web2 baseline, overshoot from the drawdown sim)
                        - infra cost per funded account-year.

Two desk structures are compared:

    per_account   - each funded account is a separate escrow; the desk eats
                     each blow-up individually. The naive onchain design.
    risk_pool     - funded agents trade against ONE shared vault inside the
                     clearing kernel; winners net losers through portfolio
                     margin before settlement; splits are paid from pool P&L.
                     This is the structure Cerdic's cross-margin kernel
                     uniquely enables, and the comparison is the finding.

Swept: fraction of skilled applicants, desk structure. The numbers to watch:
desk net per cohort, and whether the split line is ever real revenue.

Run directly: `python3 prop_desk_economics_sim.py`
"""

from __future__ import annotations

from dataclasses import dataclass

import numpy as np
import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt


@dataclass
class Config:
    n_applicants: int = 4_000
    challenge_days: int = 30
    funded_days: int = 250          # 12-month funded horizon
    eval_fee: float = 100.0
    funded_capital: float = 10_000.0
    split_to_desk: float = 0.20
    infra_per_funded_year: float = 500.0   # RPC, TEE, monitoring, amortized
    target_ret: float = 0.08
    daily_floor: float = -0.05
    max_dd: float = 0.10
    daily_vol: float = 0.02
    student_t_df: float = 5.0
    # From drawdown_enforcement_sim: enforced breach costs ~ limit + ~8%
    # overshoot; web2 post-hoc breach costs a mean ~1.5x the limit on
    # blow-up-shaped accounts (revenge-trading continuation).
    enforced_overshoot: float = 1.08
    unenforced_blowup_mult: float = 1.5
    seed: int = 23


def skill_mix(rng: np.random.Generator, n: int, skilled_frac: float) -> np.ndarray:
    """Daily drift per applicant. Skilled: positive edge. Rest: flat or negative."""
    skills = np.where(
        rng.random(n) < skilled_frac,
        0.003 + 0.002 * rng.random(n),       # skilled: +0.3-0.5%/day drift
        np.where(rng.random(n) < 0.7, 0.0, -0.002),
    )
    return skills


def run_challenge(rng: np.random.Generator, cfg: Config,
                  drifts: np.ndarray) -> np.ndarray:
    n = len(drifts)
    shocks = rng.standard_t(cfg.student_t_df, size=(n, cfg.challenge_days))
    shocks /= np.sqrt(cfg.student_t_df / (cfg.student_t_df - 2))
    rets = drifts[:, None] + cfg.daily_vol * shocks
    cum = np.cumsum(rets, axis=1)
    passed = (
        (cum[:, -1] >= cfg.target_ret)
        & (rets.min(axis=1) >= cfg.daily_floor)          # hard floor (enforced)
        & ((np.maximum.accumulate(cum, axis=1) - cum).max(axis=1) <= cfg.max_dd)
    )
    return passed


def run_funded(rng: np.random.Generator, cfg: Config,
               drifts: np.ndarray) -> tuple[np.ndarray, np.ndarray]:
    """Per funded account: total PnL over the horizon, and whether it blew up.
    Blow-up: drawdown hits max_dd -> account closed, desk eats the loss."""
    n = len(drifts)
    shocks = rng.standard_t(cfg.student_t_df, size=(n, cfg.funded_days))
    shocks /= np.sqrt(cfg.student_t_df / (cfg.student_t_df - 2))
    rets = drifts[:, None] + cfg.daily_vol * shocks
    cum = np.cumsum(rets, axis=1)
    dd = np.maximum.accumulate(cum, axis=1) - cum
    blew = dd >= cfg.max_dd
    blew_up = blew.any(axis=1)

    final = cum[:, -1].copy()
    if blew_up.any():
        first = np.argmax(blew, axis=1)
        rows = np.arange(n)[blew_up]
        final[blew_up] = cum[rows[blew_up[rows]], first[blew_up]]
        # stop PnL accrual at blow-up: recompute as value at first breach
        idx = np.where(blew_up)[0]
        final[idx] = cum[idx, first[idx]]
    return final, blew_up


def desk_pnl_per_account(cfg: Config, drifts: np.ndarray, passed: np.ndarray,
                         rng: np.random.Generator) -> dict:
    """Naive structure: one escrow per funded account, desk eats each blow-up."""
    n_passed = int(passed.sum())
    fees = cfg.n_applicants * cfg.eval_fee
    if n_passed == 0:
        return {"fees": fees, "splits": 0.0, "cost": 0.0, "infra": 0.0,
                "net": fees, "n_passed": 0}
    pnl, blew_up = run_funded(rng, cfg, drifts[passed])
    gross = pnl * cfg.funded_capital
    splits = cfg.split_to_desk * np.clip(gross, 0, None).sum()
    cost = blew_up.sum() * cfg.max_dd * cfg.enforced_overshoot * cfg.funded_capital
    infra = n_passed * cfg.infra_per_funded_year
    return {"fees": fees, "splits": splits, "cost": cost, "infra": infra,
            "net": fees + splits - cost - infra, "n_passed": n_passed}


def desk_pnl_risk_pool(cfg: Config, drifts: np.ndarray, passed: np.ndarray,
                       rng: np.random.Generator) -> dict:
    """Shared-vault structure: funded agents' flow nets inside the kernel
    (portfolio margin); splits paid from pool P&L. Pool has its own 10%
    drawdown breaker, rarely hit thanks to cross-agent diversification."""
    n_passed = int(passed.sum())
    fees = cfg.n_applicants * cfg.eval_fee
    if n_passed == 0:
        return {"fees": fees, "splits": 0.0, "cost": 0.0, "infra": 0.0,
                "net": fees, "n_passed": 0}

    n = n_passed
    d = drifts[passed]
    # Correlated daily returns: common market factor + idiosyncratic t-shocks.
    mkt = rng.standard_normal(cfg.funded_days)
    idio = rng.standard_t(cfg.student_t_df, size=(n, cfg.funded_days))
    idio /= np.sqrt(cfg.student_t_df / (cfg.student_t_df - 2))
    rho = 0.2
    rets = (d[:, None]
            + cfg.daily_vol * (np.sqrt(rho) * mkt[None, :]
                               + np.sqrt(1 - rho) * idio))

    # Per-agent paths with individual blow-up stops (kernel-enforced per-agent).
    cum = np.cumsum(rets, axis=1)
    dd = np.maximum.accumulate(cum, axis=1) - cum
    blew = dd >= cfg.max_dd
    blew_up = blew.any(axis=1)
    active = np.ones((n, cfg.funded_days), dtype=bool)
    if blew_up.any():
        first = np.argmax(blew, axis=1)
        for i in np.where(blew_up)[0]:
            active[i, first[i] + 1:] = False  # stopped at breach, cost capped

    # Pool P&L: net of active agents' returns, each at funded_capital notional.
    pool_daily = (rets * active).sum(axis=0) * cfg.funded_capital
    pool_total = pool_daily.sum()

    # Agent payouts: (1 - desk split) of individual POSITIVE PnL at stop/horizon.
    final = np.where(blew.any(axis=1),
                     cum[np.arange(n), np.clip(np.argmax(blew, axis=1), 0, None)],
                     cum[:, -1])
    agent_gross = final * cfg.funded_capital
    payouts = (1 - cfg.split_to_desk) * np.clip(agent_gross, 0, None).sum()

    # Blow-up costs are the pool's problem already (netting absorbed them);
    # the desk's residual is pool_total minus payouts. Infra as before.
    infra = n * cfg.infra_per_funded_year
    net = fees + pool_total - payouts - infra
    splits_equiv = pool_total - payouts  # what the desk keeps from trading
    return {"fees": fees, "splits": splits_equiv, "cost": 0.0, "infra": infra,
            "net": net, "n_passed": n}


def main() -> None:
    cfg = Config()
    fracs = [0.02, 0.05, 0.10, 0.15, 0.20, 0.30]
    n_trials = 12

    print(f"{'skilled%':>9} {'structure':>12} {'pass%':>7} {'fees':>10} "
          f"{'trading':>11} {'cost':>11} {'infra':>9} {'net (mean)':>12}")
    curves = {}
    for structure, fn in (("per_account", desk_pnl_per_account),
                          ("risk_pool", desk_pnl_risk_pool)):
        nets, split_shares = [], []
        for f in fracs:
            trial_nets, trial_shares = [], []
            for t in range(n_trials):
                rng = np.random.default_rng(cfg.seed + t * 1_000 + int(f * 10_000))
                drifts = skill_mix(rng, cfg.n_applicants, f)
                passed = run_challenge(rng, cfg, drifts)
                r = fn(cfg, drifts, passed, rng)
                trial_nets.append(r["net"])
                rev = r["fees"] + max(r["splits"], 0.0)
                trial_shares.append(max(r["splits"], 0.0) / rev if rev else 0.0)
                if t == 0:
                    print(f"{f:>9.0%} {structure:>12} "
                          f"{r['n_passed'] / cfg.n_applicants:>7.1%} "
                          f"{r['fees']:>10,.0f} {r['splits']:>11,.0f} "
                          f"{r['cost']:>11,.0f} {r['infra']:>9,.0f} "
                          f"{np.mean(trial_nets):>12,.0f}")
            nets.append(np.mean(trial_nets))
            split_shares.append(np.mean(trial_shares))
        curves[structure] = (nets, split_shares)

    fig, (ax1, ax2) = plt.subplots(1, 2, figsize=(11, 4.2))
    ax1.plot([f * 100 for f in fracs], [n / 1e6 for n in curves["per_account"][0]],
             "o-", color="#c0392b", label="per-account escrow")
    ax1.plot([f * 100 for f in fracs], [n / 1e6 for n in curves["risk_pool"][0]],
             "o-", color="#27ae60", label="shared risk pool (netted)")
    ax1.axhline(0, color="k", lw=0.8, alpha=0.5)
    ax1.set_xlabel("skilled fraction of applicant pool (%)")
    ax1.set_ylabel("desk net P&L per 4,000 applicants ($M)")
    ax1.set_title("Desk economics vs applicant quality")
    ax1.legend(fontsize=8)
    ax1.grid(alpha=0.3)

    ax2.plot([f * 100 for f in fracs], [s * 100 for s in curves["per_account"][1]],
             "o-", color="#c0392b", label="per-account escrow")
    ax2.plot([f * 100 for f in fracs], [s * 100 for s in curves["risk_pool"][1]],
             "o-", color="#27ae60", label="shared risk pool (netted)")
    ax2.set_xlabel("skilled fraction of applicant pool (%)")
    ax2.set_ylabel("trading P&L as % of desk revenue")
    ax2.set_title("Fees vs real trading revenue")
    ax2.legend(fontsize=8)
    ax2.grid(alpha=0.3)

    fig.tight_layout()
    fig.savefig("prop_desk_economics.png", dpi=150)
    print("\nwrote prop_desk_economics.png")


if __name__ == "__main__":
    main()
