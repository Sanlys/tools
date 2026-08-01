# Build from the repo root: docker build -f apps/{{project-name}}/backend/Dockerfile .
FROM rust:1-slim-bookworm AS builder
WORKDIR /app
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
COPY . .
RUN cargo build --release -p {{project-name}}-backend

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/{{project-name}}-backend /usr/local/bin/{{project-name}}-backend
EXPOSE {{container_port}}
ENTRYPOINT ["/usr/local/bin/{{project-name}}-backend"]
