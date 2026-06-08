# Build: from the repo root, run
#   docker build -f deploy/relay/Dockerfile -t farder-relay .
FROM rust:1-slim AS builder
WORKDIR /build
COPY . .
RUN cargo build --release -p farder-relay

FROM debian:stable-slim
RUN useradd -r -u 10001 relay && mkdir -p /data && chown relay /data
COPY --from=builder /build/target/release/farder-relay /usr/local/bin/farder-relay
USER relay
VOLUME ["/data"]
EXPOSE 4433/udp
ENTRYPOINT ["/usr/local/bin/farder-relay", "--bind", "0.0.0.0:4433", "--data-dir", "/data"]
