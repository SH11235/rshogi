# gensfen — NNUE 学習用教師局面 (PSV/pack) 生成ツール

NativeBackend で `--eval-file` 指定の評価関数を使い、エンジン同士の対局を回しながら
`PackedSfenValue` 形式の教師局面を生成する。棋力評価（Elo 比較・SPRT 等）には
`tournament` バイナリを使うこと。

## ビルド

```bash
cargo build -p tools --bin gensfen --release
```

リリースビルドのバイナリは `target/release/gensfen` に生成される。

## クイックスタート

```bash
# 基本（NativeBackend、1000局、nodes=80000）
./target/release/gensfen \
  --eval-file eval/halfkp_256x2-32-32_crelu/suisho5.bin \
  --games 1000 --nodes 80000

# 30 並列で大規模生成
./target/release/gensfen \
  --eval-file eval/model.bin \
  --startpos-file start_sfens_ply24.txt \
  --games 100000 --nodes 80000 --concurrency 30 --max-moves 320 --hash-mb 128
```

## 出力ファイル

`--out-dir` を指定しない場合、タイムスタンプ付きディレクトリが自動生成される:

```
runs/gensfen/20260317-120000/
  gensfen.jsonl          # 対局結果ログ（result 行のみの JSONL）
  gensfen.psv            # 学習データ（PackedSfenValue, 40バイト/局面）
  gensfen.info.jsonl     # info ログ（--log-info 指定時のみ）
  gensfen.eval.txt       # 評価値推移（--emit-eval-file 指定時のみ）
  gensfen.metrics.jsonl  # 対局メトリクス（--emit-metrics 指定時のみ）
```

`--out-dir path/to/dir` を指定した場合は、そのディレクトリ内に上記ファイルが生成される。

棋譜（KIF）・サマリファイル・全手 move ログは出力されない（教師局面生成に不要なため）。
KIF が必要な場合は `tournament` バイナリで対局を回し、その出力 jsonl を
[`jsonl_to_kif`](tools-reference.md) で変換する。

## 動作モード

デフォルトは **NativeBackend**（`rshogi-core` を直接呼び出すマルチスレッド単一プロセス）。
`--eval-file` で評価関数ファイルの指定が必須。

USI モードを使う場合は `--native=false --engine-path /path/to/usi-engine` を指定する。
このとき `--engine-path-black/white` で先後を別エンジンにすることも可能。

### USI 単一エンジン最適化

USI モードかつ先後同一エンジン・同一引数なら、自動で 1 プロセスで兼用される（プロセス数が半減）。
TT/履歴が共有されるため棋力評価には不向きだが、教師局面生成では問題ない。

## CLI オプション一覧

### 対局制御

| オプション | デフォルト | 説明 |
|-----------|-----------|------|
| `--games N` | 1 | 対局数 |
| `--max-moves N` | 512 | 1局の最大手数（超過で引き分け） |
| `--concurrency N` | 1 | 並行ワーカー数 |

### 時間制御

| オプション | デフォルト | 説明 |
|-----------|-----------|------|
| `--byoyomi MS` | 0 | 秒読み（ミリ秒） |
| `--btime MS` / `--wtime MS` | 0 | 持ち時間（ミリ秒） |
| `--binc MS` / `--winc MS` | 0 | インクリメント（ミリ秒） |
| `--depth N` | なし | 探索深さ制限 |
| `--nodes N` | なし | 探索ノード数制限 |
| `--timeout-margin-ms MS` | 1000 | タイムアウト検出の安全マージン |

`--depth`/`--nodes` 指定時は `NetworkDelay`, `NetworkDelay2`, `MinimumThinkingTime` を
自動で 0 に設定する（USI エンジンの時間管理パラメータが nodes モードに干渉するのを防ぐため）。

### バックエンド・エンジン設定

| オプション | デフォルト | 説明 |
|-----------|-----------|------|
| `--native[=BOOL]` | true | NativeBackend を使用（`--eval-file` 必須） |
| `--eval-file PATH` | (native 時必須) | NNUE 評価関数ファイル |
| `--keep-tt[=BOOL]` | false | TT を対局間で保持（実験用） |
| `--engine-path PATH` | (USI 時必須) | エンジンバイナリパス |
| `--engine-path-black/white PATH` | — | 先後別エンジン |
| `--engine-args ARG...` | — | エンジンに渡す追加引数 |
| `--usi-option "Name=Value"` | — | USI オプション（複数指定可） |
| `--threads N` | 1 | Threads オプション |
| `--hash-mb N` | 1024 | ハッシュサイズ（MiB） |
| `--network-delay N` / `--network-delay2 N` | — | NetworkDelay USI オプション |
| `--minimum-thinking-time N` | — | MinimumThinkingTime USI オプション |
| `--slowmover N` | — | SlowMover USI オプション |
| `--ponder` | false | USI_Ponder を有効化 |

