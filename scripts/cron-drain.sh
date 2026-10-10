#!/usr/bin/env bash
# ============================================================================
# cron-drain.sh — 让 Telegram 通知真正发出去的外部调度器。
#
# 后端**没有任何内部调度器**（C-NO-LONG-CONN：无 tokio-cron、无后台 poller）。
# 消费出站队列的唯一入口是 POST /worker，增量对账的唯一入口是 POST /reconcile，
# 两者都必须由外部（cron / k8s CronJob / 定时器）触发。没有这一步，新邮件会一直
# 堆在 Redis 队列里，通知永远不发——这是「收不到 Telegram 提醒」最常见的原因。
#
# 两个端点的成功码都是 **204 且响应体为空**，不是 200：
#   POST /reconcile  → 204 成功 / 401 / 409（另一轮进行中）/ 503（带 Retry-After）
#   POST /worker     → 204 成功 / 400（body 非法）/ 401 / 503（带 Retry-After）
# 因此看到 204 是正常的，不要当成失败。
#
# 详设计说明见 README.md 第 3.10 节（调度与探活）。
#
# --- 凭据只从环境变量或命令行 flag 读取 ---
# 不读 .env、不读配置文件、不写盘（唯一例外是可选的 --log-file / MW_LOG_FILE 运行日志）。
# 所有 *_TOKEN 一律不打印、不写日志。命令行 flag 优先于同名 MW_* 环境变量。
# <<HELP>>
# --- 配置项（flag 或环境变量，flag 优先） ---
#   --app-domain DOMAIN / MW_APP_DOMAIN
#                         必填。对外平台域名（走 LB 的域名），用于 GET /api/status、
#                         GET /healthz、POST /reconcile。只写域名，无需 https:// 前缀
#                         （脚本自动补 https://）。旧名 --app-url / MW_APP_URL 仍可用。
#   --worker-domain DOMAIN / MW_WORKER_DOMAIN
#                         可选，默认 = --app-domain。POST /worker 的地址。/worker 已在
#                         LB 路由白名单内，默认直接走 --app-domain 即可；要跳过 LB 直连
#                         源站时才需要覆盖。旧名 --worker-url / MW_WORKER_URL 仍可用。
#   --debug-domain DOMAIN / MW_DEBUG_DOMAIN
#                         可选，默认 = --worker-domain。GET /debug/* 与 POST /debug/notify
#                         的地址（需要后端以 --debug 启动且 DEBUG_TOKEN 非空，且需直连源站）。
#                         旧名 --debug-url / MW_DEBUG_URL 仍可用。
#   --reconcile-token TOKEN / MW_RECONCILE_TOKEN
#                         可选。缺省时跳过 /reconcile（仅靠 Push 回调入队的部署可接受）。
#   --worker-token TOKEN / MW_WORKER_TOKEN
#                         必填。/worker 的 Bearer token。
#   --debug-token TOKEN / MW_DEBUG_TOKEN
#                         可选，仅 --diagnose / --test-notify 用。
#   --batch N / MW_BATCH  可选，默认不发（服务端默认 10，且上限 10，超出静默截断）。
#   --timeout N / MW_TIMEOUT
#                         可选，默认 60。单次请求超时秒数。
#   --interval N / MW_INTERVAL
#                         可选，默认 30。仅 --loop 用，两次触发之间的秒数。
#   --log-file PATH / MW_LOG_FILE
#                         可选。给定时每次触发追加一行摘要（不含任何凭据）。
#   --test-text TEXT / MW_TEST_TEXT
#                         可选，仅 --test-notify 用。直发测试的正文。
#
# --- 用法 ---
#   # flag 风格（推荐交互式使用，只写域名）：
#   bash scripts/cron-drain.sh --once \
#     --app-domain messageweave.example \
#     --reconcile-token reconcile-... --worker-token worker-...
#
#   # 环境变量风格（cron 推荐，凭据不出现在 ps）：
#   MW_APP_DOMAIN=messageweave.example \
#   MW_RECONCILE_TOKEN=reconcile-... MW_WORKER_TOKEN=worker-... \
#     bash scripts/cron-drain.sh --once
#
#   # cron（每分钟，日志落盘便于事后排查）：
#   #   * * * * * env MW_APP_DOMAIN=messageweave.example \
#   #      MW_RECONCILE_TOKEN=*** MW_WORKER_TOKEN=*** \
#   #      bash /opt/messageweave/scripts/cron-drain.sh --once >> /var/log/mw-drain.log 2>&1
#
#   # 常驻进程，不依赖 crontab：
#   bash scripts/cron-drain.sh --loop --app-domain messageweave.example \
#     --reconcile-token <token> --worker-token <token> --interval 30
#
#   # 收不到提醒时先跑这个，自动定位原因：
#   bash scripts/cron-drain.sh --diagnose --app-domain messageweave.example --debug-token ***
#
#   # 绕过队列、复用生产出站路径直发一条，验证 bot token 与 chat_id 本身是否可用：
#   bash scripts/cron-drain.sh --test-notify --app-domain messageweave.example \
#     --debug-token *** --test-text "probe"
#
# --- 退出码 ---
#   0  成功（两个端点都返回 204，或 /reconcile 被主动跳过）
#   1  至少一步失败（请求失败、401、409、503）——交给 cron 重试
#   2  用法或配置错误（缺必填变量、参数非法）
#      退出码是 1/0 二值，不携带失败步数；失败步数看日志行数即可。
# <<HELP_END>>
# ============================================================================
set -uo pipefail

