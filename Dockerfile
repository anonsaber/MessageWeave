# C-DEBIAN-SLIM: Debian builder and runtime; no Alpine/musl dependencies.
FROM rust:1-slim-bookworm AS builder
WORKDIR /build
ENV CARGO_REGISTRIES_CRATES_IO_INDEX=sparse+https://mirrors.ustc.edu.cn/crates.io-index/
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY web ./web
RUN cargo build --release --locked

FROM debian:bookworm-slim AS runtime
# 诊断工具有意内置：容器 rootfs 只读，运行期无法 apt 安装，只能在构建期装入。
# 全部为只读查询能力（本机探活 / 进程 / socket / DNS / JSON 解析），
# 不引入包管理之外的写能力，也不增加 shell 解释器。
# 合计实测 19.7 MB（用 du -sb 对全量文件系统与 debian:bookworm-slim 对比测得）。
#
# 刻意不装 bind9-dnsutils（dig）：它连带 libicudata 等依赖要多花 42.7 MB，
# 而 DNS 解析用基础镜像自带的 getent 就能覆盖：
#   getent hosts motofans.club     ->  A 记录
#   getent ahostsv4 <host>         ->  仅 IPv4
#   getent ahostsv6 <host>         ->  仅 IPv6
# 需要 TLS 握手细节时改用 curl -sv，两者都已在上面。
# 注意：不要在此加入 RUN_MODE —— 该标识符已随 Config::from_env() 删除，代码零读取。
RUN apt-get update \
    && apt-get install --no-install-recommends -y \
        ca-certificates tini \
        curl procps iproute2 jq netcat-openbsd \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --create-home --home-dir /nonroot --shell /usr/sbin/nologin messageweave
COPY --from=builder /build/target/release/message-weave /usr/local/bin/message-weave
USER messageweave
ENV PORT=8080
EXPOSE 8080
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/message-weave"]
