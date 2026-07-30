resource "google_artifact_registry_repository" "tee_match" {
  project       = var.project_id
  location      = var.region
  repository_id = var.artifact_repo_name
  format        = "DOCKER"
  description   = "cerdic-tee-matcher container images"

  # Real build iteration accumulates old tags fast; per the cost notes in
  # docs/gcp-attestation-test-report.md this was already flagged as small
  # but real ongoing storage cost. Keep the last 10 and anything tagged
  # `latest` or `prod`, clean up the rest automatically.
  cleanup_policies {
    id     = "keep-recent"
    action = "KEEP"
    most_recent_versions {
      keep_count = 10
    }
  }
  cleanup_policies {
    id     = "keep-tagged"
    action = "KEEP"
    condition {
      tag_state    = "TAGGED"
      tag_prefixes = ["latest", "prod"]
    }
  }
  cleanup_policies {
    id     = "delete-old-untagged"
    action = "DELETE"
    condition {
      tag_state  = "UNTAGGED"
      older_than = "1209600s" # 14 days
    }
  }

  depends_on = [google_project_service.required]
}

resource "google_artifact_registry_repository_iam_member" "tee_match_reader" {
  project    = var.project_id
  location   = google_artifact_registry_repository.tee_match.location
  repository = google_artifact_registry_repository.tee_match.repository_id
  role       = "roles/artifactregistry.reader"
  member     = "serviceAccount:${google_service_account.tee_match.email}"
}
