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
- `crates/tools`: ベンチ/ユーティリティバイナリ（TT テスト、NNUE 解析など）。
- `docs/`: 設計/NNUE/パフォーマンス/USI ビルド手順。`docs/nnue-*`, `docs/performance/*`, `docs/usi-engine-build.md` を参照。
- `scripts/analysis|bench|gauntlet|nnue|repro|utils`: エンジン比較、NNUE 評価、バグ再現用スクリプト群。
- `runs/`, `data/`, `converted_openings/`, `logs/`: ガントレット、開幕集、学習ログなどの成果物置き場。

### ビルド & テスト
- 全体: `cargo build` / `cargo build --release`。高速化は `RUSTFLAGS="-C target-cpu=native"` 推奨。
- 個別: `cargo build -p engine-core`, `cargo run -p engine-usi -- --help`, `cargo run -p engine-usi --release --features diagnostics` 等。
- テスト: `cargo test`, `cargo test -p engine-core`, 任意モジュールは `cargo test path::to::case -- --nocapture`。
- ベンチ: `cargo bench -p engine-core`（結果は `target/criterion/`）。
- Lint/Format: `cargo fmt --all`, `cargo clippy --workspace -- -D warnings`（CI 準拠）。
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

USI ログや外部 GUI の対局ログ、gauntlet の `moves.jsonl`、および Selfplay（`selfplay_basic`）のログをどう扱うかについては、
詳細な手順とコマンド例をすべてドキュメント側に集約しています。

- 外部 GUI / サーバ由来の USI ログや gauntlet ログを解析したい場合  
  → [`docs/log-analysis-guide.md`](./docs/log-analysis-guide.md) を参照してください。  
  Python スクリプト群（`scripts/analysis/*.py` / `*.sh`）を使った  
  「評価スパイク抽出 → 事前手数リプレイ → ターゲット生成 → 再評価」のパイプラインがまとまっています。

- `selfplay_basic` が出力する Selfplay ログ（`runs/selfplay-basic/*.jsonl` + `.info.jsonl`）を使って  
  ブランダー抽出や再評価を行いたい場合  
  → [`docs/selfplay-basic-analysis.md`](./docs/selfplay-basic-analysis.md) を参照してください。  
  Rust 製 CLI（`selfplay_basic` / `selfplay_blunder_report` / `selfplay_eval_targets`）を中心にした自己対局フローを説明しています。

AGENT としてはここでは「どの種類のログに対してどのガイドを見るべきか」だけを意識し、  
具体的なコマンドやオプションは上記ドキュメントを開いて確認してください。

### 5. 計測指標（first_bad/avoidance）と A/B 運用

探索パラメータの A/B 比較や first_bad / avoidance 指標の定義・運用フローは、ドキュメント側に整理しています。
詳細な説明やコマンド例が必要な場合は次を参照してください。

- 概念・指標の定義と A/B 評価フロー全体  
  → [`docs/tuning-guide.md`](./docs/tuning-guide.md) の「NNUE 前探索パラメータ調整フロー（概要）」セクション

- 外部 USI ログからのデータセット作成（`pipeline_60_ab.sh` など）と  
  `run_eval_targets.py` / `run_ab_metrics.sh` を使った指標計測の具体的な手順  
  → [`docs/log-analysis-guide.md`](./docs/log-analysis-guide.md) の  
    「指標と A/B 評価（first_bad / avoidance）」セクション
