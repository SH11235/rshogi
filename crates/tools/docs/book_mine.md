# book_mine

`book_mine` は YANEURAOU-DB2016 テキスト定跡 `.db` を自エンジンの探索で book 外へ展開するツールです。YaneuraOu-ScriptCollection の BookMiner (`peta_next` → 掘る → peta_shock → 周回) に相当する処理を 3 つのサブコマンドで行います。

| サブコマンド | 処理 |
|---|---|
| `frontier` | book と開始局面から、次に掘る末端局面を列挙する |
| `expand` | 末端局面を MultiPV 探索し、局面と候補手を book に追加する |
| `run` | `frontier` → `expand` → 逆伝播 (`book_backprop` と同じ処理) を周回する |

局面キーは SFEN の ply を除いた盤面・手番・持ち駒の 3 フィールドです。直接キーで見つからない局面は先後反転キーでも引き、反転キーでヒットした局面の手は反転座標系として扱います (`book_backprop` と同じ規約)。

## frontier

```bash
cargo run -p tools --release --bin book_mine -- frontier \
  --book in.db \
  --roots roots.txt \
  --side both \
  --window 100 \
  --out leaves.txt \
  --report frontier.md
```

| オプション | 既定 | 説明 |
|---|---:|---|
| `--book <path>` | 必須 | 入力 `.db` |
| `--roots <path>` | 必須 | 開始局面。1 行 1 局面で `sfen <sfen> [moves ...]` または `startpos [moves ...]`。先頭の `position` は省略可。空行と `#` 行は無視 |
| `--side <black\|white\|both>` | `both` | 採掘側 (定跡を作る側の手番)。`both` は先手用と後手用を両方辿って和集合を取る |
| `--window <cp>` | `100` | 相手側局面で `value >= best - window` の手を辿る |
| `--opp-min-count <N>` | `0` | 相手側局面で `count >= N` の手も value に関係なく辿る。`0` で無効 |
| `--own-eps <cp>` | `0` | 採掘側局面で `value >= best - own_eps` の手を辿る。`0` は best のみ (同値は全部) |
| `--max-ply <N>` | `260` | 局面の ply 上限。これを超える子局面は辿らず末端にもしない |
| `--max-depth <N>` | なし | roots からの辿り手数の上限 |
| `--max-leaves <N>` | なし | 出力件数上限。roots からの手数昇順、同値は key 昇順で打ち切る |
| `--out <path>` | 必須 | 末端局面リスト (`sfen <sfen>` を 1 行 1 局面) |
| `--report <path>` | なし | Markdown レポート |

### 辿り方

roots から BFS で辿ります。訪問済みの局面 (反転キーも同一視) は再訪しないため、千日手などの循環でも停止します。`--side both` の場合は先手用と後手用の BFS を別々に行い、末端の和集合を取ります。

book 内局面では、合法な候補手の `value` の最大値を `best` として次の手を辿ります。

| 局面の手番 | 辿る手 |
|---|---|
| 採掘側 | `value >= best - own_eps` |
| 相手側 | `value >= best - window`、または `--opp-min-count` 以上の `count` を持つ手 |

非合法な手 (book の破損) は `best` を決める前に除き、stderr に警告します。非合法手の value が大きくても、合法手の中の best を辿ります。

局面の全候補手が `value=0 depth=0` (`book_from_csa` のラベル無し出力) の局面は未探索局面とみなします。この判定は局面単位だけで行います。1 手単位の `value=0 depth=0` は `book_rescore` の静的評価 (depth 0) や評価値ちょうど 0 と区別できないため、探索済み局面の中の value 0 の手も通常の値として `best` / window / own-eps の計算に使います。

末端は次の 2 種類です。

- 辿った手を指した先の局面が book 内に無い
- book 内の未探索局面

roots が book 外なら root 自身が深さ 0 の末端になります。

### 出力

`--out` には末端局面を `sfen <sfen>` の形式で 1 行ずつ書きます。同じ局面 (反転キーも同一視) は 1 回だけ、roots からの手数昇順 → key 昇順の決定的な順序です。

どちらかの玉が敵陣 3 段内にある末端には入玉フラグを付け、`<out>.entered` に同じ形式で書き出します (`--out` にも含まれます)。入玉局面の探索ラベルは過大評価しやすいため、後段でプレイアウト評価などに回す用途を想定しています。

`--report` には roots 数、辿った book 局面数、末端数 (`--max-leaves` 適用前後)、末端の種類別件数、未探索局面の末端数 (`--max-leaves` 適用前)、入玉フラグ件数、非合法手数、`--max-ply` / `--max-depth` で打ち切った手数、深さ分布を出力します。