### 開始局面

| オプション | デフォルト | 説明 |
|-----------|-----------|------|
| `--startpos-file FILE` | — | 開始局面ファイル（1行1局面、USI position 形式） |
| `--sfen SFEN` | — | 単一の開始局面 |
| `--random-startpos` | false | 開始局面をランダムに選択（順番巡回ではなく） |
| `--startpos-no-repeat[=BOOL]` | true | 開始局面を重複なしで消費（シャッフル + pop） |
| `--shuffle-seed N` | 自動生成 | 開始局面シャッフルの乱数シード |

開始局面ファイルの形式:
```
position startpos
position startpos moves 7g7f 3c3d
position sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1
```

### 教師局面の取捨

| オプション | デフォルト | 説明 |
|-----------|-----------|------|
| `--output-training-data PATH` | `<out-dir>/gensfen.psv` | 学習データ出力先 |
| `--training-data-format FORMAT` | psv | `psv`（40バイト固定）または `pack`（32バイト + メタ） |
| `--skip-initial-ply N` | 0 | 序盤 1〜N 手目をスキップ |
| `--skip-in-check BOOL` | false | 王手局面をスキップ |

### 重複回避（gensfen 固有）

| オプション | デフォルト | 説明 |
|-----------|-----------|------|
| `--dedup-hash-size N` | 67108864 (64M / 512MB) | 局面 Zobrist ハッシュ重複検出テーブル（0 で無効） |
| `--random-multi-pv N` | 0（無効） | MultiPV ランダム選択の候補数 |
| `--random-multi-pv-diff N` | 32000 | MultiPV 評価値差閾値（centipawns） |
| `--random-move-count N` | 0 | ランダムムーブ回数 |
| `--random-move-min-ply N` | 1 | ランダムムーブ開始手数 |
| `--random-move-max-ply N` | 24 | ランダムムーブ終了手数 |
| `--dedup-warn-interval N` | 1000 | dedup rate 警告のチェック間隔（ゲーム数） |
| `--dedup-warn-rate F` | 0.1 | dedup rate 警告閾値（10%） |

### 補助出力（opt-in）

| オプション | デフォルト | 説明 |
|-----------|-----------|------|
| `--log-info` | false | エンジンの info 出力を `gensfen.info.jsonl` に記録 |
| `--emit-eval-file` | false | 評価値推移を `gensfen.eval.txt` に出力 |
| `--emit-metrics` | false | 対局メトリクス JSONL を出力 |
| `--flush-each-move` | false | 毎手フラッシュ（安全だが低速） |

### 中断・再開

| オプション | 説明 |
|-----------|------|
| `--out-dir DIR` | 出力ディレクトリ（resume 時必須） |
| `--resume` | 前回中断したセッションを再開する |

## 重複回避の詳細

### ハッシュ重複検出（`--dedup-hash-size`）

局面の Zobrist ハッシュをテーブルに記録し、既出局面を検出する。重複検出時は:
1. それまでに蓄積した学習エントリを全クリア
2. 重複局面自体は記録しない
3. 対局は続行（以降のユニーク局面は通常通り記録）

全ワーカーで 1 つのテーブルを共有する（tanuki- と同じ構成）。`AtomicU64` でロックフリーアクセス。
デフォルト 64M エントリ × 8バイト = 512MB。

### 開始局面シャッフル消費（`--startpos-no-repeat`）

開始局面プールをシャッフルし、順番に 1 つずつ消費する。同じ開始局面が 2 回使われない。
プール枯渇時は再シャッフルして 2 周目に入る。

シャッフルの乱数シードは meta 行に `shuffle_seed` として保存される。resume 時は同じ
seed で順列を再構築し、完了済み対局数分だけ進めることで正確な位置を復元する。
`--shuffle-seed` で seed を明示指定することも可能（再現性が必要な場合）。

### MultiPV ランダム選択（`--random-multi-pv`）

探索時に N 候補を評価し、PV1 のスコアとの差が `--random-multi-pv-diff` 以内の候補から
ランダムに選択してプレイする。学習データには PV1 のスコアと手を記録する（局面の真の評価値）。
多様な局面を自然に生成できる。

**推奨ユースケース**: 対局数が開始局面数を大幅に上回る場合（例: 50万局 vs 3万局面プール）。
開始局面の no-repeat だけでは 2 周目以降に同一対局が再現されるため、MultiPV ランダム
またはランダムムーブとの併用を推奨する。

