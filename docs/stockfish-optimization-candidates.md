# Stockfish 最適化施策の rshogi 導入候補

調査日: 2026-03-28
対象: Stockfish リポジトリ (2024-09 ~ 2026-03 の約 709 コミット)
前提: rshogi の LayerStack アーキテクチャおよび探索全体に関係するもの

---

## この文書の役割

- 本文書は、今後試す施策候補の **台帳と優先順位** をまとめる
- 実測ログや失敗施策の詳細は [`docs/performance/nps_benchmark_layerstack.md`](/mnt/nvme1/development/rshogi/docs/performance/nps_benchmark_layerstack.md) に残す
- 個別の深掘り結果は補助文書へ分離し、本文書には状態だけを反映する

## 2026-03-28 時点の整理

既に一度回した探索変更については、候補台帳上の状態を次で固定する。

- `CMHC`: native 条件を揃えた再計測で `-11 ±22 Elo`。**不採用**
- `IIR PV 例外`: 別マシン 1000 局ではほぼ互角。**中立**
- `NMP improving bonus`: 別マシン評価でも有意差なし。**中立**

したがって、次に優先するのは tree-changing の小粒変更ではなく、
`perf` 上位ホットスポットに直結する tree-safe 寄りの施策である。

## A. 探索系（実装コスト低〜中）

### 1. Counter-Move History Continuity (CMHC)

- **SF commit**: `8b6d8def` (2026-03-07)
- **rshogi 状態**: 試行済み、**不採用**
- **実装コスト**: 小

Continuation history の更新量を、スタック上の過去エントリの正負カウントに基づいて動的にスケーリングする。

```
// Stockfish: CMHCMultipliers = {96, 100, 100, 100, 115, 118, 129}
// positiveCount = contHist[0..6] で正の数をカウント
// bonus *= CMHCMultipliers[positiveCount] / 100
```

一貫して良い手にはより大きな更新、不安定な手には抑制的な更新を適用。
rshogi は現在 `CONTINUATION_HISTORY_WEIGHTS` の固定重みで更新しているため、CMHC による動的スケーリングを追加する形で導入可能。STC/LTC で有効性確認済み。

現在の判断:

- hidden `.cargo/config.toml` による `target-cpu=native` 不一致を除去した再計測では、
  `before_cmhc_native 508W - after_cmhc_native 477W - 15D`, Elo `-11 ±22`
- raw NPS も `search_only_ab` で `+0.14%` に留まり、初回の大差は build 条件不一致が主因
- 詳細ログは [`docs/performance/cmhc_env_compare_20260328.md`](/mnt/nvme1/development/rshogi/docs/performance/cmhc_env_compare_20260328.md)

### 2. PV ライン上での IIR 無効化（followPV 追跡）

- **SF commit**: `e20ef7ed` (2026-03-18)
- **rshogi 状態**: 実装済み、**中立**
- **実装コスト**: 小

前回イテレーションの PV を記録し、現在の探索で PV ライン上のノードでは IIR をスキップ。PV の安定性が向上し、特に詰み探索で改善。

```
// Stockfish: if (!ss->followPV && !allNode && depth >= 6 && ...)
// rshogi 現在: if !in_check && !all_node && depth >= 6 && ...（PV例外なし）
```

現在の判断:

- 別マシンの `byoyomi 1000 / 1000局` ではほぼ互角
- 現時点では「revert する根拠はないが、強化とも言えない」
- 詳細ログは [`docs/performance/nps_benchmark_layerstack.md`](/mnt/nvme1/development/rshogi/docs/performance/nps_benchmark_layerstack.md)

### 3. NMP の improving 連動強化

- **SF commit**: `0571e4e3` (2026-02-25)
- **rshogi 状態**: 実装済み、**中立**
- **実装コスト**: 小

NMP の閾値に `improving` を組み込み、improving 時により積極的に NMP を適用。

```
// Stockfish: beta - 17*depth - 50*improving + 359
// (従来: beta - 17*depth + 359)
```

現在の判断:

