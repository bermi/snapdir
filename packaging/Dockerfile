# Slim, fully-static snapdir image — replaces the old ~8 MB Alpine+Bash+b3sum
# image. The Rust `snapdir` binary is built for x86_64-unknown-linux-musl
# (100% static, no libc, no shelling out to b3sum/aws/gcloud/b2/sqlite3), so it
# runs on `scratch`. Final image is just the stripped binary + CA certs (needed
# for HTTPS to S3/GCS/B2 via the ring rustls provider).
#
# Build context: the release workflow builds the musl binary first and passes
# it in via the `BIN` build-arg path, e.g.
#   docker build -f packaging/Dockerfile \
#     --build-arg BIN=target/x86_64-unknown-linux-musl/dist/snapdir .
#
# Lane note (packaging): this lives under packaging/ — it's the lightweight
# `BIN` build-arg variant consumed by release.yml from the prebuilt musl
# artifact. The self-contained user image (builds from source) is the repo-root
# Dockerfile.

# ---- stage: gather CA certificates from a tiny base ----
FROM alpine:3.21 AS certs
RUN apk add --no-cache ca-certificates

# ---- final: scratch + static binary + certs ----
FROM scratch

ARG BIN=target/x86_64-unknown-linux-musl/dist/snapdir
COPY --from=certs /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY ${BIN} /usr/local/bin/snapdir

LABEL org.opencontainers.image.title="snapdir" \
      org.opencontainers.image.description="Content-addressable directory snapshots (Rust). Authenticated directory snapshots." \
      org.opencontainers.image.url="https://github.com/bermi/snapdir" \
      org.opencontainers.image.source="https://github.com/bermi/snapdir" \
      org.opencontainers.image.licenses="MIT" \
      org.opencontainers.image.authors="bermi"

ENTRYPOINT ["/usr/local/bin/snapdir"]
