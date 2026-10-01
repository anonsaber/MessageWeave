# MessageWeave 部署指南

> [English version / 英文版 → deployment.md](deployment.md)

使用本指南部署后端、配置邮件传递并验证通知。
选择任何可以运行后端容器的平台，提供加密环境
密钥，并连接到外部管理的 Redis 服务。可选的 Cloudflare Worker
gateway 在一个或多个后端前面添加一个稳定的 URL。特定于平台的设置是
在[部署平台参考](reference.zh-CN.md#9-部署平台细节)中。

## 0. 部署前准备

准备启用持久性和公共 HTTPS URL 的外部管理的 Redis 服务
到达后端。平台或代理终止 TLS； MessageWeave 监听其中一个
HTTP 端口（“PORT”，默认“8080”）。该过程需要两个加密的启动密钥：
`REDIS_URL` 和 `CONFIG_ENCRYPTION_KEY` （编码为 64 十六进制的随机 32 字节密钥
字符）。切勿将真实值放入源代码管理、图像或日志中。业务设置
通过受保护的配置API保存在Redis中；将它们添加为环境
变量不配置服务。

生产环境没有本地数据库、数据量或内部调度程序。完整的安全边界
禁止模式位于 [`charter.md`](charter.zh-CN.md) 中。

## 1. 部署后端

### 容器平台

使用仓库根目录的 Dockerfile 构建镜像，并在部署平台配置必需的密钥。
如需进行本地容器冒烟测试，将 `.env.example` 复制为未跟踪的 `.env`，替换两个占位值后运行：

```sh
docker build -t messageweave:latest .
docker run --rm --env-file .env -p 8080:8080 messageweave:latest
```

该镜像使用 Rust 构建器和 Debian slim 运行时，以非 root 用户身份运行，并公开
仅端口“8080”。不要安装数据或日志卷。完整图像内容位于
[平台参考](reference.zh-CN.md#91-后端部署方式)。

## 2. 首次配置

在原点打开后端的配置页面进行初始设置。使用
`CONFIG_ENCRYPTION_KEY` 建立管理会话，然后完成一次性引导
表单。bootstrap 路由不在网关路由集内，因此只能在后端自己的地址上访问。bootstrap 和管理会话处理器位于
`src/notify.rs:834` 与 `src/notify.rs:1187`；业务配置保存处理器位于
`src/notify.rs:694`。后续修改可通过 Worker URL 打开受保护的配置页。

至少配置 Telegram 机器人令牌、目标聊天 ID、入站聊天允许列表、Stalwart
JMAP HTTPS 会话 URL 和应用专用密码、Webhook 密钥和工作令牌。如果启用定时对账，还需设置对账令牌。
只有用户明确同意外部处理时，才配置 LLM。配置 API 不会返回密钥内容；完整字段语义见
[配置参考](reference.zh-CN.md#5-环境变量)。

### 2.1 可选的远程诊断

在生产中保留“DEBUG_ENABLED”和“DEBUG_TOKEN”未设置，除非远程诊断
刻意需要的。两者都必须配置为挂载诊断路由，并且这些路由
只能通过后端源访问。诊断表面和响应详细信息
记录在[参考](reference.zh-CN.md#3-后端路由)中。

## 3. 构建并运行 Docker 镜像

根 Dockerfile 是容器部署路径。它有独立的 Rust 构建器和
Debian slim 运行时阶段；有关运行时内容，请参阅 [§8.2](#82-docker-镜像细节)。

## 4.1 注册 Telegram 和 Stalwart 回调

保存业务配置后，在配置页的“注册 Telegram 和 Stalwart 回调”区域操作。输入接收回调的公开 HTTPS origin：启用 Worker 时填写 Worker 地址，否则填写后端地址。页面会向 Telegram 注册 `/webhook/tg`，并向 Stalwart 注册 `/push/jmap`。凭据保留在后端，浏览器只提交回调 URL。Telegram 和 Stalwart 注册处理器分别位于 `src/notify.rs:1623` 与 `src/notify.rs:1468`。

Telegram 使用已保存的 webhook secret 和 `allowed_updates: ["message"]`。注册后，Stalwart 会自动完成验证回调和 verification-code 写回。可以安全地重复注册，重复注册同一 origin 是幂等的。更改回调 origin 会更新 Telegram（原子替换），对 Stalwart 则会在注册锁内先销毁旧订阅、再创建新订阅，因此旧 origin 不再收到推送。

## 5. 运行时配置

将启动时进程配置与 Redis 驻留业务配置分开。
进程启动时仅需要“REDIS_URL”和“CONFIG_ENCRYPTION_KEY”。完整的
业务字段列表在【配置参考】(reference.md#5-environment-variables)中。

## 6. 短请求运行模式与任务调度

该服务一次处理一个请求，并且不保留 JMAP 事件流、Telegram
长轮询，或打开后台计时器。 Telegram/JMAP 推送提供快捷路径；的
外部调度程序调用下面描述的端点。

### 6.3 调度队列排空与状态协调

后端没有内部调度程序。外部 cron、计划任务或平台定时器必须调用
[`scripts/cron-drain.sh`](../scripts/cron-drain.sh)，否则排队的通知会一直无法投递。
设置了 `MW_RECONCILE_TOKEN` 时，脚本先调用 `/reconcile`，然后排空 `/worker` 队列。

提供 `MW_APP_URL` 和 `MW_WORKER_TOKEN`；启用增量对账时还要设置 `MW_RECONCILE_TOKEN`。
将这些令牌存入调度器的密钥管理器。单次运行时，使用调度器注入的令牌：

```sh
MW_APP_URL='https://<PUBLIC_URL>' bash scripts/cron-drain.sh --once
```

增量对账通常每 5–10 分钟运行一次。两个端点成功时都返回 `204`，且响应正文为空；这是正常结果，不是错误。
脚本的 `--diagnose` 模式可帮助区分服务配置问题和调度问题。参数与运行模式见
[调度器参考](reference.zh-CN.md#93-回调注册与定时任务)。

### 6.4 可靠性行为

Worker 仅重试超时和后端“5xx”响应；它返回“4xx”响应
直接。 `/reconcile` 和 `/worker` 具有更长的每条路由超时，并且不会故障转移到
第二个后端，避免重复工作。后端队列处理至少一次并且依赖
关于 Redis 支持的幂等性。请参阅路线和状态参考以了解完整的行为。

### 6.5 投递目标

该服务允许短暂的通知延迟，并以至少 99.9% 的通知率为目标
可用性。 Redis 可用性和外部调度间隔超出范围
应用程序的控制；准确的交付保证在“NFR-NOTIFY-SLA”中进行了描述，并且
项目章程中的“NFR-RECONCILE-INTERVAL”。

## 7. 健康检查

使用 `/healthz` 检查进程存活（`src/notify.rs:118`），使用 `/ready` 检查依赖就绪
（`src/notify.rs:193`）。验证步骤见[§8.1](#81-验证部署)。

## 8.1 验证部署

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

## 8.2 Docker 镜像细节

根 Dockerfile 的构建器使用 `rust:1-slim-bookworm`；运行时间是
`debian:bookworm-slim`。运行时安装 CA 证书、`tini`、`curl`、`procps`、
`iproute2`、`jq` 和 `netcat-openbsd` 创建 `messageweave` 系统用户，设置
`PORT=8080`，并通过 `tini` 启动二进制文件。它没有“VOLUME”声明并且确实
不启动Redis。这些信息描述仓库提供的 Docker 镜像；其他平台可以使用自己的构建器和运行时镜像。

## 9. 已确认的部署选择

这些部署决定已经确定；它们不是每次安装的先决条件。

### 9.1 公共入口与状态存储

后端使用外部 HTTPS 入口和外部 Redis。 Telegram、JMAP 推送和
计划的请求使用选定的公共 URL。当Worker网关启用后，注册
Telegram 和 Stalwart 的稳定 Worker URL；不要在网关处公开凭据。
本节中的稳定 ID 指向项目章程的注册表。

## 10. 可选的 Cloudflare Worker 网关

网关是可选项，后端没有任何东西依赖它。它为多个后端 origin
提供一个公共 HTTPS 入口。所有后端必须共享同一 Redis 和业务配置。
详细的路由事实在[网关路由矩阵](reference.zh-CN.md#4-网关与后端路由对照表)中。

它的用途是负载均衡，不是防护。Cloudflare 的 Load Balancer 产品在免费
套餐上不可用，这就是免费套餐账户在多个 origin 前面做负载均衡与故障转移
的办法。没有隐藏后端的意图，也没有加固后端的意图：网关在不在，origin
都同样可以直接访问，边缘上也没有加任何访问控制。固定的路由白名单只是
限定负载均衡器转发哪些路径——bootstrap 和 `/debug/*` 不在其中，所以发给
网关的请求到不了它们——但它不是防火墙。

### 10.1 部署拓扑

Telegram webhooks、Stalwart 推送回调和外部调度可以使用稳定的 Worker
网址。 Worker 转发到后端 HTTPS 源；它不连接到 Redis 或 JMAP。

### 10.2 多实例前提

多个后端可以共享流量，因为请求状态存储在 Redis 中，重复
交付已进行重复数据删除，协调修复了错过的推送事件。无粘性会话
或需要更改应用程序代码。

所有 Redis 键名都是全局命名的，**不带实例、主机名或源站后缀**。共享同一个 Redis 在这里不是
折中：下列每一项在多源站之间都只有一份。

| 数据 | 键 | 语义 |
|---|---|---|
| 业务配置 | `config:business` | 单一全局密文块（AES-256-GCM），没有按实例划分的槽位 |
| 配置版本 | `config:business:revision` | 单一全局单调计数器，与写入在同一次 Lua 事务中提交（`CONFIG_STORE_REV_SCRIPT`） |
| 启停门 | `config:enabled` | 一个全局布尔值 |
| 对账游标 | `state:jmap:since` | 单一全局游标，无账户维度 |
| 投递流 | `stalwart:jmap:events` / `stalwart:telegram:events` | 由消费者组共享，组惰性创建，`XAUTOCLAIM` 回收空闲消息 |
| 对账锁 | `lock:reconcile` | 单一全局单飞锁，跨实例互斥 |

由此有两条必须知道的推论。

第一，**业务配置只有一份，从任何一台源站的 SPA 编辑写进去的都是同一份**。不存在“只改其中一台”
的表达方式：没有任何键带实例维度，`refresh_business_config` 也不接收实例参数。这是预期行为。
如果确实需要两套互不影响的配置（例如生产与预发并行），正确做法是部署第二个实例并指向另一个
Redis，而不是在同一个 Redis 内部做区分。

第二，**`CONFIG_ENCRYPTION_KEY` 必须逐字一致**。它是 64 个十六进制字符（32 字节），直接作为
AES-256-GCM 密钥，AAD 固定为 `message-weave/config:business/v1`；它同时也是 SPA 的管理凭据。
任一台源站持有不同的值，既解不出别的源站写入的配置，也登录不了 SPA，两条路都是 fail closed。

加源站买到的是故障转移，不是吞吐。投递靠共享消费者组分摊，但 `/reconcile` 受 `lock:reconcile`
全局串行，游标也只有一份。一台实例在批次中间挂掉，消息会在 `XAUTOCLAIM` 空闲窗口过后被另一台
接手；代价是可能重复投递（由 `dedup:*` 键去重），不会丢消息。

### 10.3 信任模型

Worker 传递请求标头和正文。仍需要后端身份验证
因为后端源可能是直接可达的。每个实例必须使用相同的共享
业务凭证和 Redis 状态。

### 10.4 路由与故障转移

HTTP 请求访问 Worker 时，会先收到永久的 `308` 重定向，前往路径和查询参数相同的
HTTPS URL，之后才进行路由校验或转发。Worker 使用“404”拒绝未注册的路由，并使用“405”拒绝错误的方法。普通
请求最多尝试“LB_MAX_ATTEMPTS”来源（默认两个），并且只有超时或“5xx”
触发故障转移。 `/reconcile` 和 `/worker` 是具有较长超时时间的单次尝试路由
覆盖。使用[参考](reference.zh-CN.md#4-网关与后端路由对照表)中的路由矩阵和调整值。

### 10.5 回调地址

使用网关时，将Telegram的webhook和Stalwart的push回调设置给Worker
域。外部调度程序可以对转发路径使用相同的 URL。引导程序和
诊断端点仍然需要后端源。

### 10.6 网关健康状态

`GET /healthz-worker` 报告 Worker 版本和后端可用性。两者都使用
用于验证部署的“可用”和“版本”； `status: no-backends` 可以返回
当未配置有效源时，HTTP 200。

### 10.7 功能边界

Worker 不是 Redis 代理，也不会使后端无状态；共享外部
Redis 仍然需要。它不添加数据库、长期连接或第二层
业务认证。

### 10.8 多源站之间的配置收敛

SPA 的一次编辑只写入 Redis 一次。各源站**惰性收敛，发生在请求边界上**：每个带请求的入口
（`/worker`、`/reconcile`、`/healthz`、`/push/jmap`、`/api/*`）都会调用
`refresh_business_config`，它先从 Redis 读全局版本，当 `remote_revision <= local_revision` 时
立即返回。没有轮询循环，也没有 watch，所以一台完全收不到流量的源站会一直保留它原有的配置。

**已知行为：重建失败后不会重试，直到下一次版本更新。** 上面的版本判断只是一个提前返回，它不是
重试保护。当某台源站的 `build_worker` 失败——JMAP session URL 不可达，或 `llm_api_key` /
`llm_base_url` 缺失或非法——该源站会把内存中的版本推进到远端值，并保留原有运行时。此后提前返回
条件对该源站永久成立，它不会再尝试，直到写入了一个**新的**版本。同一函数里的其他失败（Redis
不可达、密文块缺失、JSON 解析失败、校验失败）都不推进本地版本，因此会在下一次请求时重试；这些
路径都在构建运行时之前就返回，所以不会重建 worker。

症状是一台源站已运行新配置、另一台还停在旧配置上，而且**没有报错也没有日志**：失败路径是静默
返回的。排查方法是逐台比对 `GET /debug/config` 返回的字段值——那些值来自内存里的实时运行时。
不要用它的 `revision` 字段判断：那个值从 Redis 读取，即使该实例尚未收敛也会显示最新的全局版本。

恢复办法：在 SPA 里再动一次配置让版本前进，卡住的源站就会重新尝试一次。如果这台源站仍在
Worker 的源站列表里，这期间把它从列表中移除，避免请求被分裂到两套配置上。
