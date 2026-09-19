# rshogi Agent Guide

## 0. スコープと言語
- ここは Rust で書かれた将棋エンジン rshogi のリポジトリです。
- このコードベースでは日本語でやり取りすること。ドキュメントやコメントも可能な限り日本語（英語併記可）でまとめてください。

## Claude / Coding Expectations

### 早すぎる最適化は禁止
- 測定なしの最適化は禁止。ボトルネック議論では実測データか再現手順をセットで提示すること。

### YAGNI（必要になるまで書かない）
- 将来用のフィールド/フラグ追加、未使用コードの温存は禁止。

### 追加ベストプラクティス
- パニックより `Result` を優先し、公開 API には `///` ドキュメントを付与。

### 必須チェック
1. `cargo fmt && cargo clippy --fix --allow-dirty --tests`
2. `cargo test`
3. `bash scripts/check-tracked-abs-paths.sh`（tracked file に machine 依存の絶対パスを書かない。データは `$SHOGI_DATA`、外部 repo は `/path/to/...` を使う）
- Clippy の警告は `cargo clippy --fix --allow-dirty --tests` → 手動修正 → 再実行でゼロに。必要なら再度 `cargo fmt`。
- 警告抑止のために安易に `#[allow(...)]` や未使用変数へ `_` 接頭辞を付けることは禁止。

### 共有データ（`$SHOGI_DATA`）

教師データ / NNUE モデル / progress 係数 / 学習 checkpoint は repo 外の共有 root
（環境変数 `$SHOGI_DATA`）に置く。layout:

- `teachers/` — 教師データ (PSV / packed bin)
- `nnue/` — engine 配備モデル（USI `EvalFile` で読む）
- `progress/` — progress 係数（`LS_PROGRESS_COEFF`）
- `runs/bullet/`, `runs/tatara/` — 各 trainer の学習 checkpoint

各自の共有データ root を `SHOGI_DATA` に export して使う。tracked file には machine
固有の絶対パスでなく `$SHOGI_DATA` を書く（外部 repo は `/path/to/...` placeholder）。

## ツール開発（crates/tools）

### 大量データを扱うツールはスケーラブルに設計する
- 教師データの生成・抽出・処理系ツールは数千万〜億局面規模の入力を前提とする。
  全件を `Vec` 等に load してから処理する設計（load-all）は禁止し、入力件数に対して
  ピークメモリが線形に増える実装を避ける。streaming / reservoir sampling 等で
  ピークメモリを入力件数に非依存にすること。
- これは「早すぎる最適化は禁止」と矛盾しない。入力が大量だと確定している領域の
  スケーラビリティは micro-optimization ではなく設計要件である。スケーラビリティ
  改修時は before/after の実測（ピーク RSS・処理時間）を添える。

### ツールの新規追加・改修時はユーザー doc を更新する
- `crates/tools` のバイナリを追加・改修したら、同じ PR で
  `crates/tools/docs/<tool>.md` と索引（`crates/tools/docs/tools-reference.md` /
  `crates/tools/README.md`）を更新する。
- CLI フラグ・出力ファイル・セマンティクスを変えたら doc も必ず合わせる。

### perf / メモリのレビュー指摘を「現スケールでは問題ない」で却下しない
- perf / メモリの指摘が出たら、現在のテスト規模ではなく**想定する最大入力規模**で
  見積もってから採否を決める。本番入力が桁違いに大きいと、現スケールの感覚で
  却下した指摘が本番でメモリ破綻になる。

### サンプリング / 抽出ツールは決定性を担保する
- seed 固定 + 入力順固定（パス・ID のソート）で出力が bit 一致することを保証し、
  同一 seed 2 回実行の一致をテストで検証する。HashMap 反復順などの非決定の混入に注意。

## Unsafe コードポリシー

- `unsafe` は原則禁止
- 許可される場所: SIMD最適化、スタック割り当て、置換表など性能上必須な箇所、
  および安全な代替 API が存在しない外部ライブラリ起因バグの回避（例: exit 時
  デストラクタ連鎖の破損を避ける `_exit`）のみ
- 各 `unsafe` ブロックには以下を必ずコメントで記載:
  - なぜ安全か
  - 守るべき不変条件

## 長時間実行タスク

- 自己対局・ベンチマーク等の長時間コマンドは `run_in_background: true` でバックグラウンド実行すること
- これにより Claude がプロセス監視・結果集計を自律的に行える
- 完了待ちには `TaskOutput` ツールを使用

## 性能制約

- ホットパスでのヒープ割り当て禁止
- 評価ループ内での `Vec` 再割り当て禁止
- スタック割り当てと const generics を優先
