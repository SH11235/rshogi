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

roots から BFS で辿ります。訪問済み判定には反転を同一視した局面キーと、「実際の局面の手番が採掘側か」の組を使います。各組で訪問した最小 ply を記録し、より小さい ply で到達した場合は `--max-ply` までの残り手数が増えるため子を再展開します。同じか大きい ply では再訪せず循環でも停止しますが、反転で採掘側と相手側が入れ替わる経路は両方辿ります。`--side both` の場合は先手用と後手用の BFS を別々に行い、末端の和集合を取ります。

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
| `--engine-option <k=v>` | | USI option。複数指定可。同名キー（大小文字・前後空白を無視）の重複は起動時に拒否 |
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

同じ局面キーの入力行は連結して保持します。未探索局面に同じ指し手が複数行ある場合、全ての一致行の value/depth を更新し、それぞれの count/ponder は保持します。

`--journal` は JSON Lines 形式で、探索局面ごとに 1 行 (`key` / `sfen` / `go` / `multipv` / `engine_fingerprint` / `declaration_win` / `lines` / `multipv_used` / `extensions`) を追記します。未探索局面の既存手の searchmoves 探索は `searchmove` に対象手を入れた別レコードで、局面 key と対象手の組で再利用します。`multipv` は `--multipv` / `--multipv-delta` / `--multipv-max` を並べた文字列、`engine_fingerprint` は形式識別子 `content-v1`、`--engine` の basename、バイナリの SHA-256、`--engine-option` の正規化文字列から作ります。

キー名が大文字小文字を問わず `File` / `Dir` / `Path` / `COEFF` で終わるオプション（`EvalFile`、`EvalDir`、`LS_PROGRESS_COEFF` を含む）だけを内容識別の対象とします。それ以外（`Threads` など）は同名のファイルがあっても値を文字列のまま比較します。対象が既存の通常ファイルなら、パス文字列の代わりに内容の `sha256:<hex>` で識別します。対象が既存ディレクトリなら、その直下の通常ファイル（symlink のリンク先を含む）を名前順に並べた（ファイル名・サイズ・内容の SHA-256）のハッシュを使います。ディレクトリ内の壊れた symlink はエラーにします。存在しないオプション値のパスなどは従来どおり文字列で比較します。ファイルはストリーミングでハッシュ化し、起動時に計算した fingerprint を全 worker・周回で共有します。キー名は保持します。同じパスでも内容が変われば journal と summary の終端・収束キャッシュを再利用しません。同一のネット内容と他のオプションなら、配置パスが異なるマシン間でも journal を再利用できます。ただしエンジンの basename とバイナリの SHA-256 も引き続き一致が必要です。実行中のモデル差し替えには対応しません。

再開をまたぐ journal・終端・収束キャッシュの再利用には、`--engine-option EvalFile=<path>` または `--engine-option EvalDir=<path>`（YaneuraOu）の明示指定が必要です。モデルを識別するオプション名は、大文字小文字を問わず `EvalFile` / `EvalDir` との完全一致だけです。`BookFile` や `LS_PROGRESS_COEFF` なども内容ハッシュの対象ですが、モデル指定には数えません。どちらも指定されていない場合は既定モデルの場所を推測せず、作業ディレクトリに `eval/nn.bin` があっても fingerprint に `model=unidentified` とランダムなプロセス固有 nonce を含め、再開をまたぐキャッシュ再利用を無効化します。stderr に明示指定を促す警告をプロセスごとに一度出します。同じプロセス内では fingerprint を共有します。

内容ハッシュ導入前の journal は新しい fingerprint 形式と一致しないため再利用せず、再開時に対象局面を再探索します。モデル指定のない旧 journal も同様です。

`--resume` は key・`go`・`multipv`・`engine_fingerprint` が一致するレコードだけを再利用します。探索途中でエラーになった場合も、完了した探索は journal に追記済みなので `--resume` で再開できます。

`expand` と `run` は journal に標準ライブラリの排他ファイルロックを取得し、復旧・探索・追記からコマンド終了まで保持します。別プロセスが使用中なら `another book_mine process is using this journal` として失敗します。通常終了・エラー終了では worker 終了後に明示的に unlock し、Unix の別スレッドが生成した子の fork→exec 間の fd 複製にも解放を依存させません。ロックはプロセス強制終了時にも OS が解放し、ロックファイルは残しません。

最終行だけが不正 JSON（不完全な UTF-8 を含む）、または末尾改行を欠く場合は、stderr に警告してその行の先頭まで journal を切り詰めます。先行する正常レコードを再利用し、破棄した探索は再実行できます。最終行以外の不正 JSON はエラーとし、ファイルを変更しません。

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
- 全ての非終端末端を処理し、局面・手の追加も値の埋め込みも無かったうえ、逆伝播後に再計算した frontier に既知の終端以外の末端が無い。`--max-leaves` や残り局面予算で打ち切った周は、追加ゼロでも収束としません

