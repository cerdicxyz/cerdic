variable "project_id" {
  description = "GCP project ID. Tested against `cer-perp-tee`, see docs/gcp-attestation-test-report.md."
  type        = string
}

variable "region" {
  description = "Region for regional resources (KMS keyring, Artifact Registry, GCS bucket)."
  type        = string
  default     = "us-central1"
}

variable "zone" {
  description = "Zone for the Confidential Space VM. Must support TDX; us-central1-a confirmed working."
  type        = string
  default     = "us-central1-a"
}

variable "service_account_id" {
  description = "Account ID (not full email) for the TEE's service account."
  type        = string
  default     = "tee-match-sa"
}

variable "kms_keyring_name" {
  type    = string
  default = "cerdic-tee-keyring"
}

variable "kms_key_name" {
  type    = string
  default = "cerdic-tee-secrets"
}

variable "state_bucket_suffix" {
  description = "GCS bucket name is project_id + this suffix; bucket names are global so this needs to be reasonably unique."
  type        = string
  default     = "cerdic-tee-state"
}

variable "artifact_repo_name" {
  type    = string
  default = "cerdic-tee-matcher"
}

variable "instance_name" {
  type    = string
  default = "cerdic-tee-matcher"
}

variable "machine_type" {
  description = "Smallest TDX-capable machine type, per the tested config. Don't go bigger than the workload needs."
  type        = string
  default     = "c3-standard-4"
}

variable "confidential_space_image_family" {
  description = "Use the hardened family for anything beyond one-off testing; confidential-space-debug allows log redirection off the enclave."
  type        = string
  default     = "confidential-space"
}

variable "subnetwork" {
  description = "Self-link or name of an existing subnetwork with Private Google Access enabled. The matcher VM has no external IP, so this is required for it to reach Artifact Registry / KMS / GCS / the attestation service at all."
  type        = string
  default     = "default"
}

variable "create_vm" {
  description = "Whether `terraform apply` should create the Confidential Space VM itself. Left false by default: bring the VM up via deploy.sh once there's a real image to point it at, not on every `terraform apply` of the surrounding infra."
  type        = bool
  default     = false
}

variable "container_image" {
  description = "Full Artifact Registry image reference (with digest) the VM launches, e.g. us-central1-docker.pkg.dev/PROJECT/cerdic-tee-matcher/cerdic-tee-matcher@sha256:.... Required when create_vm is true."
  type        = string
  default     = ""
}

# --- Trading config, all optional (empty default = matcher behaves exactly
#     like an unconfigured local dev instance, see main.rs's own configure_*
#     doc for each). Fill these in once Deploy.s.sol has actually run against
#     Arc and printed real contract addresses — see docs/arc-testnet-deploy.md.

variable "settlement_rpc_url" {
  description = "Arc testnet RPC URL. Required for any settlement broadcasting, funding checkpoints, or margin attestation to happen at all."
  type        = string
  default     = ""
}

variable "account_contract" {
  description = "Account.sol address from Deploy.s.sol's output."
  type        = string
  default     = ""
}

variable "collateral_asset" {
  description = "ARC_USDC_ADDRESS — the one collateral asset the pre-trade gate checks against."
  type        = string
  default     = ""
}

variable "risk_monitor_contract" {
  description = "RiskMonitor.sol address from Deploy.s.sol's output."
  type        = string
  default     = ""
}

variable "settlement_contracts" {
  description = "marketId => SettlementEngine/FxPerpMarket contract address, e.g. { \"EURC/USDC\" = \"0x...\" }."
  type        = map(string)
  default     = {}
}

variable "market_id_overrides" {
  description = "marketId => real on-chain bytes32 id (Pyth feed id) — required on a real deployment, see api::AppState::onchain_market_id's own doc for why. Same keys as settlement_contracts."
  type        = map(string)
  default     = {}
}

variable "oracle_feeds" {
  description = "marketId => Pyth feed id for this process's own price fetching (oracle.rs) — same values as market_id_overrides today, kept as a separate map since they're conceptually different config."
  type        = map(string)
  default     = {}
}

variable "db_path" {
  description = "Where the matcher persists candle history, nonces, etc. across a process restart (persistence.rs). Boot-disk-local by default (does NOT survive a VM replace/redeploy, only a process restart within the same VM) — see this repo's docs/arc-testnet-deploy.md for the tradeoff; a real persistent-disk mount isn't wired here yet."
  type        = string
  default     = "/tmp/cerdic-state.redb"
}
