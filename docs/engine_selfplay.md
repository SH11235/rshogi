# engine_selfplay — 自己対局ハーネス

engine-usi 同士の自己対局を実行し、棋譜ログ・学習データ・統計情報を出力するツール。

## ビルド

```bash
cargo build -p tools --bin engine_selfplay --release
```

リリースビルドのバイナリは `target/release/engine_selfplay` に生成される。
以降の例では `cargo run -p tools --bin engine_selfplay --` を使用するが、ビルド済みバイナリを直接実行しても同じ。

## クイックスタート

```bash
# 10局・1秒秒読み（最も基本的な使い方）
cargo run -p tools --bin engine_selfplay -- \
  --games 10 --byoyomi 1000

# 学習データを生成しながら100局
cargo run -p tools --bin engine_selfplay -- \
  --games 100 --byoyomi 1000 --concurrency 4
```

## 出力ファイル

`--out` を指定しない場合、タイムスタンプ付きディレクトリが自動生成される:

```
runs/selfplay/20260317-120000/
  selfplay.jsonl          # メタ情報 + 全手順・結果ログ（JSONL形式）
  selfplay.summary.jsonl  # 対局セッション全体のサマリ
  selfplay.psv            # 学習データ（PackedSfenValue, 40バイト/局面）
  selfplay.kif            # KIF形式の棋譜（複数局は selfplay_g01.kif ...）
  selfplay.info.jsonl     # info ログ（--log-info 指定時のみ）
  selfplay.eval.txt       # 評価値推移（--emit-eval-file 指定時のみ）
  selfplay.metrics.jsonl  # 対局メトリクス（--emit-metrics 指定時のみ）
```

`--out path/to/output.jsonl` を指定した場合は、指定パスの親ディレクトリに上記ファイルが生成される。

## CLI オプション一覧

### 対局制御

| オプション | デフォルト | 説明 |
|-----------|-----------|------|
| `--games N` | 1 | 対局数 |
| `--max-moves N` | 512 | 1局の最大手数（超過で引き分け） |
| `--concurrency N` | 1 | 並行ワーカー数。エンジンプロセスがワーカー数×2個起動する |

### 時間制御

| オプション | デフォルト | 説明 |
|-----------|-----------|------|
| `--byoyomi MS` | 0 | 秒読み（ミリ秒）。全て0の場合は自動で1000msが設定される |
| `--btime MS` | 0 | 先手の持ち時間（ミリ秒） |
| `--wtime MS` | 0 | 後手の持ち時間（ミリ秒） |
| `--binc MS` | 0 | 先手のインクリメント（ミリ秒） |
| `--winc MS` | 0 | 後手のインクリメント（ミリ秒） |
| `--depth N` | なし | 探索深さ制限（`go depth N`） |
| `--nodes N` | なし | 探索ノード数制限（`go nodes N`） |
| `--timeout-margin-ms MS` | 1000 | タイムアウト検出の安全マージン |

### エンジン設定

| オプション | 説明 |
|-----------|------|
| `--engine-path PATH` | エンジンバイナリパス（両手番共通） |
| `--engine-path-black PATH` | 先手のエンジンバイナリ（個別指定） |
| `--engine-path-white PATH` | 後手のエンジンバイナリ（個別指定） |
| `--engine-args ARG...` | エンジンに渡す追加引数 |
| `--engine-args-black ARG...` | 先手エンジンの追加引数 |
| `--engine-args-white ARG...` | 後手エンジンの追加引数 |
| `--usi-option "Name=Value"` | USIオプション（複数指定可） |
| `--usi-option-black "Name=Value"` | 先手のUSIオプション |
| `--usi-option-white "Name=Value"` | 後手のUSIオプション |
| `--threads N` | Threadsオプション（デフォルト: 1） |
| `--threads-black N` | 先手のスレッド数 |
| `--threads-white N` | 後手のスレッド数 |
| `--hash-mb N` | ハッシュサイズ（MiB、デフォルト: 1024） |
| `--network-delay N` | NetworkDelay USIオプション |
| `--network-delay2 N` | NetworkDelay2 USIオプション |
| `--minimum-thinking-time N` | MinimumThinkingTime USIオプション |
| `--slowmover N` | SlowMover USIオプション |
| `--ponder` | USI_Ponder を有効化 |

