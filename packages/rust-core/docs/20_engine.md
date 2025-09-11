# 20_engine — 棋力向上（B/C以降のみ）

本書はエンジン側の「棋力向上」トラックに関する残タスクのみを集約します。サニティ（A セクション）は削除済みのため、実装タスク一覧からも対象を外すか Done 扱いとします。測定条件・Gate は `docs/00_charter.md` に従います。

## フェーズ構成（抜粋）

### Phase 1（Next の筆頭）: Classic NNUE 合流 or Single 差分更新
- 目的: NPS × 表現力の土台を確立
- 要点:
  - 推論/学習を HalfKP（256×2→32→32→1, ClippedReLU）で統一
  - 差分更新を実装（Single 継続時は暫定差分更新で対応）
- DoD:
  - Single 比で NPS +2〜5x（環境依存）を達成
  - 学習/推論のアーキ整合（重みの相互運用）
 - 仕様シート（別紙）: Classic NNUE（特徴量/活性/モデル形式/差分更新/シリアライズ互換）

### B-1 教師データ拡大（データ主導で強化）
- 強い教師での再注釈、曖昧局面の深掘り（multipv=2, 高 TT, 長思考）
- 分布を序/中/終盤で均し、詰み境界を増量
- パイプラインは `10_pipeline.md` の固定条件/Gate で検証

### B-3 Hard Mining（常時回収）
- 新モデルで自己対局/ベンチ→評価ブレ/不自然局面抽出→再注釈→mini-epoch 追学習
- #13 ガントレットと連動し、常時昇格判定を行う

---

## 実装タスク（E.一覧の整理版）
- [ ] Single 差分更新（暫定） or Classic NNUE の推論・学習に統一（Phase 1）
- [ ] PSV→JSONL 変換ユーティリティ（または既存コーパス取り込み手順）
- [ ] 学習レジメン更新（batch≥8k, updates≥5k, lr scheduler, 重み付け）
- [ ] Hard Mining ワークフロー（extract→再注釈→再学習）
- [ ] ミニマッチ/ベンチ スクリプト（固定ブック・反転・集計）

注: 旧「A（必須サニティ）」由来の項目は本リストから除外/Done 表記し、齟齬を防止します。

参考仕様シート（作成優先）
- Classic NNUE 仕様（簡易）: 特徴量（HalfKP 定義）、活性（ClippedReLU 閾値）、レイアウト、量子化互換、往復整合テスト
- PSV→JSONL 変換: 対応フィールド、ドロップ情報、エラー方針（fail-closed）、小さなゴールデンテスト
- Hard Mining 抽出: 既定閾値（`|Δeval| ≥ X cp`, `Δnodes ≤ Y%`）、タグ語彙（`king_safety`, `sacrifice`, `tsume_boundary` など）

---

## Sprint γ（棋力の底上げ・両輪化）
- Engine Phase 1: Classic NNUE 合流 or Single 差分更新
- B-1 教師データ拡大 / B-3 Hard Mining の回し込み（#13 ガントレットで昇格判定）

---

## DoD（エンジン側まとめ）
- Phase 1 完了時点で、推論/学習が HalfKP で統一され、差分更新が機能
- ガントレット基準（スコア率 +5%pt（=55%） & NPS ±3%以内）を満たす候補が継続的に昇格
- レポートは `docs/reports/` に蓄積、`00_charter.md` の契約（ログ/データ）に準拠
