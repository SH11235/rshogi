# engines/

評価対象として長期保持したい USI エンジンバイナリの置き場。

## 方針

- `target/production/` は `cargo clean` で消える可能性があるため、評価で繰り返し使うバイナリは
  ここに退避する。
- `/tmp/` は再起動で揮発するので NG。
- `.gitignore` で本体は除外、README と `.gitkeep` のみコミット対象。

## 推奨フロー: `cargo xtask` 経由

新規 build は `cargo xtask` 経由を推奨。preset edition 命名規則で binary を配置し、
同階層に `<binary>.meta.toml` (commit / profile / built_at / rustc) を残す:

```bash
cargo xtask build --edition ls-halfka_hm_merged-1536x16x32-psqt
# → engines/rshogi-usi-ls-halfka_hm_merged-1536x16x32-psqt
# → engines/rshogi-usi-ls-halfka_hm_merged-1536x16x32-psqt.meta.toml

cargo xtask list-binaries  # commit/age/status を表表示
```

詳細な workflow / preset の選び方 / manifest フィールド説明は
[`docs/build.md`](../docs/build.md) を参照。

## Legacy: 手動命名で残っている binary

xtask 導入前 (Issue #738) の手動命名 binary。Issue #739 で xtask 命名規則に
rename 予定:

```
rshogi-usi-<arch>-<flags>-<purpose>
```

- `<arch>`: NNUE アーキテクチャ。例 `1536x16x32`, `768x16x32`, `halfkahm`
- `<flags>`: 有効化した特徴。`psqt`, `threat`, `progdiff` 等。複数は `-` 連結
- `<purpose>`: 用途識別子。例 `v100v101cmp`, `baseline`, `spsa-tuned`

例:
- `rshogi-usi-1536x16x32-v100v101cmp` — v100 vs v101 比較用、PSQT なし
- `rshogi-usi-1536x16x32-psqt-v100v101cmp` — v101 評価用、PSQT 有効

### 現在の保持 legacy binary

| ファイル | 対応モデル | feature | 用途 | ビルド日 |
|---|---|---|---|---|
| rshogi-usi-1536x16x32-v100v101cmp | v100 系 (1536x16x32, no PSQT) | `layerstack-only,nnue-progress-diff` (default で `layerstacks-1536x16x32`) | v100 vs v101 比較 | 2026-05-09 |
| rshogi-usi-1536x16x32-psqt-v100v101cmp | v101 系 (1536x16x32, PSQT) | `layerstack-only,nnue-psqt,nnue-progress-diff` | v101 評価 | TBD |

ビルド profile は **production** で統一（公平な NPS 比較のため）。
新 xtask 経由 build もデフォルト profile は `production` (詳細は
[`docs/build.md`](../docs/build.md))。
