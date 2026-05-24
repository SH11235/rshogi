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
cargo xtask build --edition ls-halfka_hm_merged-1536x16x32-psqt

# 複数 preset を順次 build (カンマ区切り or --edition 複数回)
cargo xtask build --edition ls-halfka_hm_merged-1536x16x32-psqt,ls-halfka_hm_merged-1536x16x32-threat
cargo xtask build --edition X --edition Y

# 全 preset を build (現状 18 件)
cargo xtask build --all-presets

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
| **universal** | 開発 / debug、全 arch runtime dispatch | `edition-universal` | 全 5 FT × 全 size × 全 ext |
| **HalfKX family** | 旧 NNUE 系統 (HalfKP / HalfKa\*) を dispatch | `edition-halfkx`, `edition-halfkx-any` | HalfKX 全 variant |
| **HalfKX specific** | HalfKX 単一 architecture 専用 | `edition-halfkp-crelu`, `edition-halfka_hm_merged-screlu` | 1 architecture 固定 |
| **LayerStack family** | LS 全 FT / size / ext を dispatch | `edition-ls`, `edition-ls-any-any-any` | LS 全構成 |
| **LayerStack specific** | LS 単一構成専用、tournament 用 | `edition-ls-halfka_hm_merged-1536x16x32-psqt` 等 | 1 構成固定 |

### どれを選ぶか (典型ケース)

| 状況 | 選ぶ preset | 理由 |
|---|---|---|
| `cargo build` で困っていない | `edition-universal` (= default) | runtime dispatch で何でも読める |
| 同じ NNUE モデルで SPRT / selfplay を回したい | specific preset (例 `edition-ls-halfka_hm_merged-1536x16x32-psqt`) | dispatch 除去で最大 perf、再現性高い |
| HalfKX 系モデル数本を切替えながら触りたい | `edition-halfkx-any` | activation 含む全 HalfKX を 1 binary でカバー |
| LS 系を size 違いで切替えたい | `edition-ls-any-any-any` | size / ext を runtime dispatch |
| 全 arch / 全 size をまとめて動作確認したい | `edition-universal` | runtime dispatch で 1 binary が全 NNUE モデルを読み込める |

### 命名規則

preset 名は `edition-{arch_class}-{slots...}` 形式:

- HalfKP: `edition-halfkp-{activation}`
- HalfKa\* (4 variant): `edition-{arch}-{activation}`
- LayerStack: `edition-ls-{ft}-{size}-{ext}`
- HalfKX family alias: `edition-halfkx[-{activation}]`
- LS family alias: `edition-ls[-any-any-any]`
- universal: `edition-universal`

slot 値の意味論 (`any` / `none` / 具体値) の詳細は ADR の「slot 値の意味論」セクション
参照。

## cargo xtask reference

### `cargo xtask build`

preset edition を build して `engines/` 下に配置する。

```
cargo xtask build [--edition <preset>[,<preset>...]] [--all-presets]
                  [--flavor <name>] [--profile <name>]
```

- `--edition <name>` : preset edition (`edition-` 接頭辞省略可)。複数指定可
  (カンマ区切り or `--edition` 複数回)。
- `--all-presets`    : `list-editions` の全 preset を順次 build。`--edition` と排他。
- `--flavor <name>`  : Edition 軸と直交する flavor。`default` 以外は binary 名末尾に
  `-flavor-<name>` が付加される (例 `engines/rshogi-usi-X-flavor-tournament`)。
  Flavor 軸の中身 (`tournament` の具体内容等) は将来の別 Issue で決定。
  `[a-z0-9][a-z0-9_-]*` の slug のみ受け付ける。
- `--profile <name>` : cargo profile。デフォルト `production` (LTO=fat、Full LTO、
  単一 codegen unit)。`release` は dev iteration 向け (thin LTO)、`profiling` は
  perf 計測用 (release + debug info)、`dev` は cargo の default debug build。

build 後、`engines/rshogi-usi-<edition slug>[-flavor-<flavor>]` と
`<binary>.meta.toml` がペアで生成される。

#### 命名規則

`engines/rshogi-usi-<edition slug>[-flavor-<flavor>][.exe]`

- `<edition slug>` = preset edition から `edition-` 接頭辞を除いたもの
- `flavor` が `default` の場合は省略
- Windows host では `.exe` 拡張子付与 (Linux/macOS は空)

例:
```
edition=edition-ls-halfka_hm_merged-1536x16x32-psqt, flavor=default
  → engines/rshogi-usi-ls-halfka_hm_merged-1536x16x32-psqt

edition=edition-ls, flavor=tournament
  → engines/rshogi-usi-ls-flavor-tournament
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
edition-ls
edition-ls-any-any-any
edition-ls-halfka_hm_merged-1536x16x32-none
edition-ls-halfka_hm_merged-1536x16x32-psqt
... (現状 18 件)
edition-universal
```

