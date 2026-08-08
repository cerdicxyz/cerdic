# The actual TDX VM. Off by default (var.create_vm), see that variable's
# doc: brought up via deploy.sh once there's a real image to launch, not
# on every plan/apply of the surrounding infra.
resource "google_compute_instance" "tee_match" {
  count = var.create_vm ? 1 : 0

  project      = var.project_id
  name         = var.instance_name
  zone         = var.zone
  machine_type = var.machine_type

  # TDX doesn't support live migration, confirmed in the real test run.
  scheduling {
    on_host_maintenance = "TERMINATE"
    automatic_restart   = true
  }

  confidential_instance_config {
    confidential_instance_type = "TDX"
  }

  boot_disk {
    initialize_params {
      image = "confidential-space-images/${var.confidential_space_image_family}"
    }
  }

  network_interface {
    subnetwork = var.subnetwork
    # Deliberately no access_config block: no external IP. Reaches
    # Artifact Registry / KMS / GCS / the attestation service via Private
    # Google Access on the subnetwork instead, per the tested config.
  }

  service_account {
    email  = google_service_account.tee_match.email
    scopes = ["cloud-platform"]
  }

  shielded_instance_config {
    enable_secure_boot          = true
    enable_vtpm                 = true
    enable_integrity_monitoring = true
  }

  metadata = merge(
    {
      tee-image-reference         = var.container_image
      tee-restart-policy          = "Always"
      tee-container-log-redirect  = "false" # true only on confidential-space-debug for one-off debugging
      tee-env-CERDIC_KMS_KEY_NAME = google_kms_crypto_key.tee_secrets.id
      tee-env-CERDIC_STATE_BUCKET = google_storage_bucket.tee_state.name
      tee-env-CERDIC_STATE_OBJECT = "enclave-secrets.bin"
      tee-env-CERDIC_DB_PATH      = var.db_path
    },
    # Every one of these is genuinely optional (main.rs's own configure_*
    # functions log-and-skip when unset) — only set the ones you have real
    # values for. Map-shaped config joins to the "k=v,k=v" string
    # main.rs's own parser expects.
    var.settlement_rpc_url == "" ? {} : { tee-env-SETTLEMENT_RPC_URL = var.settlement_rpc_url },
    var.account_contract == "" ? {} : { tee-env-CERDIC_ACCOUNT_CONTRACT = var.account_contract },
    var.collateral_asset == "" ? {} : { tee-env-CERDIC_COLLATERAL_ASSET = var.collateral_asset },
    var.risk_monitor_contract == "" ? {} : { tee-env-CERDIC_RISK_MONITOR_CONTRACT = var.risk_monitor_contract },
    length(var.settlement_contracts) == 0 ? {} : {
      tee-env-CERDIC_SETTLEMENT_CONTRACTS = join(",", [for k, v in var.settlement_contracts : "${k}=${v}"])
    },
    length(var.market_id_overrides) == 0 ? {} : {
      tee-env-CERDIC_MARKET_ID_OVERRIDES = join(",", [for k, v in var.market_id_overrides : "${k}=${v}"])
    },
    length(var.oracle_feeds) == 0 ? {} : {
      tee-env-CERDIC_ORACLE_FEEDS = join(",", [for k, v in var.oracle_feeds : "${k}=${v}"])
    },
  )

  lifecycle {
    precondition {
      condition     = var.container_image != ""
      error_message = "container_image must be set when create_vm is true."
    }
  }

  depends_on = [
    google_kms_crypto_key_iam_member.tee_match_can_use,
    google_storage_bucket_iam_member.tee_match_object_admin,
    google_artifact_registry_repository_iam_member.tee_match_reader,
    google_project_iam_member.tee_match_workload_user,
  ]
}
