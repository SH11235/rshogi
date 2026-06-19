# engines/

評価対象として長期保持したい USI エンジンバイナリの置き場。

## 方針

- `target/production/` は `cargo clean` で消える可能性があるため、評価で繰り返し使うバイナリは
  ここに退避する。
- `/tmp/` は再起動で揮発するので NG。
- `.gitignore` で本体は除外、README と `.gitkeep` のみコミット対象。

## ビルドフロー: `cargo xtask` 経由

binary の配置は `cargo xtask build` で行う。preset edition 命名規則で配置し、
同階層に `<binary>.meta.toml` (commit / profile / built_at / rustc) を残す。
preset の選び方・slot 値の意味論・manifest フィールドの詳細は
[`docs/build.md`](../docs/build.md) を参照。

### 典型コマンド

```bash
# 利用可能な preset 一覧
cargo xtask list-editions

# 1 preset を build (engines/rshogi-usi-<edition slug> に配置)
cargo xtask build --edition layerstacks-halfka_hm_merged-1536x16x32-psqt

# 複数 preset を順次 build
cargo xtask build --edition layerstacks-halfka_hm_merged-1536x16x32-psqt,layerstacks-halfka_hm_merged-1536x16x32-none

# engines/ 配下の binary 一覧 + manifest を整形表示
cargo xtask list-binaries
```

### 命名規則

```
engines/rshogi-usi-<edition slug>[.exe]
```

- `<edition slug>` = preset edition 名から `edition-` 接頭辞を除いたもの
- Windows host では `.exe` 拡張子付与

例:

```
edition=edition-layerstacks-halfka_hm_merged-1536x16x32-psqt
  → engines/rshogi-usi-layerstacks-halfka_hm_merged-1536x16x32-psqt
  → engines/rshogi-usi-layerstacks-halfka_hm_merged-1536x16x32-psqt.meta.toml
```

### manifest

`engines/<binary>.meta.toml` には commit hash / profile / built_at / rustc 等を記録する。
selfplay/SPRT の事後検証 (「この binary はどの commit / profile か」) で使う。
schema とフィールド説明は [`docs/build.md` の build manifest 節](../docs/build.md#build-manifest)
を参照。

`cargo xtask list-binaries` の STATUS 列で manifest 不在 / 古い commit / dirty build を
即座に判別できる。

## PGO build

PGO build は `scripts/build_pgo.sh` 経由 (xtask 統合 scope 外、`target/production/` に出力)。
engines/ に長期保持したい場合は build 後に手動で copy + 命名する。
