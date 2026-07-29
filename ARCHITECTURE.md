# Cerdic — Private, Leveraged FX Perpetuals on Arc

## System Overview

A privacy-preserving, portfolio-margined perpetuals clearing kernel for Arc, using a
**TEE-matched execution layer** wrapped in a sealed-storage contract design. Trade EURC/USDC
and BTC/USDC with leverage; collateral, position size, direction, and PnL never touch the
public chain in plaintext.

## Architectural Philosophy

- **Portfolio margin** for on-chain validity — one collateral pool backs every position, an
  account's whole position set is margined together, not position-by-position.
- **Trusted Execution Environment** for off-chain privacy — protects order and position
  inputs during matching and settlement computation.
- **Minimal trust**: client encrypts to the TEE's pubkey, the TEE attests its code, Arc EVM
  verifies the attestation before honoring a settlement.
- **Modular markets**: any instrument implementing `IMarket` shares the same kernel, risk
  engine, and TEE-matched execution path — no forked infrastructure per market.

## Privacy Stack Decision

We run our own matcher binary on infrastructure we operate, rather than depending on a
third-party TEE network (Marlin) or a third-party attestation/confidential-EVM
platform (Automata, Oasis Sapphire). Those are real, live products and worth knowing about
(see "Landscape considered" below), but the point of this build is to own the code and the
security story end to end, and we already have both GCP and AWS to deploy on.

| Layer | Technology | Purpose | Trust model |
|---|---|---|---|
| Execution privacy | Custom Rust matcher, dual-deployed on **GCP Confidential Space (AMD SEV-SNP)** and **AWS Nitro Enclaves** | Order matching without exposing order flow | Enclave attestation, two independent hardware TEE implementations |
| Position privacy | Sealed-params blob (AES-256-GCM, enclave-held key) | Hide side, leverage, entry price, size, TP/SL post-settlement | Enclave holds decryption key |
| Settlement authority | Single authorized-TEE caller pattern | Kernel trusts attested output, never recomputes from plaintext | On-chain attestation check, custom verifier per cloud |
| Key custody | AWS Nitro path: KMS key policy conditioned on enclave `PCR0` (image measurement) | Decrypt key never releasable to anything but the exact measured enclave, enforced by KMS itself, not just client-side attestation checking | AWS KMS + Nitro Attestation Document |
| Future: attestation cost compression | ZK proof of attestation validity (own circuit or borrowed verifier) | Reduce per-verification gas | Groth16 / BN254 |
| Future: solvency privacy | ZK solvency attestation (Phase 3) | Prove collateral > liabilities without disclosing either | Groth16, multi-party trusted setup |

**Landscape considered, and why we're not depending on it directly:** Automata Network
already runs a live, audited DCAP attestation verifier across several EVM chains
(~3M gas naive, further reduced with zkVM-compressed proofs) — genuinely reusable *if* we
end up on Intel TDX; Oasis Sapphire proves the "whole confidential EVM" pattern works in
production; Zama's fhEVM shows FHE-encrypted balances are real and live on Ethereum mainnet
today (`ConfidentialERC20` / ERC-7984). None of these are the execution substrate here —
we're not running on Sapphire, and our matcher isn't using Automata's verifier yet — but
each is a legitimate later option: Automata's verifier if we add an Intel TDX deployment
alongside GCP/AWS, FHE-encrypted collateral balances (Zama-style) as a Phase 2/3 layer on
top of sealed positions, not a replacement for the TEE matching engine itself (FHE compute
is still too slow for continuous order matching).

