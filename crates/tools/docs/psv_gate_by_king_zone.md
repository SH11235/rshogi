# psv_gate_by_king_zone — 入玉ドメインのゲート生成

行対応する depth9 / DL ラベルを入玉ドメインで合成するか、同じ判定を bitmap として
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
  `ceil(records / 8)` byte に厳密一致する。

trainer は mask bit 1 の行では base（depth9）score を温存し、bit 0 の行だけ score
sidecar で override する。

## merge モード

```bash
psv_gate_by_king_zone \
  --d9 depth9.psv --dl dl.psv --out merged.psv \
  --tiers entered,advancing [--d9-abs-max 3000]
```

2 本を lockstep で読み、対象行だけ DL 側の score を depth9 score に差し替える。
全行で sfen / move / ply / result が一致することを検査し、出力サイズが入力サイズと
一致することも完了時に検証する。

## mask モード

```bash
psv_gate_by_king_zone \
  --input depth9.psv --out-mask gate.mask \
  --tiers entered,advancing [--d9-abs-max 3000]
```

depth9 base を 1 本だけ読み、対象行を bitmap の bit 1 として出力する。同一入力と同一
オプションからは常に bit 一致する。`--d9-abs-max N` 指定時は `|score| < N` も満たす
行だけが対象になる（境界の `|score| == N` は対象外）。

## tier

- `entered`: 先手玉 rank 0..=2、または後手玉 rank 6..=8
- `advancing`: `entered` ではなく、いずれかの玉が rank 3..=5
- `normal`: 上記以外（ゲート指定不可）

両モードは排他で、merge 用と mask 用の引数を混在させるとエラーになる。終了時には
entered / advancing / normal と gated の件数・割合を同じ形式で表示する。
