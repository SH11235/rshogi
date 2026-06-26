# prep_hcpe

`prep_hcpe` は、一回限りの hcpe 教師プール前処理として、汚染除去、局面重複除去、
決定的シャッフル、件数制限、分割出力をまとめて行うツールです。

hcpe は 1 レコード 38 byte として扱います。先頭 32 byte の
`HuffmanCodedPos` が局面 key、残りは eval、bestMove16、gameResult、padding です。

## 使用例

```bash
cargo run -p tools --release --bin prep_hcpe -- \
  --in "$SHOGI_DATA/teachers/a.hcpe" "$SHOGI_DATA/teachers/b.hcpe" \
  --exclude "$SHOGI_DATA/teachers/heldout.hcpe" \
  --out-dir "$SHOGI_DATA/teachers/prepared" \
  --prefix train \
  --target 100000000 \
  --seed 42
```

`--in` と `--exclude` はそれぞれパスをソートし、重複したパスを除去してから処理するため、
引数の指定順は出力に影響しません。同じファイル内容と seed からは同じ byte 列を出力します。

## オプション

| オプション | 説明 | 既定値 |
|---|---|---:|
| `--in <FILE>...` | 入力 hcpe（必須、複数可） | - |
| `--exclude <FILE>...` | 除外する局面を含む hcpe（複数可） | なし |
| `--out-dir <DIR>` | 出力ディレクトリ（必須） | - |
| `--prefix <STR>` | 出力ファイル名の接頭辞 | `chunk` |
| `--chunk-records <N>` | 1ファイルあたりのレコード数 | `250000` |
| `--target <N>` | shuffle 後に残す件数。0 は全件 | `0` |
| `--seed <U64>` | shuffle seed | `42` |
| `--expected-records <N>` | Bloom filter の想定投入件数 | `100000000` |
| `--false-positive-rate <F>` | Bloom filter の偽陽性率 | `1e-6` |

出力名は `<prefix>_00000.hcpe`, `<prefix>_00001.hcpe`, ... です。既存の同名ファイルは
上書きします。前回よりチャンク数が減る再実行では古い余剰チャンクを自動削除しないため、
空の出力ディレクトリを使用してください。

## メモリと重複判定

除外局面（汚染除去・クロス重複）は exact な `HashSet<[u8; 32]>` に保持するため、偽陰性は
ありません（= 除外集合の局面は確実に全て落ちる。汚染が出力に漏れない）。自己重複は
512-bit blocked Bloom filter で判定します。偽陽性により、ごく少数の新規局面が重複として
落ちる可能性があります（汚染漏れ方向ではないので安全側）。

**ピークメモリは `--target` で有界**です。`--target N`（>0）を指定すると、フィルタ通過後の
レコードを **reservoir sampling（Algorithm R）** で N 件の無偏標本だけ保持するため、メモリは
入力件数ではなく `target` で頭打ちになります（100M なら本体 ≈ 3.54 GiB）。`--target 0`（全件）
のときのみ、shuffle のため全生き残りをメモリに保持します（= 生き残り件数に線形）。Bloom は
既定設定で約 343 MiB、除外 `HashSet` は除外件数に比例（val 数万件なら無視できる量）。reservoir
と最終 shuffle は同一の seed 付き `ChaCha8Rng` を使うため、同じ入力・seed なら出力は byte 一致します。

各入力・除外ファイルのサイズが 38 byte の倍数でなければ、処理をエラー終了します。
終了時に `read / excluded(contam) / deduped / kept / written / chunks` を表示します。
