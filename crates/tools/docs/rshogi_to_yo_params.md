# rshogi_to_yo_params

rshogi の探索パラメータをマッピング表に従って YaneuraOu の名前へ変換します。

```bash
cargo run --release -p tools --bin rshogi_to_yo_params -- \
  --rshogi-params /path/to/rshogi.params \
  --mapping tune/yo_rshogi_mapping.toml \
  --base /path/to/yo-base.params --output yo.params
```

`--base` の項目順と min / max / step / alpha を保持します。省略すると値を中心に範囲を生成し、
i32 の上下限で切り詰めます。`--strict-range` で範囲外の変換値をエラーにできます。
符号反転した値を i32 で表せない場合は常にエラーで、出力を更新しません。

入力が rshogi の既定値と 95% 以上一致すると警告します。意図的な既定値の入力には
`--allow-rshogi-defaults`、エラーにしたい場合は `--strict-rshogi-defaults` を指定します。
両方は同時指定できません。
