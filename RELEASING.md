# リリース手順

保守者向けの手順です。最初に公開対象を決め、準備 PR のレビュー・CI が通ってから対象 commit を固定します。
版の運用方針は [CHANGELOG.md](CHANGELOG.md)、バイナリ作成は [ビルドガイド](docs/build.md)を参照してください。

| 対象 | 版と公開先 | 実施する公開操作 |
| --- | --- | --- |
| core ライブラリのみ | `rshogi-core` の 0.x 系、crates.io | core のみ publish。engine tag は作らない |
| engine 全体 | `vX.Y.Z`、GitHub Release | annotated tag と GitHub Release |
| 両方 | それぞれ独立した版 | core publish 後、engine tag と GitHub Release |

順序は **準備 PR → レビュー・CI → merge SHA 固定 → 隔離 checkout で package 検証 → 対象別公開 → Unreleased 再開** です。
公開途中で失敗したら、[失敗時の再開](#失敗時の再開)で既に公開されたものを確認してから再開します。

## 1. 準備 PR

### core の版と依存要件

core を公開する PR では `crates/rshogi-core/Cargo.toml` の `version` を必ず更新します。
前回 publish 以降の変更を確認し、公開 API の互換性に基づいて次の版を決めてください。
`Unreleased` に API の返り値変更などがある場合、単純な patch bump と決めつけないでください。

Cargo の通常の version requirement は左端の非ゼロ成分までを互換範囲とします。
例えば `"0.8"` は `>=0.8.0, <0.9.0`、`"0.8.1"` は `>=0.8.1, <0.9.0` です。
0.x 系でも、その互換範囲内で利用者を壊す API 変更を patch として出さないようにします。
次の patch 番号や engine の版から core の版を自動的に決めません。

core の版を変更したら、次を同じ PR で確認します。

- `crates/rshogi-usi/Cargo.toml` と `crates/rshogi-book/Cargo.toml` の core 依存には `path` と `version` の両方があります。新しい core の版が要件を満たすよう、必要なら更新します。
- その他の core 依存も各 manifest で確認します。path のみの依存を、今回の手順のために公開対象へ変更しません。
- `Cargo.lock` を更新し、依存要件と lockfile の整合をレビューします。
- core の README にある利用例・MSRV、公開 API の移行説明を確認します。

engine の `vX.Y.Z` は USI engine・CSA server・tools・spsa を含む release marker です。
core や workspace 内の各 crate の version を、この番号に一括で揃えません。

### 変更説明と検証

[CHANGELOG.md](CHANGELOG.md) に利用者向けの変更と移行手順をまとめます。
engine release では対象変更を `vX.Y.Z` の節へ整理し、日付を確認します。
core 単独 publish では core の版・公開対象 commit・移行内容を区別できるようにし、
engine 未公開の変更を engine release 済みと扱わないでください。

リポジトリの必須チェックと変更対象の検証を行い、準備 PR をレビュー・CI 通過後に merge します。
公開物一覧や dry-run に問題があれば、この段階で修正します。個人データ・モデル・実験成果物を
公開用に移動したり、手元の untracked ファイルを削除したりして検査を通してはいけません。

## 2. 対象 commit を固定する

準備 PR の merge commit の完全な SHA を記録します。core は **version bump PR の merge commit から** publish します。
engine も release 対象の merge SHA を固定し、以後の main の先端に読み替えません。
両方を同じ PR で準備した場合は、同じ SHA を使用します。

新しい隔離 checkout を作り、その SHA を checkout します。作業中の checkout を reset・clean したり、
untracked ファイルを移動・削除したりする必要はありません。公開時の操作は以降この隔離 checkout の root で行います。
HEAD が記録した SHA と一致し、tracked の変更や公開対象へ混入する untracked ファイルがないことを確認します。
対象 SHA の `rust-toolchain.toml` に従い、認証情報は checkout 内のファイルへ書き込みません。

```sh
git worktree add --detach /path/to/release-checkout MERGE_SHA
cd /path/to/release-checkout
```

`MERGE_SHA` は記録した完全な SHA、パスは新しく作る隔離 checkout の場所に置き換えます。

## 3. core の package 検証

core が公開対象の場合のみ実施します。workspace 全体を publish せず、常に `-p rshogi-core` を明示します。
`rshogi-book` と `xtask` は `publish = false` であり、他の path 依存 crate もこの手順の公開対象ではありません。

```sh
cargo package -p rshogi-core --locked --registry crates-io --list
cargo publish -p rshogi-core --locked --registry crates-io --dry-run --jobs 2
```

一覧では `Cargo.toml`、ソース、README に加え、`build.rs` が読む `build/checks.rs` と
`build/result_features.rs` などの必要ファイルが含まれることを確認します。
manifest の `license` 属性も確認し、`license-file` を指定する構成ならそのファイルの収録も確認します。
モデル・係数・実験データ・認証情報など、公開すべきでないファイルが含まれていないことも確認してください。
Cargo が生成する package 用ファイルが含まれる場合があります。手元のソース一覧と完全一致すること自体は要件ではありません。

`--registry crates-io` は公開先を明示し、別の default registry 設定に左右されないために指定します。
`--dry-run` は package の作成・展開後のコンパイルを確認し、upload は行いません。成功しても公開は完了していません。
既に公開済みの版でも dry-run は成功し得ます。`already exists` の警告が出た版を実 publish してはいけません。
`--locked` で失敗した場合は隔離 checkout で lockfile を変更して続行せず、準備 PR の修正へ戻ります。
ソースや package 内容を修正した場合も、新たな merge SHA を固定して検証し直します。

## 4. 公開

### core が対象の場合

対象 version が crates.io に未公開であること、対象 SHA と package 検証結果を確認して実施します。
crates.io への公開権限を持つアカウントで認証してください。

```sh
cargo publish -p rshogi-core --locked --registry crates-io --jobs 2
```

crates.io の [rshogi-core](https://crates.io/crates/rshogi-core) で対象 version の公開を確認します。
同じ version は上書き・再 upload できません。core 単独 publish では engine 用の `vX.Y.Z` tag を作りません。

### engine が対象の場合

記録した merge SHA を指す annotated tag `vX.Y.Z` を作成します。tag 名と参照先を確認し、
その tag だけを origin へ push します。既存 tag を移動したり force push したりしてはいけません。
ローカルと remote の tag が同じ対象 SHA を指すことを確認します。

```sh
git tag -a vX.Y.Z MERGE_SHA -m "vX.Y.Z"
git rev-parse 'vX.Y.Z^{commit}'
git push origin refs/tags/vX.Y.Z
git ls-remote origin 'refs/tags/vX.Y.Z' 'refs/tags/vX.Y.Z^{}'
```

annotated tag の object SHA と commit SHA は異なります。remote の `^{}` 行を記録した merge SHA と照合します。


対象 CHANGELOG 節から release notes をファイルに用意し、バイナリを添付する場合は
[ビルドガイド](docs/build.md)に従って同じ SHA から生成・検証します。
次の `vX.Y.Z` と notes のパスは実際の値に置き換えてください。

```sh
gh release create vX.Y.Z --verify-tag --title "vX.Y.Z" --notes-file /path/to/release-notes.md
gh release view vX.Y.Z
```

`--verify-tag` は remote tag の存在確認です。参照先 SHA の確認に代わるものではありません。
GitHub Release の tag・本文・添付ファイルを確認して完了とします。

## 5. Unreleased を再開する

公開結果を確認した後、通常の PR で CHANGELOG の `Unreleased` を次の変更を書ける状態にします。
既に準備 PR で空の `Unreleased` を残している場合は維持します。
core 単独 publish では、まだ engine release に含めていない変更説明を消さないでください。
この後続 PR の commit に、公開済み tag を付け替えることはしません。

## 失敗時の再開

| 状態 | 再開方法 |
| --- | --- |
| package/list/dry-run が失敗 | 公開していない。通信などの一時的失敗は同じ SHA で再試行。ソースや lockfile の修正が必要なら PR を merge し、新しい SHA で検証し直す |
| publish が upload 前に明確に失敗 | registry に対象 version がないことを確認し、同じ SHA・version のまま認証や通信の原因を解消して再試行できる |
| publish が timeout、または upload の成否不明 | crates.io で対象 version を確認する。公開済みなら再 upload せず次の操作へ進む。確認できない間は重ねて publish しない |
| core publish は成功、その後の engine 公開が失敗 | core を再 publish せず、固定済み SHA から engine 操作のみ再開する |
| tag は remote に存在し、Release 作成が失敗 | tag の SHA と `gh release view vX.Y.Z` を確認。Release がなければ同じ tag で作成を再試行する |
| Release 作成の成否不明 | `gh release view vX.Y.Z` と GitHub 上で確認する。認証・通信エラーを「存在しない」と扱わない |
| 公開済み core の内容に誤りがある | 同じ version は変更できない。修正 PR と新しい version を準備する |
| 既存 tag が予定 SHA と異なる | 停止して差異を調べる。tag の移動・force push で合わせない |

公開操作とその確認が済んだところから再開します。手元のファイルを直して既存 version・tag に上書きする運用はしません。

## 参考

- [Cargo Book: Publishing on crates.io](https://doc.rust-lang.org/cargo/reference/publishing.html)
- [Cargo Book: SemVer Compatibility](https://doc.rust-lang.org/cargo/reference/semver.html)
- [Cargo Book: Specifying Dependencies](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html)
