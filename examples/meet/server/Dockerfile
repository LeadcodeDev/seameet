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

ENV RUST_LOG=meet=debug,str0m=warn

CMD ["meet"]
