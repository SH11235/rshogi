#!/usr/bin/env bash
set -euo pipefail

# notify_on_pid.sh
# 既存プロセスの終了を待って、ntfy.sh に通知を送るユーティリティ
# - 監視対象は --pid もしくは --cmd（pgrep -f の正規表現）で指定
# - トピックは既定で `shogi-$(uuidgen | tr -d '-' | tr A-Z a-z)` を生成
# - Windows 側は https://ntfy.sh/<topic> をブラウザで開き通知を許可するだけ
#
# 使い方:
#   scripts/utils/notify_on_pid.sh \
#     (--pid 12345 | --cmd 'regex') \
#     [--topic <topic>] [--notify-start] [--priority 3] [--tags checkered_flag] \
#     [--title 'Job Finished'] [--ntfy https://ntfy.sh] [--why '説明'] [--touch /tmp/done]
#
# 例:
#   # PID で監視
#   scripts/utils/notify_on_pid.sh --pid 3323557 --notify-start --why 'AB metrics run'
#   # コマンドパターンで最も古いプロセスを監視（デフォルト）
#   scripts/utils/notify_on_pid.sh --cmd 'scripts/analysis/run_ab_metrics.sh --dataset runs/20251112-2014-tuning' --notify-start

print_usage() {
  cat <<USAGE >&2
Usage: $0 (--pid <PID> | --cmd <regex>) [options]

Options:
  --pid N               監視する PID
  --cmd REGEX           監視対象を pgrep -f REGEX で抽出
  --select oldest|newest  REGEX 指定時の選択（既定: oldest）
  --topic NAME          通知トピック。未指定時は shogi-<uuid> を生成
  --notify-start        監視開始時にも通知を送る（トピック伝達用）
  --title STR           Title ヘッダ（既定: Job Finished）
  --priority N          1..5（既定: 3）
  --tags CSV            例: checkered_flag,white_check_mark（既定: checkered_flag）
  --ntfy URL            ntfy ベース URL（既定: https://ntfy.sh）
  --why STR             本ジョブの説明（本文に追記）
  --touch PATH          完了時に PATH を touch
  -h, --help            このヘルプ
USAGE
}

# 依存コマンド確認
need() { command -v "$1" >/dev/null 2>&1 || { echo "Error: '$1' not found" >&2; exit 127; }; }
need curl

PID=""
CMD_RE=""
SELECT="oldest"
TOPIC=""
TITLE="Job Finished"
PRIORITY="3"
TAGS="checkered_flag"
NTFY_BASE="https://ntfy.sh"
WHY=""
TOUCH_PATH=""
NOTIFY_START=false

if [ $# -eq 0 ]; then
  print_usage; exit 1
fi

while [ $# -gt 0 ]; do
  case "$1" in
    --pid) PID="${2:?}"; shift 2;;
    --cmd) CMD_RE="${2:?}"; shift 2;;
    --select) SELECT="${2:?}"; shift 2;;
    --topic) TOPIC="${2:?}"; shift 2;;
    --notify-start) NOTIFY_START=true; shift 1;;
    --title) TITLE="${2:?}"; shift 2;;
    --priority) PRIORITY="${2:?}"; shift 2;;
    --tags) TAGS="${2:?}"; shift 2;;
    --ntfy) NTFY_BASE="${2:?}"; shift 2;;
    --why) WHY="${2:?}"; shift 2;;
    --touch) TOUCH_PATH="${2:?}"; shift 2;;
    -h|--help) print_usage; exit 0;;
    *) echo "Unknown arg: $1" >&2; print_usage; exit 1;;
  esac
done

if [ -z "$PID" ] && [ -z "$CMD_RE" ]; then
  echo "Error: --pid または --cmd を指定してください" >&2
  exit 1
fi

if [ -n "$CMD_RE" ] && [ -z "$PID" ]; then
  need pgrep
  case "$SELECT" in
    oldest) PID=$(pgrep -f -o -- "$CMD_RE" || true);;
    newest) PID=$(pgrep -f -n -- "$CMD_RE" || true);;
    *) echo "Error: --select は oldest|newest" >&2; exit 1;;
  esac
  if [ -z "$PID" ]; then
    echo "Error: pgrep で対象が見つかりません: $CMD_RE" >&2
    exit 2
  fi
