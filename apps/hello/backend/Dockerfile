# Build from the repo root: docker build -f apps/hello/backend/Dockerfile .
#
# Two build stages, same split as apps/portal/backend/Dockerfile: this
# tool's own compiled wasm UI (apps/hello/frontend, built with trunk) and
# the axum backend that serves both it and the API. `trunk build` also runs
# wasm-bindgen and fetches a matching wasm-bindgen-cli release if needed, so
# this stage needs network access.

FROM rust:1-slim-bookworm AS wasm-builder
WORKDIR /app
RUN rustup target add wasm32-unknown-unknown \
    && cargo install trunk --locked
COPY . .
# Must run with cwd = the frontend crate's directory: trunk resolves the
# target crate from cwd's Cargo.toml, and the workspace root's has no
# `[package]` of its own. `--dist` needs an absolute path since it's
# resolved relative to that cwd, not the repo root.
#
# Retried: trunk's first-run tool fetch (wasm-bindgen-cli/wasm-opt from
# GitHub Releases) has turned out to be a genuinely flaky download in CI --
# see apps/game-mgr/backend/Dockerfile's identical comment, where this
# actually blocked a real deploy. Safe to retry: a failed attempt doesn't
# leave `/app/dist` partially written, it errors out before producing
# output.
RUN cd apps/hello/frontend && \
    for i in 1 2 3 4 5; do \
        trunk build --release --dist /app/dist && exit 0; \
        echo "trunk build failed (attempt $i/5, likely a flaky GitHub Releases download) -- retrying in 10s" >&2; \
        sleep 10; \
    done; \
    exit 1

FROM rust:1-slim-bookworm AS backend-builder
WORKDIR /app
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
COPY . .
RUN cargo build --release -p hello-backend

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=backend-builder /app/target/release/hello-backend /usr/local/bin/hello-backend
COPY --from=wasm-builder /app/dist /app/dist
EXPOSE 8081
ENTRYPOINT ["/usr/local/bin/hello-backend"]
