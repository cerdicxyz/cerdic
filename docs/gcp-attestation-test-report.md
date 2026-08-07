# GCP Confidential Space attestation test report

Date: 2026-07-30
Project: `cer-perp-tee`
Tester: Claude Code, on request

## Update 2026-08-04: real cerdic-tee-matcher image, project cerdic-504518

Everything below this section used a purpose-built dummy container, not the
real matcher. Retested against the actual `cerdic-tee-matcher` image
(`us-central1-docker.pkg.dev/cerdic-504518/tee-match-repo/cerdic-tee-matcher@sha256:ae99264165a47f2da973487b6a3c7110fd5d7d1dac4aaac858d54d5eeb06069e`,
built natively on Cloud Build since QEMU-emulated `docker build --platform=linux/amd64`
segfaulted compiling rustc on this Apple Silicon host) on a fresh project,
`cerdic-504518` (`cer-perp-tee` is no longer the active project).

Same `c3-standard-4` / TDX / no-external-IP recipe as below, minus a fresh
service account setup (Private Google Access, IAP SSH firewall rule,
`confidentialcomputing.workloadUser` + `artifactregistry.reader` +
`logging.logWriter` on the default compute SA, none of which existed yet on
this project). The matcher's own `/pubkey` endpoint returned a real,
non-null attestation token, `attestation.rs`'s `launcher_present()` check
found the real socket, no "running in local dev mode, unattested" log line.

Decoded claims matched exactly: `iss=https://confidentialcomputing.googleapis.com`,
`aud=cerdic-tee-matcher`, `hwmodel=GCP_INTEL_TDX`, `secboot=true`,
`tdx.gcp_attester_tcb_status=UpToDate`, and critically
`submods.container.image_digest=sha256:ae99264165a47f2da973487b6a3c7110fd5d7d1dac4aaac858d54d5eeb06069e`,
byte-for-byte the digest actually pushed. This is the first time the real
deployed image (not a stand-in) has been verified end to end under genuine
TDX hardware attestation. VM deleted immediately after the token was
captured.

## Update: Intel TDX succeeds where AMD SEV-SNP failed

The AMD SEV-SNP test below found working hardware but a hard rejection from Google's own OIDC attestation service (`UNSUPPORTED_CC_TECHNOLOGY`). Retested the same workload on Intel TDX instead, same day, same project.

