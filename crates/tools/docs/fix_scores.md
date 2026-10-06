# fix_scores

対応する元ファイルのスコアを使い、前処理済み PSV のスコアだけを訂正します。
入力は同じサイズで、40 バイト固定長のレコードが同じ順番に並んでいる必要があります。

```bash
# 最初の5レコードを確認するだけで、ファイルは変更しない
cargo run --release -p tools --bin fix_scores -- \
  --original original.psv --preprocessed preprocessed.psv

# スコアを訂正する
cargo run --release -p tools --bin fix_scores -- \
  --original original.psv --preprocessed preprocessed.psv --yes
```

`--sample-count` で確認件数を変更できます。書き込みは `--yes` を指定した場合のみ行います。

`--output` を省略するか、`--preprocessed` と同じパスを明示すると、前処理済みファイルを
インプレース更新します。この入力パスが symlink の場合も、リンク先を更新します。
元ファイルと前処理済みファイルが同じパス・実体を指す場合は、元ファイルを保護するため
更新を拒否します。

別ファイルへ保存するには `--output corrected.psv` を指定します。
その出力は両入力と別の通常ファイルにしてください。入力を指す hardlink や同一パス、
symlink の出力パスは書き込み前に拒否します。前処理済みファイルの別名を出力に指定して
インプレース更新することはできません。既存の別ファイルの出力は上書きします。