宣言勝ち・合法手なしと判明した末端は、ply を除き実際の先後を保持したキーで終端として `summary.json` の `terminal_keys` に記録し、探索設定が一致する間、以後の frontier 出力と件数制限の対象から除きます。Point27 は先手28点・後手27点なので宣言勝ちは反転局面に流用しません。合法手なしも記録形式を統一するため実局面キーを使います。`all_leaves_processed` に打ち切りの有無も保存するので、`--resume` 後も後続の末端へ進めます。宣言勝ちは設定が一致する journal からも復元します。この2項目のない既存 summary は未収束として再開し、終端を再判定します。単独の `frontier` コマンドは run の終端記録を読みません。

終了時に最終周の `book.db` を `--out` に書き出します。

`--resume` を付けると、`iter-001` から連続する完了済み (`summary.json` と `book.db` がある) の周を飛ばし、最後の完了周の `book.db` から続けます。保存済み summary が収束または全末端処理済みを示していても、その book の frontier を既知の終端を除いて再計算し、空の場合だけ周回を省略します。旧版で誤収束した循環の出口もこの再計算で探索を続けます。中断した周は最初からやり直しますが、探索結果は journal から再利用します。`--resume` 無しで前回の周や journal が残っている `--work-dir` を指定するとエラーにします。

`summary.json` には `--book` / `--roots` の正準パスと SHA-256、frontier 設定 (`--side` / `--window` / `--opp-min-count` / `--own-eps` / `--max-ply` / `--max-depth` / `--max-leaves`)、`--merge` を記録します。`--resume` 時にこれらが完了済みの周の記録と異なる場合はエラーにします。パスは記録用で比較せず、ファイルは SHA-256 で照合します (同じ内容の book を別パスに置いても再開できます)。実行設定を記録していない旧形式の `summary.json` を含む `--work-dir` は再開できないため、新しい `--work-dir` で開始してください。探索設定 (`--go` / MultiPV 設定 / エンジンのバイナリ・オプション。`EnteringKingRule` を含む) は journal の再利用条件で照合します。同じ条件から作る `search_settings_fingerprint` を summary にも保存し、不一致ならその周の終端キーと収束判定を無視して stderr に通知します。完了済み book は保持し、残る末端を再計算して、不一致の journal を使わず探索し直します。新フィールドのない旧 summary は終端キーを持つ場合だけ不一致として扱います。再探索するには完了済み周数より大きい `--iterations` を指定してください。

中断した周のディレクトリには、書きかけの `book.db` などが残ることがあります (逆伝播の `book.db` は atomic 書き込みではありません)。周の完了は `summary.json` の有無で判定し、`--resume` はその周を最初からやり直して上書きするため、`summary.json` の無い周の成果物は使わないでください。

探索エラーで worker を止めるとき、他の worker で実行中の探索には `stop` を送らず、その探索の完了を待って結果を journal に回収してからエラー終了します。長い `--go` では終了までその分の時間がかかります。

## パス検証

`frontier` は `--book` / `--roots` / `--out` / `<out>.entered` / `--report`、`expand` は `--book` / `--out` / `--leaves` / `--journal` / `--report`、`run` は `--book` / `--out` / `--roots` の全ペアで正準パスの一致を検査し、同じファイルを指す組があれば起動時にエラーにします（Windows では未作成部分も大小文字を区別せず、Unix では区別します）。`run` ではさらに `--book` / `--roots` / `--engine` / `--out` が `<work-dir>/journal.jsonl` や `<work-dir>/iter-*/` 以下（`summary.json` を含む）の内部成果物を指す場合も、未作成の出力先を含め起動時に拒否します。起動時に既存の journal と iter-* 以下を検査し、内部成果物の symlink／junction はリンク先によらず拒否します。各周の開始時にも検査します。book.db、expanded.db、summary.json、leaves、各 report を含むため、内部ファイルリンクから入力や宣言済み出力への書き込みも拒否します。実行中に別プロセスが内部成果物を差し替える操作には対応しません。

## 範囲外

- BookMiner の反駁 leaf (`pr`) などの他モード
- 入玉局面のプレイアウトラベル (frontier の入玉フラグ出力まで)
- 同値で depth の異なる非 best 手を `value - 1` する補正

## `tree`: 台帳を共有する独立した木

`tree` は `frontier / expand / run` と別の展開規則です。根の読みの候補と実戦の手順から始め、こちらの手番は1位、相手の手番は上位 `opp_n` 手を伸ばします。根以外で1位の絶対値が `stop` 以上なら規則と実戦の枝を止めます。未展開手が伝播後の展開手を上回ると、回の終わりに FORCE に加えて最大 `rounds` 回伸ばし直します。根は STOP を無視し、こちらの根の候補が1手だけなら2位も加えます。

木ごとの LEVEL は最短の段を回をまたいで保持し、浅い再発見は既存の辺の子孫にも伝えます。EDGE・RULE・FORCE は累積です。手数は根の手数に経路の長さを足した最小値を使います。台帳に他の木の読みがあっても、自分で要求した局面だけを評価します。negamax は各局面を空の祖先手順から評価し、その手順内の循環だけ `draw` にします。評価集合内の子に向かう MultiPV ∪ EDGE の手をつなぎます。同値は MultiPV の順、次に手の文字列順で選びます。

### 常駐

