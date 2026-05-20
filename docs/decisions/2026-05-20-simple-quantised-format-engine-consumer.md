# ADR: Simple 量子化フォーマットの version とアーキ識別契約（エンジンを consumer とする）

- Date: 2026-05-20
- Scope: rshogi（エンジン）／ rshogi-nnue（`nnue-format` クレートの `SimpleWeights`）

---

## 背景 (Context)

rshogi-nnue #164 で bucket 無し 4 層 NNUE（Simple アーキ）と量子化フォーマット
`SimpleWeights`（`crates/nnue-format/src/simple_weights.rs`）が新設された。#165 は
この Simple モデルを学習し、**水匠5 と SPRT 対局**で棋力評価する。対局には探索
エンジンが必要なため、**rshogi エンジンが Simple モデルの consumer になる**ことが
確定している。

#164 は「推論エンジン互換」を非ゴールとしているが、これは「エンジンが Simple を
読まない」という意味ではない。後続（#165）でエンジンが読むことは確定事項。本 ADR
はその前提を明文化し、フォーマットの version と識別契約を確定する。

### 調査で判明した事実

1. **YaneuraOu upstream master の NNUE versioning**（`source/eval/nnue/`、
   `git show origin/master:` で確認、作業ツリー不触）:
   - `nnue_common.h`: `kVersion = 0x7AF32F16`（**全 NNUE ファイル共通の単一値**）。
     `evaluate_nnue.cpp` の `ReadHeader` が `version != kVersion` を `FileMismatch`
     で reject する。
   - `evaluate_nnue.h`: `kHashValue = FeatureTransformer::GetHashValue() ^
     Network::GetHashValue()`（計算値）がアーキ弁別子。
   - **version はアーキ弁別子ではない。** 形式世代スタンプであり値は 1 つ。
   - 層ハッシュ定数（`affine_transform.h:0xCC03DAE4` / `input_slice.h:0xEC42E90D`
     / `clipped_relu.h:0x538D24C7`）は `SimpleWeights::compute_fc_hash` が使う定数
     と一致 → トレーナーの `network_hash` は YaneuraOu `kHashValue` 機構の移植。
   - `clipped_relu.h` と `sqr_clipped_relu.h` は同一ハッシュ `0x538D24C7` →
     **YaneuraOu の hash は CReLU/SCReLU を区別しない**（hash は topology-only、
     活性化情報は arch 文字列にのみ存在）。YaneuraOu 自身は固定アーキビルドで
     runtime に活性化を識別する処理を持たない。

2. **bullet-shogi の bucket-less モデル**: `examples/shogi_simple.rs` 出力
   （`--output-format standard`）の v63 `quantised.bin` は version `0x7AF32F16`、
   arch 文字列は nnue-pytorch ネスト形式。**rshogi エンジンはこれを既に直接
   ロードできる**（v63 実験ドキュメントの評価コマンドが
   `EvalFile=...v63-800/quantised.bin` を `rshogi-usi` に投入し 2400 局を実施）。
   bullet-shogi の LayerStack モデル（v85）は version `0x7AF32F20`。

3. **rshogi エンジンの version 定数**: `NNUE_VERSION = 0x7AF32F16`（YaneuraOu と
   一致）／ `NNUE_VERSION_HALFKA = 0x7AF32F20`（nnue-pytorch 系）。

4. **トレーナー現状**: `SimpleWeights::NNUE_VERSION = 0x7AF32F20`。bucket-less
   アーキに nnue-pytorch/LayerStack 系列の version を付けており、YaneuraOu /
   bullet-shogi bucket-less の慣習（`0x7AF32F16`）と不一致。

---

## 決定 (Decision)

### D1. Simple フォーマットは YaneuraOu の versioning パターンに従う

- version magic はアーキ弁別子に**しない**。形式世代スタンプとして単一値を使う。
- アーキ識別は `network_hash`（= YaneuraOu `kHashValue` 機構、
  `compute_fc_hash ^ ft_hash`）+ arch 文字列で行う。
- 活性化（CReLU/SCReLU）は arch 文字列のトークン（`ClippedReLU`/`SqrClippedReLU`）
  に self-describe する。hash には含めない（`compute_fc_hash` 移植元の hash 設計
  と整合）。

### D2. `simple_weights::NNUE_VERSION` を `0x7AF32F20` → `0x7AF32F16` に変更する

- `0x7AF32F16` = YaneuraOu `kVersion` = bullet-shogi bucket-less（v63）=
  rshogi `NNUE_VERSION`。bucket-less アーキの正しい lineage。
- 現状の `0x7AF32F20` は nnue-pytorch/LayerStack 系列で、bucket-less アーキには
  不適切。
- 実モデルが 0 個の今が変更タイミング。`load` の version check・doc・テストを
  連動更新する。

