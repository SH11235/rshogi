# check_param_mapping

マッピング表の名前と、任意の params ペア・YO の USI option を照合します。

```bash
cargo run --release -p tools --bin check_param_mapping -- \
  --mapping tune/yo_rshogi_mapping.toml \
  --yo-params /path/to/yo.params --rshogi-params /path/to/rshogi.params --strict
```

`--yo-binary /path/to/engine` を追加すると公開 USI option も検査します。応答期限は
`--yo-timeout-secs`（既定 5 秒）で指定します。`--strict` は検出した不一致を非ゼロ終了にします。
値の符号反転結果を i32 で表せない場合は、`--strict` の有無によらずエラーで終了します。
