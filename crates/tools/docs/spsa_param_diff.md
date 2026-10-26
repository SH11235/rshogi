# spsa_param_diff

SPSA の現在値と基準値を比較し、変更件数と絶対差の大きい順の一覧を表示します。

```bash
cargo run -p tools --release --bin spsa_param_diff -- \
  --current /path/to/run/state.params \
  --base /path/to/base.params \
  --param-values-csv /path/to/run/param_values.csv
```

`--base` を省略すると rshogi の探索パラメータの既定値を基準にします。`--regex` で対象名を絞れます。
現在値にない項目は基準値を使い、基準にない項目は表示しません。
`--param-values-csv` を指定すると、途中で基準値から動いた項目（`touched`）も数えます。

value 列は i32、小数は四捨五入して読みます。非有限値と丸め後の範囲外値は拒否します。
差は i64 で計算するため、i32 の最小値と最大値の間の差も表示できます。
