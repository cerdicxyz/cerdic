# infra

Deployment infrastructure for `cerdic-tee-matcher`, the TEE order matcher.
Everything here targets GCP Confidential Space (Intel TDX), the attestation
path proven working end-to-end in `docs/gcp-attestation-test-report.md`.
AWS Nitro remains the documented secondary path in `ARCHITECTURE.md` but has
no infra code here yet.

## Layout

```
infra/
├── docker/                    Container image for the matcher binary
│   └── Dockerfile
├── gcp/
│   ├── terraform/              KMS key, GCS state bucket, IAM bindings,
│   │                            Artifact Registry, the Confidential Space VM
│   ├── confidential-space/     Reference metadata shape the VM launches with
│   └── deploy.sh                Build, push, and (re)create the VM
└── keeper/                    Scaffold only, not built yet, see its README
```

## Why Terraform + a shell script, not just one or the other

Terraform owns everything that should be declarative and diffable: the KMS
key and its IAM policy, the GCS state bucket, the service account, the
Confidential Space VM resource itself. `deploy.sh` owns the one step that
isn't a good fit for Terraform: building and pushing a new container image
and rolling the VM onto it, which is a build pipeline action, not
infrastructure state.

## One-time setup

```bash
cd infra/gcp/terraform
terraform init
cp terraform.tfvars.example terraform.tfvars   # fill in project_id, region, zone
terraform apply
```

This provisions, matching the tested configuration in
`docs/gcp-attestation-test-report.md`:

- A Cloud KMS key (`cerdic-tee-secrets`) whose `Decrypt`/`Encrypt` IAM
  binding is scoped to the TEE's own service account only — this is what
  `crates/cerdic-tee-matcher/src/kms.rs` calls to recover `sealed_key`,
  `settlement_signer`, and `portfolio_key_secret` across a restart instead
  of regenerating them (see that module's doc comment for what breaks if
  a deployment skips this).
- A GCS bucket holding the KMS-wrapped secrets blob, private to the same
  service account.
- An Artifact Registry Docker repo for the matcher's container image.
- The service account itself, with least-privilege roles: KMS
  encrypt/decrypt on that one key, read/write on that one GCS object,
  Artifact Registry read, Confidential Space workload user, log writer.

The VM itself is deliberately **not** created by `terraform apply` in the
default plan (see `confidential-space.tf`'s `count` guard) — bring it up via
`deploy.sh` once there's an image to point it at.

## Deploying

```bash
infra/gcp/deploy.sh
```

Builds `infra/docker/Dockerfile` from the repo root, pushes it to Artifact
Registry, then stops/starts (never `reset`, see the gotcha in the test
report) the Confidential Space VM pointed at the new image digest.

## Cost and safety notes carried over from the real test run

- `c3-standard-4` is the smallest TDX-capable machine type. Don't go bigger
  than the workload needs.
- No external IP. Private Google Access must stay enabled on the subnet
  (`terraform/network.tf` asserts this, doesn't create a new network by
  default — point `subnetwork` at an existing one with it already on, or
  let Terraform enable it on `default`).
- `on_host_maintenance = "TERMINATE"` is required for TDX; it doesn't
  support live migration.
- Use the hardened `confidential-space` image family for anything beyond
  one-off testing, not `confidential-space-debug` (that one allows log
  redirection, which is useful for debugging but also means container
  stdout/stderr leaves the enclave to Cloud Logging — see the "don't log
  proof internals" fix in `api.rs`, the same reasoning applies to the
  image family choice).
- Never leave this VM running as a bare dev/test instance. It's meant to
  run one attested workload continuously, not sit idle — an earlier idle
  `tee-match-vm`/`keepers-vm` pair ran for 28 days by accident and was the
  actual source of a billing scare, see the test report.
- Set a GCP budget alert. This is the control that would have caught that
  28-day leak automatically.