**Setup**: `c3-standard-4` (the smallest TDX-capable machine type), zone `us-central1-a`, `--confidential-compute-type=TDX`, `--maintenance-policy=TERMINATE` (required, TDX machines don't support live migration), same `confidential-space-debug` image family and `cerdic-attestation-test` container used in the SEV-SNP test, no external IP, same service account.

**Result: full success.**

```
attestation through TDX quote
successfully refreshed attestation token
```

A real, Google-signed OIDC JWT came back, decoded claims include:

```
hwmodel: GCP_INTEL_TDX
iss: https://confidentialcomputing.googleapis.com
secboot: true
swname: CONFIDENTIAL_SPACE
attester_tcb: [INTEL]
tdx: { gcp_attester_tcb_status: UpToDate }
aud: cerdic-tee-matcher
sub: https://www.googleapis.com/compute/v1/projects/cer-perp-tee/zones/us-central1-a/instances/cerdic-tdx-test
```

This is exactly the artifact `ARCHITECTURE.md`'s attestation flow needs: a verifiable, Google-issued token an on-chain `GcpAttestationVerifier` could check, tying the token to a specific measured container image (`image_digest`) and instance identity. Unlike SEV-SNP, `v1.VerifyConfidentialSpace` accepts TDX evidence today, no workaround needed.

**Two operational gotchas hit along the way, worth remembering for next time:**

- `gcloud compute instances reset` on one of these VMs triggers the TPM's dictionary-attack lockout counter (`LockoutCounter` increments, the launcher logs a warning to avoid it). Use `stop` then `start` instead when changing metadata and needing a fresh boot.
- The container image used for this test had two digests in Artifact Registry from an earlier push; one was an unpullable index/manifest mismatch (`mismatched image rootfs and manifest layers`), the other was the real pullable image. Reference images by the digest that's confirmed to actually pull, not assumed from a `:latest` tag that was never pushed.

**Recommendation**: move Intel TDX to the primary attestation path for this project instead of AMD SEV-SNP, since it works today with no dependency on Google fixing anything. AWS Nitro remains a solid secondary option per `ARCHITECTURE.md`'s existing design; SEV-SNP can be revisited later if/when Google adds support, per the original recommendations below.

**Cost**: `c3-standard-4` ran for roughly 5 minutes total across two boot attempts (one failed image pull, one success), no external IP, deleted immediately after. Under $0.05 in compute at C3 on-demand pricing.

---

## Original SEV-SNP test (superseded above for the attestation-path decision, kept for record)

## What this tested

Whether a real GCP Confidential Space VM running AMD SEV-SNP can produce a genuine, Google-issued OIDC attestation token, the mechanism `ARCHITECTURE.md`'s Privacy Stack Decision names as the primary TEE deployment target.

## Setup

- **Machine**: `n2d-standard-2` (2 vCPU, 8GB), the only machine family that supports AMD SEV-SNP on GCP, and the smallest size in that family.
- **Zone**: `us-central1-c` (SEV-SNP capacity was exhausted in `us-central1-a` and `us-central1-b` at test time, `-c` succeeded).
- **Image**: `confidential-space-debug` (the debug OS family, which allows container stdout to be redirected to Cloud Logging, needed to actually see test output; production deployments should use the hardened `confidential-space` family instead).
- **Workload**: a minimal purpose-built container (Alpine + curl) that requests an OIDC attestation token from the Confidential Space launcher's local socket (`/run/container_launcher/teeserver.sock`, `POST /v1/token`), then prints the result.
- **No external IP.** Reached Artifact Registry via Private Google Access instead (see "Bug found and fixed" below).
- **Service account**: `tee-match-sa@cer-perp-tee.iam.gserviceaccount.com`, already correctly configured (`confidentialcomputing.workloadUser`, `artifactregistry.reader`, `logging.logWriter`) from earlier work in this project.

## Result: hardware attestation succeeded, OIDC token issuance did not

**Succeeded**, real SEV-SNP hardware measured boot:

```
Shielded VM integrity event, bootCounter=1:
  policyEvaluationPassed: True
  PCR_0 .. PCR_9, PCR_14 measured, matched policy
```

This is a genuine hardware attestation, the VM's early and late boot state was measured and matched the expected policy. The confidential-computing hardware layer works.

**Failed**, the actual OIDC attestation token request:

```
2026-07-30T00:15:12.454Z  attestation through TPM quote
2026-07-30T00:15:12.767Z  failed to fetch and write OIDC token: failed to retrieve
  attestation service token: calling v1.VerifyConfidentialSpace in us-central1:
  googleapi: Error 400: attestation failed:
  AMD SEV-SNP is not currently supported by Google Cloud Attestation
  error details: reason = UNSUPPORTED_CC_TECHNOLOGY
  domain = confidentialcomputing.googleapis.com
```

**Google Cloud's own attestation verification service (`v1.VerifyConfidentialSpace`) does not currently accept AMD SEV-SNP evidence**, even though the VM itself boots and runs correctly with SEV-SNP hardware protection, and even though GCP sells `n2d`/`c2d` SEV-SNP Confidential VMs as a shipping product. The hardware and the OS-level measured boot work; the specific API this design's attestation flow depends on (issuing a verifiable OIDC token an on-chain `GcpAttestationVerifier` could check) rejects SEV-SNP evidence with a hard "unsupported technology" error, not a transient failure.

This was tested in `us-central1` only. It's possible this is region-specific, or gated behind an allowlist/preview flag GCP hasn't opened to this project, not necessarily a universal platform limitation, but as tested, today, it does not work.

## Bug found and fixed along the way

The VM sat at "container-runner.service active (running)" with no container ever starting, for several minutes, no error, nothing in Cloud Logging. Root cause: the VM has no external IP (correct, more secure, and cheaper), but the project's default VPC subnet had **Private Google Access disabled**, so a no-external-IP VM had no path to Artifact Registry at all, and the image pull just hung silently forever.

**Fixed**: enabled Private Google Access on the `default` subnet in `us-central1` (`gcloud compute networks subnets update default --region=us-central1 --enable-private-ip-google-access`). Free, instant, no downside, this should stay enabled going forward, it's what makes no-external-IP confidential VMs viable at all.

## Implication for `ARCHITECTURE.md`

The Privacy Stack Decision section names GCP Confidential Space (SEV-SNP) as primary and AWS Nitro as secondary. As tested today, the primary path's attestation issuance is blocked at the API level, not by anything in this project's code or configuration. Options, roughly in order of effort:

1. **Test Intel TDX instead of AMD SEV-SNP on GCP.** Confidential Space's docs list TDX as a supported confidential-computing technology; it may not hit the same `UNSUPPORTED_CC_TECHNOLOGY` wall. Untested here.
2. **Move AWS Nitro up in priority** while GCP's SEV-SNP attestation support matures. `ARCHITECTURE.md` already designs for this as the secondary path, it may need to become the first one actually deployed.
3. **Retest periodically.** SEV-SNP Confidential VMs on GCP are a relatively recent product; the gap between "hardware works" and "attestation API accepts it" is the kind of thing that closes over time without any action needed on this project's side.
4. **Open a GCP support case** asking specifically whether SEV-SNP support for `v1.VerifyConfidentialSpace` is planned or region-gated, this is the fastest way to get a real answer instead of guessing from error messages.

## Cost of this test

Roughly 12 minutes of `n2d-standard-2` runtime, no external IP, no static resources left running. At GCP's SEV-SNP N2D pricing (~$0.0027502/vCPU-hr + ~$0.0003686/GiB-hr on-demand), this test cost well under $0.01 in compute. The VM, its disk, and the test container image were all deleted immediately after.

## GCP account cleanup done in this session

Unrelated to the attestation test itself, but found while investigating "what's eating my credits" right before this test:

- **Deleted**: `tee-match-vm` and `keepers-vm`, two `e2-standard-2` instances (not even confidential-capable machines) that had been running continuously for ~28 days, plus their disks and a reserved static IP. This was almost certainly the actual credit drain, two general-purpose VMs idling for a month is real, ongoing money regardless of what they were for.
- **Flagged, not touched**: `tee-match-repo` (Artifact Registry) has ~30 old image versions from earlier build iterations, roughly 1.4GB total. Artifact Registry storage is billed; this is a small but real ongoing cost worth pruning to a handful of recent tags when convenient.

## Best-practice settings for Confidential Compute going forward (cost)

- **Machine size**: `n2d-standard-2` is already the smallest SEV-SNP-capable size. Don't go bigger than the workload needs, confidential VMs don't have a smaller "burstable" tier the way `e2` does.
- **No external IP** on any confidential VM that only needs to reach Google APIs (Artifact Registry, Cloud Logging, the attestation service), route through Private Google Access instead (now enabled on `default`/`us-central1`). Saves the static-IP charge and reduces attack surface.
- **`RestartPolicy: Never`** for one-shot/test workloads (what this test used) so the VM's workload exits cleanly instead of looping.
- **Never leave a Confidential Space VM running as a bare dev/test instance.** They're meant to run a specific attested workload and stop, not sit idle as a general-purpose box, that's exactly the `tee-match-vm`/`keepers-vm` mistake this session found and fixed.
- **Set a budget alert on this project** (Billing > Budgets & alerts) at whatever monthly figure would be a signal something's wrong, this is the mechanism that would have caught the 28-day leak automatically instead of relying on someone noticing.
- **Prune old Artifact Registry image tags** periodically, or set a cleanup policy (Artifact Registry supports automatic deletion of untagged/old images) so build iteration doesn't quietly accumulate storage cost.
- **Use the `confidential-space` (hardened) image family, not `-debug`, for anything beyond one-off testing.** Debug allows log redirection (useful for exactly this kind of test) but is intentionally less locked-down; it's not the image a real deployment should run continuously.
