output "service_account_email" {
  value = google_service_account.tee_match.email
}

output "kms_key_name" {
  description = "Set as CERDIC_KMS_KEY_NAME on the workload."
  value       = google_kms_crypto_key.tee_secrets.id
}

output "state_bucket_name" {
  description = "Set as CERDIC_STATE_BUCKET on the workload."
  value       = google_storage_bucket.tee_state.name
}

output "artifact_registry_repo" {
  value = "${var.region}-docker.pkg.dev/${var.project_id}/${google_artifact_registry_repository.tee_match.repository_id}"
}
