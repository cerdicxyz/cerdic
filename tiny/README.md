# tiny — privacy MVP

A small, real, end-to-end proof of Cerdic's TEE privacy pattern. Rust end to
end (contract logic aside, which is Solidity) — no TypeScript anywhere in
this directory. Two generations, both real and tested, not just written:

- **`TinyPrivacyVault`** (v1) — sealed position params, an authorized-TEE-only
  settlement path, stripped events. Proved end-to-end against Arc Testnet
  with the TEE running inside Docker. Still stores `address => balance` and a
  plaintext `trader` per position, though — anyone reading the chain can see
  who holds how much and which address a position belongs to.
- **`TinyShieldedVault`** (v2) — same sealed-params mechanism, plus removes
  the v1 gap above: deposits are commitments (not `address => balance`),
  positions and closes reference a nullifier (not a `trader` field), and a
  close pays out to a fresh address, not the one that deposited. **Proved
  end-to-end against Arc Testnet, Rust TEE running inside Docker, unlinkability
  verified against ground truth** — the depositing address ends with 0
  balance, the payout address (which never appears anywhere before the close
  call) receives the funds. See "Confirmed run" below for the actual tx links.

**What v2 still can't hide, and no shielded pool can:** the deposit
transaction itself shows an address and (since every note is the same fixed
size) an implicit amount. That's inherent to bridging a transparent ERC20 in
on a public chain — no design hides who sent a transaction. What's hidden is
the *link* from that deposit to whatever position or withdrawal comes after.

Scoped down from the full design on purpose, both versions:

| This MVP | Full design (ARCHITECTURE.md / paper/cerdic.tex) |
|---|---|
| Single collateral asset (mock USDC) | Tiered collateral (USDC/EURC/USYC/RWA) |
| One position per trader, no market extension | `IMarket` interface, FX/BTC perpetuals |
| No portfolio margin — collateral is just locked/released | Full `M(P) = f_S+f_C+f_L+f_K` |
| `authorizedTEE` is an owner-set address | `GcpAttestationVerifier` / `NitroAttestationRegistry` |
| `PositionCommitment` / `NoteCircuit` only (well-formedness + commitment/nullifier) | Full `MatchCorrectness` / `MarginCorrectness` (async, threshold-gated) |
| Toy fixed/env mock oracle price | `OracleHub` — Pyth + Chainlink + TWAP |
| Fixed-denomination notes (v2) | Variable collateral amounts |
| Single relayer (the TEE itself submits every tx) | Permissionless relayer market — see "On relayers" below |

None of that is missing by accident — this exists to validate the privacy
mechanism specifically before building the rest around it. See
`ARCHITECTURE.md` at the repo root for how this composes into the real thing.

## Confirmed run (v2, Arc Testnet, Rust TEE in Docker)

```
trader (depositor): 0x58db44d612cb411b981a1f650ee337ee27772c8c
payout (fresh):      0x57c64c97908e4040199490321040da6b47201f82
```

