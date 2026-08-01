# Build from the repo root: docker build -f apps/webhello/backend/Dockerfile .
#
# Single build stage, unlike apps/hello/backend's -- webhello's frontend is
# hand-written static HTML/JS with no compile step, so there's no trunk
# stage to run; the final image just copies static/ alongside the binary.

FROM rust:1-slim-bookworm AS builder
WORKDIR /app
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
COPY . .
RUN cargo build --release -p webhello-backend

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/webhello-backend /usr/local/bin/webhello-backend
COPY apps/webhello/backend/static ./static
EXPOSE 8082
ENTRYPOINT ["/usr/local/bin/webhello-backend"]
