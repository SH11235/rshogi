#!/bin/bash
# build_pgo.sh - PGO (Profile-Guided Optimization) ビルドスクリプト
#
# 使い方:
#   ./scripts/build_pgo.sh              # PGOビルド実行
#   ./scripts/build_pgo.sh --verify     # ビルド後にベンチマークで効果確認
#   ./scripts/build_pgo.sh --clean      # プロファイルデータを削除して終了
#
# 処理フロー:
#   1. プロファイル収集用ビルド (profile-generate)
#   2. ベンチマーク実行でプロファイル収集
#   3. プロファイルデータのマージ
#   4. PGO適用ビルド (profile-use)
#
# 期待される効果: NPS +6-7% (NNUE), +14% (Material)
# 使用プロファイル: production (Full LTO + PGO)

set -euo pipefail

cd "$(dirname "$0")/.."

# 設定（ユーザー固有のディレクトリを使用）
PGO_DATA_DIR="${TMPDIR:-/tmp}/pgo-data-${USER:-$(id -un)}"
PROFILE_FILE="$PGO_DATA_DIR/merged.profdata"
VERIFY=false

# 安全なディレクトリ削除
safe_rm_dir() {
    local dir="$1"
    if [ -z "$dir" ] || [ "$dir" = "/" ] || [ "$dir" = "$HOME" ]; then
        echo "Error: Invalid directory path: $dir"
        exit 1
    fi
    if [ -d "$dir" ]; then
        rm -rf "$dir"
        echo "Removed: $dir"
    else
        echo "Directory does not exist: $dir"
    fi
}

# 引数解析
while [[ $# -gt 0 ]]; do
    case $1 in
        --verify)
            VERIFY=true
            shift
            ;;
        --clean)
            echo "=== Cleaning PGO data ==="
            safe_rm_dir "$PGO_DATA_DIR"
            exit 0
            ;;
        -h|--help)
            echo "Usage: $0 [--verify] [--clean]"
            echo ""
            echo "Options:"
            echo "  --verify  ビルド後にベンチマークで効果確認"
            echo "  --clean   プロファイルデータを削除して終了"
            exit 0
            ;;
        *)
            echo "Unknown option: $1"
            exit 1
            ;;
    esac
done

# llvm-profdata のパスを検索
find_llvm_profdata() {
    # Rust toolchain内のllvm-profdataを探す
    local profdata
    profdata=$(find "$HOME/.rustup" -name "llvm-profdata" -type f -executable 2>/dev/null | head -1)
    if [ -n "$profdata" ]; then
        echo "$profdata"
        return 0
    fi

    # システムのllvm-profdataを探す
    for version in 21 20 19 18 17 16 15 14; do
        if command -v "llvm-profdata-$version" &>/dev/null; then
            echo "llvm-profdata-$version"
            return 0
        fi
    done

    if command -v llvm-profdata &>/dev/null; then
        echo "llvm-profdata"
        return 0
    fi

    return 1
}

LLVM_PROFDATA=$(find_llvm_profdata) || {
    echo "Error: llvm-profdata not found"
    echo "Install with: rustup component add llvm-tools-preview"
    exit 1
}

echo "=============================================="
echo "  PGO Build Script"
echo "=============================================="
echo ""
echo "llvm-profdata: $LLVM_PROFDATA"
echo "Profile dir:   $PGO_DATA_DIR"
echo ""

# Step 1: プロファイル収集用ビルド
echo "=== Step 1/4: Building with profile generation ==="
safe_rm_dir "$PGO_DATA_DIR"
mkdir -p "$PGO_DATA_DIR"

echo "Cleaning previous build artifacts..."
if ! cargo clean 2>&1; then
    echo "Warning: cargo clean failed, but continuing..."
fi

echo "Building with profile generation (production profile)..."
if ! RUSTFLAGS="-C target-cpu=native -C profile-generate=$PGO_DATA_DIR" \
    cargo build --profile production 2>&1 | tail -5; then
    echo "Error: Profile generation build failed"
    exit 1
fi

echo ""

# Step 2: プロファイル収集
echo "=== Step 2/4: Collecting profile data ==="

# ベンチマークバイナリの存在確認
if [ ! -x ./target/production/benchmark ]; then
    echo "Error: Benchmark binary not found or not executable"
    exit 1
fi

echo "Running benchmark to collect profile..."
if ! ./target/production/benchmark 2>&1 | grep -E "(Threads|Avg NPS|---|^[0-9])"; then
    echo "Error: Benchmark execution failed"
    echo "Profile data may be incomplete. Aborting PGO build."
    exit 1
fi

echo ""

# Step 3: プロファイルマージ
echo "=== Step 3/4: Merging profile data ==="
PROFRAW_COUNT=$(find "$PGO_DATA_DIR" -name "*.profraw" 2>/dev/null | wc -l)
echo "Found $PROFRAW_COUNT profile files"

if [ "$PROFRAW_COUNT" -eq 0 ]; then
    echo "Error: No profile data collected (.profraw files not found)"
    echo "This may indicate that the benchmark did not run successfully."
    exit 1
fi

"$LLVM_PROFDATA" merge -o "$PROFILE_FILE" "$PGO_DATA_DIR"/*.profraw
PROFILE_SIZE=$(ls -lh "$PROFILE_FILE" | awk '{print $5}')
echo "Merged profile: $PROFILE_FILE ($PROFILE_SIZE)"

echo ""

# Step 4: PGO適用ビルド
echo "=== Step 4/4: Building with PGO ==="

echo "Cleaning for PGO build..."
if ! cargo clean 2>&1; then
    echo "Warning: cargo clean failed, but continuing..."
fi

echo "Building with PGO profile (production profile)..."
if ! RUSTFLAGS="-C target-cpu=native -C profile-use=$PROFILE_FILE" \
    cargo build --profile production 2>&1 | tail -5; then
    echo "Error: PGO build failed"
    exit 1
fi

echo ""
echo "=============================================="
echo "  PGO Build Complete"
echo "=============================================="
echo ""
echo "Binaries:"
ls -lh ./target/production/engine-usi ./target/production/benchmark 2>/dev/null | awk '{print "  " $9 " (" $5 ")"}'
echo ""

# 効果確認（オプション）
if [ "$VERIFY" = true ]; then
    echo "=== Verification Benchmark ==="
    ./target/production/benchmark 2>&1 | grep -E "(Threads|Avg NPS|---|^[0-9])"
    echo ""
fi

echo "Done. Profile data saved in: $PGO_DATA_DIR"
echo ""
echo "To rebuild with same profile:"
echo "  RUSTFLAGS=\"-C target-cpu=native -C profile-use=$PROFILE_FILE\" cargo build --profile production"