MODE="--once"
DO_DIAGNOSE=0
DO_TEST_NOTIFY=0

APP_URL="${MW_APP_DOMAIN:-${MW_APP_URL:-}}"
WORKER_URL="${MW_WORKER_DOMAIN:-${MW_WORKER_URL:-}}"
DEBUG_URL="${MW_DEBUG_DOMAIN:-${MW_DEBUG_URL:-}}"
RECONCILE_TOKEN="${MW_RECONCILE_TOKEN:-}"
WORKER_TOKEN="${MW_WORKER_TOKEN:-}"
DEBUG_TOKEN="${MW_DEBUG_TOKEN:-}"
BATCH="${MW_BATCH:-}"
TIMEOUT="${MW_TIMEOUT:-60}"
INTERVAL="${MW_INTERVAL:-30}"
LOG_FILE="${MW_LOG_FILE:-}"
TEST_TEXT="${MW_TEST_TEXT:-}"
FAILURES=0

# --- 基础输出 -----------------------------------------------------------------
say() { printf '%s\n' "$*"; }

# 日志：默认只打 stdout；MW_LOG_FILE 给定时同步追加。只写状态码与计数，不写凭据。
log() {
  local line
  line="$(date -u '+%Y-%m-%dT%H:%M:%SZ') $*"
  if [ -n "$LOG_FILE" ]; then
    mkdir -p "$(dirname "$LOG_FILE")" 2>/dev/null || true
    printf '%s\n' "$line" >>"$LOG_FILE" 2>/dev/null || true
  fi
  printf '%s\n' "$line"
}

die() {
  printf 'cron-drain.sh: error: %s\n' "$*" >&2
  exit 2
}

usage() {
  sed -n '/^# <<HELP>>$/,/^# <<HELP_END>>$/p' "$0" \
    | sed 's/^# \{0,1\}//' \
    | grep -v '^<<HELP'
  exit "${1:-0}"
}

# 缺必填变量时把用法一起打出来，退出 2。
usage_die() {
  printf 'cron-drain.sh: error: %s\n' "$*" >&2
  usage 2
}

# 规范化域名/URL：没有协议前缀时自动补 https://，返回完整 URL。
normalize_url() {
  local v="$1"
  [ -n "$v" ] || { printf '%s' "$v"; return; }
  case "$v" in
    http://* | https://*) printf '%s' "$v" ;;
    *) printf 'https://%s' "$v" ;;
  esac
}

# --- JSON 取值（不依赖 jq）：json_field '<json>' <key> → 首个匹配键的值 -----------
json_field() {
  printf '%s' "$1" \
    | grep -oE "\"$2\"[[:space:]]*:[[:space:]]*(\"[^\"]*\"|null|true|false|-?[0-9]+([.][0-9]+)?)?" \
    | head -n1 \
    | sed -E 's/^"'"$2"'"[[:space:]]*:[[:space:]]*//' \
    | sed -E 's/^"(.*)"$/\1/'
}

# --- 发一次请求：request <method> <url> [token] → 设置 CODE / BODY / RETRY_AFTER --
# token 为空时不带 Authorization 头（公开端点用）。返回 curl 退出码。
BODY_FILE=""
CODE=""
BODY=""
RETRY_AFTER=""

