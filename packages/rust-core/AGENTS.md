# Rust Core Agent Guide

## 0. スコープと言語
- ここは npm/Turbo モノレポ `shogi` の Rust ワークスペース。上位リポジトリと密結合しているため、JavaScript パッケージの状態も常に意識してください。
- モノレポ全体（JS/Turbo/Volta）の詳細な運用ルールはリポジトリルートの `../../AGENTS.md` にまとめてあります。JS/フロント側の作業をする場合は必ずそちらを参照してください。
- Volta により Node 24.0.1 / npm 11.3.0 が固定されています。JS 側の作業前に `npm install` を一度だけ実行して依存を同期します。
- このコードベースでは日本語でやり取りすること。ドキュメントやコメントも可能な限り日本語（英語併記可）でまとめてください。

## 1. Rust Core ワークスペース
### 構成
- `crates/engine-core`: 探索・評価・時間管理・USI 型。Criterion ベンチやユニットテストが併設。
- `crates/engine-usi`: USI CLI バイナリ（薄いエントリポイント）。
- `crates/engine-wasm`: WebAssembly ブリッジ（wasm-bindgen, wasm-pack）。
- `crates/tools`: ベンチ/ユーティリティバイナリ（TT テスト、NNUE 解析など）。
- `crates/webrtc-p2p`: WebRTC P2P 伝送層。
- `docs/`: 設計/NNUE/パフォーマンス/USI ビルド手順。`docs/nnue-*`, `docs/performance/*`, `docs/usi-engine-build.md` を参照。
- `scripts/analysis|bench|gauntlet|nnue|repro|utils`: エンジン比較、NNUE 評価、バグ再現用スクリプト群。
- `runs/`, `data/`, `converted_openings/`, `logs/`: ガントレット、開幕集、学習ログなどの成果物置き場。

### ビルド & テスト
- 全体: `cargo build` / `cargo build --release`。高速化は `RUSTFLAGS="-C target-cpu=native"` 推奨。
- 個別: `cargo build -p engine-core`, `cargo run -p engine-usi -- --help`, `cargo run -p engine-usi --release --features diagnostics` 等。
- テスト: `cargo test`, `cargo test -p engine-core`, 任意モジュールは `cargo test path::to::case -- --nocapture`。
- ベンチ: `cargo bench -p engine-core`（結果は `target/criterion/`）。
- Lint/Format: `cargo fmt --all`, `cargo clippy --workspace -- -D warnings`（CI 準拠）。
- WASM: `wasm-pack build crates/engine-wasm --release --target web|nodejs`。ブラウザテストは `wasm-pack test --chrome --headless`。
- USI ログ解析の定石（seldepth や評価揺れの確認手順）:
  1. `rg(){ command rg "$@" || true; }; export -f rg` で `rg` が空振りでも落ちないようラッパを用意。
  2. `bash scripts/analysis/analyze_usi_logs.sh <log>` を流し、`max_depth`/`max_seldepth`/PV 切替回数/締切ヒットを CSV で取得する。
  3. 詳細に追いたい手前の局面は `bash scripts/analysis/replay_multipv.sh <log> -t 1 -m 1 -b 5000 -o runs/game-postmortem/<tag>` で切り出し、`runs/game-postmortem/<tag>/summary.txt` に各手の `depth/seldepth/score` を記録して前後比較する。
  4. これにより「静かチェック暴走で seldepth が跳ねたか」「評価値が右肩下がりになっていないか」を再現性高く確認できる。秒読み 5 秒なら `-b 5000` を使う。
- panic 方針: `profile.dev` / `profile.release` で `panic = "unwind"`。USI バイナリは unwind 前提なので変更する場合は安全策を明示してください。

### コーディング規約
- rustfmt（`rustfmt.toml`）: 4 スペース、100 列、Edition 2021。
- 命名: モジュール/関数は snake_case、型/トレイトは CamelCase、定数は SCREAMING_SNAKE_CASE。Feature 名は `snake` / `kebab`（例: `tt_metrics`, `ybwc`, `nightly`）。
- ループは `iter()`/`iter_mut()` + `enumerate`/`zip` を優先し、インデックス走査は避ける。
- 不要な公開 API を避け、`pub(crate)`/`pub(super)` でスコープを絞る。

