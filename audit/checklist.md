# Manual Review Checklist

## CollateralEngine
- [ ] Deposit arithmetic (rounding direction)
- [ ] Withdrawal cooldown bypass
- [ ] Reentrancy on deposit / withdraw / liquidate
- [ ] Haircut application precision
- [ ] USYC yield accrual correctness

## PositionEngine
- [ ] Position open / close state transitions
- [ ] Size overflow / underflow
- [ ] Funding index update ordering
- [ ] Leverage ceiling enforcement

## RiskMonitor
- [ ] Portfolio margin formula correctness
- [ ] Signed correlation term sign
- [ ] Maintenance vs initial margin checks
- [ ] Liquidation trigger edge cases

## SettlementEngine
- [ ] Batch settlement atomicity
- [ ] Funding payment correctness
- [ ] Fee collection / distribution

## OrderBook
- [ ] Order insertion / cancellation integrity
- [ ] Price-time priority
- [ ] Self-matching prevention
- [ ] Expired order cleanup

## OracleHub
- [ ] Staleness checks
- [ ] Price deviation between sources
- [ ] Fallback ordering

## Liquidation
- [ ] Standard → backstop → ADL cascade
- [ ] Insurance fund accounting
- [ ] Liquidator reward calculation
- [ ] Partial fill handling

## TEE Matcher (crates/cerdic-tee-matcher)
- [ ] NaCl keypair generation randomness
- [ ] Order decryption: no plaintext leakage in error paths
- [ ] Settlement payload ABI encoding correctness
- [ ] Signature nonce reuse
- [ ] Attestation quote freshness / replay protection
- [ ] Sealed key storage: integrity under restart
- [ ] Nullifier derivation collision resistance
- [ ] Multi-market routing: no cross-market settlement leakage

## TEE Client (crates/cerdic-tee-matcher/src/bin)
- [ ] NaCl box sealing: correct recipient public key
- [ ] Response decryption: no man-in-the-middle
- [ ] Commitment derivation match with on-chain verification

## ZK Circuits (crates/zk-circuits)
- [ ] Witness assignment: no under-constrained signals
- [ ] Public input binding to on-chain state root
- [ ] Proof malleability (Groth16)
- [ ] Side-channel leakage in witness computation

## Cargo Workspace
- [ ] Unsafe blocks audit: no undefined behavior
- [ ] Integer overflow / truncation in financial arithmetic
- [ ] RPC client timeout / retry / nonce management
- [ ] Async task cancellation safety
- [ ] Panic-on-error in hot paths
- [ ] Dependency supply chain: cargo-deny / cargo-vet
- [ ] Thread-local state isolation between concurrent orders

## Infra
- [ ] Docker build reproducibility
- [ ] Multi-stage build: no secrets in intermediate layers
- [ ] Network exposure: only required ports
- [ ] TEE attestation verifier endpoint authentication
- [ ] CI pipeline integrity: no unsigned third-party actions
- [ ] RPC endpoint / contract address configurability (no hardcoded mainnet)
- [ ] Key management: no private keys in images or logs