request() {
  local method="$1" url="$2" token="${3:-}" rc=0 headers=()
  CODE=""
  BODY=""
  RETRY_AFTER=""
  [ -n "$token" ] && headers=(-H "Authorization: Bearer $token")
  curl -sS \
    --max-time "$TIMEOUT" \
    --connect-timeout 10 \
    -X "$method" \
    -H "Accept: application/json" \
    "${headers[@]}" \
    -D "$BODY_FILE.h" \
    -o "$BODY_FILE" \
    -w '%{http_code}' \
    "$url" >"$BODY_FILE.code" 2>/dev/null || rc=$?
  if [ "$rc" -eq 0 ]; then
    CODE=$(cat "$BODY_FILE.code" 2>/dev/null)
    BODY=$(cat "$BODY_FILE" 2>/dev/null)
    RETRY_AFTER=$(awk -F ': ' 'toupper($1)=="RETRY-AFTER" {gsub("\r", "", $2); print $2}' \
      "$BODY_FILE.h" 2>/dev/null | head -n1)
  fi
  return "$rc"
}

curl_error() {
  case "$1" in
    6) echo "DNS 解析失败" ;;
    7) echo "连接被拒（主机或端口不可达）" ;;
    28) echo "请求超时（>${TIMEOUT}s）" ;;
    35 | 60) echo "TLS 握手或证书校验失败" ;;
    0) echo "请求未发出" ;;
    *) echo "curl 退出码 $1" ;;
  esac
}

# 503 带 Retry-After 时把建议重试间隔带上，cron 侧不用自己猜。
retry_hint() {
  if [ -n "$RETRY_AFTER" ]; then
    printf '（服务端 Retry-After=%ss）' "$RETRY_AFTER"
  fi
  return 0
}

# --- 步骤 1：POST /reconcile（增量对账；失败可容忍，主路径仍是 Push 回调）---------
run_reconcile() {
  local rc
  if [ -z "$RECONCILE_TOKEN" ]; then
    log "reconcile=skip 未设置 MW_RECONCILE_TOKEN"
    return 0
  fi
  request POST "$APP_URL/reconcile" "$RECONCILE_TOKEN"
  rc=$?
  if [ "$rc" -ne 0 ]; then
    log "reconcile=FAIL $(curl_error "$rc")（$APP_URL/reconcile）"
    FAILURES=$((FAILURES + 1))
    return 0
  fi
  case "$CODE" in
    204)
      log "reconcile=204 成功（空响应体）"
      ;;
    401)
      log "reconcile=401 token 不匹配：核对 MW_RECONCILE_TOKEN 与配置里的 reconcile_token"
      FAILURES=$((FAILURES + 1))
      ;;
    409)
      log "reconcile=409 另一轮对账进行中（lock:reconcile 被占用）$(retry_hint)"
      FAILURES=$((FAILURES + 1))
      ;;
    503)
      log "reconcile=503 业务未就绪/Redis/游标失败$(retry_hint)：交给 cron 重试"
      FAILURES=$((FAILURES + 1))
      ;;
    404)
      log "reconcile=404 $APP_URL/reconcile 不存在（MW_APP_DOMAIN 可能指错）"
      FAILURES=$((FAILURES + 1))
      ;;
    *)
      log "reconcile=$CODE ${BODY:0:200}"
      FAILURES=$((FAILURES + 1))
      ;;
  esac
}

