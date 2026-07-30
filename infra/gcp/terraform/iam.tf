# Roles the KMS/GCS/Artifact Registry files above already grant are
# resource-scoped IAM members, not listed again here. This file is for
# the remaining, project-scoped roles the workload needs to run at all.
resource "google_project_iam_member" "tee_match_workload_user" {
  project = var.project_id
  role    = "roles/confidentialcomputing.workloadUser"
  member  = "serviceAccount:${google_service_account.tee_match.email}"
}

resource "google_project_iam_member" "tee_match_log_writer" {
  project = var.project_id
  role    = "roles/logging.logWriter"
  member  = "serviceAccount:${google_service_account.tee_match.email}"
}
