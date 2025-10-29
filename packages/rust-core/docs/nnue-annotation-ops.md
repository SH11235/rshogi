# NNUE 注釈探索 運用ポリシー（恒久版）

本書は、[search-stabilization ブランチでの開発](https://github.com/SH11235/shogi/pull/195) ログで「恒久的に残すべき運用知見」を統合したものです。
探索側の安定化（P0/P1/P1.5）、計測・運用指針、トグル類の仕様を簡潔にまとめます。

## 目的とスコープ
- 目的: 学習データ生成（generator）の歩留まりを最大化しつつ、探索の健全性（PV着地、一貫した境界/スコア）を維持。
- 対象: `packages/rust-core/crates/engine-core`（探索）、`crates/tools`（generator）。

## 不変条件（安全側の原則）
- スコア/境界/PVのソース整合
  - TTがExactなら `lines[0].score_internal` はTTの内部スコア、`lines[0].bound=Exact`。それ以外は探索結果のscore/bound。
- SearchResult全体の `node_type` は後付けで書き換えない（必要なら `lines[0].bound` のみ変更）。
- 合成PVの先頭手は合法性チェックを通す（`pos.is_pseudo_legal && pos.is_legal_move`）。

## P0（空PVゼロ化：既定ON）
- 合成優先順: `lines[0]` → `stats.pv` → `best_move`（1手PV）。
- 実装要点（抜粋）
  - `parallel/mod.rs`: `synthesize_primary_line_from_result` を `finish_single_result` / `combine_results` 直後に適用（最後に一度だけ `refresh_summary`）。
  - `engine/controller.rs`: `finalize_pv_from_tt`（TT復元、Exactのみ採用、合法性チェック）。
  - タイブレーク: 完全同値では Exact を優先。
  - `mate_distance` 付与、`time_ms` は u128→u64 clamp、`exact_exhausted=false`。

## P1（アスピ安定化＋着地保証：既定ON）
- 連続アスピ失敗（同一イテで計2回）→ 直ちに FullWindow 再探索。
- 近締切ゲート（共通化済み）
  - main近締切= hard/5（80..600ms）、near-hard確定= hard/6（60..400ms）。
  - NearHard finalize は primaryのみ一度だけ送出。送出後は Soft を完全抑止。ponder中は送らない。
  - TMでhardがない場合は soft をcapにして「新イテ抑止/縮退」のみ適用（NearHardは送らない）。
- MultiPV縮退: 近締切帯では 1 へ縮退（PV1優先）。

## P1.5（Near‑final 狭窓検証：既定OFF）
- 内容: 近ハード帯で PV1先頭手を狭窓（[s−Δ, s+Δ]、既定Δ=1cp）で1回だけ検証。窓内ヒットで `lines[0].bound=Exact`、`score_internal←検証値`。既にExactならスキップ。
- 予算: `BUDGET_MS` を qnodes に換算し上限にクランプ。`MIN_DEPTH` と `MIN_TREM_MS` で実行ガード。
- 代表トグル（環境変数）
  - `SHOGI_ZERO_WINDOW_FINALIZE_NEAR_DEADLINE`（0/1, 既定0）
  - `SHOGI_ZERO_WINDOW_FINALIZE_VERIFY_DELTA_CP`（既定1, 1..32）
  - `SHOGI_ZERO_WINDOW_FINALIZE_BUDGET_MS`（既定80, 10..200）
  - `SHOGI_ZERO_WINDOW_FINALIZE_MIN_DEPTH`（既定4, 1..64）
  - `SHOGI_ZERO_WINDOW_FINALIZE_MIN_TREM_MS`（既定60, 5..500）
  - `SHOGI_ZERO_WINDOW_FINALIZE_MIN_MULTIPV`（既定0）
  - `SHOGI_ZERO_WINDOW_FINALIZE_SKIP_MATE`（既定0）
  - `SHOGI_ZERO_WINDOW_FINALIZE_MATE_DELTA_CP`（既定0, 0..32）
- 推奨: 既定OFF。End-heavy×高MPVでスポットON（目安: 800ms/MPV7→Δ=2、1200ms/MPV10→Δ=3）。

## 計測KPI（generator/探索 共通）
- 収率: `empty_pv_rate`（≈0%を目標）、`top1_exact_rate`、成功率（success/attempted）。
- 安定性: `aspiration_failures/re_searches（p99≤2）`, `root_fail_high_count`。
- 近締切: `near_deadline_params（origin, main/fin windows, t_rem）`、`multipv_shrunk`、`skip_new_iter`。
- パフォーマンス: `NPS`, `seldepth`, `tt_hit_rate`。

## A/Bの代表結果（2025-10-29）
- 短TC/浅深（300ms/MPV3）: P1.5の恩恵は小（Top1Exact/成功率 微減〜±0、NPS −1〜−2%）。
- 中TC/高MPV（600/1000ms, MPV5）: 効果は条件依存（Top1Exact +0〜+0.1pt、NPS −1〜−2%）。
- End-heavy/高MPV（800ms/MPV7, 1200ms/MPV10）: Top1Exact +0.1〜+0.2pt、NPS ±2%内（条件により増減）。
- 具体値・成果物は `docs/reports/nnue-ab-20251029.md` を参照。

## generator の見方（簡易）
- `Batch complete: N results …` は「そのバッチの成功数（採用数）」。
- `Overall progress: A/B (P%)` は「成功累計/総件数」。バッチ進行とP%が一致しないのは仕様（成功のみ加算）。
- 詳細は `docs/nnue-generator-faq.md` を参照。

## 推奨運用（まとめ）
- 既定: P0+P1のみで回し、P1.5はOFF。
- End-heavy×高MPVの注釈ジョブで、Top1Exactをわずかに押し上げたいときのみP1.5 ON（Δ=2〜3cp）。
- KPIとNPSのトレードオフを監視（±2%を許容レンジ目安）。

## 参考リンク
- A/B当日レポート: `docs/reports/nnue-ab-20251029.md`
- generator FAQ: `docs/nnue-generator-faq.md`
- ランタイムトグル一覧/解説: `docs/tuning-guide.md`
- 近締切ログ（例）
  - 実行: `near_final_zero_window=1 budget_ms=.. budget_qnodes=.. qnodes_limit_pre=.. qnodes_limit_post=.. t_rem=.. qnodes_used=.. confirmed_exact=0|1`
  - スキップ: `near_final_zero_window_skip=1 reason=already_exact|min_depth|trem_short|min_multipv|mate_near`
