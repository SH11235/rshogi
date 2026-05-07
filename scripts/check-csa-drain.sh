#!/usr/bin/env bash
# rshogi-csa-server-workers の `/api/v1/games/live` を polling し、
# in-flight 対局が 0 件で N 回連続観測されるまで待機する best-effort drain script。
#
# 用途:
#   - GitHub Actions deploy-workers.yml の production deploy 前 step
#   - ローカル手動 deploy 前確認 (`bash scripts/check-csa-drain.sh --live-url ... && wrangler deploy`)
#
# 設計詳細: docs/csa-server/deployment.md §12 を参照。

set -u
set -o pipefail

LIVE_URL=""
MAX_WAIT_SEC=3900
POLL_INTERVAL_SEC=30
REQUIRE_STABLE_ZERO=3
RETRY_ON_FETCH_ERROR=3
USER_AGENT="rshogi-deploy-drain/1.0"

EXIT_DRAINED=0
EXIT_TIMEOUT=1
EXIT_FETCH_ERROR=2
EXIT_USAGE=3

usage() {
    cat <<'EOF' >&2
Usage: check-csa-drain.sh --live-url <fullpath> [options]

Required:
  --live-url <url>           Full URL of /api/v1/games/live (no query string)

Options:
  --max-wait-sec <N>         Max total wait seconds (default 3900 = ~65min)
  --poll-interval-sec <N>    Seconds between polls (default 30)
  --require-stable-zero <N>  Consecutive zero-count observations required (default 3)
  --retry-on-fetch-error <N> Per-poll retry count on 5xx/network error (default 3)
  --user-agent <ua>          Override User-Agent header

Exit codes:
  0  drained (N consecutive zero observations)
  1  timeout (max-wait-sec elapsed before drained)
  2  fetch error (5xx persisted across retries, or response schema invalid)
  3  usage error
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --live-url)
            LIVE_URL="${2:-}"
            shift 2
            ;;
        --max-wait-sec)
            MAX_WAIT_SEC="${2:-}"
            shift 2
            ;;
        --poll-interval-sec)
            POLL_INTERVAL_SEC="${2:-}"
            shift 2
            ;;
        --require-stable-zero)
            REQUIRE_STABLE_ZERO="${2:-}"
            shift 2
            ;;
        --retry-on-fetch-error)
            RETRY_ON_FETCH_ERROR="${2:-}"
            shift 2
            ;;
        --user-agent)
            USER_AGENT="${2:-}"
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "[drain] unknown argument: $1" >&2
            usage
            exit "$EXIT_USAGE"
            ;;
    esac
done

if [ -z "$LIVE_URL" ]; then
    echo "[drain] --live-url is required" >&2
    usage
    exit "$EXIT_USAGE"
fi

# query string が既に含まれている URL は cursor append 時の `?` / `&` 分岐を生むため
# 設計上拒否する。フルパスは plain `/api/v1/games/live` を渡す運用に統一。
case "$LIVE_URL" in
    *\?*)
        echo "[drain] --live-url must not contain query string (got: $LIVE_URL)" >&2
        exit "$EXIT_USAGE"
        ;;
esac

# 数値引数を `[[ =~ ^[0-9]+$ ]]` で early validate。算術式 `$(( ... ))` や
# `[ "$N" -ge ... ]` で発生する不明瞭なシェルエラーを usage error に整える。
for var_name in MAX_WAIT_SEC POLL_INTERVAL_SEC REQUIRE_STABLE_ZERO RETRY_ON_FETCH_ERROR; do
    val="${!var_name}"
    if ! [[ "$val" =~ ^[0-9]+$ ]]; then
        flag_name="--$(echo "$var_name" | tr '[:upper:]_' '[:lower:]-')"
        echo "[drain] invalid value for $flag_name: expected non-negative integer, got: $val" >&2
        exit "$EXIT_USAGE"
    fi
done

if ! command -v jq >/dev/null 2>&1; then
    echo "[drain] jq is required but not found in PATH" >&2
    exit "$EXIT_USAGE"
fi
if ! command -v curl >/dev/null 2>&1; then
    echo "[drain] curl is required but not found in PATH" >&2
    exit "$EXIT_USAGE"
fi

# `curl` を test から function override 可能にするため、本 script では明示的に
# コマンド名 `curl` を経由する。bats 側で `curl() { ... }` を export して mock する。