- 別マシン評価でも有意差は出ていない
- 現状は「残してよいが、棋力向上施策としては数えない」
- 詳細ログは [`docs/performance/nps_benchmark_layerstack.md`](/mnt/nvme1/development/rshogi/docs/performance/nps_benchmark_layerstack.md)

---

## B. NNUE 系（LayerStack 関連）

### 4. Double-inc アキュムレータ更新（駒取り最適化）

- **SF 実装箇所**: `nnue_accumulator.cpp:238-247`
- **rshogi 状態**: trial 実装あり（2026-03-28）
- **実装コスト**: 中

連続する2手が駒取りを構成する場合（手→駒除去）、2つの差分更新を1パスで融合処理。Feature Transformer のホットパスで NPS 改善が見込める。

rshogi は現在 `forward_update_incremental()` で各手を個別処理しているため、キャプチャ時の融合処理を追加可能。

2026-03-28 trial:

- LayerStacks の `DirtyPiece` fast path を追加
- representative 4 局面の `search_only_ab` で `NPS +1.53%`
- `cycles / node -1.08%`
- `instructions / node -1.73%`
- representative 4 局面 `go depth 20` で `全depth完全一致 4/4`
- `cargo fmt && cargo clippy --fix --allow-dirty --tests && cargo test` 通過
- 現時点では採用してよい

### ~~5. Dual Network（Big/Small）による Lazy Eval~~ → 見送り

- **SF 実装**: `evaluate.cpp:49-73`, `nnue_architecture.h`
- **rshogi 状態**: 未実装
- **実装コスト**: 大（推論コード二重化 + Small ネット学習）
- **判定**: **将棋では費用対効果が低いため見送り**

#### Stockfish での仕組み

Stockfish は SFNNv13 で2つのネットワークを持つ:
- **Big**: 1024→31→32→1（高精度）
- **Small**: 128→15→32→1（高速、Big の約 1/8 の計算量）

`|simple_eval()| > 962` で Small を使い、結果が曖昧（`|nnue| < 277`）なら Big に昇格。駒得が大きい局面は軽量ネットで代替し、浮いた計算時間を探索深度に回す戦略。

#### 将棋で見送る理由

1. **大差局面で評価精度を上げても下げても対局結果に影響しない**: 上位AI同士の対局では評価値 2000〜3000 の差がついた時点で勝敗は確定的。Small の精度が低くても Big と同じ手を選ぶ
2. **大差局面の出現率が低い**: チェスでは終盤にピース交換が進み「Q vs R」のような大差だが長い局面が頻出する。将棋は投了が早く、大差局面での思考時間自体が短い。Small が使われる局面の比率が低ければ NPS 改善の恩恵は小さい
3. **閾値のジレンマ**: 閾値を下げて Small の使用率を上げると、将棋では持ち駒打ち（drop）により逆転含みの局面を Small に回すリスクがある。閾値を高くすると使用率が低すぎて効果が出ない
4. **実装コストに見合わない**: 推論コードの二重化 + Small ネット学習の工数に対し、上記の理由で得られる Elo 改善が限定的

#### 参考: Stockfish での詳細

<details>
<summary>アーキテクチャ・学習方法の詳細（折りたたみ）</summary>

##### ランタイム切り替えロジック

```
局面評価:
├─ simple_eval() = 駒の損得（高速な駒得計算）
├─ |simple_eval()| > 962 の場合:
│   ├─ Small ネットで評価（高速）
│   ├─ 結果が曖昧（|nnue| < 277）なら:
│   │   └─ Big ネットで再評価（精度優先）
│   └─ そのまま使用
└─ それ以外:
    └─ Big ネットで評価（精度優先）
```

##### アーキテクチャ詳細

- Big / Small でアキュムレータは完全に別（共有不可）
- Feature Transformer も別インスタンス（Stockfish では Big のみ Threat 特徴を含む）
- メモリ増加: Small 追加で約 5MB（Big の 38MB に対し 13.8% 増）
- コードは C++ テンプレートで共通化

##### 学習方法

Stockfish の Small ネットは蒸留ではなくスクラッチ学習:

