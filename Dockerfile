# ============================================================================
# 仅本地开发 / 本地容器调试用。生产不执行本文件。
#
# 线上部署走仓库根目录 hoststack.yaml + runtime: rust，由 HostStack 自己的
# agent 在 rust:slim-trixie 里构建、拷进 debian:trixie-slim 的 runner 容器运行
# ——完全不经过本 Dockerfile（连 Debian 版本都不同：bookworm vs trixie）。
# 因此本文件里的诊断工具、ENTRYPOINT tini、EXPOSE 8080 在线上都不生效。
# 判定当前跑的是哪条路径：容器内看 /proc/1/cmdline 与 /etc/os-release。
# 生产启动命令的真源是 hoststack.yaml 的 start.command，见 docs/deployment.md
# §5.1；本文件只服务于 `docker run` / 本地起一个容器来验证行为。
# ============================================================================
#
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
