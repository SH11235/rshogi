#!/bin/bash
# ローカルベンチマークのベースライン管理スクリプト

set -e

BASELINE_DIR="${HOME}/.shogi-benchmark/baselines"
CURRENT_DIR="$(pwd)"
MACHINE_ID=$(hostname)-$(uname -m)

# コマンドライン引数の処理
COMMAND=${1:-run}
BASELINE_NAME=${2:-default}

# ディレクトリの準備
mkdir -p "$BASELINE_DIR/$MACHINE_ID"

case "$COMMAND" in
    "run")
        echo "Running benchmark..."
        cargo build --release --bin parallel_benchmark
        
        # システム情報を自動取得
        echo "Collecting system information..."
        
        # CPU情報の取得（lscpuが無い場合の代替手段も含む）
        if command -v lscpu &> /dev/null; then
            CPU_MODEL=$(lscpu | grep 'Model name' | cut -d':' -f2 | xargs || echo "Unknown")
        elif [ -f /proc/cpuinfo ]; then
            CPU_MODEL=$(grep 'model name' /proc/cpuinfo | head -1 | cut -d':' -f2 | xargs || echo "Unknown")
        else
            CPU_MODEL="Unknown"
        fi
        
        # メモリ情報の取得
        if command -v free &> /dev/null; then
            MEMORY=$(free -h | grep Mem | awk '{print $2}' || echo "Unknown")
        elif [ -f /proc/meminfo ]; then
            MEMORY=$(awk '/MemTotal/ {print $2/1024/1024 "G"}' /proc/meminfo || echo "Unknown")
        else
            MEMORY="Unknown"
        fi
        
        # アーキテクチャ情報
        ARCH=$(uname -m || echo "Unknown")
        
        # Rust情報の取得
        RUST_VERSION=$(rustc --version || echo "Unknown")
        
        # Git情報の取得（リポジトリ外でも動作するように）
        if git rev-parse --git-dir > /dev/null 2>&1; then
            GIT_COMMIT=$(git rev-parse HEAD 2>/dev/null || echo "none")
            GIT_BRANCH=$(git branch --show-current 2>/dev/null || echo "none")
        else
            GIT_COMMIT="none"
            GIT_BRANCH="none"
        fi
        
        # JSONの作成（jqを使用して安全に作成）
        if command -v jq &> /dev/null; then
            jq -n \
                --arg hostname "$(hostname)" \
                --arg cpu "$CPU_MODEL" \
                --arg cores "$(nproc)" \
                --arg memory "$MEMORY" \
                --arg os "$(uname -s)" \
                --arg kernel "$(uname -r)" \
                --arg arch "$ARCH" \
                --arg rust_version "$RUST_VERSION" \
                --arg timestamp "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
                --arg git_commit "$GIT_COMMIT" \
                --arg git_branch "$GIT_BRANCH" \
                '{
                    hostname: $hostname,
                    cpu: $cpu,
                    cores: ($cores | tonumber),
                    memory: $memory,
                    os: $os,
                    kernel: $kernel,
                    arch: $arch,
                    rust_version: $rust_version,
                    timestamp: $timestamp,
                    git_commit: $git_commit,
                    git_branch: $git_branch
                }' > system-info.json
        else
            # jqがない場合は従来の方法で作成（エスケープに注意）
            cat > system-info.json <<EOF
{
    "hostname": "$(hostname)",
    "cpu": "$CPU_MODEL",
    "cores": $(nproc),
    "memory": "$MEMORY",
    "os": "$(uname -s)",
    "kernel": "$(uname -r)",
    "arch": "$ARCH",
    "rust_version": "$RUST_VERSION",
    "timestamp": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
    "git_commit": "$GIT_COMMIT",
    "git_branch": "$GIT_BRANCH"
}
EOF
        fi
        
        # ベンチマーク実行
        # クイックモードの判定
        if [ "${QUICK:-0}" = "1" ]; then
            echo "Running in quick mode (depth=4, limited threads)..."
            ./target/release/parallel_benchmark \
                --threads 1,2,4 \
                --depth 4 \
                --skip-stop-latency \
                --output benchmark-result.json
        else
            ./target/release/parallel_benchmark \
                --threads 1,2,4,8,$(nproc) \
                --depth 10 \
                --output benchmark-result.json
        fi
        
        # 結果とシステム情報を結合
        jq -s '.[0] * {system_info: .[1]}' benchmark-result.json system-info.json > complete-result.json
        
        # ベースラインとの比較（存在する場合）
        BASELINE_FILE="$BASELINE_DIR/$MACHINE_ID/$BASELINE_NAME.json"
        if [ -f "$BASELINE_FILE" ]; then
            echo "Comparing with baseline '$BASELINE_NAME'..."
            cargo run --release --bin benchmark_compare -- \
                "$BASELINE_FILE" \
                complete-result.json \
                --tolerance 2.0 \
                --format text
        else
            echo "No baseline found for '$BASELINE_NAME' on this machine."
        fi
        
        # 結果を保存
        TIMESTAMP=$(date +%Y%m%d-%H%M%S)
        RESULT_FILE="$BASELINE_DIR/$MACHINE_ID/history/${BASELINE_NAME}_${TIMESTAMP}.json"
        mkdir -p "$(dirname "$RESULT_FILE")"
        cp complete-result.json "$RESULT_FILE"
        echo "Result saved to: $RESULT_FILE"
        ;;
        
    "save")
        # 現在の結果をベースラインとして保存
        if [ ! -f "complete-result.json" ]; then
            echo "Error: No benchmark result found. Run 'benchmark-baseline.sh run' first."
            exit 1
        fi
        
        BASELINE_FILE="$BASELINE_DIR/$MACHINE_ID/$BASELINE_NAME.json"
        cp complete-result.json "$BASELINE_FILE"
        echo "Baseline '$BASELINE_NAME' saved for machine '$MACHINE_ID'"
        ;;
        
    "list")
        # 保存されているベースラインを表示
        echo "Available baselines for $MACHINE_ID:"
        ls -la "$BASELINE_DIR/$MACHINE_ID/"*.json 2>/dev/null || echo "No baselines found."
        ;;
        
    "compare")
        # 2つのベースラインを比較
        BASELINE1=${2:-default}
        BASELINE2=${3:-current}
        
        cargo run --release --bin benchmark_compare -- \
            "$BASELINE_DIR/$MACHINE_ID/$BASELINE1.json" \
            "$BASELINE_DIR/$MACHINE_ID/$BASELINE2.json" \
            --format text
        ;;
        
    "export")
        # ベースラインをエクスポート（他のマシンと共有用）
        tar -czf "baseline-export-$MACHINE_ID-$(date +%Y%m%d).tar.gz" \
            -C "$BASELINE_DIR" \
            "$MACHINE_ID"
        echo "Baselines exported to baseline-export-$MACHINE_ID-$(date +%Y%m%d).tar.gz"
        ;;
        
    *)
        echo "Usage: $0 [command] [baseline_name]"
        echo ""
        echo "Commands:"
        echo "  run [name]      - Run benchmark and compare with baseline"
        echo "  save [name]     - Save current result as baseline"
        echo "  list            - List available baselines"
        echo "  compare [b1] [b2] - Compare two baselines"
        echo "  export          - Export baselines for sharing"
        echo ""
        echo "Options:"
        echo "  QUICK=1         - Run quick benchmark (depth=4, fewer threads)"
        echo ""
        echo "Example:"
        echo "  $0 run            # Run with default baseline"
        echo "  $0 save v1.0      # Save as 'v1.0' baseline"
        echo "  $0 compare v1.0 v1.1"
        echo "  QUICK=1 $0 run    # Quick benchmark for development"
        ;;
esac