### D3. TODO A（`network_hash` に活性化を XOR）は採用しない

- 活性化は arch 文字列に自己記述済みで、`load()` が `arch_identity` 文字列一致で
  活性化不一致を reject 済み（`load_rejects_activation_mismatch` テストが保証）。
- YaneuraOu 自身、`kHashValue` に活性化を含めない（CReLU/SCReLU 同ハッシュ）。
  `compute_fc_hash` はそれを忠実に移植している。
- XOR は情報の二重化であり、`compute_fc_hash` 移植元の hash 設計（topology-only）
  からの逸脱。

### D4. TODO B（`arch_feature_name` の曖昧さ解消）は採用しない

- `FeatureSet::canonical_name`（`halfka-hm-merged` 等）は既に flat な識別名。
- `arch_feature_name` は nnue-pytorch arch 文字列の互換トークンであり変更不可。
- エンジンは feature set を `canonical_name` / `feature_hash` で識別する。

### D5. エンジンは Simple モデルの consumer である

- #164 の「推論エンジン互換 非ゴール」=「エンジンの**既存 file-size 検出
  HalfKA 経路**とそのまま byte 互換にはしない」の意。「エンジンが読まない」ではない。
- #165 SPRT のためエンジンは Simple モデルをロードする。byte layout / arch 文字列 /
  hash は cross-repo の安定契約として扱う。
- エンジン側のアーキ識別は **rshogi オリジナルの仕様**（runtime に多 NNUE アーキを
  dispatch する shogi エンジンは rshogi のみで、reference 例なし）。3 チャネルの
  併用で行う:
  1. **topology**（feature_set + 次元）= weight セクションのファイルサイズ式で
     候補を絞り込む。
  2. **整合性 / 改竄検出**（補助）= offset 4 の `network_hash` を計算値と照合。
  3. **活性化**（CReLU / SCReLU）= arch 文字列の `ClippedReLU` / `SqrClippedReLU`
     トークンを parse。

### D6. `shogi-features` クレートを共有する（TODO G）

- `shogi-features` は純粋クレート（依存は `shogi-format` のみ）でエンジンから
  再利用可能。
- 共有により 5 feature set の indexing parity が定義上保証され、再実装による
  drift が消える。
- rshogi-nnue 側は「GPU 非依存維持」の確認のみ。

---

## 影響 (Consequences)

- version / hash / arch 文字列 すべて YaneuraOu パターンに統一。新規捏造値なし。
- version 共有でも hash 識別なら誤経路は起きない。
- `shogi-features` 共有で feature index の drift が消える。
- `0x7AF32F16` は engine の HalfKP loader と version 値を共有するため、Simple
  `.bin` を engine consumer 向けに配布するには arch 文字列ベースの dispatcher
  実装が前提となる（dispatcher 無しでロードすると silent corruption になりうる）。

---

## 検討した代替案 (Alternatives Considered)

- **A1: version を `0x7AF32F21`（新規値）にしてアーキ弁別子にする**（PR 170
  レビュー提案）。**却下。** YaneuraOu は version を弁別子にせず単一 `kVersion`
  + hash 弁別。`0x7AF32F21` は YaneuraOu master に存在しない捏造値であり、
  確立パターンに反する。version 共有による誤経路は D5 の hash 識別で解消する。
- **A2: `0x7AF32F20` を維持**。**却下。** bucket-less アーキに
  LayerStack/nnue-pytorch 系列 version を付けるのは lineage 不整合。
- **A3: `network_hash` に活性化を XOR**（メモ `20260520_simple_arch_
  engine_integration.md` の TODO A）。**却下。** D3 参照。

---

## 参照 (References)

- rshogi-nnue: `crates/nnue-format/src/simple_weights.rs`,
  `crates/shogi-features/src/feature_set.rs`,
  `docs/decisions/2026-05-20-simple-quantised-format-engine-consumer.md`
  （本 ADR の reasoning に追従した同名 ADR、PR #176 で 2026-05-20 landed）
- YaneuraOu master: `source/eval/nnue/nnue_common.h`（`kVersion`）,
  `evaluate_nnue.h`（`kHashValue`）, `evaluate_nnue.cpp`（`ReadHeader`）
- bullet-shogi: `examples/shogi_simple.rs`,
  `checkpoints/v63/v63-800/quantised.bin`,
  `docs/experiments/v63_halfka-hm_1024x2-8-64_crelu_dlsuisho15b_800sb.md`
- rshogi エンジン: `crates/rshogi-core/src/nnue/{activation,spec,network,constants}.rs`
- 関連メモ: `docs/experiments/20260520_simple_arch_engine_integration.md`
- 関連 Issue（背景のみ・本 ADR は独立タスク）: rshogi-nnue #164 / #165
