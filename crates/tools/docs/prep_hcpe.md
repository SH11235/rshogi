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

出力名は `<prefix>_00000.hcpe`, `<prefix>_00001.hcpe`, ... です。
同じ prefix の `.hcpe` が既にある出力先は、処理前にエラーにします。空の出力先か別の prefix を
指定してください。ファイルは新規作成し、入力・除外ファイルと同じ実体への書き込みも拒否します。
`--prefix` にパス区切りや `.` / `..` は使えません。中断後の再開機能はなく、部分出力が残った場合も
新しい出力先か prefix で実行してください。

## メモリと一時ディスク

自己重複は 512-bit blocked Bloom filter で近似判定します。新規局面を誤って重複扱いする
偽陽性があり、指定した率は古典的 Bloom のサイズ計算に使う目安です。blocked 方式の実効率は
高めに出る場合があります。入力が `--expected-records` を超える場合は警告します。
既定設定の Bloom 本体は約 343 MiB で、想定投入件数に比例します。

`--target N`（N > 0）は reservoir sampling で最大 N 件をメモリに保持します。
レコード本体の上限は `38 × N` byte（100M 件なら約 3.54 GiB）です。
`--target 0` は 250000 件までメモリに保持し、それを超えると 128 bit 乱数キーで外部ソートして
全通過レコードを並べ替えます。ソートのバッファは 250000 件、マージは最大 32 ファイルずつです。
同じ入力・seed なら出力 byte 列は一致しますが、大規模な全件出力の順番は旧版とは異なります。

除外局面は完全一致で判定します。250000 種類未満はメモリの集合、それ以上はディスク上の
ソート済み 32 byte キーを二分探索するため、除外件数に応じてメモリが増え続けません。
大きな除外集合は入力ごとにランダムなディスク読み取りが発生し、速度が低下する場合があります。

一時ファイルは OS の一時ディレクトリを使います。全件 shuffle は通過 1 件あたり 62 byte、
除外集合は 1 レコードあたり 32 byte のファイルを作り、マージ中は元ファイルとマージ結果が
一時的に共存します。目安としてそれぞれ 2 倍までの空き容量を確保し、必要なら OS の
一時ディレクトリ設定（Linux の `TMPDIR`、Windows の `TEMP` など）を容量のある場所へ変更してください。
一時ファイルは正常終了時や通常のエラー返却時に削除します。強制終了や電源断では残る場合があります。
入力ファイルは削除しません。

各入力・除外ファイルのサイズが 38 byte の倍数でなければ、処理をエラー終了します。
終了時に `read / excluded(contam) / deduped / kept / written / chunks` を表示します。
`kept` はフィルタ（汚染除去・重複除去）を通過した総レコード数（reservoir サンプリング前）、
`written` は実際に出力したレコード数です。`--target N`（N>0）指定時は reservoir で N 件に
絞るため `kept`（通過総数）≥ `written`（=N、ただし通過総数が N 未満なら通過総数）になります。
