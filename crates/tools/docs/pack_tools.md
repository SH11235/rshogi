# 学習データ処理ツール群

NNUE学習用の PackedSfenValue 形式データを処理するツール群。

## PackedSfenValue 形式

やねうら王互換の学習データ形式（40バイト/レコード）：

| フィールド | サイズ | 説明 |
|------------|--------|------|
| sfen | 32 | PackedSfen（局面） |
| score | 2 | 評価値（i16） |
| move | 2 | 最善手（Move16形式） |
| game_ply | 2 | 手数（u16） |
| game_result | 1 | 勝敗（1=勝ち, 0=引分, -1=負け） |
| padding | 1 | パディング |

## ツール一覧

### shuffle_psv

学習データをシャッフル。学習時のバイアスを防ぐために必須。

```bash
cargo run -p tools --release --bin shuffle_psv -- \
  --input data.psv --output shuffled.psv
```

| オプション | 説明 | デフォルト |
|------------|------|------------|
| `-i, --input` | 入力ファイル | 必須 |
| `-o, --output` | 出力ファイル | 必須 |
| `--seed` | 乱数シード（再現性） | ランダム |
| `--chunk-size` | チャンクサイズ（大規模ファイル用） | 0（全読み込み） |

### split_psv

PSV ファイルを複数ファイルへ分割。1 ファイルあたりの局面数または容量を指定できる。
入出力はストリーミングで行うため、大きなファイルでも少しずつ書き出せる。

```bash
# 1 ファイル 1 億局面で分割
cargo run -p tools --release --bin split_psv -- \
  --input data.psv --output-prefix split/train \
  --records-per-file 100000000

# 1 ファイル 4GB 目安で分割
cargo run -p tools --release --bin split_psv -- \
  --input data.psv --output-prefix split/train \
  --bytes-per-file 4GB
```

| オプション | 説明 | デフォルト |
|------------|------|------------|
| `-i, --input` | 入力ファイル | 必須 |
| `--output-prefix` | 出力プレフィックス（`prefix_000.bin` 形式） | 必須 |
| `--records-per-file` | 1 ファイルあたりの局面数 | - |
| `--bytes-per-file` | 1 ファイルあたりの容量（`4GB`, `3500MiB` など） | - |
| `--write-chunk-records` | 1 回の読み書きで扱う局面数 | `1000000` |
| `--start-index` | 出力ファイル番号の開始値 | `0` |
| `--digits` | 出力ファイル番号の最小桁数 | `3` |
| `--suffix` | 出力ファイル拡張子 | `.bin` |

### merge_psv

複数の PSV ファイルを入力順どおりに 1 ファイルへ結合。
`--input-dir` 使用時はファイル名の昇順で処理する。

```bash
# 明示した順序で結合
cargo run -p tools --release --bin merge_psv -- \
  --input split/train_000.bin,split/train_001.bin,split/train_002.bin \
  --output merged.psv

# ディレクトリから結合
cargo run -p tools --release --bin merge_psv -- \
  --input-dir split --pattern "train_*.bin" \
  --output merged.psv
```

| オプション | 説明 | デフォルト |
|------------|------|------------|
| `--input` | 入力ファイル（カンマ区切り） | - |
| `--input-dir` | 入力ディレクトリ（`--input` と排他） | - |
| `--pattern` | `--input-dir` 使用時の glob パターン | `*.bin` |
| `-o, --output` | 出力ファイル | 必須 |
| `--write-chunk-records` | 1 回の読み書きで扱う局面数 | `1000000` |

### rescore_psv

局面に探索スコアを付与。既存データの再評価に使用。

```bash
cargo run -p tools --release --bin rescore_psv -- \
  --input data.psv --output rescored.psv \
  --nnue model.nnue --use-qsearch
```

| オプション | 説明 | デフォルト |
|------------|------|------------|
| `-i, --input` | 入力ファイル | 必須 |
| `-o, --output` | 出力ファイル | 必須 |
| `--nnue` | NNUEモデルファイル | 必須 |
| `--use-qsearch` | qsearch評価を使用 | false |
| `--search-depth` | 深さ指定探索（qsearchと排他） | - |
| `--apply-qsearch-leaf` | qsearch leaf置換も適用 | false |
| `--skip-in-check` | 王手局面をスキップ | false |
| `-t, --threads` | スレッド数（0=自動） | 0 |
| `--delete-input` | 処理後に入力を削除 | false |

### preprocess_psv

qsearch leaf置換を適用。局面をqsearchのPV末端に置換。

```bash
cargo run -p tools --release --bin preprocess_psv -- \
  --input data.psv --output processed.psv \
  --nnue model.nnue --rescore
```