### 開始局面

| オプション | 説明 |
|-----------|------|
| `--startpos-file FILE` | 開始局面ファイル（1行1局面、USI position形式） |
| `--sfen SFEN` | 単一の開始局面（SFENまたはUSI positionコマンド） |
| `--random-startpos` | 開始局面をランダムに選択（デフォルトは順番に巡回） |

開始局面ファイルの形式:
```
position startpos
position startpos moves 7g7f 3c3d
position sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1
```

### 出力制御

| オプション | デフォルト | 説明 |
|-----------|-----------|------|
| `--out PATH` | 自動生成 | 出力JSONLのパス |
| `--log-info` | false | エンジンのinfo出力をログに記録 |
| `--flush-each-move` | false | 毎手フラッシュ（安全だが低速） |
| `--emit-eval-file` | false | 評価値推移ファイルを出力 |
| `--emit-metrics` | false | 対局メトリクスJSONLを出力 |

### 学習データ

| オプション | デフォルト | 説明 |
|-----------|-----------|------|
| `--output-training-data PATH` | `<output>.psv` | 学習データ出力先（PackedSfenValue形式） |
| `--no-training-data` | false | 学習データ出力を無効化 |
| `--skip-initial-ply N` | 0 | 序盤N手をスキップ（定跡部分の除外） |
| `--skip-in-check BOOL` | true | 王手局面をスキップ |

学習データは PackedSfenValue 形式（40バイト/局面）で、Nodchip learner互換。
手数制限やタイムアウトで終了した対局（InProgress）の局面は含まれない。

### パス権（特殊ルール）

| オプション | 説明 |
|-----------|------|
| `--pass-rights-black N` | 先手のパス権数 |
| `--pass-rights-white N` | 後手のパス権数 |

パス権有効時は学習データ出力が自動的に無効化される（PackedSfen形式がパス権非対応のため）。

### 中断・再開

| オプション | 説明 |
|-----------|------|
| `--resume` | 前回中断したセッションを再開する |

## 中断・再開（Resume）

長時間実行を中断して後で再開できる。

### 仕組み

1. Ctrl-C で中断すると、実行中の対局を完了させてからグレースフルに終了する
2. 完了済みの対局データはすべて出力ファイルに書き込まれる
3. `--resume` 付きで同じコマンドを再実行すると、出力JSONLから完了済み対局数を自動検出して続きから実行する

### 使い方

```bash
# 初回実行（途中でCtrl-Cで中断）
cargo run -p tools --bin engine_selfplay -- \
  --games 100000 --byoyomi 1000 --concurrency 30 \
  --out runs/selfplay/large_run/selfplay.jsonl

# 再開（同じ引数 + --resume）
cargo run -p tools --bin engine_selfplay -- \
  --games 100000 --byoyomi 1000 --concurrency 30 \
  --out runs/selfplay/large_run/selfplay.jsonl \
  --resume
```

### 注意事項

- `--resume` には `--out` の指定が必須（自動生成パスでは前回のファイルを特定できないため）
- `--games` は合計の目標対局数を指定する（追加分ではない）
- 設定の一致は検証されない。同じCLI引数で再実行すること
- 学習データ（.psv）、info ログ、eval ファイルなどもすべて追記される

## 使用例

### 基本的な自己対局

```bash
# 1秒秒読み、300手制限で10局
cargo run -p tools --bin engine_selfplay -- \
  --games 10 --max-moves 300 --byoyomi 1000
```

### 学習データ生成（実用的なレシピ）

