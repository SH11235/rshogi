# psv_dedup

PSV の先頭 32 byte を局面キーとして重複を除去し、最初のレコードを残します。
[方式の選択と使い方](pack_tools.md#重複除去ツールの選び方)も参照してください。

```bash
cargo run -p tools --release --bin psv_dedup -- \
  --input "$SHOGI_DATA/teachers/source.psv" \
  --output "$SHOGI_DATA/teachers/unique.psv"
```

64 bit の局面ハッシュを比較するため、ハッシュ衝突で異なる局面を除去する可能性があります。
メモリもユニーク局面数に比例します。完全な一致判定が必要な場合や入力が大きい場合は
[psv_dedup_partition](psv_dedup_partition.md) を使ってください。
出力が入力と同じ実体（hardlink を含む）を指す場合や、出力自体が symlink の場合は拒否します。

入力ファイルのサイズが 40 byte の倍数でない場合、処理前にエラーで終了します。
