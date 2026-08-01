# Build from the repo root: docker build -f apps/idp/backend/Dockerfile .
#
# Unlike portal-backend, there's no wasm/trunk build stage here: the IDP's
# own frontend (apps/idp/frontend) is plain static HTML/CSS/JS -- no build
# step at all, deliberately (see docs/architecture.md), so any OAuth client
# redirecting a browser here gets a small, fast login page regardless of
# what stack that client is built with.
FROM rust:1-slim-bookworm AS builder
WORKDIR /app
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
COPY . .
RUN cargo build --release -p idp-backend

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/idp-backend /usr/local/bin/idp-backend
COPY --from=builder /app/apps/idp/frontend/static /app/static
COPY --from=builder /app/apps/idp/backend/migrations /app/migrations
ENV STATIC_DIR=/app/static
EXPOSE 4000
ENTRYPOINT ["/usr/local/bin/idp-backend"]