```bash
book_mine tree serve --state /path/to/state --out-root /path/to/trees \
  --engine /path/to/engine \
  --engine-option "EvalFile=$SHOGI_DATA/nnue/model.bin" \
  --engine-option "LS_PROGRESS_COEFF=$SHOGI_DATA/progress/coeff.json" \
  --engine-option DrawValueBlack=-70 --engine-option DrawValueWhite=70 \
  --nodes 1000000000 --multipv 3 --hash 1024 --workers 16
```

`EvalFile` / `EvalDir` は必須で、既定のモデルを推測して再利用しません。serve では指定した全 option が engine から広告されることも確認します。読み条件と参照ファイルは実行中に変更しないでください。必要な USI option は `--engine-option K=V` を繰り返して指定します。`Threads=1`、`USI_Hash=--hash`、`MultiPV=--multipv` は固定です。局面ごとに `usinewgame` と `isready` を送り、手順履歴を渡さず `position sfen` と `go nodes` で読みます。bound 付き info は捨て、MultiPV 番号ごとの最後の行を番号順に保存します。値の順序は詰め直しません。受理する読みの中で手が重複した場合は検査エラーにします。mate は符号付き `30000 - |n|` です。

`--out-root` は対象の出力の根で、state に固定されます (再開で変えると起動を拒否)。`--opp-n` (既定 2)・`--stop` (300)・`--rounds` (3)・`--draw` (0) は、キューの行で `opts` を省いた項目の既定です。読み条件 (指紋) は state に固定され、変えるときは新しい state dir で起動し、`--journal` で同じ台帳を指定して読みを使い回します。

### 対象のキュー

読む対象は `<state>/targets.jsonl` (JSON Lines、追記型) に `book_mine tree enqueue` で足します。serve は動いたまま新しい行を受け付けます。

```bash
book_mine tree enqueue --state /path/to/state --id p12_w \
  --root "sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1" \
  --own w --depth 6 --priority 100 \
  --game "7g7f 8c8d 2g2f" --opp-n 2 --stop 300 --rounds 3 --extra 5i6h --memo "局 24"
# {"start":0,"end":243,"lines":1}   ← 受領 (この追記のバイト範囲と行数)

book_mine tree enqueue --state /path/to/state --from more.jsonl   # 全行を検査してからまとめて追記
book_mine tree enqueue --state /path/to/state --repair            # 末尾の書きかけの行だけを切り詰める
```

| 項目 | 内容 |
|---|---|
| `id` | `[A-Za-z0-9_-]{1,64}`。出力先の dir 名。大小文字だけが違う id は拒否 |
| `root` | `sfen <盤> <手番> <持駒> <手数>` か `startpos moves <USI の手順>`。合法でなければ拒否 |
| `own` | `b` / `w` / `bw`。`bw` は先手と後手の 2 本の木 (読みは台帳で共有)。2 本は一緒に受け付けるか一緒に拒否する |
| `depth` | 1〜20 |
| `priority` | 0〜1000000、小さいほど先。目安: 0 = 急ぎ、100 = 既定、1000 = 空きを埋めるだけ |
| `hold` | 真なら読まない (読みかけは読み終えて台帳に入れる)。解除で再開 |
| `game` | 根からの実戦の手順 (USI の配列の配列)。各手順の最初の `depth+1` 手を局面別に合わせる |
| `opts` | `opp_n` (1〜8)・`stop` (1〜30000)・`rounds` (0〜10)・`draw` (−30000〜30000)・`extra` (根で合法な手、8 手まで。こちらの手番の根では MultiPV 内の手だけを伸ばす) |
| `memo` | 自由 (道具は読まない) |

- 行は差分でなく完全な指定で、同じ `id` では検査を通った最後の行が有効です。`root` と `own` は `id` ごとに固定で、変えた行は拒否します (別の `id` で足す)。拒否した行は `targets.status.json` の `rejected` (バイト範囲・`id`・理由) と ALERT に残り、有効な定義は前のままです。
- enqueue は行の形と局面・手順の合法性を serve と同じ関数で検査し、通らなければ何も書かずに非 0 で終わります。`<state>/targets.lock` を取ってから、末尾が行の境界であることを確かめ、まとめて 1 回で追記して fsync し、受領を標準出力に出します。末尾に書きかけの行があれば何も変えずに非 0 で終わるので、`--repair` で切り詰めてください (切り詰めた中身を標準エラーに出します)。書き込みが途中で失敗したときは受領を出しません。どこまで入ったかは、キューの改行で終わった行と `targets.status.json` の `processed_bytes`・`rejected` で確かめます。
- キューは手で書いてもかまいませんが、検査と lock がありません。serve は `targets.lock` を共有で取ってから最後の改行までを読み (enqueue の追記と fsync が終わった行だけを読む)、書きかけの行は次の周回に回します。処理済みの範囲の内容が変わったら、戻るまで受付を止めて ALERT と `targets.status.json` の `queue_error` に書きます (読みは続けます)。キューが通常のファイルでない場合も同じです (起動時にリンクなら起動を拒否します)。戻せないとき (処理済みの行が失われたときなど) は、新しい state dir で起動して `--journal` で同じ台帳を指定し、続けたい対象を足し直します。
- 段数・`opts`・`game` を変えた行は、既定で解決した定義が今の世代と違うので新しい世代を作り、台帳にある局面は読み直しません。priority・hold・memo だけが違う行は世代を変えません。`opts` を省いた項目は、その行を処理したときの serve の既定で解決して保存します。起動オプションを変えた後に priority だけを変えた行を足すと、解決後の値が変わって新しい世代になります。世代を変えたくないときは解決済みの値を `opts` に書いてください。