```bash
# depth 9 / nodes 50000 制限、10並列で1万局
# 1手あたりの探索量を固定して安定した教師データを得る
cargo run -p tools --bin engine_selfplay --release -- \
  --games 10000 --depth 9 --nodes 50000 \
  --threads 1 --concurrency 10 \
  --skip-initial-ply 8 \
  --out runs/selfplay/train_d9n50k/selfplay.jsonl

# 中断後に再開する場合
cargo run -p tools --bin engine_selfplay --release -- \
  --games 10000 --depth 9 --nodes 50000 \
  --threads 1 --concurrency 10 \
  --skip-initial-ply 8 \
  --out runs/selfplay/train_d9n50k/selfplay.jsonl \
  --resume

# nodes 制限のみで大量生成（depth なし）
cargo run -p tools --bin engine_selfplay --release -- \
  --games 100000 --nodes 10000 \
  --threads 1 --concurrency 30 \
  --skip-initial-ply 8 \
  --out runs/selfplay/train_n10k/selfplay.jsonl

# byoyomi ベースで品質重視（1手5秒、4スレッド）
cargo run -p tools --bin engine_selfplay --release -- \
  --games 10000 --byoyomi 5000 \
  --threads 4 --concurrency 8 \
  --skip-initial-ply 8 \
  --out runs/selfplay/train_5s/selfplay.jsonl
```

**パラメータ選択の目安:**

| 方式 | 速度 | 品質 | 用途 |
|------|------|------|------|
| `--depth 9 --nodes 50000` | 中 | 中〜高 | 汎用。探索量が安定し再現性が高い |
| `--nodes 10000` | 高 | 中 | 大量局面の生成。浅い探索だが量でカバー |
| `--byoyomi 1000` | 中 | 中 | 時間ベース。ハードウェア依存で品質が変動 |
| `--byoyomi 5000 --threads 4` | 低 | 高 | 品質重視。少量だが深い探索 |

- `--threads 1 --concurrency N` はCPUコア数に応じて N を調整（目安: コア数の80%程度）
- `--threads T --concurrency N` では合計 `T × N × 2` プロセスが起動する点に注意
- `--skip-initial-ply 8` は序盤の定跡手順をスキップする標準設定

### 大規模学習データ生成

```bash
# 30並行で10万局、学習データを指定パスに出力
cargo run -p tools --bin engine_selfplay --release -- \
  --games 100000 --byoyomi 1000 --concurrency 30 \
  --skip-initial-ply 8 \
  --out runs/selfplay/train_100k/selfplay.jsonl \
  --output-training-data runs/selfplay/train_100k/train.psv
```

### 異なるエンジン同士の対局

```bash
# エンジンAを先手、エンジンBを後手にして対局
cargo run -p tools --bin engine_selfplay -- \
  --games 100 --byoyomi 5000 \
  --engine-path-black ./engine_a \
  --engine-path-white ./engine_b \
  --threads-black 4 --threads-white 4
```

### 深さ制限での対局（学習データ生成向け）

```bash
# depth 6 で高速に大量局面を生成
cargo run -p tools --bin engine_selfplay -- \
  --games 50000 --depth 6 --concurrency 30 \
  --skip-initial-ply 8
```

### 特定局面からの対局

```bash
# SFENファイルの局面を順番に使用
cargo run -p tools --bin engine_selfplay -- \
  --games 1000 --byoyomi 1000 \
  --startpos-file positions.txt

# ランダムに選択
cargo run -p tools --bin engine_selfplay -- \
  --games 1000 --byoyomi 1000 \
  --startpos-file positions.txt --random-startpos
```

### 詳細なデバッグ情報付き

```bash
# infoログ + 評価値推移 + メトリクス を全て出力
cargo run -p tools --bin engine_selfplay -- \
  --games 5 --byoyomi 5000 \
  --log-info --emit-eval-file --emit-metrics --flush-each-move
```

## 出力形式の詳細

### JSONL（selfplay.jsonl）

各行が独立したJSONオブジェクト。`type` フィールドで種別を判別:

- `"meta"`: セッション設定（1行目に1回のみ）
- `"move"`: 各手の詳細（SFEN、指し手、評価値、消費時間など）
- `"result"`: 対局結果（`outcome`: `"black_win"` / `"white_win"` / `"draw"`）

### PackedSfenValue（.psv）

バイナリ形式、1局面40バイト:

| オフセット | サイズ | フィールド |
|-----------|--------|-----------|
| 0 | 32 | PackedSfen（局面データ） |
| 32 | 2 | score（探索評価値、手番視点、cp） |
| 34 | 2 | move16（最善手） |
| 36 | 2 | game_ply（手数） |
| 38 | 1 | game_result（1=勝ち, 0=引き分け, -1=負け、手番視点） |
| 39 | 1 | padding |
