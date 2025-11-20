# Debug Position MultiPV Tool

## 概要

`debug_position_multipv` は、`engine-usi` を USI プロトコル経由で呼び出し、特定局面の **MultiPV 情報を時間・プロファイル別に比較するための解析ツール** です。

- 同じ SFEN に対して `--time-ms` を変えつつ、評価値・深さ・ノード数の推移を比較したいとき
- `selfplay_basic` / `selfplay_eval_targets` で気になった局面を、`profile=base/short/gates` などのプリセット付きで再現したいとき
- USI 生ログ（`info` 行）を丸ごと保存して、後で `grep` や別ツールで解析したいとき

に使用します。

## 使い方

### 基本的な使用方法（単発）

```bash
cargo run --release -p tools --bin debug_position_multipv -- \
  --sfen "SFEN文字列" \
  --time-ms 1000 \
  --multipv 4 \
  --engine-path target/release/engine-usi \
  --threads 8 \
  --engine-type enhanced \
  --profile short
```

### 同一局面を複数時間で比較する（並列実行）

```bash
# game_id=2, ply=27 の局面を 1s/2s/5s/20s × profile=short で一括解析
cargo run --release -p tools --bin debug_position_multipv -- \
  --sfen "l4k1nl/1r2g1gb1/psn2pspp/3Bp1p2/1p1N5/6PPP/PP1PPP3/R8/L1SGKGSNL b 2Pp 14" \
  --time-ms 1000 --time-ms 2000 --time-ms 5000 --time-ms 20000 \
  --multipv 4 \
  --engine-path target/release/engine-usi \
  --threads 8 \
  --hash-mb 256 \
  --engine-type enhanced \
  --profile short \
  --tag g2-ply27 \
  --out-json runs/multipv/g2-ply27/batch.json \
  --raw-log runs/multipv/g2-ply27/g2-ply27.log
```

- `--time-ms` を複数指定すると、CPU コア数と `--threads` から自動計算した並列度の範囲で、複数の `engine-usi` プロセスを並列に起動します。
- `--raw-log` が指定されている場合、各 `time-ms` ごとに `*.t1000ms` のようなサフィックス付きファイルに USI 生ログを保存します。
- `--out-json` が指定されている場合:
  - 単発時は 1 オブジェクト
  - 複数 `time-ms` 指定時は `[{...}, {...}, ...]` の配列を出力します。

### オプション一覧

- `--sfen <SFEN>`  
  - 必須。解析する局面を指定します。
  - 形式:
    - 「先頭に `sfen` なし」の SFEN: `lnsgkgsnl/... b - 1`
    - もしくは完全な `position` コマンド: `position sfen ... moves ...`
- `--time-ms <TIME_MS>`  
  - 必須（複数指定可）。1 回の探索時間（ミリ秒）。例: `1000`, `2000`, `5000`。
  - 複数指定した場合はバッチモードになり、自動的に並列実行されます。
- `--multipv <u8>`  
  - 必須。取得する PV 本数。例: `2`〜`4`。
- `--engine-path <path>`  
  - `engine-usi` バイナリのパス。デフォルト: `target/release/engine-usi`。
- `--threads <u32>`  
  - エンジンの `Threads` setoption。デフォルト: `1`。
  - 並列実行時は `available_parallelism / threads` を元に、同時起動するエンジン数が決まります。
- `--hash-mb <u32>`  
  - `USI_Hash` setoption の値（MB）。デフォルト: `256`。
- `--engine-type <enhanced|enhanced_nnue|nnue|material>`  
  - `engine-usi` 側のエンジンタイプ。内部的には `setoption name UsiEngineType value ...` を送ります。
