# ベンチマークベースライン管理ガイド

## 概要

ベンチマークのベースライン管理により、性能の経時変化を追跡し、リグレッションを検出できます。

## CI環境でのベースライン管理

### 1. GitHub Actions Cache
- 最も簡単な方法
- ブランチごとにベースラインを保持
- 自動的に古いキャッシュは削除される

### 2. GitHub Releases
- 特定のバージョンをベースラインとして固定
- 長期保存に適している
- URLで直接アクセス可能

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

## ローカル環境でのベースライン管理

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

### ベースラインの保存場所

```
~/.shogi-benchmark/
└── baselines/
    └── <hostname>-<arch>/
        ├── default.json      # デフォルトベースライン
        ├── v1.0.json        # バージョン固有
        ├── before-opt.json  # 最適化前
        └── history/         # 実行履歴
            └── default_20250807-143022.json
```

### マシン間での比較

異なるマシン間でのベンチマーク比較は推奨されませんが、相対的な性能変化は参考になります：

```bash
# マシンAでエクスポート
./scripts/benchmark-baseline.sh export

# マシンBでインポート
tar -xzf baseline-export-machineA-20250807.tar.gz -C ~/.shogi-benchmark/baselines/

# 相対性能を確認
./scripts/benchmark-baseline.sh compare machineA/v1.0 machineB/v1.0
```

## ベストプラクティス

### 1. ベースラインの更新タイミング
- 大きな最適化の前後
- リリースバージョンごと
- アーキテクチャ変更時

### 2. 環境の一貫性
- 同じマシンでの比較を基本とする
- CPUガバナーを`performance`に設定
- バックグラウンドプロセスを最小化

```bash
# Linux環境での準備
sudo cpupower frequency-set -g performance
```

### 3. 統計的信頼性
- 複数回実行して平均を取る
- 外れ値を除外
- 標準偏差も記録

### 4. メタデータの記録
- コミットハッシュ
- コンパイラバージョン
- システム構成
- 実行日時

## 高度な使用例

### 継続的なパフォーマンストラッキング

```bash
# 日次ベンチマークスクリプト
#!/bin/bash
DATE=$(date +%Y%m%d)
./scripts/benchmark-baseline.sh run daily
./scripts/benchmark-baseline.sh save daily-$DATE

# 週次レポート生成
for day in $(seq 7); do
    date=$(date -d "$day days ago" +%Y%m%d)
    if [ -f ~/.shogi-benchmark/baselines/*/daily-$date.json ]; then
        echo "Results for $date:"
        ./scripts/benchmark-baseline.sh compare daily daily-$date
    fi
done
```

### プロファイルベースの最適化

```bash
# 最適化前のベースライン
./scripts/benchmark-baseline.sh run
./scripts/benchmark-baseline.sh save before-optimization

# 最適化作業...

# 最適化後の測定
./scripts/benchmark-baseline.sh run
./scripts/benchmark-baseline.sh compare before-optimization default
```