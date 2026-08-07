# simulations/agentic-finance

Sims informing Cerdic's agent-native design. Each directory has a sim, its findings
in FINDINGS.md, and generated charts. Reproduce any sim: `python3 <name>_sim.py`
(deps: numpy, matplotlib).

| Sim | Question answered | Headline result |
|---|---|---|
| `drawdown_enforcement_sim.py` | Does kernel-level (pre-trade, atomic) drawdown enforcement beat web2 post-hoc monitoring? | Kernel: breach depth 0.0% (floor held). Web2: median -10.9%, p99 -35.1%. Kernel also leaks zero PNL. |
| `reputation_convergence_sim.py` | Do TEE-attested track records produce better capital allocation than self-reported ones? | Attested rank corr 0.70 vs 0.27 self-reported; weak agents hold >5% of capital in 2.8% of rounds (attested) vs 62.2% (self-reported). |

See FINDINGS.md for full analysis and design consequences.
