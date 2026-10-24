# 学習データ処理ツール群

NNUE学習用の PackedSfenValue 形式データを処理するツール群。

## PackedSfenValue 形式

やねうら王互換の学習データ形式（40バイト/レコード）：

| フィールド | サイズ | 説明 |
|------------|--------|------|
| sfen | 32 | PackedSfen（局面） |
| score | 2 | 評価値（i16） |
| move | 2 | Move16 形式。gensfen が直接出力する PSV では最善手ラベル、`pack_to_psv` で pack から展開した PSV では実着手ラベル |
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
| `--ls-bucket-mode` | LayerStacks routing (`progresskpabs` / `kingrank9`) | LS 時必須 |
| `--ls-progress-buckets` | progresskpabs の routing bucket 数 | LS progress 時必須 |
| `--ls-progress-coeff` | progresskpabs 係数ファイル | LS progress 時必須 |
| `--use-qsearch` | qsearch評価を使用 | false |
| `--search-depth` | 深さ指定探索（qsearchと排他） | - |
| `--apply-qsearch-leaf` | qsearch leaf置換も適用 | false |
| `--skip-in-check` | 王手局面をスキップ | false |
| `-t, --threads` | スレッド数（0=自動） | 0 |
| `--delete-input` | 処理後に入力を削除 | false |

### preprocess_psv

qsearch leaf置換を適用し、局面を qsearch の PV 末端に置換します。`--moved-mask` を指定すると、
局面が変わった出力行だけを後段の `psv_select_by_mask` で抽出できる bitmap も同時に生成します。

```bash
cargo run -p tools --release --bin preprocess_psv -- \
  --input data.psv --output processed.psv \
  --nnue model.nnue --rescore \
  --moved-mask moved.bits
```

| オプション | 説明 | デフォルト |
|------------|------|------------|
| `-i, --input` | 入力ファイル | 必須 |
| `-o, --output` | 出力ファイル | 必須 |
| `--moved-mask` | packed SFEN が変わった出力行の LSB-first bitmap 出力先 | なし |
| `--max-ply` | qsearch の最大深さ | 16 |
| `-t, --threads` | 並列処理スレッド数（0=自動） | 1 |
| `--nnue` | NNUEモデルファイル（省略時は Material 評価、`--rescore` には必須） | なし |
| `--ls-bucket-mode` | LayerStacks routing (`progresskpabs` / `kingrank9`) | LS 時必須 |
| `--ls-progress-buckets` | progresskpabs の routing bucket 数 | LS progress 時必須 |
| `--ls-progress-coeff` | progresskpabs 係数ファイル | LS progress 時必須 |
| `--limit` | 処理するレコード数の上限（0=無制限） | 0 |
| `-v, --verbose` | レコード処理エラーの詳細を表示 | false |
| `--no-fix-stm-sign` | 手番反転時の score / game_result 符号補正を無効化 | false |
| `--rescore` | 置換後にNNUEで再評価（推奨） | false |
| `--skip-in-check` | 王手局面をスキップ | false |
| `--score-clip` | `--rescore` 時の score クリップ範囲（±指定値） | 10000 |

mask の byte `j` の bit `k`（LSB が bit 0）は、出力 PSV の record `j * 8 + k` に対応します。
サイズは `ceil(出力 records / 8)` byte で、最終 byte の未使用 bit は 0 です。bit 1 の条件は
出力レコードの先頭 32 byte（packed SFEN）が入力レコードと異なることです。score、move16、
game_ply、game_result だけが変わった行は bit 0 です。

`--skip-in-check` で除外した入力行と、処理エラーの行（パース・unpack 失敗など。破棄されます）
は PSV にも mask にも行を作りません。`--moved-mask` の有無で PSV 出力は変わりません。
後続の bit index は
入力行番号ではなく、実際の出力行番号に詰められます。出力 PSV と mask は `.tmp` へ書き、
正常完了時だけ最終パスへ rename します。input、output、moved mask に同じパスまたは同一実体は
指定できません。output / moved mask が `--nnue` モデルや `--ls-progress-coeff` 係数と
同一実体の場合も拒否します。派生する `.tmp` パスがこれらのパス (NNUE モデル・係数含む) や
互いに衝突する場合も拒否します。

2 ファイルの確定は PSV、mask の順に行うため、両方をまとめたトランザクションではありません。
「新 PSV + 前回 run の古い mask」の組ができないよう、PSV を確定する前に既存の最終 mask を
削除します。mask の rename だけが失敗した場合は PSV のみ確定し、完成済みの mask は `.tmp` に
残します（エラーメッセージにパスを表示）。処理・書き込み中のエラーではどちらも確定せず
`.tmp` を削除します。Ctrl-C による中断も同様にどちらも確定せず、エラー終了（非 0 exit）します。