# --- 步骤 2：POST /worker（排空出站队列；Telegram 只在批末触发，漏掉就没通知）-------
run_worker() {
  local rc
  # 服务端默认批大小 10 且上限 10；只有显式设置 MW_BATCH 时才带 body。
  local extra=()
  if [ -n "$BATCH" ]; then
    extra=(-H "Content-Type: application/json" --data "{\"batch\": $BATCH}")
  fi
  CODE=""
  BODY=""
  RETRY_AFTER=""
  rc=0
  curl -sS \
    --max-time "$TIMEOUT" \
    --connect-timeout 10 \
    -X POST \
    -H "Accept: application/json" \
    -H "Authorization: Bearer $WORKER_TOKEN" \
    "${extra[@]}" \
    -D "$BODY_FILE.h" \
    -o "$BODY_FILE" \
    -w '%{http_code}' \
    "$WORKER_URL/worker" >"$BODY_FILE.code" 2>/dev/null || rc=$?
  if [ "$rc" -eq 0 ]; then
    CODE=$(cat "$BODY_FILE.code" 2>/dev/null)
    BODY=$(cat "$BODY_FILE" 2>/dev/null)
    RETRY_AFTER=$(awk -F ': ' 'toupper($1)=="RETRY-AFTER" {gsub("\r", "", $2); print $2}' \
      "$BODY_FILE.h" 2>/dev/null | head -n1)
  fi
  if [ "$rc" -ne 0 ]; then
    log "worker=FAIL $(curl_error "$rc")（$WORKER_URL/worker）"
    FAILURES=$((FAILURES + 1))
    return 0
  fi
  case "$CODE" in
    204)
      log "worker=204 成功：队列已排空（空响应体，204 即正常）"
      ;;
    400)
      log "worker=400 请求体非法：MW_BATCH=$BATCH 传成了非 JSON"
      FAILURES=$((FAILURES + 1))
      ;;
    401)
      log "worker=401 token 不匹配：核对 MW_WORKER_TOKEN 与配置里的 worker_token"
      FAILURES=$((FAILURES + 1))
      ;;
    404)
      if printf '%s' "$BODY" | grep -q 'route not forwarded'; then
        log "worker=404 $WORKER_URL/worker 被 LB 拦截（route not forwarded）"
        log "      LB 版本太旧：/worker 需要 LB_VERSION >= 2026.10.3（见 /healthz-worker）"
        log "      升级 LB，或把 MW_WORKER_DOMAIN 指向源站直连地址绕过 LB"
      else
        log "worker=404 $WORKER_URL/worker 不存在（MW_WORKER_DOMAIN 可能指错）"
      fi
      FAILURES=$((FAILURES + 1))
      ;;
    503)
      log "worker=503 业务未就绪或 Redis/出站失败$(retry_hint)：交给 cron 下一轮重试"
      FAILURES=$((FAILURES + 1))
      ;;
    *)
      log "worker=$CODE ${BODY:0:200}"
      FAILURES=$((FAILURES + 1))
      ;;
  esac
}