# 1 polling tick: 全ページ巡回して live_games の合計件数を取得。
# 成功時は count を stdout に出力し exit 0、5xx / parse 失敗時は exit 1 を返す。
# 各 fetch は --retry-on-fetch-error 回まで再試行 (per-tick counter)。
fetch_live_count_once() {
    local url="$1"
    local total=0
    local cursor=""
    local body status fetch_url attempt rc
    local max_retries="$RETRY_ON_FETCH_ERROR"

    while :; do
        if [ -n "$cursor" ]; then
            fetch_url="${url}?cursor=$(jq -nr --arg v "$cursor" '$v|@uri')"
        else
            fetch_url="$url"
        fi

        attempt=1
        body=""
        while :; do
            rc=0
            # `-w '\n%{http_code}'` で stdout 末尾に HTTP ステータスを出力。
            # body と status を 1 回の curl 呼び出しで取り出すための idiom。
            body=$(curl -sS --max-time 15 \
                -A "$USER_AGENT" \
                -H "X-CSA-Internal-Poll: deploy-drain" \
                -w '\n%{http_code}' \
                "$fetch_url") || rc=$?
            status="${body##*$'\n'}"
            body="${body%$'\n'*}"

            if [ "$rc" -eq 0 ] && [ "$status" -ge 200 ] && [ "$status" -lt 300 ]; then
                break
            fi

            if [ "$attempt" -ge "$max_retries" ]; then
                echo "[drain] fetch failed (rc=$rc status=${status:-n/a}) after $attempt attempts: $fetch_url" >&2
                return 1
            fi
            echo "[drain] fetch transient error (rc=$rc status=${status:-n/a}); retry $attempt/$max_retries in 10s" >&2
            attempt=$((attempt + 1))
            sleep 10
        done

        # schema 検証: `live_games` が array、`next_cursor` が null か string であることを
        # `jq -e` で gate する。配列以外 / 欠落 / object 等は `length` が 0 を返し
        # fail-open で drained 誤判定するリスクがあるため、必ず array 確認を通す。
        local count
        if ! count=$(printf '%s' "$body" | jq -er '
            if (.live_games | type) != "array" then
                error("live_games is not an array (type=\(.live_games | type))")
            elif (has("next_cursor") | not) then
                error("next_cursor field missing")
            elif (.next_cursor != null and (.next_cursor | type) != "string") then
                error("next_cursor must be null or string (type=\(.next_cursor | type))")
            else
                .live_games | length
            end
        ' 2>&1); then
            echo "[drain] schema validation failed for $fetch_url: $count" >&2
            return 1
        fi
        if ! [[ "$count" =~ ^[0-9]+$ ]]; then
            echo "[drain] unexpected live_games length: $count" >&2
            return 1
        fi
        total=$((total + count))

        # `// ""` で null/absent を空文字に正規化する。schema check で type が
        # null|string であることは保証済なので、空文字判定だけで終端を識別できる。
        cursor=$(printf '%s' "$body" | jq -r '.next_cursor // ""' 2>/dev/null) || cursor=""
        if [ -z "$cursor" ]; then
            break
        fi
    done

    printf '%s\n' "$total"
}

START_EPOCH=$(date +%s)
STABLE=0
LAST_COUNT=-1

trap 'echo "[drain] interrupted (SIGTERM); flushing state and exiting" >&2; emit_result 0; exit '"$EXIT_TIMEOUT" TERM
trap 'echo "[drain] interrupted (SIGINT); flushing state and exiting" >&2; emit_result 0; exit '"$EXIT_TIMEOUT" INT

emit_result() {
    local drained="$1"
    local now elapsed
    now=$(date +%s)
    elapsed=$((now - START_EPOCH))
    local drained_str
    if [ "$drained" = "0" ]; then
        drained_str="false"
    else
        drained_str="true"
    fi
    printf '{"drained":%s,"waited_sec":%d,"final_count":%d,"stable_observations":%d}\n' \
        "$drained_str" "$elapsed" "$LAST_COUNT" "$STABLE"
}

while :; do
    if ! COUNT=$(fetch_live_count_once "$LIVE_URL"); then
        emit_result 0
        exit "$EXIT_FETCH_ERROR"
    fi
    LAST_COUNT="$COUNT"

    NOW=$(date +%s)
    ELAPSED=$((NOW - START_EPOCH))

    if [ "$COUNT" -eq 0 ]; then
        STABLE=$((STABLE + 1))
        echo "[drain] elapsed=${ELAPSED}s count=0 stable=${STABLE}/${REQUIRE_STABLE_ZERO}" >&2
        if [ "$STABLE" -ge "$REQUIRE_STABLE_ZERO" ]; then
            echo "[drain] drained: ${STABLE} consecutive zero observations" >&2
            emit_result 1
            exit "$EXIT_DRAINED"
        fi
    else
        if [ "$STABLE" -gt 0 ]; then
            echo "[drain] elapsed=${ELAPSED}s count=$COUNT (resetting stable counter from $STABLE)" >&2
        else
            echo "[drain] elapsed=${ELAPSED}s count=$COUNT" >&2
        fi
        STABLE=0
    fi

    if [ "$ELAPSED" -ge "$MAX_WAIT_SEC" ]; then
        echo "[drain] timeout: elapsed=${ELAPSED}s >= max=${MAX_WAIT_SEC}s, last_count=$LAST_COUNT" >&2
        emit_result 0
        exit "$EXIT_TIMEOUT"
    fi

    sleep "$POLL_INTERVAL_SEC"
done
