# ADR: Simple 量子化フォーマットの version とアーキ識別契約（エンジンを consumer とする）

- Date: 2026-05-20
- Scope: rshogi（エンジン）／ rshogi-nnue（`nnue-format` クレートの `SimpleWeights`）
- 個別タスク扱い: 本 ADR は rshogi-nnue #164 とは**独立した個別タスク**として扱う。
  #164 のスレッドにはコメントしない。#164/#165 は背景として参照するのみ。
- 既存レビューとの関係: 本 ADR の D2 は、PR 170 のレビューが提案した
  `version = 0x7AF32F21` を**覆す**（後述 A1）。レビューは YaneuraOu の versioning
  実装を参照せずに下した判断であり、本 ADR は YaneuraOu master の実証で差し替える。
- rshogi-nnue 側追従: D2 / D3 / D4 / D5 / D6 は rshogi-nnue 側で同名 ADR
  (`docs/decisions/2026-05-20-simple-quantised-format-engine-consumer.md`) と
  `simple_weights::NNUE_VERSION = 0x7AF32F16` 変更として 2026-05-20 に landed
  済 (本 ADR の reasoning に追従した形)。本 ADR 確定後の追加変更は engine 側
  作業として実施。

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
     **YaneuraOu の hash は CReLU/SCReLU を区別しない**。活性化は arch 文字列で識別。

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
- 活性化（CReLU/SCReLU）は arch 文字列トークン（`ClippedReLU`/`SqrClippedReLU`）で
  識別する。hash には含めない（YaneuraOu 準拠）。

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
- XOR は情報の二重化であり、フォーマットが依拠する YaneuraOu パターンからの逸脱。

### D4. TODO B（`arch_feature_name` の曖昧さ解消）は採用しない

- `FeatureSet::canonical_name`（`halfka-hm-merged` 等）は既に flat な識別名。
- `arch_feature_name` は nnue-pytorch arch 文字列の互換トークンであり変更不可。
- エンジンは feature set を `canonical_name` / `feature_hash` で識別する。

### D5. エンジンは Simple モデルの consumer である

- #164 の「推論エンジン互換 非ゴール」=「エンジンの**既存 file-size 検出
  HalfKA 経路**とそのまま byte 互換にはしない」の意。「エンジンが読まない」ではない。
- #165 SPRT のためエンジンは Simple モデルをロードする。byte layout / arch 文字列 /
  hash は cross-repo の安定契約として扱う。
- エンジンはアーキを **hash で識別**する（version でも file size でもなく）。
  これは YaneuraOu `ReadHeader` パターン準拠であり、version 共有時の誤経路を
  構造的に排除する。

### D6. `shogi-features` クレートを共有する（TODO G）

- `shogi-features` は純粋クレート（依存は `shogi-format` のみ）でエンジンから
  再利用可能。
- 共有により 5 feature set の indexing parity が定義上保証され、再実装による
  drift が消える。
- rshogi-nnue 側は「GPU 非依存維持」の確認のみ。

---

## 影響 (Consequences)

良い面:
- version / hash / arch 文字列 すべて YaneuraOu パターンに統一。新規捏造値なし。
- version 共有でも hash 識別なら誤経路は起きない。
- `shogi-features` 共有で feature index の drift が消える。

コスト・残作業:
- トレーナー（rshogi-nnue）: `NNUE_VERSION` 変更 + round-trip 検証は **済**
  （本 ADR の reasoning に追従して同名 ADR と `simple_weights::NNUE_VERSION =
  0x7AF32F16` 化、quantised/raw round-trip smoke 検証込みで 2026-05-20 landed）。
- エンジン（rshogi、**別タスク**）: hash ベースのアーキ識別実装、Simple loader、
  feature index の bit 一致検証、未実装 feature set 2 種
  （`halfka-merged` / `halfka-hm-split`）。
- byte レイアウト比較（`SimpleWeights` 出力 vs bullet-shogi
  `--output-format standard`）未実施 → エンジン側スコープの精密化に必要。
- 配布 gate: rshogi-nnue 側 ADR にも明記されているとおり、Simple `.bin` を
  エンジン consumer 向けに publish するのは、本 ADR D5 の hash 駆動 dispatcher
  が engine 側で landed したあと。それまでは rshogi-nnue 内テスト fixture と
  内部検証用途に限定する（version `0x7AF32F16` が現状 HalfKP loader と一致する
  ため、dispatcher を直さないと silent corruption になる）。

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

## 未解決 (Open Questions)

- `SimpleWeights` の出力が bullet-shogi `--output-format standard`（v63、エンジン
  が既にロード可能）と byte 一致するか。一致すればエンジン改修が大幅に縮小する。
  → byte レイアウト比較を別途実施。
- エンジン側の Simple 対応（hash 識別 / loader / bit 一致検証 / 未実装
  feature set）は本 ADR とは別タスクで扱う。

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
