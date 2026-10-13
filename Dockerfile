# syntax=docker/dockerfile:1
# Multi-stage Rust build with cargo-chef dependency caching.
# Pattern: github.com/LukeMathWalker/cargo-chef — the `cook` layer is cached as
# long as recipe.json (manifests + lock) is unchanged, so editing source does
# NOT recompile dependencies.
#
# One Dockerfile ships any workspace binary:
#   docker build --build-arg BIN=<crate-bin> .
# selects which crate's `[[bin]]` to compile (defaults to `app`). The chosen
# binary is always installed under the stable name `app`, so ENTRYPOINT never
# changes regardless of which crate was built.

# Global build arg (must be declared before the first FROM). ARGs do not cross
# stage boundaries, so each stage that uses it re-declares `ARG BIN` below.
ARG BIN=app

FROM lukemathwalker/cargo-chef:latest-rust-1.96.0 AS chef
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
# Re-declare to pull the global default into this stage's scope.
ARG BIN
COPY --from=planner /app/recipe.json recipe.json
# Cached dependency layer (only re-runs when recipe.json changes).
RUN cargo chef cook --release --recipe-path recipe.json
COPY . .
# Alternative caching: '--mount=type=cache,target=/usr/local/cargo/registry --mount=type=cache,target=/app/target' can be added; cargo-chef already handles dep-layer caching so this is only for extra hit-rate on the final build.
RUN cargo build --release --bin "$BIN"

# Minimal runtime — no Rust toolchain, non-root.
FROM debian:bookworm-slim AS runtime
# Re-declare again so the COPY below can expand $BIN.
ARG BIN
RUN apt-get update \
  && apt-get install -y --no-install-recommends ca-certificates \
  && rm -rf /var/lib/apt/lists/* \
  && useradd --system --create-home --no-log-init app
# Install the selected binary under the fixed name `app` to keep ENTRYPOINT stable.
COPY --from=builder /app/target/release/$BIN /usr/local/bin/app
USER app
WORKDIR /app
ENTRYPOINT ["app"]