- `--profile <base|short|rootfull|gates|custom>`  
  - `selfplay_eval_targets` と同じプリセットを適用します。
  - `base`:
    - `SearchParams.RootBeamForceFullCount = 0`
  - `short`:
    - `SearchParams.RootBeamForceFullCount = 0`
    - `RootSeeGate = true`
    - `RootSeeGate.XSEE = 150`
    - 環境変数: `SHOGI_QUIET_SEE_GUARD=1`, `SHOGI_CAPTURE_FUT_SCALE=120`
  - `rootfull`:
    - `SearchParams.RootBeamForceFullCount = 4`
  - `gates`:
    - `SearchParams.RootBeamForceFullCount = 0`
    - `RootSeeGate.XSEE = 0`
    - 環境変数: `SHOGI_QUIET_SEE_GUARD=0`
  - `custom` または未知の文字列:
    - プリセットからの `setoption` は送らず、呼び出し側が `engine-usi` 側で自由に調整する前提です。
- `--out-json <path>`  
  - 結果を JSON としてファイル出力します。
  - 単発時: `AnalysisOutput` 1件、複数 `time-ms` 時: `Vec<AnalysisOutput>` の配列。
- `--tag <string>`  
  - 分析タグ。出力 JSON / 標準出力両方に含まれます。
  - バッチモード時は `--tag g2-ply27` のように指定すると、自動的に `g2-ply27-t1000ms` のような形で time-ms ごとにサフィックスが付きます。
- `--raw-log <path>`  
  - USI 生ログの出力先プレフィックス。
  - 実際には `path.t1000ms`, `path.t2000ms` のように time-ms ごとにファイルを分けて保存します。

## 出力フォーマット

### 標準出力（人間向け）

単発時の例:

```text
SFEN: l4k1nl/1r2g1gb1/psn2pspp/3Bp1p2/1p1N5/6PPP/PP1PPP3/R8/L1SGKGSNL b 2Pp 14
Engine: enhanced
Time: 1000 ms (actual 0.95s)
Depth: 6
Nodes: 33000
Tag: g2-ply27-t1s

=== MultiPV ===
#1: score=+1803 depth=5 pv=6d7c 6d7c 8b6b
#2: score=+1700 depth=5 pv=6e7c+
```

複数 `time-ms` を指定した場合は、このブロックが time-ms ごとに連続して表示されます。

### JSON 出力

`--out-json` を指定すると、構造化データを JSON で取得できます。

- 単発 (`--time-ms` 1個) の場合:

```json
{
  "sfen": "l4k1nl/1r2g1gb1/...",
  "engine_type": "enhanced",
  "time_ms": 1000,
  "actual_ms": 947,
  "depth": 6,
  "nodes": 33493,
  "threads": 8,
  "hash_mb": 256,
  "profile": "short",
  "tag": "g2-ply27-t1s",
  "multipv": [
    {
      "rank": 1,
      "score": { "Cp": 1803 },
      "depth": 5,
      "seldepth": 10,
      "nodes": 14916,
      "pv": ["6d7c", "6d7c", "8b6b"]
    },
    {
      "rank": 2,
      "score": { "Cp": 1700 },
      "depth": 5,
      "seldepth": 9,
      "nodes": 12000,
      "pv": ["6e7c+"]
    }
  ]
}
```

- 複数 `time-ms` の場合は、これが配列になります:

```json
[
  { "...": "time_ms=1000 の結果" },
  { "...": "time_ms=2000 の結果" },
  { "...": "time_ms=5000 の結果" }
]
```

`score` は enum 形式（`{ "Cp": 1803 }` / `{ "Mate": 3 }`）ですが、後段ツール側で `cp` / `mate` に展開して扱う想定です。

## Selfplay ログからの再現ワークフロー例

`selfplay_basic` のログから「game_id=2, ply=27」のような局面を拾い、このツールで MultiPV 解析する一連の流れの例です。

### 1. 対象局面の特定

例として、ログファイル:

- `runs/selfplay-basic/20251119-011552-selfplay_enhanced_8t_static-rook_d2_1000ms.info.jsonl`
- `runs/selfplay-basic/20251119-011552-selfplay_enhanced_8t_static-rook_d2_1000ms.jsonl`

に対して、`game_id=2, ply=27` の情報を探します。

