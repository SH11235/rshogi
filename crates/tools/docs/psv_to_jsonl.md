# psv_to_jsonl

40 バイト固定長の PackedSfenValue (PSV) を、SFEN・評価値・指し手を持つ JSONL に
変換します。教師データの確認・デバッグ用です。`depth` と `nodes` は常に 0 です。

```bash
cargo run --release -p tools --bin psv_to_jsonl -- \
  --input data.psv --output data.jsonl --limit 10000
```

`--limit` の既定値 0 は全レコードを処理します。`--verbose` で詳細を表示できます。

出力には入力と別の通常ファイルを指定してください。同一パス、入力を指す hardlink、
symlink の出力パスは書き込み前に拒否します。既存の別ファイルの出力は上書きします。
symlink である `/dev/stdout`、`/dev/fd/N` やプロセス置換 `>(...)` の出力先も、
この共通規約により拒否されます。
