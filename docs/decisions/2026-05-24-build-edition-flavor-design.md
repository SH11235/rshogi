# ビルド設定の Edition 軸設計 (Flavor 軸は将来検討)

- **Status**: Proposed
- **Date**: 2026-05-24
- **設計レビュー**: local Codex (REQUEST CHANGES → 反映済) / local Claude (APPROVE WITH SUGGESTIONS → 反映済) / GitHub Codex bot + Claude bot (PR #733 で auto-review、Critical 1 + Major 2 + Minor 2 + 事実誤り 1 を全て反映)

## Context

rshogi の Cargo feature 設計は現状、network アーキテクチャの選択と最適化の有効化が
同じ feature 群で表現されており、build 用途別の組合せが組み手任せになっている。
具体的な問題:

- `default = ["search-no-pass-rules", "layerstacks-1536x16x32"]` で LayerStack 1
  サイズが暗黙固定。別サイズや HalfKX 系を使うには `--no-default-features` から
  組み直す必要があり、ユーザ視点で組合せが見通せない。
- `layerstack-only` feature が「network 経路の絞り込み」と「FT が HalfKA_hm 系
  であることの暗黙前提」を兼ねている。PR #719 で HalfKaMerged / HalfKaHmSplit の
  新 FT が追加された結果、`layerstack-only` 配下で gate 不整合 (Issue #728) が発生。
- 「全アーキ対応汎用 build」「HalfKX 系全部 build」「LayerStack 系全部 build」
  「個別 architecture 専用 (大会用) build」を user が選べる必要がある。
- YaneuraOu (以下 YO) 同等の "FOR_TOURNAMENT 相当" の用途分離もあれば嬉しいが、
  効果計測が前提のため本 ADR の Decision には含めない (後述)。

YO の build 設計 (`source/Makefile`) を参考にすると以下 3 軸の直交設計が見える:

| 軸 | YO の表現 | 値の例 |
|---|---|---|
| Edition (network) | `YANEURAOU_EDITION` 単一変数 | `YANEURAOU_ENGINE_NNUE` / `..._SFNNwoP1536_V2_32x32` / `KPPT` / `MATERIAL` |
| Flavor | `make` target | `normal` / `evallearn` / `tournament` (`-DFOR_TOURNAMENT`) / `pgo` |
| Target CPU | `TARGET_CPU` 変数 | `AVX512` / `AVX2` / `ZEN3` / `WASM` |

ただし YO は Edition mutex (1 binary = 1 architecture) で「universal build」を持たない。
rshogi では universal (= 全 arch を runtime dispatch する開発・debug 用) を残したい。

Cargo features は additive で mutex を表現しづらいため、feature 直接指定だけで
build target を表現させると組合せ爆発と "怪しい組合せ" が出る。

## Decision

### 軸構成: Edition 軸を本 ADR で確定、Flavor は将来検討

- **Edition** (本 ADR の主スコープ): network architecture と FT / 拡張 / size の
  構成を一意に決める軸。Cargo feature の preset bundle として表現。
- **Flavor**: build の用途 (default 開発用 / `tournament` 等) を分ける軸。**本 ADR
  では軸の存在のみを宣言し、`tournament` の具体内容は別 Issue で計測後に決定**する
  (CLAUDE.md「測定なしの最適化禁止」に従う、後述「Flavor 軸の扱い」)。
- **Target CPU は設計軸に含めない**: `.cargo/config.toml` の `target-cpu=native` で
  永続的に build マシン最適化を採用。配布等の例外ケースは ad-hoc に `RUSTFLAGS`
  上書きで 1 発対応 (build コマンドレベル、設計には含めない)。

**WHY**: Edition 軸だけでも 5 build target カテゴリ (後述) を表現できる。Flavor は
効果未検証の最適化軸 (YO `FOR_TOURNAMENT` の rshogi 版が実際に Elo を稼げるかは
未測定)、ADR Decision に組み込むのは時期尚早。3 軸目 (Target CPU) は実用上ローカル
1 環境向けで十分。

### 5 build target カテゴリと canonical Edition 名

| カテゴリ | 用途 | Edition 例 (canonical) | 想定 model / 既存 binary 例 |
|---|---|---|---|
| **universal** | smoke / debug、全 arch runtime dispatch | `edition-universal` | 全モデル (debug 用) |
| **HalfKX family** | 旧 NNUE 系統全部 dispatch | `edition-halfkx-any` (= `edition-halfkx` alias) | suisho5, HalfKa* 系全部 |
| **HalfKX specific** | HalfKX 単一 architecture 専用 build | `edition-halfkp-crelu` 等 | suisho5.bin 専用、tournament 用 |
| **LayerStack family** | LayerStack 全 FT / size / 拡張 dispatch | `edition-ls-any-any-any` (= `edition-ls` alias) | v100/v101/v102 系全部 |
| **LayerStack specific** | LayerStack 単一構成専用 build | `edition-ls-halfka_hm_merged-1536x16x32-psqt` 等 | 1 model 専用、tournament 用 |

「**カテゴリ**」と「**Edition**」の関係を明示すると: カテゴリは抽象分類で、
specific 系はカテゴリ内に多数の concrete Edition を生成しうる
(例: HalfKX specific には `edition-halfkp-crelu`, `edition-halfka_hm_merged-screlu`
 等が複数存在)。

specific 系は const generics と早期 LTO 除去で最大 perf、universal/family 系は
柔軟性優先で runtime dispatch を残す trade-off。

### Edition 命名ルール (canonical, user-facing)

各 architecture ごとに slot 順序と個数が定義される。HalfKX 系と LS 系で slot
構成が異なる:

| Architecture | 書式 | slot 数 |
|---|---|---|
| HalfKP | `edition-halfkp-{activation}` | 1 (activation) |
| HalfKa* (4 variant) | `edition-{arch}-{activation}` | 1 (activation) |
| HalfKX family | `edition-halfkx-{activation}` または alias `edition-halfkx` | 1 (activation) |
| LayerStack | `edition-ls-{ft}-{size}-{ext}` または alias `edition-ls` | 3 (ft / size / ext) |
| Universal | `edition-universal` | 0 |

#### slot 値の意味論 (全 slot 共通)

| slot 値 | 意味 |
|---|---|
| 具体値 (`1536x16x32`, `psqt`, `crelu`, `halfka_hm_merged` 等) | コンパイル時固定、その値以外はサポート外 |
| `none` (ext slot のみ) | 拡張ゼロを **明示** (省略との区別) |
| `any` | runtime dispatch、その軸の全候補を build に含める |

各 slot の許容値マトリクス:

| slot 種別 | 具体値 | `none` 可? | `any` 可? |
|---|---|---|---|
| activation (HalfKX) | `crelu` / `screlu` / `pairwise` | ✗ | ✓ |
| ft (LS / HalfKX 共通) | `halfkp` / `halfka_split` / `halfka_merged` / `halfka_hm_split` / `halfka_hm_merged` | ✗ | ✓ |

注: `ft` slot は LS / HalfKX どちらの family でも **5 variant 全てを命名規則
として認める**。ただし**実装の現状**: LayerStack network
(`network_layer_stacks.rs`) は HalfKaHmMerged (= 旧 `HalfKA_hm`) 専用に
hardcode されており、他 4 variant (`halfkp` / `halfka_split` / `halfka_merged`
/ `halfka_hm_split`) を LS で使うには LS の FT 部分を generic 化する別作業が
必要 (→ Issue SH11235/rshogi#734、本 ADR の Phase 1/2 のスコープ外)。Phase 1
の build.rs check では実装済 FT のみ enable (LS は `halfka_hm_merged` だけ通る)。
LS FT generic 化が完了したら他 4 variant も build に通せるようになる、という
forward-compatible な命名設計。
| size (LS) | `1536x16x32` / `1536x32x32` / `768x16x32` / `512x16x32` | ✗ | ✓ |
| ext (LS) | `psqt` / `threat` / `psqt_threat` | ✓ (`none` = 拡張なし固定) | ✓ |

`none` は ext slot 専用 (「拡張ゼロを明示」)。他 slot に `none` は不要 (slot 値が
必ず 1 つ以上存在するため)。

**slot 強制ルール**: slot 省略は alias `edition-halfkx` / `edition-ls` を除き
禁止。明示的に「全 dispatch」したいなら `any` を書く。

#### listed = required ON / unlisted = required OFF (ext slot のみ)

ext slot は `psqt` / `threat` の組合せを bit pattern として扱う:

| Edition 名 | PSQT | Threat |
|---|:---:|:---:|
| `edition-ls-halfka_hm_merged-1536x16x32-none` | OFF | OFF |
| `edition-ls-halfka_hm_merged-1536x16x32-psqt` | **ON** | OFF |
| `edition-ls-halfka_hm_merged-1536x16x32-threat` | OFF | **ON** |
| `edition-ls-halfka_hm_merged-1536x16x32-psqt_threat` | **ON** | **ON** |
| `edition-ls-halfka_hm_merged-1536x16x32-any` | 全 4 通り dispatch | 同左 |

複数拡張を ON にする場合は alphabetical underscore 結合 (`psqt_threat`、順序固定)。

#### Universal / family alias

- `edition-universal`: 全 architecture × 全構成 dispatch。slot ゼロの特別 alias。
  内部展開は後述「atomic feature の private 化と mode sentinel」セクションの
  Cargo.toml サンプル中、`edition-universal = [...]` を参照。
- `edition-halfkx` ≡ `edition-halfkx-any` (短縮 alias)。
- `edition-ls` ≡ `edition-ls-any-any-any` (短縮 alias)。

`edition-halfkx-{activation}` 形式 (例: `edition-halfkx-crelu`) は「HalfKX 全
architecture を runtime dispatch しつつ activation だけ固定」を表す family
build。**use case**: 手元のモデル全部 crelu と分かっているとき、architecture
側の柔軟性を残しつつ activation 分岐コードを除去して若干の perf gain。
ニッチだが ABI 上は valid な組合せなので命名規則として残す (build.rs check
は family mode で activation 複数 OR を許容、1 個だけ有効も valid)。

#### hyphen と underscore の使い分け

Cargo feature 名は `-` / `_` / `0-9` / `a-z` (ASCII) が許容される。本 ADR では
**slot 区切りは `-`、slot 内の複合語は `_`** を採用:

- slot 区切り例: `edition-ls-{ft}-{size}-{ext}` → `edition-ls-halfka_hm_merged-1536x16x32-psqt_threat`
- tatara canonical 名 (`halfka-hm-merged`) との対応: tatara 側は `-` 区切りで
  「halfka-hm-merged」表記、rshogi の Edition slot 内ではアンダースコアに変換した
  `halfka_hm_merged` 表記 (Cargo feature 階層内で slot 区切り `-` と衝突しないため)
- 両者は同一の network architecture を指す。**意味は完全に同一、表記が階層構造の
  都合で異なるだけ**。

#### 実験 ID は名前に出さない

`edition-ls-v100` / `edition-ls-v101` のような内部実験 ID を user-facing 名に
使わない。OSS の理解しやすさと長期保守性を優先。短縮したいユーザは local の
`.cargo/config.toml` alias で対応 (個人運用に閉じる):

```toml
# 個人の .cargo/config.toml (commit しない選択肢あり)
[alias]
build-mymodel = "build --profile production --no-default-features --features edition-ls-halfka_hm_merged-1536x16x32-psqt"
```

### atomic feature の private 化と mode sentinel

現状の atomic feature (`layerstack-only`, `layerstacks-1536x16x32`, `nnue-psqt`,
`nnue-threat`, `nnue-progress-diff` 等) は内部実装の詳細として残し、user は
preset edition feature 経由でのみ触る。直接指定は experimental / upstream
ビルド用途として可能 (後方互換)、ただし非推奨。

新しい atomic feature 軸 + **build mode sentinel** (build.rs が mode を判別する
ための marker feature):

```toml
# atomic (private、user は直接触らない想定)
ft-halfka_hm_merged = []
ft-halfka_hm_split  = []
ft-halfka_merged    = []
ft-halfka_split     = []
ft-halfkp           = []
ls-arch             = []
halfkx-arch         = []
ls-size-1536x16x32  = []
ls-size-1536x32x32  = []
ls-size-768x16x32   = []
ls-size-512x16x32   = []
ls-ext-psqt         = []
ls-ext-threat       = []
halfkx-activation-crelu   = []
halfkx-activation-screlu  = []
halfkx-activation-pairwise = []

# build mode sentinel (build.rs が読む、preset edition がセットする)
mode-universal = []  # 全 dispatch、build.rs check 緩和
mode-family    = []  # family dispatch、build.rs check 部分緩和
mode-specific  = []  # 単一構成 build、build.rs check 厳格

# preset edition (canonical, user-facing)
edition-universal = [
  "mode-universal",
  "ls-arch", "halfkx-arch",
  "ft-halfkp", "ft-halfka_split", "ft-halfka_merged", "ft-halfka_hm_split", "ft-halfka_hm_merged",
  "ls-size-1536x16x32", "ls-size-1536x32x32", "ls-size-768x16x32", "ls-size-512x16x32",
  "ls-ext-psqt", "ls-ext-threat",
  "halfkx-activation-crelu", "halfkx-activation-screlu", "halfkx-activation-pairwise",
  # nnue-progress-diff は universal では含めない (後述)
]
# 以下は preset 構成の例示 (実装時に Cargo.toml で完全列挙する):
edition-halfkx-any = [
  "mode-family", "halfkx-arch",
  "ft-halfkp", "ft-halfka_split", "ft-halfka_merged", "ft-halfka_hm_split", "ft-halfka_hm_merged",
  "halfkx-activation-crelu", "halfkx-activation-screlu", "halfkx-activation-pairwise",
]
edition-ls-any-any-any = [
  "mode-family", "ls-arch",
  "ft-halfka_hm_merged", "ft-halfka_hm_split", "ft-halfka_merged",
  "ls-size-1536x16x32", "ls-size-1536x32x32", "ls-size-768x16x32", "ls-size-512x16x32",
  "ls-ext-psqt", "ls-ext-threat",
]
# L0=1536 系 specific edition は **全て** nnue-progress-diff を含める
# (PSQT/Threat の有無、L1xL2 サイズによらず同じポリシー)
edition-ls-halfka_hm_merged-1536x16x32-none = [
  "mode-specific", "ls-arch", "ft-halfka_hm_merged", "ls-size-1536x16x32",
  "nnue-progress-diff",
]
edition-ls-halfka_hm_merged-1536x16x32-psqt = [
  "mode-specific", "ls-arch", "ft-halfka_hm_merged", "ls-size-1536x16x32", "ls-ext-psqt",
  "nnue-progress-diff",
]
edition-ls-halfka_hm_merged-1536x16x32-threat = [
  "mode-specific", "ls-arch", "ft-halfka_hm_merged", "ls-size-1536x16x32", "ls-ext-threat",
  "nnue-progress-diff",
]
edition-ls-halfka_hm_merged-1536x16x32-psqt_threat = [
  "mode-specific", "ls-arch", "ft-halfka_hm_merged", "ls-size-1536x16x32",
  "ls-ext-psqt", "ls-ext-threat",
  "nnue-progress-diff",
]
edition-ls-halfka_hm_merged-1536x32x32-none = [
  "mode-specific", "ls-arch", "ft-halfka_hm_merged", "ls-size-1536x32x32",
  "nnue-progress-diff",
]

# L0=768 / 512 系 specific edition は nnue-progress-diff を **含めない** (退行回避)
edition-ls-halfka_hm_merged-768x16x32-none = [
  "mode-specific", "ls-arch", "ft-halfka_hm_merged", "ls-size-768x16x32",
]
edition-ls-halfka_hm_merged-512x16x32-none = [
  "mode-specific", "ls-arch", "ft-halfka_hm_merged", "ls-size-512x16x32",
]

# HalfKX specific
edition-halfka_hm_merged-screlu = [
  "mode-specific", "halfkx-arch", "ft-halfka_hm_merged", "halfkx-activation-screlu",
]
edition-halfkp-crelu = [
  "mode-specific", "halfkx-arch", "ft-halfkp", "halfkx-activation-crelu",
]

# (他 preset edition は同様のパターンで列挙、L0 size と progress-diff の対応関係に注意)
```

`mode-*` は preset edition がセットする marker。user は触らない (preset 経由でのみ
有効化される)。build.rs の check で「universal/family なら複数 feature 有効を許容、
specific なら 1 個に制限」を判別する根拠になる。

### `nnue-progress-diff` の扱い (universal / family は除外)

`nnue-progress-diff` は L0=1536 で +3〜4% 改善するが L0=768 / 512 で -2〜6% 退行
する (memory `feature_nnue_progress_diff`)。よって:

- **specific Edition (L0=1536 系)**: preset に含める ✓
- **specific Edition (L0=768 / 512 系)**: preset に含めない ✗
- **family / universal**: 多 size を持つため preset に含めない ✗

build.rs check:
```
if nnue-progress-diff が有効:
    mode-specific が必須 AND ls-size-1536x16x32 か ls-size-1536x32x32 が有効
    それ以外は compile_error!
```

universal で大規模 perf 退行を起こさないことを構造的に保証する。

### `build.rs` 整合性チェック

mode sentinel と組合せて以下の不正組合せを fail-fast:

```
[mode が必ず ちょうど 1 個 有効]
  (mode-universal as u8) + (mode-family as u8) + (mode-specific as u8) != 1
  → compile_error! ("edition-* preset を 1 つだけ有効化してください")
  # 注: XOR (`a xor b xor c`) は「奇数個 true」を返すため 3 個全 true を弾けない。
  # 整数カウントで判定する。

[ls 系で必要 feature 揃ってる]
  ls-arch 有効 AND ls-size-* がゼロ
  → compile_error! ("ls-arch を有効化するには ls-size-* を 1 個以上必要")

[specific で size 重複なし]
  mode-specific 有効 AND ls-size-* が 2 個以上
  → compile_error! ("specific Edition では ls-size-* を 1 個だけ")

[specific で activation 重複なし]
  mode-specific 有効 AND halfkx-activation-* が 2 個以上
  → compile_error! ("specific Edition では halfkx-activation-* を 1 個だけ")

[specific で ft 重複なし]
  mode-specific 有効 AND ft-* が 2 個以上
  → compile_error! ("specific Edition では ft-* を 1 個だけ")

[progress-diff の size 依存]
  nnue-progress-diff 有効 AND (mode-specific が無効 OR (ls-size-1536x16x32 と ls-size-1536x32x32 のどちらも無効))
  → compile_error! ("nnue-progress-diff は L0=1536 系 specific Edition でのみ有効")

[universal は他 edition と排他]
  mode-universal 有効 AND (mode-family OR mode-specific)
  → compile_error! ("edition-universal は他 edition との同時指定不可")
```

family / universal mode では複数 feature 有効化を**正当**として扱い、specific
mode のみ厳格な 1-of-N チェックを適用する。

### Flavor 軸の扱い (本 ADR では宣言のみ、内容は別 Issue)

Edition と直交する Flavor 軸の存在を本 ADR で**宣言する**が、`tournament` の
具体内容と効果計測は**別 Issue / 後続 ADR で扱う**:

- Flavor 軸: Edition と独立な build option (例: `--features tournament`)
- 想定内容: YO `FOR_TOURNAMENT` 相当。`nnue-stats` / `search-stats` 強制 off、
  `info string` 拡張削減、開発用 assertion 削減 等
- 採否判断: 実装後の SPRT 等で Elo gain 計測、有意差なければ axis として非採用
- 採用時の命名: `--features tournament` で flavor on (preset edition と組合せ可)

**WHY Decision に含めない**: CLAUDE.md「測定なしの最適化禁止」。Flavor 軸を ADR
の Decision として固定すると、効果不明のまま実装が走るリスク。別 Issue で計測 →
有意差確認 → 別 ADR (本 ADR の supplement) で正式採用 のフローにする。

`evallearn` / `pgo` 等 YO の他 flavor は rshogi では別運用 (rshogi-nnue / tatara
側が学習担当、PGO は ad-hoc 対応) のため flavor 軸非対象。

### `xtask` / `justfile` で binary 名自動化

Edition の組合せに対する build を 1 コマンドで実行、出力 binary を
`engines/` 下に自動命名で配置:

```bash
cargo xtask build --edition ls-halfka_hm_merged-1536x16x32-psqt
# → engines/rshogi-usi-ls-halfka_hm_merged-1536x16x32-psqt を生成
```

命名規則: `engines/rshogi-usi-{edition}[-flavor-{flavor}]`
(flavor が default なら省略、`tournament` 等の場合は付加)

実装手段 (xtask crate / justfile / Makefile) はどれでも実用上同等。プロジェクト
既存ツーリングに合わせて Phase 2 で決定。

### 段階的導入と Issue #728 への依存

#### Issue #728 PR 依存 (blocking)

Issue #728 の PR 群が完了するまで本 ADR Implementation は着手不可:

- **PR 1** (parser 後方互換強化): vocabulary 揃え前段、新名 `halfka_hm_merged`
  等を parser が受理する状態にする
- **PR 2** (enum rename): `FeatureSet::HalfKaHmMerged` 等の Rust 名が確定
- **PR 3** (参照書き換え): 全コードベースで新名が一貫適用される

本 ADR Phase 1 着手前に **PR 3 まで merge 完了**が必要 (PR 4 のディレクトリ
rename は optional、Phase 2 と並走可)。

tatara Issue #242 (emit 文字列同期) は rshogi PR 1 merge 後に並行可能。本 ADR
Implementation とは独立。

#### Phase 1: atomic feature 直交化

スコープ:
- 既存 `layerstack-only` / `layerstacks-1536x16x32` / `nnue-psqt` / `nnue-threat`
  / `nnue-progress-diff` の rename / 分割
- 新 atomic feature 追加 (`ft-*`, `ls-arch`, `halfkx-arch`, `ls-size-*`,
  `ls-ext-*`, `halfkx-activation-*`, `mode-*` 等、概算 30 個)
- 既存 feature 直接指定 build の alias で互換維持 (`layerstack-only` ⇔ 新名)
- build.rs 整合性チェック導入

影響範囲見積もり (実 grep で確定すべき、概算):
- `Cargo.toml` `[features]` セクション: 現 ~15 行 → ~50 行
- CI workflow yml: feature 指定箇所を grep 確定 (おそらく 5-10 箇所)
- `scripts/build_*.sh`: 影響箇所を grep 確定
- 既存 `engines/` binary の rebuild は Phase 2 で対応 (Phase 1 では feature
  alias で旧 build script が引き続き動く)

#### 旧 feature 名 → 新 feature 名のマッピング (Phase 1 移行表)

Phase 1 着手時、既存 atomic feature を以下の対応で rename / 分割。**移行対象は
旧 5 系統 (`layerstack-only` / `layerstacks-*` / `nnue-psqt` / `nnue-threat` /
`nnue-progress-diff`)、加えて Phase 1 で新規追加する atomic feature 4 群** (FT
選択 / arch / activation / mode sentinel):

| 旧 feature 名 | 新 atomic feature 名 | 備考 |
|---|---|---|
| `layerstack-only` | `ls-arch` | alias で互換維持 (移行期間中) |
| `layerstacks-1536x16x32` | `ls-size-1536x16x32` | 同上 |
| `layerstacks-1536x32x32` | `ls-size-1536x32x32` | 同上 |
| `layerstacks-768x16x32` | `ls-size-768x16x32` | 同上 |
| `layerstacks-512x16x32` | `ls-size-512x16x32` | 同上 |
| `nnue-psqt` | `ls-ext-psqt` | 同上 |
| `nnue-threat` | `ls-ext-threat` | 同上 |
| `nnue-progress-diff` | `nnue-progress-diff` (名称継続) | 用途は build.rs check で L0=1536 specific のみに制限 |
| (新規) | `ft-halfkp` / `ft-halfka_split` / `ft-halfka_merged` / `ft-halfka_hm_split` / `ft-halfka_hm_merged` | Phase 1 新規追加、FT 選択軸 |
| (新規) | `halfkx-arch` | HalfKX family 経路有効化 |
| (新規) | `halfkx-activation-{crelu,screlu,pairwise}` | HalfKX activation 軸 |
| (新規) | `mode-universal` / `mode-family` / `mode-specific` | build.rs check 用 sentinel |

互換 alias 維持期間: Phase 2 merge までを移行期間とし、Phase 2 merge と同時に
旧 feature 名 alias を deprecate (warning)、その次の minor version で削除。

#### 移行対象外 (直交 feature として残置)

以下の既存 atomic feature は Edition 軸と直交する横断機能で、本 ADR の rename
スコープ対象外。Phase 1 着手時もそのまま残置:

- `debug` / `search-stats` / `nnue-stats` / `diagnostics` / `tt-trace` /
  `use-lazy-evaluate` / `move-features` / `deep`: 開発・デバッグ用横断 feature
- `search-no-pass-rules`: 探索ルール、Edition 非依存
- `wasm-threads`: WASM target 用、Edition 非依存
- `threat-profile-same-class` / `threat-profile-same-class-major-pawn`:
  `nnue-threat` (= `ls-ext-threat`) の profile 選択 sub-axis。Edition 軸の
  ext slot とは直交する補助 feature として残置 (将来 slot 化するかは別議論)

#### Phase 2: preset edition + binary naming 自動化

スコープ:
- `edition-*` preset features を Cargo.toml に追加
- `xtask` / `justfile` で binary 名自動化
- `engines/` 下命名規則の更新、`engines/README.md` 反映
- 主要 build target (universal, halfkx, ls の 3 family + tournament 用 specific
  数個) を最初に整備、残りは需要に応じて追加

WASM target 受入条件:
- `cargo xtask build --edition universal --target wasm32-...` が成功すること
- `.cargo/config.toml` の WASM 向け設定との整合確認 (現状 `target-cpu=native`
  は別 triple なので影響なし想定だが Phase 2 で smoke build 確認)
- WASM では `tournament` flavor は無意味と想定 (assertion 削減等は WASM では
  影響薄)、別 ADR で確認

#### 既存ユーザへの通知計画

Phase 1 着手前: GitHub Issue で feature rename 通知。Phase 2 完了時: CHANGELOG
に移行ガイド掲載 + engine README 更新。

## Rejected alternatives

### 案 A 単独: orthogonal atomic feature + Cargo alias

`.cargo/config.toml` の `[alias]` だけで preset を表現する案。**短所**: alias は
git tracked にしづらく、user 配布性が低い。binary 名の自動化機構もない。preset
を Cargo.toml feature として表現する方が canonical で再現性が高い。

### 案 B: cargo workspace 内で bin を分割 (wrapper crate per target)

`rshogi-usi-universal` / `rshogi-usi-ls-foo` 等の薄い wrapper crate を build
target ごとに作る。**短所**: workspace の見通し悪化、共通 logic の集約コストが
高い、Cargo の feature unification で同一 feature combo の異なる wrapper を同時
build すると cache 共有・workflow 並列性が落ちる可能性 (要計測だが現時点で根拠
レベルの懸念)。Phase 2 の xtask で十分機能を満たせる。

### 案 C: 接頭辞 `edition-tournament-*` / `edition-family-*` で specific/flexible を区別する命名

文脈接頭辞で slot 省略の意味を変える (`tournament-` なら listed = ON / unlisted
= OFF、`family-` なら unlisted = dispatch)。**短所**: 同じ文字列が違う slot
rule を持つ状態が発生して長期的に混乱の元。slot 強制 (`any` / `none` 明示) の
方が読み手にとって一意で安全。

### 案 D: 実験 ID (`edition-ls-v101` 等) を canonical 名に採用

短縮効果は大きいが、**OSS で意味不明な番号が露出するうえ、将来の自分が忘れる**。
実験 ID は git tracked な experiment doc 内で管理し、user-facing 名には出さない。
個人運用で短縮したい場合は local alias で対応。

### 案 E: Edition `_merged` `_split` を slot 化する命名

`edition-halfka-{factorization}` のように merged/split を slot として外出し。
**短所**: HalfKa の 4 variant は実装上独立の concrete architecture (`FeatureSet`
enum の 4 variant) で、slot 軸というより 2x2 grid の固有名。industry 名と一致させた
leaf 命名のほうが自然 (`halfka_hm_merged` 等)。Issue #728 の整理結果と整合。

### 案 F: 配布対応の 3 軸目 (Target CPU 軸)

YO 同等の `target-cpu-{avx2,avx512,wasm}` を設計軸として組み込む案。**短所**:
rshogi は実用上ローカル 1 環境向けで `target-cpu=native` で十分、配布要件が
発生したときに 1 発上書き (`RUSTFLAGS=-Ctarget-cpu=avx512 cargo build ...`) で
対応できるため、設計軸として常設する価値がない。matrix が膨らみメンテコストに
見合わない。

### 案 G: cargo-make / cargo-script / proc-macro 等の追加ツーリングで preset 表現

`cargo-make` の Makefile.toml や proc-macro による Edition 展開も選択肢として
あるが、**短所**: 依存ツールを追加すると初見コントリビュータの barrier が
上がる。Cargo features + xtask (xtask は cargo workspace 内の通常の bin crate)
だけで完結する設計のほうが standard で学習コストが低い。

## Consequences

### Positive

- user は **Edition を 1 個選ぶ**だけで build 構成が決まる。組合せ爆発が API
  レベルで消える。
- atomic feature 直接指定の experimental 用途も後方互換で残せる。
- binary 命名が自動化され、`engines/` 下の管理が機械的になる。
- universal / family / specific の 3 段階で perf vs 柔軟性の trade-off を選べる。
- build.rs 整合性チェックで不正組合せ (size 重複 / progress-diff の L0 不一致 等)
  を fail-fast、実行時バグの予防。
- 新規 architecture 追加時の手順が明確化 (atomic feature 追加 → 関連 preset
  edition 追加 → universal にも組み込む)。

### Negative / トレードオフ

- atomic feature 数が増える (現状 ~15 個 → 30 個程度想定)。命名規則と build.rs
  整合性チェックで管理コストを抑える。
- preset edition feature が多くなると Cargo.toml の `[features]` セクションが
  数十行になる。block 単位でコメントを入れて読み手の負担を減らす。
- Phase 1 で既存 feature 名を rename / 分割するため、CI / docs / spsa 関連
  ツールへの波及。互換 alias を移行期間 (Phase 2 merge 完了まで) 維持。
- `target-cpu=native` 永続採用は 5950X (Zen3) では AVX2 まで、将来 AVX512
  マシンで build した場合は AVX512 命令を含むため**ビルドマシン以下の CPU で
  build 成果物が動かなくなる** (現状の運用ポリシー上問題なし、配布時のみ ad-hoc
  対応)。

### 依存と関連

- **事前依存 (blocking)**: Issue #728 PR 1〜3 merge 完了
  - Edition 命名で使う `halfka_hm_merged` / `halfka_split` の vocabulary が
    rshogi 全コードベースで固定される
- **関連 (並列可)**:
  - tatara Issue #242 (emit 文字列同期): rshogi PR 1 merge 後に並行
  - tatara ADR `2026-05-19-nnue-feature-set-two-axis-model.md`: feature-set 2 軸
    モデル設計、本 ADR の命名 vocabulary と意味的に一致 (表記は階層構造の都合で
    underscore 変換)
- **本 ADR の範囲外** (別 ADR / Issue で扱う):
  - `tournament` flavor の具体内容と効果計測 (別 Issue で実装 → 計測 → 別 ADR
    で正式採用判断)
  - `xtask` 実装の詳細選定 (justfile / xtask crate / Makefile のいずれか)
  - `engines/` 下の命名 prefix convention の最終確定 (Phase 2 着手時)
  - WASM target との完全整合確認 (Phase 2 受入条件で smoke 確認)
  - **LayerStack の FT generic 化**: 現状 LS は HalfKaHmMerged 専用 hardcode、
    他 4 variant (`halfkp` / `halfka_split` / `halfka_merged` /
    `halfka_hm_split`) を LS で使えるようにするには `network_layer_stacks.rs`
    の refactor が必要。本 ADR は命名規則として 5 variant 全許容と決めるが、
    実装は別 Issue (→ Issue SH11235/rshogi#734) で進める

## 参考

- Issue SH11235/rshogi#728: `FeatureSet` enum rename + arch parser 整理 (本 ADR
  の事前依存)
- Issue SH11235/tatara#242: arch_feature_name 同期 (関連、並列可)
- tatara ADR `docs/decisions/2026-05-19-nnue-feature-set-two-axis-model.md`:
  feature-set 2 軸モデルと公開 5 cell 設計
- 既存 rshogi ADR `docs/decisions/2026-05-20-simple-quantised-format-engine-consumer.md`:
  Simple 量子化フォーマット (フォーマット側の参考)
- YO Makefile (`/mnt/nvme1/development/YaneuraOu/source/Makefile`): build 設計
  の参考元 (`YANEURAOU_EDITION` / `make tournament` 周り)
