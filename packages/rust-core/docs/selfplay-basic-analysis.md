# Selfplay 基本エンジン vs 本エンジン — 自己対局 & 分析ガイド

`selfplay_basic` を使って「本エンジン（Black） vs ShogiHome 風簡易エンジン（White）」の自己対局を行い、  
そのログを `selfplay_blunder_report` / `selfplay_eval_targets` で解析するワークフローをまとめます。

## 1. 自己対局ログの取得

- カレントディレクトリ: `packages/rust-core`
- 代表的なコマンド（秒読み 5 秒 / 8 スレッド / 最大 180 手）:

```bash
cargo run --release -p tools --bin selfplay_basic -- \
  --games 1 \
  --max-moves 180 \
  --think-ms 5000 \
  --threads 8 \
  --basic-depth 2
```

- Black: 本エンジン（engine-usi 相当のコア）
- White: ShogihomeBasicStyle（`--basic-style`）で指定した簡易エンジン（既定 `static-rook`）
- 出力:
  - `runs/selfplay-basic/<timestamp>-selfplay_...jsonl` — 1 手ごとのログ（main_eval/basic_eval/result 等）
  - 同名 `.info.jsonl` — `engine-usi` の `info` 行を JSON 化した診断ログ
  - 同名 `*.kif` / `*_gNN.kif` — 1 対局ごとの KIF（ゲーム数が複数なら `_g01`/`_g02`... と分割）

### 1.1 早期終了オプション（評価値ドロップ検出）

終局まで指し切る前に「本エンジン側の評価値が大きく落ちた対局だけを途中で止めて分析に回したい」場合は、  
次の 2 つのオプションを併用します。

```bash
cargo run --release -p tools --bin selfplay_basic -- \
  --games 1 \
  --max-moves 512 \
  --think-ms 5000 \
  --threads 8 \
  --basic-depth 2 \
  --early-stop-delta-cp 400 \
  --early-stop-follow-plies 4
```

- `--early-stop-delta-cp <N>`  
  - 先手（本エンジン）の連続する 2 回の手番間で、`score_cp` の差分が **`-N`cp 以下** になったときに早期終了モードを起動します。  
    - 例: `--early-stop-delta-cp 400` のとき、前回先手手番から -400cp 以上悪化した手を検出。
  - 判定は `main_eval.score_cp` を使います（mate 評価のみの場合は対象外）。

- `--early-stop-follow-plies <M>`（既定 `0`）  
  - ドロップ検出後、さらに **M 手（plies）だけ局面を進めてから**その対局を終了します。  
  - `0` の場合は、ドロップ検出後の手をログに記録した時点で即座に対局を打ち切り、結果は `draw` としてマークされます。

この機能により、

- `--max-moves` は十分大きく設定しておきつつ、
- 本エンジンの評価が大きく崩れた対局だけを短く切り上げて、
- その局面とログを `selfplay_blunder_report` / `selfplay_eval_targets` で重点的に調べる

といった運用が可能になります。

## 2. ブランダー抽出 + ターゲット生成（selfplay_blunder_report）

自己対局ログ（JSONL + info）から「評価が大きく落ちた手」と、その数手前の局面をターゲットとして抽出します。

### 2.1 基本コマンド

```bash
cargo run -p tools --bin selfplay_blunder_report -- \
  runs/selfplay-basic/<log>.jsonl \
  --threshold 400 \
  --back-min 0 \
  --back-max 3
```

- `threshold`: 直前手との差分 `delta_cp` がこの値以下（負方向）の手をブランダー候補とみなす（例: -400cp 以下）。
- `back-min` / `back-max`: スパイク手から何手遡るかの範囲。  
  例: `back-min=0, back-max=3` なら「スパイク発生局面 + 1〜3手前」がすべてターゲット候補になる。

### 2.2 出力内容

出力先: `runs/analysis/<log>-blunders/`

- `blunders.json`  
  - 各候補ごとに以下のような情報を記録:
    - `log_path` / `info_log_path`
    - `game_id`, `ply`, `move_usi`, `side_to_move`
    - `sfen_before`（スパイク直前局面）
    - `eval_before_cp` / `eval_after_cp` / `delta_cp`
    - `eval_before` / `eval_after`（score/depth/nodes/pv など）
    - `back_plies`: 何手前の局面をターゲットにしたか
    - `info_lines`: 該当手番の `info` 行（最大 `--max-info-lines` 件）
- `targets.json`  
  - `run_eval_targets.py` 互換の形式で、再解析用の `pre_position` を列挙:
    - `tag`: `<logstem>-g<game>-ply<ply>-back<back_plies>` など
    - `pre_position`: `position sfen ...` または `position startpos moves ...`
    - `origin_log`, `origin_game`, `origin_ply`, `origin_delta_cp`, `back_plies`
- `summary.txt`  
  - 入力ログ / 閾値 / ブランダー件数 / ターゲット件数などの簡易サマリ。

## 3. ターゲット再解析（selfplay_eval_targets）

`targets.json` に含まれる各局面を `engine-usi` に投げ直し、複数の探索プロファイルで評価します。

### 3.1 基本コマンド

```bash
cargo run -p tools --bin selfplay_eval_targets -- \
  runs/analysis/<log>-blunders/targets.json \
  --threads 8 \
  --byoyomi 2000
```

- デフォルトプロファイル（コード内定義）:
  - `base`     — 既定の探索プロファイル
  - `rootfull` — RootBeamForceFullCount を増やした設定
  - `gates`    — RootSeeGate や QuietSeeGuard を解除した設定 など
- オプション:
  - `--threads`: USI Threads
  - `--byoyomi`: `go byoyomi` のミリ秒
  - `--min-think`: `SearchParams.MinThinkMs`
  - `--warmup-ms`: `Warmup.Ms`
  - `--engine-path`: `engine-usi` バイナリの明示指定（必要な場合）

### 3.2 出力内容

`runs/analysis/<log>-blunders/` に以下のファイルを生成します:

- `summary.json`  
  - 各 `tag` × profile ごとに:
    - `eval_cp`（最終深さでの cp 評価）
    - `depth`
    - `bestmove`
    - `origin_log` / `origin_ply` / `back_plies`
    - `log_path`（下記ログファイルへのパス）
- `*_gNN__<profile>.log`  
  - USI ログ生データ（`info depth ... score cp ...` / `bestmove ...` など）

## 4. 推奨ループ（改善サイクル）

1. `selfplay_basic` で 1 局〜数局の自己対局を回す。
2. `selfplay_blunder_report` でブランダー候補とターミナル付近の局面（back_plies を含む）を抽出。
3. `selfplay_eval_targets` で Multi Profile 再解析し、「どの profile / depth で何が起きているか」を把握。
4. 原因仮説に基づき `engine-core` / `engine-usi` の探索/評価/Finalize を修正。
5. 同じ条件で 1〜3 を再度実行し、スパイクが減ったか・右肩上がりが改善したかを確認。

Selfplay ログの解析に関しては、可能な限りこの Rust ツールチェーン（`selfplay_basic` / `selfplay_blunder_report` / `selfplay_eval_targets`）を正規ルートとして使用し、  
Python の `scripts/analysis/*.py` は外部 USI ログの後処理に限って利用することを推奨します。
