# Keeper VM — scaffold only, not built

Not implemented. This directory exists so the shape has somewhere to land
later; nothing in here is deployable yet.

## What a keeper does, per `docs/spec-contracts-tee.md` section 2.4

```
1. Keeper watches only public state: portfolioKey + collateral status, never position detail.
2. Keeper asks the TEE to check a portfolioKey (POST /liquidation-check).
3. If underwater: TEE calls SettlementEngine.liquidate(...).
```

The keeper itself never sees decrypted position data (side, leverage,
entry price, size) — that stays inside the enclave per `ARCHITECTURE.md`'s
privacy table. A keeper only needs `portfolioKey` + `collateral` from
`SettlementEngine.loadSealed`, and a way to call `/liquidation-check`. It
doesn't run inside a TEE itself; it's a plain, non-confidential worker.

## Why it's not the same VM as the matcher

The matcher's whole design is a single measured, attested workload — adding
an unrelated always-on polling loop to that image would grow its attestable
surface for no privacy benefit, since the keeper doesn't handle any secret
material. A prior incident this project already hit is relevant here: an
earlier `keepers-vm` (`e2-standard-2`, not even confidential-capable) ran
idle for 28 days and was a real, avoidable billing leak — see
`docs/gcp-attestation-test-report.md`. Whatever replaces it should default
to being easy to notice if left idle (a budget alert, a
`RestartPolicy`/liveness signal, whatever's cheapest to wire up), not repeat
that.

## Planned shape (not yet built)

- `e2-small` or similar, non-confidential, standard GCE instance —
  no TDX/SEV-SNP needed, it never touches key material.
- Polls `SettlementEngine` (or a future indexer) for `portfolioKey`s with
  non-trivial collateral, calls `POST /liquidation-check` for each, and
  submits the on-chain liquidation trigger for anything flagged.
- Its own service account, scoped to read-only chain RPC access and
  nothing else — no KMS, no GCS state bucket, no Artifact Registry beyond
  pulling its own (separate) image.
- `main.tf` here should mirror `../gcp/terraform`'s structure once this is
  actually built: its own service account, its own minimal IAM, a
  `google_compute_instance` resource. Left empty until then rather than
  half-built.
