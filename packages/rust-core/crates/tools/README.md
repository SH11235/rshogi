# tools

将棋エンジン開発用ツール群

## ツール一覧

### 対局・データ生成

| ツール | 説明 |
|--------|------|
| `engine_selfplay` | USIエンジン同士の自己対局、学習データ（PackedSfenValue）生成 |
| `generate_training_data` | 棋譜ファイルから学習データを生成 |
| `floodgate_pipeline` | Floodgate棋譜のダウンロード・変換 |

### 学習データ処理

| ツール | 説明 |
|--------|------|
| `shuffle_pack` | PackedSfenValue ファイルのシャッフル |
| `rescore_pack` | 局面の再評価（探索スコア付与） |
| `preprocess_pack` | pack ファイルの前処理（フィルタリング等） |
| `pack_to_jsonl` | pack 形式 → JSONL 変換（デバッグ・確認用） |
| `fix_scores` | スコアの補正 |

### ベンチマーク・分析

| ツール | 説明 |
|--------|------|
| `benchmark` | エンジン性能ベンチマーク |
| `compare_eval_nnue` | NNUE評価値の比較 |

## クイックスタート

### 自己対局で学習データ生成

```bash
cargo run -p tools --release --bin engine_selfplay -- \
  --games 100 --byoyomi 1000
# → runs/selfplay/<timestamp>-selfplay.pack
```

### 学習データのシャッフル

```bash
cargo run -p tools --release --bin shuffle_pack -- \
  --input data.pack --output shuffled.pack
```

### ベンチマーク実行

```bash
cargo run -p tools --release --bin benchmark -- --internal
```

## ドキュメント

各ツールの詳細は `docs/` を参照：

- [benchmark](docs/benchmark.md) - ベンチマークツールの詳細
- [engine_selfplay](docs/engine_selfplay.md) - 自己対局ツールの詳細
- [pack_tools](docs/pack_tools.md) - 学習データ処理ツール群

各ツールのオプション一覧は `--help` で確認できます。

## 使用例

より多くのコマンド例は [examples/README.md](examples/README.md) を参照。