## expand

```bash
cargo run -p tools --release --bin book_mine -- expand \
  --book in.db \
  --out expanded.db \
  --leaves leaves.txt \
  --engine /path/to/usi-engine \
  --engine-option "EvalFile=$SHOGI_DATA/nnue/model.bin" \
  --go "nodes 10000000" \
  --parallel 12 \
  --multipv 4 \
  --journal expand.jsonl \
  --resume \
  --report expand.md
```

| オプション | 既定 | 説明 |
|---|---:|---|
| `--book <path>` | 必須 | 入力 `.db` |
| `--out <path>` | 必須 | 出力 `.db` |
| `--leaves <path>` | 必須 | `frontier` の出力 (形式は `--roots` と同じ) |
| `--engine <path>` | 必須 | USI エンジンの実行ファイル |
| `--engine-option <k=v>` | | USI option。複数指定可 |
| `--go "<args>"` | `nodes 100000` | `go` 引数 |
| `--parallel <N>` | `1` | 並列エンジン数 |
| `--multipv <K>` | `4` | 初回 MultiPV 数 |
| `--multipv-delta <cp>` | `100` | 1 位と K 位の差がこの値以内なら K を増やして再探索する。`0` で無効 |
| `--multipv-max <K>` | `16` | MultiPV 数の上限。合法手数も上限になる |
| `--extend-ply <N>` | `0` | 展開した局面から最善手を N 手辿った局面も展開する |
| `--journal <path.jsonl>` | 必須 | 探索結果を追記する journal |
| `--resume` | | `--journal` 内の現設定一致レコードを再利用する |
| `--report <path>` | なし | Markdown レポート |

エンジンは各 worker で `Threads=1` / `Hash=256` を既定に起動し、`--engine-option` で上書きできます。`MultiPV` は探索ごとに `setoption` で設定します。MultiPV を 2 以上にしうる設定 (`--multipv` が 2 以上、または `--multipv-delta` が正で `--multipv-max` が `--multipv` より大きい) で、エンジンが `usi` 応答で `MultiPV` オプションを広告しない場合はエラーにします。MultiPV 非対応エンジンでは `--multipv 1 --multipv-delta 0` (または `--multipv-max 1`) を指定してください。

### 探索

各末端局面をその局面のまま MultiPV 探索します (子局面の探索はしません)。MultiPV 値は同一探索内の比較なので、ノード内でスケールが揃います。

1. `MultiPV = min(--multipv, 上限)` で探索する。上限は `--multipv-max` と合法手数の小さい方
2. `--multipv-delta` が正で、K 行揃い、K 行の value の最大と最小の差が `--multipv-delta` 以内なら、K を `--multipv` 分増やして (上限まで) 再探索する
3. 最後の探索の各 MultiPV 行を候補手にする

各行の値は次のとおりです。

| 項目 | 値 |
|---|---|
| 指し手 | その行の PV 初手 |
| `value` | その行の `score` (探索局面の手番側視点)。`score mate N` は `±(30000 - \|N\|)`、cp は `[-30000, 30000]` にクリップ |
| `depth` | その行の `depth`。欠落または 0 の場合は 1 (未探索の印 `value=0 depth=0` と取り違えないため) |
| `ponder` | その行の PV 2 手目 (子局面で合法な場合のみ。無ければ `none`) |
| `count` | `0` |

`lowerbound` / `upperbound` 付きの info 行は使わず、MultiPV 番号ごとに最後の確定行を採ります。`go nodes` などで反復の途中に打ち切られると、最終ブロックの行ごとに深さが揃わず、MultiPV 1 行目や value 最大の行がエンジンの `bestmove` と一致しないことがあります。そのため MultiPV の拡張判定は番号順でなく value の最大・最小で行い、`bestmove` は journal に別に記録します。`bestmove` の行が value 最大の行でなかった局面数は report に出します。

### book への反映