### モジュール構造ポリシー
- ファイル単位モジュール（`src/foo.rs` + `src/foo/bar.rs`）。新規 `mod.rs` は作らない。
- バイナリ（`engine-usi` や `src/bin/*.rs`）は薄く保ち、ロジックはライブラリクレートへ集約。
- `lib.rs` は再エクスポートで API を整形し、必要なら `prelude` を最小限用意。
- `#[cfg(feature = "...")]` で機能ゲートを明示し、`docsrs` ビルドを壊さない。
- マクロは通常モジュール内で定義し、必要時のみ `#[macro_export]`。

### テスト指針
- ユニットテストは各モジュール内の `mod tests` または `src/.../tests/*.rs` に配置。
- Criterion ベンチは `[[bench]]` に登録し、`cargo bench` で回す。
- WASM 専用検証は `crates/engine-wasm` + `wasm-pack test`。
- Focused Test Naming（zstd 系）: `test_zstd_*` プレフィックスを付け、`cargo test --release -p tools --features zstd -- test_zstd` で抽出可能に。zstd 系テストは常に `--features zstd` を付けて実行。

### NNUE Training Ops — Quick SOP（必読）
1. **評価（Gauntlet）原則**: `scripts/nnue/evaluate-nnue.sh` を使用。`--threads 1` 固定、固定オープニング（例: `runs/fixed/20251011/openings_ply1_20_v1.sfen`）。pv_spread_samples==0 の場合は `pv_probe --depth 8 --samples 200` が自動補助。
2. **シャード実行**: 長時間評価は `scripts/nnue/gauntlet-sharded.sh BASE CAND TOTAL_GAMES SHARDS TIME OUT_DIR [BOOK]`。seed は shard 番号でずらし、各 shard も `--threads 1`。
3. **Champion/Teacher 管理**: `runs/ref.nnue`（Single）、`runs/baselines/current/{single.fp32.bin, champion.manifest.json}`、`runs/baselines/current/classic.nnue` をシンボリックリンク＋ manifest で管理。`runs/auto_adopt_classic_from_exp3.sh` などを活用。
4. **cp 表示の校正**: 基本は注釈側で `calibrate_teacher_scale` の mu/scale を `--wdl-scale`/`--teacher-scale-fit linear` に反映。ランタイム USI オプション補正は例外時のみ。Classic v1 エクスポートには `--final-cp-gain` があり、Q16→cp を整える。
5. **ログ/ドキュメント**: 計画は `docs/nnue-training-plan.md`、実施ログは `docs/reports/nnue-training-log.md`、評価手順は `docs/nnue-evaluation-guide.md` で一元管理。
6. **並行実行ルール**: ガントレットは 1 コア相当で実行。他タスクは `taskset` + `nice` + `ionice` で衝突回避（例: `taskset -c 1-31 nice -n 10 ionice -c2 -n7 <cmd>`）。
7. **採否基準**: 短TC (0/10+0.1, 2000局): 勝率 ≥ 55%, |ΔNPS| ≤ 3%（±5% は要追試）。長TC (0/40+0.4, 800–2000局): 勝率 ≥ 55%, ΔNPS は参考（-3% まで）。量子化モデルは FP32 と比較して短TC非劣化・長TC ±1%（±3% 運用可）。
8. **よく使うスクリプト**: `scripts/nnue/phase1_batch.sh`（学習パイプ, 環境変数で O/M/E, TIME_MS, MULTIPV, KD_* 指定）、`scripts/nnue/evaluate-nnue.sh`、`scripts/nnue/gauntlet-sharded.sh`、`scripts/nnue/merge-gauntlet-json.sh`。

## 2. Claude / Coding Expectations
### 早すぎる最適化は禁止
- 「必要になるまで書かない」が原則。測定無しの最適化、将来用のフィールド/フラグ追加、未使用コードの温存は禁止。
- 探索や NNUE のボトルネック議論では、実測データか再現手順をセットで提示してください。

### フォーマット文字列は最新構文を使う
```rust
// ❌ NG
println!("Note: {} positions had errors and were skipped", final_errors);
// ✅ OK
println!("Note: {final_errors} positions had errors and were skipped");
```
- すべての `format!`/`println!`/`eprintln!`/`write!` でインライン補間を用い、`uninlined_format_args` lint を満たすこと。

### 必須チェック
1. `cargo fmt`
2. `cargo clippy --workspace -- -D warnings`
3. `cargo test`
- Clippy の警告は `cargo clippy --fix --allow-dirty --tests` → 手動修正 → 再実行でゼロに。必要なら再度 `cargo fmt`。
- 警告抑止のために安易に `#[allow(...)]` や未使用変数へ `_` 接頭辞を付けることは禁止（既存コードの例外はレビューで合意を得てから適用）。

