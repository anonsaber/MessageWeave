# C-DEBIAN-SLIM: Debian builder and runtime; no Alpine/musl dependencies.
FROM rust:1-slim-bookworm AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY web ./web
RUN cargo build --release --locked

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install --no-install-recommends -y ca-certificates tini \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --create-home --home-dir /nonroot --shell /usr/sbin/nologin stalwart-bot
COPY --from=builder /build/target/release/stalwart-bot /usr/local/bin/stalwart-bot
USER stalwart-bot
ENV PORT=8080 \
    RUN_MODE=webhook
EXPOSE 8080
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/stalwart-bot"]
