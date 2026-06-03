# packaging handoff for docker-modernize (phase 11) @ 2026-06-03T12:56:36Z

## Summary

Made the **scratch + static-musl + CA-certs** image canonical and gave users a
working `docker build .` from a clean checkout.

- **Created a self-contained root `Dockerfile`** (multi-stage). Builder stage
  `FROM --platform=$BUILDPLATFORM rust:1.96-slim-bookworm` (pinned to the repo
  toolchain, `rust-toolchain.toml` channel `1.96.0`) builds the binary **from
  source** with `cargo build --release --locked --target <musl> -p snapdir-cli`,
  then `strip`s it. Final stage `FROM scratch` carries only the static `snapdir`
  binary + `ca-certificates.crt`. No build-args required.
  - CA certs are copied from the **builder image's** preinstalled Debian trust
    store (`/etc/ssl/certs/ca-certificates.crt`), NOT via `apk add`. They are
    needed at runtime because the workspace uses `rustls-native-certs`, which
    reads the OS bundle.
  - The root Dockerfile contains **no** `bash`, `b3sum`, or `apk add`.
  - **Key correctness fix:** the image builds the musl target **native to the
    build platform** (`ARG TARGETARCH` -> `aarch64`/`x86_64-unknown-linux-musl`).
    The original `x86_64`-hardcoded version failed on this arm64 (Apple Silicon)
    host: `ring`/`blake3` ship per-arch C/asm, and an arm64-only Debian C
    toolchain cannot assemble x86-64 SSE2 asm (`-m64` unrecognized, unknown
    mnemonics `punpckldq`/`movdqa`). Building native-arch musl makes
    `musl-tools`' `musl-gcc` assemble and statically link cleanly on any host.
- **Deleted `utils/website.dockerfile`** (dead busybox Retype-site server).
- **Bumped `packaging/Dockerfile`** certs base `alpine:3.20` -> `alpine:3.21`
  and replaced the stale lane-note comment (it claimed the root Dockerfile was
  the "frozen Bash-era oracle image" — now it's the self-contained user image).
  It remains the lightweight `BIN` build-arg variant for `release.yml`.
- **Updated `.dockerignore`** (was only `.env`) to exclude `.git` and `target/`.
  Required for `docker build .` to work at all: without it the host's 28 GB
  `target/` got shipped as build context and filled the Docker VM disk
  ("no space left on device") before any build step ran.

`release.yml`, `ci.yaml`, and all Cargo config were left untouched.

## Files changed

```
 .dockerignore            | 7 ++++++-
 Dockerfile               | <new, 64 lines>
 packaging/Dockerfile     | 8 +++++---
 utils/website.dockerfile | 6 ------ (deleted)
```

(`Dockerfile` is a new untracked file; `git diff --stat HEAD` lists the three
tracked changes — `.dockerignore`, `packaging/Dockerfile`, and the
`utils/website.dockerfile` deletion — plus the new root `Dockerfile`.)

## Local verification result

Full verification command (PM will re-run), exit 0:

```
docker build -t snapdir-phase11-test . \
  && docker run --rm snapdir-phase11-test --version \
  && ! test -e utils/website.dockerfile \
  && ! grep -riqE 'bash|b3sum|apk add' Dockerfile
```

Build tail:
```
#9  Compiling snapdir-cli v0.5.0 (/src/crates/snapdir-cli)
#9     Finished `release` profile [optimized] target(s) in 1m 31s
#9  + strip target/aarch64-unknown-linux-musl/release/snapdir
#10 [stage-1 1/2] COPY --from=builder /etc/ssl/certs/ca-certificates.crt ...
#11 [stage-1 2/2] COPY --from=builder /snapdir /usr/local/bin/snapdir
#12 naming to docker.io/library/snapdir-phase11-test  DONE
```

`docker run --rm snapdir-phase11-test --version` ->
```
snapdir 0.5.0
```
(the `version` subcommand prints the same.)

`! test -e utils/website.dockerfile` -> pass (file gone).
`! grep -riqE 'bash|b3sum|apk add' Dockerfile` -> pass (no match).

`FULL_VERIFICATION_EXIT=0`

Image: `FROM scratch`, 25.4 MB total. Extracted binary:
```
ELF 64-bit LSB executable, ARM aarch64, version 1 (SYSV),
statically linked, ... stripped
```

## Reuse check / Blockers

- Root `Dockerfile` final stage is `FROM scratch` — only the static `snapdir`
  binary + `ca-certificates.crt`. Confirmed.
- No `bash`, `b3sum`, or `apk add` in the root Dockerfile. Confirmed by grep.
- CA certs sourced from the **builder** (`COPY --from=builder
  /etc/ssl/certs/ca-certificates.crt`), Debian-preinstalled, no `apk add`.
- Static musl links with **ring / no aws-lc**: `grep -ci aws-lc Cargo.lock` = 0;
  binary is `statically linked` and runs on bare `scratch` (no libc present),
  proving the ring rustls path links statically. No aws-lc-rs break.
- `packaging/Dockerfile` still valid: rebuilt it with a dummy `BIN` artifact on
  the `alpine:3.21` certs base — image builds clean.
- `utils/website.dockerfile` is gone.

Note for PM/CI: the root Dockerfile builds the musl target **native to the build
host** (arm64 here -> `aarch64-unknown-linux-musl`; amd64 -> `x86_64`). This is
the correct portable behavior for a from-source `docker build .` and does not
affect `release.yml`, which still produces per-target artifacts via `cross` and
feeds the `x86_64` musl binary into `packaging/Dockerfile` unchanged.

Ready for PM verification: YES
