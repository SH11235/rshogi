# ベンチマーク結果

このディレクトリには、engine-cliのパフォーマンスベンチマーク結果を保存します。

## 命名規則

- `YYYY-MM-DD_<description>.md` - 日付と説明を含むファイル名
- `latest.md` - 最新の結果（オプション）

## ベンチマーク実行方法

```bash
# 基本的な実行
cargo bench --bench buffering_benchmark --features buffered-io

# 高速テスト実行（開発中）
cargo bench --bench buffering_benchmark --features buffered-io -- \
  --sample-size 10 --warm-up-time 1 --measurement-time 3

# 詳細な実行（リリース前）
cargo bench --bench buffering_benchmark --features buffered-io -- \
  --sample-size 50 --measurement-time 60
```

## 結果の記録方法

1. ベンチマークを実行
2. 出力を新しいファイルに保存：
   ```bash
   # コミット情報を含める
   echo "commit $(git rev-parse HEAD)" > results/YYYY-MM-DD_description.md
   cargo bench --bench buffering_benchmark --features buffered-io >> results/YYYY-MM-DD_description.md
   ```

3. 分析コメントを追加（オプション）

## 結果の比較

異なる日付の結果を比較する場合：

```bash
# 簡易比較
diff results/2025-01-25_buffering_baseline.md results/2025-01-26_optimization.md

# サイドバイサイド比較
sdiff results/2025-01-25_buffering_baseline.md results/2025-01-26_optimization.md
```

## 重要な指標

- **time**: 実行時間の平均値
- **thrpt**: スループット（要素/秒）
- **change**: 前回実行からの変化率
- **outliers**: 外れ値の数

## 注意事項

- ベンチマーク実行時は他の重いプロセスを停止する
- 電源管理設定を「パフォーマンス」モードにする
- 複数回実行して結果の安定性を確認する