# ビルドガイド

rshogi-usi バイナリの build 手順と、複数 NNUE architecture binary を運用するための
`cargo xtask` workflow をまとめる。設計の根拠 (なぜ preset edition を導入したか) は
ADR [`docs/decisions/2026-05-24-build-edition-flavor-design.md`][adr] を参照。

[adr]: ./decisions/2026-05-24-build-edition-flavor-design.md

## 目次

- [前提環境](#前提環境)
- [典型ワークフロー](#典型ワークフロー)
- [preset edition の選び方](#preset-edition-の選び方)
- [cargo xtask reference](#cargo-xtask-reference)
- [build manifest](#build-manifest)
- [build profile の使い分け](#build-profile-の使い分け)
- [既存 build scripts との関係](#既存-build-scripts-との関係)

## 前提環境

- Rust toolchain (stable、`rust-toolchain.toml` の指定に従う)
- Linux / macOS / Windows (x86_64 + AVX2 を想定、`.cargo/config.toml` で
  `target-cpu=native` を永続採用)
- 並列探索を活かす場合は AVX2 以上の CPU
- NNUE モデルは別途用意 (起動時 `setoption name EvalFile value <path>` で指定)
- **LayerStack (LS) 系 preset** (`edition-layerstacks*` および `edition-universal`) を動かす
  場合、bucket routing を明示する。`progresskpabs` モデルでは次の 3 項目が必須:
  `setoption name LS_BUCKET_MODE value progresskpabs`、
  `setoption name LS_PROGRESS_BUCKETS value <学習時の bucket 数>`、
  `setoption name LS_PROGRESS_COEFF value <progress.bin path>`。
  両玉の相対段で 9 bucket を選ぶモデルでは
  `setoption name LS_BUCKET_MODE value kingrank9` のみを指定し、
  `LS_PROGRESS_BUCKETS=0`、`LS_PROGRESS_COEFF` 未指定にする。
  `nnue-progress-diff` feature の有無に関わらず、`progresskpabs` を使う LS 系経路では
  bucket 数と progress.bin の指定が必須。HalfKX 単一 preset
  (`edition-halfkp-*` /
  `edition-halfka_hm_merged-*` 等) は LS 経路を通らないので不要。
  現状 progress.bin は `eval/` 配下に同梱されていないため、学習側
  (bullet-shogi 等) で生成したものを参照する運用。

## 典型ワークフロー

### 開発・デバッグ build (1 binary)

```bash
# default は edition-universal (全 architecture runtime dispatch)
cargo build --release

# 単体テスト
cargo test
```

`cargo build` は `target/release/rshogi-usi` を生成する。これは作業領域なので
別 preset を rebuild すると上書きされる点に注意。

### 複数 architecture binary を残したい場合 (selfplay / SPRT / tournament)

`cargo xtask` を使うと preset ごとに別名で `engines/` 下に配置できる:

```bash
# 利用可能な preset を確認
cargo xtask list-editions

# 1 preset を build (engines/rshogi-usi-<preset> に配置)
cargo xtask build --edition layerstacks-halfka_hm_merged-1536x16x32-psqt

# 複数 preset を順次 build (カンマ区切り or --edition 複数回)
cargo xtask build --edition layerstacks-halfka_hm_merged-1536x16x32-psqt,layerstacks-halfka_hm_merged-1536x16x32-threat
cargo xtask build --edition X --edition Y

# 全 preset を build (現状 24 件)
cargo xtask build --all-presets

# Edition 軸と直交する opt-in feature を追加 (engines/rshogi-usi-<preset>+mimalloc に配置)
cargo xtask build --edition layerstacks-halfka_hm_merged-1536x16x32-none --features mimalloc

# engines/ 配下の binary 一覧 + manifest を表表示
cargo xtask list-binaries
```

build 後、各 binary の横に `<binary>.meta.toml` が書き出される (commit hash / profile /
built_at / rustc 等を記録、後追い可能)。

## preset edition の選び方

preset edition は network architecture + LayerStack 構成 + activation を一意に決める
build target で、5 カテゴリ × 複数 concrete preset の構造を持つ。

| カテゴリ | 用途 | preset 名の例 | 含まれる arch |
|---|---|---|---|
| **universal** | アプリ / 開発、header 駆動の動的推論 | `edition-universal` | HalfKX / LS、全 5 FT、任意次元 |
| **HalfKX family** | 旧 NNUE 系統 (HalfKP / HalfKa\*) を dispatch | `edition-halfkx`, `edition-halfkx-any` | HalfKX 全 variant |
| **HalfKX specific** | HalfKX 単一 architecture 専用 | `edition-halfkp-crelu`, `edition-halfka_hm_merged-screlu` | 1 architecture 固定 |
| **LayerStack family** | LS 全 FT / size / ext を dispatch | `edition-layerstacks`, `edition-layerstacks-any-any-any` | LS 全構成 |
| **LayerStack specific** | LS 単一構成専用、tournament 用 | `edition-layerstacks-halfka_hm_merged-1536x16x32-psqt` 等 | 1 構成固定 |

### どれを選ぶか (典型ケース)

| 状況 | 選ぶ preset | 理由 |
|---|---|---|
| `cargo build` で困っていない | `edition-universal` (= default) | runtime dispatch で何でも読める |
| 同じ NNUE モデルで SPRT / selfplay を回したい | specific preset (例 `edition-layerstacks-halfka_hm_merged-1536x16x32-psqt`) | dispatch 除去で最大 perf、再現性高い |
| HalfKX 系モデル数本を切替えながら触りたい | `edition-halfkx-any` | activation 含む全 HalfKX を 1 binary でカバー |
| LS 系を size 違いで切替えたい | `edition-layerstacks-any-any-any` | size / ext を runtime dispatch |
| 全 arch / 任意 size をまとめて動作確認したい | `edition-universal` | header 駆動で対応 family を 1 binary に集約できる |

`edition-universal` は NNUE header にある L1/L2/L3 を実行時に読み、HalfKX と
LayerStacks のバッファおよび dense layer を動的に構成する。従って、新しい次元を
追加するたびに Cargo feature とバイナリを増やす必要はない。HalfKP / HalfKaSplit /
HalfKaMerged / HalfKaHmSplit / HalfKaHmMerged、HalfKX の CReLU / SCReLU /
PairwiseCReLU、LayerStacks の PSQT / Threat profile 0 / EffectBucket、および
KingRank9 / ProgressKPAbs bucket mode を扱う。

一方、family / specific preset は従来どおり const-generic 実装を使う。大会、selfplay、
SPRT など同じモデルを長時間使う用途では、SIMD 最適化と dispatch 除去を得られる
specific preset を選ぶ。universal の目的はモデル互換性と配布サイズであり、固定
edition の置き換えではない。

### 命名規則

preset 名は `edition-{arch_class}-{slots...}` 形式:

- HalfKP: `edition-halfkp-{activation}`
- HalfKa\* (4 variant): `edition-{arch}-{activation}`
- LayerStack: `edition-layerstacks-{ft}-{size}-{ext}`
- HalfKX family alias: `edition-halfkx[-{activation}]`
- LS family alias: `edition-layerstacks[-any-any-any]`
- universal: `edition-universal`

slot 値の意味論 (`any` / `none` / 具体値) の詳細は ADR の「slot 値の意味論」セクション
参照。

## cargo xtask reference

### `cargo xtask build`

preset edition を build して `engines/` 下に配置する。

```
cargo xtask build [--edition <preset>[,<preset>...]] [--all-presets]
                  [--profile <name>] [--features <name>[,<name>...]]
```

- `--edition <name>` : preset edition (`edition-` 接頭辞省略可)。複数指定可
  (カンマ区切り or `--edition` 複数回)。
- `--all-presets`    : `list-editions` の全 preset を順次 build。`--edition` と排他。
- `--profile <name>` : cargo profile。デフォルト `production` (LTO=fat、Full LTO、
  単一 codegen unit)。`release` は dev iteration 向け (thin LTO)、`profiling` は
  releaseの詳細debug情報付き、`production-profiling` はproductionの最適化条件に
  行情報を加えた解析用profile。`dev` はcargoのdefault debug build。
- `--features <name>` : edition に追加で有効化する rshogi-usi の opt-in feature。
  複数指定可 (カンマ区切り or `--features` 複数回)。build 対象の全 edition に付く
  (`--all-presets` とも併用可)。詳細は下の「追加 feature」参照。

build 後、`engines/rshogi-usi-<edition slug>` と `<binary>.meta.toml` がペアで
生成される。

#### 追加 feature (`--features`)

Edition 軸と直交する opt-in feature (`mimalloc` / `prepacked-nnue` / `search-stats` /
`nnue-stats` / `tt-trace` / `diagnostics` / `allocation-stats` / `tt-write-stats` 等) を
preset edition に重ねて build する。指定できる名前は `crates/rshogi-usi/Cargo.toml` の
`[features]` に定義されたもので、次は拒否する:

- rshogi-usi に存在しない名前
- `default` (xtask は `--no-default-features` で edition を 1 つに固定する)
- `edition-*` (edition は `--edition` で指定する)
- edition の構成部品: rshogi-core のいずれかの preset edition が bundle する feature
  (`mode-*` / `layerstack-arch` / `ft-*` / `layerstacks-<dims>` / `nnue-psqt` /
  `nnue-threat` / `nnue-progress-diff` / preset に束ねられた `threat-profile-*` 等)。
  edition 名と実際の構成が食い違う binary を作らないため、該当 preset を `--edition` で選ぶ

feature 同士の排他 (`mimalloc` と `allocation-stats` 等) は xtask では検査せず、
cargo build のエラーで検出される。

#### 命名規則

`engines/rshogi-usi-<edition slug>[+<feature>...][.exe]`

- `<edition slug>` = preset edition から `edition-` 接頭辞を除いたもの
- `+<feature>` = `--features` で追加した feature。名前順にソートし重複を除いて並べるため、
  指定順によらず同じ構成は同じ名前になる。追加なしなら何も付かない。Edition 名は
  slot 区切りに `-`、slot 内の複合語に `_` を使い、feature 名も `-` を含むため、
  区切りにはどれとも衝突しない `+` を使う
- Windows host では `.exe` 拡張子付与 (Linux/macOS は空)

例:
```
edition=edition-layerstacks-halfka_hm_merged-1536x16x32-psqt
  → engines/rshogi-usi-layerstacks-halfka_hm_merged-1536x16x32-psqt

edition=edition-layerstacks-halfka_hm_merged-1536x16x32-none, features=search-stats,mimalloc
  → engines/rshogi-usi-layerstacks-halfka_hm_merged-1536x16x32-none+mimalloc+search-stats
```

### `cargo xtask list-editions`

rshogi-core の Cargo.toml から `edition-*` preset を抽出して列挙する。
新しい preset が ADR 拡張で追加された場合の発見にも使える。

```bash
$ cargo xtask list-editions
edition-halfka_hm_merged-screlu
edition-halfkp-crelu
edition-halfkx
edition-halfkx-any
edition-layerstacks
edition-layerstacks-any-any-any
edition-layerstacks-halfka_hm_merged-1536x16x32-none
edition-layerstacks-halfka_hm_merged-1536x16x32-psqt
... (現状 24 件)
edition-universal
```

### `cargo xtask list-binaries`

`engines/` 下の binary を manifest と合わせて整形表示する。

```bash
$ cargo xtask list-binaries
BINARY                                            EDITION                                        PROFILE     COMMIT    AGE  SIZE    STATUS
rshogi-usi-layerstacks-halfka_hm_merged-1536x16x32-psqt    edition-layerstacks-halfka_hm_merged-1536x16x32-psqt    production  5616ea7c  2h   2.9 MB  current
rshogi-usi-layerstacks-halfka_hm_merged-1536x16x32-threat  edition-layerstacks-halfka_hm_merged-1536x16x32-threat  production  4a34e06b  5d   2.8 MB  stale
```

STATUS 列:

| 値 | 意味 |
|---|---|
| `current` | manifest 記録の commit が HEAD と一致 |
| `stale`   | HEAD と異なる commit で build されている (古い) |
| `dirty`   | manifest 記録時に working tree が dirty だった |
| `(no manifest)` | xtask 経由 build でない binary (`.meta.toml` 不在、user が手動配置した場合) |
| `(manifest broken)` | `.meta.toml` が存在するが parse 失敗 (stderr に warning も出力) |

## build manifest

xtask 経由 build では `engines/<binary>.meta.toml` を同階層に書き出す。後から
「この binary はどの commit / profile か」を追跡するための manifest。

例:
```toml
schema_version = 1
edition = "edition-layerstacks-halfka_hm_merged-1536x16x32-psqt"
profile = "production"
commit = "5616ea7c056ff21b6705c0ef00ca7266b7b2849f"
commit_dirty = false
built_at = "2026-05-24T22:30:00+09:00"
rustc = "rustc 1.85.0 (abc 2026-01-01)"
binary = "rshogi-usi-layerstacks-halfka_hm_merged-1536x16x32-psqt"
```

`--features` 付きで build した場合だけ、`edition` の次に `features` が入る
(追加なしの build では field ごと省略され、上の例と同じ内容になる):

```toml
edition = "edition-layerstacks-halfka_hm_merged-1536x16x32-none"
features = ["mimalloc"]
```

`cargo xtask list-binaries` の EDITION 列も、追加 feature があれば binary 名と同じ
`<edition>+<feature>` 形式で表示する。

旧 v1 manifest (`flavor` field を含む) も `cargo xtask list-binaries` で
parse 失敗せず読める (serde 既定の未知フィールド silently ignore)。

| field | 内容 |
|---|---|
| `schema_version` | manifest schema version (現在 1) |
| `edition` | build に使った preset edition の正式名 |
| `features` | `--features` で追加した feature (ソート・重複除去済み)。追加なしなら field 自体を省略 |
| `profile` | cargo profile 名 |
| `commit` | `git rev-parse HEAD` の結果。取得失敗時は `"unknown"` + stderr warning |
| `commit_dirty` | build 時に `git status --porcelain` が非空だったか |
| `built_at` | build 完了時刻 (Local TZ、RFC3339) |
| `rustc` | `rustc --version` の出力。取得失敗時は `"unknown"` + stderr warning |
| `binary` | 同階層 binary のファイル名 |

manifest は selfplay/SPRT 後の事後検証 (「Elo +93 出た binary はどの commit?」「dirty
build じゃないか？」) で使う。

## build profile の使い分け

`Cargo.toml` で定義済みの profile:

| profile | 用途 | 設定 |
|---|---|---|
| `dev` (default) | デバッグ / 単体テスト | `opt-level=1`、`target/debug/` |
| `release` | 開発 iteration、軽い perf 比較 | `opt-level=3`、thin LTO、`target/release/` |
| `profiling` | perf 計測 (release + debug info) | release + debug info 保持 |
| `production` | 本番デプロイ / 公平 NPS 比較 | Full LTO、codegen_units=1、`panic=abort`、`target/production/` |
| `production-profiling` | production条件でのソース行対応付け | production継承、`debug="line-tables-only"`、`strip="none"` |

本番条件のホットスポットをソース行へ対応付ける場合は、例えば次のようにビルドする。

```bash
cargo run --release -p xtask -- build \
  --edition layerstacks-halfka_hm_merged-1536x16x32-none \
  --profile production-profiling
```

行情報だけを保持し、詳細な変数・型のデバッグ情報は目的としない。
Windows MSVCでは対応するPDBを `target/production-profiling/` に保持する。
xtaskが `engines/` へコピーするのは実行ファイルとmanifestのみで、PDBは含まれない。
再ビルドで上書きされる前に、解析対象の実行ファイルと対応するPDBを一緒に退避する。
バイナリを別の場所で解析するときも同じビルドのPDBを用意する。
debug情報によってコード配置が変わる可能性があるため、productionとバイナリや速度が
完全に同じとは限らない。速度のA/B比較では両側のprofileを揃え、解析用buildと混在させない。

xtask の `--profile` デフォルトは `production`。SPRT 等の棋力比較は profile を揃える
必要があるため固定 (memory `feedback_build_profile_consistency`)。

PGO build は `scripts/build_pgo.sh` 参照 (NPS +6-7% を狙う、本番計測前の追い込み用)。

## 既存 build scripts との関係

- `scripts/build_pgo.sh`: profile-generate → benchmark → profile-use → build を集約。
  PGO build は xtask 統合 scope 外。直接 `bash scripts/build_pgo.sh --verify` を実行。
- `scripts/build_*.sh`: ad-hoc build script は readable な atomic feature 名
  (`layerstack-arch` / `layerstacks-*` / `nnue-psqt` / `nnue-threat`) を直接指定すれば動く。

`cargo build --features <atomic feature>` の直接指定もサポートするが、
新規 user は preset edition (`cargo xtask build --edition <name>`) 経由を推奨。

## 今後の追加予定 (本ドキュメントの未対応項目)

ADR Phase 2 完了後 / 別 Issue で追加予定:

- `cargo xtask verify <binary>`: 既存 binary に USI smoke (usi/isready/readyok) を流す
  自動検証。preset → 推奨 NNUE model のマッピング policy 決定後に着手 (Tier 2)。
- `cargo xtask clean --stale`: 古い binary を listing / 削除 (destructive、user 確認 prompt 付き)。
- WASM target 整備 (Issue #740): `cargo xtask build --target wasm32-unknown-unknown` 対応。
- preset → model 対応表 / SPSA tune 後 binary の運用 doc。

## 関連 ドキュメント

- [ADR: ビルド設定の Edition 軸設計][adr] (本ドキュメントの設計根拠)
- [`engines/README.md`](../engines/README.md) (binary 保管方針)
- [`docs/nnue-supported-architectures.md`](./nnue-supported-architectures.md) (NNUE arch 一覧)
- [`docs/nnue-architecture-detection.md`](./nnue-architecture-detection.md) (auto-detect ロジック)
