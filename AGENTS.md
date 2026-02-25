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

## YO alignment 調査の既知事実

乖離調査時に再調査不要な確認済み領域:

- **TT 実装**: エントリサイズ、クラスタ構造、probe/save/replacement policy、generation — 全て YO と完全一致確認済み
- **NNUE 評価値**: 同一局面で fresh NNUE eval は完全一致
- **TT eval 信用問題**: RS は `eval_helpers.rs` で常に `nnue_evaluate()` を呼ぶよう修正済み（YO の `USE_LAZY_EVALUATE` 未定義動作に準拠）
- **pinned_pieces_excluding**: avoid 駒を pinner 候補から除外するよう修正済み
- **root correction_value**: in_check に関わらず常に計算するよう修正済み（LMR の r 計算で使用されるため）

**再発パターン**:
- `mate_1ply` の差異 → TT カスケード伝播 → 大規模ノード乖離。新しい乖離が見つかったら `mate_1ply` を最初に A/B テストまたは FEATURE_COUNT で確認すべき。
- **バッファ collect パターン**: MovePicker の手を事前にバッファに全 collect してからイテレートすると、TT手の探索前にスコアリングが固定され、captureHistory 等の history 値が探索後と異なる。YO 準拠の逐次 `next_move` 方式を使うこと。（ProbCut で発見・修正済み）

### SE と stack 上書きの注意

YO のコード順序（reduction → ttPv調整 → lmrDepth → Step14 → SE → do_move）を忠実に守る。SE の再帰 search_node が同一 ply の stack を上書きするため、SE 前に参照すべき値（tt_pv 等）を SE 後に参照するとバグになる。

### 調査効率化の原則

1. **ログが出ない = 条件ミスではなくコードパスが異なる可能性を疑う**。main moves loop で出ないなら ProbCut/NullMove 等の pre-loop パスを確認。
2. **PLY drill-down が深い（p>=4）場合は A/B テストを先に試す**。疑わしい機能を両エンジンで無効化→乖離消失の確認は O(1) で原因経路を絞れる。
3. **静的コード比較で見つからない乖離は「実行タイミングの差」を疑う**。同一式でも history/TT の参照タイミングが異なるとスコアが変わる。
4. **r が全 root move で同一の定数オフセット → root レベル変数を疑う**。correction_value, improving, delta 等の root 共通変数が原因。per-move 変数なら手ごとに値が異なるはず。

### YO乖離調査の最短導線（2026-02-23 追記）

- 新しい乖離が出たら、まず `.claude/skills/yo-measure/SKILL.md` の「最短ルート（first mismatch固定）」に従うこと。
- 最初にやることは必ず以下の順:
  1. `depth` 一致帯を確定（どこまで一致するか）
  2. 乖離が出る最初の `iter/mc/mv` を root 粒度で1件確定
  3. `root+pm chain+ply+depth(+window)` で同一文脈ゲートして1 plyずつ降りる
  4. `val` ではなく `nd` の最初の差分点を固定
  5. 必要時は `TT probe/write` を `seq + fullkey + cluster/slot` で時系列化
- `alpha/beta` 単独ゲート、全return箇所への無差別ログ追加は非推奨（混入とノイズで時間を失いやすい）。

詳細は `docs/performance/yo_alignment_status.md` と `.claude/skills/yo-measure/SKILL.md` を参照。
今回の実例は `docs/performance/yo_alignment_case12013_findings_20260222.md` を参照。

## 性能制約

- ホットパスでのヒープ割り当て禁止
- 評価ループ内での `Vec` 再割り当て禁止
- スタック割り当てと const generics を優先
