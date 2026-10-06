# pack_to_psv

GenSfen の可変長 `.pack` 棋譜を、40 バイト固定長の PackedSfenValue (PSV) に展開します。
move16 は実着手ラベルを実 YaneuraOu 形式に変換します。

```bash
cargo run --release -p tools --bin pack_to_psv -- \
  --input data.pack --output teacher.psv
```

- `--input` はカンマ区切りで複数ファイルを指定できます。
- `--input-dir` と `--pattern`（既定 `*.pack`）でも入力を指定できます。
  `--input` と `--input-dir` は同時に指定できません。
- `--max-games` の既定値 0 は全対局を処理します。正の値では逐次処理し、指定数で止めます。

出力には全入力と別の通常ファイルを指定してください。同一パス、いずれかの入力を指す
hardlink、symlink の出力パスは、出力を作成する前に拒否します。
既存の別ファイルの出力は上書きします。