# --- --diagnose：分清「配置没配好」还是「调度没跑起来」--------------------------
diagnose() {
  local code field body
  local issues=0

  say "=== 诊断：为什么收不到 Telegram 提醒 ==="
  say ""

  # 1) 公开端点，不用 token：先确认后端活着。
  if [ -z "$APP_URL" ]; then
    say "  [1] 跳过（未设置 MW_APP_DOMAIN，本次诊断只用了 MW_DEBUG_DOMAIN）"
  else
    request GET "$APP_URL/api/status"
    code="$CODE"
    say "  [1] GET $APP_URL/api/status -> ${code:-不可达}"
    if [ "$code" != "200" ]; then
      say "        后端不可达：先修服务本身，与调度无关"
      issues=$((issues + 1))
    fi
  fi

  if [ -z "$DEBUG_TOKEN" ]; then
    say ""
    say "未设置 MW_DEBUG_TOKEN，跳过 /debug/* 深入检查。"
    say "要分清「配置缺失」与「调度缺失」，需要后端以 --debug 启动并设置 MW_DEBUG_TOKEN。"
    say "（/debug/* 不在 LB 白名单内，必须直连源站。）"
    return 0
  fi

  # 2) 业务配置：未保存时出站全部 no-op，这是最常见的静默失败。
  request GET "$DEBUG_URL/debug/config" "$DEBUG_TOKEN"
  if [ "$CODE" = "401" ]; then
    say "  [2] GET /debug/config -> 401 MW_DEBUG_TOKEN 无效"
    issues=$((issues + 1))
    return 0
  fi
  if [ "$CODE" != "200" ]; then
    say "  [2] GET /debug/config -> ${CODE:-请求失败}（未启用 --debug 或地址不对）"
    issues=$((issues + 1))
    return 0
  fi
  body="$BODY"
  field="$(json_field "$body" business_configured)"
  if [ "$field" != "true" ]; then
    say "  [2] business_configured=$field ★ 业务配置从未保存"
    say "        /worker 与 /reconcile 会一律 503，通知永远不发。先保存一次配置。"
    issues=$((issues + 1))
  else
    say "  [2] business_configured=true"
  fi

  field="$(json_field "$body" chat_id)"
  if [ -z "$field" ] || [ "$field" = "123" ] || [ "$field" = "0" ]; then
    say "        telegram.chat_id='$field' ★ 占位值或为空，通知永远发不出去"
    issues=$((issues + 1))
  else
    say "        telegram.chat_id=$field"
  fi

  field="$(json_field "$body" webhook_secret_configured)"
  if [ "$field" != "true" ]; then
    say "        webhook_secret_configured=$field ★ JMAP Push 回调无法通过签名校验"
    say "        改配置后需要重新 POST /api/push/register 才生效。"
    issues=$((issues + 1))
  else
    say "        webhook_secret_configured=true"
  fi

  field="$(json_field "$body" session_url)"
  if [ -z "$field" ]; then
    say "        jmap.session_url 为空 ★ 取不到邮件列表"
    issues=$((issues + 1))
  else
    say "        jmap.session_url=$field"
  fi

  field="$(json_field "$body" worker_token_configured)"
  if [ "$field" != "true" ]; then
    say "        worker_token_configured=$field ★ /worker 会一律 401，队列永远排不掉"
    issues=$((issues + 1))
  fi

  # 3) 依赖可达性：Redis、JMAP、Telegram bot token。
  request GET "$DEBUG_URL/debug/redis" "$DEBUG_TOKEN"
  if [ "$CODE" = "200" ]; then
    body="$BODY"
    field="$(json_field "$body" reachable)"
    code="$(json_field "$body" global_enabled)"
    if [ "$field" != "true" ]; then
      say "  [3] Redis 不可达 detail=$(json_field "$body" detail) ★ 队列与游标都不可用"
      issues=$((issues + 1))
    else
      say "  [3] Redis 可达，global_enabled=$code"
      if [ "$code" = "false" ]; then
        say "        ★ 全局开关被关闭：所有出站 no-op，通知不会发"
        issues=$((issues + 1))
      fi
    fi
  else
    say "  [3] GET /debug/redis -> ${CODE:-请求失败}：无法确认 Redis 状态"
    issues=$((issues + 1))
  fi

  request GET "$DEBUG_URL/debug/telegram" "$DEBUG_TOKEN"
  if [ "$CODE" = "200" ]; then
    body="$BODY"
    field="$(json_field "$body" ok)"
    if [ "$field" != "true" ]; then
      say "  [4] Telegram bot token 无效 detail=$(json_field "$body" detail)"
      say "        ★ 通知永远发不出去，与 cron 无关"
      issues=$((issues + 1))
    else
      say "  [4] Telegram bot token 有效（getMe 成功）"
    fi
  else
    say "  [4] GET /debug/telegram -> ${CODE:-请求失败}"
    issues=$((issues + 1))
  fi

  request GET "$DEBUG_URL/debug/jmap" "$DEBUG_TOKEN"
  if [ "$CODE" = "200" ]; then
    body="$BODY"
    field="$(json_field "$body" ok)"
    if [ "$field" != "true" ]; then
      say "  [5] JMAP 会话不可达 detail=$(json_field "$body" detail)"
      say "        ★ 拉不到新邮件，对账与通知都不会有内容"
      issues=$((issues + 1))
    else
      say "  [5] JMAP 会话可达"
    fi
  else
    say "  [5] GET /debug/jmap -> ${CODE:-请求失败}"
    issues=$((issues + 1))
  fi

  # 6) 游标位置：有游标说明 /reconcile 跑过；null 说明从未成功对账。
  request GET "$DEBUG_URL/debug/worker" "$DEBUG_TOKEN"
  if [ "$CODE" = "200" ]; then
    body="$BODY"
    field="$(json_field "$body" reconcile_cursor)"
    if [ -z "$field" ] || [ "$field" = "null" ]; then
      say "  [6] reconcile_cursor=null ★ /reconcile 从未成功执行过"
      issues=$((issues + 1))
    else
      say "  [6] reconcile_cursor=$field"
    fi
  else
    say "  [6] GET /debug/worker -> ${CODE:-请求失败}"
    issues=$((issues + 1))
  fi

  say ""
  if [ "$issues" -gt 0 ]; then
    say "结论：上面标 ★ 的 $issues 项要先修；修完再确认 cron 确实在执行本脚本"
    say "      （crontab -l / kubectl get cronjob，或手动 bash 本脚本 --once 一次）。"
    FAILURES=$((FAILURES + issues))
  else
    say "结论：配置与依赖全部正常。收不到提醒多半是调度没跑——"
    say "      确认 cron 确实在执行本脚本，并看它的退出码是否为 0。"
  fi
}

