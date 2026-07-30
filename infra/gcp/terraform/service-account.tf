# The TEE's own identity. Everything else in this directory (KMS IAM
# binding, GCS bucket IAM, Confidential Space workload identity) is scoped
# to this one service account, which is the actual enforcement mechanism
# behind kms.rs's "attestation-gated key release" claim: GCP only ever
# hands out credentials for this account to a VM whose measured boot state
# and container currently match Confidential Space's attestation policy,
# so binding KMS/GCS access to this account (not to a broader principal) is
# what makes key recovery attestation-gated instead of merely
# password-protected.
resource "google_service_account" "tee_match" {
  project      = var.project_id
  account_id   = var.service_account_id
  display_name = "cerdic-tee-matcher Confidential Space workload identity"
}