### `cargo xtask list-binaries`

`engines/` 下の binary を manifest と合わせて整形表示する。

```bash
$ cargo xtask list-binaries
BINARY                                            EDITION                                        PROFILE     COMMIT    AGE  SIZE    STATUS
rshogi-usi-ls-halfka_hm_merged-1536x16x32-psqt    edition-ls-halfka_hm_merged-1536x16x32-psqt    production  5616ea7c  2h   2.9 MB  current
rshogi-usi-ls-halfka_hm_merged-1536x16x32-threat  edition-ls-halfka_hm_merged-1536x16x32-threat  production  4a34e06b  5d   2.8 MB  stale
rshogi-usi-1536x16x32-v100v101cmp                 -                                              -           -         12d  2.7 MB  (no manifest)
```

STATUS 列:

| 値 | 意味 |
|---|---|
| `current` | manifest 記録の commit が HEAD と一致 |
| `stale`   | HEAD と異なる commit で build されている (古い) |
| `dirty`   | manifest 記録時に working tree が dirty だった |
| `(no manifest)` | xtask 経由 build でない旧 binary (`.meta.toml` 不在) |
| `(manifest broken)` | `.meta.toml` が存在するが parse 失敗 (stderr に warning も出力) |

## build manifest

xtask 経由 build では `engines/<binary>.meta.toml` を同階層に書き出す。後から
「この binary はどの commit / profile か」を追跡するための manifest。

例:
```toml
schema_version = 1
edition = "edition-ls-halfka_hm_merged-1536x16x32-psqt"
flavor = "default"
profile = "production"
commit = "5616ea7c056ff21b6705c0ef00ca7266b7b2849f"
commit_dirty = false
built_at = "2026-05-24T22:30:00+09:00"
rustc = "rustc 1.85.0 (abc 2026-01-01)"
binary = "rshogi-usi-ls-halfka_hm_merged-1536x16x32-psqt"
```

| field | 内容 |
|---|---|
| `schema_version` | manifest schema version (現在 1) |
| `edition` | build に使った preset edition の正式名 |
| `flavor` | `--flavor` の値 (default は `"default"`) |
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

xtask の `--profile` デフォルトは `production`。SPRT 等の棋力比較は profile を揃える
必要があるため固定 (memory `feedback_build_profile_consistency`)。

PGO build は `scripts/build_pgo.sh` 参照 (NPS +6-7% を狙う、本番計測前の追い込み用)。

## 既存 build scripts との関係

- `scripts/build_pgo.sh`: profile-generate → benchmark → profile-use → build を集約。
  PGO build は xtask 統合 scope 外。直接 `bash scripts/build_pgo.sh --verify` を実行。
- `scripts/build_*.sh`: ad-hoc build script は当面そのまま動く。preset edition の
  alias (`layerstack-only` ⇔ `ls-arch` 等) で旧 feature 名指定でも build 可能。
  ADR Phase 2 移行期間中は alias 維持。

`cargo build --features <atomic feature>` の直接指定も後方互換で残しているが、
新規 user は preset edition (`cargo xtask build --edition <name>`) 経由を推奨。

## 今後の追加予定 (本ドキュメントの未対応項目)

ADR Phase 2 完了後 / 別 Issue で追加予定:

- `cargo xtask verify <binary>`: 既存 binary に USI smoke (usi/isready/readyok) を流す
  自動検証。preset → 推奨 NNUE model のマッピング policy 決定後に着手 (Tier 2)。
- `cargo xtask clean --stale`: 古い binary を listing / 削除 (destructive、user 確認 prompt 付き)。
- engines/ 既存 binary の rename (Issue #739): 手動命名 (`rshogi-usi-1536x16x32-psqt-v100v101cmp` 等)
  を xtask 命名規則 (`rshogi-usi-<edition>`) に揃える。
- WASM target 整備 (Issue #740): `cargo xtask build --target wasm32-unknown-unknown` 対応。
- preset → model 対応表 / SPSA tune 後 binary の運用 doc。

## 関連 ドキュメント

- [ADR: ビルド設定の Edition 軸設計][adr] (本ドキュメントの設計根拠)
- [`engines/README.md`](../engines/README.md) (binary 保管方針 + manual naming legacy)
- [`docs/nnue-supported-architectures.md`](./nnue-supported-architectures.md) (NNUE arch 一覧)
- [`docs/nnue-architecture-detection.md`](./nnue-architecture-detection.md) (auto-detect ロジック)
