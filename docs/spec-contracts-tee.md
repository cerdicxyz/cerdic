# Cerdic — Smart Contract Flow & TEE Code Spec

Companion to `ARCHITECTURE.md` and `paper/cerdic.tex`. Where those two describe the design
and the reasoning behind it, this is the concrete interface-level spec: exact function
signatures, message shapes, and the TEE binary's internal structure. Diagram:
`docs/cerdic-contract-tee-sequence.excalidraw`.

Fresh spec, not tied to any existing scaffolding in the repo — this is what to build against,
not a description of what's already there.

## 1. Contracts

Six contracts, one interface. Each does one job; the kernel never interprets market-specific
data, and nothing except `SettlementEngine` can mutate collateral or positions.

```solidity
/// Custody + the account's portfolio-scoped identity. `portfolioKey`, not the trader's
/// address, is what every other contract keys state on — the TEE derives it once per
/// trader and every subsequent call uses it instead of `msg.sender`.
interface IAccount {
    function portfolioKeyOf(address trader) external view returns (bytes32);
    function deposit(address asset, uint256 amount) external;
    function withdraw(address asset, uint256 amount) external;
    function collateralBalanceOf(bytes32 portfolioKey, address asset) external view returns (uint256);
}

/// Tier/haircut registry and effective-collateral valuation.
/// C_eff = Σ balance_a · (1 − haircut_a) · price_a
interface ICollateralEngine {
    function registerAsset(address asset, uint8 tier, uint16 haircutBps) external;
    function effectiveCollateral(bytes32 portfolioKey) external view returns (uint256 cEff);
}

/// Opaque position storage. Only `SettlementEngine` writes; this contract doesn't know
/// what a position *means*, only how to store and hand back the bytes a market extension
/// encoded.
interface IPositionEngine {
    function load(bytes32 portfolioKey, bytes32 marketId) external view returns (bytes memory sealedParams);
    function store(bytes32 portfolioKey, bytes32 marketId, bytes calldata sealedParams) external; // onlyAuthorizedTEE
    function clear(bytes32 portfolioKey, bytes32 marketId) external; // onlyAuthorizedTEE
}

/// The only contract that moves collateral between accounts or opens/closes positions.
/// Every entrypoint here trusts the TEE's attested output; it never recomputes PnL or
/// margin from plaintext, it only enforces its own invariants (no double-settlement, no
/// negative collateral).
interface ISettlementEngine {
    function settleMatch(
        bytes32 matchId,
        bytes32 marketId,
        bytes32 portfolioKeyA,
        int256 collateralDeltaA,
        bytes calldata sealedParamsA,
        bytes32 portfolioKeyB,
        int256 collateralDeltaB,
        bytes calldata sealedParamsB
    ) external; // onlyAuthorizedTEE

    function settleFunding(bytes32 marketId, int256 rate) external; // onlyAuthorizedTEE

    function liquidate(
        bytes32 portfolioKey,
        bytes32 marketId,
        uint256 settlementAmount,
        address keeper,
        uint256 keeperReward
    ) external; // onlyAuthorizedTEE
}

/// Portfolio margin, computed off-chain by the TEE against decrypted position data,
/// enforced on-chain as a single number check.
/// M(P) = f_S(P) + f_C(P) + f_L(P) + f_K(P)
interface IRiskMonitor {
    function marginRequirement(bytes32 portfolioKey) external view returns (uint256 mP);
    function isLiquidatable(bytes32 portfolioKey) external view returns (bool);
}

/// The trust boundary. Every `onlyAuthorizedTEE` call above checks in here first.
/// GCP and AWS each get their own verifier implementation behind this one interface, so
/// the kernel contracts never know which cloud produced the attestation.
interface IAttestationRouter {
    function isAuthorizedTEE(address signer) external view returns (bool);
    function registerAttestation(address signer, bytes calldata attestationDoc, uint8 cloud) external; // admin
    function revoke(address signer) external; // admin — pulls authorization on proof failure or key rotation
}
```

