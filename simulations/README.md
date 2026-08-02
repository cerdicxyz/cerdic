# Simulations

Research-stage Python models validating kernel design decisions before they get built.
Not part of the build (no `Cargo.toml`/`package.json` here, nothing in this directory is
imported by the real crates), kept for the record of what was actually tested and why.

## backstop-maker/

Three simulations built in sequence, each answering a different question about the
synthetic backstop maker design (`crates/risk::backstop_maker`, implemented from these
findings):

1. **`backstop_maker_sim.py`** — is a kernel-owned resting quote, priced off the oracle,
   safe from manipulation? Ablates four guard configurations (naive / TWAP-only /
   TWAP+cap / fully guarded) against a simulated one-tick price-manipulation attacker.
   Naive loses a mean of $56k per run; fully guarded (TWAP pricing, notional cap, a
   fast-vs-slow last-look check) is profitable and catches 99.9% of attacks while still
   serving 99.3% of ordinary flow. Caught a real bug in the first pass: last-look
   compared against the same slow TWAP used for quoting and rejected 32% of legitimate
   flow along with the attacks, fixed by splitting it into a fast reference (last-look)
   and a slow one (quote-centering), the standard fix for that failure mode.

2. **`liquidity_bootstrap_sim.py`** — does the backstop maker actually solve the
   cold-start liquidity problem, not just survive attacks? Compares a market with no
   backstop (organic market makers only) against one with the kernel's own resting quote.
   Without it, 62% of real demand goes unserved and a real market maker never shows up
   at all in a third of runs, a genuine, sometimes-permanent deadlock, not just slow
   bootstrap. With it, 100% of demand is served from tick one and that served volume is
   the signal real market makers watch for, so they arrive fast and take over price-time
   priority once they do (backstop's fill share collapses to ~35% once a real maker is
   active).

3. **`dmm_stipend_sim.py`** — real FX/CLOB venues (EBS and similar) don't wait for
   liquidity to show up, they pay a Designated Market Maker to commit to it, with
   measurable obligations and a rebate sized to a break-even subsidy. This models that
   against the passive backstop: a DMM quoting tighter (5bp vs. the passive backstop's
   8bp) fully offset by a stipend that reimburses net losses only. Found the passive
   backstop isn't actually free either, informed order flow alone (not just attackers)
   cost it a mean $233k across runs, unbudgeted in the earlier sims. The DMM's cost is
   the same shape of loss, just paid for explicitly instead of silently absorbed by
   whatever capital backs the backstop.

Run any of them directly: `python3 backstop_maker_sim.py` (needs `numpy` + `matplotlib`).
