---
description: 指定エンジン間の総当たり自己対局を実行し、結果を集計する
user-invocable: true
allowed-tools:
  - Bash
  - Read
  - Grep
  - Glob
  - TaskCreate
  - TaskUpdate
  - TaskList
  - TaskOutput
  - AskUserQuestion
---

# 自己対局評価スキル

以下の指示に従い、指定されたエンジン間の総当たり自己対局を実行し、結果を集計する。

## 入力パラメータ

ユーザーから以下の情報を `$ARGUMENTS` として受け取る。
情報が不足している場合は質問して補完すること。

### 必須情報
- **対象エンジン一覧**: 各エンジンの commit ハッシュ（短縮可）、バイナリパス、説明
- **確認ポイント**: 特に注目する比較（例: "E vs D: TT 16bit の棋力効果"）

### デフォルト値（指定がなければ以下を使用）
- 秒読み: 2000ms
- スレッド: 1
- ハッシュ: 256MB
- 各方向の対局数: 100（双方向で200局/カード）
- NNUE: `EvalFile=eval/halfkp_256x2-32-32_crelu/suisho5.bin`

## 実行手順

### 1. バイナリ存在確認

各エンジンのバイナリパスが存在するか確認する。
存在しないバイナリがあればユーザーに報告し、ビルド方法を提案する。

### 2. 出力ディレクトリの作成

実験ごとに個別のディレクトリを作成し、ログファイルの混入を防ぐ。

```
mkdir -p runs/selfplay/{YYYYMMDD}-{HHMMSS}-{PURPOSE}
```

- `{PURPOSE}` はユーザーの実験目的を短く要約したもの（例: `tt-16bit`, `lmr-tuning`）
- このディレクトリパスを以降の `--out-dir` オプションで使用する

### 3. tournament バイナリで総当たり自己対局を実行

`tournament` バイナリ1コマンドで、全ペアの総当たり対局を並列実行する。
`--engine` を複数指定すると自動で C(N,2) ペアの対局を生成する。

```
cargo run -p tools --release --bin tournament -- \
  --engine {ENGINE_A} --engine {ENGINE_B} [--engine {ENGINE_C} ...] \
  --games {GAMES} --byoyomi {BYOYOMI} --hash-mb {HASH} --threads {THREADS} \
  --concurrency {CONCURRENCY} \
  --usi-option {NNUE} \
  --out-dir runs/selfplay/{DIR}
```

- `--concurrency`: 並列対局数（デフォルト1）。CPUコア数に応じて調整。
- `--report-interval`: N局ごとに進捗を表示（デフォルト10）。
- `--engine-usi-option "INDEX:Name=Value"`: エンジン個別の USI オプション（0始まりインデックス）。
  指定したエンジンは共通 `--usi-option` が**完全に置換**される（マージではない）。
- 出力は `{out-dir}/{label_i}-vs-{label_j}.jsonl` に自動生成される。

**注意:** `run_in_background: true` で起動し、`TaskOutput` で完了を監視すること。

#### 外部エンジンとの対局例

rshogi と YaneuraOu のように異なるエンジンを対局させる場合、
エンジンごとに必要な USI オプションが異なるため `--engine-usi-option` を使う:

```
cargo run -p tools --release --bin tournament -- \
  --engine target/rshogi-usi-{HASH} \
  --engine /path/to/YaneuraOu-binary \
  --engine-usi-option "0:EvalFile=eval/halfkp_256x2-32-32_crelu/suisho5.bin" \
  --engine-usi-option "1:EvalDir=/path/to/eval" \
  --engine-usi-option "1:BookFile=no_book" \
  --games 100 --byoyomi 3000 --concurrency 5 \
  --out-dir runs/selfplay/{DIR}
```

### 4. 完了待ち・結果集計

Background task の完了を `TaskOutput` で検知する。
完了後、ディレクトリ内のファイルを指定して集計する:

```
cargo run -p tools --release --bin analyze_selfplay -- runs/selfplay/{DIR}/*.jsonl
```

以下の内容をマークダウンファイル（`docs/performance/` 配下）に出力する:

1. **対局条件**: 秒読み・スレッド・ハッシュ・対局数・NNUE
2. **総合結果表**: 各カードの勝敗・勝率・Elo差
3. **確認ポイントの評価**: ユーザーが指定した比較ポイントについての分析
4. **総括**: 全体的な傾向と推奨事項

## 入力例

```
/selfplay エンジン:
- A: 3526b075 target/rshogi-usi-3526b075 ベースライン
- B: 232d847d target/rshogi-usi-232d847d move ordering完了
- D: 4778e1c6 target/rshogi-usi-4778e1c6 LMR修正（TT変更前）
- E: 5806777e target/rshogi-usi-5806777e TT 16bit（最新）

確認ポイント:
1. E vs D: TT 16bit の棋力効果
2. E vs A: 全修正+TT の総合効果
3. B vs D: Step14+LMR が move ordering 完了時点より良いか悪いか
```
