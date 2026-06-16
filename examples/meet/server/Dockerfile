# Build context: repository root
# docker build -f examples/meet/server/Dockerfile -t seameet-server .

FROM rust:1.85-bookworm AS builder

WORKDIR /build

COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
COPY examples/meet/ examples/meet/

RUN cargo build --release -p meet

# ---

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/meet /usr/local/bin/meet

EXPOSE 3001/tcp
EXPOSE 10000/udp

ENV RUST_LOG=meet=info,str0m=warn

# Production note: this image listens on plain ws://0.0.0.0:3001. Terminate
# TLS upstream (nginx, Caddy, ALB, …) and forward to this container. Set
# SEAMEET_AUTH_TOKEN to a strong shared secret, or replace the example's
# auth hook with a real identity check before exposing publicly.

CMD ["meet"]
