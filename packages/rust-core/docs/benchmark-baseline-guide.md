# ベンチマークベースライン管理ガイド

## 概要

ベンチマークのベースライン管理により、性能の経時変化を追跡し、リグレッションを検出できます。本ガイドでは、`parallel_benchmark` ツールのベースライン機能と、スクリプトによる管理方法を説明します。

## parallel_benchmark の組み込みベースライン機能

### 基本的な使用方法

```bash
# 1. 初回実行：ベースラインを作成
cargo run --release --bin parallel_benchmark -- \
  --threads 1,2,4 \
  --fixed-total-ms 1000 \
  --dump-json baseline.json

# 2. 以降の実行：ベースラインと比較
cargo run --release --bin parallel_benchmark -- \
  --threads 1,2,4 \
  --fixed-total-ms 1000 \
  --baseline baseline.json \
  --dump-json current.json

# 3. CI用：厳密モード（回帰検知で失敗）
cargo run --release --bin parallel_benchmark -- \
  --baseline baseline.json \
  --strict
```

### 回帰検知基準

- **実効スピードアップ**: 5%以上の低下で警告
- **重複率**: 10%以上の増加で警告

### 出力例

```
=== REGRESSION CHECK ===
Performance regression detected for 2 threads!
  Speedup: 1.25x -> 1.15x
  Duplication: 30.0% -> 35.0%
```

## スクリプトによるベースライン管理

### 使用方法

```bash
# 初回実行
./scripts/benchmark-baseline.sh run

# ベースラインとして保存
./scripts/benchmark-baseline.sh save v1.0

# 以降の実行で自動比較
./scripts/benchmark-baseline.sh run

# 特定のベースラインと比較
./scripts/benchmark-baseline.sh run v1.0

# ベースライン一覧
./scripts/benchmark-baseline.sh list
```

### スクリプトの特徴

- マシン固有のベースライン管理
- システム情報の自動収集（CPU、メモリ、Rustバージョン）
- 複数バージョンの管理

## CI環境でのベースライン管理

### 1. GitHub Actions Cache

```yaml
name: Benchmark
on: [push, pull_request]

jobs:
  benchmark:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v3
      
      - name: Cache baseline
        uses: actions/cache@v3
        with:
          path: baseline.json
          key: benchmark-baseline-${{ runner.os }}-${{ hashFiles('Cargo.lock') }}
          restore-keys: |
            benchmark-baseline-${{ runner.os }}-
      
      - name: Run benchmark
        run: |
          if [ -f baseline.json ]; then
            cargo run --release --bin parallel_benchmark -- \
              --baseline baseline.json \
              --dump-json current.json \
              --strict
          else
            cargo run --release --bin parallel_benchmark -- \
              --dump-json baseline.json
          fi
      
      - name: Update baseline on main
        if: github.ref == 'refs/heads/main'
        run: cp current.json baseline.json
```

### 2. GitHub Releases

```bash
# リリース時にベースラインを保存
gh release create v1.0.0 baseline.json

# ベースラインのダウンロード
wget https://github.com/user/repo/releases/download/v1.0.0/baseline.json
```

### 3. 専用ブランチ

```bash
# benchmark-dataブランチに結果を保存
git checkout benchmark-data
mkdir -p baselines/$(date +%Y/%m)
cp benchmark-result.json baselines/$(date +%Y/%m)/$(date +%d)-$GITHUB_SHA.json
git add baselines/
git commit -m "Add benchmark baseline for $GITHUB_SHA"
git push
```

## JSON形式のベースライン

### 構造

```json
{
  "metadata": {
    "version": "0.1.0",
    "commit_hash": "abc123def456",
    "cpu_info": {
      "model": "AMD Ryzen 9 5950X",
      "cores": 16,
      "threads": 32
    },
    "timestamp": 1754699601,
    "config": {
      "tt_size_mb": 256,
      "num_threads": [1, 2, 4],
      "depth_limit": 8
    }
  },
  "results": [
    {
      "thread_count": 1,
      "mean_nps": 410550.0,
      "std_dev": 131777.16,
      "avg_speedup": 1.0,
      "helper_share_pct": 16.31,
      "effective_speedup": 1.0
    }
  ]
}
```

### メタデータの活用

- **環境の一貫性確認**: CPU、コンパイラバージョンの変更検出
- **設定の追跡**: TTサイズ、スレッド数などの設定変更
- **再現性**: コミットハッシュによる正確なバージョン特定

## ベストプラクティス

### 1. 測定の安定性

```bash
# 複数回実行して安定性を確認
for i in {1..5}; do
  cargo run --release --bin parallel_benchmark -- \
    --iterations 10 \
    --dump-json run-$i.json
done

# 結果の統計分析
# （標準偏差が平均の10%以内であることを確認）
```

### 2. ベースラインの更新タイミング

- **更新すべき場合**:
  - 意図的な性能改善後
  - アルゴリズムの大幅な変更後
  - 新機能追加後

- **更新すべきでない場合**:
  - バグ修正のみの変更
  - リファクタリング
  - ドキュメント更新

### 3. 複数環境での管理

```bash
# 環境ごとのベースライン
baseline-linux-x86_64.json
baseline-macos-arm64.json
baseline-windows-x86_64.json

# 環境に応じた比較
if [[ "$OSTYPE" == "linux-gnu"* ]]; then
  BASELINE="baseline-linux-x86_64.json"
elif [[ "$OSTYPE" == "darwin"* ]]; then
  BASELINE="baseline-macos-arm64.json"
fi
```

## トラブルシューティング

### ベースラインとの大きな乖離

1. **環境変更の確認**
   ```bash
   # メタデータの比較
   jq '.metadata' baseline.json current.json
   ```

2. **他のプロセスの影響**
   ```bash
   # CPUガバナーを performance に設定
   sudo cpupower frequency-set -g performance
   ```

3. **測定のばらつき**
   ```bash
   # イテレーション数を増やす
   --iterations 20
   ```

### CI での不安定な結果

- 専用のベンチマーク runner を使用
- 時間帯を固定（例：夜間のみ実行）
- 許容範囲を広げる（5% → 10%）

## 関連ドキュメント

- [並列探索ベンチマークガイド](performance/parallel-benchmark-guide.md)
- [パフォーマンスドキュメント](performance/README.md)

## 更新履歴

| 日付 | 内容 |
|------|------|
| 2025-08-09 | parallel_benchmark の組み込み機能を追加 |