| オプション | 説明 | デフォルト |
|------------|------|------------|
| `-i, --input` | 入力ファイル | 必須 |
| `-o, --output` | 出力ファイル | 必須 |
| `--nnue` | NNUEモデルファイル | - |
| `--rescore` | 置換後にNNUEで再評価（推奨） | false |
| `--skip-in-check` | 王手局面をスキップ | false |
| `-t, --threads` | スレッド数（0=自動） | 1 |

### validate_psv

PSV ファイルの不正局面を検出・除去。学習データの品質チェックに使用。

```bash
# 検出のみ
cargo run -p tools --release --bin validate_psv -- \
  --data data.psv

# ディレクトリ内の全ファイルをチェック
cargo run -p tools --release --bin validate_psv -- \
  --input-dir /path/to/dir --pattern "*.bin"

# 不正レコードを除去して出力
cargo run -p tools --release --bin validate_psv -- \
  --data data.psv --output clean.psv
```

チェック項目：
- PackedSfen の unpack 失敗（ハフマン符号破損等）
- SFEN パースエラー
- 玉の不在、駒数超過、行き所のない駒、二歩
- 手番でない側の玉に王手
- `game_result` が {-1, 0, 1} 以外
- ファイルサイズが 40 バイトの倍数でない（末尾端数）

| オプション | 説明 | デフォルト |
|------------|------|------------|
| `--data` | 入力ファイル（カンマ区切りで複数可） | - |
| `--input-dir` | 入力ディレクトリ（`--data` と排他） | - |
| `--pattern` | `--input-dir` 使用時の glob パターン | `*.bin` |
| `--output` | 出力ファイル（正常レコードのみ書き出し） | - |
| `--max-errors` | 不正レコードの詳細表示件数 | 100 |
| `-t, --threads` | スレッド数（0=自動） | 0 |

### psv_to_jsonl

PSV形式をJSONLに変換。デバッグ・内容確認用。

```bash
cargo run -p tools --release --bin psv_to_jsonl -- \
  --input data.psv --output data.jsonl
```

出力例：
```json
{"sfen":"lnsgkgsnl/...","score":123,"depth":0,"best_move":"7g7f","nodes":0}
```

### expand_psv_from_policy

dlshogi 系 ONNX モデルのポリシー出力を使い、各局面の合法手のうち選択確率が閾値を超える手の
次局面を新しい PSV として書き出す。学習データの局面カバレッジを拡張する用途に使用。

AobaZero (`--onnx-model`) と標準 dlshogi (`--dlshogi-onnx-model`, DL水匠等) の両方に対応。

