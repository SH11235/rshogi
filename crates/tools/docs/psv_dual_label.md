# psv_dual_label

`psv_dual_label` は通常 PSV の score sidecar への退避、行対応 sidecar からの
dual-label PSV 生成、逆抽出、fail-closed 検証を行うツールです。dual-label PSV は dedup / shuffle で base score、
DL score、entered gate bit を一つの 40 byte レコードとして原子的に運ぶための形式です。

## レコード形式

1 レコードは通常 PSV と同じ 40 byte 固定長です。整数は little-endian です。

| offset | 通常 PSV | dual-label PSV |
|---|---|---|
| 0-31 | packed sfen | 同じ |
| 32-33 | score (i16 LE) | **base score** |
| 34-35 | move16 | **DL score (i16 LE)** |
| 36-37 | gamePly | 同じ |
| 38 | game_result | 同じ |
| 39 | padding（生成系によっては非ゼロ） | **bit0 = entered gate bit** (1 = 学習側で base を温存)、**bit1-7 は必ず 0** |

入力 sidecar の契約は次のとおりです。

- `dl.i16`: i16 LE × records。サイズは `records × 2 byte` と厳密一致する。
- `entered.bits`: LSB-first bitmap。byte `j` の bit `k` は record
  `j * 8 + k` に対応し、bit 1 が entered。サイズは `ceil(records / 8) byte` と
  厳密一致し、最終 byte の未使用 bit は 0。

学習側 trainer が all 相当で読む場合は全レコードの DL score を使います。gated 相当では
entered gate bit が 1 のレコードだけ base score を温存し、それ以外は DL score を使います。
offset 34-35 は dual-label PSV では指し手ではないため、通常 PSV として読ませてはいけません。

## dump-scores: 通常 PSV の score を退避する

```bash
cargo run -p tools --release --bin psv_dual_label -- dump-scores \
  --base plain.psv \
  --out-scores dl.i16
```

通常 PSV の各 40 byte レコードから score 列 (offset 32-33) を little-endian の 2 byte の
まま入力順に転記します。出力サイズは `records × 2 byte` です。局面の decode は行いません。
空の通常 PSV も受理し、空の sidecar を生成します。

`extract --out-scores` は dual-label PSV の DL score 列 (offset 34-35、通常 PSV の move16
列) を抽出する別の操作です。通常 PSV の score 列を退避するときは `dump-scores` を使います。

## embed: sidecar を埋め込む

```bash
cargo run -p tools --release --bin psv_dual_label -- embed \
  --base base.psv \
  --scores dl.i16 \
  --mask entered.bits \
  --out dual.psv
```

base のサイズ、sidecar の厳密なサイズ、mask 最終 byte の未使用 bit を出力作成前に検査します。
base の move16 は DL score、padding は entered gate bit（bit0 のみ）で上書きされます。
生成系によっては base の padding が非ゼロの場合がありますが、embed は拒否せず上書きします。
非ゼロ move16 と非ゼロ padding を上書きした件数は、それぞれ
`overwritten_nonzero_move16` と `overwritten_nonzero_padding` として表示されます。

## extract: sidecar へ戻す

```bash
cargo run -p tools --release --bin psv_dual_label -- extract \
  --dual dual.psv \
  --out-base base.psv \
  --out-scores dl.i16 \
  --out-mask entered.bits
```

出力は一つ以上を指定し、必要なものだけ省略できます。一回の streaming 走査で同時生成します。
`--out-base` は move16 と padding を 0 に戻し、base score と他フィールドを保持します。
`--out-mask` の最終 byte の未使用 bit は常に 0 です。reserved bit (padding bit1-7) が
立っている入力は行番号付きで拒否します。
mask は入力順に bit を詰め、チャンク境界に padding を挟みません。末尾の部分 byte と
flush が正常に完了した後で、既存の fsync・サイズ検算・publish へ進みます。

`embed` に渡した base の move16 と padding が全行 0 なら、続けて `extract` した
`base.psv`、`dl.i16`、`entered.bits` は元の入力と bit 一致します。

## validate: fail-closed 検証

```bash
cargo run -p tools --release --bin psv_dual_label -- validate \
  --dual dual.psv

# 局面 decode を決定的な等間隔 100 万件に限定
cargo run -p tools --release --bin psv_dual_label -- validate \
  --dual dual.psv --sample 1000000
```

次を検証し、一つでも違反があれば非ゼロで終了します。統計は成否にかかわらず stdout に
出力します。

- ファイルサイズが 40 byte の倍数で、padding bit1-7 が全行 0。
- entered gate bit が packed sfen 由来の判定と一致する。entered は
  「先手玉 rank ≤ 2 または後手玉 rank ≥ 6」。
- DL score の絶対値が `--dl-abs-max`（既定 32000）以下。
- DL score の 16 bit を通常 PSV の move16 と解釈したとき、局面の合法手になる割合が
  `--max-move-like-frac`（既定 0.05）以下。0 を指定すると move-like 行を 1 行も許容しない。

`--sample N` は entered 整合と move-like 判定だけを先頭固定の決定的な等間隔最大 N 件へ
限定します。サイズ、reserved bit、DL range は常に全件を走査します。

## 安全性とメモリ

各処理は固定上限の chunk で streaming し、ピークメモリは入力レコード数に依存しません。
出力がいずれかの入力と同じパス・hardlink を指す場合や symlink の場合は、truncate 前に
拒否します。extract の複数出力も同じ実体を指定できません。処理完了時には全出力のサイズを
契約値と照合します。
空 (0 レコード) の入力は embed / extract とも拒否します (validate が不正とする空の
dual PSV を成功として publish しない)。
`dump-scores` / `embed` / `extract` は同じディレクトリの `<出力>.partial` に書き、全検査・fsync・サイズ検算の
成功後だけ最終パスへ publish し、publish 後に親ディレクトリを sync します (電源断でも
publish 済み出力が空・切り詰め状態に巻き戻らない)。publish 前にエラーになった場合は既存の最終パスを変更せず、
`.partial` を削除します。複数出力の publish 途中で後続の rename に失敗した場合、先に publish 済みの
最終出力は検証済み成果物として残し、未 publish の `.partial` だけを削除します。エラーには publish 済み・
未 publish のパスを列挙します。
