# HCPE（38 byte）形式

`hcpe_to_psv`、`yardstick_label`、`psv_to_hcpe3 --format hcpe`、`rescore_hcpe`、
`ek_testset export-hcpe` は、同じ little-endian の固定長レコードを扱います。
PSV（40 byte）や HCPE3（可変長。1局面出力では46 byte）とは異なる形式です。

| フィールド | offset | 長さ | 意味 |
|---|---:|---:|---|
| hcp | 0 | 32 | Huffman 符号化局面 |
| eval | 32 | 2 | i16、手番側視点。範囲 -32768〜32767 |
| bestMove16 | 34 | 2 | cshogi move16 の16 bit列。0 は指し手なし |
| gameResult | 36 | 1 | 0=引分、1=先手勝ち、2=後手勝ち |
| dummy | 37 | 1 | 未使用。新規生成は0、既存レコード更新では元のbyteを保持 |

レイアウトは [cshogi の型定義](https://github.com/TadaoYamaoka/cshogi/blob/master/cshogi/_cshogi.pyx)
に対応します。cshogi の `bestMove16` は signed 16 bit の格納型ですが、指し手の生ビット列を
保つため、Rust側では u16 として扱います。`gameResult` も生のbyteを保持し、
無効な勝敗値をスキップするかどうかは各ツールの規則に従います。
共有codec自体は局面・指し手・勝敗の意味を検証せず、値を正規化しません。

`rescore_hcpe` は eval 以外の36 byteをそのまま保存します。新規出力の
`psv_to_hcpe3 --format hcpe` と `ek_testset export-hcpe` は dummy=0 を書きます。
`hcpe_to_psv` が出力する別形式のPSVでは padding=0 となり、HCPEのdummyは引き継ぎません。
勝敗の手番側視点への変換、評価値のclamp、無効入力の扱いは各ツールの文書を参照してください。
