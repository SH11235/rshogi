# psv_gate_by_king_zone — 入玉ドメインのゲート生成

行対応する base / override PSV を入玉ドメインで合成するか、同じ判定を bitmap として
出力するストリーミングツール。固定サイズのチャンクを読み、局面 classify を Rayon で
並列化してから元の行順で書き出す。ピークメモリは入力件数に依存しない。

## 行対応の不変条件とファイル契約

base PSV の行順と行数が唯一の対応キーであり、sidecar / bitmap は行を追加・削除・
並べ替えしない。入力 PSV のサイズが 40 byte の倍数でない場合や、merge 入力の行数・
非 score フィールドが一致しない場合は fail-closed で停止する。出力がいずれかの入力と
同じファイル実体を指す場合も、出力を作成・truncate する前に拒否する。

- score sidecar: little-endian i16 × records。record `i` は offset `i * 2`。サイズは
  `records * 2` byte に厳密一致する。
- mask bitmap: byte `j` の bit `k`（`k=0` が LSB）は record `j*8+k` に対応する。
  bit 1 はゲート対象。最終 partial byte の余り bit は 0。サイズは
  `ceil(records / 8)` byte に厳密一致する。空の入力からは空の mask を生成する。
  チャンク境界を跨いでも bit の並びは連続し、padding はファイル末尾だけに入る。

base はゲート該当行で採用され、学習側では温存される score 側である。override は
非該当行で採用される score 側で、学習側の `--score-override` に対応する。trainer は
mask bit 1 の行では base score を温存し、bit 0 の行だけ score sidecar で override
する。

## merge モード

```bash
psv_gate_by_king_zone \
  --base base.psv --override override.psv --out merged.psv \
  --tiers entered,advancing [--base-score-abs-max 3000]
```

2 本を lockstep で読み、ゲート該当行では base 側、非該当行では override 側の score
を採用する。非 score フィールドは override 側から供給する。全行で sfen / move / ply /
result が一致することを検査し、出力サイズが入力サイズと一致することも完了時に検証
する。たとえば depth9 ラベルを base、DL リスコアを override として指定できる。

## mask モード

```bash
psv_gate_by_king_zone \
  --base base.psv --out-mask gate.mask \
  --tiers entered,advancing [--base-score-abs-max 3000]
```

base PSV を 1 本だけ読み、対象行を bitmap の bit 1 として出力する。同一入力と同一
オプションからは常に bit 一致する。`--base-score-abs-max N` 指定時は
`|base score| < N` も満たす行だけが対象になる（境界の `|base score| == N` は対象外）。

## tier

- `entered`: 先手玉 rank 0..=2、または後手玉 rank 6..=8
- `advancing`: `entered` ではなく、いずれかの玉が rank 3..=5
- `normal`: 上記以外（ゲート指定不可）

両モードは排他で、`--override` / `--out` と `--out-mask` を混在させるとエラーに
なる。終了時には entered / advancing / normal と gated の件数・割合を同じ形式で
表示する。
