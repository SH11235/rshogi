# validate_psv

PSV の不正局面を検出し、任意で正常なレコードだけを書き出します。

```bash
cargo run -p tools --release --bin validate_psv -- \
  --data "$SHOGI_DATA/teachers/source.psv" \
  --output "$SHOGI_DATA/teachers/valid.psv"
```

`--output` を省略すると検査のみ行います。出力が入力と同じ実体（hardlink を含む）を指す場合や、
出力自体が symlink の場合は拒否します。