- `nnue-pytorch` の L1-128 ブランチで 500 epoch 設定、399 epoch で採用
- `lambda=1.0`（教師データのみ）
- 学習データのフィルタリングが鍵:
  - 第1版: `|simple_eval| > 1000` の局面のみ
  - 第2版: 駒数分布の偏りを是正（3駒除外、駒数別の閾値設定）
- Small ネットが実際に使われる局面だけを学習データにしている
- Big ネットの学習パイプラインがあれば、Small の追加学習は比較的容易

</details>

### 6. AVX512-ICL アキュムレータリフレッシュキャッシュ高速化

- **SF commit**: `8b499683` (2026-03-18)
- **rshogi 状態**: Finny Tables はあるが ICL 特化最適化は未確認
- **実装コスト**: 中

`update_accumulator_refresh_cache` の ICL 専用パスで +0.44% の速度改善。ICL 固有の SIMD 最適化（`_mm512_maskz_compress` 等）の追加余地がある。

---

## C. rshogi 固有の次候補（perf 起点）

Stockfish 由来ではないが、直近の `perf` と実験ログから優先度が高いもの。

### 7. `Position::attackers_to_occ()` / SEE / legality hot path

- **rshogi 状態**: 未着手
- **実装コスト**: 中

`Position::attackers_to_occ()` は直近 `perf` でも約 5% 前後を占めており、
合法手判定・SEE・`pseudo_legal` と一体で探索コストへ効いている。
NNUE 差だけではなく探索本体差を詰める候補として有力。

### 8. `MovePicker::next_move()` の残差削減

- **rshogi 状態**: 一部改善済み、残差あり
- **実装コスト**: 小〜中

`ExtMoveBuffer` 除去や score/sort 軽量化は採用済みだが、
`select_*` ループや stage dispatch にはまだ改善余地がある。

### 9. `LayerStackBucket::propagate()` の explicit / aligned 再設計

- **rshogi 状態**: generic helper 合成は失敗済み
- **実装コスト**: 中〜大

`perf` 上の最大ホットスポットだが、単純な helper 合成では退行した。
再挑戦するなら YO 寄りの explicit kernel と aligned buffer 前提で設計し直す必要がある。

---

## D. 検討・却下済み

### Sparse Input Affine Transform（find_nnz）

再計測済み (2026-03-29)。ブランチ `perf/l1-sparse`、詳細は [`docs/performance/l1_sparse_input_optimization.md`](/mnt/nvme1/development/rshogi/docs/performance/l1_sparse_input_optimization.md)。

- chunk ゼロ率 30% (v82-300 モデル実測)
- find_nnz + lookup table 方式: NPS **-1.31%**, instructions **-5.34%**
- perf stat で IPC 劣化の原因を特定: load queue stall +24%, store queue stall +69%
- 30% ゼロ率 + num_regs=2 では損益分岐に届かない
- **再評価条件**: ゼロ率 50%+、OUTPUT_DIM 増 (num_regs 4+)、INPUT_DIM 増

---

## 推奨優先順位

| 優先度 | 施策 | 実装コスト | 現在の判断 |
|--------|------|-----------|------------|
| **1** | Double-inc FT 更新 | 中 | 採用 |
| **2** | `attackers_to_occ()` / SEE / legality | 中 | 有力 |
| **3** | `MovePicker::next_move()` 残差 | 小〜中 | 有力 |
| **4** | AVX512-ICL FT cache | 中 | このマシンでは有力 |
| **5** | `LayerStackBucket::propagate()` 再設計 | 中〜大 | 工数大だが本命 |
| **6** | IIR PV 例外 | 小 | 中立、keep |
| **7** | NMP improving 連動 | 小 | 中立、keep |
| ~~8~~ | ~~CMHC~~ | ~~小~~ | ~~不採用~~ |
| ~~9~~ | ~~Dual Network~~ | ~~大~~ | ~~見送り~~ |

次の実装は、まず `Double-inc FT 更新` から入る。
これは tree-safe 寄りで評価がしやすく、現行ホットスポット
`update_accumulator_with_cache` / refresh 系に直結するため。
