# psv_entered

`psv_entered` は、入玉の行 (どちらかの玉が敵陣三段) だけを別のラベルで付け直した教師を作るツールです。
入玉の行を抜き出して別のモデルで付け直し、元の PSV に戻して [dual-label PSV](psv_dual_label.md) を書くまでを 2 つの subcommand で行います。

```text
元の PSV ──extract──▶ 入玉の行 ──(rescore_psv 等で付け直す)──▶ 付け直した入玉の行
   │                                                              │
   └──────────────────────── merge-dual ◀─────────────────────────┘
                                 │
                                 ▼
                          dual-label PSV
```

入玉の判定は [`psv_gate_by_king_zone`](psv_gate_by_king_zone.md) の `--tiers entered` と同じ
(先手玉が一〜三段目、または後手玉が七〜九段目) です。packed sfen の先頭にある両玉のマスだけを読み、
局面を組み立てないので、数百億行でも読み書きの速さで処理できます。盤面の残りは検査しません
(玉のマスが 81 以上か、両玉が同じマスのときだけ失敗します)。

## extract: 入玉の行を抜き出す

```bash
cargo run -p tools --release --bin psv_entered -- extract \
  --input material.psv \
  --out material.entered.psv
```

入玉の行を 40 byte のまま入力順に書き出します。終わると `records=` (入力の行数) と `entered=` (書いた行数) を表示します。

## merge-dual: 付け直した入玉の行を戻して dual-label PSV を書く

```bash
cargo run -p tools --release --bin psv_entered -- merge-dual \
  --base material.psv \
  --sparse relabeled/material.entered.psv \
  --out material.dual.psv \
  [--dl-scores material.dl.i16] \
  [--out-mask material.entered.bits]
```

元の PSV を 1 回読み、行ごとに次を書きます。

| offset | 中身 |
|---|---|
| 0-31, 36-38 | 元の行のまま |
| 32-33 (base score) | 入玉の行は `--sparse` の次の 1 行の score、それ以外は元の score |
| 34-35 (DL score) | `--dl-scores` の値。省略したときは元の行の score |
| 39 | 入玉の行は 1、それ以外は 0 |

元の PSV が空 (0 行) なら失敗します。終わると `records=`・`entered=` と、上書きした元の move16・padding が 0 でなかった行数
(`overwritten_nonzero_move16=`・`overwritten_nonzero_padding=`) を表示します。元の PSV に dual-label PSV を渡して
`--dl-scores` を省くと offset 34 の DL score が元の score で上書きされるので、この数で気付けます。

`psv_dual_label embed` に「入玉の行の score を差し替えた PSV」「DL score の sidecar」「入玉の bitmap」を渡したときと同じバイト列になります。

- `--sparse` は `extract` の出力を付け直したもので、行順はそのままにします。score 以外 (sfen・move16・gamePly・game_result) が
  元の入玉の行と順に一致しなければ中断し、行数が元の入玉の行数と合わなければ失敗します。
  付け直しには score だけを書き換えるモード (`rescore_psv` の ONNX・内蔵 NNUE での score の付け直しなど) を使います。
  move16 を最善手に書き換える・局面を葉に置き換えるモードで付け直した行は、一致しないので受け付けません。
- `--dl-scores` は i16 LE × 行数の sidecar で、サイズが合わなければ失敗します。元の score が既に DL の値なら省略します。
- `--out-mask` は入玉の行の LSB-first bitmap (`psv_dual_label` の `entered.bits` と同じ形) です。
- 出力は `<out>.partial` に書き、すべて書けてから名前を変えて公開します。`--out-mask` を先に、`--out` を最後に公開するので、
  途中で失敗しても `--out` は公開されません。書く途中で失敗したときは `.partial` を消します。
  出力が入力と同じ実体を指すとき、出力どうしが同じ実体を指すときは、書く前に拒否します。
