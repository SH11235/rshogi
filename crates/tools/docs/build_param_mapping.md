# build_param_mapping

YO と rshogi の params ペアから、同じ値または符号が逆の値を使ってマッピング候補を作ります。

```bash
cargo run --release -p tools --bin build_param_mapping -- \
  --yo-params /path/to/yo.params --rshogi-params /path/to/rshogi.params \
  --output mapping.toml
```

rshogi の既定値から動いている項目を候補にします。同名項目の重複は拒否します。
一意に決まらない候補は `ambiguous`、対応がない項目は `unmapped` に出力するため、結果を確認してください。
i32 の最小値は同値候補として扱えますが、符号反転先を i32 で表せないため逆符号候補は作りません。