- book 外の局面は新しい局面として追加します。`sfen` 行は末端局面の ply 付き SFEN です
- book 内の局面 (反転キー一致を含む) は、既存の手の `value` / `depth` / `count` / `ponder` を**変更せず** (既存ラベルの由来を混ぜない)、まだ無い手だけを追加します。反転キーでヒットした局面には反転座標系の手として書きます
- 例外として、未探索局面 (全候補手が `value=0 depth=0`) の既存手はラベルを持たないため、**全ての**既存手に `value` / `depth` を埋めます (`count` / `ponder` は保持)。MultiPV 行にある手はその行の値を使い、MultiPV 行に無い既存手は `go <--go の引数> searchmoves <手>` でその手に限った探索を 1 手ずつ行って値を得ます (値の規約は MultiPV 行と同じ)。0/0 の手が一部だけ残ると、探索済み局面の中の値 0 の手として `frontier` や逆伝播の best になりうるためです。これにより、MultiPV の手が全て既存の未ラベル手と重なる局面も次の `frontier` で未探索局面として再列挙されません
- searchmoves 探索で `bestmove` が指定手 (か `resign`) でない場合は、エンジンが searchmoves に対応していないとみなしてエラーにします。未探索局面を含む book を展開するには searchmoves 対応のエンジンが必要です
- `bestmove` は指定手 (か `resign`) でも、指定手を PV 初手とする確定行が得られない場合 (探索量が小さすぎる、合法手が無い等) は、別のエラーにします
- 未探索局面の既存手のうち非合法な手と `none` 行はラベル付けできず、`value=0 depth=0` のまま残ります (件数を report に出します)。`run` の逆伝播はこれらを局面の best から除きます
- 合法手が無い局面は探索せず、report に記録します
- エンジンが `bestmove win` を返した宣言勝ち可能局面は、book に `win` 相当を入れず候補手も追加しません。report に mate 1 相当の値 (`29999`) で記録します。probe 側は root の宣言判定で処理されます
- エンジンが返した非合法手は追加せず、件数を report に記録します

`--extend-ply N` を指定すると、展開した局面のエンジンの `bestmove` を指した先の局面 (`bestmove` が採取した MultiPV 行に無い、または非合法な場合は、合法な行のうち value 最大の手。孤立局面を作らないため)が book 外なら、それも同様に展開します。これを N 手分繰り返します。

### journal と決定性

`--journal` は JSON Lines 形式で、探索局面ごとに 1 行 (`key` / `sfen` / `go` / `multipv` / `engine_fingerprint` / `declaration_win` / `lines` / `multipv_used` / `extensions`) を追記します。未探索局面の既存手の searchmoves 探索は `searchmove` に対象手を入れた別レコードで、局面 key と対象手の組で再利用します。`multipv` は `--multipv` / `--multipv-delta` / `--multipv-max` を並べた文字列、`engine_fingerprint` は `book_extend` と同じく `--engine` の basename、バイナリの SHA-256、`--engine-option` の正規化文字列から作ります。

`--resume` は key・`go`・`multipv`・`engine_fingerprint` が一致するレコードだけを再利用します。探索途中でエラーになった場合も、完了した探索は journal に追記済みなので `--resume` で再開できます。

出力 `.db` は探索順や worker の完了順に依存しません。局面は key 昇順、手は `value` 降順 → USI 昇順で書き出し、同じ入力と journal からは bit 一致します。出力は一時ファイルへ書き切ってから rename する atomic 書き込みです。

### report

追加局面数 (うち `--extend-ply` 由来)、手を追加した既存局面数、手の追加も値の埋め込みも無かった既存局面数、追加手数、未探索局面で値を埋めた手数と局面数、searchmoves 探索数と journal 再利用数、ラベル付けできなかった非合法な既存手数、MultiPV を拡張した局面数と拡張回数、mate 行数、`bestmove` の行が value 最大でなかった局面数、非合法手数、宣言勝ち可能局面 (一覧付き)、合法手の無い局面、探索数と journal 再利用数を出力します。

## run

```bash
cargo run -p tools --release --bin book_mine -- run \
  --book in.db \
  --out mined.db \
  --roots roots.txt \
  --side black \
  --engine /path/to/usi-engine \
  --engine-option "EvalFile=$SHOGI_DATA/nnue/model.bin" \
  --go "nodes 10000000" \
  --parallel 12 \
  --iterations 5 \
  --max-new-positions 20000 \
  --work-dir mine_work \
  --resume
```

`frontier` と `expand` のオプションはそのまま指定できます (`--book` / `--out` / `--journal` / `--report` / `--leaves` を除く)。

| オプション | 既定 | 説明 |
|---|---:|---|
| `--book <path>` | 必須 | 1 周目の入力 `.db` |
| `--out <path>` | 必須 | 最終周の book の書き出し先 |
| `--iterations <N>` | `1` | 周回数 |
| `--max-new-positions <N>` | なし | 累計追加局面数の上限 |
| `--merge <min\|replace>` | `replace` | 逆伝播の合成。BookMiner 相当は `replace` |
| `--work-dir <dir>` | 必須 | 各周の成果物の保存先 |
| `--resume` | | `--work-dir` の完了済みの周の続きから再開する |