> **Tip**: スコア付け (rescore) と局面展開 (expand) を同じモデルで両方行う場合は、
> `rescore_psv --expand-output-dir ...` を使うと **推論 1 パス** で両方を同時に実行できる
> （value 出力で rescore、policy 出力で expand）。詳細は
> [rescore_psv.md](rescore_psv.md#ポリシー展開--expand-output-dirについて) を参照。

**前提条件**: ONNX Runtime のセットアップが必要。詳細は [rescore_psv.md](rescore_psv.md) を参照。

```bash
# ビルド（両モデル対応にする場合）
cargo build --release -p tools --features aobazero-onnx,dlshogi-onnx --bin expand_psv_from_policy

# AobaZero モデルで実行
ORT_DYLIB_PATH=~/lib/onnxruntime-linux-x64-gpu-1.24.2/lib/libonnxruntime.so \
cargo run --release -p tools --features aobazero-onnx --bin expand_psv_from_policy -- \
  --input data.psv --output expanded.psv \
  --onnx-model aoba_model.onnx

# 標準 dlshogi モデル (DL水匠等) で実行
ORT_DYLIB_PATH=~/lib/onnxruntime-linux-x64-gpu-1.24.2/lib/libonnxruntime.so \
cargo run --release -p tools --features dlshogi-onnx --bin expand_psv_from_policy -- \
  --input data.psv --output expanded.psv \
  --dlshogi-onnx-model dlshogi_model.onnx
```

| オプション | 説明 | デフォルト |
|------------|------|------------|
| `-i, --input` | 入力 PSV ファイル | 必須 |
| `-o, --output` | 出力 PSV ファイル | 必須 |
| `--onnx-model` | AobaZero ONNX モデル（排他） | - |
| `--dlshogi-onnx-model` | 標準 dlshogi ONNX モデル（排他） | - |
| `--draw-ply` | 引き分け手数（`--onnx-model` 使用時のみ） | 0 |
| `--batch-size` | 推論バッチサイズ | 1024 |
| `--gpu-id` | GPU デバイス ID（-1 で CPU） | 0 |
| `--tensorrt` | TensorRT EP を使用 | false |
| `--tensorrt-cache` | TensorRT エンジンキャッシュディレクトリ | - |
| `--threshold` | 選択確率の閾値（%） | 10.0 |

出力 PSV の `score`、`move16`、`game_result` は 0 で初期化される。
必要に応じて `rescore_psv` でスコアを付与すること。

処理中は進捗バー（局面数、処理速度、経過時間）が表示される。
Ctrl+C で安全に中断可能（現在のバッチ完了後に停止）。

#### 参考性能値

DL水匠15b (55MB) + RTX 3080 Ti, CUDA EP, 50 万局面 (threshold=10%):

| バッチサイズ | 処理時間 | スループット | 展開率 |
|:---:|:---:|:---:|:---:|
| 256 | 66s | ~7,600 局面/s | 2.17x |
| 1024 | 63s | ~8,000 局面/s | 2.17x |
| 2048 | 58s | ~8,600 局面/s | 2.17x |

GPU 推論が律速のため、バッチサイズ 1024〜2048 が推奨。

### fix_scores

スコアの補正処理。

## 典型的なワークフロー

### engine_selfplay で生成した場合

`engine_selfplay` は探索スコアを同時に記録するため、rescoreは不要：

```bash
# 1. 自己対局（スコア付きデータを生成）
cargo run -p tools --release --bin engine_selfplay -- \
  --games 1000 --byoyomi 1000

# 2. シャッフル
cargo run -p tools --release --bin shuffle_psv -- \
  --input runs/selfplay/*.psv --output training_shuffled.psv
```

### 既存の棋譜から生成した場合

スコアがない場合は rescore が必要：

```bash
# 1. 棋譜から学習データ生成（engine_selfplay で PSV 出力、または floodgate_pipeline で SFEN 抽出後に変換）

# 2. スコア付与
cargo run -p tools --release --bin rescore_psv -- \
  --input data.psv --output rescored.psv \
  --nnue model.nnue --use-qsearch --threads 8

# 3. シャッフル
cargo run -p tools --release --bin shuffle_psv -- \
  --input rescored.psv --output training_shuffled.psv
```

### qsearch leaf置換を適用する場合

学習データの質を向上させる前処理：

```bash
cargo run -p tools --release --bin preprocess_psv -- \
  --input data.psv --output processed.psv \
  --nnue model.nnue --rescore --skip-in-check
```

### ポリシーネットワークで局面を拡張する場合

dlshogi モデルの有力手から次局面を生成し、学習データを増やす。
**同じ ONNX モデルで rescore と expand を両方行うなら `rescore_psv` の
`--expand-output-dir` で 1 パス実行** が推奨（GPU 推論が 1 回で済み、I/O も減る）。

```bash
# 1 パス版: rescore + expand を同一推論で同時に実行
cargo run --release -p tools --features dlshogi-onnx --bin rescore_psv -- \
  --input data.psv --output-dir rescored/ \
  --expand-output-dir expanded/ \
  --expand-threshold 10.0 \
  --dlshogi-onnx-model model.onnx \
  --onnx-tensorrt --onnx-tensorrt-cache /tmp/trt_cache

# expanded/data.psv は score=0 で書き出されるため、NNUE で再スコア
cargo run -p tools --release --bin rescore_psv -- \
  --input expanded/data.psv --output-dir rescored_expanded/ \
  --nnue model.nnue --use-qsearch

# 元データと結合してシャッフル
cat rescored/data.psv rescored_expanded/data.psv > combined.psv
cargo run -p tools --release --bin shuffle_psv -- \
  --input combined.psv --output training_shuffled.psv
```

rescore と expand で異なるモデルを使いたい場合、または既存スコア付き PSV から
policy 展開だけ追加したい場合は `expand_psv_from_policy` を単独で使う：

```bash
# 1. ポリシーで局面拡張（確率 10% 超の手の次局面を生成）
cargo run --release -p tools --features dlshogi-onnx --bin expand_psv_from_policy -- \
  --input data.psv --output expanded.psv \
  --dlshogi-onnx-model policy_model.onnx --threshold 10.0

# 2. 拡張局面にスコアを付与（rescore 用モデル）
cargo run -p tools --release --bin rescore_psv -- \
  --input expanded.psv --output-dir rescored/ \
  --nnue model.nnue --use-qsearch

# 3. 元データと結合してシャッフル
cat data.psv rescored/expanded.psv > combined.psv
cargo run -p tools --release --bin shuffle_psv -- \
  --input combined.psv --output training_shuffled.psv
```

## 注意事項

- 大規模ファイル（数GB以上）を処理する場合は `--chunk-size` オプションを使用
- `--delete-input` はディスク容量節約に有効だが、元ファイルが削除されるので注意
- スコアのスケール（FV_SCALE）は通常24（nn.bin形式）、nnue-pytorch形式は16