### 世代と出力先

対象の出力は `<out-root>/<id>/<own>/g<世代>/` です。前の世代の dir は残し、その `STATUS` を `SUPERSEDED` にします (中身は消しません)。後段 (登録) には `book_mine tree wait` の出力か `targets.status.json` で、**今の世代かつ `ready=true`** の出力先だけを渡してください。各世代の dir には記録として根と手順の `paths.record.tsv` (paths.tsv 形式) を書きます。serve はこれを入力に使いません。replay に使うときは dir の外へ写してから指定します。

serve は出力の根の `serve.lock` を終了まで保持し、根の下はその serve だけが書きます (対象の数によらず lock は 1 つ)。同じ出力の根で 2 本目の serve は起動できません。`tree replay` は出力先の `tree.lock` を取り、上の階層に `serve.lock` があればそれも取ってから書くので、serve が動いている間は根の下へ replay できません。`serve.lock` がシンボリックリンクなど通常のファイルでなければ、serve も replay も拒否します。serve は世代の dir を用意する (前の世代を SUPERSEDED にする) ときにその dir の `tree.lock` を一時的に取るので、serve より前に始まった replay が終わるまでその対象を用意しません。前の世代の dir の SUPERSEDED は書けなければ次の周回でやり直し、消された前の世代の dir は作り直しません。エンジンを起動できない失敗 (パスの誤りや開けるファイル数の上限など) は局面の失敗に数えず、読みが成功するまで ALERT に 1 回だけ書いて、次の周回で起動し直します。出力の根が台帳・state の中にあれば起動を拒否し、対象の出力先が台帳・state と重なる行と、出力先の途中・生成物がシンボリックリンクの行は拒否します。

### 受け付けとチェックポイント

`<state>/accepted.json` がチェックポイントです。有効な定義 (`id`・手番・解決済みの定義・出力先・世代・受付順・priority・hold)、キューを処理した位置 `processed_bytes` とそこまでの sha256、確定した拒否の一覧を、新しい行を処理するたびに 1 回の原子的な書き込みで確定します。副作用 (前の世代の SUPERSEDED、新しい世代の dir・lock・`QUEUED`、列の作り直し) はチェックポイントの後に行い、どれも冪等です。チェックポイントの前に落ちた行は次の起動で処理し直し、後に落ちたら副作用だけをやり直します。過去の行は再適用しません。inbox の TOML で受け付けていた頃の state (`accepted.json` が配列) からは再開できません。新しい state dir で起動し、古い台帳を `--journal` で指定してください。

列は局面ごとに、それを求める保留でない木の `(priority, 受付順, 段)` の最小を選び、`(priority, 受付順, 段, key)` の順に並べます。受付順は `id` が最初に受け付けられた順で、世代を変えても変わりません。空いた worker に先頭から割り当て、読みかけの worker は取り戻しません (優先度の高い対象の次の局面は、実行中の読みが終わるまで待つことがあります)。同じ局面と指紋の要求は列中・実行中とも 1 つにまとめます。

### 状態と待機

`<state>/targets.status.json` は 1 回の原子的な書き込みで出す 1 つの JSON です: `run_id`、`processed_bytes`、`processed_sha256`、`all_ended`、`targets` (対象ごとに `id`・`own`・`generation`・`priority`・`hold`・`state`・`ready`・`read` (読んだ局面数)・`required` (要る局面数)・`out`)、`rejected`。`state` は `RESUMING / QUEUED / RUNNING / HELD / DONE / NOT_READY / FAIL`。`queue_error` はキューの受付が止まっている理由です (無ければ null)。`all_ended` は有効な木がすべて `DONE / NOT_READY / FAIL / HELD` のときに真 (0 本でも真) で、worker が止まったことは意味しません。serve は起動のたびに `run_id` (時刻と PID) を作り、起動ごとの lock `<state>/run/<run_id>.lock` を取って終了まで保持します (前の起動の lock ファイルは起動時に消します)。状態のファイルはこの lock を取った後にしか今の `run_id` で書きません。出力に触れる前にこのファイルを全対象 `RESUMING`・`ready=false` で書き直します (書けなければ起動をやめます)。`ready=true` はその世代の出力一式が揃ってから出します。書き込みに失敗したら ALERT に書いて次の周回でやり直します。

```bash
receipt=$(book_mine tree enqueue --state "$STATE" --from targets.jsonl)
book_mine tree wait --state "$STATE" --receipt "$receipt" [--ids p12_w,p11_b]
```

