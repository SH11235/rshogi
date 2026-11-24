# nnue_mcts_gensfen

CPU 上で NNUE を葉評価に使う MCTS の平手自己対局を生成します。各着手について、根で実際に訪問した候補手と訪問数を HCPE3 に記録します。

## 少量生成

```bash
cargo build -p tools --no-default-features --features nnue-arch --release \
  --bin nnue_mcts_gensfen --bin hcpe3_to_dl_rows

run_dir="$SHOGI_DATA/runs/rshogi-dl/nnue-mcts-smoke"
mkdir -p "$run_dir"
target/release/nnue_mcts_gensfen \
  --eval-file "$SHOGI_DATA/nnue/<model>/nn.bin" \
  --output "$run_dir/games.hcpe3" --games 16 --workers 4 --visits 512

target/release/hcpe3_to_dl_rows "$run_dir/games.hcpe3" > "$run_dir/games.rows"
```

学習用 shard は [HCPE3 からの変換](../../../python/rshogi-dl/README.md#shard-の作成と読み込み)を参照してください。

- `--visits` は1手あたりに完了させる MCTS simulation 数の上限です。最初の simulation は根の展開に使われるため、候補手の総訪問数はこの上限より少なくなります。各 worker の探索木は `--tree-memory-mb`（既定16 MiB）を上限とし、対局中に満杯になればエラーになります。
- `--sample-plies`（既定24）までは根の訪問数に比例して着手を選び、その後は MCTS が返した最善手を指します。探索で結果が確定した局面も最善手を指します。対局生成は seed と対局番号で乱数を固定し、出力は対局番号順です。
- `--max-moves`（既定512）までに決着しなければ引き分けです。平手の最終局面から算出した対局結果を保存します。
- `--value-scale`（既定600）は NNUE スコアを sigmoid で手番側期待得点に変換する試作値です。HCPE3 の eval 欄には根の探索値を同じ尺度で逆変換して保存します。`hcpe3_to_dl_rows` は value 教師に対局結果を使います。
- `--prior uniform|static|qsearch` で合法手の事前確率を選びます。既定の `uniform` は全合法手を等確率にします。`static` は各合法手の直後の NNUE 静的評価、`qsearch` はそこから最大8手の静止探索の評価を使います。終局する手の確定勝ちと確定負けにはそれぞれ 32000 cp、−32000 cp を使い、非終局の評価はこの範囲の内側に収めます。
- `--prior-temperature <cp>`（既定100、正の有限値）は `static` と `qsearch` の softmax 温度です。小さいほど高評価の手に事前確率が集中します。葉の値はどのモードでも `--value-scale` で変換した NNUE 静的評価です。
- 評価関数のレシピに合わせて `--fv-scale` を指定できます。LayerStacks は `--ls-bucket-mode` が必須です。進行度方式では学習時の `--ls-progress-buckets` と、複数 bucket の場合は `--ls-progress-coeff` も指定してください。
- 出力先の親ディレクトリは事前に作成してください。出力ファイルは全局の書き込みが成功してから公開し、既存ファイルは上書きしません。
