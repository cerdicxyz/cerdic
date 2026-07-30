#!/usr/bin/env bash
# Builds infra/docker/Dockerfile, pushes it to Artifact Registry, and rolls
# the Confidential Space VM onto the new image.
#
# Assumes infra/gcp/terraform has already been applied (KMS key, GCS
# bucket, service account, Artifact Registry repo all exist). Run from
# anywhere; paths below are resolved relative to this script.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"

PROJECT_ID="${PROJECT_ID:?set PROJECT_ID, e.g. cer-perp-tee}"
REGION="${REGION:-us-central1}"
ZONE="${ZONE:-us-central1-a}"
INSTANCE_NAME="${INSTANCE_NAME:-cerdic-tee-matcher}"
REPO_NAME="${REPO_NAME:-cerdic-tee-matcher}"
IMAGE_TAG="${IMAGE_TAG:-$(git -C "${REPO_ROOT}" rev-parse --short HEAD)}"

IMAGE_URI="${REGION}-docker.pkg.dev/${PROJECT_ID}/${REPO_NAME}/cerdic-tee-matcher:${IMAGE_TAG}"

echo "==> Building ${IMAGE_URI}"
docker build -f "${SCRIPT_DIR}/../docker/Dockerfile" -t "${IMAGE_URI}" "${REPO_ROOT}"

echo "==> Pushing ${IMAGE_URI}"
docker push "${IMAGE_URI}"

# Resolve to a digest, not just the mutable tag: Confidential Space's
# measured-boot policy should pin an exact image, and the test report
# found a real bug from trusting an assumed `:latest` push that never
# actually happened. Always verify the digest that's about to be launched
# is the one that was just pushed.
IMAGE_DIGEST="$(gcloud artifacts docker images describe "${IMAGE_URI}" \
  --format='get(image_summary.fully_qualified_digest)')"
echo "==> Resolved digest: ${IMAGE_DIGEST}"

if gcloud compute instances describe "${INSTANCE_NAME}" --zone "${ZONE}" --project "${PROJECT_ID}" &>/dev/null; then
  echo "==> Updating existing instance metadata"
  gcloud compute instances add-metadata "${INSTANCE_NAME}" \
    --project "${PROJECT_ID}" --zone "${ZONE}" \
    --metadata "tee-image-reference=${IMAGE_DIGEST}"

  # Per docs/gcp-attestation-test-report.md: `gcloud compute instances
  # reset` trips the TPM's dictionary-attack lockout counter. Use
  # stop-then-start for a clean boot onto the new metadata instead.
  echo "==> Stopping instance for a clean reboot onto the new image"
  gcloud compute instances stop "${INSTANCE_NAME}" --project "${PROJECT_ID}" --zone "${ZONE}"
  echo "==> Starting instance"
  gcloud compute instances start "${INSTANCE_NAME}" --project "${PROJECT_ID}" --zone "${ZONE}"
else
  echo "==> No existing instance found."
  echo "    Create it via Terraform: set create_vm=true and container_image=${IMAGE_DIGEST}"
  echo "    in terraform.tfvars, then \`terraform apply\` from infra/gcp/terraform."
fi

echo "==> Done. Tail logs with:"
echo "    gcloud compute instances get-serial-port-output ${INSTANCE_NAME} --project ${PROJECT_ID} --zone ${ZONE}"