`wait` は次を同じ 1 回の読み取りで満たしたときに、対象ごとの `id`・`own`・`generation`・`state`・`ready`・`out` を JSON で出して 0 で終わります: `processed_bytes` が受領の `end` 以上、受領の範囲 `[start, end)` に拒否が無い、`all_ended` が真 (`--ids` を付けたらその対象と受領の範囲で足した `id` が終わった状態)、状態のファイルを書いた serve 自身が生きている (状態の `run_id` の起動ごとの lock が取られている)。受領の範囲に拒否があればその理由を出して非 0、serve が落ちていれば非 0 で終わります。範囲の外の拒否 (他の書き手の行や同じ `id` の過去の拒否) は見ません。受領の範囲が未処理のまま `queue_error` が 2 秒続けば、その理由を出して非 0 で終わります (チェックポイントを一時的に書けない場合も含みます。直した後に待ち直してください)。`--ids` に状態に無い id を渡すと (打ち間違いや、別の受領で足してまだ処理されていない id)、待たずに非 0 で終わります。状態のファイルが前の起動のもの (その `run_id` の lock が取られていない) なら、state の lock で新しい serve が動いているかを確かめ、動いていれば新しい状態が出るまで待ち、動いていなければ非 0 で終わります。lock は共有 lock で一瞬試すだけです (serve は起動時に state の lock を 1 秒ほど待ち直すので、待機と起動が重なっても起動は失敗しません)。

### 並列数・停止・監視

- `<state>/workers` の整数を書き換えて並列数を変更します。初回の既定16。既存の workers ファイルがあれば再起動時も `--workers` より優先します。不正な内容の通知は内容または mtime が変わるまで1回です。減らすと余った worker は読みかけを台帳へ保存してから終了します。0は全 worker の drain で、serve は常駐を続けます。数を戻すと再開します。
- `<state>/stop` を作ると読みかけを保存して serve を終了します。再開前に stop を除き、同じコマンドを実行します。kill 後も、保存済みの読みから木の回を再構築します。台帳の改行の無い末尾だけは切り捨てます。
- 木が全て完了しても `IDLE` で受付を続けます。プロセスの `STATUS` は `<状態> <run_id>` の 1 行です (人が見るためのもので、wait は使いません)。
- `--stall-secs` (既定600) の間 info の nodes / depth のどちらも増えなければ `ALERT` に書きます。`info string` や同じ値の繰り返しでは時計を戻しません。既定では探索を止めません。
- 固まった worker を明示的に作り直すには `<state>/restart/<worker番号>` を作ります。受付時点でその worker が読んでいる局面を列に戻します。対象の読みが無い番号は ALERT して制御ファイルを消します。明示 restart は自動再起動の上限に数えません。エンジン切断は自動で作り直します。`--max-restarts` (既定3) を超える同一局面の失敗、または再現する読みの検査エラーでは、その局面を待つ木だけを FAIL にして ALERT し、他の木の処理を続けます。serve の MultiPV は1行以上かつ指定数以下を受理し、各手の合法性と重複が無いことを検査します。根の不成省略などで指定数より少なくても受理します。0行の場合は合法手の有無によらず `bestmove resign` / `bestmove win` を受理し、展開しない葉として保存します。1位が `win` の1行だけで `bestmove win` が返る場合も、0行の宣言勝ちと同じ葉として扱います。合法手数や根の候補数から行数は推測しません。
- state の lock と journal 自体の lock を終了まで保持します (Windows でもデータの範囲は lock しないので、serve の実行中に journal を読めます)。別 state でも同じ journal の同時使用はできません。木の出力は出力の根の `serve.lock` で別の serve や replay から保護します。`--journal` を省略すると `<state>/journal.jsonl` を使います。

journal、state dir とその管理ファイル (キュー・lock・チェックポイント・状態のファイルなど) がシンボリックリンクなら、リンク先の有無によらず拒否します。親 dir の途中のリンクと未作成部分の `..` は解決して比較します。journal と state の管理パスの衝突も拒否します。replay では、木の出力 dir（配下の生成物全体）と journal・state・paths.tsv・定義ファイルの衝突を拒否します。

プロセスの `STATUS` は `RUNNING / IDLE / STOPPED / FAIL`。木の出力先の `STATUS` は `QUEUED / RUNNING / HELD / STOPPED / DONE / NOT_READY / FAIL / SUPERSEDED` です。プロセスの STOPPED は workers=0 または stop 指示で読みかけの保存が終わった状態で、待ち列が残っていても使います。木の QUEUED はまだ読み始めていない待機、RUNNING は読み始めた未完了の木、STOPPED は停止指示によりその木の実行中の読みが無くなった状態です。`progress.json` に木 (id・手番・世代・priority・hold) の回・読み済み数・要求数・列の長さと、worker の局面・経過秒・nodes / depth を記録します。`ALERT` は日時付きの追記です。キューの読み取りと出力先の用意の失敗は、内容が変わるまで 1 回通知して次の周回でやり直します。STATUS は内容が変わった場合だけ書きます。終了した木（DONE / NOT_READY / FAIL）の出力 dir を削除しても再作成しません。STATUS・progress.json・ALERT の書き込み失敗は標準エラーへ報告して次の周期で再試行します（終了した木の出力 dir が残っている場合）。journal と成果物の書き込み失敗はエラーとして扱います。

### 探索の記録