```bash
cd packages/rust-core

# 探索ログ（info.jsonl）側で、該当 ply の探索履歴を確認
rg '"game_id":2,"ply":27' \
  runs/selfplay-basic/20251119-011552-selfplay_enhanced_8t_static-rook_d2_1000ms.info.jsonl

# サマリ（jsonl）側で、手番直前の SFEN を取得
rg '"game_id":2,"ply":27' \
  runs/selfplay-basic/20251119-011552-selfplay_enhanced_8t_static-rook_d2_1000ms.jsonl
```

後者の `jsonl` には、例えば次のような行が含まれます:

```json
{"game_id":2,"ply":27,"side_to_move":"b","sfen_before":"l4k1nl/1r2g1gb1/psn2pspp/3Bp1p2/1p1N5/6PPP/PP1PPP3/R8/L1SGKGSNL b 2Pp 14", ...}
```

ここで `sfen_before` フィールドの値:

```text
l4k1nl/1r2g1gb1/psn2pspp/3Bp1p2/1p1N5/6PPP/PP1PPP3/R8/L1SGKGSNL b 2Pp 14
```

が、`debug_position_multipv` に渡す SFEN になります。

### 2. debug_position_multipv で再現 & MultiPV 解析

上で取得した SFEN をそのまま `--sfen` に渡し、`--time-ms` を変えながら解析します。

```bash
cargo build --release -p engine-usi -p tools

mkdir -p runs/multipv/g2-ply27

cargo run --release -p tools --bin debug_position_multipv -- \
  --sfen "l4k1nl/1r2g1gb1/psn2pspp/3Bp1p2/1p1N5/6PPP/PP1PPP3/R8/L1SGKGSNL b 2Pp 14" \
  --time-ms 1000 --time-ms 2000 --time-ms 5000 --time-ms 20000 \
  --multipv 4 \
  --engine-path target/release/engine-usi \
  --threads 8 \
  --hash-mb 256 \
  --engine-type enhanced \
  --profile short \
  --tag g2-ply27 \
  --out-json runs/multipv/g2-ply27/batch.json \
  --raw-log runs/multipv/g2-ply27/g2-ply27.log
```

- `batch.json` には `time_ms=1000/2000/5000/20000` それぞれの `AnalysisOutput` が配列としてまとまって出力されます。
- `g2-ply27.log.t1000ms` などには USI 生ログがそのまま保存されるため、
  - `info depth` / `info string` を後から `rg` で抽出して詳細な挙動を追う
  - 別のログ解析ツールに渡して、depth/seldepth/score の時系列を可視化する
 などの post-mortem 分析に利用できます。

## Claude Code への注意事項

このツールは以下の場面で優先的に使用してください:

1. **MultiPV ベースの手順比較をしたいとき**
   - 特定局面に対して、上位 N 手のスコア・深さ・ノード数を比較したいケース。
   - selfplay の blunder 局面を抽出して、候補手の入れ替わりを調べるとき。

2. **時間依存性（1s/2s/5s/…）の評価を取りたいとき**
   - `--time-ms` を変えながら、どの時点で候補手が安定するかを見たいとき。
   - 並列実行を使って、複数の時間設定を一気に回したいとき。

3. **selfplay_eval_targets と同じプロファイルで再現したいとき**
   - `--profile base|short|rootfull|gates` を指定することで、`selfplay_eval_targets` と同一プリセットを適用した状態で再現できます。

4. **USI 生ログを残して後で解析したいとき**
   - `--raw-log` を指定すると `info` / `info string` / `bestmove` をそのままログに残せるため、別ツールでの post-mortem 解析に便利です。

一方で、以下の用途では従来の `debug_position` を使った方がシンプルです:

- 手生成や Perft の検証（`--moves` や `--perft`）
- `engine-core` 内部の `SearchStats` フィールド（TTヒットなど）を直接見に行きたいとき

## 関連ドキュメント

- [`debug_position` ツール](./debug-position-tool.md) - エンジン内部デバッグ・手生成検証用
- [`selfplay-basic-analysis.md`](../selfplay-basic-analysis.md) - Selfplay ログの分析ワークフロー（本ツールの利用シーンの一つ）
