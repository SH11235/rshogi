# generate_net_spsa_params

学習済み LayerStacks NNUE `.bin` から、net 係数を整数 delta として調整する SPSA
`.params` を生成する。NNUE を engine の compile-time architecture へロードせず header と
`.bin` layout を走査するため、bucket 数・層次元・FT 種別・PSQT／Threat の有無が異なる
LayerStacks net にも使用できる。

入力はストリーミング走査し、巨大になり得る圧縮 FT weights payload は seek で読み飛ばす。
常駐するのは decode 済み FT bias と全 bucket の FC block だけで、SHA-256 も入力全体を
保持せず計算する。`--output` が `--nnue` と同じ実体を指すパス、hardlink、symlink は
入力を truncate する前に拒否する。

## 基本形

```bash
cargo run --release -p tools --bin generate_net_spsa_params -- \
  --nnue "$SHOGI_DATA/nnue/model.bin" \
  --output spsa_params/net.params
```

既定では `out_w,out_b` を対象にし、最大 4096 parameter まで出力する。各行の `value` は
元係数ではなく engine が `base + delta` として適用する delta なので、常に `0` で始まる。
元係数は `// base=<value>` に記録される。先頭の `#` 行には net の basename、SHA-256、
architecture、bucket 数が入り、後続の焼き込み処理で入力 net の取り違えを検証できる。
続く `# spsa-format: c-end-r-end-v1` は列6/7が `c_end/r_end` であることを宣言する。
生成ファイルは追加の形式指定なしで現行 `spsa` に渡せる。

## kind と既定値

| kind | 対象 | 要素型 | delta 範囲 | `c_end` |
|---|---|---|---:|---:|
| `out_w` | bucket ごとの output weight | `i8` | ±24 | 2 |
| `out_b` | bucket ごとの output bias (`index=0`) | `i32` | ±512 | 32 |
| `ft_b` | bucket 非依存の FT bias | `i16` | ±48 | 4 |
| `l2_w` | bucket ごとの第 2 FC 層 weight | `i8` | ±24 | 2 |

既定値は、Stockfish の net SPSA で最終的に動く幅が整数 ±6 程度という実績を起点に、
終端摂動をその 1/3〜1/2、探索幅を数倍とした暫定値である。`out_b` は FV_SCALE 前の
`i32` なので i8 系の 16 倍にしている。実運用前に runbook §7.1 の ±c smoke で較正する。

FC weight の SIMD padding 列は parameter に出力しない。出力順は常に
`out_w` → `out_b` → `ft_b` → `l2_w`、その中で bucket → `.bin` flat index の昇順になる。

## 対象の絞り込みと上書き

```bash
cargo run --release -p tools --bin generate_net_spsa_params -- \
  --nnue "$SHOGI_DATA/nnue/model.bin" \
  --output spsa_params/net.params \
  --targets out_w,out_b,ft_b,l2_w \
  --select 'abs-below=8' \
  --range out_w=16 \
  --range ft_b=32 \
  --c-end out_w=1 \
  --max-params 12000
```

`--select all` は全係数、`--select zero` は現在値が 0 の係数、
`--select abs-below=<T>` は `|base| < T` の係数だけを出力する。`--range` と `--c-end` は
kind ごとに複数回指定できる。不明な kind、重複 override、上限超過、非 LayerStacks net、
末尾に余分な byte がある net は error で停止する。

## SPSA engine への受け渡し

生成した `.params` は `spsa` の初期値と engine の option spec の両方に同じファイルを渡す。

```bash
cargo run --release -p tools --bin spsa -- \
  --run-dir runs/spsa/net-example \
  --init-from spsa_params/net.params \
  --total-pairs 6400 \
  --engine-args=--spsa-net-spec \
  --engine-args=spsa_params/net.params \
  --usi-option EvalFile="$SHOGI_DATA/nnue/model.bin"
```

チューニング後の確定 delta を `.bin` に焼き込む finalize ツールは後続対応とする。