### 追加ベストプラクティス
- パニックより `Result` を優先し、公開 API には `///` ドキュメントを付与。
- 関数は単機能・短尺・説明的な変数名で。`iter` ベースの表現を優先。
- 仕様変更時はテストを先に追加し、`docs/` の対応するガイドを更新すること。

### コメント/ドキュメント方針（実験ラベルの禁止）
- コードコメントに一時ラベル（例: `S1`/`S2`/`試験A` などフェーズ名）を記載しないこと。
  - 理由: ラベルは文脈依存で将来の読者に伝わらないため。履歴はコミットログとレポートで追跡する。
- 代替方針:
  - コードには「恒久仕様の意図」と「将棋エンジン上の理由」を日本語で簡潔に記載（例: 手駒による連続王手の組合せ爆発を抑えるため 等）。
  - 背景・計測結果・判断は `docs/reports/<YYYYMMDD>-*.md` にまとめ、必要に応じてパス（ファイル名）をコメントに併記して参照可能にする。
  - 他実装を根拠にする場合は “YO 準拠” 等の恒久的な参照語とファイルパスで示し、短期の試行ラベルは使わない。

### 作業ログ（docs/reports）運用ルール
- 目的: 改修・計測・検証の過程を後から振り返れるよう、作業ごとに Markdown でログを残す。
- 出力場所: `docs/reports/`（無ければ作成）。
- 形式: Markdown（拡張子 `.md`）。
- ファイル名規約: 先頭に日付タイムスタンプを付ける。既定は `YYYYMMDD-タイトル.md`。
  - 例: `20251108-作業内容.md`／必要に応じて `YYYYMMDD-HHMM-タイトル.md` でも可。
- Git 取り扱い: 共有を目的としない“作業メモ”が主用途のため、原則コミット不要（任意）。
  - 共有したい内容に限り整形してコミット可。機微情報（鍵・トークン等）が含まれていないか要確認。
  - ローカルのみで差分を出したくない場合は `.git/info/exclude` に `docs/reports/*.md` を追加（個人環境の ignore）。
- 大きな生成物や付随データは `runs/`, `logs/`, `data/` へ配置し、レポートから相対パス参照する。
- 最低限の記載項目テンプレート:

```markdown
# タイトル（簡潔に）

- 日時: 2025-11-08 09:30 JST
- 作業種別: 改修 / 計測 / 再現 / ドキュメント など
- 目的: （何を確認/改善したいか）
- 変更/設定: （ブランチ/コミット、主要 setoption・環境変数・フラグ）
- 実行コマンド: （そのまま貼る。乱数seed/並列数/TCを明示）
- 入出力: （入力データ/ログ/生成物の相対パス）
- 結果サマリ: （数値・差分・グラフの説明。必要なら表や画像参照）
- 次アクション: （続きの実験やパッチ方針）
```

### runs/ 生成物ディレクトリの命名規約（新規）
- 目的: 生成物を時系列で自然ソートできるようにする。ls/エクスプローラでの閲覧性を担保。
- 規約: `runs/<YYYYMMDD>[-HHMM]-<tag>[-subtag]` の形式で作成する。
  - 例: `runs/20251112-tuning`, `runs/20251112-1530-spsa`, `runs/20251112-gauntlet-quick20`, `runs/20251112-postmortem-5s`
  - 既存の `diag-20251112-...` のような日付後置スタイルは新規作成では用いない。
  - サブフォルダ配下の固定レイアウト（例: `runs/gauntlet_usi/`）は従来通りで良いが、当日生成の結果ディレクトリは本規約に揃える。
  - 半角英数字・ハイフンのみを使用し、スペースは使わない。

## 3. USI/探索ガードに関する方針（重要）

### FinalizeSanity を“対局向けの提案”として出さない
- FinalizeSanity（Finalize 前の軽量検査）は、表面的な症状に蓋をする性質が強く、発動時点で既に不利局面に入っていることが多い。
- 本プロジェクトでは、対局品質の議論や改善提案において FinalizeSanity を解決策として推奨しない。
- 代替方針:
  - 探索側の根本整備（アスピレーション/再探索、LMR ゲーティング、再捕獲・二手脅威の拡張など）で未然に抑止する。
  - 出力層での読み筋補完（TT 由来）や PV 再構成は許容（最終手を変えず可視性を上げる目的に限る）。
