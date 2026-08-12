# Build from the repo root: docker build -f apps/game-mgr/backend/Dockerfile .
#
# Two build stages, same split as apps/hello/backend/Dockerfile: this
# tool's own compiled wasm UI (apps/game-mgr/frontend's GameMgrPanel, built
# with trunk) and the axum backend that serves both it and the API. `trunk
# build` also runs wasm-bindgen and fetches a matching wasm-bindgen-cli
# release if needed, so this stage needs network access.

FROM rust:1-slim-bookworm AS wasm-builder
WORKDIR /app
RUN rustup target add wasm32-unknown-unknown \
    && cargo install trunk --locked
COPY . .
# Must run with cwd = the frontend crate's directory: trunk resolves the
# target crate from cwd's Cargo.toml, and the workspace root's has no
# `[package]` of its own. `--dist` needs an absolute path since it's
# resolved relative to that cwd, not the repo root.
RUN cd apps/game-mgr/frontend && trunk build --release --dist /app/dist

FROM rust:1-slim-bookworm AS backend-builder
WORKDIR /app
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
COPY . .
# Baked into `GET /api/v1/ping`'s `version` field (api::ping) so a deployed
# build can be confirmed by eye in the Settings tab instead of guessed at --
# release.yml passes --build-arg GIT_SHA=<commit> (the same sha the image
# is tagged with). Unset for a local `docker build`, in which case `ping`
# falls back to the crate's version.
ARG GIT_SHA
ENV GIT_SHA=$GIT_SHA
RUN cargo build --release -p game-mgr-backend

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=backend-builder /app/target/release/game-mgr-backend /usr/local/bin/game-mgr-backend
COPY --from=wasm-builder /app/dist /app/dist
EXPOSE 8083
ENTRYPOINT ["/usr/local/bin/game-mgr-backend"]
