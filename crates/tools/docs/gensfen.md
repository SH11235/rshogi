# gensfen — NNUE 学習用棋譜・教師局面 (PSV/pack) 生成ツール

USIエンジン同士の対局を回しながら、PackedSfenValue 形式の教師局面を生成する。
棋力評価（Elo 比較・SPRT 等）には `tournament` バイナリを使うこと。

## ビルド

```bash
cargo build -p tools --bin gensfen --release
```

リリースビルドのバイナリは `target/release/gensfen` に生成される。

## クイックスタート

```bash
# 任意のUSIエンジンで10局・1秒秒読み
./target/release/gensfen \
  --engine-path /path/to/your/usi-engine \
  --games 10 --byoyomi 1000

# 学習データを生成しながら100局
./target/release/gensfen \
  --engine-path /path/to/your/usi-engine \
  --games 100 --byoyomi 1000 --concurrency 4
```

`--engine-path` を省略した場合は rshogi のエンジンバイナリ（`target/release/rshogi-usi`）が自動検出される。

## 出力ファイル

`--out-dir` を指定しない場合、タイムスタンプ付きディレクトリが自動生成される:

```
runs/gensfen/20260317-120000/
  gensfen.jsonl          # メタ情報 + 対局結果ログ（JSONL形式）
  gensfen.summary.jsonl  # 対局セッション全体のサマリ
  gensfen.psv            # 学習データ（PackedSfenValue, 40バイト/局面）
  gensfen.kif            # KIF形式の棋譜（複数局は gensfen_g01.kif ...）
  gensfen.info.jsonl     # info ログ（--log-info 指定時のみ）
  gensfen.eval.txt       # 評価値推移（--emit-eval-file 指定時のみ）
  gensfen.metrics.jsonl  # 対局メトリクス（--emit-metrics 指定時のみ）
```

`--out-dir path/to/dir` を指定した場合は、そのディレクトリ内に上記ファイルが生成される。