**Why TEE, not full ZK-circuit position privacy?** A comparable design (Cerida, a
Stellar-based private perp DEX) proves position validity with Groth16 circuits over
Poseidon2 commitment/nullifier chains — real cryptographic privacy for every position, no
enclave trust required. That is a stronger guarantee and a heavier build: circuit design,
trusted setup, proving infrastructure for every state transition. Cerdic's kernel already
does its most expensive computation off-chain (portfolio margin recomputation scales with a
trader's entire cross-market position set, Section below) — adding per-position ZK proving
on top compounds cost rather than removing trust. Sealed params + an authorized-TEE-settler
contract gets most of the same on-chain opacity in plain Solidity, and composes with a
ZK layer later (solvency attestation, batch verification) without requiring it up front.

## Architecture Diagram

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                              Trader / Agent                                  │
│                                                                              │
│  ┌────────────────────┐  ┌───────────────┐  ┌───────────────────────────┐  │
│  │ Wallet / Session Key│  │ Trading UI    │  │ Encryptor (NaCl / ECIES)  │  │
│  │ (sign Arc txs)      │  │ (order form)  │  │ encrypts order to         │  │
│  └─────────┬──────────┘  └───────┬───────┘  │ TEE public key             │  │
│            │                     │           └─────────────┬─────────────┘  │
└────────────┼─────────────────────┼─────────────────────────┼────────────────┘
             │                     │                         │
             ▼                     ▼                         ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│         TEE Matcher (GCP Confidential Space + AWS Nitro, self-operated)     │
│                                                                             │
│  ┌────────────────────────────────────────────────────────────────────────┐ │
│  │                        Enclave Boundary                                │ │
│  │                                                                        │ │
│  │  ┌──────────────┐  ┌──────────────┐  ┌──────────────────────┐        │ │
│  │  │ Decryptor    │  │ Attestation  │  │ Portfolio Margin      │        │ │
│  │  │ (order/       │  │ (signed      │  │ Recomputation          │        │ │
│  │  │  sealedParams)│  │  quote)      │  │ (f_S+f_C+f_L+f_K)     │        │ │
│  │  └──────┬───────┘  └──────────────┘  └──────────────────────┘        │ │
│  │         │                                                             │ │
│  │  ┌──────▼────────────────────────────────────────────────────┐        │ │
│  │  │              CLOB / RFQ Matching Engine                    │        │ │
│  │  │              (price-time priority, in-enclave state)       │        │ │
│  │  └────────────────────────────────────────────────────────────┘        │ │
│  └────────────────────────────────────────────────────────────────────────┘ │
└────────────────────────────────┬────────────────────────────────────────────┘
                                 │  attested settlement call
                                 │  (matchId, collateral deltas, sealedParams)
                                 ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                              Arc EVM                                        │
│                                                                             │
│  ┌──────────────────────┐                                                  │
│  │ Attestation Verifier  │  authorizes exactly one caller (the TEE)        │
│  └──────────┬────────────┘                                                 │
│             │                                                              │
│  ┌──────────▼─────────────────────────────────────────────────────────────┐ │
│  │                          Clearing Kernel                               │ │
│  │                                                                         │ │
│  │  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐  ┌────────────┐ │ │
│  │  │ Account /    │  │ Collateral   │  │ Position     │  │ Settlement │ │ │
│  │  │ PortfolioKey │  │ Engine       │  │ Engine       │  │ Engine     │ │ │
│  │  │ (Tier 1-4)   │  │ (sealed)     │  │ (TEE-gated)  │  │ (TEE-gated)│ │ │
│  │  └──────────────┘  └──────────────┘  └──────────────┘  └────────────┘ │ │
│  └─────────────────────────────────────────────────────────────────────────┘ │
│             │                                                              │
│  ┌──────────▼──────────┐  ┌──────────────────────┐  ┌────────────────────┐ │
│  │ FX Perp (EURC/USDC) │  │ BTC Perp (USDC)      │  │ Insurance Fund      │ │
│  └──────────────────────┘  └──────────────────────┘  └────────────────────┘ │
│                                                                             │
│  Settlement: USDC / EURC · StableFX (spot leg) · CCTP · USYC collateral    │
└─────────────────────────────────────────────────────────────────────────────┘
```

## Data Flow: Full Trade Lifecycle

### Phase 1: Opening a Position

```
Trader                          TEE Matcher                    Arc EVM
 │                                │                             │
 │  1. Request TEE pubkey +       │                             │
 │     attestation                │                             │
 │───────────────────────────────▶│                             │
 │                                │                             │
 │  2. Verify attestation         │                             │
 │     against expected image     │                             │
 │     hash (out-of-band)         │                             │
 │                                │                             │
 │  3. Encrypt order              │                             │
 │     { side, price, size,       │                             │
 │       leverage, market,        │                             │
 │       tp, sl }                 │                             │
 │───────────────────────────────▶│                             │
 │                                │                             │
 │                                │  4. Decrypt inside enclave   │
 │                                │  5. Validate against         │
 │                                │     portfolio margin         │
 │                                │     (reads account state     │
 │                                │      via portfolioKey)       │
 │                                │  6. Rest in book / match     │
 │                                │     immediately              │
 │                                │                             │
 │  7. Receive fill confirmation  │                             │
 │◀───────────────────────────────│                             │
 │                                │                             │
 │                                │  8. Submit settlement:       │
 │                                │     openPosition(            │
 │                                │       positionId,            │
 │                                │       collateral,            │
 │                                │       sealedParams)          │
 │                                │────────────────────────────▶│
 │                                │                             │  9. Verify caller
 │                                │                             │     is attested TEE
 │                                │                             │  10. Store sealed
 │                                │                             │      position, emit
 │                                │                             │      (positionId,
 │                                │                             │       collateral)
```

### Phase 2: Private Matching (Two Takers Crossing)

```
Trader A                TEE Matcher              Trader B                Arc EVM
 │  encrypted order        │                        │                       │
 │─────────────────────────▶│                        │                       │
 │                          │◀───────────────────────│  encrypted order      │
 │                          │                        │                       │
 │                          │ decrypt both, match     │                       │
 │                          │ inside enclave           │                       │
 │                          │                        │                       │
 │                          │ submit settleMatch(matchId, collateralDeltaA, collateralDeltaB) │
 │                          │─────────────────────────────────────────────────▶│
 │                          │                        │                       │  verify TEE caller,
 │                          │                        │                       │  apply deltas,
 │                          │                        │                       │  emit (matchId) only
```

### Phase 3: Liquidation

```
Keeper                          TEE Matcher                    Arc EVM
 │  1. Watchlist scan (public    │                             │
 │     collateral + status       │                             │
 │     only — no position detail)│                             │
 │                                │                             │
 │  2. Request liquidation check │                             │
 │     for positionId            │                             │
 │───────────────────────────────▶│                             │
 │                                │  3. Fetch sealedParams from  │
 │                                │     chain, decrypt in enclave│
 │                                │  4. Fetch oracle price       │
 │                                │  5. Compute solvency         │
 │                                │     (portfolio-wide, not     │
 │                                │      position-isolated)      │
 │                                │                             │
 │                                │  6. If underwater: submit    │
 │                                │     liquidate(positionId,    │
 │                                │     settlementAmount)        │
 │                                │────────────────────────────▶│
 │                                │                             │  7. Verify TEE caller,
 │                                │                             │     mark liquidated,
 │                                │                             │     pay keeper reward,
 │                                │                             │     emit (positionId) only
```

## Market Maker Offers

`OrderBook` (`packages/contracts/src/execution/`) already stores signed limit orders for the
public CLOB path. Market makers quoting into the private RFQ path need a different shape:
one maker, many standing quotes, across many markets, without pre-locking collateral against
every quote individually. Sizing capital per-quote is what makes market making on most
on-chain venues capital-inefficient — a maker quoting five FX pairs either locks 5x the
capital it actually needs, or under-quotes to stay safe.

### Offer, not order

A maker `Offer` differs from a taker's `Order` in one structural way: it doesn't debit
collateral at placement. Collateral is checked and pulled only at match time, against the
maker's live `effectiveCollateral` (Section "Portfolio Margin Model") at that moment — so a
maker can post standing quotes across every market it's willing to fill without carving out
capital per quote up front.

```
struct Offer {
    address maker;
    bytes32 marketId;
    Side side;
    uint256 tick;            // price, quantized to the market's tickSpacing
    uint128 maxSize;
    uint64 expiry;
    bytes32 group;           // 0x0 = ungrouped; see "Offer groups" below
    bool reduceOnly;         // can only close existing exposure, never open new
    address ratifier;        // optional: must approve the fill before it executes
}
```

`reduceOnly` reuses the same semantics `PositionEngine` already needs for close-side orders
(Section "Smart Contract Design") — a reduce-only offer can never increase the maker's net
exposure, only unwind it. `ratifier` is the same attestation pattern the TEE matcher uses
elsewhere in this design (Section "Attestation Verifier"): a fill is only valid if the
designated ratifier contract approves it, so a standing offer can be gated on "the TEE
attests this match was computed correctly" instead of trusting the taker's calldata blindly.

### Offer groups

A maker quoting the same book on both sides, or quoting several correlated markets, doesn't
want each `Offer` sized independently — sizing five offers at $200k each when the maker only
has $200k of real risk appetite either over-commits capital or forces conservative undersized
quotes. An offer `group` shares one consumed-size counter across every offer tagged with it:
filling any offer in the group debits the shared cap, so the maker's other offers in that
group shrink automatically instead of needing a manual resize. This is the same problem
`RiskMonitor`'s correlation term (`f_K`, Section "Portfolio Margin Model") solves for a
trader's positions, applied to a maker's outstanding quotes instead.

### Settlement rounding

`SettlementEngine` rounds any residual dust from a fill against the taker, in the maker's
favor. This isn't a maker subsidy — it closes a specific manipulation path: a taker who
can choose rounding direction on a sequence of small fills can grind a maker down over many
trades, and asymmetric rounding removes that degree of freedom. It costs the taker at most
one wei-equivalent of price precision per fill, which is negligible at any real trade size.

## TEE Deployment

One binary (`cerdic-tee-matcher`, Rust), two independent deployments. Same order-matching
and portfolio-margin logic runs on both; they attest independently and the kernel can
require agreement from either (liveness/failover) or both (defense-in-depth quorum,
Phase 1+) before honoring a settlement.

### Primary: GCP Confidential Space (AMD SEV-SNP)

The proven pattern — same shape as our earlier `cer-perp` build.

```
┌──────────────────────────────────────────────────────────────┐
│               GCP Confidential Space (AMD SEV-SNP)             │
│                                                               │
│  workload.operator.google.com/confidential-space               │
│                                                               │
│  ┌─────────────────────────────────────────────────────────┐  │
│  │  Container: cerdic-tee-matcher (Rust)                    │  │
│  │                                                          │  │
│  │  ┌────────────────────┐  ┌───────────────────────────┐  │  │
│  │  │  Decryption Key     │  │  Attestation Token         │  │  │
│  │  │  (X25519, generated │  │  (OIDC, signed by AMD       │  │  │
│  │  │   on first boot,    │  │   SEV-SNP hardware, incl.   │  │  │
│  │  │   never leaves)     │  │   container image digest)   │  │  │
│  │  └────────────────────┘  └───────────────────────────┘  │  │
│  │                                                          │  │
│  │  Memory: encrypted by AMD SEV-SNP hardware                │  │
│  │  - Order/position plaintext exists only in enclave RAM   │  │
│  │  - Hypervisor / GCP operator cannot read enclave memory   │  │
│  └──────────────────────────────────────────────────────────┘  │
└──────────────────────────────────────────────────────────────┘

Client verification flow:
  Trader ──▶ GET /attestation ──▶ returns OIDC token + pubkey
  Trader ──▶ verify token signature against Google's JWKS
  Trader ──▶ check token's image-digest claim matches expected hash
  Trader ──▶ if valid: encrypt order/params with pubkey and send

On-chain verification: RS256 JWT signature check (EVM `modexp` precompile at 0x05
handles the RSA verification), claim checks (`iss`, `aud`, image digest) done in Solidity.
```

### Secondary: AWS Nitro Enclaves

Independent hardware isolation model (hypervisor-enforced, not memory-encryption-based
like SEV-SNP) — a genuinely different failure domain, not just a second copy of the same
trust assumption. Also gives a stronger key-custody story: KMS itself, not just the client,
refuses to release the decryption key to anything but the exact measured enclave.

```
┌──────────────────────────────────────────────────────────────┐
│                    AWS Nitro Enclave (EC2)                     │
│                                                               │
│  ┌─────────────────────────────────────────────────────────┐  │
│  │  cerdic-tee-matcher (Rust, vsock-only I/O — no network,  │  │
│  │  no disk, no SSH into the enclave itself)                 │  │
│  │                                                          │  │
│  │  ┌────────────────────┐  ┌───────────────────────────┐  │  │
│  │  │  KMS-wrapped key    │  │  Nitro Attestation Doc     │  │  │
│  │  │  (decrypt call to   │  │  (COSE_Sign1, signed by    │  │  │
│  │  │   KMS includes the  │  │   AWS Nitro root CA,        │  │  │
│  │  │   attestation doc;  │  │   includes PCR0/1/2         │  │  │
│  │  │   KMS key policy    │  │   measurements)             │  │  │
│  │  │   requires PCR0     │  │                             │  │  │
│  │  │   match to decrypt) │  │                             │  │  │
│  │  └────────────────────┘  └───────────────────────────┘  │  │
│  └──────────────────────────────────────────────────────────┘  │
└──────────────────────────────────────────────────────────────┘

Client verification flow: same shape (fetch attestation doc, verify AWS root CA chain +
PCR0 measurement, then encrypt to the enclave's public key).

On-chain verification: COSE_Sign1 + X.509 chain check against AWS's Nitro root CA. Heavier
to implement in Solidity than the GCP JWT path — realistic option for MVP is verifying the
attestation document off-chain (in a small always-on relayer we also run) and posting only
the resulting authorized-signer address on-chain, gated by a short validity window that
forces re-attestation; full on-chain COSE verification is a Phase 1 hardening item, not an
MVP blocker.
```

### Quorum / failover policy

- **MVP**: either enclave may submit settlements independently (liveness first — one cloud
  provider incident shouldn't halt trading).
- **Phase 1+**: require matching output from both enclaves for settlement above a notional
  threshold — real defense-in-depth once both paths are hardened, not before.

### Local dev mode (no enclave)

```
  Trader ──▶ GET /pk ──▶ returns X25519 pubkey (no attestation)
  Trader ──▶ encrypt order with pubkey
  Trader ──▶ POST /match ──▶ server decrypts, matches, returns fill
  (inputs visible to whoever runs the dev server — never used past testnet)
```

## Smart Contract Design

### Clearing Kernel (`packages/contracts/src/clearing/`)

```
contract Account
  - deposit(asset, amount)
  - withdraw(asset, amount)
  - portfolioKeyOf(trader) → bytes32          // TEE-derived, not the trader address

contract CollateralEngine
  - registerAsset(asset, tier, haircutBps)
  - effectiveCollateral(portfolioKey) → uint256
  // Tier 1: USDC/EURC 0%, Tier 2: USYC/stUSD 2-5%, Tier 3: ETH/BTC-LST 10-20%,
  // Tier 4: RWA 15-35%. CCTP-bridged assets keep tier, +2-5% haircut.

contract PositionEngine
  - openPosition(portfolioKey, marketId, collateral, sealedParams: bytes)
    // onlyAuthorizedTEE
    // sealedParams = AES-256-GCM(side, leverage, entryPrice, size, tpPrice, slPrice)
    // Only collateral + status + marketId stored plain.
  - closePosition(positionId, settlementAmount)  // onlyAuthorizedTEE

contract SettlementEngine
  - settleMatch(matchId, collateralDeltaA, collateralDeltaB)  // onlyAuthorizedTEE
  - settleFunding(marketId, rate)                              // onlyAuthorizedTEE
  - liquidate(positionId, settlementAmount, keeperReward)       // onlyAuthorizedTEE
  // Never recomputes PnL/thresholds from plaintext — trusts the attested caller,
  // checks its own invariants (collateral bounds, no double-settlement).

contract RiskMonitor
  - marginRequirement(portfolioKey) → uint256      // M(P) = f_S+f_C+f_L+f_K
  - isLiquidatable(portfolioKey) → bool
  // Computed off-chain by the TEE against decrypted position data, enforced on-chain
  // as a single number check — the chain never sees the position set that produced it.

interface IMarket
  struct MarketPosition { marketId, size /*signed*/, entryPrice, margin, leverageCeiling }
  function getPnL(oraclePrice) → int256
  function getFunding(position, period) → int256
  function validateOpen(size, collateral) → bool
  function validateClose(position) → bool
```

### Attestation Verifier

```
contract GcpAttestationVerifier
  - init(expectedImageDigest: bytes32, googleJwksAddr: address)
  - submitAttestation(oidcToken: bytes, enclaveSigner: address)
    // Verifies RS256 signature over the OIDC token against Google's JWKS
    // (RSA verify via the modexp precompile), checks iss/aud claims and that
    // the token's image-digest claim == expectedImageDigest, then registers
    // enclaveSigner as authorized until the token's expiry.
  - authorizedUntil(signer: address) → uint256

contract NitroAttestationRegistry
  - init(expectedPcr0: bytes32, relayer: address)
  - submitAttestation(enclaveSigner: address, validUntil: uint256)
    // onlyRelayer — the relayer verified the Nitro COSE_Sign1 doc + PCR0 off-chain
    // and is itself a fixed, known address. Short validity window forces
    // periodic re-attestation instead of trusting one relay forever.
    // Full on-chain COSE/X.509 verification is a Phase 1 upgrade, not MVP-blocking.
  - authorizedUntil(signer: address) → uint256

contract AttestationRouter
  - authorizedTEE(cloud: enum{GCP, AWS}) → address
    // Kernel contracts check this before honoring a settlement call from either path.
```

Future (Phase 3, if it earns its cost): compress on-chain verification with a ZK proof of
attestation validity instead of the full JWT/COSE check per call — same idea as Automata's
zkVM-compressed DCAP path, own circuit or a borrowed verifier, not required for MVP.

### FX Perpetual Market (`packages/contracts/src/markets/`)

```
contract FxPerpMarket is IMarket
  // EURC/USDC. Funding = interest-rate differential (r_quote - r_base).
  // Spot leg of a settled trade may route through StableFX PvP settlement
  // instead of Cerdic bootstrapping its own EURC/USDC liquidity.
```

## Portfolio Margin Model

```
M(P) = f_S(P) + f_C(P) + f_L(P) + f_K(P)

f_S = scenario margin: max loss across price-shift / funding-spike / correlation
      -breakdown scenarios
f_C = concentration charge: penalizes exposure concentrated in one asset group
f_L = liquidity charge: larger for thinner markets
f_K = correlation adjustment (SIGNED sizes):
      β · Σ ρ_ij · s_i · s_j
      s_i·s_j < 0 (opposite direction, correlated) → reduces margin (real hedge)
      s_i·s_j > 0 (same direction, correlated)      → increases margin (concentrated risk)
```

Unsigned sizes here is a real bug — it grants a margin discount to same-direction bets
regardless of correlation direction. (Caught and fixed during design — see paper §5.)

MVP ships two markets (EURC/USDC, BTC/USDC) that aren't economically correlated, so
`f_C`/`f_K` are near-zero until a second correlated market exists. `f_S`/`f_L` do real work
from day one. Hedging itself needs no new code — it's `f_K` pricing an offset a trader
already opened manually. A strategy vault only adds automation on top of the same math.

## ZK Correctness Layer

**The gap this closes:** the TEE gives confidentiality and code-identity (attestation
proves *which binary ran*), but nothing on-chain today checks that the binary's output was
actually correct — the kernel just trusts the attested caller's numbers. A compromised
enclave, or bad code that somehow got attested, could hand the kernel a wrong match price
or a wrong margin number and nothing would catch it. Two small, targeted circuits close
this without redoing the full commitment/nullifier privacy stack `cer-perp` built (that
problem is already handled here by sealed params + TEE, Section above) — these are purely
about *correctness*, not confidentiality. Simple versions, not the full constraint-count
depth of a production circuit:

```
MatchCorrectness (Rust, arkworks R1CS, Groth16/BN254 — same stack as cer-perp)

  Private:  side_a, price_a, size_a, side_b, price_b, size_b
  Public:   cmt_a, cmt_b, matchPrice, matchSize

  Constrains:
    1. side_a + side_b == 1                          (opposite sides)
    2. cmt_a == H(side_a, price_a, size_a)            (commitment matches, per side)
    3. matchPrice crosses both limits                 (within [price_a, price_b] bound)
    4. matchSize <= size_a  AND  matchSize <= size_b   (no over-fill)

  ~10-15 constraints if H is a single hash call and comparisons use simple range
  checks — deliberately not handling stop orders, TIF, or partial-fill chains yet.
```

```
MarginCorrectness (same stack)

  Private:  the account's real position set {size_i, entryPrice_i, marketId_i}
  Public:   portfolioKey, M_claimed

  Constrains:
    1. f_K == β · Σ ρ_ij · s_i · s_j                  (straight multiply-accumulate,
                                                         cheap — no comparisons needed)
    2. f_S == max over a FIXED small scenario set      (parallel shifts only for the
       (not the full dynamic set in the Portfolio Margin Model section)     simple version — drop funding-
                                                         spike/correlation-breakdown
                                                         scenarios for v1)
    3. M_claimed == f_S + f_C + f_L + f_K
    4. M_claimed exposed publicly; RiskMonitor checks it against C_eff on-chain instead
       of trusting a number the TEE merely asserts

  f_C and f_L can stay TEE-asserted-only for the simple version (the Portfolio Margin Model section already
  notes they're near-zero with two uncorrelated MVP markets) — proving f_S and f_K
  correct is where the real risk (mispriced hedges, wrong worst-case loss) lives.
```

This is a Phase 1 item (closes a real correctness gap early), not a Phase 3
someday-footnote — but it must not sit on the matching hot path. TEE matching is chosen
specifically because it runs at near-native speed (single-digit-percent overhead), which is
the entire reason it's viable for continuous order matching where FHE and full ZK matching
are not. Groth16 proof generation does not share that property: `cer-perp`'s own numbers
for a comparable circuit set were ~5-15s (UltraHonk) and ~1-5min (RISC Zero Groth16) per
proof — anywhere from three to five orders of magnitude slower than the matching itself.
Requiring a proof inline, per match, would silently throw away the latency advantage TEE
matching was chosen for. So proof generation is **asynchronous and threshold-gated**, not
inline:

```
Settlement path (every match):
  TEE decrypts, matches, computes settlement → submits attested settlement immediately
  → kernel applies it on the strength of the attestation alone, same as today

Proof path (async, decoupled from the match):
  - Below a per-market notional threshold: attestation alone is sufficient, no proof
    generated — retail-size flow settles at TEE speed, unchanged.
  - Above the threshold, or for the periodic margin-relevant events that already trigger
    M(P) recomputation (Portfolio Margin Model section): the enclave generates the
    MatchCorrectness / MarginCorrectness proof in the background and submits it as a
    follow-up transaction, verified on-chain after the fact.
  - If a proof ever fails to verify for an already-settled trade: the settlement doesn't
    unwind (funds already moved), but it flags the TEE instance and pulls its
    authorization — the same fallback posture as an attestation failure elsewhere in this
    design, not a new failure mode.
```

The threshold and the async delay are both tunable — the point is that ZK correctness is a
guarantee that arrives shortly *after* a trade, the way settlement finality or a block
confirmation does, not a gate the trade has to wait behind. Small/high-frequency flow gets
TEE-speed settlement with no proof at all; large flow gets the same fast settlement plus a
correctness guarantee that lands a few seconds to minutes later.

## Project Structure

```
├── paper/
│   ├── cerdic.tex                # formal spec — kernel, margin proof sketch, security model
│   └── cerdic-propdesk.tex       # prop-desk brief
│
├── docs/
│   └── *.excalidraw, overflow.png  # architecture + flow diagrams (editable sources)
│
├── packages/
│   ├── contracts/                # Foundry / Solidity, Arc EVM
│   │   └── src/
│   │       ├── clearing/         # Account, CollateralEngine, PositionEngine,
│   │       │                     # SettlementEngine, RiskMonitor, LiquidationEntry
│   │       ├── markets/          # BtcPerpMarket (built), FxPerpMarket (TODO)
│   │       ├── oracle/           # OracleHub, PythConsumer, ChainlinkConsumer,
│   │       │                     # MarketImpactTwap
│   │       ├── execution/        # OrderBook (public CLOB) + Market Maker Offers (TODO)
│   │       ├── lib/              # ProtocolConstants, RingBuffer
│   │       ├── privacy/          # GcpAttestationVerifier, NitroAttestationRegistry,
│   │       │                     # AttestationRouter (not yet scaffolded)
│   │       └── agents/           # Agent identity + capability tokens (not yet scaffolded)
│   │
│   └── shared/                   # TS types/constants (no frontend consumer yet — see `app/`)
│
├── app/                           # Vite + React frontend
│
├── crates/                        # Rust workspace — off-chain matching/risk/TEE-matcher
│   ├── clob/                     # off-chain matching engine (built, ~1.2k lines)
│   ├── risk/                     # margin engine — isolated only today, needs
│   │                             # f_S/f_C/f_L/f_K upgrade
│   ├── cerdic-tee-matcher/       # the TEE binary — see docs/spec-contracts-tee.md
│   └── common/                   # shared types
│
├── infra/                         # deployment/infra config — Docker, TEE deploy scripts,
│                                   # IaC (empty — TODO)
│
├── mobile/                        # mobile client (empty — TODO)
│
├── tiny/                          # standalone MVP: TEE-shielded vault, unlinkable positions —
│                                   # see tiny/README.md, not wired into packages/
│
└── ARCHITECTURE.md                # this file
```

## Privacy Model

| Component | Sees | Does not see |
|---|---|---|
| **Arc EVM** | `positionId`, `matchId`, `collateral` amount, `status`, encrypted `sealedParams` blob | side, leverage, entry price, size, TP/SL, PnL, which account owns a `portfolioKey` |
| **TEE enclave** | decrypted order/position params (inside hardware-isolated memory) | private keys (never leave the trader's wallet), other enclaves' state |
| **TEE operator (us — GCP / AWS)** | encrypted ciphertext, attestation logs | decrypted order/position data, enclave's decryption key |
| **Trader** | their own positions and trades | other traders' positions, trades, or portfolio composition |
| **Keeper** | `positionId`, `collateral`, `status` (enough to know *something* may be liquidatable) | why — no entry price, size, or PnL until the TEE itself submits a liquidation |
| **Regulator (Phase 3)** | selective-disclosure solvency proof | non-disclosed positions |

## Security Properties

1. **Settlement authority**: only a currently-attested TEE (GCP or AWS path) may call
   kernel settlement/liquidation/TP-SL functions — enforced by `AttestationRouter`.
2. **No plaintext re-derivation on-chain**: the kernel never recomputes PnL or checks a
   threshold against a plaintext field; it trusts the TEE's attested output and checks its
   own invariants (collateral bounds, no double-settlement).
3. **Fallback, not failure**: if attestation verification fails, the system falls back to
   public settlement rather than freezing trader funds.
4. **Event minimality**: no event emits price, size, side, or PnL — only opaque IDs
   (Section: On-chain footprint).
5. **Portfolio unlinkability**: cross-margined positions are grouped by a TEE-derived
   `portfolioKey` hash, not the trader's address.
6. **Two independent hardware trust domains**: GCP (AMD SEV-SNP, memory-encryption-based)
   and AWS (Nitro, hypervisor-isolation-based) are different TEE technologies on different
   clouds — a single-vendor or single-cloud compromise doesn't take down the other path.
7. **Key custody enforced outside the client, not just by it**: on the AWS path, KMS's key
   policy itself refuses to release the decryption key to anything but the exact measured
   enclave (`PCR0` match) — the guarantee doesn't depend solely on a trader correctly
   verifying an attestation before trusting it.
8. **Oracle redundancy**: Pyth (primary) + Chainlink (staleness check) + CLOB TWAP
   (circuit-breaker reference); >δ divergence pauses the affected market.
9. **Liquidation cascades**: tranched liquidation + time-in-distress ordering + a
   volume-based circuit breaker, not all-at-once forced closure.
10. **Attestation cost compression (Phase 3, optional)**: ZK-compressed attestation
    verification if per-call JWT/COSE checks prove too expensive at volume — defense-in-depth
    over the TEE, not a replacement for it.
11. **TEE compromise blast radius**: leaks order/position flow to whoever broke the enclave —
    does not let anyone forge a settlement, since kernel correctness never depends on the
    enclave, only confidentiality does.

## Build Milestones

**MVP — every item below real and working end-to-end, not a stub:**
- Clearing kernel (mostly built) — `Account`, `CollateralEngine`, `PositionEngine`,
  `SettlementEngine`, `RiskMonitor`, `LiquidationEntry`.
- Portfolio margin upgrade: `f_S+f_C+f_L+f_K` replacing the current isolated-margin-only
  `RiskMonitor`.
- `FxPerpMarket` (EURC/USDC) alongside the existing `BtcPerpMarket`.
- `cerdic-tee-matcher` (Rust), deployed to GCP Confidential Space (primary) implementing
  the sealed-params / authorized-settler / stripped-events design above, replacing the
  `rfq-matcher` stub.
- `GcpAttestationVerifier` contract + public-settlement fallback path.
- Agent accounts: identity, capability tokens, session keys, one working agent type
  (trading agent), Nanopayments-integrated.
- Frontend (`app/`, Vite + React), Arc Testnet deployment.

**Phase 1 — hardening**: AWS Nitro second deployment (`NitroAttestationRegistry`, full
on-chain COSE/X.509 verification replacing the interim relayer); dual-cloud quorum for
settlements above a notional threshold; formal verification of kernel + margin invariants;
wider `f_S` scenario coverage; additional crypto markets; dynamic haircuts; mainnet.

**Phase 2 — more markets**: additional FX pairs; RWA/rate module; strategy vaults; internal
repo market; CCTP v2. Deferred, not cut — needs a second correlated market and a working
agent system to build on.

**Phase 3 — scale**: ZK-compressed attestation verification (own circuit, if per-call cost
warrants it at volume); ZK solvency attestation; institutional sub-accounts; agent strategy
vaults; migration path to Arc's protocol-level privacy roadmap (APS) if/when it ships.

## Key References

- [Circle Arc](https://circle.com/arc)
- [Circle StableFX](https://www.circle.com/blog/introducing-circle-stablefx-and-circle-partner-stablecoins)
- [Ostium](https://www.ostium.com) — closest live precedent for leveraged onchain FX/RWA
- [GCP Confidential Space](https://cloud.google.com/confidential-computing/confidential-space/docs) — primary TEE deployment target, AMD SEV-SNP + OIDC attestation
- [AWS Nitro Enclaves](https://docs.aws.amazon.com/enclaves/latest/user/nitro-enclave.html) — secondary TEE deployment, hypervisor-isolation + KMS-enforced key custody
- [Automata DCAP Attestation](https://github.com/automata-network/automata-dcap-attestation) — reference for on-chain attestation verification patterns and gas costs, not currently depended on
- [Marlin Kalypso](https://marlin.org/kalypso) — ZK-compressed TEE attestation verification, reference for a possible Phase 3 optimization
- [Messari — TEE: A Privacy Engine for Institutional Onchain Markets](https://messari.io/report/tee-a-privacy-engine-for-institutional-onchain-markets)
- `paper/cerdic.tex` — formal specification, portfolio margin derivation, security analysis
- `/Users/mac/work/cer-perp` — prior private-perp-DEX build (Stellar/Soroban, dual
  Noir+RISC0 ZK stack); source of the sealed-params, authorized-settler, and
  stripped-events patterns adapted here
