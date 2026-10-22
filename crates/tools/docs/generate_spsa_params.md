# generate_spsa_params

`SearchTuneParams::option_specs()` の探索パラメータから、現行SPSA用のcanonicalを生成する。

```bash
cargo run --release -p tools --bin generate_spsa_params -- \
  --output spsa_params/canonical.params
```

先頭は `# spsa-format: c-end-r-end-v1`。各行は
`name,int,value,min,max,c_end,r_end` で、値と範囲はエンジンの既定値に一致する。
`c_end = max(1, round((max-min)/20))`、`r_end = 0.002` を出力する。
`--output` の既存ファイルは上書きされるので、比較や移行には別の出力先を使う。

既存の調整済み値は自動的に取り込まない。旧ファイルから移行する場合は生成結果を
別ファイルへ保存し、名前を照合して値列だけを転記したうえで範囲と学習率を確認する。
[実行runbook](spsa_runbook.md#7-params-ファイルの形式と移行) を参照。
