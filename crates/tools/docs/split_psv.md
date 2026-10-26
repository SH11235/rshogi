# split_psv

PSV を局面数または容量で分割します。入力全体をメモリへ載せず処理します。

```bash
cargo run -p tools --release --bin split_psv -- \
  --input "$SHOGI_DATA/teachers/source.psv" \
  --output-prefix "$SHOGI_DATA/teachers/shards/train" \
  --records-per-file 100000000
```

既定の出力名は `train_000.bin`、`train_001.bin`、… です。
容量で指定するときは `--records-per-file` の代わりに `--bytes-per-file 4GB` を使います。
レコードサイズは 40 byte で、端数の容量はレコード境界へ切り下げます。
入力末尾の不完全なレコードは警告して無視します。

| フラグ | 既定 | 説明 |
|---|---|---|
| `--input` / `-i` | 必須 | 入力 PSV |
| `--output-prefix` | 必須 | 出力名の接頭辞 |
| `--records-per-file` | — | 1 ファイルの局面数。容量指定とどちらか一方が必須 |
| `--bytes-per-file` | — | 1 ファイルの容量。`GB` は十進、`GiB` は二進 |
| `--write-chunk-records` | 1000000 | 一度に読み書きする局面数（1 以上、最大 13421772） |
| `--start-index` | 0 | 開始番号 |
| `--digits` | 3 | 番号の最小桁数 |
| `--suffix` | `.bin` | 出力名の末尾 |

既存 shard は上書きします。入力と同じ実体（hardlink を含む）を指す shard や symlink は、
その shard を開く際に拒否します。途中まで作られた他の shard は残るため、再実行時は専用の出力先を使ってください。