fi

if [ -z "$TOPIC" ]; then
  # 既定トピック: shogi-$(uuidgen | tr -d '-' | tr A-Z a-z)
  gen_uuid() {
    if command -v uuidgen >/dev/null 2>&1; then
      uuidgen
    elif [ -r /proc/sys/kernel/random/uuid ]; then
      cat /proc/sys/kernel/random/uuid
    elif command -v openssl >/dev/null 2>&1; then
      openssl rand -hex 16
    else
      date +%s%N
    fi
  }
  uid=$(gen_uuid)
  uid=$(printf '%s' "$uid" | tr -d '-' | tr 'A-Z' 'a-z')
  TOPIC="shogi-$uid"
fi

SUB_URL="${NTFY_BASE%/}/$TOPIC"

# 監視対象情報の収集（cmdline は NUL 区切り）
CMDLINE=""
if [ -r "/proc/$PID/cmdline" ]; then
  CMDLINE=$(tr '\0' ' ' < "/proc/$PID/cmdline" || true)
fi

HOST=$(hostname 2>/dev/null || echo host)
USER_NAME=$(id -un 2>/dev/null || echo user)
TTY_NAME=$(ps -o tty= -p $$ 2>/dev/null || echo "?")

echo "[notify_on_pid] topic=$TOPIC (${SUB_URL})" >&2
echo "[notify_on_pid] watching PID=$PID on $HOST (user=$USER_NAME tty=$TTY_NAME)" >&2
if [ -n "$CMDLINE" ]; then echo "[notify_on_pid] cmdline=$CMDLINE" >&2; fi

send_ntfy() {
  local title="$1"; shift
  local tags="$1"; shift
  local body="$1"; shift
  curl -sS -X POST \
    -H "Title: ${title}" \
    -H "Priority: ${PRIORITY}" \
    -H "Tags: ${tags}" \
    -d "$body" \
    "$SUB_URL" >/dev/null
}

NOW_ISO() { date -Iseconds; }

OBSERVED_AT=$(NOW_ISO)
OBSERVED_TS=$(date +%s)

if [ "$NOTIFY_START" = true ]; then
  BODY_START=$(cat <<EOS
Monitoring started on $HOST
pid=$PID
user=$USER_NAME tty=$TTY_NAME
cmdline=${CMDLINE:-N/A}
observed_at=$OBSERVED_AT
why=${WHY:-}
EOS
)
  send_ntfy "Monitoring Started" "hourglass_flowing_sand" "$BODY_START" || true
fi

# 終了待ち（対象が既に終了している場合は即時通知）
ALREADY_DONE=false
if kill -0 "$PID" 2>/dev/null; then
  # tail --pid は対象終了で即時復帰
  tail --pid="$PID" -f /dev/null || true
else
  ALREADY_DONE=true
fi

FINISHED_AT=$(NOW_ISO)
FINISHED_TS=$(date +%s)
WAIT_SECS=$(( FINISHED_TS - OBSERVED_TS ))

BODY_FIN=$(cat <<EOS
Job finished on $HOST
pid=$PID
user=$USER_NAME tty=$TTY_NAME
cmdline=${CMDLINE:-N/A}
observed_at=$OBSERVED_AT
finished_at=$FINISHED_AT
waited_secs=$WAIT_SECS
already_done=$ALREADY_DONE
why=${WHY:-}
subscribe=${SUB_URL}
EOS
)

send_ntfy "$TITLE" "$TAGS" "$BODY_FIN" || {
  echo "Error: ntfy 送信に失敗しました ($SUB_URL)" >&2
  exit 3
}

if [ -n "$TOUCH_PATH" ]; then
  mkdir -p "$(dirname "$TOUCH_PATH")" 2>/dev/null || true
  : >"$TOUCH_PATH" || true
fi

echo "[notify_on_pid] notified -> $SUB_URL (title='$TITLE', priority=$PRIORITY, tags=$TAGS)" >&2

