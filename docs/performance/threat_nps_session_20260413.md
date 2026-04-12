# Threat NPS 最適化セッション記録 (2026-04-13)

feat/threat-2a ブランチでの NPS 最適化セッション。
Threat モデル (v92) で v87 に時間あたりで勝ち越す設定を目指した。

## 初期状態 (`7833c1ce`)

- v87 (L0=1536, no Threat) ~590K NPS
- v92 (L0=512, Threat profile 0) ~469K NPS
- **gap -21% (v87 有利)**

## 実施した施策

### ✅ 採用

#### 1. Threat leaper source fast path (`1fe24736`)

`append_changed_threat_indices` の Step 3 source loop で、source sq が
changed_bb 外かつ occupied 非依存駒種 (Pawn/Knight/Silver/Gold 系) の
場合を fast path 化。非 changed target は before/after で相殺されるので
列挙スキップ。

**計測**: NPS +0.20% (instructions -1.44%)、cycles は誤差範囲。
計算量削減は本質的だが cycles 反映は薄い。累積改善の前提として採用。

#### 2. LayerStacks cache の Stockfish 風 piece_list 差分化 (`72842680`)

`AccumulatorCacheLayerStacks` の cache entry を刷新。

- 旧: `active_indices: [u32; MAX_ACTIVE]` (sort 済)
- 新: `piece_list: [BonaPiece; 40]`

refresh 処理:
- 旧: `append_active_indices` → sorted u32 配列 → マージ差分
- 新: `piece_list` を直接 cache に渡し、40 slot を slot-wise 比較、
  変化 slot のみ `idx_fn` で feature index を算出して add/sub

**計測 (vs `7833c1ce`)**:

| 項目 | baseline | sfcache | Δ |
|---|---:|---:|---:|
| avg_nps | 454,125 | 490,171 | **+7.94%** |
| cycles/node | 9,650.6 | 8,941.7 | **-7.35%** |
| instructions/node | 19,111.9 | 17,422.9 | **-8.84%** |

**正しさ検証**: `go nodes 200000` で baseline/candidate 両者とも depth=15,
nodes=111185, score=cp 105, pv = `7g7f 4a3b 2g2f 8c8d ...` 完全一致。

**本質的な差**: Threat Finny Tables PoC (`59157c79`, -0.77%) が失敗したのは
sorted 対称差方式で cache entry を 8KB 拡大したため。本改修は 160 bytes → 80
bytes に縮小しつつ、sort を完全に除去 (fixed overhead ~320 ops/call 消失)。

### ❌ 不採用 (計測で効果なし、revert 済)

#### 3. `find_usable_accumulator` + `forward_update_incremental` 再評価

sfcache 上で cherry-pick (`d4b245ea`, `32439b00`) → 計測 → revert
(`9cc3dcbd`, `7eee31b6`)。

**計測**: NPS **-1.91%** (cycles +1.96%, instructions -0.03%)。sfcache で
HalfKA refresh が軽くなったため、forward_update の ancestor walk overhead
が Threat rebuild 節約分を上回る。

#### 4. update path の fused prev→curr fast path (`e855bfe5` → revert `31dfde71`)

`curr.copy_from_slice(prev)` + `try_apply_dirty_piece_fast` を、AVX-512/AVX2 で
`curr[i] = prev[i] - sub + add` の 1 回 SIMD loop に fuse。fast path 成功時は
8KB memcpy を排除。

**計測**: NPS **-0.31%** (cycles +0.33%, instructions -0.61%)。LTO=fat
production build では copy_from_slice と後続の SIMD loop が prefetch overlap
で十分効率化されており、手動 fuse のメリットが出ない。

## パターン認識: production build で効かないマイクロ最適化

fastpath, forward_update, fused_prev の 3 つは「instructions は削減できるが
cycles/CPI が相殺される」パターンを示した:

| PoC | instructions Δ | cycles Δ | NPS Δ |
|---|---:|---:|---:|
| leaper fastpath | -1.44% | -0.19% | +0.20% |
| forward_update | -1.35% | +0.83% | -0.91% |
| fused_prev | -0.61% | +0.33% | -0.31% |

**共通原因**: LTO=fat + cgu=1 production build が既に十分 aggressive で、
localized SIMD/branch 最適化の余地が小さい。**アルゴリズム的に仕事量を減らす
改修 (sfcache)** のみが NPS に効く。

## sfcache 後の perf profile (fresh)

profiling build + v92 complex-middle 20s で perf record:

