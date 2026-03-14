FROM rust:1.83-bookworm AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src/ src/
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates bash curl && \
    rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/wartable /usr/local/bin/wartable
COPY dashboard/ /opt/wartable/dashboard/

RUN mkdir -p /root/.wartable/logs

EXPOSE 9400
ENTRYPOINT ["wartable"]