`--trace-dir <dir>` を付けると、読みごとに `go` から `bestmove` までの info 行を `<dir>/<run_id>.jsonl` に追記します (`run_id` は `targets.status.json` と同じ起動ごとの id)。既定では記録しません。読み条件の指紋にも state にも入らないので、同じ state と台帳のまま起動ごとに付け外しや dir の変更ができます。台帳・木の処理・出力には影響しません。trace-dir が state dir・出力の根・台帳と同じかその中にあるとき (state の `stop` や `restart/` を指示として読んでしまう)、または trace-dir 自体がシンボリックリンクのときは起動を拒否します。dir とファイルは最初の記録を書くときに作ります。1 読みあたりの大きさは info 行の数に比例し、深さ 40・MultiPV 3 で 20〜30 KB 程度です。メモリに持つのは各 worker の読みかけ 1 局面分だけで、1 回の探索の行が合計 4 MiB を超えたら、その探索の行を捨てて以後は集めず、ALERT に書いてその探索の記録を出しません (台帳の読みは変わりません)。

1 行が 1 回の探索で、次の JSON オブジェクトです。

| 項目 | 内容 |
|---|---|
| `key` | 手数を除く SFEN の 3 フィールド (台帳の `reading.key` と同じ) |
| `ply` | `position sfen` に渡した手数 |
| `fingerprint` | 読み条件の指紋 (台帳の `settings.fingerprint` と同じ) |
| `worker` | worker 番号 |
| `started` / `finished` | `go` を送った時刻 / `bestmove` を受けた時刻 (RFC 3339、UTC) |
| `bestmove` | `bestmove` の手 |
| `lines` | `go` の後に受けた `info` で始まる行を、受けた順にそのまま並べた配列。台帳の読みを作るときに見る行と同じで、`info string` と bound 付きの行も含む |

- 記録するのは、検査を通って台帳に追記した探索だけです。drain (`workers` を減らす・0 にする) と `stop` は読みかけを読み終えるので記録します。途中で終わった探索 (明示 restart、engine の終了・切断、読みの検査失敗、serve のエラー終了) は記録しません。台帳にある局面は探索しないので記録はありません。台帳への追記の直後にプロセスが落ちると、台帳にあって記録に無い読みが残ることがあります。
- 書き込みは 1 行を 1 回で追記します。書けないとき (dir を作れない、ディスクの空き不足など) は、書けるようになるまで ALERT に 1 回だけ書き、読みは続けます。次の記録のときに dir とファイルを開き直し、書けなかった読みの記録は書き直しません。探索の記録は読みの結果に使わないので、記録のために高価な読みを止めません。
- 読む側は空行と JSON として読めない行を読み飛ばしてください。落ちたときの末尾の書きかけの行と、書き込み失敗で途中まで書いた行がこれに当たります (失敗の次の行は改行から書くので、後の行は壊れません)。

`lines` からは次の規則で MultiPV を組み立てます。対象は ` pv ` と ` score ` を含み、`lowerbound` / `upperbound` の語を含まない行です。番号は `multipv` の値 (無い行は 1)、手は `pv` の次の語、深さは `depth` の値、値は `score cp n` なら n、`score mate n` なら符号付き `30000 - |n|` です。

- 台帳の読みの再現: 対象の行のうち、番号ごとに最後の行を番号順に並べます。`nodes` では絞りません。engine は nodes 上限を少し超えてから止まるので、最後の行の `nodes` は上限を超えることがあります。
- N ノード時点の読み (近似): 上の対象のうち `nodes` が N 以下の行に限って、番号ごとに最後の行を取ります。info 行は反復深化の 1 反復ごと (MultiPV では番号ごと) に出るので、N で止めた探索そのものではなく、N までに終わった最後の反復の結果です。`nodes` の無い行はこの比較に使えないので除きます。

### Python の読みの取り込みと再生

```bash
book_mine tree import --journal /path/to/tree-journal.jsonl \
  --engine /path/to/engine \
  --engine-option "EvalFile=$SHOGI_DATA/nnue/model.bin" \
  --engine-option DrawValueBlack=-70 --engine-option DrawValueWhite=70 \
  --nodes 1000000000 --multipv 3 --hash 1024 \
  --input /path/to/python-out/d0.jsonl /path/to/python-out/d1.jsonl

book_mine tree replay --journal /path/to/tree-journal.jsonl --tree /path/to/tree.toml
```

replay の木の定義は TOML です。TOML 内の相対パスは TOML の親 dir から解決します。

```toml
paths = "../../example.paths.tsv"
# paths_sha256 = "..."  # 任意の内容検証
out = "../../example-out"
depth = 6
opp_n = 2
stop = 300
own = "w"
rounds = 3
extra = ["6b5a"]
# fingerprint = "..."  # import / serve が表示する読み条件。省略時はプロセスの条件
# draw = 0            # 手順上の循環に付ける手番側の値。USI DrawValue とは別
```

paths.tsv はヘッダ無しで `key<TAB>ply<TAB>USIの手順<TAB>メモ`。key は手数を除く SFEN の3フィールドで、同じ根をまとめ、根の手番をそろえます。GAME は各手順の最初の `depth+1` 手を局面別に合わせます。`own` は `b` / `w`。こちらの根の EXTRA は MultiPV 内だけ、相手の根の EXTRA は合法なら採用します。