| Children | Self | 関数 |
|---:|---:|---|
| 16.45% | 4.58% | search_node |
| 13.83% | **12.71%** | refresh_accumulator_with_cache |
| 7.51% | 7.28% | MovePicker::next_move (out of scope) |
| 7.21% | 5.70% | update_accumulator_with_cache |
| 6.49% | 6.46% | attackers_to_occ |
| 6.15% | 5.82% | append_changed_threat_indices |
| 5.06% | 4.71% | LayerStacksNetwork::evaluate |
| 4.32% | 3.86% | refresh_perspective_with_cache |
| 2.62% | 2.62% | threat_features::attacks_from_piece |

**refresh_accumulator_with_cache self 12.71% の内訳** (推定):
- HalfKA refresh (refresh_perspective_with_cache 4.32%): sfcache で軽量化済
- **Threat full rebuild ~9.5%**: `threat_acc.fill(0)` + `for_each_active_threat_index`
  + `add_threat_weights` ループ。これが次の最大ターゲット。

## v87 vs v92 直接計測 (sfcache 込、clean environment)

| 項目 | v87 (baseline) | v92 (candidate) | Δ |
|---|---:|---:|---:|
| avg_nps | 621,939 | 497,058 | **-20.08%** |
| cycles/node | 7,059.6 | 8,829.2 | +25.07% |
| instructions/node | 14,870.2 | 17,423.7 | **+17.17%** |
| CPI | 0.475 | 0.507 | +6.7% |

- **instructions/node +17.17%** が Threat の純粋計算コスト
- **CPI +6.7%** が Threat weight table (106MB) のメモリ圧迫
- sfcache は v87/v92 両方に効くため **相対 gap は 20% で変わらない**

## selfplay 実戦棋力 (sfcache 両側、byoyomi 1000ms)

### v87-400 vs v92-60 (v92 は session 初期の baseline checkpoint)

200 局 (100×2 入替)、`start_sfens_ply32.txt`、concurrency=7

- v87-400: 108W-89L-3D (54.0%), avg_nps **502K**, avg_depth 19.12
- v92-60:  89W-108L-3D (44.5%), avg_nps **413K**, avg_depth 18.95
- **Elo: +33 ±48 (v87 有利、CI は 0 を含み統計的に有意ではない)**

v92-60 は early checkpoint (training 途中)。NPS 18% 劣位でも Elo +33 程度
の差に収まっているのは Threat eval 品質が補償している証拠。

### v87-400 vs v92-160 (v92 最終 checkpoint)

200 局。v92 の training は 160 sb で停止済み (v92 実験 doc 参照)。

- v87-400: 110W-89L-1D (55.2%)
- v92-160: 89W (44.5%)
- **Elo: +37 ±48**

v92-60 の +33 とほぼ同じ。training sb 不釣り合い (v87-400 vs v92-160) の影響で
v87 有利。

### v87-160 vs v92-160 (apples-to-apples sb 揃え)

同じ training sb で比較した公平な計測。

- v87-160: 109W-91L-0D (54.5%), avg_depth 19.11
- v92-160: 91W (45.5%), avg_depth 18.88
- **Elo: +31 ±48**

v92 実験 doc の過去計測 (v92-140 vs v87-140 @byoyomi, 非 sfcache ビルド):
**-13 ±30 Elo** とほぼ一致 (CI [-13, +49] と [+19, +79] で十分重なる)。

**結論**: sfcache 導入後でも v92 は byoyomi で v87 に -13 〜 -31 Elo 程度
負けている。sfcache は絶対 NPS を上げたが相対 gap (v87/v92 NPS 比) は
変わらないため、playing strength の相対関係も変化しなかった。

## 最終状態 (`bc89a5c6`)

- v87 (L0=1536, no Threat): ~622K NPS
- v92 (L0=512, Threat): ~497K NPS
- **gap -20% (v87 有利、sfcache で 21% → 20% に微減)**

## 残っている最適化余地と次のステップ

### 推論側

#### A. Threat full rebuild 削減 (`refresh_accumulator_with_cache` の 9.5%)

現状 refresh path で `threat_acc.fill(0)` + 全 threat pair 列挙。
sfcache と同様の発想で **Threat も piece_list 差分で incremental update**
する案 (Threat Finny Tables v2)。

設計:
- AccCacheEntry に `threat_accumulation: [i16; L1]` を追加 (+1KB/entry)
- 新関数 `append_changed_threat_indices_from_piecelist_diff`:
  - cached_piece_list と current_piece_list を受け取り、差分 slot から
    affected source squares を抽出
  - 各 source について before/after threat pair を列挙して removed/added
- refresh_or_cache cache hit 時に threat 差分も apply

実装コスト: 300+ 行、correctness 検証困難。
期待効果: refresh path の Threat 9.5% のうち半分を削減 → **NPS +4-5%**
リスク: micro-optimization PoC と同じ CPI 悪化で効果相殺の可能性あり。

