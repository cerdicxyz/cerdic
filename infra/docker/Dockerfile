# cerdic-tee-matcher — Confidential Space container image.
#
# Build from the repo root (not this directory), since the crate is part
# of the `crates/` workspace:
#   docker build -f infra/docker/Dockerfile -t cerdic-tee-matcher .
#
# This produces a real container meant to run inside GCP Confidential Space
# (Intel TDX), per `docs/gcp-attestation-test-report.md`. Unlike
# `tiny/tee/Dockerfile` (an earlier, deliberately non-confidential prototype
# image), the privacy property this whole design is for only holds once
# this image is running as the measured workload inside an actual
# Confidential Space VM, not just running the same binary anywhere.

FROM rust:1-slim-bookworm AS builder
WORKDIR /build

RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libssl-dev ca-certificates && \
    rm -rf /var/lib/apt/lists/*

COPY crates ./crates

WORKDIR /build/crates
RUN cargo build --release --bin cerdic-tee-matcher && \
    cp target/release/cerdic-tee-matcher /tmp/cerdic-tee-matcher

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/*
COPY --from=builder /tmp/cerdic-tee-matcher /usr/local/bin/cerdic-tee-matcher

# Confidential Space injects CERDIC_KMS_KEY_NAME / CERDIC_STATE_BUCKET /
# CERDIC_STATE_OBJECT via `tee-env-*` metadata keys, see
# `infra/gcp/confidential-space/workload-metadata.yaml`. Outside
# Confidential Space (local `docker run`) they're simply unset and
# `kms::recover_or_generate` falls back to ephemeral secrets.
EXPOSE 8787
CMD ["cerdic-tee-matcher"]
