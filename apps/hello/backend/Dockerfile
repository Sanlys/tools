# Build from the repo root: docker build -f apps/hello/backend/Dockerfile .
FROM rust:1-slim-bookworm AS builder
WORKDIR /app
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
COPY . .
RUN cargo build --release -p hello-backend

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/hello-backend /usr/local/bin/hello-backend
EXPOSE 8081
ENTRYPOINT ["/usr/local/bin/hello-backend"]