完了時の `Moved: <件数> (<率>%)` は、実際に書き出した PSV レコード数を分母とします。
`--moved-mask` を省略した場合も統計は表示されます。

#### 変更行だけ DL 再スコアして書き戻す

`psv_select_by_mask` と `psv_scatter_by_mask` を組み合わせる場合、mask の基準は原本
`shard.psv` ではなく `preprocess_psv` の出力 `leaf.psv` です。途中で compact PSV の順序を
変更しないでください。

```bash
# 1. qsearch 葉置換と同時に変更行 mask を生成
cargo run --release -p tools --bin preprocess_psv -- \
  --input shard.psv --output leaf.psv --nnue qsearch.nnue \
  --moved-mask moved.bits

# 2. leaf.psv から変更行だけを入力順に抽出
cargo run --release -p tools --bin psv_select_by_mask -- \
  --input leaf.psv --mask moved.bits --out moved.psv

# 3. compact PSV を DL モデルで再スコア（出力は rescored/moved.psv）
cargo run --release -p tools --bin rescore_psv -- \
  --input moved.psv --output-dir rescored \
  --dlshogi-onnx-model model.onnx

# 4. 同じ mask で score を leaf.psv の対応行へ書き戻す
cargo run --release -p tools --bin psv_scatter_by_mask -- \
  --input leaf.psv --mask moved.bits \
  --compact rescored/moved.psv --out final.psv
```

mask の検証条件と書き戻し時の packed SFEN 照合は
[`psv_select_by_mask`](psv_select_by_mask.md) と
[`psv_scatter_by_mask`](psv_scatter_by_mask.md) を参照してください。DL 推論のセットアップと
高速化オプションは [`rescore_psv`](rescore_psv.md) にあります。

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

### jsonl_to_psv

`tournament` / `analyze_selfplay` 互換の自己対局 JSONL を PSV に変換。
各 `move` 行の `sfen_before` と `move_usi`、`result` 行の勝敗から
PackedSfenValue を生成する。スコアはログ内の `eval.score_cp` / `eval.score_mate`
を暫定値として入れるため、別エンジンで教師スコアを付け直す場合は後段で
`rescore_psv` を実行する。

```bash
cargo run -p tools --release --bin jsonl_to_psv -- \
  --input-dir runs/selfplay \
  --pattern "*.jsonl" \
  --output selfplay_from_jsonl.psv \
  --missing-score zero
```

| オプション | 説明 | デフォルト |
|------------|------|------------|
| `--input` | 入力 JSONL ファイル、ディレクトリ、glob（カンマ区切り可） | - |
| `--input-dir` | 入力ディレクトリ（`--input` と排他） | - |
| `--pattern` | ディレクトリ入力時の glob パターン | `*.jsonl` |
| `-o, --output` | 出力 PSV ファイル | 必須 |
| `--missing-score` | `eval` 欠損局面の扱い。`skip` または `zero` | `skip` |
| `--max-games` | 変換する最大対局数（0=全件） | `0` |

#### 破損ログの扱い

対局中にプロセスが落ちたログは、最終行が改行なしで途中まで書かれていたり、
行が NUL 埋め / 不正 UTF-8 になっていることがある。これらは変換を中断させず、
該当行だけを破棄して健全な行から局面を取り出す。件数は Summary に出る:

- `Truncated tail lines` — 改行なしで切れた最終行。検出時点でそのファイルの読み込みを終える
- `Corrupt lines` — 不正 UTF-8 または JSON として解釈できない行。警告はファイルごと先頭 5 件まで
- `Parse errors` — JSON としては妥当だが `move` / `result` として読めない行
  （必須フィールドの欠落や型不一致。未知の `type` は無視され、ここには入らない）

`result` 行を失った対局は `Orphan games` として局面ごと破棄される（勝敗が確定
できないため）。破損が想定外に多い場合は Summary の件数で気付けるので、変換後に
必ず確認すること。

### expand_psv_from_policy