# --- --test-notify：绕过队列直发一条，验证 bot token 与 chat_id 本身是否可用 ------
test_notify() {
  local text="${TEST_TEXT:-[messageweave] cron-drain direct-send test}"
  local code rc body

  rc=0
  curl -sS \
    --max-time "$TIMEOUT" \
    --connect-timeout 10 \
    -X POST \
    -H "Accept: application/json" \
    -H "Authorization: Bearer $DEBUG_TOKEN" \
    -G \
    --data-urlencode "text=$text" \
    -D "$BODY_FILE.h" \
    -o "$BODY_FILE" \
    -w '%{http_code}' \
    "$DEBUG_URL/debug/notify" >"$BODY_FILE.code" 2>/dev/null || rc=$?
  code=$(cat "$BODY_FILE.code" 2>/dev/null)
  body=$(cat "$BODY_FILE" 2>/dev/null)
  if [ "$rc" -ne 0 ] || [ -z "$code" ]; then
    say "--test-notify: 请求未发出（需要 MW_DEBUG_DOMAIN 与 MW_DEBUG_TOKEN）"
    FAILURES=$((FAILURES + 1))
    return 0
  fi
  case "$code" in
    200)
      say "--test-notify: 200 已直发到 chat_id=$(json_field "$body" chat_id)（复用生产出站路径，不经队列）"
      say "      没收到 → 问题在 chat_id 填错或 Telegram 侧，与 cron 无关"
      ;;
    401)
      say "--test-notify: 401 MW_DEBUG_TOKEN 无效"
      FAILURES=$((FAILURES + 1))
      ;;
    403)
      say "--test-notify: 403 目标 chat_id 不在 allowlist 内"
      FAILURES=$((FAILURES + 1))
      ;;
    502)
      say "--test-notify: 502 Telegram 发送失败 ★ bot token 或 chat_id 不可用，与 cron 无关"
      FAILURES=$((FAILURES + 1))
      ;;
    503)
      say "--test-notify: 503 业务未就绪，先保存配置"
      FAILURES=$((FAILURES + 1))
      ;;
    *)
      say "--test-notify: $code ${body:0:300}"
      FAILURES=$((FAILURES + 1))
      ;;
  esac
}

