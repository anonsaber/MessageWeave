# MessageWeave 部署指南

> [English version / 英文版 → deployment.md](deployment.md)

使用本指南部署后端、配置邮件传递并验证通知。
选择任何可以运行后端容器的平台，提供加密环境
密钥，并连接到外部管理的 Redis 服务。可选的 Cloudflare Worker
gateway 在一个或多个后端前面添加一个稳定的 URL。特定于平台的设置是
在[部署平台参考](reference.zh-CN.md#9-deployment-platform-details)中。

## 0. Before deployment

准备启用持久性和公共 HTTPS URL 的外部管理的 Redis 服务
到达后端。平台或代理终止 TLS； MessageWeave 监听其中一个
HTTP 端口（“PORT”，默认“8080”）。该过程需要两个加密的启动密钥：
`REDIS_URL` 和 `CONFIG_ENCRYPTION_KEY` （编码为 64 十六进制的随机 32 字节密钥
字符）。切勿将真实值放入源代码管理、图像或日志中。业务设置
通过受保护的配置API保存在Redis中；将它们添加为环境
变量不配置服务。

生产环境没有本地数据库、数据量或内部调度程序。完整的安全边界
禁止模式位于 [`charter.md`](charter.zh-CN.md) 中。

## 1. Deploy a backend

### Container platform

构建根 Dockerfile 并在平台中配置所需的机密。对于本地人来说
容器冒烟，将 `.env.example` 复制到未跟踪的 `.env`，替换两个占位符，
然后运行：

```sh
docker build -t messageweave:latest .
docker run --rm --env-file .env -p 8080:8080 messageweave:latest
```

该镜像使用 Rust 构建器和 Debian slim 运行时，以非 root 用户身份运行，并公开
仅端口“8080”。不要安装数据或日志卷。完整图像内容位于
[平台参考](reference.zh-CN.md#91-backend-deployment-paths)。

## 2. First-time configuration

在原点打开后端的配置页面进行初始设置。使用
`CONFIG_ENCRYPTION_KEY` 建立管理会话，然后完成一次性引导
表单。Worker 网关不会公开 bootstrap 路由。bootstrap 和管理会话处理器位于
`src/notify.rs:834` 与 `src/notify.rs:1187`；业务配置保存处理器位于
`src/notify.rs:694`。后续修改可通过 Worker URL 打开受保护的配置页。

至少配置 Telegram 机器人令牌、目标聊天 ID、入站聊天允许列表、Stalwart
JMAP HTTPS 会话 URL 和应用程序密码、Webhook 机密和工作人员令牌。设置一个
如果使用计划的协调，则协调令牌。仅当用户
已明确同意外部处理。秘密值不会由
配置API；完整的字段语义位于[配置参考](reference.zh-CN.md#5-environment-variables)中。

### 2.1 Optional remote diagnostics

在生产中保留“DEBUG_ENABLED”和“DEBUG_TOKEN”未设置，除非远程诊断
刻意需要的。两者都必须配置为挂载诊断路由，并且这些路由
只能通过后端源访问。诊断表面和响应详细信息
记录在[参考](reference.zh-CN.md#3-backend-routes)中。

## 3. Build and run the Docker image

根 Dockerfile 是容器部署路径。它有独立的 Rust 构建器和
Debian slim 运行时阶段；有关运行时内容，请参阅 [§8.2](#82-docker-image-details)。

## 4.1 注册 Telegram 和 Stalwart 回调

保存业务配置后，在配置页的“注册 Telegram 和 Stalwart 回调”区域操作。输入接收回调的公开 HTTPS origin：启用 Worker 时填写 Worker 地址，否则填写后端地址。页面会向 Telegram 注册 `/webhook/tg`，并向 Stalwart 注册 `/push/jmap`。凭据保留在后端，浏览器只提交回调 URL。Telegram 和 Stalwart 注册处理器分别位于 `src/notify.rs:1439` 与 `src/notify.rs:1315`。

Telegram 使用已保存的 webhook secret 和 `allowed_updates: ["message"]`。注册后，Stalwart 会自动完成验证回调和 verification-code 写回。可以安全地重复注册；更改回调 origin 会更新 Telegram webhook，并为新的 URL 创建 Stalwart subscription。

## 5. Runtime configuration

将启动时进程配置与 Redis 驻留业务配置分开。
进程启动时仅需要“REDIS_URL”和“CONFIG_ENCRYPTION_KEY”。完整的
业务字段列表在【配置参考】(reference.md#5-environment-variables)中。

## 6. Short-request runtime and scheduling

该服务一次处理一个请求，并且不保留 JMAP 事件流、Telegram
长轮询，或打开后台计时器。 Telegram/JMAP 推送提供快捷路径；的
外部调度程序调用下面描述的端点。

### 6.3 Schedule queue draining and reconciliation

后端没有内部调度程序。外部 cron、计划作业或平台计时器
必须调用 [`scripts/cron-drain.sh`](../scripts/cron-drain.sh);否则排队的通知
仍然未交付。当设置“MW_RECONCILE_TOKEN”时，脚本调用“/reconcile”，然后耗尽
工作队列。

提供`MW_APP_URL`和`MW_WORKER_TOKEN`；当对帐时设置“MW_RECONCILE_TOKEN”
已启用。将令牌存储在调度程序的秘密管理器中。对于单次运行：

使用调度程序的秘密管理器注入的令牌，运行：

```sh
MW_APP_URL='https://<PUBLIC_URL>' bash scripts/cron-drain.sh --once
```

调节节奏通常为 5-10 分钟。两个成功的端点都返回“204”
身体空虚；不要将其视为错误。脚本的“--diagnose”模式有帮助
将服务配置问题与调度问题分开。其论点和
操作模式列在[调度程序参考](reference.zh-CN.md#93-callback-registration-and-scheduled-work)中。

### 6.4 Reliability behavior

Worker 仅重试超时和后端“5xx”响应；它返回“4xx”响应
直接。 `/reconcile` 和 `/worker` 具有更长的每条路由超时，并且不会故障转移到
第二个后端，避免重复工作。后端队列处理至少一次并且依赖
关于 Redis 支持的幂等性。请参阅路线和状态参考以了解完整的行为。

### 6.5 Delivery objective

该服务允许短暂的通知延迟，并以至少 99.9% 的通知率为目标
可用性。 Redis 可用性和外部调度间隔超出范围
应用程序的控制；准确的交付保证在“NFR-NOTIFY-SLA”中进行了描述，并且
项目章程中的“NFR-RECONCILE-INTERVAL”。

## 7. Health checks

使用 `/healthz` 检查进程存活（`src/notify.rs:118`），使用 `/ready` 检查依赖就绪
（`src/notify.rs:193`）。验证步骤见[§8.1](#81-verify-the-deployment)。

## 8.1 Verify the deployment

首先检查后端：

|请求|预期结果|含义|
|---|---|---|
|`GET /healthz`|`200`|进程存活。|
|`GET /ready`|`200`|配置、Redis、JMAP 和 Telegram 探针均就绪；依赖故障返回 `503`。|
|`GET /api/status`|`{"ready":true}`|必需的启动配置已就绪；`missing` 字段列出缺失变量名。|

启用 Worker 网关后，还请求“GET /healthz-worker”。要求
`available >= 1` 并检查 `version` 是否与部署的 `LB_VERSION` 匹配；仅 HTTP 200
可能意味着“无后端”。 `cloudflare-worker/` 目录的测试和部署命令是
在其自述文件中描述。

将“/help”发送到配置的 Telegram 聊天并确认回复。然后发送一封新电子邮件并
确认外部调度程序耗尽其通知。健康的“/healthz”只能证明
进程活跃度；它并不能证明 Redis 或上游任一者可用。

## 8.2 Docker image details

根 Dockerfile 的构建器使用 `rust:1-slim-bookworm`；运行时间是
`debian:bookworm-slim`。运行时安装 CA 证书、`tini`、`curl`、`procps`、
`iproute2`、`jq` 和 `netcat-openbsd` 创建 `messageweave` 系统用户，设置
`PORT=8080`，并通过 `tini` 启动二进制文件。它没有“VOLUME”声明并且确实
不启动Redis。这些信息描述仓库提供的 Docker 镜像；其他平台可以使用自己的构建器和运行时镜像。

## 9. Confirmed deployment choices

这些部署决定已经确定；它们不是每次安装的先决条件。

### 9.1 Public ingress and state

后端使用外部 HTTPS 入口和外部 Redis。 Telegram、JMAP 推送和
计划的请求使用选定的公共 URL。当Worker网关启用后，注册
Telegram 和 Stalwart 的稳定 Worker URL；不要在网关处公开凭据。
本节中的稳定 ID 指向项目章程的注册表。

## 10. Optional Cloudflare Worker gateway

Worker 为多个后端源提供一个公共 HTTPS 入口点。所有后端
必须共享相同的Redis和业务配置。 Worker 仅转发其固定的
路由白名单； bootstrap 和 `/debug/*` 仍然仅限原始。详细路线事实在
[网关路由矩阵](reference.zh-CN.md#4-gateway-vs-backend-route-matrix)。

### 10.1 Topology

Telegram webhooks、Stalwart 推送回调和外部调度可以使用稳定的 Worker
网址。 Worker 转发到后端 HTTPS 源；它不连接到 Redis 或 JMAP。

### 10.2 Multi-instance prerequisites

多个后端可以共享流量，因为请求状态存储在 Redis 中，重复
交付已进行重复数据删除，协调修复了错过的推送事件。无粘性会话
或需要更改应用程序代码。

### 10.3 Trust model

Worker 传递请求标头和正文。仍需要后端身份验证
因为后端源可能是直接可达的。每个实例必须使用相同的共享
业务凭证和 Redis 状态。

### 10.4 Routes and failover

HTTP 请求访问 Worker 时，会先收到永久的 `308` 重定向，前往路径和查询参数相同的
HTTPS URL，之后才进行路由校验或转发。Worker 使用“404”拒绝未注册的路由，并使用“405”拒绝错误的方法。普通
请求最多尝试“LB_MAX_ATTEMPTS”来源（默认两个），并且只有超时或“5xx”
触发故障转移。 `/reconcile` 和 `/worker` 是具有较长超时时间的单次尝试路由
覆盖。使用[参考](reference.zh-CN.md#4-gateway-vs-backend-route-matrix)中的路由矩阵和调整值。

### 10.5 Callback URLs

使用网关时，将Telegram的webhook和Stalwart的push回调设置给Worker
域。外部调度程序可以对转发路径使用相同的 URL。引导程序和
诊断端点仍然需要后端源。

### 10.6 Gateway health

`GET /healthz-worker` 报告 Worker 版本和后端可用性。两者都使用
用于验证部署的“可用”和“版本”； `status: no-backends` 可以返回
当未配置有效源时，HTTP 200。

### 10.7 Boundaries

Worker不是Redis代理，不会使后端无状态；共享外部Redis
仍然需要。它不添加数据库、长期连接或第二层
商业认证。
