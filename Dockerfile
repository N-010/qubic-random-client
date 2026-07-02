FROM rust:1-bookworm AS builder

WORKDIR /build

RUN apt-get update \
    && apt-get install --no-install-recommends --yes cmake git perl pkg-config \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock build.rs ./
COPY proto ./proto
COPY src ./src

RUN cargo build --release --locked

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install --no-install-recommends --yes ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/RandomCient /usr/local/bin/RandomCient

USER 65532:65532
WORKDIR /app

ENTRYPOINT ["/usr/local/bin/RandomCient"]
