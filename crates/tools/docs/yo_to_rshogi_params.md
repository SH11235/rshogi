# yo_to_rshogi_params

YaneuraOu名の `.params` から値列を読み、対応表に従ってrshogi名へ変換する。
符号反転も対応表に従う。YO入力の列6/7は変換先のscheduleへコピーしない。

```bash
cargo run --release -p tools --bin yo_to_rshogi_params -- \
  --yo-params tune/suisho10.params \
  --mapping tune/yo_rshogi_mapping.toml \
  --output spsa_params/from_yo.params
```

出力には `# spsa-format: c-end-r-end-v1` が付く。`--base` を省略すると
`generate_spsa_params` と同じ範囲・`c_end`・`r_end` を使う。
`c_end = max(1, round((max-min)/20))`、`r_end = 0.002`。
マッピング対象外のrshogi値は既定値のまま残る。

`--base <path>` を指定すると、そのファイルの範囲・`c_end/r_end` と
マッピング対象外の値を保持する。baseには同じ形式宣言が必要。
列6/7が現形式と確認済みの未宣言baseに限り `--assume-c-end-r-end` を指定できる。
旧 `step,delta` のbaseへ宣言だけを追加したり、確認フラグだけで読み替えたりしない。

範囲外への値転記はwarningとなり、`--strict-range` を付けるとerrorで停止する。
出力先の既存ファイルを上書きするため、入力と別の新しいパスを使う。
新runへの投入と既存canonicalの移行は [実行runbook](spsa_runbook.md) を参照。