出力先には入力 PSV・モデル本体・ORT ライブラリと異なるファイルを指定してください。
[出力パスの保護と中断時の扱い](expand_psv_from_policy.md#出力パスの保護)も参照してください。

dlshogi 系 ONNX モデルのポリシー出力を使い、各局面の合法手のうち選択確率が閾値を超える手の
次局面を新しい PSV として書き出す。学習データの局面カバレッジを拡張する用途に使用。

AobaZero (`--onnx-model`) と標準 dlshogi (`--dlshogi-onnx-model`, DL水匠等) の両方に対応。

> **Tip**: スコア付け (rescore) と局面展開 (expand) を同じモデルで両方行う場合は、
> `rescore_psv --expand-output-dir ...` を使うと **推論 1 パス** で両方を同時に実行できる
> （value 出力で rescore、policy 出力で expand）。詳細は
> [rescore_psv.md](rescore_psv.md#ポリシー展開--expand-output-dir) を参照。

**前提条件**: ONNX Runtime のセットアップが必要。詳細は [rescore_psv.md](rescore_psv.md) を参照。

```bash
# ビルド（dlshogi は default で有効。AobaZero も対応にする場合は aobazero-onnx を追加）
cargo build --release -p tools --features aobazero-onnx --bin expand_psv_from_policy

# AobaZero モデルで実行
ORT_DYLIB_PATH=~/lib/onnxruntime-linux-x64-gpu-1.24.2/lib/libonnxruntime.so \
cargo run --release -p tools --features aobazero-onnx --bin expand_psv_from_policy -- \
  --input data.psv --output expanded.psv \
  --onnx-model aoba_model.onnx

# 標準 dlshogi モデル (DL水匠等) で実行
ORT_DYLIB_PATH=~/lib/onnxruntime-linux-x64-gpu-1.24.2/lib/libonnxruntime.so \
cargo run --release -p tools --bin expand_psv_from_policy -- \
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

## 重複除去ツールの選び方

PSV 重複除去には 3 種類のツールを用意している。入力規模・利用可能メモリ・一時ディスクの有無で選ぶ。重複キーはいずれも先頭 32 バイトの PackedSfen（first-wins）。

| ツール | 方式 | 正確性 | メモリ | 一時ディスク | I/O パス | 想定規模 |
|---|---|---|---|---|---|---|
| [`psv_dedup`](../src/bin/psv_dedup.rs) | 全件 `HashSet<u64>` | ほぼ exact (64bit hash 衝突のみ) | ユニーク局面数 × ~16 B | 不要 | 1 | 数億〜数十億 |
| [`psv_dedup_bloom`](psv_dedup_bloom.md) | Blocked Bloom Filter | 近似 (`--fpr` で制御、偽陽性あり) | 固定 (入力規模と `fpr` で決定) | 不要 | 1 | 数百億 (メモリ潤沢) |
| [`psv_dedup_partition`](psv_dedup_partition.md) | ディスクパーティション + `HashSet<[u8;32]>` | **完全 exact** | 最大パーティションのユニーク局面ぶん | 入力と同等 | 2 | 数十億〜数百億 (メモリ限定) |

### 選び方フロー

```
入力 < 数十億件？
├─ Yes → psv_dedup（シンプル・速い）
└─ No
    ├─ メモリに余裕あり（数十 GiB 以上）
    │   ├─ 偽陽性が許容できる → psv_dedup_bloom（1 パス・最速）
    │   └─ exact が必須         → psv_dedup_partition
    └─ メモリが限られている
        └─ 一時ディスクに余裕あり → psv_dedup_partition（exact・低メモリ）
```

### reference モードが必要な場合

既存 dedup 済みファイルとの **差分だけ** を抽出したい場合は `psv_dedup_bloom --reference` または `psv_dedup_partition --reference` が対応（`psv_dedup` は非対応）。近似で良いなら bloom、exact が必須なら partition を選ぶ。

### 重複率の事前調査

本番 dedup の前に重複率を把握したい場合は [`psv_dedup_check`](../src/bin/psv_dedup_check.rs) を使う。
- `--table-size 4G` 等で direct-mapped テーブルを指定すれば固定メモリの近似チェック
- 指定しない場合は `HashMap` で正確カウント (重複度合いの分布も出る)

## 典型的なワークフロー

### gensfen で生成した場合

`gensfen` は探索スコアを同時に記録するため、rescoreは不要：

```bash
# 1. 教師局面生成（スコア付きデータを生成）
cargo run -p tools --release --bin gensfen -- \
  --eval-file eval/model.bin \
  --games 1000 --nodes 80000

# 2. シャッフル
cargo run -p tools --release --bin shuffle_psv -- \
  --input runs/gensfen/*/gensfen.psv --output training_shuffled.psv
```

`--out-dir` で独自パス（例: `data/myrun`）を指定したときも `gensfen.psv` というファイル名は変わらないため、
複数 run を一括収集する場合は `data/*/gensfen.psv` のように glob を調整する。

### 既存の棋譜から生成した場合

スコアがない場合は rescore が必要：

```bash
# 1. 棋譜から学習データ生成（gensfen で PSV 出力、または floodgate_pipeline で SFEN 抽出後に変換）

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
cargo run --release -p tools --bin rescore_psv -- \
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
cargo run --release -p tools --bin expand_psv_from_policy -- \
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

`jsonl_to_psv` は JSONL の `interrupted` / `censored` を未決として除外し、
`Skipped in-progress` に計上する。旧 CSA の `draw` + reason `interrupted` /
`censored` も除外する。旧 `max_moves` は引分として保持する。
