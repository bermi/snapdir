# Self-contained user image: builds the fully-static musl `snapdir` from source
# and ships it on `scratch`. `docker build .` works from a clean checkout with no
# build-args, on either an amd64 or arm64 host.
#
# Why static musl + scratch: the whole workspace standardizes on the `ring`
# rustls crypto provider (aws-lc-rs is BANNED in deny.toml), so the binary links
# 100% statically against musl — no libc, no shelling out to external hashing,
# cloud-CLI, or database binaries. The only runtime file it needs is the CA
# bundle (for HTTPS to
# S3/GCS/B2, loaded by rustls-native-certs), copied from the Debian builder's
# preinstalled trust store.
#
# We build the musl target NATIVE to the build platform (TARGETARCH): ring and
# blake3 ship per-arch C/asm, so a native musl toolchain assembles them cleanly.
# A native-musl build needs no cross C compiler — `musl-tools` supplies the
# musl-gcc the cc-rs builds use, matched to the builder's own architecture.
#
# Lane note (packaging): this root Dockerfile is the canonical self-contained
# user image. `packaging/Dockerfile` is the lighter `BIN` build-arg variant that
# `release.yml` feeds with the prebuilt musl artifact.

# ---- stage: build the static musl binary from source ----
# Toolchain is pinned by this base image (rust 1.96).
# Build on the native platform so the musl target matches the builder arch.
FROM --platform=$BUILDPLATFORM rust:1.96-slim-bookworm AS builder
ARG TARGETARCH
RUN set -eux; \
    case "${TARGETARCH:-$(dpkg --print-architecture)}" in \
      amd64) RUST_MUSL=x86_64-unknown-linux-musl ;; \
      arm64) RUST_MUSL=aarch64-unknown-linux-musl ;; \
      *) echo "unsupported TARGETARCH=${TARGETARCH}" >&2; exit 1 ;; \
    esac; \
    echo "$RUST_MUSL" > /rust-musl-target; \
    rustup target add "$RUST_MUSL"; \
    apt-get update; \
    apt-get install -y --no-install-recommends musl-tools; \
    rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
RUN set -eux; \
    RUST_MUSL="$(cat /rust-musl-target)"; \
    cargo build --release --locked --target "$RUST_MUSL" -p snapdir-cli; \
    strip "target/${RUST_MUSL}/release/snapdir"; \
    cp "target/${RUST_MUSL}/release/snapdir" /snapdir

# ---- final: scratch + static binary + CA certs ----
FROM scratch
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=builder /snapdir /usr/local/bin/snapdir
LABEL org.opencontainers.image.title="snapdir" \
      org.opencontainers.image.description="Content-addressable directory snapshots (Rust). Authenticated directory snapshots." \
      org.opencontainers.image.url="https://github.com/bermi/snapdir" \
      org.opencontainers.image.source="https://github.com/bermi/snapdir" \
      org.opencontainers.image.licenses="MIT" \
      org.opencontainers.image.authors="bermi"
ENTRYPOINT ["/usr/local/bin/snapdir"]