- 例外: 再現実験・回帰切り分けのための一時的スイッチとして言及するのは可。ただし「最終採用策としての提案」は不可。

> 実務ルール: レビューやPR、チャット上の提案で FinalizeSanity を“採用策”として挙げないこと。必要時は探索ロジック/時間管理/出力PV補完など根治策を優先して検討する。

## 4. ログ分析ワークフロー（USIログ→落下局面→再現→再評価）

> 詳細な探索パラメータ調整フローと構造化ログ（`moves.jsonl`）→`targets.json`→`run_eval_targets.py` の配管は  
> `docs/tuning-guide.md` に集約してあります。ここでは実行スクリプトと典型コマンドのみを列挙します。

新規セッションで USI 対局ログから“評価落下（スパイク）”を見つけ、問題局面を短時間で再現・再評価するための標準手順です。各ステップは単独でも利用可能です。

### 4.1 事前準備
- 推奨カレント: `packages/rust-core`（本ディレクトリ）。
- 環境整備: エンジンは `target/release/engine-usi` を前提。未ビルド時は `cargo build -p engine-usi --release`。
- `rg`（ripgrep）を使うスクリプトが多いので、空振りで止まらないラッパを用意:
  ```bash
  rg(){ command rg "$@" || true; }; export -f rg
  ```

### 4.2 概況サマリの取得（CSV）
- スクリプト: `scripts/analysis/analyze_usi_logs.sh`
- 目的: 最大深さ・seldepth・PV切替・near_final 等の概況を1行CSV化。
- 実行例:
  ```bash
  bash scripts/analysis/analyze_usi_logs.sh taikyoku-log/taikyoku_log_YYYYMMDDHHMM.md \
    | tee runs/diag-$(date +%Y%m%d)/summary.csv
  ```

### 4.3 評価スパイクの抽出（落下セグメント候補）
- スクリプト: `scripts/analysis/extract_eval_spikes.py`
- 目的: 指定閾値以上の評価変動点を抽出し、replay 用プレフィクスを列挙。
- 主なオプション:
  - `--threshold <cp>`: 変動絶対値の下限（既定 300）。
  - `--back <plies>` / `--forward <plies>`: 前後に含める文脈手数（既定 3/2）。
  - `--topk <k>`: 上位K件に絞る（0で無制限）。
  - `--out <dir>`: 出力先（未指定時は `runs/analysis/spikes-<basename>`）。
- 実行例:
  ```bash
  python3 scripts/analysis/extract_eval_spikes.py \
    --threshold 200 --back 4 --forward 2 --topk 6 \
    --out runs/diag-$(date +%Y%m%d)-spikes \
    taikyoku-log/taikyoku_log_YYYYMMDDHHMM.md
  # 生成物: summary.txt / evals.csv / spikes.csv / prefixes.txt
  ```

### 4.4 事前手数リプレイ（MultiPV/Threads/秒読み指定）
- スクリプト: `scripts/analysis/replay_multipv.sh`
- 目的: ログ終盤の「position startpos moves …」から、指定プレフィクス長で直前局面を多数再現し、bestmove と最終 info を収集。
- 主なオプション:
  - `-p "<num ...>"`: 再現する手数プレフィクス（例: `"24 26 28 30"`）。
  - `-e <path>`: エンジン（既定 `target/release/engine-usi`）。
  - `-o <dir>`: 出力先（既定 `runs/game-postmortem/<date>-10s`）。
  - `-m <n>`: MultiPV（既定 1）。
  - `-t <n>`: Threads（既定 8）。
  - `-b <ms>`: byoyomi ミリ秒（既定 10000）。
  - `--profile match|postmortem`: 追加 setoption（`postmortem` はデバッグ寄り）。
  - `--inherit-setoptions|--no-inherit-setoptions`: ログ内 setoption の継承有無（既定 継承）。
- 実行例（5秒・8スレッド、スパイク上位）:
  ```bash
  PREF=$(cat runs/diag-20251110-1854_spikes/prefixes.txt)
  bash scripts/analysis/replay_multipv.sh taikyoku-log/taikyoku_log_YYYYMMDDHHMM.md \
    -p "$PREF" -o runs/game-postmortem/$(date +%Y%m%d)-5s \
    -t 8 -m 1 -b 5000 --profile match
  # 出力: summary.txt（bestmove/last_info を集約）
  ```