1. Deposit (plain tx, trader's own wallet): [0xca07214f...](https://testnet.arcscan.app/tx/0xca07214f3cfaa15901629a325ab78b70918b67ee01da9b3d7ece5d8150304c6c)
2. Open (TEE-submitted, encrypted order + note secret never on-chain): [0x9ccae1b0...](https://testnet.arcscan.app/tx/0x9ccae1b00e0e014ff5f9d581fc13b0f5e98c26f9be18897fcfdf0451aee88168)
3. Close (TEE-submitted, pays out to the fresh address): [0x49e32bb1...](https://testnet.arcscan.app/tx/0x49e32bb157b4a96ce091969d5a3ef776f95200beae15b710c4a2bfbb014779e4)

Verified against `cast call`, not just the demo's own printout: depositor's
USDC balance is `0`, the fresh payout address holds `500000000`, and nothing
in either transaction's calldata or the `Deposited`/`PositionOpened` event
logs contains a field connecting the two addresses. `TinyShieldedVault.sol`
never stores a `trader` field at all — there's no address to leak.

## On relayers

The TEE **already acts as the relayer** for open/close — it submits both
transactions and pays their gas; the payout address never signs anything or
needs gas of its own before receiving funds. That's the real Tornado-Cash-style
relayer property (a fresh address can receive funds without first needing gas,
which is itself normally a linkability leak — "who funded this address's first
gas"), and it falls out of the existing `onlyAuthorizedTEE` design for free.

What's *not* yet decentralized: it's **one** relayer, not a market of them,
and it's the same party that already holds the note secret (it has to, to
derive the nullifier — see `note.rs`/`note_circuit.rs`). A real Tornado
deployment lets you pick *any* relayer, because the relayer doesn't need to
be trusted — the ZK proof it submits is what authorizes the withdrawal, not
the relayer's word. We're not there yet: `openPosition`/`closePosition` are
gated by `onlyAuthorizedTEE`, not by verifying a submitted proof, because
on-chain Groth16 verification isn't wired up (see "Moving the ZK layer to
real on-chain verification" below — same next step, now with an added
motivation: once the contract verifies the proof itself, *any* address can
relay a close, not just the TEE, which removes the TEE as a single point of
both cryptographic trust and correlation risk).

## What it proves

1. A trader's order (`side`, `size`) — and in v2, the note secret — is
   encrypted client-side to the TEE's public key and is **never** sent to the
   contract in plaintext.
2. The contract stores only an opaque `sealedParams` ciphertext blob plus
   whatever it needs for accounting (v1: collateral; v2: commitment/nullifier,
   no address).
3. Only the TEE's signing address can call `openPosition` / `closePosition` —
   the contract never recomputes PnL from plaintext, it trusts the attested
   (here: pre-authorized) caller.
4. On-chain events carry only IDs — no side, size, price, PnL, or (v2) address
   ever appears in a log.
5. `arkworks-prover` — real, hand-written R1CS circuits (arkworks,
   Groth16/BN254, the stack `ARCHITECTURE.md`'s ZK Correctness Layer names):
   `PositionCommitment` (v1, well-formedness) and `NoteCircuit` (v2, MiMC-5
   commitment/nullifier). `tiny-tee` calls `NoteCircuit::derive` directly as a
   library, not a subprocess — one Rust implementation, not two.

## Layout

```
tiny/
├── contracts/           Foundry project
│                        ├── TinyPrivacyVault.sol (v1) + MockUSDC.sol
│                        ├── TinyShieldedVault.sol (v2 — commitment/nullifier, no address linkage)
│                        └── Deploy.s.sol (v1), Deploy2.s.sol (v2), Fund.s.sol/Demo.s.sol (v1 Foundry-scripted flow)
├── tee/                 tiny-tee (Rust) — axum HTTP server + ethers chain calls +
│                        crypto_box/chacha20poly1305 crypto + arkworks-prover for note derivation.
│                        Local dev mode — see "Moving to a real TEE" below.
├── client/               tiny-client (Rust) — `demo` (full deposit→open→close flow)
│                        and `derive-commitment` subcommands
├── arkworks-prover/      Rust R1CS circuits (arkworks, Groth16/BN254), lib + CLI
│                        ├── circuit.rs — PositionCommitment (v1, toy hash)
│                        ├── note_circuit.rs + mimc.rs — NoteCommitment/Nullifier (v2, MiMC-5)
│                        └── main.rs — `position` / `note` (full proof) / `derive` (fast, no proof) subcommands
└── docker-compose.yml    anvil (local chain) + tee service
```

## Run it (v2 — TinyShieldedVault)

### 1. Build everything once

```bash
cd tiny/arkworks-prover && cargo build --release
cd ../tee && cargo build --release        # also builds the `keygen` binary
cd ../client && cargo build --release
```

### 2. Generate the TEE's keys

```bash
cd tiny/tee && ./target/release/keygen
```

Copy the output into `tiny/tee/.env`. Note `TEE_ADDRESS` separately — the
deploy script needs it.

### 3. Deploy

```bash
cd tiny/contracts
export DEPLOYER_PRIVATE_KEY=<a funded key>
export TEE_ADDRESS=<from step 2>
forge script script/Deploy2.s.sol --rpc-url $RPC_URL \
  --private-key $DEPLOYER_PRIVATE_KEY --broadcast
```

Fill in `VAULT_ADDRESS` / `USDC_ADDRESS` in `tiny/tee/.env` and
`tiny/client/.env` from the printed output, and `RPC_URL` in both.

### 4. Run the TEE

Locally:

```bash
cd tiny/tee && ./target/release/tiny-tee
```

Or in Docker (build from `tiny/`, not `tiny/tee/` — the crate path-depends on
`../arkworks-prover`):

```bash
cd tiny
docker build -f tee/Dockerfile -t cerdic-tiny-tee \
  --build-context host-cargo-registry="$HOME/.cargo/registry" .
docker run --env-file tee/.env -p 8787:8787 cerdic-tiny-tee
```

(The `--build-context` flag reuses your host's already-populated cargo
registry as a bind-mounted, offline dependency source — Docker Desktop's VM
networking to crates.io can be badly degraded on some machines/networks;
this sidesteps it entirely rather than fighting it with retries.)

### 5. Run the demo

```bash
cd tiny/client
export RPC_URL=... VAULT_ADDRESS=... USDC_ADDRESS=... TRADER_PRIVATE_KEY=...
./target/release/tiny-client demo --secret 424242
```

Prints the deposit, open, and close tx links, plus an explicit
depositor-vs-payout-address unlinkability check at the end.

## Run it (v1 — TinyPrivacyVault, still supported)

`Deploy.s.sol` + `Fund.s.sol` + `Demo.s.sol` (Foundry-scripted, `vm.ffi`-driven)
are unchanged and still work the same way, pointed at `tiny-client`'s Rust
binary instead of the old TS `ffi.ts`. `Fund.s.sol` and `Demo.s.sol` are
deliberately separate scripts — see the doc comment on `Fund.s.sol`:
`forge script --broadcast` defers sending its transactions until the whole
simulation finishes, but `vm.ffi` has an *immediate* real on-chain effect, so
interleaving deposit and open/close in one script means the TEE can only ever
see a *previous* run's balance.

## Generate ZK proofs standalone

Independent of the contract/TEE flow — proves the statement on its own:

```bash
cd tiny/arkworks-prover
cargo run --release -- position --side 0 --size 500000000 --entry-price 65000000000 --salt 12345
cargo run --release -- note --secret 424242
cargo run --release -- derive --secret 424242   # fast path, no Groth16 proving — what tiny-tee actually calls (as a library, not this CLI)
```

`position`/`note` run a full Groth16 round trip (setup → prove → verify) and
print `verify: VALID`. Try `position --side 5` or `--size 0` to see the
circuit's well-formedness constraints actually reject a malformed witness.

## Moving to a real TEE

This MVP runs the TEE as a plain Docker container — `tee/Dockerfile` says so
explicitly. To make the privacy property real rather than just structurally
correct:

1. Deploy the same `tee/` image into GCP Confidential Space (AMD SEV-SNP) or
   an AWS Nitro Enclave, per `ARCHITECTURE.md`'s TEE Deployment section.
2. Replace `GET /pk` with a real attestation endpoint (OIDC token for GCP,
   COSE_Sign1 doc for Nitro) and have the client verify it before trusting
   the public key, instead of trusting it unconditionally like this MVP does.
3. Move `TEE_PRIVATE_KEY` and `BOX_SECRET_KEY_B64` from plain env vars into
   enclave-generated, non-exportable key material.
4. Swap `authorizedTEE` (an owner-set address) for `GcpAttestationVerifier` /
   `NitroAttestationRegistry` gating who may call the settlement functions.

The decrypt → derive/seal → sign logic in `tee/src/main.rs` does not change.

## Moving the ZK layer to real on-chain verification

`arkworks-prover` verifies locally today; nothing on either contract checks a
proof yet, and `PositionCommitmentCircuit`'s commitment function is a toy
multiply-accumulate chain, not a cryptographic hash (see the comment atop
`circuit.rs`). `NoteCircuit` (v2) already uses a real one-way function
(MiMC-5) — see "On relayers" above for why wiring this up on-chain matters
beyond just correctness. To close the loop for real:

1. Generate a Solidity Groth16 verifier for each circuit (arkworks doesn't
   ship one for you the way SP1 does — this is genuinely more work, and the
   tradeoff named earlier for choosing arkworks over a zkVM). Typical path:
   export the verifying key and use a Groth16-Solidity-verifier generator
   (e.g. snarkjs's `zkey export solidityverifier` workflow, fed the
   arkworks-exported VK) rather than hand-writing pairing checks.
2. Add a `verifyOpenProof(...)` call gating `openPosition` on both contracts,
   matching the `AttestationRouter + ZK Verifiers` shape in `ARCHITECTURE.md`.
   For v2 specifically, this is what unlocks permissionless relaying (see
   "On relayers").
3. Have `tiny-tee` generate the proof asynchronously after submitting the
   settlement, per the async/threshold framing already in `ARCHITECTURE.md`'s
   ZK Correctness Layer section — proof generation must not sit on the
   matching hot path.
4. Replace the dev trusted setup (`ChaCha20Rng::seed_from_u64(42)` in
   `arkworks-prover/src/main.rs` — deterministic, not secure) with a real
   (ideally multi-party) ceremony before this touches real funds.