`IMarket` (implemented per instrument — FX perp first, BTC perp second) stays exactly as
described in the paper: `getPnL`, `getFunding`, `validateOpen`, `validateClose`. The kernel
never branches on which market it is.

## 2. Contract flow

### 2.1 Deposit (public, no TEE involved)

```
Trader → Account.deposit(asset, amount)
       → Account emits CollateralDeposited(trader, asset, amount)
```

Nothing private here — deposits are plain ERC-20 transfers. Privacy starts at order
submission, not at custody.

### 2.2 Open a position (two takers crossing, or taker vs. resting maker offer)

```
1. Trader encrypts { side, price, size, leverage, marketId, tp, sl } to the TEE's pubkey.
2. TEE decrypts both sides of the match inside the enclave.
3. TEE recomputes portfolio margin for both portfolioKeys against current account state
   (reads CollateralEngine.effectiveCollateral + IRiskMonitor.marginRequirement).
4. If both sides pass margin: TEE calls
     SettlementEngine.settleMatch(matchId, marketId,
       portfolioKeyA, collateralDeltaA, sealedParamsA,
       portfolioKeyB, collateralDeltaB, sealedParamsB)
5. AttestationRouter.isAuthorizedTEE(msg.sender) gates the call before anything mutates.
6. SettlementEngine writes both sealed positions via PositionEngine.store, applies both
   collateral deltas, emits only (matchId) — no side, size, or price on-chain.
```

### 2.3 Close a position

Same shape as open, one-sided: trader encrypts a close request naming the position, TEE
recomputes settlement PnL against the oracle price and current sealed params, calls
`settleMatch` with a single leg (the counterparty leg is the kernel's own collateral pool
adjustment, not a second trader).

### 2.4 Liquidation

```
1. Keeper watches only public state: portfolioKey + collateral status, never position detail.
2. Keeper asks the TEE to check a portfolioKey.
3. TEE fetches sealedParams from PositionEngine.load, decrypts in-enclave, fetches the
   oracle price, computes IRiskMonitor.isLiquidatable (portfolio-wide, not per-position).
4. If underwater: TEE calls SettlementEngine.liquidate(portfolioKey, marketId,
   settlementAmount, keeper, keeperReward).
5. Kernel marks liquidated, pays the keeper reward, emits (portfolioKey) only.
```

Liquidation is tranched (Section "Portfolio Margin" in the paper): one `liquidate` call
closes enough size to restore health, not the whole position, unless `LLTV`-equivalent
math says nothing smaller would work.

### 2.5 Market maker offers

Standing offers (`docs/spec` companion to the "Market Maker Offers" section in
`ARCHITECTURE.md`) don't debit collateral at placement — `effectiveCollateral` is checked
at match time, same as a taker order, so a maker can quote many markets against one
capital pool without pre-locking anything per quote.

## 3. TEE code spec — `cerdic-tee-matcher`

One Rust binary, two independent deployments (GCP Confidential Space primary, AWS Nitro
secondary), same code, same behavior, different attestation formats and key-custody paths.

### 3.1 Module layout

```
cerdic-tee-matcher/
├── attestation/     # produces the enclave's attestation doc on demand
│                     #   GCP:  OIDC token, signed by SEV-SNP hardware
│                     #   AWS:  COSE_Sign1 document, AWS root CA chain
├── keystore/         # X25519 decryption keypair
│                     #   GCP:  key generated in-enclave, never leaves memory
│                     #   AWS:  KMS-wrapped key, policy conditioned on PCR0
├── decrypt/           # NaCl/ECIES box-open of incoming order + offer payloads
├── book/               # in-enclave order book state — CLOB price-time priority
│                        # + resting maker offers (Section 2.5), never persisted plaintext
├── margin/              # portfolio margin recomputation: f_S + f_C + f_L + f_K
│                        # reads CollateralEngine/RiskMonitor via RPC, does not trust
│                        # any client-supplied number
├── settle/               # builds + signs settleMatch / liquidate / settleFunding calls,
│                        # submits to Arc EVM, is the ONLY module allowed to hold the
│                        # settlement signing key
└── api/                  # HTTP surface, see 3.2
```