### 4.5 ターゲット生成（“真の悪手”精度向上のための遡り）
- 目的: 落下点の2〜5手前から同条件で再評価し、符号反転の起点（真の悪手）を見つける。
- スクリプト1: `scripts/analysis/extract_positions_from_log.py`
  - `--log <path>`: USIログ。
  - `--out <path>`: 出力 JSON（`targets.json` 推奨）。
  - 実行例:
    ```bash
    python3 scripts/analysis/extract_positions_from_log.py \
      --log taikyoku-log/taikyoku_log_YYYYMMDDHHMM.md \
      --out runs/diag-YYYYMMDD/targets.json
    ```
- スクリプト2: `scripts/analysis/expand_targets_back.py`
  - `--in <targets.json>` / `--out <targets_back.json>`
  - `--min <plies>` / `--max <plies>`: 何手遡るかの範囲。
  - 実行例:
    ```bash
    python3 scripts/analysis/expand_targets_back.py \
      --in runs/diag-YYYYMMDD/targets.json \
      --out runs/diag-YYYYMMDD/targets_back.json \
      --min 2 --max 5
    cp runs/diag-YYYYMMDD/targets_back.json runs/diag-YYYYMMDD/targets.json
    ```

### 4.6 3プロファイル再評価（base / rootfull / gates）
- スクリプト: `scripts/analysis/run_eval_targets.py`
- 目的: 同一局面を3つの探索プロファイルで再評価し、落下の再現性や軽減度合いを可視化。
- 既定プロファイル（スクリプト内定義）
  - `base`: `SearchParams.RootBeamForceFullCount=0`
  - `rootfull`: `SearchParams.RootBeamForceFullCount=4`
  - `gates`: `SearchParams.RootBeamForceFullCount=0`, `RootSeeGate.XSEE=0`, `SHOGI_QUIET_SEE_GUARD=0`
- 主なオプション/環境変数:
  - `--threads <n>`（既定 1）, `--byoyomi <ms>`（既定 2000）, `--minthink <ms>`, `--warmupms <ms>`
  - `ENGINE_BIN=<path>` を設定すると別バイナリを使用可。
- 実行例（1秒, 8T, MultiPV=3 内蔵）:
  ```bash
  ENGINE_BIN=target/release/engine-usi \
  python3 scripts/analysis/run_eval_targets.py runs/diag-YYYYMMDD \
    --threads 8 --byoyomi 1000 --minthink 100 --warmupms 200
  # 出力: runs/diag-YYYYMMDD/summary.json（tag/ profile / eval_cp / depth）
  ```

### 4.7 A/B ガントレット（任意）
- ツール: `./target/release/usi_gauntlet`
- 目的: base と candidate の勝率比較（短TCは `concurrency=1` 推奨）。
- 実行例:
  ```bash
  ./target/release/usi_gauntlet --engine target/release/engine-usi \
    --base-init runs/gauntlet_usi/init/base_init.usi \
    --cand-init runs/gauntlet_usi/init/cand_init.usi \
    --book runs/gauntlet_usi/short20.sfen \
    --games 20 --byoyomi-ms 500 \
    --engine-threads 8 --hash-mb 1024 \
    --concurrency 1 --adj-enable \
    --out runs/gauntlet_usi/quick20_byoyomi500
  ```

### 4.8 時間見積りと時短のコツ（FAQ）
- `run_eval_targets.py` は「ターゲット数 × 3プロファイル × (byoyomi+約6秒)」相当で延びます。
- まず `extract_eval_spikes.py --topk` で対象を少数に絞り、`--byoyomi 500~1000`＋`--warmupms 0~200` で粗く傾向を掴む。
- スレッドは対局再現と合わせる（例: 8）。ガントレットは `concurrency=1` が安定。

### 4.9 Dropガード回帰セット（pre-50〜54 + drop_21/drop_28 ターゲット）
- スクリプト: `scripts/analysis/run_dropguard_regression.sh`
  - 役割: 9筋押し合いセグメント（プレフィクス43–54）を `replay_multipv.sh`（byoyomi=10s, MultiPV=3, Threads=8）で再抽出し、同時に `drop_21_line1179` / `drop_28_line1484` 系の back2〜5 を `run_eval_targets.py` で再評価。
  - ターゲット定義は `scripts/analysis/regression_targets/drop_guard_targets.json` に集約（`runs/diag-20251111_statscore` と同一内容）。
  - 主要オプション（長時間なので `timeout` せず実行推奨）:
    ```bash
    # デフォルト: log=taikyoku_log_enhanced-parallel-202511101854.md, prefix=50..54
    scripts/analysis/run_dropguard_regression.sh \
      --log taikyoku-log/taikyoku_log_enhanced-parallel-202511101854.md \
      --out runs/regressions/dropguard-$(date +%Y%m%d-%H%M)
    ```
    - `--mp-byoyomi`, `--mp-multipv`, `--eval-byoyomi` などで短TC版も可。
    - 出力: `…/multipv/summary.txt`（pre-50..54 の MultiPV 情報）、`…/diag/summary.json`（run_eval_targets の結果）。
  - 使い所: 探索改修後に drop guard セグメントの劣化をクイック確認する回帰セットとして活用。

