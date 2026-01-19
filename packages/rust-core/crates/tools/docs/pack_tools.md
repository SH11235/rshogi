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

### shuffle_pack

学習データをシャッフル。学習時のバイアスを防ぐために必須。

```bash
cargo run -p tools --release --bin shuffle_pack -- \
  --input data.pack --output shuffled.pack
```

| オプション | 説明 | デフォルト |
|------------|------|------------|
| `-i, --input` | 入力ファイル | 必須 |
| `-o, --output` | 出力ファイル | 必須 |
| `--seed` | 乱数シード（再現性） | ランダム |
| `--chunk-size` | チャンクサイズ（大規模ファイル用） | 0（全読み込み） |

### rescore_pack

局面に探索スコアを付与。既存データの再評価に使用。

```bash
cargo run -p tools --release --bin rescore_pack -- \
  --input data.pack --output rescored.pack \
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

### preprocess_pack

qsearch leaf置換を適用。局面をqsearchのPV末端に置換。

```bash
cargo run -p tools --release --bin preprocess_pack -- \
  --input data.pack --output processed.pack \
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

### pack_to_jsonl

pack形式をJSONLに変換。デバッグ・内容確認用。

```bash
cargo run -p tools --release --bin pack_to_jsonl -- \
  --input data.pack --output data.jsonl
```

出力例：
```json
{"sfen":"lnsgkgsnl/...","score":123,"depth":0,"best_move":"7g7f","nodes":0}
```

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
cargo run -p tools --release --bin shuffle_pack -- \
  --input runs/selfplay/*.pack --output training_shuffled.pack
```

### 既存の棋譜から生成した場合

スコアがない場合は rescore が必要：

```bash
# 1. 棋譜から学習データ生成（別途 generate_training_data 等で）

# 2. スコア付与
cargo run -p tools --release --bin rescore_pack -- \
  --input data.pack --output rescored.pack \
  --nnue model.nnue --use-qsearch --threads 8

# 3. シャッフル
cargo run -p tools --release --bin shuffle_pack -- \
  --input rescored.pack --output training_shuffled.pack
```

### qsearch leaf置換を適用する場合

学習データの質を向上させる前処理：

```bash
cargo run -p tools --release --bin preprocess_pack -- \
  --input data.pack --output processed.pack \
  --nnue model.nnue --rescore --skip-in-check
```

## 注意事項

- 大規模ファイル（数GB以上）を処理する場合は `--chunk-size` オプションを使用
- `--delete-input` はディスク容量節約に有効だが、元ファイルが削除されるので注意
- スコアのスケール（FV_SCALE）は通常24（nn.bin形式）、nnue-pytorch形式は16
