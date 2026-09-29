# LayerStacks 計測用 probe

同一 binary のまま FT の配置と子ノードの EvalHash prefetch を切り替える。
どちらも `usi` の option 一覧には出さず、`setoption` でのみ受け付ける。

```text
setoption name ProbeFtLargePages value true
setoption name ProbeEvalHashPrefetch value 2
isready
```

- `ProbeFtLargePages`: bool、既定 `false`。net を読む前に設定する。
  Windows では LayerStacks FT の i16 重みだけに `MEM_LARGE_PAGES` を試みる。
  TT と同じ権限処理を使用し、失敗時は従来の heap に戻す。
  `true` のロード時に `info string ft_alloc=large_pages|heap bytes=<n>` を出す。
  `bytes` は重みの論理バイト数（OS のページ切り上げ前）。既にロードした net は移動しない。
  static / universal の両方と、`prepacked-nnue` の FT に対応する。
  Windows の prepacked は `true` 時だけ mapping から私有領域へコピーする。
  Linux の THP・共有メモリ処理は従来どおり。
- `ProbeEvalHashPrefetch`: `0` / `1` / `2`、既定 `0`。
  `0` は現行、`1` は全ての子で停止、`2` は qsearch と ProbCut が作る子だけ停止する。
  通常探索が作った子から qsearch に入る場合は prefetch する。
  EvalHash の probe / store、TT の prefetch、評価結果は変更しない。
  不正な値は設定を変えない。着手経路への追加は Relaxed load と分岐 1 個。

L1/L2 の重みは対象外。bucket ごとの小さな配列を個別に Large Pages 化すると、
ページ単位の切り上げで確保量が大きく増えるため、FT 単体の配置を比較する。
`WeightBox::make_mut` は私有 Large Pages をそのまま更新し、read-only backing は
従来どおり heap へ複製する。clone の複製先も heap になる。

uProf では `nnue::probe_copy` の次の関数名で accumulator のコピーを区別できる。

| 関数 | コピー元と先 |
| --- | --- |
| `finny_entry_to_accumulator` | Finny entry → accumulator |
| `one_move_prev_to_current` | 1 手差分の prev → current（AVX-512 のコピーを含む） |
| `forward_source_clone` | static LayerStacks の forward source clone |
| `forward_source_to_current` | forward source → current |
| `null_move_child_copy` | null move / pass の空差分の子 |

関数は `#[inline(never)]` とし、コピー後の異なる `black_box` タグで同一関数の
畳み込みと memcpy への末尾呼び出しを避ける。コピー内容は変えないが、関数呼び出しと
タグのコストが加わるため、この binary 自体を最適化の採否根拠にはしない。
シンボルを残して調べるときは `--profile production-profiling` を使う。

単体テスト `probe_modes_preserve_evaluation_and_fixed_depth_search` は、FT の off / on /
強制 heap fallback と prefetch の全 3 mode の組合せについて、静的評価・refresh / 差分 /
forward / null move と depth 3 の bestmove・score・nodes・PV の一致を検証する。
Large Pages の成功自体は OS の権限と空きメモリに依存するため必須にしない。
