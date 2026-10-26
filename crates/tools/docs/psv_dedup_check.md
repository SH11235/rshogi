# psv_dedup_check

PSV の局面重複率と出現回数の分布を調べます。

```bash
cargo run -p tools --release --bin psv_dedup_check -- \
  --data "$SHOGI_DATA/teachers/source.psv" --table-size 4M --by-ply
```

`--table-size` はバイト数ではなくエントリ数です。`4M` は 4194304 個の u64、約 32 MiB を使います。
指定時は固定メモリの近似集計で、テーブルの上書きにより重複を見逃す場合があります。
省略すると全ユニーク局面の 64 bit ハッシュと出現回数を保持し、メモリはユニーク局面数に比例します。
どちらのモードもハッシュ衝突で異なる局面を同一視する可能性があります。
完全な局面一致で重複を除去するには [psv_dedup_partition](psv_dedup_partition.md) を使ってください。

入力ファイルのサイズが 40 byte の倍数でない場合、処理前にエラーで終了します。