### 5. 計測指標（first_bad/avoidance）と A/B 運用（重要）

> 指標の定義や評価フロー全体（`pipeline_60_ab.sh` → `run_eval_targets.py` → `run_ab_metrics.sh` など）は  
> `docs/tuning-guide.md` の「NNUE 前探索パラメータ調整フロー（概要）」に整理してあります。  
> ここでは AGENTS 向けに優先度と代表的なスクリプトだけを抜粋します。
- 目的: 「真の悪手を避けられるか」を中心に、対局品質に直結する指標で A/B を評価する。
- 指標の優先度（高→低）:
  - 悪手回避率（avoidance_rate）: first_bad タグにおいて“原ログの悪手”と異なる手を選べた割合。高いほど良い（最重要）。
  - first_bad 限定スパイク率: 比較採否に使わない。first_bad は“既に悪手後の局面”で深いマイナスは妥当なため、ゼロ化を目的にしない（診断の参考表示に留める）。
  - overall スパイク率: 症状の平均。採否の主指標にはしない（大幅悪化のみ警戒）。
  - avg_depth/NPS: 副作用監視（浅くなり過ぎないか）。
- first_bad の定義: 落下点ログから back 2..6 手を生成し、「その origin で最初に cp≤-600 を満たす back」を first_bad とする（スクリプトが自動導出。A/B 実行時は“そのプロファイルの再評価結果”で判定）。
- 推奨しきい値: 悪手回避後の“非大幅悪化”は `eval_cp > -200` で判定（任意）。
- ディレクトリ規約: `runs/<YYYYMMDD>[-HHMM]-<tag>` を厳守（例: `runs/20251112-1530-ab-foo`）。
- 主要スクリプト:
  - `scripts/analysis/pipeline_60_ab.sh`: ログ→スパイク抽出→back生成→60件選別→10秒評価→CSV/メトリクス出力（ワンコマンド）。
  - `scripts/analysis/run_ab_metrics.sh`: 既存データセット（`targets.json`）に対し、複数プリセットを直列評価し、overall/first_bad（参考）/avoidance（最重要）をまとめて出力。
  - `scripts/analysis/summarize_first_bad_metrics.py`: first_bad 限定スパイク率を算出（CSV不在時は summary/targets から導出）。
  - `scripts/analysis/summarize_avoidance.py`: first_bad について原ログから“悪手”を復元し、評価 bestmove と比較して回避率を算出。
- コマンド例:
  - データセット作成（60件, 10秒）:
    - ``ENGINE_BIN=target/release/engine-usi scripts/analysis/pipeline_60_ab.sh --logs 'taikyoku-log/taikyoku_log_enhanced-parallel-202511*.md' --out runs/$(date +%Y%m%d-%H%M)-tuning --threads 8 --byoyomi 10000``
  - A/B（first_bad/avoidance 含む一括評価）:
    - ``scripts/analysis/run_ab_metrics.sh --dataset runs/20251112-2014-tuning --out-root runs/$(date +%Y%m%d-%H%M)-ab scripts/analysis/param_presets/f1e47_lmp_mid.json scripts/analysis/param_presets/f1e47_lmp_mid_lmr200.json``
  - 出力物（各プリセット配下）: `metrics.json`（overall）, `metrics_first_bad.json`（first_bad 限定, 参考）, `avoidance.json`（回避率）
- 所要時間目安: 60件×10秒は 1プリセット ≈ 16分（±数分）。直列3案で ≈ 50分。
- 並行実行の注意: 同一データセット配下で `run_eval_targets.py` を多重実行しない（`summary.json` 競合を避ける）。
- Finalize/MateGate/InstantMate は“安全弁”。採用策ではなく診断に限定（方針 3 を再確認）。
