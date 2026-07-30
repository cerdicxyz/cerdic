# Holds the one KMS-wrapped object kms.rs reads/writes
# (CERDIC_STATE_OBJECT, default "enclave-secrets.bin"). The object itself
# is already KMS ciphertext before it ever reaches this bucket, so bucket
# access is defense in depth, not the only thing standing between an
# attacker and the plaintext secrets.
resource "google_storage_bucket" "tee_state" {
  project                     = var.project_id
  name                        = "${var.project_id}-${var.state_bucket_suffix}"
  location                    = var.region
  uniform_bucket_level_access = true
  force_destroy               = false

  versioning {
    # A bad write must never destroy the only copy of the previous
    # wrapped secrets, that's the difference between "restart with fresh
    # secrets" (recoverable, just orphans one boot's positions) and
    # "restart with no secrets and no way back" (not recoverable).
    enabled = true
  }

  depends_on = [google_project_service.required]
}

resource "google_storage_bucket_iam_member" "tee_match_object_admin" {
  bucket = google_storage_bucket.tee_state.name
  role   = "roles/storage.objectAdmin"
  member = "serviceAccount:${google_service_account.tee_match.email}"
}
