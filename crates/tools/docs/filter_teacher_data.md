# filter_teacher_data

PSV 教師データをフィルタリングし、評価値のクリップと統計の集計を行います。

```bash
cargo run --release -p tools --bin filter_teacher_data -- \
  --input teacher.psv --output filtered.psv \
  --score-abs-max 30000 --score-clip 2500 --stats stats.json
```

`--filter-in-check` は王手局面を除外し、`--threads` で並列度を指定できます（0 は自動）。
`--ply-min` は指定手数未満を除外します。`--limit` の既定値 0 は全レコードを処理します。
`--histogram` で分位点を含む統計を、`--target-scale` で飽和率を集計できます。

統計だけを集計するには `--stats-only` を指定します。この場合 `--output` は省略でき、
指定してもそのパスには書き込みません。`--stats` の JSON 保存は引き続き有効です。

PSV 出力と統計 JSON の保存先は、入力と別の通常ファイルにしてください。
同一パス、入力を指す hardlink、symlink の出力パスは処理前に拒否します。
PSV 出力と統計 JSON が同じパス・実体を指す指定も拒否します。
`--stats-only` では未使用の `--output` をこの検査に含めません。
既存の別ファイルの出力は上書きします。
