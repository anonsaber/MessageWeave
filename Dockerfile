# C-DEBIAN-SLIM: Debian builder and runtime; no Alpine/musl dependencies.
FROM rust:1-slim-bookworm AS builder
WORKDIR /build
ENV CARGO_REGISTRIES_CRATES_IO_INDEX=sparse+https://mirrors.ustc.edu.cn/crates.io-index/
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY web ./web
RUN cargo build --release --locked

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install --no-install-recommends -y ca-certificates tini \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --create-home --home-dir /nonroot --shell /usr/sbin/nologin messageweave
COPY --from=builder /build/target/release/message-weave /usr/local/bin/message-weave
USER messageweave
ENV PORT=8080 \
    RUN_MODE=webhook
EXPOSE 8080
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/message-weave"]
