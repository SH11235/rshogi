# mate_bench

df-pn 詰み探索 (`rshogi_core::mate::dfpn`、USI の `go mate` と同じ solver) を局面集で解き、
結果の内訳・探索量・時間を集計する。詰み探索の改良を同じ局面集で比べるために使う。

## 使い方

```bash
# 詰将棋の局面集 (最短 11 手で詰む問題) から 1000 行おきに選んで解き、正解と照合する
cargo run --release -p tools --bin mate_bench -- \
  --input $SHOGI_DATA/tsume/yaneuraou-2020/mate11.sfen \
  --stride 1000 --offset 0 --expect-mate-len 11 \
  --nodes 1000000 --jobs 8 --out mate11.tsv

# 実戦局面を手番そのままと、手番を反転した局面 (詰めろ判定の使い方) の両方で解く
cargo run --release -p tools --bin mate_bench -- \
  --input positions.sfen --flip --time-ms 1000 --out real.tsv

# 作品集 (各行 `<SFEN><TAB><作意手数>`) を最短手順モードで解き、作意手数と照合する
cargo run --release -p tools --bin mate_bench -- \
  --input works.tsv --shortest tsume --time-ms 60000 --out works-result.tsv
```

## オプション

| オプション | 説明 | 既定 |
|---|---|---|
| `--input` | 入力 SFEN ファイル (1 行 1 局面。`sfen ` 接頭辞は外し、空行と `#` で始まる行は読み飛ばす)。行の SFEN の後に `<TAB><手数>` があれば、その局面の正解の詰み手数 (作意手数) として照合に使う | 必須 |
| `--stride` / `--offset` | 有効な行のうち `index % stride == offset` の行だけを使う。offset を変えると重ならない局面集を作れる (調整用と評価用) | 1 / 0 |
| `--limit` | 解く局面数の上限 (反転局面を含む)。0 は無制限 | 0 |
| `--flip` | 手番を反転した局面も解く。元の手番側が王手されている局面は反転しない | なし |
| `--nodes` | 1 局面あたりのノード数の上限。0 は無制限 | 0 |
| `--time-ms` | 1 局面あたりの時間の上限 (ミリ秒)。0 は無制限 | 0 |
| `--hash-mb` | 置換表のサイズ (MB、スレッドごと) | 64 |
| `--mode` | 探索の進め方 (`Speed` / `Power`。USI の `MateMode` と同じ) | Power |
| `--shortest` | 最短手順を求める (USI の `MateShortest` と同じ)。`tsume` は最短手順 (詰将棋ルール: 最短手数、受け方は最長の応手、無駄合いは数えない、同手数なら駒余りの無い方)、`all` は最短手順 (全応手: 合駒をすべて受け方の応手として数える)。省略すると通常 (詰むかどうかを速く調べ、手順は最短とは限らない)。規則の詳細は rshogi-usi の README の「最短手順」 | なし (通常) |
| `--expect-mate-len` | 正解の詰み手数 (入力の行に手数があればその行はそちらを使う)。正解の分かっている局面で、不詰・非対応の結果を誤りとして数え、1 件でもあれば終了コード 1 で終わる。この手数より短い詰み手順は「要確認」として並べる (局面集の正解が成り立たない問題もあるため。例えば不成で詰む手を見落とした生成)。`--shortest` では、正解より長い手順も「要確認」として並べる (無駄合いの判定の違いなどで、作意手数と合わない局面) | なし |
| `--jobs` | 局面を並列に解くスレッド数。各スレッドが自分の置換表を持つ。時間を比べるときは 1 にする | 1 |
| `--threads` | 1 局面を解くスレッド数。スレッドどうしで置換表を共有し、最初の 1 万ノードは 1 スレッドで解く。2 以上ではノード数がスレッドの進み具合で変わり、決定的でない。`--jobs` × `--threads` が CPU のスレッド数を超えると遅くなる | 1 |
| `--out` | 局面ごとの結果を書く TSV (入力と同じファイルは拒否する) | なし |

## 出力

標準出力:

- 結果の内訳 (`mate` / `nomate` / `unknown` / `unsupported`)。`unsupported` は受け方の玉が無い局面と、手番でない側が王手されている局面 (攻め方の玉が無い詰将棋は解く)
- ノード数の合計と、全スレッド合計の nodes/s
- 判定できた局面 (`mate` / `nomate`) のノード数と時間の分位点 (中央値・90%・99%・最大)。分位点は対数ヒストグラム (1 オクターブを 8 区間) の区間の上端で近似する
- ノード予算 (1,000 / 10,000 / 100,000 / 1,000,000 / 10,000,000) ごとの判定できた局面数
- 正解の詰み手数の分かっている局面があれば、正解との照合 (誤り・正解より短い・`--shortest` では正解より長い・正解と同じ手数の数) と、誤り・要確認の局面と手順 (合わせて先頭 50 件)。`--flip` で反転した局面は正解の手数を持たない

TSV (`--out`、入力順) の列:

| 列 | 内容 |
|---|---|
| `sfen` | 解いた局面 (反転局面は反転後の SFEN) |
| `result` | `mate` / `nomate` / `unknown` / `unsupported` |
| `pv_len` | 詰み手順の手数 (詰み以外は 0) |
| `nodes` | 探索したノード数 (詰み手順の復元を含む) |
| `micros` | 解くのにかかった時間 (μs) |
| `pv` | 詰み手順 (USI 表記、空白区切り) |
| `expect_len` | 正解の詰み手数 (無ければ空) |

入力は読みながら解くので、メモリは局面数に依存しない (解いている途中と書き出し待ちの局面は `jobs × 64` 件まで)。

## wasm で測る

ブラウザに組み込むとき (wasm) の速さは、rshogi-core の example `mate_bench_wasm` を WASI 向けにビルドして
Node.js 20 以降 (V8。Chromium と同じエンジン) で動かして測る。`mate_bench` は wasm にビルドできない依存 (tokio) を
持つため、rshogi-core だけに依存する 1 スレッドの最小版を別に置いている。局面の選び方 (`--stride` `--offset`
`--limit` `--flip`、行末の `<TAB><作意手数>` は読めることだけ確かめて使わない) と `--nodes` `--hash-mb` `--mode` `--shortest` は `mate_bench` と同じ。
時間の上限・分位点・正解との照合は無い。

```bash
# rshogi のリポジトリの直下で
rustup target add wasm32-wasip1
cargo build --release -p rshogi-core --example mate_bench_wasm --target wasm32-wasip1

# wasm から見えるのはカレントディレクトリだけなので、局面集のあるディレクトリで相対パスを渡す
cd $SHOGI_DATA/tsume
node /path/to/rshogi/scripts/run_wasi.mjs \
  /path/to/rshogi/target/wasm32-wasip1/release/examples/mate_bench_wasm.wasm \
  --input yaneuraou-2020/mate11.sfen --stride 1000 --nodes 2000000 --out mate11-wasm.tsv
```

- 出力は `局面` と `nodes` の 2 行と、`--out` の TSV (`sfen` `result` `pv_len` `nodes` `micros` `pv`)。
  同じ `--hash-mb` `--mode` `--shortest` なら、ノード数・判定・手順は native の `mate_bench` と一致する
  (探索深さの上限が違うので、深さ 1024 に達する局面を除く。その局面では判定も変わりうる)。
- `--out` は存在しないファイルにする (wasm ではパスの別名を見分けられないので、入力を上書きしないよう既存のファイルには書かない)。
- 引数の誤りは終了コード 2、実行中の失敗は 1 (それまでの局面の TSV は残る)。

探索は手数ごとに再帰するので、wasm ではスタックが 2 種類要る: wasm のメモリ上のスタック (Rust の既定 1 MB) と、
実行エンジンの呼び出しスタック (Node.js のメインスレッドはおよそ 1 MB、Chromium の Worker はおよそ 770 KB)。
1 手あたり前者を約 160 byte、後者を約 340〜400 byte (ビルドで変わる) 使うので、wasm では探索深さの上限を
1024 手 (native は 2048 手) にして後者を約 400 KB 以下に抑えている。呼び出しスタックが足りないと `RangeError: Maximum call stack size exceeded`
で探索が止まる (`node --stack-size=<KB>` で上限を変えて確かめられる)。wasm のメモリ上のスタックが足りないと
`RuntimeError: memory access out of bounds` で止まる (リンカの `-zstack-size=<byte>` で大きさを変えて確かめられる)。
最短手順モード (`--shortest`) では、手順を選ぶ再帰と、`tsume` の無駄合いの判定で別の局面を入れ子に解く分だけ、
どちらのスタックも多く使う。