#### B. Threat バッチ prefetch

`add_threat_weights` を per-index 呼び出しから、バッファ collect → sorted
apply with next-row prefetch に変更。

期待効果: +1-3% (小粒)

### 学習側 (Task D)

v92 / v93 / v94 実験の既存結果:
- v92 (L0=512 profile 0): NPS 77% of v87, eval 品質 +43 Elo at depth 10
- v93 (L0=768 profile 1, 11% dims削減): NPS 76% of v87, eval -3 to +6 Elo vs v91
- v94 (L0=512 profile 10 cross-side, 55% dims削減): NPS +12-13% vs v92, eval -28 to -45 Elo ← **不採用**

v92 実験の結論: 「Threat テーブルアクセスのオーバーヘッドが支配的で L0 縮小の効果が微小」
v93 実験の結論: 「同種ペア除外 (profile 1) は eval 維持、NPS +3% の微改善」
v94 実験の結論: 「cross-side (profile 10) は eval 劣化が深刻、不採用」

**推奨された次の方向性** (v94 doc より):
1. **中間 dims の探索**: full (216,720) と cross-side (96,320) の中間、
   130,000〜160,000 dims で「何を残すか」を再設計
2. **same-side 情報の部分保持**: 特定の class 組合せだけ残す
3. **cache-friendly レイアウト化**: feature index の並び替え、cache line
   align、prefetch で miss 率改善 (eval 品質不変で NPS 狙い)

### 戦略的含意

v87 NPS への追いつきは **inference 単独では困難**。Threat 関連コスト
(instructions +17%, CPI +7%) を削るには学習側の dims 削減 + レイアウト改善が
本質的に必要。

本 session の selfplay 計測で v92-160 vs v87-160 は **-31 ±48 Elo** と、
sfcache 導入前の v92-140 vs v87-140 測定 (-13 ±30 Elo) と統計的に同水準。
sfcache は絶対 NPS を上げたが **v87 vs v92 の相対 gap は変わらず** (両方に
同程度効くため)、playing strength の相対関係は変化しなかった。

v92 を v87 超えに押し上げるには以下のいずれかが必要:

1. **v92 固有の追加 NPS 改善**: Threat full rebuild 削減 (Threat Finny Tables
   v2) など。推定 +4-5% NPS。ただし過去の micro-optimization PoC と同じ
   CPI 悪化で効果相殺の可能性あり
2. **学習側での dims 削減 + eval 品質維持** (Task D): 中間 dims 探索
   (130K-160K)、cache-friendly レイアウト化
3. **L0 を更に縮小** (例: 384): 学習側の追加実験

**最有望**: v93 (L0=768, profile 1) の継続強化 + sfcache 適用。v93 は
L0=768 で eval 容量が v92 より大きく、同時に profile 1 で Threat dims も 11%
削減。v91 実験 doc では v91/v93 共に NPS 75-77% 程度だが、eval 品質向上の
余地がある。

## 参照

- `docs/performance/threat_table_cpi_measurement_20260412.md` — 詳細計測ログ
- `docs/performance/threat_dimension_reduction_plan.md` — dims 削減計画
- `/tmp/perf_measure_20260413/*.json` — JSON 計測結果
- `runs/selfplay/20260413-035000-v87-vs-v92-sfcache/` — selfplay ログ

## コミット履歴 (セッション)

```
bc89a5c6 docs(perf): v87 vs v92 gap の構造分析を追記
28a79233 docs(perf): update path fused prev→curr PoC の計測結果を追記 (revert)
31dfde71 Revert "perf(nnue): update path の fused prev→curr fast path PoC"
e855bfe5 perf(nnue): update path の fused prev→curr fast path PoC
b5a7455e docs(perf): sfcache 上での forward_update 再評価結果を追記 (revert)
7eee31b6 Revert "perf(nnue): LayerStacks do_update! で forward_update_incremental を活用"
9cc3dcbd Revert "perf(nnue): forward_update_incremental で source_acc clone を削除"
32439b00 perf(nnue): forward_update_incremental で source_acc clone を削除
d4b245ea perf(nnue): LayerStacks do_update! で forward_update_incremental を活用
c2fd6034 docs(perf): Stockfish 風 cache 改修の計測結果を追記
72842680 perf(nnue): LayerStacks cache を Stockfish 風 piece_list 差分方式に置換 ★
1b594067 docs(perf): Threat leaper fast path PoC の計測結果を追記
1fe24736 perf(nnue): Threat changed indices の leaper source fast path PoC ★
```

★ = 採用された改修
