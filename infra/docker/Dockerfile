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
#
# Real, confirmed gap this label fixes: the hardened `confidential-space`
# image family rejects EVERY `tee-env-*` metadata override by default
# ("env var ... is not allowed to be overridden on this image; allowed
# envs to be overridden: []", confirmed live trying to launch this exact
# image with no label at all) — a genuine security feature, not a bug: the
# MEASURED IMAGE decides what an operator's VM metadata is allowed to
# inject, not the other way around, so a compromised/malicious VM operator
# can't silently redirect a workload's config. This allow-list is
# deliberately just the deploy-time config this binary's own main.rs
# already treats as ordinary, non-secret operator input (RPC URL, contract
# addresses, KMS/GCS coordinates) — never a workload behavior flag, and
# never anything that could substitute for the real secrets KMS/GCS
# themselves gate.
LABEL "tee.launch_policy.allow_env_override"="CERDIC_KMS_KEY_NAME,CERDIC_STATE_BUCKET,CERDIC_STATE_OBJECT,CERDIC_DB_PATH,SETTLEMENT_RPC_URL,CERDIC_ACCOUNT_CONTRACT,CERDIC_COLLATERAL_ASSET,CERDIC_RISK_MONITOR_CONTRACT,CERDIC_SETTLEMENT_CONTRACTS,CERDIC_MARKET_ID_OVERRIDES,CERDIC_ORACLE_FEEDS,CERDIC_LOG,CERDIC_HTTP_PORT"

EXPOSE 8787
CMD ["cerdic-tee-matcher"]
