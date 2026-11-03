# hcpe3_to_psv

dlshogi の可変長棋譜 hcpe3 を、40 バイト固定長の PackedSfenValue (PSV) に展開します。
hcpe3 は 1 局を「開始局面 + 指し手列」で持つので、開始局面から指し手を順に指して
各局面を復元し、1 局面 1 レコードの PSV にします。

```bash
cargo run --release -p tools --bin hcpe3_to_psv -- \
  --input "$SHOGI_DATA/teachers/selfplay.hcpe3" --output "$SHOGI_DATA/teachers/selfplay.psv"

# ディレクトリ内の .hcpe3 を一括変換（パスのソート順で連結）
cargo run --release -p tools --bin hcpe3_to_psv -- \
  --input-dir "$SHOGI_DATA/teachers/hcpe3" --output out.psv
```

## オプション

| オプション | 説明 | 既定値 |
|---|---|---:|
| `--input <FILES>` | 入力 hcpe3（カンマ区切りで複数可）。`--input-dir` と排他 | - |
| `--input-dir <DIR>` | 入力ディレクトリ。`--pattern` と組み合わせる | - |
| `--pattern <GLOB>` | `--input-dir` 使用時の glob パターン | `*.hcpe3` |
| `--output <FILE>` | 出力 PSV。正常終了時に既存ファイルを置換 | - |
| `--max-games <N>` | 全入力ファイルを通算した最大対局数（0 = 全件） | `0` |

並列数は `RAYON_NUM_THREADS` で指定できます。スレッド数を変えても出力バイト列は変わりません。

## 入力形式

各対局は次の並びです (数値はすべて little-endian)。

| 項目 | 長さ | 内容 |
|---|---:|---|
| hcp | 32 | 開始局面の HuffmanCodedPos |
| moveNum | 2 | 指し手の数 (u16) |
| result | 1 | 下位 2 bit が勝敗 (0=引分、1=先手勝ち、2=後手勝ち)。上位 bit の終局理由は読まない |
| opponent | 1 | 読まない |
| 指し手 × moveNum | 6 + 4 × candidateNum | selectedMove16 (u16)、eval (i16)、candidateNum (u16)、続いて候補手 (move16 u16 + visitNum u16) × candidateNum |

move16 は [HCPE 形式](hcpe-format.md) と同じ符号化です。候補手と訪問数 (policy) は PSV に載せないので読み飛ばします。

## 出力

| PSV の項目 | 値 |
|---|---|
| sfen | 指す前の局面 |
| score | その手の eval (手番側視点)。値の変換はしない |
| move16 | selectedMove16 を YaneuraOu 形式に変換した値 |
| game_ply | 開始局面を 1 とした手数 (hcpe3 は開始局面の手数を持たない) |
| game_result | result を手番側視点 (1=勝ち、-1=負け、0=引分) に変換した値 |

candidateNum が 0 の手 (探索していない手。dlshogi の学習でも使わない) の局面は、
局面の復元には使いますが出力しません。件数は終了時の `Skipped unsearched` に出ます。

## 出力の扱いとエラー

入力パスは辞書順に並べ、各ファイルの対局順・指し手順で出力します。入力は最大 128 局・
合計 16,384 局面を目安とするバッチで読み、保持するデータ量は総ファイルサイズに比例しません。

出力には全入力と別の通常ファイルを指定してください。同一パス、入力を指す hardlink、
symlink の出力パスは拒否します。出力先と同じディレクトリの一時ファイル (`.hcpe3_to_psv_*.tmp`)
へ書き、変換と書込みがすべて成功した後で置換します。既存出力の mode bits の扱いは
[`pack_to_psv`](pack_to_psv.md) と同じです。

途切れた対局、HCP の復号失敗、合法手でない指し手、勝敗値 3、I/O エラーでは非 0 で終了し、
既存出力を保持します。エラーがある対局を飛ばすモードはありません。
