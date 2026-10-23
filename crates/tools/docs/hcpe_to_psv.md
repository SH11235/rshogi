# hcpe_to_psv

入力の [38 byte HCPE形式](hcpe-format.md) は共通です。出力PSVのpaddingは0とし、入力HCPEのdummyは引き継ぎません。

`hcpe_to_psv` は、hcpe（cshogi HuffmanCodedPosAndEval, 38B/レコード）を
PSV（YaneuraOu PackedSfenValue, 40B/レコード）へ変換するツールです。

外部公開の hcpe 教師/検証プール（例: dlshogi 系で標準の floodgate 検証局面）を、
nnue-train の `--data` / `--test-data` が読む PSV 形式にするのが主用途です。

## 使用例

```bash
# 単一ファイル
cargo run --release -p tools --bin hcpe_to_psv -- \
  --input "$SHOGI_DATA/validation/floodgate_hcpe_yamaoka/floodgate.hcpe" \
  --output "$SHOGI_DATA/validation/floodgate_hcpe_yamaoka/floodgate.psv"

# ディレクトリ内の .hcpe を一括変換（パスのソート順で連結）
cargo run --release -p tools --bin hcpe_to_psv -- \
  --input-dir "$SHOGI_DATA/teachers/pool" --output out.psv
```

## オプション

| オプション | 説明 | 既定値 |
|---|---|---:|
| `--input <FILES>` | 入力 hcpe（カンマ区切りで複数可）。`--input-dir` と排他 | - |
| `--input-dir <DIR>` | 入力ディレクトリ。`--pattern` と組み合わせる | - |
| `--pattern <GLOB>` | `--input-dir` 使用時の glob パターン | `*.hcpe` |
| `--output <FILE>` | 出力 PSV ファイル | - |
| `--chunk <N>` | 並列変換のチャンクサイズ（レコード数） | `65536` |
| `--threads <N>` | rayon スレッド数（0 = 自動） | `0` |

## フィールド対応

| hcpe | PSV | 変換 |
|---|---|---|
| `hcp` (HuffmanCodedPos, Apery/cshogi 形式) | `sfen` (PackedSfen, YaneuraOu 形式) | Huffman テーブルが異なるため `unpack_hcp_to_parts` → `pack_sfen_from_parts` で直接再パック |
| `eval` (手番側視点 cp) | `score` | そのままコピー（視点は両形式で同一規約）。詰み帯の数値表現は生成系依存で、値変換は行わない |
| `bestMove16` (cshogi Move16) | `move16` (**実 YaneuraOu Move16**: bit14=駒打ち/bit15=成り) | `hcpe_move16_to_psv` で再エンコード。旧リポジトリ内部形式 (B) とは別形式 |
| `gameResult` (絶対視点 0=draw / 1=black_win / 2=white_win) | `game_result` (手番側視点 1=win / -1=loss / 0=draw) | 手番で符号を決定 |
| （なし） | `game_ply` | hcpe に手数情報が無いため 1 固定 |

## 挙動

- 入力順（複数ファイルはパスのソート順）を保持して連結出力する。決定的。
- チャンク読み + rayon 並列。ピークメモリはチャンクサイズのみに依存し入力サイズに非依存。
  実測 (856,923 局面, 32.5MB): 0.15 秒 / ピーク RSS 10.4MB（逐次 Position 経由の初期実装比 24 倍）。
- `.partial` へ書き、正常完了時のみ最終パスへ rename する（中断時の途中書き PSV はバイト長が
  40 の倍数になり下流チェックをすり抜けるため、残さない）。
- Linux/macOS と Windows の両方に対応する。入力と出力、または入力と `.partial` が
  同じファイル実体なら拒否する。パス文字列が異なる hardlink も同一ファイルとして検出するため、
  出力作成による入力の truncate を防げる。
- 壊れたレコード（hcp デコード失敗 / bestMove16 の駒打ち駒種不正 / gameResult が 0/1/2 以外）は
  skip して種別ごとに件数を summary に出す。`bestMove16 == 0`（終局直前レコード等、指し手なし）も
  skip するが `No bestmove` として別カウントする。
- 入力サイズが 38 の倍数でないファイルは即エラー。