`--for-train` 指定時は `.psv` と `.jsonl`（簡素化版）のみ出力される。詳細は[学習局面生成](#学習局面生成)を参照。

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
| `--out-dir DIR` | 自動生成 | 出力ディレクトリ |
| `--log-info` | false | エンジンのinfo出力をログに記録 |
| `--flush-each-move` | false | 毎手フラッシュ（安全だが低速） |
| `--emit-eval-file` | false | 評価値推移ファイルを出力 |
| `--emit-metrics` | false | 対局メトリクスJSONLを出力 |
| `--no-kif` | false | KIF棋譜ファイルの出力を無効化 |

### 学習データ

| オプション | デフォルト | 説明 |
|-----------|-----------|------|
| `--for-train` | false | 学習局面生成に特化したモード（後述） |
| `--output-training-data PATH` | `<out-dir>/gensfen.psv` | 学習データ出力先（PackedSfenValue形式） |
| `--no-training-data` | false | 学習データ出力を無効化 |
| `--skip-initial-ply N` | 0 | 序盤N手をスキップ |
| `--skip-in-check BOOL` | false | 王手局面をスキップ |

### 中断・再開

| オプション | 説明 |
|-----------|------|
| `--resume` | 前回中断したセッションを再開する |

### パス権（特殊ルール）

| オプション | 説明 |
|-----------|------|
| `--pass-rights-black N` | 先手のパス権数 |
| `--pass-rights-white N` | 後手のパス権数 |

パス権有効時は学習データ出力が自動的に無効化される（PackedSfen形式がパス権非対応のため）。

## 学習局面生成

NNUE等の学習に使う教師データ（PackedSfenValue形式）を大量生成するためのモード。

### `--for-train` フラグ

`--for-train` を指定すると、以下の動作が変更される:

**`--for-train` が行うこと:**

| 項目 | 通常モード | `--for-train` |
|------|-----------|---------------|
| バックエンド | USI外部プロセス | **NativeBackend**（rshogi-core直接呼び出し） |
| USI 単一エンジン | 無効 | **有効**（先後同一エンジン時、1プロセスで兼用） |
| 学習データ (.psv) | 出力 | 出力 |
| JSONL (対局ログ) | 全手順を記録 | result行のみ（resume用の最小限） |
| KIF (棋譜) | 出力 | **出力しない** |
| summary (サマリ) | 出力 | **出力しない** |
| ハッシュ重複検出 | 無効 | **有効**（64Mエントリ） |
| 開始局面消費方式 | 順番巡回 | **シャッフル+pop**（重複なし） |
| MultiPVランダム | 無効 | 無効（--random-multi-pv N で有効化） |
| 時間管理パラメータ | エンジンデフォルト | depth/nodes 指定時は **自動で 0** |

大量対局（数万〜数十万局）では KIF だけで数GBに達するため、`--for-train` の使用を推奨する。

#### NativeBackend

`--for-train` 時は USI プロトコル経由ではなく、rshogi-core の探索エンジンを直接呼び出す（単一プロセス・マルチスレッド）。

**メリット:**
- **メモリ削減**: 60プロセス×評価関数(144MB) → 評価関数1コピーで済む
- **速度向上**: USI パイプ通信 + テキスト解析のオーバーヘッドがない
- **TT制御**: 置換表のクリア/保持をオプションで切替可能

`--eval-file` で NNUE 評価関数ファイルの指定が必須。

### 重複回避オプション

tanuki- の棋譜生成手法を参考にした重複局面回避機能。`--for-train` 時は適切なデフォルトが自動適用される。個別に指定して上書きすることも可能。

| オプション | for-train default | 通常 default | 説明 |
|---|---|---|---|
| `--native[=BOOL]` | true | false | NativeBackend を使用（`--eval-file` 必須） |
| `--eval-file PATH` | (必須) | — | NNUE評価関数ファイル（NativeBackend用） |
| `--keep-tt[=BOOL]` | false | false | TT を対局間で保持（tanuki-は毎回クリア。実験用） |
| `--dedup-hash-size N` | 67108864 | 0 (無効) | ハッシュ重複検出テーブルサイズ（エントリ数） |
| `--startpos-no-repeat[=BOOL]` | true | false | 開始局面を重複なしで消費（シャッフル+pop） |
| `--shuffle-seed N` | 自動生成 | — | 開始局面シャッフルの乱数シード（resume時はmetaから復元） |
| `--random-multi-pv N` | 0 (無効) | 0 (無効) | MultiPVランダム選択の候補数 |
| `--random-multi-pv-diff N` | 32000 | 32000 | MultiPV評価値差閾値（cp） |
| `--random-move-count N` | 0 | 0 | ランダムムーブ回数（0で無効） |
| `--random-move-min-ply N` | 1 | 1 | ランダムムーブ開始手数 |
| `--random-move-max-ply N` | 24 | 24 | ランダムムーブ終了手数 |

#### ハッシュ重複検出（`--dedup-hash-size`）

局面の Zobrist ハッシュをテーブルに記録し、既出局面を検出する。重複検出時は:
1. それまでに蓄積した学習エントリを全クリア
2. 重複局面自体は記録しない
3. 対局は続行（以降のユニーク局面は通常通り記録）

全ワーカーで1つのテーブルを共有する（tanuki- と同じ構成）。`AtomicU64` でロックフリーアクセス。
64Mエントリ × 8バイト = 512MB。

#### 開始局面シャッフル消費（`--startpos-no-repeat`）

開始局面プールをシャッフルし、順番に1つずつ消費する。同じ開始局面が2回使われない。プール枯渇時は再シャッフルして2周目に入る。

シャッフルの乱数シードは meta 行に `shuffle_seed` として保存される。resume 時は同じ seed で順列を再構築し、完了済み対局数分だけ進めることで正確な位置を復元する。`--shuffle-seed` で seed を明示指定することも可能（再現性が必要な場合）。

#### MultiPVランダム選択（`--random-multi-pv`）

探索時に N 候補を評価し、PV1 のスコアとの差が `--random-multi-pv-diff` 以内の候補からランダムに選択してプレイする。学習データには PV1 のスコアと手を記録する（局面の真の評価値）。多様な局面を自然に生成できる。

**推奨ユースケース**: 対局数が開始局面数を大幅に上回る場合（例: 50万局 vs 3万局面プール）。開始局面の no-repeat だけでは 2 周目以降に同一対局が再現されるため、MultiPV ランダムまたはランダムムーブとの併用を推奨する。

#### ランダムムーブ（`--random-move-count`）

序盤の `--random-move-min-ply` 〜 `--random-move-max-ply` の範囲から N 手をランダムに選び、その手数では合法手からランダムに1手選択する（エンジン探索をスキップ）。ランダムムーブ前の蓄積エントリは全クリアされる（tanuki- 方式）。

#### dedup rate 警告（`--dedup-warn-interval`, `--dedup-warn-rate`）

`--dedup-warn-interval N`（デフォルト: 1000）ゲームごとに直近区間の dedup rate をチェックし、`--dedup-warn-rate`（デフォルト: 0.1 = 10%）を超えると stderr に警告を出力する。長時間実行中に MultiPV の不足をリアルタイムで検知できる。interval はワーカー数で自動分割されるため（`interval / concurrency`、最小 1）、concurrency を上げても総ゲーム数ベースでの検知タイミングは概ね一定。警告はワーカーごとに独立して出力されるため、同一タイミングで最大 concurrency 行の警告が表示されることがある。

#### MultiPV 値の選定ガイド（実験結果）

以下は NativeBackend、nodes=5000〜10000 での実測値。

**10 局面での周回テスト（局面/game）:**

| MultiPV | 5周 | 10周 |
|---|---|---|
| 0（無効） | 33.8 | — |
| 2 | 78.7 | — |
| 4 | 85.3 | 83.9（微減） |
| 8 | 102.3 | 111.9（維持） |

MultiPV=4 は周回数が増えると効率が低下し始めるが、8 は安定。

**1000 局面での周回テスト（MultiPV=8）:**

| 周回 | games | PSV局面数 | 局面/game | 効率 |
|---|---|---|---|---|
| 5周 | 5,000 | 540,750 | 108.2 | ≈100% |
| 10周 | 10,000 | 1,085,122 | 108.5 | ≈100% |

開始局面数が十分（1000+）であれば、MultiPV=8 で 10 周しても効率はほぼ落ちない。

**推奨:**

| games / startpos 比率 | MultiPV | 備考 |
|---|---|---|
| ≤ 1倍 | 0（不要） | 全ゲームが異なる開始局面 |
| 2-5倍 | 4 | 軽微な周回 |
| 5倍以上 | 8 | 長期周回でも安定 |
| 10倍以上 | 8 + ランダムムーブ | さらなる多様性が必要な場合 |

### 学習データのオプション詳細

#### `--skip-initial-ply N`（デフォルト: 0）

序盤の手番1〜N手目の局面を学習データから除外する。開始局面ファイルで途中局面を使う場合（例: 32手目以降）はデフォルトの0で問題ない。平手初期局面から対局する場合は、定跡部分を除外するために適宜指定する。

#### `--skip-in-check BOOL`（デフォルト: false）

`true` を指定すると、王手がかかっている局面を学習データから除外する。デフォルトでは全局面を記録する。

#### `--output-training-data PATH`

学習データの出力先パスを指定する。省略時は出力ディレクトリ内の `gensfen.psv` に出力される。

#### `--no-training-data`

学習データの出力を完全に無効化する。対局結果のみが必要な場合に使用する。

### 学習データ生成の例

```bash
# NativeBackend で 10万局、30並列（推奨）
# --for-train が重複回避オプションを自動適用する
./target/release/gensfen \
  --for-train \
  --eval-file eval/halfkp_256x2-32-32_crelu/suisho5.bin \
  --startpos-file start_sfens_ply24.txt \
  --games 100000 \
  --nodes 80000 \
  --depth 9 \
  --concurrency 30 \
  --max-moves 320 \
  --hash-mb 128
```

重複回避オプションを個別に調整する場合:

```bash
# ランダムムーブも追加、dedup テーブルサイズを変更
./target/release/gensfen \
  --for-train \
  --eval-file eval/model.bin \
  --startpos-file start_sfens_ply24.txt \
  --games 100000 \
  --nodes 80000 \
  --concurrency 30 \
  --random-move-count 3 \
  --random-move-max-ply 16 \
  --dedup-hash-size 134217728
```

YaneuraOu USI で学習データ生成する場合:

```bash
./target/release/gensfen \
  --for-train \
  --engine-path /path/to/YaneuraOu-halfkp_256x2-32-32 \
  --usi-option "EvalDir=/path/to/eval_dir" \
  --usi-option "FV_SCALE=24" \
  --usi-option "PvInterval=0" \
  --startpos-file start_sfens_ply24.txt \
  --games 100000 \
  --depth 9 \
  --nodes 80000 \
  --concurrency 30 \
  --max-moves 320 \
  --hash-mb 128
```

`--for-train` + `--engine-path` 指定で USI 単一エンジンモードが自動適用される（NativeBackend ではなく USI を使用）。`--depth`/`--nodes` 指定時は `NetworkDelay`, `NetworkDelay2`, `MinimumThinkingTime` が自動で 0 に設定される（USI エンジンの時間管理パラメータが nodes モードに干渉するのを防止）。

中断・再開する場合は `--out-dir` を指定して同一ディレクトリを使う:

```bash
# 初回実行
./target/release/gensfen \
  --for-train \
  --eval-file eval/model.bin \
  --startpos-file start_sfens_ply24.txt \
  --games 100000 --nodes 80000 --concurrency 30 \
  --out-dir data/gensfen/train

# 中断後に再開（同じ引数 + --resume）
./target/release/gensfen \
  --for-train \
  --eval-file eval/model.bin \
  --startpos-file start_sfens_ply24.txt \
  --games 100000 --nodes 80000 --concurrency 30 \
  --out-dir data/gensfen/train \
  --resume
```

### 学習データの形式

PackedSfenValue 形式（40バイト/局面）で、Nodchip learner互換。

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
3. `--resume` 付きで同じコマンドを再実行すると、出力JSONLから完了済み対局数を自動検出して続きから実行する

### 注意事項

- `--resume` には `--out-dir` の指定が必須（自動生成パスでは前回のディレクトリを特定できないため）
- `--games` は合計の目標対局数を指定する（追加分ではない）
- `--shuffle-seed` は meta から自動復元される。CLI で異なる seed を指定するとエラーになる
- 学習データ（.psv）、info ログ、eval ファイルなどもすべて追記される
- Ctrl-C を2回押すと強制終了する（進行中の対局は破棄される）

## 使用例

### 基本的な自己対局

```bash
./target/release/gensfen \
  --engine-path /path/to/usi-engine \
  --games 10 --max-moves 300 --byoyomi 1000
```

### 異なるエンジン同士の対局

```bash
./target/release/gensfen \
  --games 100 --byoyomi 5000 \
  --engine-path-black ./engine_a \
  --engine-path-white ./engine_b \
  --threads-black 4 --threads-white 4
```

### 特定局面からの対局

```bash
./target/release/gensfen \
  --engine-path /path/to/usi-engine \
  --games 1000 --byoyomi 1000 \
  --startpos-file positions.txt --random-startpos
```

## JSONL 出力形式

各行が独立したJSONオブジェクト。`type` フィールドで種別を判別:

- `"meta"`: セッション設定（1行目に1回のみ）
- `"move"`: 各手の詳細（`--for-train` 時は出力されない）
- `"result"`: 対局結果（`outcome`: `"black_win"` / `"white_win"` / `"draw"`）