### 3.2 HTTP API

```
GET  /pubkey        → { pubkey: base64, attestation: base64 }
                       Client fetches this before encrypting anything, verifies the
                       attestation out-of-band against the expected enclave image hash.

POST /order          Body: NaCl-box-encrypted { side, marketId, price, size, leverage,
                       tp, sl } to the enclave pubkey.
                       → { status: "resting" | "filled", matchId?: bytes32 }

POST /offer            Body: encrypted { marketId, side, tick, maxSize, expiry, group,
                       reduceOnly } — standing maker quote, see Section 2.5.
                       → { offerId: bytes32 }

POST /liquidation-check  Body: { portfolioKey } (plaintext — this is public info by design,
                       see 2.4). → { liquidatable: bool }

POST /liquidate         Body: { portfolioKey, liquidator } (plaintext, same posture as
                       /liquidation-check — a keeper supplies its own address to receive
                       liquidatorReward). Recomputes liquidatability fresh, then submits
                       SettlementEngine.liquidateSealed if still underwater.
                       → { executed: bool, txHash?: bytes32 }

GET  /health           → { status: "ok", attested: bool }
```

No endpoint ever returns decrypted order contents, matched price, or position detail —
only opaque IDs and booleans. The only plaintext that ever leaves the enclave is what's
already public on-chain after settlement (collateral deltas via the emitted event, not the
order that produced them).

### 3.3 Trust boundary

`settle/` is the only module with signing authority, and it only ever calls the six
`onlyAuthorizedTEE` entrypoints in Section 1 — it does not have a generic "call anything"
capability. If the enclave is compromised, the attacker gets order flow (a confidentiality
break), not a forged settlement (`AttestationRouter` still requires the compromised key to
be the one `registerAttestation` was called with, and the kernel's own invariant checks in
`SettlementEngine` don't disappear just because the caller is authorized).

### 3.4 ZK correctness proofs (async, decoupled from settlement)

`settle/` never blocks on a proof — attestation alone is sufficient to settle, per
`ARCHITECTURE.md`'s ZK Correctness Layer. Proof generation is a separate background job in
the same binary, triggered above a per-market notional threshold or on the same events that
trigger a portfolio margin recomputation.

```solidity
interface IZkVerifier {
    function submitMatchProof(bytes32 matchId, bytes calldata proof, bytes calldata publicInputs) external;
    function submitMarginProof(bytes32 portfolioKey, bytes calldata proof, bytes calldata publicInputs) external;
    // On verification failure: does NOT unwind the already-settled trade, instead calls
    // AttestationRouter.revoke(signer) — same fallback posture as an attestation failure.
}
```

```
cerdic-tee-matcher/
└── proof/    # generates MatchCorrectness / MarginCorrectness proofs (arkworks, Groth16/
              # BN254) in the background after settle/ has already submitted, then calls
              # IZkVerifier.submitMatchProof / submitMarginProof as a follow-up tx
```

### 3.5 Attestation lifecycle

```
Enclave boot → generate/unseal keystore → request attestation doc from hardware
            → AttestationRouter.registerAttestation(signer, doc, cloud)  [admin, once per
              deploy, not per-request]
            → enclave is now an authorized settler until revoked
```

Per-request attestation (re-attesting on every `/pubkey` call) is for the *client's*
verification, not the contract's — the contract only checks `isAuthorizedTEE(msg.sender)`,
a cheap storage read, not a fresh attestation proof on every settlement. Re-registration
happens on key rotation or redeploy, not on a timer.