import では engine とファイル型 USI option の内容 SHA-256、全 option、明示 `SPSAParamsFile` が無い場合のバイナリ隣接 `spsa.params` の有無と内容、nodes、MultiPV、局面ごとの初期化・info 解析の版を条件の指紋にします。Python の記録は元の engine / 評価ファイルの digest を持たないため、**指定した条件の宣言による取り込み**です。出どころのパスと入力ファイルの SHA-256 も記録します。nodes_limit / threads / hash / draw / MultiPV 行数が宣言と違う行、不正な局面・手、同じ条件と局面で MultiPV の値や深さ・bestmove が異なる重複を拒否します。取り込み全体を検査してから追記します。

replay は engine を起動せず、serve と同じ木の処理を台帳の読みだけで進めます。不足や ready:false は非0で終了します。出力先と journal・paths.tsv の衝突を検査し、出力先と journal の lock を取る前には既存の出力を変更しません。複数条件が台帳にある場合は `--fingerprint` で選びます。tree 用台帳は版付きで、既存 `expand / run` の台帳とは共用できません。 台帳は行単位で読み、serve・import は宣言した fingerprint、replay は選択した fingerprint の読みだけをメモリに保持します。serve はその読みを直接参照し、別のキャッシュへ複製しません。replay は同じ排他 lock を保持したまま条件（Settings）の一覧だけを収集し、条件を選んでから対象の読みを読み込みます。他の fingerprint の行も形式・版を検査し、改行の無い末尾を回復しますが、読みの妥当性と重複の矛盾検査は対象の fingerprint 内だけで行います。保持量は対象条件の局面数に比例し、過去条件の総レコード数には比例しません（全行の走査時間は必要です）。局面ごとには読み (MultiPV 数行) だけを持ちます。条件の同一性は指紋で決まり、内容が同じモデルを別のパスで指定した読みも同じ条件として使い回します。台帳の保持は 1 局面あたり 1 KiB 前後で、10 万局面で約 100 MiB です (これとは別に、各木が自分の局面の読みを持ちます)。定跡の局面を深く読む用途ではこの規模を超えない想定で、ディスク上の索引は持ちません。

### 別の台帳の読みの取り込み

別のマシンの台帳 (複製したもの) の読みを、この台帳の条件の読みとして足します。取り込み先の読み条件は serve と同じ引数で指定します。

```bash
book_mine tree import-journal --from-journal /path/to/other-machine-journal.jsonl \
  --journal /path/to/tree-journal.jsonl \
  --engine /path/to/engine \
  --engine-option "EvalFile=$SHOGI_DATA/nnue/model.bin" \
  --engine-option DrawValueBlack=-70 --engine-option DrawValueWhite=70 \
  --nodes 1000000000 --multipv 3 --hash 1024
```

取り込み元に条件が複数あれば `--fingerprint` で選びます (省略時は唯一の条件)。取り込み元は共有 lock で読み、ファイルを変更しません。serve などが書き込み中の台帳は拒否するので、複製してから渡します。改行で終わらない末尾の行は無視します。選んだ条件の行は、どの行も条件の中身から計算し直した指紋が選んだ指紋と合い、版とファイル型 option のパスを除いて同じ条件であることを確かめ、合わない行があれば取り込みません (モデルを別のパスへ移した後に serve が追記した行は同じ条件です)。

取り込み先の台帳は排他 lock を取って追記するので、その台帳を使っている serve を止めてから実行します (使用中なら lock の取得に失敗して何も書きません)。

次がすべて同じときだけ、取り込み元の読みを取り込み先と同じ条件の読みとみなします。

| 項目 | 比べ方 |
|---|---|
| nodes・MultiPV・hash | 値 |
| ファイル型の USI option (`EvalFile`・`LS_PROGRESS_COEFF` 等) と、明示しない場合のバイナリ隣接 `spsa.params` | 内容の SHA-256 (パスはマシンごとに違ってよい) |
| それ以外の USI option (`DrawValueBlack` 等) | 名前と値 |
| エンジン | バイナリが同じか、ソースの版が同じ |

エンジンのバイナリの違い (CPU 向けの最適化・production build など) は条件の違いとみなしません。ソースの版は、serve と import-journal がエンジンを `--source-version` 付きで起動して得る値で、台帳の条件に記録します (条件の指紋には含めないので、記録する前の台帳の指紋は変わりません)。版が得られない・`unknown`・`-dirty` のときは、別のバイナリとの間の取り込みに `--force` が要ることを stderr に 1 行で警告します。rshogi-usi の版は次の形です ([rshogi-usi の README](../../rshogi-usi/README.md))。

- エンジンのソース (`crates/rshogi-core`・`crates/rshogi-book`・`crates/rshogi-usi`・ルートの `Cargo.toml`・`Cargo.lock`) の HEAD の git の id をまとめた id。tools や docs だけの commit では変わりません
- そのソースに commit されていない変更があれば `-dirty`
- 有効な feature のうち、探索・評価の結果を変えないと確かめたもの (モデルの形式の選択・計数・診断など) 以外を `+` 以降に名前順で付けます。新しい feature は結果を変える側に入ります。`nnue-progress-diff` も付く側に入るので、同じソースでも universal edition と、それを束ねる specific preset (L0=1536 系など) のビルドは版が違います
- git が無い・失敗する (`safe.directory` で拒否される等)、または git の作業木の最上位でビルドしていなければ `unknown`