# --- 主流程 --------------------------------------------------------------------
main() {
  local do_drain=1
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --once) MODE="--once" ;;
      --loop) MODE="--loop" ;;
      --diagnose) DO_DIAGNOSE=1 ;;
      --test-notify) DO_TEST_NOTIFY=1 ;;
      --help) usage 0 ;;
      --app-domain=*) APP_URL="${1#--app-domain=}" ;;
      --app-domain) shift; APP_URL="${1:?--app-domain 需要值}" ;;
      --app-url=*) APP_URL="${1#--app-url=}" ;;
      --app-url) shift; APP_URL="${1:?--app-url 需要值}" ;;
      --worker-domain=*) WORKER_URL="${1#--worker-domain=}" ;;
      --worker-domain) shift; WORKER_URL="${1:?--worker-domain 需要值}" ;;
      --worker-url=*) WORKER_URL="${1#--worker-url=}" ;;
      --worker-url) shift; WORKER_URL="${1:?--worker-url 需要值}" ;;
      --debug-domain=*) DEBUG_URL="${1#--debug-domain=}" ;;
      --debug-domain) shift; DEBUG_URL="${1:?--debug-domain 需要值}" ;;
      --debug-url=*) DEBUG_URL="${1#--debug-url=}" ;;
      --debug-url) shift; DEBUG_URL="${1:?--debug-url 需要值}" ;;
      --reconcile-token=*) RECONCILE_TOKEN="${1#--reconcile-token=}" ;;
      --reconcile-token) shift; RECONCILE_TOKEN="${1:?--reconcile-token 需要值}" ;;
      --worker-token=*) WORKER_TOKEN="${1#--worker-token=}" ;;
      --worker-token) shift; WORKER_TOKEN="${1:?--worker-token 需要值}" ;;
      --debug-token=*) DEBUG_TOKEN="${1#--debug-token=}" ;;
      --debug-token) shift; DEBUG_TOKEN="${1:?--debug-token 需要值}" ;;
      --batch=*) BATCH="${1#--batch=}" ;;
      --batch) shift; BATCH="${1:?--batch 需要值}" ;;
      --timeout=*) TIMEOUT="${1#--timeout=}" ;;
      --timeout) shift; TIMEOUT="${1:?--timeout 需要值}" ;;
      --interval=*) INTERVAL="${1#--interval=}" ;;
      --interval) shift; INTERVAL="${1:?--interval 需要值}" ;;
      --log-file=*) LOG_FILE="${1#--log-file=}" ;;
      --log-file) shift; LOG_FILE="${1:?--log-file 需要值}" ;;
      --test-text=*) TEST_TEXT="${1#--test-text=}" ;;
      --test-text) shift; TEST_TEXT="${1:?--test-text 需要值}" ;;
      *) die "未知参数：$1（--help 查看用法）" ;;
    esac
    shift
  done

  # 命令行 flag 优先于环境变量；未显式指定时回退到默认值。
  WORKER_URL="${WORKER_URL:-$APP_URL}"
  DEBUG_URL="${DEBUG_URL:-$WORKER_URL}"
  # 没有协议前缀的域名自动补 https://
  APP_URL="$(normalize_url "$APP_URL")"
  WORKER_URL="$(normalize_url "$WORKER_URL")"
  DEBUG_URL="$(normalize_url "$DEBUG_URL")"

  [ -n "$APP_URL" ] || [ -n "$WORKER_URL" ] || [ -n "$DEBUG_URL" ] \
    || usage_die "缺少 --app-domain / --worker-domain / --debug-domain 之一（或对应 MW_* 环境变量）"
  for pair in "TIMEOUT=$TIMEOUT" "INTERVAL=$INTERVAL" "BATCH=$BATCH"; do
    [ -n "${pair#*=}" ] || continue
    case "${pair#*=}" in
      *[!0-9]*) die "${pair%%=*} 必须是整数" ;;
    esac
  done
  [ "$TIMEOUT" -ge 1 ] || die "--timeout / MW_TIMEOUT 至少 1 秒"
  if [ -n "$BATCH" ]; then
    [ "$BATCH" -ge 1 ] || die "--batch / MW_BATCH 至少 1"
    if [ "$BATCH" -gt 10 ]; then
      log "MW_BATCH=$BATCH 超过服务端上限 10，实际按 10 处理"
    fi
  fi

  # 诊断 / 直发测试是独立模式，不跑真正的排空。诊断缺 token 时也能降级跑公开端点。
  if [ "$DO_DIAGNOSE" -eq 1 ] || [ "$DO_TEST_NOTIFY" -eq 1 ]; then
    do_drain=0
    [ "$DO_TEST_NOTIFY" -eq 1 ] && [ -z "$DEBUG_TOKEN" ] && die "直发测试需要 --debug-token / MW_DEBUG_TOKEN"
  else
    [ -n "$WORKER_URL" ] || usage_die "缺少 --worker-domain / MW_WORKER_DOMAIN（默认取 --app-domain）"
    [ -n "$WORKER_TOKEN" ] || usage_die "缺少 --worker-token / MW_WORKER_TOKEN（/worker 是排空的唯一入口）"
  fi

  BODY_FILE="$(mktemp -d 2>/dev/null)/body" || die "无法创建临时目录"
  trap 'rm -rf "$(dirname "$BODY_FILE")"' EXIT

  [ "$DO_DIAGNOSE" -eq 1 ] && diagnose
  [ "$DO_TEST_NOTIFY" -eq 1 ] && test_notify

  if [ "$do_drain" -eq 1 ]; then
    if [ "$MODE" = "--loop" ]; then
      trap 'log "收到退出信号，循环结束"; exit 0' TERM INT
      log "--loop 启动 --interval=${INTERVAL}s --timeout=${TIMEOUT}s 按 Ctrl-C 退出"
      while :; do
        run_reconcile
        run_worker
        sleep "$INTERVAL"
      done
    else
      run_reconcile
      run_worker
    fi
  fi

  if [ "$FAILURES" -gt 0 ]; then exit 1; fi
  exit 0
}

main "$@"