### ランダムムーブ（`--random-move-count`）

序盤の `--random-move-min-ply` 〜 `--random-move-max-ply` の範囲から N 手をランダムに選び、
その手数では合法手からランダムに 1 手選択する（エンジン探索をスキップ）。
ランダムムーブ前の蓄積エントリは全クリアされる（tanuki- 方式）。

### dedup rate 警告

`--dedup-warn-interval`（デフォルト 1000）ゲームごとに直近区間の dedup rate をチェックし、
`--dedup-warn-rate`（デフォルト 0.1 = 10%）を超えると stderr に警告を出力する。
長時間実行中に MultiPV の不足をリアルタイムで検知できる。interval はワーカー数で
自動分割される（`interval / concurrency`、最小 1）。

### MultiPV 値の選定ガイド（実験結果）

NativeBackend、nodes=5000〜10000 での実測値。

**10 局面での周回テスト（局面/game）:**

| MultiPV | 5周 | 10周 |
|---|---|---|
| 0（無効） | 33.8 | — |
| 2 | 78.7 | — |
| 4 | 85.3 | 83.9（微減） |
| 8 | 102.3 | 111.9（維持） |

**1000 局面での周回テスト（MultiPV=8）:**

| 周回 | games | PSV局面数 | 局面/game | 効率 |
|---|---|---|---|---|
| 5周 | 5,000 | 540,750 | 108.2 | ≈100% |
| 10周 | 10,000 | 1,085,122 | 108.5 | ≈100% |

**推奨:**

| games / startpos 比率 | MultiPV |
|---|---|
| ≤ 1倍 | 0（不要） |
| 2-5倍 | 4 |
| 5倍以上 | 8 |
| 10倍以上 | 8 + ランダムムーブ |

## 学習データ形式

PackedSfenValue 形式（40バイト/局面）で、Nodchip learner 互換。

| オフセット | サイズ | フィールド |
|-----------|--------|-----------|
| 0 | 32 | PackedSfen（局面データ） |
| 32 | 2 | score（探索評価値、手番視点、cp） |
| 34 | 2 | move16（最善手） |
| 36 | 2 | game_ply（手数） |
| 38 | 1 | game_result（1=勝ち, 0=引き分け, -1=負け、手番視点） |
| 39 | 1 | padding |

手数制限やタイムアウトで終了した対局（InProgress）の局面は含まれない。

## 中断・再開（Resume）

長時間実行を中断して後で再開できる。

### 仕組み

1. Ctrl-C で中断すると、進行中の対局の完了を待ってからグレースフルに終了する
2. 完了済みの対局データはすべて出力ファイルに書き込まれる
3. `--resume` 付きで同じコマンドを再実行すると、出力 JSONL から完了済み対局数を
   自動検出して続きから実行する

### 注意事項

- `--resume` には `--out-dir` の指定が必須
- `--games` は合計の目標対局数を指定する（追加分ではない）
- `--shuffle-seed` は meta から自動復元される。CLI で異なる seed を指定するとエラー
- 学習データ（.psv）、info ログ、eval ファイルなどもすべて追記される
- Ctrl-C を 2 回押すと強制終了する（進行中の対局は破棄される）

## 使用例

### YaneuraOu USI で学習データ生成

```bash
./target/release/gensfen \
  --native=false \
  --engine-path /path/to/YaneuraOu-halfkp_256x2-32-32 \
  --usi-option "EvalDir=/path/to/eval_dir" \
  --usi-option "FV_SCALE=24" \
  --usi-option "PvInterval=0" \
  --startpos-file start_sfens_ply24.txt \
  --games 100000 \
  --depth 9 --nodes 80000 \
  --concurrency 30 --max-moves 320 --hash-mb 128
```

### 再開可能な大規模生成

```bash
# 初回
./target/release/gensfen \
  --eval-file eval/model.bin \
  --startpos-file start_sfens_ply24.txt \
  --games 100000 --nodes 80000 --concurrency 30 \
  --out-dir data/gensfen/train

# 中断後に再開（同じ引数 + --resume）
./target/release/gensfen \
  --eval-file eval/model.bin \
  --startpos-file start_sfens_ply24.txt \
  --games 100000 --nodes 80000 --concurrency 30 \
  --out-dir data/gensfen/train \
  --resume
```

## JSONL 出力形式

各行が独立した JSON オブジェクト。`type` フィールドで種別を判別:

- `"meta"`: セッション設定（1行目に1回のみ）
- `"result"`: 対局結果（`outcome`: `"black_win"` / `"white_win"` / `"draw"`）