版が違う、どちらかの版が分からない (記録する前の台帳、`--source-version` に答えないエンジン、`unknown`、`-dirty`) ときは、バイナリが同じでなければ拒否します。探索の結果を変えない変更 (速度の改善だけ等) しか無い版どうしなら、`--force` で取り込めます。`--force` が許すのはエンジンの版の違いだけで、他の項目の違いは拒否します。

取り込みは全件を検査してから、取り込み先の条件 (取り込み先のエンジンの版を含む) で追記します。`--force` で取り込んだ読みも取り込み先の版で記録されるので、その台帳からさらに取り込むときは版の違いを問われません (出どころには `force=true` が残ります)。同じ局面で値の同じ読みは飛ばし、値の違う読みがあれば何も書かずに拒否します。各行の出どころには取り込み元の完全な行の SHA-256・条件の指紋・`--force` の有無・パスを記録します。標準出力に取り込み先と取り込み元の指紋、追記数・飛ばした数を出します。

### 出力と登録可否

木ごとの出力先に `registration_status.json` と使用した読みの `d<段>.jsonl` を書きます。ready の場合は `own_nodes.tsv`、両方の手番の局面の手ごとの伝播後の値 `node_moves.tsv`、`tree.db`、`tree_rule.db`、参考値の `tree_bp_min.db`、根の表と最後の集計を含む `report.txt` を出します。TSV の列と JSONL は node_calib / 登録・評価値更新の後段と互換です。min はライブラリの `backprop_file_with` を呼びます。参考値の生成・読み込みに失敗した場合は報告し、min 欄を空値として他の成果物を完成させます。report.txt は Python の標準出力から段ごとの進み具合を除いた内容（根の候補、伸ばし直し、警告を含む）です。

未展開手が上回る局面が残ると `ready:false` と `unresolved` を記録し、`NOT_READY` にします。これは計算自体の失敗ではありません。rounds を増やす場合は、同じ `id` で `opts.rounds` を増やした行を足すと新しい世代で読み直します。読みは同じ台帳から再利用できます。途中・失敗・ready:false の出力先には `own_nodes.tsv`・`node_moves.tsv` を残しません。ready のときも全出力が完成してから登録材料を公開し、その後に DONE を書きます。

`node_moves.tsv` は木が届く局面すべて (こちらの手番も相手の手番も) の手を、伝播後の値の順に 1 行ずつ並べます。列は `key`・`ply`・`level` (根からの段)・`remaining` (その局面から先に伸ばせた段数 = depth − level)・`nodes` (読みのノード数の上限)・`move`・`value` (手番の側から見た伝播後の値。伸ばしていない手は 1 回の読みの値)・`expanded` (伸ばした手なら 1)・`rank` です。勝ち宣言の読みの局面は載せません。

### 定跡の元データへの登録

```bash
book_mine tree register --master moves.tsv --since 20261022-tag --note "計画の名前" \
  --conflicts conflicts.tsv [--alt-gap 40] [--apply] <木の出力先>...
```

ready の木 (`STATUS` が DONE) の `node_moves.tsv` を読み、定跡の元データの表 (TSV) に手を登録します。`--apply` を省くと元データは書き換えず、集計と食い違いの一覧だけを出します。`--conflicts` はまだ無いファイルにしてください (既にあれば何もせずに止まります。確認だけの実行でも書くので、元データや木の出力を上書きしないためです)。確認だけの実行と `--apply` の実行では、一覧を別のファイル名にします。`node_moves.tsv` の無い古い出力先は `tree replay` で作り直してから渡します。

元データの表は `key ply move role source evals nodes lineage tier since note` の 11 列 (見出し行つき) です。`key` は SFEN の盤・手番・持ち駒、`role` は `main` (局面ごとにちょうど 1 つ) / `alt` / `record` (指さない手)、`tier` は `curated` / `wide`、`evals` は手番の側から見た値 (カンマ区切りで複数可) です。

規則 (局面ごと、先手・後手の手番の区別なし):

- 局面の値によらず登録します。各局面の main は伝播後の値の 1 位 (伸ばしていない手は 1 回の読みの値で比べる)、alt は main との差が `--alt-gap` 未満の手です。`record` の手は足さず、次の手を使います。
- 元データに main の無い局面には、`source=read`・`tier=wide` の行で main と alt を足します。
- 元データの main が木の 1 位と違うとき、その main が読みから作った行 (`source` が `read` / `deep`) で、局面に `curated` の行が無ければ、木の 1 位を main にし、元の main を alt に残します。それ以外 (`pin` / `top` / `team` などの main、`curated` の局面) は main を残し、木の 1 位を alt で足して `--conflicts` の一覧に出します。一覧の列は `key ply side main main_source main_tier main_tree_value tree_best tree_value tree` (値は手番の側から) です。
- `source` が `read` / `deep` の main・alt の行で木に値のある手は、`evals` と `nodes` を木の値に更新します。
- 同じ局面が複数の木にあるときは、`remaining` の大きい (その局面から深く伸ばした) 木を使い、同じなら先に渡した木を使います。
- 結果は局面・役割 (main・alt・record の順)・手の順に並べ、局面ごとに main がちょうど 1 つ (`curated` の行だけでも 1 つ) で、同じ手の行が重ならないことを確かめてから書きます。

