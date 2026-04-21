# tools

将棋エンジン開発用ツール群

## ツール一覧

### 対局・データ生成

| ツール | 説明 |
|--------|------|
| `tournament` | 複数エンジンの round-robin 並列トーナメント、SPRT 検定 |
| `analyze_selfplay` | tournament 出力の集計・Elo/nElo 算出・SPRT post-hoc 判定 |
| `engine_selfplay` | USIエンジン同士の自己対局、学習データ（PackedSfenValue）生成 |
| `floodgate_pipeline` | Floodgate棋譜のダウンロード・変換 |

### 学習データ処理

| ツール | 説明 |
|--------|------|
| `shuffle_psv` | PSV ファイルのシャッフル |
| `rescore_psv` | 局面の再評価（探索スコア付与） |
| `preprocess_psv` | PSV ファイルの前処理（qsearch leaf置換等） |
| `validate_psv` | PSV ファイルの不正局面検出・除去 |
| `psv_to_jsonl` | PSV 形式 → JSONL 変換（デバッグ・確認用） |
| `fix_scores` | スコアの補正 |
| `psv_dedup` / `psv_dedup_bloom` / `psv_dedup_partition` | PSV 局面の重複除去（3 方式。使い分けは [pack_tools.md](docs/pack_tools.md#重複除去ツールの選び方)） |

### ベンチマーク・分析

| ツール | 説明 |
|--------|------|
| `benchmark` | エンジン性能ベンチマーク |
| `compare_eval_nnue` | NNUE評価値の比較 |

### NNUE 学習

NNUE モデルの学習には [bullet-shogi](https://github.com/SH11235/bullet-shogi/tree/shogi-support) を使用しています。
教師データは上記の PSV ツール群で生成・前処理し、bullet-shogi で学習を行います。

## クイックスタート

### 自己対局で学習データ生成

```bash
cargo run -p tools --release --bin engine_selfplay -- \
  --games 100 --byoyomi 1000
# → runs/selfplay/<timestamp>-selfplay.psv
```

### 学習データのシャッフル

```bash
cargo run -p tools --release --bin shuffle_psv -- \
  --input data.psv --output shuffled.psv
```

### ベンチマーク実行

```bash
cargo run -p tools --release --bin benchmark -- --internal
```

## ドキュメント

各ツールの詳細は `docs/` を参照：

- [tournament](docs/tournament.md) - 並列トーナメント・SPRT 検定
- [engine_selfplay](docs/engine_selfplay.md) - 自己対局ツールの詳細
- [benchmark](docs/benchmark.md) - ベンチマークツールの詳細
- [pack_tools](docs/pack_tools.md) - 学習データ処理ツール群

各ツールのオプション一覧は `--help` で確認できます。

## 使用例

より多くのコマンド例は [examples/README.md](examples/README.md) を参照。
