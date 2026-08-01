# Build from the repo root: docker build -f apps/{{project-name}}/backend/Dockerfile .
#
# Two build stages, same split as apps/hello/backend/Dockerfile: this
# tool's own compiled wasm UI (apps/{{project-name}}/frontend, built with
# trunk) and the axum backend that serves both it and the API. `trunk
# build` also runs wasm-bindgen and fetches a matching wasm-bindgen-cli
# release if needed, so this stage needs network access.

FROM rust:1-slim-bookworm AS wasm-builder
WORKDIR /app
RUN rustup target add wasm32-unknown-unknown \
    && cargo install trunk --locked
COPY . .
RUN cd apps/{{project-name}}/frontend && trunk build --release --dist /app/dist

FROM rust:1-slim-bookworm AS backend-builder
WORKDIR /app
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
COPY . .
RUN cargo build --release -p {{project-name}}-backend

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=backend-builder /app/target/release/{{project-name}}-backend /usr/local/bin/{{project-name}}-backend
COPY --from=wasm-builder /app/dist /app/dist
EXPOSE {{container_port}}
ENTRYPOINT ["/usr/local/bin/{{project-name}}-backend"]
