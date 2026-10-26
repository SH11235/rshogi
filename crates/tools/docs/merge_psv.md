# merge_psv

PSV をレコードの順序を保って結合します。

```bash
cargo run -p tools --release --bin merge_psv -- \
  --input-dir "$SHOGI_DATA/teachers/shards" --pattern 'train_*.bin' \
  --output "$SHOGI_DATA/teachers/merged.psv"
```

[使い方とオプション](pack_tools.md#merge_psv)を参照してください。
出力が入力と同じ実体（hardlink を含む）を指す場合や、出力自体が symlink の場合は拒否します。
