#!/bin/bash
# MoveGenハング証拠収集スクリプト
# Phase 1: 分類（CPU/IO/ロック）のための証拠を収集

set -euo pipefail

TIMESTAMP=$(date +%Y%m%d_%H%M%S)
DIR="hang_evidence_${TIMESTAMP}"
mkdir -p "$DIR"

echo "=== MoveGen Hang Evidence Collection ==="
echo "Evidence directory: $DIR"

# ハングを起こす
echo "Starting engine with hang-inducing config..."
SKIP_LEGAL_MOVES=0 ./target/release/engine-cli < crates/engine-cli/tests/subprocess_test_positions.txt &
PID=$!
echo "Engine PID: $PID"

# プロセスが開始されるまで待つ
sleep 2

if ! ps -p $PID > /dev/null; then
    echo "ERROR: Process already terminated!"
    exit 1
fi

# 1. CPU使用率とスレッド状態
echo "Collecting thread states..."
ps -L -p $PID -o pid,tid,pcpu,stat,wchan:30,comm > "$DIR/ps_threads.txt"
echo "Thread states saved to $DIR/ps_threads.txt"

# 判定を表示
echo ""
echo "=== CPU Usage Analysis ==="
cat "$DIR/ps_threads.txt"
echo ""

# 高CPU使用率のチェック
if grep -E '(9[0-9]|100)\..*engine-cli' "$DIR/ps_threads.txt" 2>/dev/null || false; then
    echo "⚠️  HIGH CPU DETECTED - Likely a computation loop"
elif grep -E 'futex_wait' "$DIR/ps_threads.txt" 2>/dev/null || false; then
    echo "🔒 FUTEX WAIT DETECTED - Likely a lock/mutex issue"
elif grep -E '(pipe_wait|poll_schedule|do_wait)' "$DIR/ps_threads.txt" 2>/dev/null || false; then
    echo "📝 IO WAIT DETECTED - Likely an I/O blocking issue"
fi

# 2. straceを開始（バックグラウンド）
echo ""
echo "Starting strace..."
strace -f -ttT -e trace=read,write,futex,ppoll,select,epoll_wait -p $PID -o "$DIR/strace.log" 2>/dev/null &
STRACE_PID=$!

# straceが開始されるのを待つ
sleep 1

# 3. SIGUSR1でスタックダンプ
echo "Sending SIGUSR1 for stack dump..."
kill -USR1 $PID
sleep 1

# 4. straceを少し実行
echo "Collecting system calls for 3 seconds..."
sleep 3

# 5. すべて終了
echo "Terminating strace..."
kill $STRACE_PID 2>/dev/null || true

echo "Terminating engine..."
kill -TERM $PID 2>/dev/null || true
sleep 1
kill -KILL $PID 2>/dev/null || true

# 6. ログの解析
echo ""
echo "=== Strace Summary ==="
if [ -f "$DIR/strace.log" ]; then
    echo "Last 20 system calls:"
    tail -20 "$DIR/strace.log" | grep -v "SIGCHLD" || true
    
    echo ""
    echo "Futex calls:"
    grep -c "futex" "$DIR/strace.log" 2>/dev/null || echo "0"
    
    echo "Write calls:"
    grep -c "write" "$DIR/strace.log" 2>/dev/null || echo "0"
fi

# 7. ハングの分類
echo ""
echo "=== Hang Classification ==="
echo "Based on collected evidence:"

CPU_USAGE=$(awk '$3 > 90 {print $3}' "$DIR/ps_threads.txt" | head -1 || true)
FUTEX_COUNT=$(grep -c "futex.*FUTEX_WAIT" "$DIR/strace.log" 2>/dev/null || echo "0")
WRITE_BLOCKED=$(grep -E "write.*<unfinished" "$DIR/strace.log" 2>/dev/null | grep -v "resumed>" | wc -l || echo "0")

if [ -n "$CPU_USAGE" ]; then
    echo "Type: CPU LOOP (${CPU_USAGE}% CPU usage)"
    echo "Action: Check for infinite loops in MoveGen"
elif [ "$FUTEX_COUNT" -gt 5 ]; then
    echo "Type: LOCK/MUTEX WAIT (${FUTEX_COUNT} futex waits)"
    echo "Action: Check for deadlocks or lock ordering issues"
elif [ "$WRITE_BLOCKED" -gt 0 ]; then
    echo "Type: I/O BLOCKING (${WRITE_BLOCKED} blocked writes)"
    echo "Action: Check stderr/stdout buffering"
else
    echo "Type: UNKNOWN"
    echo "Action: Review strace.log manually"
fi

echo ""
echo "Evidence collected in: $DIR/"
echo "Next steps:"
echo "1. Review $DIR/strace.log for patterns"
echo "2. Check stderr output for SIGUSR1 stack trace"
echo "3. Run Phase 2 minimization based on hang type"