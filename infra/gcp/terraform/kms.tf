# Backs crates/cerdic-tee-matcher/src/kms.rs's recover_or_generate:
# CERDIC_KMS_KEY_NAME should be set to
# google_kms_crypto_key.tee_secrets.id.
resource "google_kms_key_ring" "tee_match" {
  project  = var.project_id
  name     = var.kms_keyring_name
  location = var.region

  depends_on = [google_project_service.required]
}

resource "google_kms_crypto_key" "tee_secrets" {
  name     = var.kms_key_name
  key_ring = google_kms_key_ring.tee_match.id
  purpose  = "ENCRYPT_DECRYPT"

  # Rotation would break recovery (an old key version can still decrypt
  # what it wrote, but there's no reason to add that complexity until
  # this project actually needs key rotation).
  lifecycle {
    prevent_destroy = true
  }
}

# The whole point: only tee_match's own identity can wrap/unwrap this key,
# and GCP only issues that identity's credentials to a currently-attested
# Confidential Space VM. No other principal in the project, including
# project owners/editors by default IAM, gets this binding.
resource "google_kms_crypto_key_iam_member" "tee_match_can_use" {
  crypto_key_id = google_kms_crypto_key.tee_secrets.id
  role          = "roles/cloudkms.cryptoKeyEncrypterDecrypter"
  member        = "serviceAccount:${google_service_account.tee_match.email}"
}