各周は次の順に処理し、成果物を `<work-dir>/iter-XXX/` に保存します。

| ファイル | 内容 |
|---|---|
| `leaves.txt`, `leaves.txt.entered`, `frontier.md` | その周の入力 book に対する `frontier` の出力 |
| `expanded.db`, `expand.md` | `expand` の出力 |
| `book.db`, `backprop.md` | `expanded.db` を逆伝播した book。次の周の入力になる |
| `summary.json` | 周の集計 (末端数・追加局面数・追加手数・値を埋めた手数) と実行設定。周の完了印として最後に書く |

逆伝播は `book_backprop` と同じ処理をライブラリとして呼びます (`--draw-value 0`、`--max-iters 1000`、`--skip-unusable-moves`、`--skip-unsearched-children` 相当)。非合法手や `none` 行の値 (ラベル付けできず 0/0 のまま残った行を含む) が局面の best になって伝播しないよう、これらを best から除きます。また、候補手が 1 件以上あり全行の `depth=0` の未探索局面は、直接キー・反転キーとも子局面が book に無いものとして扱い、親の手の探索値を保持します。この逆伝播の判定には `value` を使わず、候補手の無い局面は除外しません。未探索の子の 0 が親の実探索値を上書きすることを防ぐためです。加えて、逆伝播入力時に全候補が `value=0 depth=0` の局面は行を保持し、次の frontier で未探索として列挙できるようにします。逆伝播は循環の引分下限を設けず、連続王手と内部距離による同値補正を常時扱います（[既定出力の変更と規則](book_backprop.md)）。探索 depth は保持し、合流手の自動補完は run では無効です。journal は `<work-dir>/journal.jsonl` に全周分を追記し、周をまたいで再利用します。エンジンは周をまたいで起動したままにします。

周回は次のいずれかで終わります。

- `--iterations` 周に達した
- 累計追加局面数が `--max-new-positions` に達した。各周の末端数は残り予算で打ち切ります (`--extend-ply` で辿った局面の分は超過しえます)
- 末端が無くなった、または局面・手の追加も値の埋め込みも無かった

終了時に最終周の `book.db` を `--out` に書き出します。

`--resume` を付けると、`iter-001` から連続する完了済み (`summary.json` と `book.db` がある) の周を飛ばし、最後の完了周の `book.db` から続けます。中断した周は最初からやり直しますが、探索結果は journal から再利用します。`--resume` 無しで前回の周や journal が残っている `--work-dir` を指定するとエラーにします。

`summary.json` には `--book` / `--roots` の正準パスと SHA-256、frontier 設定 (`--side` / `--window` / `--opp-min-count` / `--own-eps` / `--max-ply` / `--max-depth` / `--max-leaves`)、`--merge` を記録します。`--resume` 時にこれらが完了済みの周の記録と異なる場合はエラーにします。パスは記録用で比較せず、ファイルは SHA-256 で照合します (同じ内容の book を別パスに置いても再開できます)。実行設定を記録していない旧形式の `summary.json` を含む `--work-dir` は再開できないため、新しい `--work-dir` で開始してください。探索設定 (`--go` / MultiPV 設定 / エンジン) は journal の再利用条件で照合され、異なる場合は journal を再利用せず探索し直します。

中断した周のディレクトリには、書きかけの `book.db` などが残ることがあります (逆伝播の `book.db` は atomic 書き込みではありません)。周の完了は `summary.json` の有無で判定し、`--resume` はその周を最初からやり直して上書きするため、`summary.json` の無い周の成果物は使わないでください。

探索エラーで worker を止めるとき、他の worker で実行中の探索には `stop` を送らず、その探索の完了を待って結果を journal に回収してからエラー終了します。長い `--go` では終了までその分の時間がかかります。

## パス検証

`frontier` は `--book` / `--roots` / `--out` / `<out>.entered` / `--report`、`expand` は `--book` / `--out` / `--leaves` / `--journal` / `--report`、`run` は `--book` / `--out` / `--roots` の全ペアで正準パスの一致を検査し、同じファイルを指す組があれば起動時にエラーにします (`book_extend` と同じ検査)。

## 範囲外

- BookMiner の反駁 leaf (`pr`) などの他モード
- 入玉局面のプレイアウトラベル (frontier の入玉フラグ出力まで)
- 同値で depth の異なる非 best 手を `value - 1` する補正
