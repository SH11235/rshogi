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
- Clippy の警告は `cargo clippy --fix --allow-dirty --tests` → 手動修正 → 再実行でゼロに。必要なら再度 `cargo fmt`。
- 警告抑止のために安易に `#[allow(...)]` や未使用変数へ `_` 接頭辞を付けることは禁止。

## Unsafe コードポリシー

- `unsafe` は原則禁止
- 許可される場所: SIMD最適化、スタック割り当て、置換表など性能上必須な箇所のみ
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

## YaneuraOu 比較調査

- 計測結果・乖離分析: `docs/performance/yo_alignment_status.md`
- YO ソース正規パス: `/mnt/nvme1/development/YaneuraOu/source/`
- 比較スキル: `.claude/skills/yo-compare/SKILL.md`
- YaneuraOu と実装詳細差異があれば必ず修正して仕様を完全に一致させること

### ビルド・計測の注意

- rshogi: ソース変更後は `cargo clean && cargo build --release` を推奨（incremental cache 破損防止）
- YO: `make clean COMPILER=g++ && make COMPILER=g++ -j$(nproc)`、FV_SCALE=24 必須
- YO 出力には `grep -a` を使用（バイナリ文字混入対策）
- root move ごとのノード数は rshogi 側で +1 ずれる（合計は一致）

### YO の設計知識

- `if (ss->inCheck) { goto moves_loop; }` で eval〜pruning を全スキップ → rshogi では `!in_check` ガード必須
- `moved_piece(m)` = `moved_piece_after(m)`（成り後の駒、Stockfish と異なる）
