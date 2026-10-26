# syntax=docker/dockerfile:1
# Multi-stage Rust build with cargo-chef dependency caching.
# Pattern: github.com/LukeMathWalker/cargo-chef — the `cook` layer is cached as
# long as recipe.json (manifests + lock) is unchanged, so editing source does
# NOT recompile dependencies.
#
# One Dockerfile ships any workspace binary:
#   docker build --build-arg BIN=<crate-bin> .
# selects which crate's `[[bin]]` to compile (defaults to `agent-slop`). The
# chosen binary is installed under the stable name `app`, so ENTRYPOINT never
# changes regardless of which crate was built.

# Global build arg (must be declared before the first FROM). ARGs do not cross
# stage boundaries, so each stage that uses it re-declares `ARG BIN` below.
ARG BIN=agent-slop

# The tag tracks rust-toolchain.toml `channel`; Renovate bumps both together.
FROM lukemathwalker/cargo-chef:latest-rust-1.99.1 AS chef
WORKDIR /app
# rust-toolchain.toml pins the toolchain, which rustup would otherwise resolve
# per build. Installing it here, keyed only on that file, makes it a cached layer.
COPY rust-toolchain.toml .
RUN rustup show

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
# Re-declare to pull the global default into this stage's scope.
ARG BIN
# Cached dependency layer. Only re-runs when recipe.json content changes, and
# the cache mounts additionally survive that, so a source edit recompiles only
# the touched crates.
COPY --from=planner /app/recipe.json recipe.json
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,target=/app/target,sharing=locked \
    cargo chef cook --release --recipe-path recipe.json

COPY . .
# target/ is a cache mount and therefore not part of the image, so the binary must
# be copied out inside this same RUN to survive into the runtime stage.
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,target=/app/target,sharing=locked \
    cargo build --release --bin "$BIN" \
 && cp "/app/target/release/$BIN" /usr/local/bin/app

# Minimal runtime — no Rust toolchain, non-root. trixie because the cargo-chef
# builder image links the binary against a newer glibc than bookworm ships.
FROM debian:trixie-slim AS runtime
RUN apt-get update \
  && apt-get install -y --no-install-recommends ca-certificates \
  && rm -rf /var/lib/apt/lists/* \
  && useradd --system --create-home --no-log-init app
# The selected binary was installed under the fixed name `app` to keep ENTRYPOINT stable.
COPY --from=builder /usr/local/bin/app /usr/local/bin/app
USER app
WORKDIR /app
ENTRYPOINT ["app"]
