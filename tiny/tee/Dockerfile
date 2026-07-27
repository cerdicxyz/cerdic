# Tiny TEE matcher — local dev mode, Rust end to end.
#
# This Dockerfile is deliberately a plain Rust container, NOT a confidential
# container image. Running it does not give you the privacy property this
# whole design is for — it gives you the *shape* of the flow (decrypt
# off-chain, derive commitment/nullifier, seal on-chain, authorized-signer
# settlement) so it can be exercised e2e before pointing the same image at
# GCP Confidential Space or an AWS Nitro Enclave, per ARCHITECTURE.md's TEE
# Deployment section.
#
# Build from the `tiny/` directory (not `tiny/tee/`), since this crate
# path-depends on ../arkworks-prover:
#   docker build -f tee/Dockerfile -t synchra-tiny-tee .
FROM rust:1-slim-bookworm AS builder
WORKDIR /build

RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

COPY arkworks-prover ./arkworks-prover
COPY tee ./tee

WORKDIR /build/tee
# Docker Desktop's VM has a badly degraded network path to crates.io on this
# machine (connections stalling for minutes before timing out — confirmed
# not a bandwidth issue, since the identical dependency tree builds natively
# on the host in ~3 minutes). Rather than fight that, reuse the host's
# already-populated cargo registry cache as a bind-mounted build context, so
# the container never needs to reach crates.io at all.
RUN --mount=type=bind,from=host-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/tee/target \
    --mount=type=cache,target=/build/arkworks-prover/target \
    cargo build --release --offline --bin tiny-tee && \
    cp target/release/tiny-tee /tmp/tiny-tee

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /tmp/tiny-tee /usr/local/bin/tiny-tee

EXPOSE 8787
CMD ["tiny-tee"]
