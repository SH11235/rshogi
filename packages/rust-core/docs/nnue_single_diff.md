# NNUE Single 差分更新（SingleChannelNet）

本ドキュメントは、Engine Core に実装された SINGLE_CHANNEL（HalfKP → ReLU → 1出力）の差分更新土台について、概要と現状の制約、使い方をまとめたものです。

## 概要

- ネット構成: `acc_dim=256` 前提の 1 チャンネル簡易 NNUE。
- 内部表現: ReLU 遅延（pre のみ保持）。
  - 差分適用は `pre_*` のみに加減算し、評価直前に一度だけ `max(pre, 0)` を適用して出力層の内積を計算します。
  - `acc_for(color)` は pre（負値を含み得る）を返します。評価には `evaluate_from_accumulator_pre(acc_for(...))` を使用してください。
- 視点分離: 黒視点（king=Black）と白視点（king flip）の fid を完全に分離し、`removed_b/w`, `added_b/w` で別々に更新。

## 並列・フォールバック方針

- `NNUEEvaluatorWrapper::evaluate` では `tracked_hash` が一致しない場合は安全側で **フル評価にフォールバック**。
- Classic(HalfKP) は一時 Accumulator を `refresh` して評価、Single は `net.evaluate(pos)` を呼びます。
- フォールバックは正しさ最優先。並列最適化は将来の Step 6（スレッドローカル Acc など）で検討。

### 運用規約（重要）

- 盤面更新は必ずラッパのフック対で行うこと（`do_move(pre_pos, mv)` → `pos.do_move(mv)`／`pos.undo_move(mv, u)` → `undo_move()`）。
  - ラッパを経由しない局面更新（HookSuppressor 等）が入る経路は、評価時に安全側のフル評価へフォールバックする。
- Null-move は `do_move(Move::null())`／`undo_move()` を呼ぶ（Acc は複製で積み、縮退しない）。
- `restore_single_at()` 直後は `tracked_hash = Some(pos.hash)`。最初の `do_move` までに `pos` を直接変更しないこと（不一致時はフォールバック）。

## 計測（ベンチ）

`nnue_benchmark` にて 3 モードを計測します。

- Refresh-only: `refresh(pos) → evaluate_from_accumulator_pre(acc_for(...))`
- Incremental (ApplyOnce): `acc0=refresh(pos)` を固定して、各 `mv` に `apply_update(&acc0, pos, mv)` を適用
- Incremental-Chain: 盤面と acc を前進させながら連鎖（`acc←apply_update(acc, p, mv); p.do_move(mv)`）

実行例:

```
cargo run -p tools --bin nnue_benchmark -- \
  --single-weights runs/nn/mock_base.nnue --seconds 3
```

## 既知の制約・今後

- 現状、並列探索（HookSuppressor 経路）ではフル評価フォールバック（正しさ優先）。
- `removed_*/added_*` は重複 fid を `sort_unstable+dedup` で軽減済み。さらなる合成（相殺）で最適化の余地あり。
- `scale` は学習メタとして保持。推論は `w2/b2` が cp 直値で整合している前提（トレーナ側でゲイン・オフセットを焼き込み）。

## テスト

- 差分 vs リフレッシュの整合（通常手／打ち／捕獲／成り／王移動フォールバック）。
- 成駒の捕獲→手駒化のケースを追加済み。
- 2 手連鎖（非トリビアルネット）で ReLU 交差や視点分離の穴を検証。
