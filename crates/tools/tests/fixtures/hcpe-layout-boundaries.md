# HCPE layout boundary fixture

`hcpe-layout-boundaries.hcpe` は3レコード（114 byte）の固定fixtureです。
[cshogi の `HuffmanCodedPosAndEval` 定義](https://github.com/TadaoYamaoka/cshogi/blob/master/cshogi/_cshogi.pyx)
の順序・幅とlittle-endian表現から独立に作成したbyte列で、Rust codecの出力ではありません。
hcpはoffset検査用のパターンであり、合法局面を表しません。

| レコード | hcp 32 byte | eval | bestMove16 | gameResult | dummy |
|---|---|---:|---|---:|---|
| 1 | 0x00〜0x1f の昇順 | -32768 | 0x1234 | 0 | 0xa5 |
| 2 | 0x1f〜0x00 の降順 | 32767 | 0xffff | 2 | 0x5a |
| 3 | すべて0xff | -1 | 0x0000 | 1 | 0xff |

末尾6 byteは順に `00 80 34 12 00 a5`、`ff 7f ff ff 02 5a`、`ff ff 00 00 01 ff`。
codecテストはこれらを明示したフィールド値へdecodeし、別途組み立てたフィールド値のencodeも
固定byte列と照合します。evalだけを書き換える場合の残り36 byte保持も検査します。
合法局面の互換性は既存のcshogiオラクル `psv_to_hcpe3_yaneuraou_sample.hcpe` と
対応するPSV変換のintegrationテストで検証します。
