# book_mine

`book_mine` は YANEURAOU-DB2016 テキスト定跡 `.db` を自エンジンの探索で book 外へ展開するツールです。末端局面の列挙・探索による展開・値の逆伝播を繰り返す処理を `frontier`・`expand`・`run` の 3 つのサブコマンドで行います。このほか、台帳を共有する定跡の木 (`tree`)、定跡の元データの点検 (`master validate`・`master add`・`master add-reads`・`master hold-early`・`master export`・`master diff`・`master audit`・`master rejoin`・`master cycles`・`master check`)、相手の見立てで辿って読み始める局面を出す `master walk`、自前と外部定跡の最善・指定点差以内の手を突き合わせる `master compare`、元データの手で辿れる出口を列挙する `master exits`、外部定跡と値が食い違う局面を出す `master opp-book-diff`、戦型の候補ごとに RAMU の手を替えて辿り比べる `master opening-report`、注目の線をビュアーで辿る定跡を作る `master line-db`、大きい定跡の木を借りた登録 (`borrow`) を後の節で説明します。

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
| `--merge <min\|replace>` | `replace` | 逆伝播の合成。`replace` は伝播した値で置き換える |
| `--work-dir <dir>` | 必須 | 各周の成果物の保存先 |
| `--resume` | | `--work-dir` の完了済みの周の続きから再開する |

各周は次の順に処理し、成果物を `<work-dir>/iter-XXX/` に保存します。

| ファイル | 内容 |
|---|---|
| `leaves.txt`, `leaves.txt.entered`, `frontier.md` | その周の入力 book に対する `frontier` の出力 |
| `expanded.db`, `expand.md` | `expand` の出力 |
| `book.db`, `backprop.md` | `expanded.db` を逆伝播した book。次の周の入力になる |
| `summary.json` | 周の集計 (末端数・追加局面数・追加手数・値を埋めた手数) と実行設定。周の完了印として最後に書く |

逆伝播は `book_backprop` と同じ処理をライブラリとして呼びます (`--draw-value 0`、`--skip-unusable-moves`、`--skip-unsearched-children` 相当)。非合法手や `none` 行の値 (ラベル付けできず 0/0 のまま残った行を含む) が局面の best になって伝播しないよう、これらを best から除きます。また、候補手が 1 件以上あり全行の `depth=0` の未探索局面は、直接キー・反転キーとも子局面が book に無いものとして扱い、親の手の探索値を保持します。この逆伝播の判定には `value` を使わず、候補手の無い局面は除外しません。未探索の子の 0 が親の実探索値を上書きすることを防ぐためです。加えて、逆伝播入力時に全候補が `value=0 depth=0` の局面は行を保持し、次の frontier で未探索として列挙できるようにします。循環・連続王手・同値補正の扱いは `book_backprop` と同じです（[規則](book_backprop.md)）。探索 depth は保持し、合流手の自動補完は run では無効です。journal は `<work-dir>/journal.jsonl` に全周分を追記し、周をまたいで再利用します。エンジンは周をまたいで起動したままにします。

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

- `frontier` で、伝播後に最善へ繰り上がった未展開の手を優先して列挙するモードなど、上記以外のモード。反駁の点検は `master audit --refutation-table / --enqueue-out` でできます
- 入玉局面のプレイアウトラベル (frontier の入玉フラグ出力まで)
- 同値で depth の異なる非 best 手を `value - 1` する補正

## `tree`: 台帳を共有する独立した木

`tree` は `frontier / expand / run` と別の展開規則です。根の読みの候補と実戦の手順から始め、こちらの手番は1位、相手の手番は上位 `opp_n` 手を伸ばします。根以外で1位の絶対値が `stop` 以上なら規則と実戦の枝を止めます。未展開手が伝播後の展開手を上回ると、回の終わりに FORCE に加えて最大 `rounds` 回伸ばし直します。根は STOP を無視し、こちらの根の候補が1手だけなら2位も加えます。

木ごとの LEVEL は最短の段を回をまたいで保持し、浅い再発見は既存の辺の子孫にも伝えます。EDGE・RULE・FORCE は累積です。手数は根の手数に経路の長さを足した最小値を使います。台帳に他の木の読みがあっても、自分で要求した局面だけを評価します。negamax は各局面を空の祖先手順から評価し、その手順内の循環だけ `draw` にします。循環に入らない局面の値は手順に依らないので 1 度だけ計算します。大きな循環が重なる木で、循環の中を手順ごとに数える呼び出しが 1 回の評価で 200 万回を超えたら、以後は循環の中の局面も最初に求めた値を使い回します (別の手順から来たときの千日手の判定を引き継がない近似)。循環に入る手を指す側が先手なら `draw_black`、後手なら `draw_white` を (指定があれば) 使います (どちらもその手を指す側から見た値)。エンジンの `DrawValueBlack` / `DrawValueWhite` は、読む側 (根の手番) から見た千日手の値で、探索の中では相手側にはその符号を返した値になります。木をエンジンに合わせるなら、自分 (`own`) が先手の木は `draw_black = DrawValueBlack`・`draw_white = −DrawValueBlack`、後手の木は `draw_white = DrawValueWhite`・`draw_black = −DrawValueWhite` にします (−70 / 70 の組はどちらの条件も満たす)。`draw` 1 つだけでは、自分の側を正しくすると相手の側が逆向きになります。評価集合内の子に向かう MultiPV ∪ EDGE の手をつなぎます。同値は MultiPV の順、次に手の文字列順で選びます。

### 常駐

```bash
book_mine tree serve --state /path/to/state --out-root /path/to/trees \
  --engine /path/to/engine \
  --engine-option "EvalFile=$SHOGI_DATA/nnue/model.bin" \
  --engine-option "LS_PROGRESS_COEFF=$SHOGI_DATA/progress/coeff.json" \
  --engine-option DrawValueBlack=-70 --engine-option DrawValueWhite=70 \
  --nodes 1000000000 --multipv 3 --hash 1024 --workers 16
```

`EvalFile` / `EvalDir` は必須で、既定のモデルを推測して再利用しません。serve では指定した全 option が engine から広告されることも確認します。読み条件と参照ファイルは実行中に変更しないでください。必要な USI option は `--engine-option K=V` を繰り返して指定します。`Threads=1` と `USI_Hash=--hash` は起動ごとに指定します。MultiPV と nodes は木ごとの `opts` を使い、省略時は `--multipv` / `--nodes` を使います。局面ごとに `usinewgame` と `isready` を送り、手順履歴を渡さず `position sfen` と `go nodes` で読みます。bound 付き info は捨て、MultiPV 番号ごとの最後の行を番号順に保存します。値の順序は詰め直しません。受理する読みの中で手が重複した場合は検査エラーにします。mate は符号付き `30000 - |n|` です。

`--out-root` は対象の出力の根で、state に固定されます (再開で変えると起動を拒否)。`--opp-n` (既定 2)・`--stop` (300)・`--rounds` (3)・`--draw` (0) は、キューの行で `opts` を省いた項目の既定です。読み条件は state に固定しません。エンジン・モデル・SPSA パラメータ・nodes・MultiPV・千日手の値・Hash を変えても、同じ state と台帳で再開できます。保存済みの読みは、それぞれ読んだときの条件を保持します。

木は実行中、各局面で最初に使った読みを保持します。後からその木自身の条件の読みが届いても差し替えず、再構築した木では優先条件を選び直します。同じ局面を異なる条件で読む要求は順に処理します。`require_condition` のない木が先に読みを要求し、後の `require_condition` のある木が自身の条件で読み直すことがあります。

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
| `roots` | 追加の根の配列 (形は `root` と同じ、`root` と同じ手番に限る)。CLI の引数は無く `--from` の行で渡す。`borrow cut` が段 0 の行で使う |
| `own` | `b` / `w` / `bw`。`bw` は先手と後手の 2 本の木 (読みは台帳で共有)。2 本は一緒に受け付けるか一緒に拒否する |
| `depth` | 0〜20。0 は根 (と `roots`) を読むだけで伸ばさない |
| `priority` | 0〜1000000、小さいほど先。目安: 0 = 急ぎ、100 = 既定、1000 = 空きを埋めるだけ |
| `hold` | 真なら読まない (読みかけは読み終えて台帳に入れる)。解除で再開 |
| `game` | 根からの実戦の手順 (USI の配列の配列)。各手順の最初の `depth+1` 手を局面別に合わせる |
| `opts` | `opp_n` (1〜8)・`stop` (1〜30000)・`rounds` (0〜10)・`draw`・`draw_black`・`draw_white` (−30000〜30000)・`extra` (根で合法な手、8 手まで。こちらの手番の根では MultiPV 内の手だけを伸ばす)・`multipv` / `nodes` (正数)・`require_condition` (真偽値、既定 false) |
| `memo` | 自由 (道具は読まない) |
| `group` | 一緒に読み切りたい木のまとまりの名前 (制御文字を含まない 1〜128 バイト)。serve は使わず、`tree queue-review` の再採点と `--group-table` が読む。この欄を知らない古い serve は行を拒否する |

`opts.multipv` / `opts.nodes` は同じ serve の中で木ごとに指定できます (例: `"opts":{"multipv":5,"nodes":2000000000,"require_condition":true}`、`enqueue --from` で渡します)。木の読み条件は serve の設定の MultiPV・nodes を置き換えたものです。既定では、台帳にその局面の読みがあれば条件を問わず再利用します。同じ局面に複数の読みがある場合は木の条件と同じ指紋の読みを優先し、無ければ `nodes_limit` が最大の読み、同点なら PV 行数が最多の読み、さらに同点なら指紋の文字列が最小の読みを使います。新しいモデル・エンジンを優先したいときは serve の既定条件を変更してください。木は自身の条件を優先します。`require_condition:true` では、その木の条件と完全に同じ指紋の読みだけで読み済みと判定し、無ければその条件で読み直します。

- 行は差分でなく完全な指定で、同じ `id` では検査を通った最後の行が有効です。`root` と `own` は `id` ごとに固定で、変えた行は拒否します (別の `id` で足す)。拒否した行は `targets.status.json` の `rejected` (バイト範囲・`id`・理由) と ALERT に残り、有効な定義は前のままです。
- `--from` の入力内で同じ `id` が重複すると、id と重複した両方の行番号を示して、追記前にエラーにします。既に受付にある id と同じ行は、定義・hold・priority などの差し替えとして受け付けます。
- enqueue は行の形と局面・手順の合法性を serve と同じ関数で検査し、通らなければ何も書かずに非 0 で終わります。`<state>/targets.lock` を取ってから、末尾が行の境界であることを確かめ、まとめて 1 回で追記して fsync し、受領を標準出力に出します。末尾に書きかけの行があれば何も変えずに非 0 で終わるので、`--repair` で切り詰めてください (切り詰めた中身を標準エラーに出します)。書き込みが途中で失敗したときは受領を出しません。どこまで入ったかは、キューの改行で終わった行と `targets.status.json` の `processed_bytes`・`rejected` で確かめます。
- キューは手で書いてもかまいませんが、検査と lock がありません。serve は `targets.lock` を共有で取ってから最後の改行までを読み (enqueue の追記と fsync が終わった行だけを読む)、書きかけの行は次の周回に回します。処理済みの範囲の内容が変わったら、戻るまで受付を止めて ALERT と `targets.status.json` の `queue_error` に書きます (読みは続けます)。キューが通常のファイルでない場合も同じです (起動時にリンクなら起動を拒否します)。戻せないとき (処理済みの行が失われたときなど) は、新しい state dir で起動して `--journal` で同じ台帳を指定し、続けたい対象を足し直します。
- 段数・`opts`・`game` を変えた行は、既定で解決した定義が今の世代と違うので新しい世代を作り、既定では台帳にある局面は読み直しません (`require_condition:true` は同じ条件の読みが必要です)。priority・hold・memo・group だけが違う行は世代を変えません。`opts` を省いた項目は、その行を処理したときの serve の既定で解決して保存します。ただし `multipv` / `nodes` は省略のまま保存し、起動のたびに serve の現在の `--multipv` / `--nodes` を使います。この既定を変えると省略した木の読み条件も変わり、`require_condition:true` の木はその条件で読み直します。`multipv` / `nodes` 以外の起動時の既定を変えた後に priority だけを変えた行を足すと、解決後の値が変わって新しい世代になります。世代を変えたくないときは解決済みの値を `opts` に書いてください。

### `tree seeds`: 自己対局から次に読む根を作る

```sh
book_mine tree seeds --games matches/games.jsonl --games rematch/games.jsonl \
  --out seeds.jsonl --table seeds.tsv --tag holes \
  --depth 6 --opp-n 2 --stop 300 --rounds 3 --draw-black -70 --draw-white 70
book_mine tree enqueue --state tree-state --from seeds.jsonl
```

`tournament` の JSON Lines を読み、enqueue に渡す行を生成します。エンジンや稼働中のキューには触れません。
既定では同じ起点の先手が全勝なら出力せず、それ以外の起点では全局を `own=b` の種にし、後手勝ち・引分の局を
`own=w` の種にもします。起点は最初に `eval` が付いた move の `sfen_before` です。`eval` がなければ
その局は使いません。`--sweep-loser` を付けると、先手全勝の起点からも全敗した `own=w` の種だけを作ります。
その場合も根・offsets・局数の下限・手順の選択・除外・積み直しは通常の `own=w` と同じ規則です。
先手全勝でない起点の採否は変わりません (ただし sweep の根と同じ局面の `own=w` の根は 1 行に合わさり、その行の class は sweep、priority は `--priority-early` になります)。オフセット先の根も、その根を通る局がすべて先手勝ちなら `own=b` を出力しません。
`--chance-cp` を付けると、勝たなかった側 (先手勝ちなら後手、後手勝ちなら先手、引分なら両方) が
起点以降で初めて指定値以上の評価値を得た手から、`--chance-back` 手前の局面をその側の根に追加します。
起点より前には戻らず、起点と同じ局面になる場合は追加しません。評価値は early と同じ変換でその側から見た値にし、
詰みは通常の cp より強い勝ち負けとして扱います。exact primary のない eval は判定に使いません。
先手全勝の起点でも、後手の chance の根は `--sweep-loser` によらず作ります。
chance の根には `--min-games-offset` を適用せず、1 局から作ります。省略時は従来と同じ出力です。
起点と根の同一性は SFEN の手数を除いた局面で判断し、`startpos_idx` は使いません。

| 指定 | 既定 | 意味 |
| --- | --- | --- |
| `--games <JSONL>` | 必須、繰り返し可 | tournament の棋譜。正準パス (canonicalize) が同じ入力の重複は除く |
| `--out <JSONL>` / `--table <TSV>` | 必須 | キューの行と判断用一覧 |
| `--tag <NAME>` | 必須 | id の先頭。[A-Za-z0-9_-] の 1〜40 文字 |
| `--depth` | 6 | 木の段数 (0〜20)。`game` は根から最大 depth+1 手 |
| `--opp-n` / `--stop` / `--rounds` | 省略 | opts に書く相手の手数・打ち切り値・読み直し回数。範囲と省略時の扱いは `master walk` と同じ |
| `--draw-black` / `--draw-white` | 省略 | opts の先後別の循環値 (-30000〜30000) |
| `--offsets` | `0,6,12` | 起点から根までの手数。0 は常に含む。重複は除く |
| `--min-games-offset` | 2 | 0 より先の根に必要な局数 (正数)。同じ側の種で、同じ起点からその根までの全手順が一致する局だけ数える |
| `--max-lines` | 3 | 根の `game` の最大本数 (正数)。その手順を通った局数の降順、同数なら USI 手順の文字列順 |
| `--collapse-plies` | 30 | 起点からの早い崩れを見る手数 |
| `--collapse-cp` | 150 | 早い崩れとする不利な評価値の絶対値 (正数) |
| `--chance-cp <CP>` | なし | 正数。勝たなかった側が途中でこの評価値以上になった局から追加の根を作る |
| `--chance-back <N>` | 6 | 閾値に初めて届いた手の何手前を根にするか (0 以上)。起点より前には戻らない |
| `--priority-early` / `--priority-late` | 2 / 100 | early・sweep・chance の固定優先度と late の優先度の始まり。小さい値から処理 |
| `--sweep-loser` | off | 先手全勝の起点から全敗した後手側 (`own=w`) の根を作る。優先度は `--priority-early`、表の class は `sweep` |
| `--opp-freq` | なし | `master walk` と同じ、手数を除いた SFEN 順のテキスト頻度定跡。手の count の合計を根の局数とする |
| `--opp-book` | なし | `master walk` と同じ `.db` / `.ybb` の大きい定跡。根に手があるかを調べる |
| `--exclude-queue` | なし、繰り返し可 | `master walk` と同じ除外。id ごとに最初の行の根と `own` に固定して違う行は飛ばし、有効な行の `game` を合わせる (段数などそれ以外の欄は最後の有効な行)。段数 0 は除外に使わない。根・追加の根と側、および game の depth+1 手以内の自分の手番の局面と側が一致する行を除く |
| `--update-out <JSONL>` | なし | `--exclude-queue` が必要。同じ根・同じ own の既存行に不足する game を足した完全な行を出す |
| `--update-sources <TSV>` | なし | `--update-out` が必要。更新行ごとの `id`・`source`・`added` を見出し付き TSV に出す |

`--exclude-queue` は受付の順に渡してください。`--update-out` は、同じ id の行の `game` を全部合わせ、
最初に現れた順で同じ手順の重複を除きます。最初の行と `root` (局面と手数) か `own` が違う行は serve が拒否する行なので、合わせる対象にも最後の欄の元にもせず、警告を出して飛ばします。既存の木との重複を見る除外の判定も同じ規則で、有効な行の `game` を全部合わせて使います。それ以外の欄 (`priority`・`hold`・`depth`・`opts`・`memo` など) は、
指定ファイル順・行順で最後の行の値を使います。depth 1 以上で、
`root` の局面 (SFEN 手数は不問) と `own` の文字列が種と一致する行が対象です。
`own=bw` と種の `b` / `w` は一致しません。追加の根や `game` の途中の局面だけで一致する行は、従来どおり除外するだけです。
既存行の `id`・`root`・`roots`・`own`・`depth`・`priority`・`hold`・`opts` を保持し、
統合した `game` の手順と順序を保持します。種の手順は既存 depth+1 手まで集計し、局数降順、同数なら USI 手順の文字列順で
先頭 `--max-lines` 本を選び、統合した `game` の最初の depth+1 手と一致しないものだけを末尾に足します。
「追加 N 本」は統合後に新しく足した本数です。追加本数を memo の末尾にも付け、追加がなければその行を出しません。更新行は id 順です。
標準出力に更新 id ごとの元ファイルのパスと追加本数を出します。
`--update-sources` を指定すると同じ内容を `id<TAB>source<TAB>added` の TSV に id 順で保存します。
`source` は指定順で最後の有効な既存行を読んだ `--exclude-queue` のパス、`added` は新しく足した進行の本数です。
追加のない id は含めず、更新行がなければ見出しだけを出します。
元ファイルがある台の state に、その id の更新行だけを `tree enqueue --from` で積み直してください。
複数台の更新行を一つの state にまとめて投入しないでください。

```sh
book_mine tree seeds --games matches/games.jsonl --out seeds.jsonl --table seeds.tsv --tag holes \
  --exclude-queue machine-a/targets.jsonl --exclude-queue machine-b/targets.jsonl \
  --update-out updates.jsonl --update-sources update-sources.tsv
```

結果の有効性は `analyze_selfplay` と同じペア受領処理を使います。ファイルごとに `(pair_index, attempt)` の
両スロットが揃い、どちらにも `error=true` がない組だけを使い、重複スロットは最初の結果を保持します。
`outcome=in_progress` の局は種から除外し、その件数を標準出力に警告として出します。同じペアの完了局は使います。
片スロットだけの組や失敗した attempt のもう一方の局も使いません。pair の情報がない旧形式では result の
出現順の `seq/2`・`seq%2` にフォールバックします。並列ログでは明示 pair のある現行形式を使ってください。
複数の成功 attempt があれば analyzer と同じくそれぞれを使います。ファイルをまたぐペアの結合はしません。

オフセットの局数検査を通した後、根の局面と `own` ごとに合流をまとめます。根の SFEN 手数は最小値です。
同じ局が複数の経路で同じ根に来ても局数は 1 と数えます。id は walk と同じ
`<tag>_<根のSFEN手数>_<局面のSHA-256の先頭16桁>_<b|w>` です。

局ごとに、起点から指定手数以内に終局したか、その範囲の `eval.last_exact_primary.score` が
最終的に負けた側から見て `-collapse-cp` 以下なら early と判定します。引分の局は、どちらかの側から見て
その閾値以下になれば early です。同じ局から作る種には own によらず同じ判定を使い、
根を通る種の局のどれかが early なら、その根と own の行を early にします。
先手全勝の起点から採用した種があれば、合流後も class は `sweep` とし、early と同じ優先度・並びで扱います。
chance の根も同じ局面・own の根に合流し、手順の選択、除外、積み直し、外部定跡の重みは既存の根と同じです。
class は `chance`、優先度は `--priority-early` です。early・sweep と合流すれば既存の class を保持し、late との合流では chance にします。
`--sweep-loser`・`--chance-cp` 指定時は標準出力の先後別の要約にもそれぞれ sweep・chance の行数を追加します。
評価値は指した側からの値を先後に合わせて使い、両エンジンの読みを対象にします。起点の読みを距離 0 とし、
距離 collapse-plies の読みも含めます。終局は起点の手を 1 手目として数えます (result の `plies`、省略時は最後の move の `ply`)。詰みは通常の cp より強い
勝ち負けとして扱い、exact primary のない eval は崩れの判定には使いません。

残りは late で、重みは **頻度定跡の根の局数 + 大きい定跡に根の手があれば 1** です。
外部定跡を指定したときは、出力する late 全体を重み降順 (同数は own、局面キー順) に並べ、
0 始まりの順位を priority-late に足します。どちらも指定しないときは late の優先度は一定です。
優先度がキューの上限 1000000 を超える指定はエラーになります。

TSV の列は `own`、`root_ply` (SFEN 手数)、`priority`、`class` (early/late/sweep/chance)、`games` (根を通る種の局数)、
`key` (手数なし SFEN)、`game_ids`、`weight` です。`game_ids` は `ファイル番号:game_id` の文字列順で
先頭の最大 8 件をカンマで並べ、`;total=総局数` を付けます (例: `0:1,0:10;total=2`)。
全 ID の一覧ではなく決定的な見本です。総局数は `games` 列と同じです。
ファイル番号は正準パスにして重複を除き、その文字列順に並べた入力の 0 始まりの番号です。
除外した根は TSV にも出しません。標準出力には側ごとの除外数・class ごとの出力行数と合計を出します。

入力は行単位で走査し、結果の選別、全勝起点の判定と接頭手順の局数検査、根の集計のため計 3 回読みます。
結果の選別後は有効なスロットの ID と結果行のバイト位置だけを残し、結果本体は終局行で読み直します。
同じ ID の除外結果が先行していても、最初の手順と採用した結果を組み合わせます。
保持するのはペア受領の状態と有効 ID・結果行の位置、終局していない局の手順、起点・接頭手順・根・手順の種類ごとの集計です。
根ごとには局数と最大 8 件の ID、手順ごとには局数だけを保持します。根や手順が同じ局を追加しても、
その集計が保持する ID の数は増えません。出力は入力が同じならバイト一致します。

### 世代と出力先

対象の出力は `<out-root>/<id>/<own>/g<世代>/` です。前の世代の dir は残し、その `STATUS` を `SUPERSEDED` にします (中身は消しません)。後段 (登録) には `book_mine tree wait` の出力か `targets.status.json` で、**今の世代かつ `ready=true`** の出力先だけを渡してください。各世代の dir には記録として根と手順の `paths.record.tsv` (paths.tsv 形式) を書きます。serve はこれを入力に使いません。replay に使うときは dir の外へ写してから指定します。

serve は出力の根の `serve.lock` を終了まで保持し、根の下はその serve だけが書きます (対象の数によらず lock は 1 つ)。同じ出力の根で 2 本目の serve は起動できません。`tree replay` は出力先の `tree.lock` を取り、上の階層に `serve.lock` があればそれも取ってから書くので、serve が動いている間は根の下へ replay できません。`serve.lock` がシンボリックリンクなど通常のファイルでなければ、serve も replay も拒否します。serve は世代の dir を用意する (前の世代を SUPERSEDED にする) ときにその dir の `tree.lock` を一時的に取るので、serve より前に始まった replay が終わるまでその対象を用意しません。前の世代の dir の SUPERSEDED は書けなければ次の周回でやり直し、消された前の世代の dir は作り直しません。入力の照合に成功した後のエンジン起動失敗は局面の失敗に数えず、読みが成功するまで ALERT に 1 回だけ書いて、次の周回で起動し直します。入力ファイルの消失や読取り失敗 (開けるファイル数の上限を含む) は再試行せず、serve 全体を非0で停止します。出力の根が台帳・state の中にあれば起動を拒否し、対象の出力先が台帳・state と重なる行と、出力先の途中・生成物がシンボリックリンクの行は拒否します。

### 受け付けとチェックポイント

`<state>/accepted.json` がチェックポイントです。有効な定義 (`id`・手番・解決済みの定義・出力先・世代・受付順・priority・hold)、キューを処理した位置 `processed_bytes` とそこまでの sha256、確定した拒否の一覧を、新しい行を処理するたびに 1 回の原子的な書き込みで確定します。副作用 (前の世代の SUPERSEDED、新しい世代の dir・lock・`QUEUED`、列の作り直し) はチェックポイントの後に行い、どれも冪等です。チェックポイントの前に落ちた行は次の起動で処理し直し、後に落ちたら副作用だけをやり直します。過去の行は再適用しません。inbox の TOML で受け付けていた頃の state (`accepted.json` が配列) からは再開できません。新しい state dir で起動し、古い台帳を `--journal` で指定してください。

列は局面ごとに、それを求める保留でない木の `(priority, 受付順, 段)` の最小を選び、`(priority, 受付順, 段, key)` の順に並べます。受付順は `id` が最初に受け付けられた順で、世代を変えても変わりません。空いた worker に先頭から割り当て、読みかけの worker は取り戻しません (優先度の高い対象の次の局面は、実行中の読みが終わるまで待つことがあります)。同じ局面の要求は優先する木の条件から読み、異なる条件の読みは直列に実行します。`require_condition:false` の木どうしでは 1 回の読みを共有し、`true` の木にはその木の条件で読んだ結果が必要です。

### 状態と待機

`<state>/targets.status.json` は 1 回の原子的な書き込みで出す 1 つの JSON です: `run_id`、`processed_bytes`、`processed_sha256`、`all_ended`、`targets` (対象ごとに `id`・`own`・`generation`・`priority`・`hold`・`state`・`ready`・`read` (読んだ局面数)・`required` (要る局面数)・`out`)、`rejected`。`state` は `RESUMING / QUEUED / RUNNING / HELD / DONE / NOT_READY / FAIL`。`queue_error` はキューの受付が止まっている理由です (無ければ null)。`all_ended` は有効な木がすべて `DONE / NOT_READY / FAIL / HELD` のときに真 (0 本でも真) で、worker が止まったことは意味しません。serve は起動のたびに `run_id` (時刻と PID) を作り、起動ごとの lock `<state>/run/<run_id>.lock` を取って終了まで保持します (前の起動の lock ファイルは起動時に消します)。状態のファイルはこの lock を取った後にしか今の `run_id` で書きません。出力に触れる前にこのファイルを全対象 `RESUMING`・`ready=false` で書き直します (書けなければ起動をやめます)。`ready=true` はその世代の出力一式が揃ってから出します。書き込みに失敗したら ALERT に書いて次の周回でやり直します。

```bash
receipt=$(book_mine tree enqueue --state "$STATE" --from targets.jsonl)
book_mine tree wait --state "$STATE" --receipt "$receipt" [--ids p12_w,p11_b]
```

`wait` は次を同じ 1 回の読み取りで満たしたときに、対象ごとの `id`・`own`・`generation`・`state`・`ready`・`out` を JSON で出して 0 で終わります: `processed_bytes` が受領の `end` 以上、受領の範囲 `[start, end)` に拒否が無い、`all_ended` が真 (`--ids` を付けたらその対象と受領の範囲で足した `id` が終わった状態)、状態のファイルを書いた serve 自身が生きている (状態の `run_id` の起動ごとの lock が取られている)。受領の範囲に拒否があればその理由を出して非 0、serve が落ちていれば非 0 で終わります。範囲の外の拒否 (他の書き手の行や同じ `id` の過去の拒否) は見ません。受領の範囲が未処理のまま `queue_error` が 2 秒続けば、その理由を出して非 0 で終わります (チェックポイントを一時的に書けない場合も含みます。直した後に待ち直してください)。`--ids` に状態に無い id を渡すと (打ち間違いや、別の受領で足してまだ処理されていない id)、待たずに非 0 で終わります。状態のファイルが前の起動のもの (その `run_id` の lock が取られていない) なら、state の lock で新しい serve が動いているかを確かめ、動いていれば新しい状態が出るまで待ち、動いていなければ非 0 で終わります。lock は共有 lock で一瞬試すだけです (serve は起動時に state の lock を 1 秒ほど待ち直すので、待機と起動が重なっても起動は失敗しません)。

### `tree status`: 複数の台の進みと idle までの時間

```bash
book_mine tree status --state state-a --state state-b \
  --journal ledger-a.jsonl --journal ledger-b.jsonl
book_mine tree status --state state-a --state state-b --json --stale-hours 2
```

`--state` の順に1行ずつ、最後に合計を出します。既定は Markdown の表、`--json` は1行1オブジェクトの JSONL です。
`--journal` は全体を省略するか、`--state` と同数を同じ順序で指定します。省略時は速さ・時間が `-` (JSON では `null`) になります。

- engine 稼働/全: `progress.json` の `key` がある worker 数 / 全 worker 数。
- 木: 読み中 (RUNNING) / 待ち (QUEUED) / 終わった (DONE + NOT_READY、括弧内はうち読めずに終わった NOT_READY) / 保留 (HELD)。NOT_READY は ready にならず終了した木で、待機・局面・残り・depth・idleまでの時間の計算には含めません。
- 局面: active な木 (RUNNING、QUEUED) の `read` / `required` の合計。可視の残りは各木の `max(required - read, 0)` の合計。
- 増加を含む残り: 指定した全stateの DONE の `read` を実績として共有します。同じ `(id, own)` は1件とし (`own=bw` の先後は別の木)、最大の `read` を持つ実績のdepth・世代を使います (同値なら指定順で最初の実績)。対象の木と `(depth, generation >= 2)` が同じで、`read >= 対象の required` の実績が10件以上あれば、その平均と80パーセンタイル (`sorted[int(0.8 * n)]`) を終了時の局面数とします。10件未満なら同じdepthの全世代でやり直し、それも不足なら `required` を使います。各木の `max(予測局面数, required) - read` を0以上として合計します。
- 局面/時: 台帳の末尾から `provenance = "engine"` の有効な最新400件 (不足時はある分) の `reading.sec` の平均で、`worker総数 × 3600 / 平均秒` を計算します。prefetch・speculative の読みも含むので、実際に木が進む速さとしてはやや楽観的です。不正なJSON・形式不正・負のsecの行は飛ばします。サンプルなし・平均秒が0なら速さは不明です。
- 1 engine の NPS: 局面/時と同じ最新400件のうち、`reading.nodes` があり `reading.sec > 0` の行で `nodes / sec` の中央値を取ります (偶数件は中央2件の平均)。該当する行がなければ不明です。nodes の無い行も局面/時には含めます。
- 合計 NPS: 1 engine の NPS × 全 worker 数です。NPS は読む木ごとのノード上限によらず台の探索速度を表し、局面/時は読む木のノード上限に左右されます。どちらも prefetch・speculative の読みを含みます。NPS は100万単位、小数2桁で `0.75M` のように表示し、不明なら `-` です。
- idleまでの時間: 増加を含む残り (平均 / p80) を局面/時で割った時間です。合計は各台の残りの和 / 速さの和です。台ごとの終了時間の最大値ではなく、仕事を均等に配れる場合の目安です。合計対象に速さ不明の台があれば合計の速さ・時間も不明です。
- 深さ×木の数: `accepted.json` の受付済み定義を `(id, own, generation)` で照合し、active な木の深さごとの数を表示します。DONE の実績の深さにも同じ定義を使います。世代が一致しない木は深さ不明です。`accepted.json` が欠損・読めない場合は `targets.jsonl` の同じ id の最初の有効行を近似値として使い、状態欄と JSON の `reason` にその旨を出します。この近似では世代ごとの深さ変更は反映できません。キューの不正行・8 MiB を超える行・改行のない末尾は飛ばし、取得できない木は `不明` に入れます。

表では伸びを見込む残り局面数を整数に丸め、時間は小数第1位まで表示します。JSON の数値の精度は変えません。

小さい木が先に完了するため、DONEに基づく増加推定は低めに偏ります。p80も終了時刻を80%の確率で保証する値ではありません。
読み取りはserveとロックを共有しないため、各ファイルの更新の間には短いずれがあり得ます。
台帳は64 KiBずつ末尾から読み、全体をメモリに載せません (1行は最大8 MiB)。末尾に改行がない行は完全なJSONとして読める場合だけ対象にします。

`targets.status.json`・`progress.json` や指定した台帳の欠損・読み取りエラーは `unreadable` と理由をその台の行に出し、他の台の処理を続けて終了コード0で終わります。
`progress.json` の更新から `--stale-hours` (既定2時間、非負数) を超えた台は `stale` と表示します。
`stale`・`unreadable` は合計から除外します。合計 NPS は対象の台の合計 NPS を足し、合計行の 1 engine の NPS はそれを対象の全 worker 数で割ります。対象の台に NPS 不明があれば合計の NPS も不明で、worker 総数が0なら 1 engine の NPS は不明です。合計対象が0台なら合計行のstatusは `empty` で、速さ・時間は不明です。読み取れたstaleな台のDONEも推定の実績には含めます。

JSONLのキーは全行共通です。`kind` は `state` / `total`、`state` は指定パス (合計は `null`)、`status` は `ok` / `stale` / `unreadable` / `empty` (合計のみ)、`reason` はエラー理由または深さの近似値を使った通知 (通常は `null`) です。
数値は `workers_busy`, `workers_total`, `trees_running`, `trees_waiting`, `trees_done`, `not_ready`, `trees_held`, `positions_read`, `positions_required`, `remaining_visible`, `remaining_mean`, `remaining_p80`, `positions_per_hour`, `nps_per_engine`, `nps_total`, `hours_until_idle_mean`, `hours_until_idle_p80`。
`nps_per_engine`・`nps_total` は nodes/秒の数値で、不明なら `null` です。
`depth_histogram` はdepth文字列 (`不明` を含む) をキー、木数を値にしたオブジェクトです。`unreadable` の個数欄は0で、合計には使いません。

### `tree queue-review`: 台ごとの残りと未着手の木を点検する

複数の台の受付と進みの写しを読み、締めまでに読める量、未着手の木の到達の重みと重複、
保留候補の行を出します。受付や稼働中の serve には書き込みません。

```sh
book_mine tree queue-review \
  --box box-a=/path/to/box-a --box box-b=/path/to/box-b \
  --rate box-a=1.44 --box-rate box-b=20 \
  --cutoff 2026-12-01T09:00:00+09:00 \
  --walk-table b=walk-black.tsv --walk-table w=walk-white.tsv \
  --table queue-review.tsv --boxes queue-boxes.tsv --hold-out hold-candidates
```

各台の dir には `enqueue/*.jsonl` と、その受付の受領 `<file>.jsonl.receipt`
(`{"start":0,"end":100,"lines":1}` の形)、`fetch/progress.json` を置きます。
受領名は `<file>.receipt` でも読めます。同じ JSONL に両方の受領名がある場合はエラーです。
受領のない JSONL は読みません。受領の `start` が小さい順に読み、同じ id は最後の行を使います。
受領の範囲の重なり・行数違い、受付と progress の対象・own・priority・hold の不一致はエラーです。
受付と progress は同じ時点の写しを用意してください。
`fetch/progress.json` が無い台 (立てた直後で最初の回収がまだの台) は、読んでいる木が 0 本の空の台として扱い、
受付の木をすべて未着手 (read 0) として数えます。その旨を標準エラーに 1 行出します。worker 数が分からないので、
`--box-rate` を渡さない限り速さは 0 で、`--plan-out` では `--box-rate` が無いとエラーにします。progress がある台の受付と progress の不一致は、上のとおりエラーのままです。
受付の受領の範囲に serve が拒否した行がないことが前提です。`tree wait` などで確かめてください。

| 引数 | 既定 | 内容 |
| --- | --- | --- |
| `--box <NAME>=<DIR>` | 必須、繰り返し可 | 台の名前と写しの dir。名前は英数字・`_`・`-` |
| `--rate <NAME>=<N>` | `--default-rate` | worker あたりの局面/時 |
| `--default-rate <N>` | 1.44 | 台別に指定しない worker の速さ |
| `--box-rate <NAME>=<N>` | なし | 台全体の局面/時。worker 数と `--rate` による計算を上書き |
| `--cutoff <RFC3339>` | 必須 | 締めの時刻。実行時点からの時間を使い、過ぎていれば 0 時間 |
| `--tree-size <N>` | 92 | 木ごとの局面数の仮定 (正数) |
| `--walk-table <b\|w>=<TSV>` | なし、繰り返し可 | `master walk --table` の表。同じ側の複数表は重みを加算 |
| `--start-moves <USI>` | 初期局面 | 初期局面からの開始手順。表・memo がこの手順で始まる場合は二重に指さず、相対手順なら前に足す |
| `--value-floor <W>` | 0.001 | reach-high と reach-low、および再採点の C と D の境界 (以上が high / C) |
| `--table <TSV>` | 通常の点検では必須 | 未着手の根ごとの分類表 |
| `--boxes <TSV>` | 通常の点検では必須 | 台ごとの残りと読める量の表 |
| `--hold-out <DIR>` | 通常の点検では必須 | 台ごとの hold 候補の JSON Lines を出す dir |
| `--prefix <NAME>` | qrhold | hold・計画・再採点・続きの JSONL と帯の表の名前の先頭。英数字・`_`・`-` |
| `--hold-class <CLASS>` | 追加なし、繰り返し可 | reach-low に加えて保留候補にする class |
| `--plan-out <DIR>` | なし | 指定時だけ、台ごとの積み荷 `<prefix>_<box>.jsonl` と `<prefix>_plan.tsv` を作る。hold 候補とは別の出力先を使う |
| `--master <TSV>` | なし | 再採点に必須の元データ (`moves.tsv`) |
| `--rerank-table <TSV>` | なし | 全台の未完了・未失敗の木を再採点した表 |
| `--rerank-out <DIR>` | なし | 台ごとの更新行 `<prefix>_rerank_<box>.jsonl`。priority・hold・group のどれかが変わる行だけを出力。`--add` があれば `<prefix>_rerank-add.jsonl` も出す。`--hold-out`・`--plan-out` と併用不可 |
| `--rerank-keep-below <N>` | 10 | 現在の priority が N 未満なら保持し、新たな hold を付けない。hold 解除の候補には含める |
| `--rerank-tiers <TSV>` | なし | 再採点の後に当てる段の表 ([段の表](#段の表---rerank-tiers))。`--rerank-table`・`--rerank-out` のどちらかが必要 |
| `--boost-queue <JSONL>` | なし、繰り返し可 | `tree seeds` などの行の root・roots を重要度が上がった局面として扱う |
| `--group-table <TSV>` | なし | 受付行の `group` ごとの、締めまでに読み切る見込みの表。再採点と一緒なら再採点後の見込みも出す |
| `--not-ready-out <DIR>` | なし | NOT_READY の続きの候補を台ごとの `<prefix>_not_ready_<box>.jsonl` に出す。`--not-ready-table` と一緒に指定 |
| `--not-ready-table <TSV>` | なし | 全 NOT_READY の木の判定表。`--not-ready-out` と一緒に指定 |
| `--not-ready-sent <JSONL>` | なし、繰り返し可 | 直前に送った続きの行。このファイル内の id は再送候補から外す。`--not-ready-out` が必要 |
| `--band-per-tree` | 無効 | 受付の RUNNING・QUEUED の木を priority 順に 1 本ずつ数えた累積の帯。出力先と列は下記参照 |
| `--target-black <cp>` / `--target-white <cp>` | 50 / 80 | walk の出口の目標。先手は先手評価が black 以上、後手は white 未満なら未達扱いにしない |
| `--max-ratio <F>` | 0.95 | 計画の残り/読める量の上限。有限の非負数 |
| `--box-deadlines` | 無効 | 台の dir の `deadline` (Unix 秒の整数 1 行) と cutoff の早い方を締めにする。無い台は cutoff |
| `--condition <NAME>=<LABEL>` | 空ラベル、繰り返し可 | 台の読み条件のラベル。同じ条件の台には同じラベルを付ける |
| `--add <JSONL>` | なし、繰り返し可 | 新規の木の行。`tree enqueue --from` と同じ形。計画で割り振るか、再採点で既存の木と並べて採点する (`--plan-out`・`--rerank-table`・`--rerank-out` のどれかが必要) |
| `--update <JSONL>` | なし、繰り返し可 | 既存の木への足し込み行。`tree seeds --update-out` の出力など |
| `--bands <P,...>` | 9,26,60,120 | 計画の帯の上端の priority (小さい順)。`<prefix>_bands.tsv` の区切り |
| `--balance-priority <P>` / `--balance-hours <H>` | なし | 両方で指定。P 以下の帯の読み終わり時間が H 時間を超える台から木を移す |
| `--move-max-ratio <F>` | 1.0 | move・balance の移し先の、移した後の台全体の残り/読める量の上限。`inf` で外す |
| `--move-band-factor <F>` | 1.0 | move・balance の移し先の帯の読み終わり時間の上限を、移す前の全台の同じ帯の読み終わり時間の最大の F 倍にする。`inf` で外す |
| `--update-size <N>` | 30 | hold false の台へ足し込みを送る id あたりの追加局面数の仮定 |

`--update`・`--condition`・`--balance-priority` と、明示指定する `--max-ratio`・`--update-size`・`--bands`・`--move-max-ratio`・`--move-band-factor` には `--plan-out` が必要です。
`--box-deadlines` は計画を出さない場合にも `--boxes` の hours・capacity・ratio に反映します。

`--not-ready-out` と `--band-per-tree` は、それぞれ通常の点検引数を省略して単独で使えます。
この 2 つを指定すると、各台の `fetch/targets.status.json` も読みます。受付と status の id・own を照合し、
priority と hold は受付の最後の行を使います。速さは通常の点検と同じ `--box-rate`、または
`--rate` / `--default-rate` と worker 数から求めます。実測値を使う場合は `--box-rate <NAME>=<局面/時>` で渡してください。

```sh
book_mine tree queue-review \
  --box box-a=/path/to/box-a --box-rate box-a=20 \
  --cutoff 2026-12-01T09:00:00+09:00 --tree-size 150 \
  --not-ready-out continuations --not-ready-table not-ready.tsv \
  --not-ready-sent previous-continuations.jsonl --band-per-tree --prefix next
```

続きの候補は hold でない NOT_READY の木です。受付の最後の JSON 行を写し、`opts.rounds` だけを 6 にします。
同じ主根 (`root`)・同じ own の DONE、または稼働中の台で hold でない RUNNING・QUEUED の写しがあれば「手当て済み」です。
追加の根 (`roots`) だけの一致では手当て済みにしません。
台の dir に `END` があれば稼働中には数えません。照合対象は `--box` で渡した台です。判定する木の台が終わっていたら「終わった台」として送りません。
終わった台で DONE になった写しを手当て済みに数えるには、終わった台も `--box` に渡します
(その台の `fetch/targets.status.json` と受付が揃っている必要があります)。
`startpos moves ...` の根は手順を指して局面に直し、通常の queue-review と同じ局面キーで照合します。
受付の `opts` が無い木、既に rounds が 6 以上の木、`--not-ready-sent` の id も候補から外します。
その木の priority 以下の受付の帯に `tree-size` を足した時間が cutoff を越える木は「帯で回らない」です。
own が `bw` の受付行は両側を判定し、両方とも「送る」のときだけ 1 行出します。
片側だけが候補なら表に「片側だけ候補」と記し、JSONL には出しません。
`--plan-out` と併用し、計画と続きの JSONL の両方に同じ id が出る場合は、競合として全出力の書き込み前にエラーにします (宛先の台が違う場合も含みます)。
計画を送信し、受付の写しを更新してから続きの候補を作ってください。
NOT_READY ごとに独立に判定し、同時に選ばれた別の続きの候補の量は足しません。
表には hold の木も含め、判定・理由・台・id・own・priority・read・required・rounds・帯の量と時間・締めまでの時間・手当て済みの写しを出します。

`--band-per-tree` の表は `<prefix>_band_per_tree.tsv` です。出力先は `--plan-out`、`--rerank-out`、
`--not-ready-out`、`--hold-out` の順で最初に指定された dir です。それらが無ければ
`--group-table`、`--rerank-table` の順で最初に指定された表と同じ dir に出します。
単独で帯だけを出す場合は標準出力が TSV になります。
hold でない RUNNING・QUEUED の各木を受付の priority 順に並べ、
`max(tree-size, required) - read` (負になる場合は 0) の累積を速さで割ります。
同じ priority の木は status の並び順を保ちます。その priority 以下の全量は、同じ priority の最後の行で読めます。
列は `phase`・`box`・`id`・`own`・`priority`・`source`・`remaining`・`cumulative`・`rate`・`hours`・`cutoff_hours`・`over` です。
`source=accepted` は受付の木、`source=send` は送る予定の行、`over` は締めを越えるかの真偽です。
`phase=before` が受付の帯です。`--plan-out`、`--rerank-out` または `--not-ready-out` と併用すると、
送る行を反映した `phase=after-send` も出します。同じ id・own の受付分は送る行で置き換え、
hold を付ける行なら受付分を外します。同じ id・own を複数回送る場合は最後の行を使います。
送る行は own ごとに 1 本 `tree-size` です。ただし同じ id・own の status がある行は、
read の値によらず `max(tree-size, required) - read` (負なら 0) を使います。
送る行は出力の種別ごとに持つ宛先の台で照合します。台名が `a` と `not_ready_a`、`rerank_a` のように重なっても、別の台の行は加算しません。
宛先が未定の `<prefix>_rerank-add.jsonl` は、台別の帯には加算しません。
通常の `--bands` による帯の区切りと、その計画判定は変わりません。

計画の例 (通常の点検引数に追加):

```sh
--plan-out dispatch --prefix batch01 --max-ratio 0.95 --box-deadlines \
--condition box-a=model-a --condition box-b=model-a \
--add new-trees.jsonl --update extra-lines.jsonl --update-size 30
```

再採点は `--rerank-table`・`--rerank-out` の片方だけでも使えます。`--master <moves.tsv>` と、再採点の対象の木の own ごとの `--walk-table b=...` / `--walk-table w=...` が必要です。対象に b と w の両方があれば両側の表を指定してください。対象の側の表が無い場合はエラーにします。
`--rerank-out` は `--hold-out`・`--plan-out` と併用できません。同じ最新の受付行から作る更新を順に積むと、後の行が前の priority・hold を上書きするためです。別々に実行し、先の更新を受け付けた後の受付を次の入力に使ってください。`--rerank-table` だけなら点検・計画と併用できます。
再採点だけなら `--table`・`--boxes`・`--hold-out` を省略できます。通常の点検を出す場合はこの 3 つを揃えます。

```sh
book_mine tree queue-review \
  --box box-a=/path/to/box-a --box box-b=/path/to/box-b \
  --cutoff 2026-12-01T09:00:00+09:00 \
  --walk-table b=walk-black.tsv --walk-table w=walk-white.tsv \
  --master moves.tsv --rerank-table rerank.tsv --rerank-out rerank --boost-queue seeds.jsonl
```

再採点は memo の手順を使わず、受付の root の局面と progress の own で判定します。
reach は同じ側の walk の到達重み、failing はその局面を通る目標未達の行の重みの合計です。
branch は元データの逆辺を根から最大 16 手たどり、同じ own の walk に重みのある局面が見つかったときの補完重みです。
own の手番では main・alt の辺だけ、相手の手番では record を含むすべての辺をたどります。
到達した局面の reach に、経路中の own の alt 1 回につき 0.5 を掛け、候補の最大を採用します。
重みのある局面に着いた経路はそこで止めます。根自体に重みがあれば branch=reach です。
branch_ply は採用した局面から根までの手数です。同じ重みなら短い経路を採用し、候補がなければ両列とも 0 です。
すべての木の逆探索を段ごとにまとめ、各段で元データを逐次読みします（最大 16 回）。元データ全体の逆辺は保持せず、探索中の局面と木ごとの訪問済み局面・alt 数だけを保持し、alt 数が減る場合だけ再訪します。
区分の判定には `effective = max(reach, branch)` を使います。
未達は `entry_value_black` が空でなく、b の表で target-black 未満、w の表で target-white 以上の場合です。
列がない古い walk 表は failing=0 として扱います。開始手順より前の局面は集計に含めません。

| class | 条件 (上から優先) | priority の基準 |
| --- | --- | --- |
| A | failing ≥ 0.005、または boost の根 | 10 |
| B | effective ≥ 0.005 | 40 |
| C | effective ≥ value-floor | 100 |
| D | 上記以外 | 現在値を保持 |

A〜C の priority は `min(現在値, 基準 + min(root の手数 / 5 の切り捨て, 19))` とし、数字を小さくする変更だけを提案します。
D の priority は保持します。
居場所と棚上げは finished・failed を含む全台の行から判定します。同じ id・own がどこかの台で finished または failed なら、その側は表にも更新行にも出しません。
同じ id・own が複数台にある場合、hold でない台を居場所とし、他の台の hold 行は表にも出しません。
全台で hold なら棚上げとし、`--box` の指定順で最初の台だけを対象にします。
棚上げの未着手の A〜C は hold を外します。ただし、別の id で根の局面 (盤・手番・持ち駒、`root` の局面だけで `roots` は見ない) と own の側が同じ木 (名前を変えて移した写し、同じ台のものも含む) が
`--box` のどれかの受付か `--add` にあり、その写しが hold でない (読んでいる・読み待ち) か finished・failed なら hold を外しません。
写しがどれも hold なら、`--box` の順 (同じ台の中は progress の順、`--add` は最後) で最初の木だけを外す候補にし、ほかは hold のまま残します。
その最初の木が読み途中で hold を保持する場合や D の場合は、どの写しの hold も外れません。
`own=bw` の行は先後それぞれの側で照合し、片側にでも写しがあれば (その側の区分によらず) 受付行ごと hold を残します。
照合は `--box` に渡した台だけで行うので、写しが渡していない台にあると見つかりません。D に新たな hold を付けるのは、read=0・hold=false・effective=0 で、
現在の priority が `--rerank-keep-below` 以上の場合だけです。
read > 0、または worker の key がその木の roots の局面に一致する木は読み途中とみなし、hold を保持します。更新行は最新の受付行の priority・hold だけを変えた写しです。
`--add` の木も再採点の対象にし、既存の木と同じ区分・priority の規則で採点します。新しい木は居場所が 1 つの未着手の木として扱い、
hold=true の行は全台 hold の木と同じく棚上げ (A〜C なら hold を外す)、hold=false の行は D かつ effective=0 で priority が `--rerank-keep-below` 以上なら hold を付けます。
受付済みの id や `--add` どうしで重複する id はエラーです。表では box が空、state が new です。
`--rerank-out` には、`--add` の全部の行を priority・hold だけ替えて `<prefix>_rerank-add.jsonl` に出します (変わらない行も出します)。
これを `--plan-out` の `--add` に渡すと、同じ物差しの priority で台へ割り振れます (計画は hold=true の行を読める量に数えず hold のまま置きます)。
`--add` の木も group 揃えに加わるので、同じ group の既存の木の priority が `--add` の木に合わせて小さくなり、台ごとの更新行に出ることがあります。
標準出力の区分・上げる/下げる・hold の数には `--add` の木も入ります。`--group-table` の rerank の行は台にある木だけを数え、`--add` の木は入りません。
同じ `group` の木は、受付行に書く最終の priority・hold (下の `own=bw` と読み途中の規則を当てた後) で、hold でない行の priority の最小にそろえます。
hold の行は最小の計算に入れず、priority も変えません。そろえるのは数字を小さくする変更だけで、`--rerank-keep-below` で保持する木にも当てます。
表の new_priority も、受付行が hold でなければそろえた値です。
`own=bw` は表では先後別に採点します。受付行の priority・hold は共通なので、JSONL は小さい priority を採用し、
hold は両側とも停止可能な場合だけ付けます。片側でも読み途中なら元の hold を保持します。
表と集計は側ごとの提案を示します。

再採点表は `box id own state ply reach branch branch_ply failing boost class old_priority new_priority old_hold new_hold shelved read root group tier rerank_priority copy_of`
の列をタブ区切りで持ち、new_priority・ply・id の昇順です。state は new (`--add` の木)、held、running (読み途中)、queued です。group は更新後の受付行の `group` (無ければ空) です。
tier は当たった段の行の name (段の表が無いか当たらなければ空)、rerank_priority は段を当てる前の priority (再採点と group 揃えの後)、new_priority は段を当てた後の priority です。
copy_of は hold の行で、hold を外さない理由になる写しの `<台>:<id>` です (`--add` の木は台を `add` と書きます)。
標準出力には区分ごとの木の数、priority を上げる (数値を小さくする) / 下げる数とそのうち group でそろえた数、hold を外す / 付ける数を出します。
写しが無ければ hold を外していた受付行があればその数を、段の表があれば段ごとの受付行の数と、hold でないのにどの段にも当たらなかった受付行の数を出します。

#### 段の表 (`--rerank-tiers`)

再採点の結果に、id と区分で決める priority の規則 (段) を当てます。表はタブ区切りで、`#` で始まる行と空行は読み飛ばします。
最初の行は見出し `name match class old_priority priority group` です。

| 列 | 形 | 意味 |
| --- | --- | --- |
| `name` | 空でない文字列 (表の中で重複不可) | 段の名前。再採点表の tier 列に出す |
| `match` | カンマ区切りの id の型 | `*` は任意の文字列で、型は id 全体と照合する。`x58i8*` は接頭辞、`*bridge*` は部分一致、`*` の無い型は id の完全一致 |
| `class` | `A`〜`D` の並び、または `*` | 区分の集合 (`ABC` など)。`own=bw` の行は先後のうち良い方 (A に近い方) の区分で照合する |
| `old_priority` | `lo..hi`・`..hi`・`lo..`、または `*` | 受付の今の priority の範囲 (両端を含む) |
| `priority` | `=N`・`lo..hi`・`..hi`・`lo..`・`keep`・`rerank` | `=N` は N に固定、範囲は再採点の priority をその範囲に収める、`keep` は受付の今の priority のまま、`rerank` は再採点の priority のまま |
| `group` | 空、または group の名前 | 空でなければ受付行の `group` をこの名前に書き換える |

表は上から順に照合し、最初に当たった行だけを使います。どの行にも当たらない受付行は再採点の priority のままです。
段は受付行ごと (台と id ごと) に、hold・group 揃え・読み途中の規則をすべて当てた後に当て、その値で確定します (段の後に group をそろえ直しません)。
hold のまま残る受付行は段を当てず、priority も受付の今の値に戻します。新たに hold を付ける行は hold だけを変えた行になります。`--add` の行にも同じく当てます。
変わらない行を出さない規則は段を当てた後の値で判定します。

まとまりごとに読み切る順に並べ直す例:

```tsv
# 種の id の接頭辞は seed1*,seed2* のように並べる
name	match	class	old_priority	priority	group
as-is	x25k32*,x23top*	*	*	keep	
propagation	*bridge*,*refut*,x49br*	*	..8	=2	
seed	seed1*,seed2*	ABC	*	=3	
seed-d-low	seed1*,seed2*	D	..4	=13	
seed-d	seed1*,seed2*	D	*	keep	
prepared-line	x57kf*	*	*	=6	
small	*	*	..4	=7	
small-8	*	*	..8	=9	
rest	*	*	*	keep	
```

最後の `rest` のように `keep` の行で受け止めると、段に当たらない木は再採点の priority を使わず受付の値のままになります。
再採点で priority を動かさずに段だけを当てたいときは、`--rerank-keep-below` を priority の上限 (1000000) より大きくします。この場合は新たな hold も付きません。

`--group-table` は台ごとに、finished・failed・hold でない木を priority の小さい順 (同点は progress の順) に並べ、
木ごとの残り `max(tree-size, requested) - read` (下限 0) を積み上げて、その台の読める量 (`--boxes` の capacity と同じ) と比べます。
積み上げた後も読める量以内なら full (読み切る見込み)、積み上げる前が読める量未満なら part (途中まで)、それ以外は none (手が付かない) です。
読み途中の木も priority の順に並べるだけで先に置きません。表は `group basis trees held boxes priority_min priority_max full part none remaining`
の列をタブ区切りで持ち、group の昇順です。basis は current (今の受付) と、再採点と一緒のときだけ rerank (再採点の更新行を受け付けた後の priority・hold) の 2 行です。
trees・priority の幅・full/part/none・remaining は hold でない木、held は hold の木の数で、`own=bw` は先後を別の木として数えます。boxes はその group の hold でない木がある台です。
標準出力には basis ごとに group の数と、全部 full・全部 none・混在・全部 hold (hold でない木が無い) の group の数を出します。
`--group-table` だけなら walk 表・元データは要りません。

計画は点検時点の remaining から始め、次の順で作ります。capacity が 0 の台の計画 ratio は無限大です。

1. 足し込みの id を全台の progress で探します。hold false で failed でない台が 1 台ならそこへ、
   2 台以上ならエラーです。0 台なら、その id のある全台へ送り、hold は保持します。
   どこにもなければ `drop-update` にします。送り先の最後の受付行の game・memo だけを置き換え、
   載せ替えた行の合法性を受付と同じ検査で確認し、不正なら送り先の台と id を含むエラーにします。
   hold false の台の見込み量に update-size を id ごとに 1 回加えます。同じ id の足し込みは最後の game・memo を使います。
   failed が無い progress は false として読みます。
serve は priority の小さい順に読むので、木が締めまでに読めるかは、その台の「その木の priority 以下の見込み量」(以下、帯の量) で決まります。
台に入るか・移せるかの判定は、台全体の remaining ではなく帯の量で行います。

2. 上限を超える台を名前順に点検します。未着手の木を reach-low 優先、priority の大きい順、
   同点は id の逆順に見ます。reach-low は元の台で hold にします。それ以外は、その台での帯の量が
   読める量 × max-ratio 以内の木は動かしません (後ろの木を外せば読めるため)。超える木は、移した後に
   移し先での帯の量 + 木の量が上限以内となる他の台のうち、その比が最小の台へ移します。同点は台の名前順です。
   同じ id を受付または計画に既に持つ台は、既存の木を置き換えてしまうため移動先から除きます (hold を含む)。
   元の行は hold true、移し先は hold false にし、定義を変えず世代を増やしません。
   移し先には、移動元の台へ実際に送った最後の足し込みの game・memo も反映します。
   移し先は、後述の移し先の上限も満たす台に限ります。
   移せなければ priority 100 以上は hold、100 未満は動かさず `keep-over` にします。
   元の台が上限以内になったら止めます。hold・移動では、初期の remaining と同じく木ごとの
   `max(tree-size, requested) - read` (下限 0) と、移動元の台に足し込み済みの update-size 分の見込み量を引き、移し先に同量を足します。
   bw は両側とも未着手の場合だけ対象にし、側ごとに求めた見込み量を一緒に扱います。
3. `--balance-priority P --balance-hours H` があれば、P 以下の帯の読み終わり時間 (P 以下の見込み量 / 台の速さ) が H を超える台を
   名前順に見て、P 以下の未着手の木 (reach-low を除く) を priority の大きい順 (同点は id の逆順) に、読み終わり時間が H 以下に収まる他の台へ移します
   (`balance`、理由 `band-hours`)。移し先はその木の帯の量でも上限以内で、移した後の読み終わり時間が最小の台です (同点は名前順)。
   移し先の条件 (同じ id、条件ラベル、同じ根、移し先の上限) と見込み量の扱いは 2 と同じです。元の台の読み終わり時間が H 以下になったら止めます。
4. 新規行を priority の小さい順 (同点はファイルの指定順・行順) に割り振ります。
   追加後に帯の量 + 木の量が上限以内となる台のうち、その比が最小の台、同点は名前順です。
   入る台がなければ、条件を満たす台のうち現在の ratio 最小の台へ hold true で置き (`add-hold`)、
   見込み量は増やしません。hold=true の入力行も同じ台へ hold のまま置きます (`add-hold`、理由 `input-hold`)。受付済みの id や新規行どうしの id の重複はエラーです。
5. 移し込みや新規行で後ろへ押し出された木は動かし直しません。計画の後でも帯の量が上限を超える hold でない未着手の木
   (受付の木・移し込んだ木・新規行。`keep-over` を記録した木を除く) を `over` (理由 `band-over`) として記録します。
   priority 100 以上でも hold にはしません (serve は priority の小さい順に読むので、急ぎの木の妨げにならない)。
   これらは締めまでに読み切れない見込みなので、次の点検で priority・置き場を見直してください。

move・balance の移し先の上限は次の 2 つです。帯の量の判定は移した木の priority 以下しか見ないため、
priority の大きい木から順に移すと、移し先の台全体があふれたり、同じ priority の木が 1 台に集まったりすることを防ぎます。

- 移した後の移し先の台全体の残り/読める量が `--move-max-ratio` 以下。判定は移す時点の量で行います。
  移せずに `keep-over` になりかけた木は、全台を一巡して各台が自分の木を外した後に、もう一度だけ移し先を探します。
  `hold-over` にした木もその後で同じ条件の移し先を探し、見つかれば `move` を記録して移します (元の台では hold のまま)。
- 移した木の priority を含む帯とそれより上の帯 (`--bands` の区切り) のそれぞれで、移した後の移し先の読み終わり時間
  (`<prefix>_bands.tsv` の after_hours と同じく、帯の上端以下の見込み量 / 台の速さ) が、移す前の全台の同じ帯の
  読み終わり時間 (before_hours) の最大 × `--move-band-factor` 以下。速さ 0 の台は最大に含めません。

帯の量が収まる木は動かさないので、計画の後も台全体の ratio (標準出力の「計画 残り/読める量」) が max-ratio を超えることがあります。
締めまでに読めるかは `<prefix>_bands.tsv` の after_hours と cutoff_hours、`over` の行で確かめます。

「同じ根」は root・roots のいずれかの局面が手数を除いて一致し、id が違うことです。
移動の重複判定は全台の受付 (hold を含む) で調べます。新規行は移動後の配置に合わせて割り振ります。移動は、元と同じ条件ラベルの台へ、
または同じ根の別 id が一つもない場合に限り別のラベルの台へ行います。
新規行の条件ラベルは、台ごと・id ごとに計画行があればその行 (hold を含む)、なければ受付行を使い、
その最終状態の hold false の行だけから根ごとに集めます。移動元や保留にした木のラベルは残さず、
`add-hold` の行もラベルを追加しません。
ラベルが 1 種類の根は同じラベルの台に置き、既に複数種類ある根やラベルのない根は条件で縛りません。
hold false で追加した新規行のラベルは後続の割り振りに反映します。
root・roots のすべての根の条件を満たす台がなければエラーです。
止めた台をこの照合から外す場合は、その台を `--box` に渡さないでください。

`<prefix>_plan.tsv` の列は `action / id / own / priority / from / to / reason` です。
action は `update`・`update-held`・`drop-update`・`hold-low`・`move`・`hold-over`・`keep-over`・`balance`・`add`・`add-hold`・`over`。
表は判断の順の記録で、同じ id に複数の行があれば後の行が最終です (例: `keep-over` の後に `balance` で移した木)。

`<prefix>_bands.tsv` は台ごと・帯ごとに `box / priority_lo / priority_hi / before_remaining / before_hours / after_remaining / after_hours / cutoff_hours`
の列を持ちます。帯は `--bands` の上端で区切り (既定 `9,26,60,120` で 0〜9・10〜26・27〜60・61〜120・121〜)、最後の帯の priority_hi は空です。
remaining はその帯の見込み量、hours はその帯の上端以下の見込み量の合計を台の速さで割った読み終わり時間 (速さ 0 で量があれば inf)、
before は点検時点、after は計画の後、cutoff_hours は締めまでの時間です。
台ごとの積み荷はこの順で生成し、同じ id が再び出たら後の行だけを残します。
行の無い台の JSONL は作りません。出力 dir は必要に応じて作り、既存の `<prefix>_*.jsonl`・`<prefix>_plan.tsv`・`<prefix>_bands.tsv` があれば
エラーにして上書きしません。標準出力には各台の計画 ratio の前後を表示します。
計画は写しを前提とするため、投入前に受付・進捗の変化を確認してください。

速さ・重み・下限には有限の非負数を指定します。`--walk-table` を省くと到達の重みは無く、
主根で手順のある木は not-reached または path-mismatch になります。表の `side` 列があれば指定した側の行だけを読みます。
表の各行の `moves` を合法性を確認しながら指し直し、開始局面・途中・止まりの局面に `weight` を足します。
同じ行で同じ局面を再訪しても、その行の重みは 1 回だけ足します。異なる行の重みは加算します。
walk の表は同じ手数・同じ局面への合流をまとめ、`moves` には最初の手順だけを残します。
そのため、途中の局面の重みは目安です (合流で別の手順に載った重みは入らず、代表の手順に載った合流後の重みは入るので、少なくも多くも出ます)。同じ own の `own` の行の止まり (`key`) で、どの行でも途中の局面になっていない根だけを reach-low にし、それ以外 (rare などの止まり、途中の局面) は
重みが下限未満でも reach-partial にします。
`stop=out` の止まりは「外部定跡の外の出口」として別に重みを加算します。

未着手は、finished でも hold でもなく、read が 0 で、どの worker の key もその木の
`root`・`roots` の局面でない木です。局面の比較では SFEN の手数を除きます。
`--table` は `box / id / own / priority / root / class / reach_weight / dup_of` の順です。
追加の根がある木は根ごとに行を出します。`own=bw` の受付は serve と同じく b・w を別々に点検します。

| class | 判定 |
| --- | --- |
| extra-root | `roots` の追加の根。memo の手順による判定は `root` だけに行う |
| seed | memo に `moves=` がない。到達の重みで測らない |
| path-mismatch | memo の手順が非合法、または指し終えた局面が根と違う |
| beyond-out | 正しい memo の手順が、同じ own の walk の out の出口を通る。最初に通った出口の重みを引き継ぐ |
| reach-high | 同じ own の表で根の到達の重みが下限以上 |
| reach-low | 同じ own の表の `own` の行の止まりで、どの行でも途中の局面になっていない根 (重みが確定する) で、到達の重みが下限未満 |
| reach-partial | 到達の重みが下限未満だが確定しない根 (途中の局面、rare などの止まり、途中でも通る止まり) |
| not-reached | 正しい memo の手順があり、出口も根の到達の重みもない |

memo は語の途中 (`start_moves=` など) の `moves=` を除き、`moves=` の後の空白区切りの USI 手順を使い、`;`・`；`・`,`・改行や次の `名前=値`
で終えます。手順の不一致は出口や到達の重みより先に判定します。
`reach_weight` は reach-partial でも算出した値をそのまま示し、extra-root・seed・not-reached・path-mismatch では空欄です。
`dup_of` は同じ根・同じ own を持つ別の台または別 id を `台:id` のカンマ区切りで最大 8 件示し、`;total=N` を付けます。
N は自分を除いた重複の本数です。重複がなければ空欄です。
相手の木は未着手に限らず、finished でも hold でもなければ重複として数えます。

`--boxes` は `box / workers / rate / hours / capacity / remaining / ratio / unstarted` の順です。
`rate` は台全体の局面/時、`capacity = rate × hours`、`remaining` は finished でも hold でもない
各木の `max(tree_size, requested) − read` (負なら 0) の和です。
progress の failed の木も、他の条件を満たせば残りと未着手に数えます。
`ratio = remaining / capacity` で、1 を超えると見積もった量を締めまでに読み切れません。
読める量が 0 で残りがあれば `inf`、両方 0 なら 0 とします。重複の分は残りから引きません。
標準出力にも台ごとの割合と class ごとの本数を出します。複数根の木が異なる class に入る場合は、
それぞれの class に 1 本ずつ数えます。

`--hold-out` には `<prefix>_<NAME>.jsonl` を各台について作ります (候補が無ければ空ファイル)。
既定では reach-low だけを対象にし、最後の受付行の JSON の `hold` だけを true にします。
複数根の木はすべての根、`own=bw` の木は両側が未着手で対象 class に入る場合だけ出力します。
extra-root・seed・beyond-out・not-reached・reach-high・reach-partial・path-mismatch は既定では保留にしません。
今の main から辿れなくなったことだけを理由に、読む価値が低いと判断しないためです。
出力先は台の dir の外に置いてください。入力と同じファイルや出力先どうしの重複はエラーです。
既存の出力ファイルは上書きします。`--hold-out` の前回のファイルは削除しません
(今回と同じ名前のファイルは上書きします)。出した hold 候補を受付に積む操作は、このコマンドでは行いません。

### 並列数・停止・監視

- `<state>/workers` の整数を書き換えて並列数を変更します。初回の既定16。既存の workers ファイルがあれば再起動時も `--workers` より優先します。不正な内容の通知は内容または mtime が変わるまで1回です。減らすと余った worker は読みかけを台帳へ保存してから終了します。0は全 worker の drain で、serve は常駐を続けます。数を戻すと再開します。
- `<state>/stop` を作ると読みかけを保存して serve を終了します。再開前に stop を除き、同じコマンドを実行します。kill 後も、保存済みの読みから木の回を再構築します。台帳の改行の無い末尾だけは切り捨てます。
- 木が全て完了しても `IDLE` で受付を続けます。プロセスの `STATUS` は `<状態> <run_id>` の 1 行です (人が見るためのもので、wait は使いません)。
- `--stall-secs` (既定600) の間 info の nodes / depth のどちらも増えなければ `ALERT` に書きます。`info string` や同じ値の繰り返しでは時計を戻しません。既定では探索を止めません。
- worker の初回起動・再生成では、engine と全ファイル型 USI option (`EvalFile` / `EvalDir`・係数・明示 SPSA ファイルなど)、暗黙の `spsa.params` の有無・内容を開始時の SHA-256 と照合します。起動前、USI 初期化後、初回 `usinewgame` / `isready` 後に検査し、不一致や読取り不能は局面の失敗として再試行せず、ALERT を記録して serve 全体を非0で停止します。その worker の読みは journal に追記しません。原因を解消し、元と同じ内容で再開してください。fingerprint・journal・checkpoint の形式は変わりません。
- serve 稼働中の engine・モデル・パラメータは不変に保ってください。照合は worker 起動時のみで、大きなモデルにも全内容の読取りコストが発生します。ファイルシステムを排他ロックする仕組みではないため、検査の間だけ書き換えて元へ戻す競合や、検査後・稼働中の書換えとエンジン側の遅延読取りを完全には検出できません。内容を変更する場合は serve を終了し、新条件の state を使ってください。
- 固まった worker を明示的に作り直すには `<state>/restart/<worker番号>` を作ります。受付時点でその worker が読んでいる局面を列に戻します。対象の読みが無い番号は ALERT して制御ファイルを消します。明示 restart は自動再起動の上限に数えません。エンジン切断は自動で作り直します。`--max-restarts` (既定3) を超える同一局面の失敗、または再現する読みの検査エラーでは、その局面を待つ木だけを FAIL にして ALERT し、他の木の処理を続けます。serve の MultiPV は1行以上かつ指定数以下を受理し、各手の合法性と重複が無いことを検査します。根の不成省略などで指定数より少なくても受理します。0行の場合は合法手の有無によらず `bestmove resign` / `bestmove win` を受理し、展開しない葉として保存します。1位が `win` の1行だけで `bestmove win` が返る場合も、0行の宣言勝ちと同じ葉として扱います。合法手数や根の候補数から行数は推測しません。
- state の lock と journal 自体の lock を終了まで保持します (Windows でもデータの範囲は lock しないので、serve の実行中に journal を読めます)。別 state でも同じ journal の同時使用はできません。木の出力は出力の根の `serve.lock` で別の serve や replay から保護します。`--journal` を省略すると `<state>/journal.jsonl` を使います。

journal、state dir とその管理ファイル (キュー・lock・チェックポイント・状態のファイルなど) がシンボリックリンクなら、リンク先の有無によらず拒否します。親 dir の途中のリンクと未作成部分の `..` は解決して比較します。journal と state の管理パスの衝突も拒否します。replay では、木の出力 dir（配下の生成物全体）と journal・state・paths.tsv・定義ファイルの衝突を拒否します。

プロセスの `STATUS` は `RUNNING / IDLE / STOPPED / FAIL`。木の出力先の `STATUS` は `QUEUED / RUNNING / HELD / STOPPED / DONE / NOT_READY / FAIL / SUPERSEDED` です。プロセスの STOPPED は workers=0 または stop 指示で読みかけの保存が終わった状態で、待ち列が残っていても使います。木の QUEUED はまだ読み始めていない待機、RUNNING は読み始めた未完了の木、STOPPED は停止指示によりその木の実行中の読みが無くなった状態です。`progress.json` に木 (id・手番・世代・priority・hold) の回・読み済み数・要求数・列の長さと、worker の局面・経過秒・nodes / depth を記録します。`ALERT` は日時付きの追記です。キューの読み取りと出力先の用意の失敗は、内容が変わるまで 1 回通知して次の周回でやり直します。STATUS は内容が変わった場合だけ書きます。終了した木（DONE / NOT_READY / FAIL）の出力 dir を削除しても再作成しません。STATUS・progress.json・ALERT の書き込み失敗は標準エラーへ報告して次の周期で再試行します（終了した木の出力 dir が残っている場合）。journal と成果物の書き込み失敗はエラーとして扱います。

### 動かしたまま別の台帳を取り込む

serve が作る `<state>/import/` に、`tree import-journal --from-journal` で読める台帳の複製を置けます。書き込み中のファイルを受け付けないよう、同じ dir の別の拡張子で書き終えてから、一意な名前の `*.jsonl` に rename してください。公開後は変更しません。

```bash
cp /path/to/combined-journal.jsonl /path/to/state/import/hourly-001.tmp
mv /path/to/state/import/hourly-001.tmp /path/to/state/import/hourly-001.jsonl
```

`.jsonl` 以外は読みません。ファイル名順に 1 本ずつ検査し、エンジン・モデル・SPSA パラメータ・nodes・MultiPV・Hash・千日手の値などの違いにかかわらず、すべての条件の行を元の条件のまま取り込みます。`serve --accept-fingerprint <FP>` は互換性のため受け付けますが、動作を変えない非推奨の指定です (FP は 64 桁の小文字の 16 進数)。各行の指紋や読みの検査に失敗した行と、取り込み元の同じ指紋・同じ局面の先の行と値が食い違う行は飛ばします。JSON の形式不正・台帳の版の不一致・空ファイルは、ファイル全体を追記せずに拒否します。改行で終わらない末尾の行は `import-journal` と同じく無視し、状態の `ignored_tail` に記録します。

検査は別スレッドでストリーミングし、`import/` 内の一時ファイルを使います。局面・値の短いハッシュと入力の行番号を、局面のハッシュで分けたバケットに書き出します。初期分割数は入力サイズから決め、1 バケットが 262,144 局面を超えた場合は再分割するため、重複検査の表は入力行数によらず約 64 MiB 以下に収まります。先読みの印もディスク上で最初の行へ集約します。追記には入力順の一時台帳を使い、同じ値の重複は追記せず、従来どおり元の行数で件数と食い違い一覧を記録します。一時ディスクには対象行のコピーに加え、1 行あたり 42 バイトの検査情報が必要で、再分割中は該当バケット分がさらに必要です (取り込み先の台帳を保持するメモリは別です)。全体の検査後、worker の完了回収と木の進行・読みの割り当ての間に追記します。1 周回で最大 1024 行を読み、読み取りが 5ms に達した場合もそこで区切ります。その周回の行をまとめて追記し、最後に 1 回だけ `sync_data` します。検査・書き込み・同期に要する時間はこの 5ms に加わるため、周回全体の所要時間の上限ではありません。追記した局面を要求している木の先読みだけを再計算し、待っていた木をエンジンで読まずに進めます。

取り込み先に同じ指紋・同じ局面がある場合、同じ値なら飛ばし、異なる値なら `keep-existing` と同じく先の値を残します。既に worker が同じ条件で読んでいる場合も、先に台帳へ入った方を残し、後から完了した worker の結果は追記せずに数え、台帳と値が異なる場合は `ALERT` に記録します。取り込み元の先読みの印は、新しく追記する局面と、取り込み先に同じ条件・同じ値がある局面に引き継ぎます。同じ指紋・同じ局面が複数行ある場合、後の行にだけ印があっても引き継ぎます。別の条件の行には印を移しません。

完了したファイルは `import/done/<名前>.jsonl` へ移り、食い違いは `import/done/<名前>.jsonl.conflicts.tsv` に書きます (列とエスケープは `import-journal` と同じ、0 件ならヘッダだけ)。異常終了で `done/` に残った一覧の一時ファイル (`.tmp*`) は再開には使わず、serve 停止中に削除できます。失敗したファイルは `import/failed/` に移り、理由を `ALERT` と `targets.status.json` の `import.reason` に出して次のファイルへ進みます。

ログと `targets.status.json` の `import` に、最新ファイルの状態と `counts` (`appended`: 追記、`skipped_same`: 同じ値、`conflicts`: 取り込み先との食い違い、`skipped_invalid`: 不正な行、`skipped_conflicting`: 取り込み元内の値の衝突、`skipped_condition`: 互換用、常に 0) を出します。件数は入力の行ごとです。`discarded_worker` は起動中に後着結果を捨てた累計です。中断時は受付に残ったファイルを次の起動で検査し直し、追記済みの行は飛ばします。処理済みファイルを再投入しても同じ値の読みは増えません。再開時の件数は再処理した時点の値になります。先読みの読み・使用・未使用の統計には、取り込み元で先読みの印が付いた局面も含まれます。`speculated` の集計には、先読み worker が完了したものの台帳への追記を見送った後着結果も含まれます。

### 空いた worker での先読み

`tree serve --prefetch` は、本来の要求を配り終えた空き worker で、次の回の伸ばし直しを予測して読みます。既定では無効です。こちらの手番で依存する先がすべて読み済みで、循環を含まない局面を対象にし、本体とは別の影の木で不足と手数を求めます。本来の要求と同じキーは重複して配らず、本来の需要に変わった先読みは横取りの対象から外します。

| 引数 | 既定 | 意味 |
|---|---|---|
| `--prefetch` | 無効 | 先読みを有効にする |
| `--prefetch-workers N` | 現在の workers | 先読みだけを求められている読みかけの上限。現在の workers を超えない |
| `--prefetch-preempt-below 割合` | `0.25` | 本来の要求に worker を譲る進みの境界。0〜1、境界以上は止めない |
| `--prefetch-work 単位` | `500000` | 影 1 本の計算量の上限。周回全体の単位の合計もこの値までなので、作り直すのは通常 1 周回に 1 本 (最大 4 本) |

進みは `max(go からの経過秒 / 見込み秒, 観測 nodes / nodes 上限)` です。見込みには読み終えた直近 50 件の 1 ノードあたりの秒数 (`sec / nodes_limit`) の中央値に、そのジョブの nodes 上限を掛けた秒数を使い、10 件未満なら横取りしません。起動時は台帳の追記順で復元し、他の台帳から import した読みは除きます。進みが最小の先読みから止め、同じなら priority の低い木を選びます。`--prefetch-workers` を小さくすることで無駄を抑えられますが、線以上に進んだ読みが worker を占めて本来の要求を待たせる場合があります。

横取り、需要の消滅、`stop`、workers の減少では USI `stop` を送り、応答を捨ててエンジンを再利用します。停止要求後、送信前に処理する受信行は最大 256 行とし、その間に受け取った bestmove は完了として扱います。応答の期限は送信から絶対の 10 秒で、info が続いても延長しません。期限切れ・EOF・送信失敗は異常としてエンジンを捨てます。先読みの異常は本来の失敗と分け、局面ごとに 2 回で先読みから外します。本来の要求としては改めて読めます。制御ファイルの RESTART は従来どおりエンジンを捨てる指示です。

`stop` では先読みだけの読みかけを止め、本来の需要がある読みは完了を待ちます。workers の減少では退役する worker と先読み上限を超える分を止めます。先読みとして開始した読みは、hold・世代変更・終了などで需要を失うと止めますが、別の木の有効な需要が残れば続けます。本来の要求として開始した読みは、需要を失っても完了を待ちます。周回の残予算が影 1 本の上限に満たなければ、その木は次の周回の先頭で再構築します。影の再構築が上限で後回しになった間は古い候補の需要を保ち、新しくは配りません。

先読みは木の `running` と STATUS には含めません。プロセスは workers=0 で読みかけがなければ STOPPED、本来の要求・読みかけまたは配れる先読みがあれば RUNNING、それ以外は IDLE です。先読み有効時の `progress.json` の `queued` は `{ "normal": 件数, "prefetch": 件数 }` になります。`prefetch` に `queued`・`running`・理由ごとの `stopped`・停止時の進み (時間の見込みと nodes の比の大きい方、異常時は観測 nodes の比) の合計 `discarded`・先読み済み `read`・利用済み `used`・未利用 `unused` を出します。`stopped`・`discarded` は先読みとして開始した読みだけを数え、本来の読みへの RESTART は含めません。`stopped` のキーは `preempt` / `no_demand` / `stop` / `workers` / `restart` です。更新は先読みの有無によらず 1 秒ごと、またはプロセスの状態が変わったときです。先読み無効時の形式は変えません。

台帳の Record に `prefetch: true` を保存します。読みそのものと読み条件は変えず、古い台帳も読めます。`tree import-journal` も印を引き継ぎます。先読み済みの集合は台帳から復元し、利用したキーの集合・停止理由ごとの件数・捨てた量は state の `prefetch_stats.json` に原子的に保存して再開時に読み直します。利用は木の level に入った時点で数え、木を縮めた後や世代変更後も残します。未利用数は後から利用されると減ります。外れの読みは台帳に残るため、台帳の容量と読込み費用は増えます。

木の結果が同じという保証は、同じ定義・同じ局面に同じ読みを使った replay と再開の比較に限ります。実エンジンでは、異なる手数で読んだ結果を先に台帳へ確定させることがあります。予測後に段が浅く見つかる、先が伸ばし直される、別の枝と合流して伝播値が変わることが外れの要因です。

#### 読みかけの局面の子の先読み

`--speculate-at 割合` (`--prefetch` と一緒に指定、0 より大きく 1 未満、例 `0.25`) を付けると、読みの観測 nodes が `nodes 上限 × 割合` を越えた時点で、その時点の MultiPV の上位の手 (multipv 1 から指定数まで揃った最後の組。bound の行は除く) を取り、その局面を待っている木の子を先読みの候補にします。木は段ごとに親の読みを待つので、段数の多い木では直列の待ちが縮みます。

- 候補は、木で読み終えていない段 `depth` 未満の局面の、途中の上位の手と実戦の手の子です。手番によらず MultiPV の全部の手の子を挙げます。根以外で途中の 1 位の値の絶対値が木の `stop` 以上なら挙げません (木もその先を伸ばさないため)。合法手が MultiPV の数より少ない局面などで組が揃わなければ出しません。
- 挙げた子も辿ります。途中の上位の手があればその全部の手で、台帳の読みがあれば木と同じ規則 (自分の手番は 1 位、相手の手番は上位 `opp_n` 手、どちらも実戦の手を足す。`stop` 以上は辿らない) で、`depth` まで辿ります。合流した局面は浅い段のものを使います。木にすでにある局面・読み済みの局面は挙げません。1 本の木で 1 回に辿る局面は 5,000 までで、浅い段から打ち切ります。
- 途中の上位の手は 1 つの局面で最初の 1 回だけ使います (RESTART などの読み直しでは出し直しません)。読み終えると台帳の読みに替わります。止められて、どの木の局面・候補でもなくなった局面の分は捨てます。途中の手と値は台帳に残しません (`--trace-dir` には残ります)。
- 途中の上位手のヒントは、木と同じ MultiPV の読みからだけ使います。
- 候補は伸ばし直しの予測と同じ列に並び、`--prefetch-workers` の上限と横取りの規則も同じです。親を読み終えて外れと分かった子は、読み始めていなければ列から外し、読み始めていれば読み終えて台帳に残します。伸ばし直しの予測で始めた読みが後から候補になった場合も同じです。木の終了・hold・世代替えで需要を失ったときは、既存の先読みと同じく止めます。
- `--speculate-priority 優先度` (`--speculate-at` と一緒に指定) を付けると、キューの `priority` がこの値以下の木の候補は、それより `priority` の大きい木の本来の読みより先に配ります (同じ `priority` では本来の読みが先)。worker を空けずに、判断を急ぐ木の段の待ちだけを縮めるためのものです。その読みは、それより `priority` の大きい本来の読みには横取りされません。`priority` が同じか小さい本来の読みには、普通の先読みを先に止めたうえで、足りない分だけ普通の先読みと同じ規則で譲ります。同じ局面を優先しない木も求めていても、順と横取りに使う `priority` は優先する木のものです。親を読み終えて外れと分かった子は優先を外し、普通の先読みとして横取りの規則に従います。`--prefetch-workers` の上限を超えた分は、優先しない先読みから止めます (workers を減らしたときは、退く番号の worker の読みを種類によらず止めます)。外れの読みの分、`priority` の大きい木は遅れます。`progress.json` の `speculate.urgent` は、この優先で読んでいる worker の数です。
- `progress.json` の `speculate` に、途中の上位の手を出した局面の数 `sources`・本来の需要になっていない読みかけ `running`・読み終えた子 `read`・そのうち木の局面になった `used`・未利用 `unused` を出します。`sources` と `read` は `prefetch_stats.json` の `speculation_sources`・`speculated` に保存し、再開時に読み直します。台帳の Record は `prefetch: true` で、先読みの集計 (`prefetch` の `read` / `used` / `unused`) にも含みます。
- 読み条件の指紋は変わりません。

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

### 台帳からの再生

```bash
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
# fingerprint = "..."  # 木が優先する読み条件。replay の --fingerprint を指定するとそちらを優先
# draw = 0            # 手順上の循環に付ける手番側の値。USI DrawValue とは別
# draw_black = -70    # 先手番の局面で循環に入る手の値 (先手から見た値)。省くと draw
# draw_white = 70     # 後手番の局面で循環に入る手の値 (後手から見た値)。省くと draw
```

paths.tsv はヘッダ無しで `key<TAB>ply<TAB>USIの手順<TAB>メモ`。key は手数を除く SFEN の3フィールドで、同じ根をまとめ、根の手番をそろえます。GAME は各手順の最初の `depth+1` 手を局面別に合わせます。`own` は `b` / `w`。こちらの根の EXTRA は MultiPV 内だけ、相手の根の EXTRA は合法なら採用します。

条件の指紋は、engine とファイル型 USI option の内容 SHA-256、全 option、明示 `SPSAParamsFile` が無い場合のバイナリ隣接 `spsa.params` の有無と内容、nodes、MultiPV、局面ごとの初期化・info 解析の版から作ります。

同じ指紋・局面で値が同じ読みは、再取り込みでも追記しません。複数条件を含む同じ台帳を再取り込みしても `appended=0` でサイズは変わりません。ただし、未記録の先読みの印を補う行だけは追記します。

replay は engine を起動せず、serve と同じ木の処理を台帳の読みだけで進めます。不足や ready:false は非0で終了します。出力先と journal・paths.tsv の衝突を検査し、出力先と journal の lock を取る前には既存の出力を変更しません。`--fingerprint` は優先する条件を指定します。その指紋の読みが無い局面では、`nodes_limit` が最大、同点なら PV 行数が最多、さらに同点なら指紋の文字列が最小の読みを選びます。CLI の指定を省略した場合は TOML の `fingerprint` を優先し、それもなければ局面ごとに同じ規則で選びます。複数条件を含む台帳でも指定は不要です。条件の指定が無いときに台帳の任意の条件を木全体へ割り当てることはありません。

台帳はすべての条件の読みを保持し、各行をその行自身の設定で検査します。読みが行自身の設定による検査を通り、設定から再計算した指紋が保存された指紋と一致すれば有効です。指紋の再計算は、ファイル型 option のパスを内容のハッシュに置き換え、版情報を除いた設定ごとに共有します。`engine_version` は参考情報で、版の違いでは行を飛ばしません。条件の代表情報には最初に分かった版を残します。指紋の不一致や読みの不正は `invalid` に数えて飛ばします。同じ指紋・同じ局面で同じ値の重複はまとめ、先の読みを保持します。先の行と値が異なる行は `conflicting` に数えて飛ばし、先の値を残します。飛ばした行は条件や先読みの印を変更しません。元の行はディスク上に残し、指紋別の `invalid`・`conflicting` 件数を標準エラーと serve 起動時の `journal_skipped` に出します。serve 自身の追記では、値の異なる重複を引き続きエラーにします。JSON の形式不正と版の不一致は読み込み時もエラーです。別の指紋の読みは同じ局面でも共存します。条件の設定は台帳内で共有し、保持量は条件と局面の組の数に比例します。改行の無い末尾は回復します。tree 用台帳は版付きで、既存 `expand / run` の台帳とは共用できません。指紋の計算にはモデルのパスではなく内容を使います。

### 開発者向けの検証 (テスト): 台帳による並列読みの模擬

開発者向けに、エンジンを起動せず木の依存による worker の空きを調べるテストがあります。
バイナリには含まれず、serve の動作や入力ファイルを変更しません。

入力 TSV はヘッダーなしで、1 行に `state dir<TAB>journal path<TAB>tree id` を指定します。
`targets.jsonl` の受付済み範囲を `accepted.json` と照合し、`targets.status.json` の
今の世代で完了した木を読み込みます。手番が一意でない id は使えません。
台帳は state の指紋で選択し、現在の木の規則で最後まで再生できない木は理由とともに除外します。
台帳は lock を取らず、読込み開始時のサイズまでの確定行だけを使います。
追記中の未確定な末尾は無視し、元ファイルの切詰めや修復はしません。

```bash
BOOK_TREE_SIM_INPUT=/path/to/input.tsv cargo test -p tools --lib \
  book_tree::simulation::journal_simulation -- --ignored --nocapture
```

環境変数がなければ何もせず成功します。固定 seed 42 で 5・10・20・40 本を各 5 組と全件を
128 worker に積みます。5・10 本は先読みなし、20・40 本と全件は先読みなしと
先読み上限 128・64・32 × 代用時間の平均・最大を標準出力に出します。
抽出は 20・40・5・10 本の順で行います。
本数が足りない部分集合は省きます。受付順は入力の state dir の初出順、同じ state 内では
元の受付順 (`target.order`) の順です。priority は各 serve の値をそのまま使います。
同じキーは 1 回だけ読み、各木には自身の台帳の答えを渡します。

本来の要求は preflight で使ったキーを対象に、そのキーを求めた木の秒数の最大を使います。
先読みは本来の要求を配った後の空きだけを使い、同じキーの読みが実行中なら待ちます。
先読みでも本来の要求になるキーには同じ秒数を使います。それ以外は、その組の全標本の台帳に
ある答えの最大です（そのキーを本来要求しない木も含みます）。どの台帳にも無いキーは、
本来の要求のキー別秒数の平均 (`mean`) または最大 (`max`) で代用し、その先を展開しません。
台帳に無い読みを打ち切ることは有用な合流も失い得るため、偏りの向きは決まりません。

予測は、依存の推移閉包が読み済みで循環を含まない局面から行います。
`level` に無い MultiPV の手は読みの値のまま比較します。浅い再発見、伸ばし直し、
回の途中で別の枝から `level` に入る合流によって、予測が外れることがあります。
影は元の定義から作り、本体の force と予測を合わせ、rounds を 0 にして台帳で進めます。
本体が読みを取り込んだときと影の不足に読みが来たときに作り直します。
構築・依存解析・展開の処理単位を数え、1 回 500,000 単位で打ち切った影は捨てます。
1 周回の作り直しは、単位の合計が `--prefetch-work` に収まる分 (通常 1 本、最大 4 本) とし、残りは更新が必要な印を保持して次の周回の最初から処理します。
後回しの周回では模擬の時計を進めないので、この本数の上限は T′ に影響しません。

表の時間は秒です。`T_sec` は先読みなしの完了時刻、`Tprime_sec` は先読みありの
全木の完了時刻、`delta_sec` と `delta_pct` は T′ − T とその割合 (%) です。
`workers_done` は外れた先読みを含む全 worker の終了時刻です。
先読みなしの行には `L_sec`（本来の読み時間の総和 / worker 数）、`1-L/T`、
`utilization_20`（全時間を 20 等分した区間ごとの worker 使用率、0〜1、コンマ区切り）も出します。
先読みなしはキー別最大時間と全キー平均時間の両方を出し、`baseline_timing` で区別します。
平均時間の場合の L は ceil(キー数 / worker 数) × 平均時間です。
先読みありの比較対象はキー別最大時間です。`N` / `U` / `mixed` は木の数 / 本来の要求の
共有キー数 / 複数の読み条件を含むキー数です。
`read` / `used` / `wasted` は先読みしたキー数 / 最終的に木が使った数 / 外れた数、
`absent` / `absent_pct` はどの台帳にも無い先読みの数 / 先読みに対する割合です。
`waste_worker_sec` は外れに費やした worker 秒の合計です。
`same_wait` は同じキーの先読みを待った秒数の合計、`other_wait` は別の先読みを外すと
空く枠に入れた本来の要求の待ち秒数の合計です。共有キューのキーを 1 要求として数えます。
`same_p95` / `other_p95` はそれぞれ 1 要求あたりの 95% 点で、待ち 0 の要求も含みます。
`same_positive` / `other_positive` はそれぞれ待ちが正の要求キー数、
`same_max` / `other_max` はその最大秒数です。
`max_concurrent` はまだ本来の要求になっていない先読みの同時実行数の最大です。
`added_round_X` は本体の `round_reports` の added の総和です。既存の added は手数ではなく、
木・回ごとの伸ばし直した局面 X を数えます。`added_moves` は最終的な force の手数の総和で、
木ごとに同じ X・手は 1 回だけ数えます（初期 force は空）。
`read` は共有されたキー数なので、この追加手数と同じ単位ではありません。
`unread_X_round` / `cyclic_X_round` は依存に未読 / 循環（祖先を含む）があり予測を除外した
局面 X の数です。対象は手を比較する自分の手番・深さ内・展開手ありの局面で、
同じ木・回・X は作り直しをまたいで 1 回だけ数えます。両理由のある X は両方に数えます。
`work_limits` は処理上限で影を丸ごと捨てた試行数です。正常な候補 0 件とは区別し、
未完の依存解析の内訳は集計しません。
`no_slot_keys` は候補として得られた後、先読みを配る前に本来の要求になったキー数です。
本来の要求と先読み上限により空きが無い場合を含み、共有キーを 1 回だけ数えます。
候補が取り消された後も追跡するため、予測時点で配れなかったキーの参考値です。
これらは追加手数の排他的な内訳ではなく、予測を妨げた理由別の観測数です。
`rebuilds` / `rebuild_max_sec` / `rebuild_total_sec` は影を作り直した回数と実測時間の最大 / 合計です。
模擬の時計には影の計算時間を加算しません。

この模擬は全木を時刻 0 に積み、失敗・再試行・起動時間を含みません。
台帳に無い先読みが 5% を超える場合は代用時間への依存も合わせて判断してください。
標本で効果が小さくても、波で積む運用や積み続ける運用の効果を否定する結果にはなりません。

### 別の台帳の読みの取り込み

別のマシンの台帳 (複製したもの) の読みを、それぞれ元の条件を保持して足します。条件の違いでは取り込みを拒否しません。

```bash
book_mine tree import-journal --from-journal /path/to/other-machine-journal.jsonl \
  --journal /path/to/tree-journal.jsonl \
  --engine /path/to/engine \
  --engine-option "EvalFile=$SHOGI_DATA/nnue/model.bin" \
  --engine-option DrawValueBlack=-70 --engine-option DrawValueWhite=70 \
  --nodes 1000000000 --multipv 3 --hash 1024
```

`--from-journal` は複数回指定できます。指定順に 1 本ずつ処理し、取り込み先の読み込みと排他 lock の取得は実行全体で 1 回です。同値の重複、食い違い、先読みの印は、同じ順序で 1 本ずつ実行した場合と同じように扱います。`reject` では後のソースに食い違いがあっても実行全体を拒否し、何も追記しません。`keep-existing` の一覧は全ソース共通の 1 ファイルになり、`incoming_provenance` でソースを識別できます。同じ実体の取り込み元・取り込み先、正規化後に重複するソース、台帳と同じ一覧出力先は拒否します。

繰り返し取り込むときは `--offsets /path/to/import-offsets.json` を指定できます。ファイルが無ければ全ソースを先頭から処理して作成します。ソースごとに最後の完全行の末尾バイト位置と署名を保存し、次回はその位置から後の完全行だけを解析します。差分の読み込み中に同じ条件・局面・値の行を集約し、保持する読みは異なる値の数に比例します。同値行の先読みの印は集約先に引き継ぎます。新しい完全行が無ければ解析しません。改行のない末尾は次回に回し、保存位置を進めません。空ファイルや完全行のない全体読み込みはエラーですが、検証済みの保存位置からの新しい範囲が空なのは正常です。

署名は保存位置までの先頭最大 4096 バイト、区切りの `|`、保存位置直前の最大 4096 バイトを連結した SHA-256 です。保存位置が現在の完全行の末尾より後、または署名が不一致なら、ソースを先頭から取り込み直して `rewritten=1` を表示します。同値の行は飛ばします。この署名は両端の検査であり、検査範囲外の変更は検出できないため、ソースは追記専用として扱ってください。

再開状態は取り込み先の正規化パスと実行終了時の長さ・同じ方式の署名、除外リストのファイル全体の SHA-256 と `--fingerprint` のフィルタ値 (各々指定なしなら null) も保持します。取り込み先のパス、保存済みの長さの先頭・末尾それぞれ最大 4096 バイトの署名、除外リストの SHA-256、指紋フィルタが一致しなければ失敗します。取り込み先が保存済みの長さより短い場合も失敗します。取り込み先の作り直しや除外リスト・指紋フィルタの変更時には、**新しい取り込み先を用意し、offsets ファイルを削除して**先頭から実行してください。`fingerprint` は指定なしの null も含めて必須です。ソースごとの `destination_conflicts` も必須で、競合がなければ空のリストにします。未知の項目を含む状態ファイルも拒否します。状態の読み込み・検証は取り込み先の排他 lock の下で行います。状態用の一時ファイルは追記前に同じ dir に作成し、台帳の追記に成功した後で同期・原子的な置換・dir の同期を行います。置換前の失敗時は古い状態を保ちます。その実行で指定しなかったソースの状態も保持します。offsets の親 dir は先に作っておきます。台帳・除外リスト・一覧と offsets を同じ出力先にはできません。

`--exclude-readings /path/to/excluded.jsonl` は、取り込まない読みを次の JSONL 形式で指定します。

```jsonl
{"key":"4k4/9/9/9/9/9/9/9/K8 b -","ply":1,"nodes":1000000000,"sec":12.5}
```

`key`・`ply`・実測の `nodes`・`sec` がすべて一致する行を、重複の集約・取り込み先との照合より前に除き、`excluded` に数えます。`sec` は JSON から読み取った f64 の値で完全一致させるため、台帳の値をコピーしてください。`-0.0` と `0.0` は同じ値として扱います。空行は無視し、不正な行はエラーにします。除外照合には識別子の 16 バイトのダイジェストを整列した配列を使い、メモリは約 16 バイト × 件数です。対象は取り込み先が既にメモリに保持する台帳索引の読みの部分集合なので、既存の索引と同じ規模に収まります。

各ソースの結果を `source=<path> start=<byte> end=<byte> appended=<件数> skipped=<件数> conflicts=<件数> excluded=<件数>` と表示し、保存位置を破棄したソースには `rewritten=1` を加えます。その後に `fingerprint=` と全ソースの `source_fingerprint=`、最後に合計の `appended=<total> skipped=<total> conflicts=<total>` を出します。範囲取り込みの出どころには、その範囲で読んだ完全行の SHA-256 を使い、開始位置が 0 より大きければ `:offset=<start>` を付けます。開始位置 0 の出どころの書式は従来と同じです。

`--fingerprint <FP>` を指定すればその条件だけ、省略すればすべての条件を取り込みます。取り込み元は共有 lock で読み、ファイルを変更しません。serve などが書き込み中の台帳は拒否するので、複製してから渡します。改行で終わらない末尾の行は無視します。各行の条件から指紋を計算し直して元の指紋と一致することと、その条件で読みが妥当なことを検査します。

取り込み先の台帳は排他 lock を取って追記するので、その台帳を使っている serve を止めてから実行します (使用中なら lock の取得に失敗して何も書きません)。動かしたまま取り込む場合は、[serve の受付 dir](#動かしたまま別の台帳を取り込む) に台帳の複製を置きます。

同じ指紋・同じ局面の読みが取り込み先と取り込み元の両方にあるとき、値 (MultiPV の手・評価値・深さと bestmove) が同じなら取り込み元の行を飛ばします。取り込み先と値が違う重複の扱いは `--on-conflict` で選びます。取り込み元の不正な行と、取り込み元内の同じ条件・同じ局面で値が食い違う後続行は、serve の読み込みと同じ規則で飛ばします。指紋別の `invalid`・`conflicting` 件数を標準エラーに出します。borrow value/cut と tree register の台帳照合も同じ規則を使います。`engine_version` の違いでは拒否しません。範囲読み込み時の代表版はその範囲で最初に分かった版で、全体読み込みの代表版とは異なる場合があります。版は参考情報であり、指紋や読みの採否には影響しません。

範囲をまたぐソース内の食い違いも `conflicting` として飛ばします。新しい範囲の行が宛先と食い違う、または範囲内に同じ条件・局面で複数の値がある場合だけ、対象の条件・局面を集め、同じソースの先頭から保存位置までを一度だけ行単位で読み直します。検査・フィルタ・除外を通った最初の同条件・同局面の値と異なる行は、ソース内の食い違いとして飛ばします。`reject` でも成功して保存位置が進み、宛先の `conflicts` 件数や TSV 一覧には含めません。初出の値と同じ行、または先行範囲に該当行がない行は通常の宛先競合として扱います。対象がなければ先行範囲の追加読み込みはありません。照合用のメモリは対象の条件・局面の数と一行の大きさに依存し、先行範囲の行数には比例しません。offsets のソースごとの項目には、保存位置と署名に加え、`keep-existing` でソースの最初の値を取り込み先との競合により残せなかった条件・局面の識別子を `destination_conflicts` に保存します。識別子は 16 バイトのダイジェストの hex 表記で、値の履歴は保存しません。差分行が取り込み先と同値でも、この識別子に該当すれば先行範囲を照合し、ソース内で食い違う行とその先読みの印を除きます。保持件数は競合した識別子の数に比例します。ソースを先頭から取り込み直すときはリストを空にし、その実行の競合から作り直します。

| `--on-conflict` | 値の違う重複があるとき |
|---|---|
| `reject` (既定) | 1 件でもあれば取り込み全体を拒否し、台帳を変えない |
| `keep-existing` | 取り込み先の値を残し、残さなかった読みを `--conflicts-out` の TSV に書く。`--conflicts-out` は必須で、`reject` では指定できない |

複数のマシンが同じ条件で読んだ台帳を合わせるときなど、食い違いを一覧で確かめて扱いを後から決めたいときに `keep-existing` を使います。一覧の列は `key`・`ply`・`existing_bestmove`・`existing_cp`・`incoming_bestmove`・`incoming_cp`・`incoming_provenance` で、評価値は MultiPV の 1 本目です (bestmove が `win` の読みは空)。一覧の出力先の dir は先に作っておきます (台帳の dir と違い、作りません)。同じ名前のファイルがあれば置き換えます。タブ・LF・CR・`\` は `\t`・`\n`・`\r`・`\\` と書きます。食い違いが 0 件でもヘッダだけの一覧を書きます。一覧は読みを追記する前に書くので、一覧を書けなければ読みを追記せずに失敗します (台帳の末尾の書きかけの行は、台帳を開くときに切り詰めます)。一覧の出力先が取り込み元・取り込み先の台帳と同じなら拒否します。件数は、取り込み元の同じ指紋・同じ局面の行を 1 つにまとめた後の数です。標準出力の最後の行は `appended=<追記> skipped=<飛ばした (食い違いを含む)> conflicts=<食い違い>` です。食い違いのあった局面を使う木の出力は、残した値で伝播したものとは限らないので、登録の前に `tree replay` で作り直すかを確かめてください。

`--accept-fingerprint <FP>` と `--force` は互換性のため受け付けますが、動作を変えない非推奨の指定です。エンジンの版や参照ファイルの内容を取り込み先と照合する必要はありません。

標準出力の `source_fingerprint=` は、取り込む条件の指紋をカンマ区切りで列挙します。

`--engine`・`--engine-option`・`--nodes`・`--multipv`・`--hash` は CLI の互換性のため引き続き受け付けます。これらから計算した指紋を `fingerprint=<FP>` と表示しますが、取り込む行の条件は書き換えません。各行の出どころには元の行の SHA-256・指紋・パスなどを残します。全件を検査してから追記し、標準出力に追記数・飛ばした数を出します。

### 出力と登録可否

木ごとの出力先に `registration_status.json` と使用した読みの `d<段>.jsonl` を書きます。ready の場合は `own_nodes.tsv`、両方の手番の局面の手ごとの伝播後の値 `node_moves.tsv`、`tree.db`、`tree_rule.db`、参考値の `tree_bp_min.db`、根の表と最後の集計を含む `report.txt` を出します。min はライブラリの `backprop_file_with` を呼びます。参考値の生成・読み込みに失敗した場合は報告し、min 欄を空値として他の成果物を完成させます。report.txt は根の候補、伸ばし直し、警告と最後の集計を含みます。

未展開手が上回る局面が残ると `ready:false` と `unresolved` を記録し、`NOT_READY` にします。これは計算自体の失敗ではありません。rounds を増やす場合は、同じ `id` で `opts.rounds` を増やした行を足すと新しい世代で読み直します。読みは同じ台帳から再利用できます。途中・失敗・ready:false の出力先には `own_nodes.tsv`・`node_moves.tsv` を残しません。ready のときも全出力が完成してから登録材料を公開し、その後に DONE を書きます。

`registration_status.json` は `ready`・`unresolved` に加え、木の全ての読みが同じ条件と確認できる場合だけ `fingerprint` (台帳の settings.fingerprint と同じ 64 桁) を持ちます。serve と replay の両方が出力し、異なる条件や由来の不明な読みが混ざる場合は鍵を省きます。

`node_moves.tsv` は木が届く局面すべて (こちらの手番も相手の手番も) の手を、伝播後の値の順に 1 行ずつ並べます。列は `key`・`ply`・`level` (根からの段)・`remaining` (その局面から先に伸ばせた段数 = depth − level)・`nodes` (読みのノード数の上限)・`move`・`value` (手番の側から見た伝播後の値。伸ばしていない手も木の別の局面に合流すればその子の値を使い、合流しなければ 1 回の読みの値)・`expanded` (木の辺として伸ばした手なら 1。合流だけで子の値を使う手は 0 のまま)・`rank` です。勝ち宣言など、指し手を持たない読みの局面は載せません。

### 定跡の元データへの登録

```bash
book_mine tree register --master moves.tsv --since 20261022-tag --note "計画の名前" \
  --conflicts conflicts.tsv [--schema schema] [--journal journal-copy.jsonl] [--alt-gap 40] [--bench-expanded] [--bench-single] [--replace-curated] [--skip-keys skip.txt] \
  [--switch-rule line [--keep-line 20] [--keep-min-depth 1] | --switch-rule propagated [--single-enqueue-out single.jsonl [--exclude-queue queue.jsonl]... [--walk-table walk.tsv [--single-min-weight 0.001] [--single-priority-band 0.05=1]...]]] \
  [--bridge-replies] [--opp-bench] [--draw-value-black -70] [--draw-value-white 70] [--auto-skip-flip found.txt [--auto-skip-flip-jobs 4] [--auto-skip-flip-rounds 10]] [--apply] <木の出力先>...
```

ready の木 (`STATUS` が DONE) の `node_moves.tsv` を読み、定跡の元データの表 (TSV) に手を登録します。`--apply` を省くと元データは書き換えず、集計と食い違いの一覧だけを出します。`--conflicts` はまだ無いファイルにしてください (既にあれば何もせずに止まります。確認だけの実行でも書くので、元データや木の出力を上書きしないためです)。確認だけの実行と `--apply` の実行では、一覧を別のファイル名にします。`node_moves.tsv` の無い古い出力先は `tree replay` で作り直してから渡します。

`--skip-keys <FILE>` で、一部の局面だけを登録から除外できます。ファイルは 1 行に 1 局面の SFEN キー (盤・手番・持ち駒の 3 フィールド) を書きます。手数つきの 4 フィールドも受け付けます。SFEN を局面として読み直して正規化するため、手数 (`+1` などの表記を含む) や持ち駒の並び順の違いは照合に影響しません。空行と、前後の空白を除いて `#` で始まる行は無視し、不正な SFEN はファイル名・行番号つきでエラーにします。指定した局面と、その先後反転キーで合流する局面を除外します。指定局面には main・alt を足さず、既存の全行の main・評価値・控えなども変更しません。指定局面の子の局面 (木の先) の行は今までどおり足されます。

複数の木にある同じ局面は 1 局面として数え、「指定で取り込まなかった局面 N」を出します。「木どうしで 1 位が違った局面」は除外した局面を含みません。ファイル内の重複は正規化後にまとめ、指定した向きも反転した向きも木に無かった指定は「指定したが木に無かった局面 M」に数えます。木の行ではない既存の元データの局面は、反転キーも木に無い場合、`--skip-keys` に書いても問題は解消せず、この M に含まれます。その場合、原因は木が足した先の局面の手にあるため、別に調べる必要があります。

対局で定跡を直接のキーだけで引く (先後反転した局面の項目を引かない) なら、反転キーで合流する局面を登録から除く必要はありません。`--skip-keys`・`--auto-skip-flip` を付けずに登録し、続く `master cycles`・`master rejoin` に `--direct-lookup` を付けます (`master cycles` の「反転キーで合流した局面で止めない」)。`--bridge-replies` を付けた登録と `master audit` の伝播は反転した局面へ辺を張らないので、反転キーでは止まりません。`--bridge-replies` を付けない `--switch-rule` の伝播と `master audit` には `--direct-lookup` に当たる指定が無く、今までどおり止まります。

`--auto-skip-flip <OUT>` を付けると、登録した元データを `master cycles` と同じ伝播 (千日手の値は `--draw-value-black`・`--draw-value-white`、相手の手番の局面は補わない) で解き、反転キーで合流した局面で止まるか (その向きの値を 1 行で保てない、または保証検査で反転キーで合流した向きに破れる局面がある) を確かめます。止まるなら、木を取り込まない元データだけでは反転キーで止まらないことを確かめ、取り込まない局面を探します。

探す途中の組は「軽い検査」で調べます。軽い検査は、main を替えるかを決める登録の中の伝播 (`--switch-rule`) を省いて登録した元データを同じ伝播で解きます。省いた判定は main と alt を入れ替えるだけで、局面の手とその値は変えないので、結果が元の検査と違うのは、同値の手の選び方が手の並びに左右される場合だけです。全部の木の検査、止まる木の確認、見つけた局面を除けば通ることの確認は、元の検査 (`--switch-rule` の伝播を含む登録) でします。

最初に一度だけ、木ごとに独立に調べます。

- 元データと木 1 本だけ (既存の `--skip-keys` を適用) を軽い検査で調べ、止まる木を元の検査で確かめて集めます。
- 止まる木ごとに、その木の未除外の全局面を候補にして、取り込まなければその木だけで通る小さい組を、候補を半分ずつ軽い検査で確かめて探します (局面を除いて通った後に、さらに除いても止まり直さない場合、試行は見つかる局面の数 × 候補数の対数ほど)。全部除いても反転キーで止まる木や、見つけた組を除いても元の検査で通らない木の結果は捨てます。
- 見つけた局面を木の入力順・木の中は発見順でまとめ、同じキーの重複を除いて取り込まない局面に足します。木ごとの探索は同じ初期の skip を使い、他の木の結果には依存しません。

その後は全部の木を検査し、止まるなら次を通るまで繰り返します。木どうしの組み合わせで初めて止まる場合もここで調べます。

1. 全部の木の未除外の全局面 (キーの昇順) を候補にして、取り込まなければ全部の木で通る小さい組を探します。候補を 6 個に分け、その部分だけを除いた組を並列に軽い検査で確かめ、通った最初の部分をさらに分けます。どの部分だけでも通らなければ、後ろの部分から順に、要る局面を拾います。
2. 見つけた組を除いた全部の木を元の検査で確かめ、通れば取り込まない局面に足して終わります。
3. 全部除いても軽い検査で止まるとき、または見つけた組で通らないときは、木の一覧からそれだけで止まる組 (原因の木。どの 1 本を除いても止まらなくなる組) を delta debugging (ddmin) で絞り、原因の木が取り込む局面から、取り込まなければ原因の木だけで通る組 (どの 1 つを戻しても止まる組) を同じく絞って足します。この探索は全部元の検査でします。その後、全部の木でもう一度確かめます。

局面を除いた途中の組や木を減らした途中の組が反転キー以外の理由で止まったときは、その組を「通らない」「反転キーでは止まらない」組とみて探し続けます (標準エラーに最初の 1 件と件数を出します)。木を 1 本も取り込まない元データだけで止まるときは、木からは直せないのでエラーで止まります。

元データと木は 1 回だけ読み、試行ごとの登録と伝播はメモリの上でします (ファイルは書きません)。そのため木ごとの局面を全部メモリに持ちます (付けないときは木を読むたびにまとめ、木どうしで重なる局面を捨てます。`--journal` で照合する指紋の無い木だけは、手ごとの `cond` を決めるまで木ごとの局面も持ちます)。登録が書き換えるのは木にある局面の行だけなので、試行ではその行だけを登録し直し、ほかの局面は最初に 1 回だけ伝播用の形にしたものを使います。元データと木の全局面について、局面の読み込みと手を指した後の局面も最初に 1 回だけ求め、試行の伝播で使い回します。足した局面の SFEN キーを `OUT` に 1 行ずつ書き (`--skip-keys` と同じ形。足す局面が無ければ空のファイル。既にあるファイルは上書きし、`--apply` の無い確認だけの実行でも書きます)、木ごとの検査で止まった木の本数と検査時間、木ごとの絞り込み・全部の木の絞り込みの試行数と時間、足した局面と、最後に足した局面の数 (木ごとの段とその後の巡の内訳)・試行の回数 (うち軽い検査の回数)・かかった時間を標準エラーに出します。続く登録の集計・食い違いの一覧・`--apply` の書き換えは、`--skip-keys` と足した局面を合わせて除いた結果です。次の版では `OUT` の行を `--skip-keys` のファイルに足してください。

木ごとの検査と局面の探索、全部の木の絞り込みは `--auto-skip-flip-jobs <N>` (既定 4、1 以上、`--auto-skip-flip` が必要) 本まで並列に回します。1 なら逐次です。1 本の木の探索は逐次のままです。足したキーの一覧と、原因の木・候補数・追加キーのログは並列の実行順にも N にもよらず決まります (木ごとの段は木の入力順、全部の木の絞り込みは候補のキーの順)。登録・伝播内部の警告や通知は試行中に出るため、並列時は混ざることがあります。反転キー以外のエラーも木の入力順で最初の 1 件を表示し、全試行の件数を集計します。試行内部のグラフ構築・応手補完も同じ rayon pool を使います。pool の並列度は通常の rayon の並列度と N の大きい方とし、同時に回す試行の数だけを N 以下に抑えます。試行ごとに別の pool を作らないため、内部の並列度が N 倍にはなりません (通常の global pool は別に残ります)。共有する元データ・木・使い回す局面の表に加え、登録後の全局面の写し・伝播グラフなどの試行ごとの作業メモリが必要です。同時に回す試行の分だけ増えるため、メモリは 1 試行分 × N を目安に見積もってください。

木ごとの段は `--auto-skip-flip-rounds` に数えません。その後の足し直しは `--auto-skip-flip-rounds` (既定 10) 回までで、0 なら木ごとの段も行わず、使い切っても止まるなら `OUT` に見つけた局面だけを書き、食い違いの一覧も元データも書かずにエラーで止まります。全部の木の登録が反転キー以外の理由 (反転キーで合流した向きに破れる局面の無い保証検査の失敗など) で止まったときも、そこでエラーで止まります。main を替えるかを決める登録の中の伝播 (`--switch-rule`) が反転キーで止まったときも、反転キーで止まった組として扱います。元の検査 1 回は登録 (`--switch-rule` の伝播を含む) と伝播 1 回ぶん、軽い検査 1 回は伝播 1 回ぶんの時間がかかります。所要時間は木ごとの試行数の偏り、N、CPU とメモリの空きに依存します。付けないときは検査せず、今までどおり登録します。

探した後は全部の木を全行で登録し直し、その元データが同じ伝播で通ることを、探す間に使い回した表に頼らずに確かめてから書き出します。ここで止まったときは `OUT` だけが残り、食い違いの一覧も元データも書きません。探す途中の軽い検査は余分な局面を含む組を選ぶことがあり、見つけた組が最小とは限りません。

元データの表は `key ply move role source evals nodes cond value_kind lineage tier since note` の 13 列 (見出し行つき) です。`key` は SFEN の盤・手番・持ち駒、`role` は `main` (局面ごとにちょうど 1 つ) / `alt` / `bench` (控え、指さない手) / `record` (指さない手)、`tier` は `curated` / `wide`、`evals` は手番の側から見た値 (カンマ区切りで複数可) です。

`cond` は `registration_status.json` の `fingerprint` から決めます。指紋の無い木には `--journal` を指定できます。候補の台帳は `--journal` を繰り返して全て渡してください。serve が動いている台帳も渡せます (lock を取らずに読み、末尾の書きかけの行は無視します)。読み始めた後の追記は照合対象に含めません。`d<段>.jsonl` の全ての読みを key・nodes_limit・実際の nodes・bestmove・MultiPV の手と cp と depth (順位を含む) で照合し、threads・hash・draw も候補の settings と一致する場合だけ候補に残します。読みごとに、一致した指紋がちょうど 1 つならその指紋に決まり、見つからない読みと複数の指紋に一致する読みは決まりません。全ての読みが同じ指紋に決まった木は、全部の手がその条件です。`--journal` を渡さない指紋の無い木と、読みの無い木は、全部の手が `unknown` です。

読みの条件が 1 つに決まらない木 (違う条件の読みが混ざる木、決まらない読みがある木) では、`cond` を手ごとに決めます。

- 1 回の読みの手 (`value_kind` が `single`) は、その局面の読みの条件です。値はその読みの値そのものです。
- 木の中で先の局面を読んだ手 (`tree:K`) は、行き先の局面から同じ木の中を辿れる読みを全部見ます (行き先の局面自身の読みを含み、手を指す局面の読みは含みません)。伝播は行き先の全部の手の値から 1 位を選ぶので、値の来た 1 本の手順だけでなく、行き先から辿れる読みを全部数えます。全部が同じ条件ならその名前、違う条件が 2 つ以上あれば `mixed` です。条件の決まらない読み、値の来た読みを辿れない手 (`value_kind` が `unknown` の手)、循環 (値が辿る手順で変わる) が混ざるときは、違う条件が 2 つ以上見つかれば `mixed`、そうでなければ `unknown` です。
- 木の中で先を確かめられない手 (`unknown`) は `unknown` です。

台帳の行の指紋をそのまま使うので、台帳の側で条件の印が付け替わっている読みは、付け替わった条件で書きます。

手ごとに決めた条件名と `mixed` は、既にある行の `cond` を上書きします。値とノード数が同じで既にある行に条件名があっても、手ごとの `cond` が別の条件名か `mixed` なら替わります (木全体を `unknown` にしていたときは既知の名前が残っていました)。手ごとの `cond` が `unknown` のときは、下の規則どおり既知の名前を残します。

1 スレッドでノード数で止める読みはほぼ決定的なので、同じソースを別の機械で build した条件の台帳を 1 本だけ渡すと、その台帳の条件に決まることがあります。読みの一致だけでは実行した機械やバイナリを特定できません。候補の台帳は全部渡してください。各台帳を 1 回だけ読み、全ての木を突き合わせます。

`--schema` があれば指紋に対応する conditions.tsv の name を使い、対応がなければ表への追加を促して失敗します。schema がなければ指紋の先頭 8 文字です。1 回の実行中に先頭 8 文字が同じ別の指紋を条件名に使おうとするとエラーになります。`--schema` の conditions.tsv で別の名前を付けてください。手ごとに決めるときは、木の読みが一致した指紋の全部に名前が要ります (足りなければ、木の出力先と指紋を付けたエラーで止まります)。stdout に `読みが 1 つの条件に決まらない木: N (登録に使う局面の手の cond: 条件名 A / mixed B / unknown C)` を出します。A・B・C は、その木のうち登録に使う局面 (`--skip-keys` と、同じ局面が重なって使わない木の局面を除く) の手の数です。`--journal` を渡さなければ A・B は 0 です。

`value_kind` は、その手の行き先が同じ木の `node_moves.tsv` にある場合、`expanded` によらず `tree:K` です。K は行き先の局面の残り段数 + 1 (`remaining + 1`)。行き先の key は negamax と同じ規則 (指した後の SFEN から手数を除く、先後反転はしない) で求めます。`expanded=1` でも行き先が TSV にない場合や、`d*.jsonl` にだけある局面 (勝ち宣言の葉や辺で届かない局面など) に合流する場合は、残り段数を確かめられないので `unknown` にします。読みの一覧 (`d*.jsonl`) がない木も、行き先が TSV になければ合流の有無を確かめられないため `unknown` です。それ以外は `single` です。古い木の出力にも同じ判定を使い、TSV の書式と `expanded` の意味は変えません。K は残り段数の目安で、循環や最善手順の長さではありません。新しい行の lineage は空です。更新を許す source は schema の replaceable=1、schema を省くと `read`・`ext` です。

規則 (局面ごと、先手・後手の手番の区別なし):

- 局面の値によらず登録します。各局面の main は伝播後の値の 1 位 (伸ばしていない手は 1 回の読みの値で比べる)、alt は main との差が `--alt-gap` 未満の手です。`record` の手は足さず、次の手を使います。`bench` (控え) 自身の source が schema で置換可能で、木の 1 位か、1 位との差が `--alt-gap` 未満なら alt に戻します。1 位なら既存の alt と同じ条件で main にでき、置換可能な source の評価値も更新します。main が無い局面でも、復帰した 1 位の控えを main にし、note に main への復帰と記します。この手は main の追加数には含めず、復帰数に数えます。curated の main がない局面の curated の控えは、`--replace-curated` の有無によらず復帰・昇格の候補から除き、「curated の main が無い控え」として別集計します。source が置換不可の控えは候補から除外して変更せず、「保護された控え」として別集計します。集計に「bench から戻した手」も出します。`borrow value` は控えを復帰・main 昇格させず、控えの行を変更しません。控えの復帰は `tree register` と `master audit --revive-bench` で行います。
- 元データに main の無い局面には、`source=read`・`tier=wide` の行で main と alt を足します。
- `--bench-expanded` を付けると、木の中で先の局面を読んだ手 (`value_kind` が `tree:K`、行き先が同じ木の `node_moves.tsv` にある手) は、木の 1 位との差が `--alt-gap` 以上でも `bench` (控え) の行で足します (`source=read`・`tier=wide`、`evals`・`nodes`・`cond`・`value_kind` は木の値、note は `木で先を読んだ手。伝播後で 1 位との差 N`)。省くと、先を読んだ手でも差が `--alt-gap` 以上なら行にしません。差だけで行を落とすと、その手の先の局面が元データにあっても手前の局面とつながらず、後から先を深く読んだ値が手前に伝わらないためです (控えの行を伝播に使うのは `master audit --opp-bench`)。足すのはその局面に行の無い手だけで、既にある行 (main・alt・bench・record) の役割は変えません。`curated` の局面にも `wide` の控えとして足します。`--skip-keys` の局面と、木の手から外した不成には足しません。1 回の読みの手 (`single`) と、先を確かめられない手 (`unknown`) は、この指定では足しません (`--bench-single`)。足した控えは指さない手で、次の登録で差が `--alt-gap` 未満になれば上の規則で alt に戻ります。版の手順で `master audit --revive-bench` と併用すると、差が `--alt-gap` 以上 `--demote-line` 未満で値が `--bad-eval` より上の手はその版の点検で alt になり、指し得る手になります (元データは先後で共用なので、その局面の手番の側の手として)。差が `--demote-line` 以上の手は bench のまま残り、相手の手番では `master audit --opp-bench` で伝播に使います。複数の木で同じ局面が重なると 1 本の木の手だけを使うので (下の `remaining` の規則)、採られなかった木だけが先を読んだ手は控えになりません。行き先の局面を `--skip-keys`・`--auto-skip-flip` で除いた控えは、先の局面が登録されないので値が登録時のまま残ります。集計に「先を読んだ手を bench で足した手」を手番別に出します。
- `--bench-single` を付けると、1 回の読みの手 (`value_kind` が `single`、伸ばしていない MultiPV の手) と、木の中で先を確かめられない手 (`unknown`。行き先が同じ木の `node_moves.tsv` に無いだけで、木が実際には先を読んだ手も含みます) も、木の 1 位との差が `--alt-gap` 以上なら `bench` の行で足します (値・`cond`・`value_kind` は木の値のまま、note は `木の 1 回の読みの手。伝播後で 1 位との差 N` か `木の中で先を確かめられない手。伝播後で 1 位との差 N`)。元データで「読んだが悪かった手」(行があり値を持つ) と「読んでいない手」(行が無い) を見分けるための指定です。`--bench-expanded` とは独立で、両方付けると木が読んだ手は全部が行になります。足す手の条件 (行の無い手だけ、`--skip-keys`・不成・`curated` の扱い)、次の登録や `master audit --revive-bench` で alt に戻る条件、複数の木が重なる局面の制限は `--bench-expanded` と同じです。差が `--alt-gap` 以上 `--demote-line` 未満で値が `--bad-eval` より上の手は `--revive-bench` を通すと alt になるので、版の手順で併用すると alt にする線は実質 `--demote-line` になり、切り替え用の書き出しで指し得る手と、循環の検査の対象が増えます。局面ごとに MultiPV の数だけ行が増えます。`--bench-expanded`・`--bench-single` の控えが `master audit --revive-bench` で alt に戻ると、反転キーで合流する局面が新しく出て `master cycles` が止まることがあります (`--auto-skip-flip` が検査するのは登録直後の元データで、点検で alt が増えた後の元データではありません)。集計に「1 回の読みの手・先を確かめられない手を bench で足した手」を手番別に出します。
- 登録の前に、元データの main・alt のうち成れる角・飛・歩の不成を `record` にし (下の「成れる角・飛・歩の不成」)、木の手からも外します。木の手から外した数と合わせて stderr に出します。
- 元データの main が木の 1 位と違うとき、その main が読みから作った行 (schema の replaceable=1、schema 省略時は `read`・`ext`) で、局面に `curated` の行が無ければ、木の 1 位を main にし、元の main を alt に残します。それ以外 (`replaceable=0` の source (schema 省略時は `read`・`ext` 以外) の main、`curated` の局面) は main を残し、木の 1 位を alt で足して `--conflicts` の一覧に出します。`--replace-curated` を付けると、`curated` の局面でも main の source が置き換えてよいものなら木の 1 位を main にします。元の main は alt にして `tier=wide` に移し、新しい main は元の main の `tier` を引き継ぎ、局面の `curated` の main を 1 つに保ちます (新しい main の行き先に `curated` の行が無ければ、丁寧な定跡の書き出しはそこで終わります)。元の main の note に `(curated から wide)` を足し、集計に curated の main を替えた局面の数を出します。`record` の行と、置き換えない source の main はこの指定でも替えません。
- main を替える前に、仮に登録した元データを `master audit` と同じ伝播 (`--bridge-replies`・`--draw-value-black`・`--draw-value-white` は `master audit` の同じ名前の指定と同じ意味) で解き、元の main と木の 1 位の伝播後の値を比べます。元の main が `--keep-line` (既定 20、0 で比べない) 以上上回り、元の main の値が `--keep-min-depth` (既定 1) 局面以上先の読みから来ていれば、main を替えずに木の 1 位を alt で足し、`--conflicts` の一覧に出します。木は根の局面から読むので、元の main の先にある元データの深い読みが入らず、木の値だけで比べると深い値を浅い値で替えることがあるためです。登録後の元データで比べるので、木が元の main の先を読み直した分は値に入り、続く `master audit` が見る値とも揃います (版の手順の `master audit` と同じ `--bridge-replies`・千日手の値を渡してください)。`--keep-line` を `master audit --line` より小さくするのは、`audit` が戻さない差の局面を拾うためです。`--keep-min-depth 1` は、元の main の値が、その手の 1 回の読みの値のまま (元データに先の局面が無いか、補った局面で定跡に無い応手が最善) のときは残しません。段数は、手の行き先から最善の手を辿り、読みの値で止まるまでの局面の数です (循環に入ったらそこまで)。元の main か木の 1 位の値が分からない (読みの値も先の局面の値も無い) 局面は比べずに替えます。比べる伝播は `master audit --opp-records` のように相手の record を使うことはしません。`--opp-bench` を付けると、`master audit --opp-bench` と同じく、相手の手番の局面で読みの値のある bench (この登録で足す控えを含む) を相手の手として使います (版の手順の `master audit` に `--opp-bench` を渡すなら、登録にも渡して値を揃えます)。元の main が木の読み (MultiPV) に入っていないかどうかでは比べ方を変えません (一覧の `main_tree_value` が `-` の局面がそれです)。集計の「食い違い」に、元の main の値が上なので残した局面 (`--switch-rule propagated` では残した全局面) の数を添えます。
- `--switch-rule propagated` では `--keep-line`・`--keep-min-depth` を使わず (指定すると止まります)、同じ伝播の値で `master audit --switch-rule propagated` と同じ規則で決めます。元の main と木の 1 位の値がどちらも手の先の局面の値から来ていれば、木の 1 位が 1 でも上回るときだけ替え、同じ値か元の main が上なら残します。どちらかの値が 1 回の読み (元データに手の先の局面が無いか、補った局面で定跡に無い応手が最善) なら替えず、どちらも `--conflicts` の一覧に `reason=keep` で出します (段数 0 の側が 1 回の読み)。`--single-enqueue-out` を渡すと、1 回の読みの手を伸ばす木の行を書きます (形と `--exclude-queue` の除外は `master audit --single-enqueue-out` と同じで、木の 1 位が元の main を上回る差の大きい順に並べます。`--walk-table`・`--single-min-weight`・`--single-priority-band` を渡すと、同じ辿りの重みで並べ、下限で落とし、priority を付けます)。値の分からない手の局面は今までどおり替えます。
- 一覧の列は `key ply side main main_source main_tier main_tree_value tree_best tree_value tree reason main_value main_depth best_value best_depth gap main_value_black best_value_black` です。`reason` は、元の main の値が上なので残した局面 (`--switch-rule propagated` では同じ値の局面と、片方の値が 1 回の読みなので残した局面も) が `keep`、source・curated で替えなかった局面が `protected` です。`main_value`・`best_value` は上の伝播後の値 (手番の側から)、`main_depth`・`best_depth` はその段数、`gap` は `main_value − best_value`、`*_black` は先手から見た値です。`protected` の局面ではこれらの列は `-` です。`main_tree_value`・`tree_value` は木の値 (手番の側から) で、`main_tree_value` は元の main が木の読みに無ければ `-` です。
- 更新を許す source の main・alt の行で木に値のある手は、`evals`・`nodes`・`cond`・`value_kind` を木の値に更新します。`evals`・`nodes` がともに同じなら、既知の `cond` や `value_kind` を `unknown` へ下げず、それぞれ元の情報を保持します。別の既知の条件や値の付き方への変更は更新します。値またはノード数が変わるときは、元の情報を引き継ぎません。
- 同じ局面が複数の木にあるときは、`remaining` の大きい (その局面から深く伸ばした) 木を使い、同じなら先に渡した木を使います。
- 結果は局面・役割 (main・alt・bench・record の順)・手の順に並べ、局面ごとに main がちょうど 1 つ (`curated` の行だけでも 1 つ) で、同じ手の行が重ならないことを確かめてから書きます。

### `tree inventory`: 登録していない読み終わった木の一覧

```bash
book_mine tree inventory --root <木の出力の根>... --out candidates.txt --table inventory.tsv \
  [--registered registered.txt] [--excluded-readings excluded.jsonl] [--exclude-prefix <id の接頭辞>]... [--keep-depth0]
book_mine tree register ... $(cat candidates.txt)
```

`--root` (繰り返し指定) の下を辿り、`registration_status.json` を持つディレクトリを木の出力先 (`tree register` に渡す単位) として数えます。出力先そのものを `--root` に渡しても構いません。出力先の中は辿らず、シンボリックリンクも辿りません。同じ実体の出力先が複数の根から見つかったら 1 回だけ数えます。同じ木を別の場所に写したもの (回収の写しと保管用の写しなど) は実体が別なので別の出力先として数えますが、下の `registered-copy`・`duplicate` の判定で候補から外します。出力先のファイルは読むだけで、lock も取りません (読んでいる途中に `tree replay` が書き換えた出力先は、`tree register` が lock を取って確かめ直します)。

出力先ごとに次の順で確かめ、最初に当たった判定を付けます (`registered-copy`・`compare-failed`・`duplicate` は、他の判定で候補に残った出力先だけを最後に確かめます)。

| 判定 | 条件 | 表の `detail` |
|---|---|---|
| `registered` | `--registered` の一覧にある | |
| `bad-status-json` | `registration_status.json` を JSON として読めない | エラー |
| `not-ready` | `registration_status.json` の `ready` が true でない | |
| `status:<語>` | `STATUS` が `DONE` (`tree register` が受け付ける内容) でない。`<語>` は `STATUS` の先頭の語で、ファイルが無ければ空 | `STATUS` の内容 |
| `no-node-moves` | `node_moves.tsv` が無い | |
| `prefix` | 木の id が `--exclude-prefix` のどれかで始まる | 当たった接頭辞 |
| `depth0` | `d1.jsonl` が無い (根の局面しか読んでいない)。`--keep-depth0` を付けるとこの判定をせず候補に残す | |
| `bad-readings` | `--excluded-readings` の照合で `d<段>.jsonl` に読めない行がある (読めない行を飛ばすと外した読みを見落とすので候補にしない) | ファイル・行番号とエラー |
| `excluded-reading` | `--excluded-readings` の読みそのものを含む | `matched=<一致した行数> readings=<木の読みの行数>` |
| `registered-copy` | id・own・世代が同じ `registered` の出力先と内容が同じ | その登録済みの出力先 |
| `compare-failed` | id・own・世代が同じ登録済みの出力先と内容を比べられなかった (どちらかの `d<段>.jsonl`・`node_moves.tsv` が無い・読めない) | 相手の出力先とエラー |
| `duplicate` | id・own・世代が同じで、パスの順で先の候補と内容が同じ | 候補に残した出力先 |
| `candidate` | 上のどれでもない | `--excluded-readings` があれば `matched=0 readings=<行数>`。id・own・世代が同じで内容の違う登録済みの出力先か候補があれば `differs-from=<その出力先>` |

`--registered` は登録済みの出力先を 1 行に 1 つ書いたテキストです (空行は無視)。一覧の行も見つけた出力先も、シンボリックリンクや別名を解決した実体のパスにしてから比べるので、同じ場所を別の書き方で指していても一致します。stdout の「登録済みの一覧のうち根の下に無かったパス」は、もう無いパスと、どの根の下でも見つからなかったパスの数です (一覧の書き間違いや、渡し忘れた根の目安)。

`--excluded-readings` は外した読みの一覧 (JSON Lines) で、各行の `key` (SFEN の盤・手番・持ち駒。木の読みと同じ表記の文字列で比べる)・`nodes`・`sec` を使います (`ply` など他の欄は無視、空行は無視、3 つの欄が欠けた行はエラー)。木の `d<段>.jsonl` に **key・nodes・sec が全部一致する行** が 1 つでもある木だけを外します。同じ局面を後で読み直した行は nodes・sec が違うので一致せず、その値で作った木は候補に残ります。`sec` は両方を JSON の数値 (倍精度) として読んだ値が等しいかで比べるので、`2320.998829311` と `2.320998829311e3` のような表記の違いは一致し、表示の桁を丸めた値は一致しません。`0.0` と `-0.0` は等しいものとして扱います。実際に探索した `nodes` が記録されていない読み (`nodes` が null) は一致しません。

照合では、候補の木の読みと一覧の行をそれぞれ key・nodes・sec の SHA-256 の先頭 16 バイトにし、この 128 bit の一致で判定します (元の値の比べ直しはしません)。木の読みの側は 1 行 20 バイトの一時ファイル (`--table` と同じディレクトリに作り、終わると消える) に並べ、一覧は 262,144 行 (約 4 MiB) ずつ表に載せて一時ファイルを 1 回ずつ走査します。メモリは一覧の行数にも木の読みの行数にも比例せず、出力先の数 (パスと判定の 1 行ぶん) にだけ比例します。一覧が 262,144 行以下なら走査は 1 回です。

写しの判定 (`registered-copy`・`duplicate`) は、id・own・世代が同じ出力先が複数あり、その中に候補があるときだけ、候補と登録済みの出力先の `d<段>.jsonl` と `node_moves.tsv` (ファイル名と内容) の SHA-256 を比べます。id・own・世代だけでは同じ木とみなしません。同じ名前で読み直した木は内容が違うので、それぞれそのまま判定し、候補に残るときは `detail` に `differs-from=` で相手の出力先を書きます。比べる側のどちらかで `node_moves.tsv` か全部の `d<段>.jsonl` が無い・読めないときは、内容が違うとは決めず、候補を `compare-failed` にして登録に渡しません (同じ内容の登録済みの出力先が他にあれば `registered-copy`)。id・own・世代を取れない出力先 (下の `-`) は写しの判定をしません。

木の id・own・世代は、出力先の実体のパスの末尾 `<id>/<own>/g<世代>` から取ります。末尾が `g<数字>` でない出力先は 3 つとも `-` で、`--exclude-prefix` には当たりません。

出力:

- `--out`: `candidate` の出力先を 1 行に 1 つ、パスの順で書きます。出力 (`--out`・`--table`) は `--root` の外に置きます。`--root` の下を指すとき (リンク越しの別名を含む。木の読みや `node_moves.tsv` を上書きしないため)、`--out` と `--table` が同じファイルを指すとき (相対パスと絶対パス、リンク越しの別名を含む)、`--registered`・`--excluded-readings` のファイルを指すときは、何も書かずに止まります。`--root` に渡した書き方に続くパスなので、そのまま `tree register` の引数にできます。
- `--table`: 全部の出力先を 1 行ずつ、パスの順に並べた TSV (見出し行つき) です。列は `tree` (出力先)・`id`・`own`・`generation`・`place`・`verdict`・`detail`。`place` は根から `<id>/<own>/g<世代>` の手前までの相対パス (無ければ `-`) で、複数の台の写しを 1 つの根の下に並べたときに台を見分けられます。候補から外れた出力先とその理由は必ずこの表に残ります。
- stdout: 出力先の総数と、判定ごとの件数。

## `master validate`: 元データの全件検査

```bash
book_mine master validate --master moves.tsv
```

`--master` は必須です。入力を変更せず、次の 13 列をこの順で持つ TSV を検査します。

```text
key ply move role source evals nodes cond value_kind lineage tier since note
```

- 見出しの完全一致、各行の列数、1 行以上の手があること。区切りはタブ、行末は LF または CRLF です。セル内のタブ・CR・LF は使えません。
- `since` が空白だけでないこと、同じ `key` と `move` の重複がないこと。
- `role` は `main` / `alt` / `bench` (控え、指さない手) / `record`、`tier` は `curated` / `wide`。
- `ply` は空か ASCII の 10 進数字だけ。`evals` は空かカンマ区切りの整数です。先頭の `+` (例 `+5`) も受け付けます。各値と加算途中の合計は i64、偶数丸めした平均は i32 の範囲が必要です。小数・指数表記・値の前後の空白・空の要素は使えません。`bench`・`record` の行も検査します。
- 局面に `main` / `alt` がある場合、`main` がちょうど 1 行あること。全行と `curated` の行だけでそれぞれ検査します。指さない手 (`bench`・`record`) だけの局面も受け付けます。
- `key` は両玉を 1 枚ずつ含む SFEN の盤面・手番・持ち駒の 3 フィールドで、読み込んだ局面を書き出した正規形と一致すること。
- `move` はその局面の合法手を表す USI 文字列であること (`bench`・`record` も検査します)。
- `main`・`alt` の `move` が成れる角・飛・歩の不成でないこと (下の「成れる角・飛・歩の不成」)。誤りとして報告するので、残っていれば `master audit --apply` で `record` にしてから検査し直します。

`--schema` を省いた場合は語彙を検査しません。誤りがあれば、検出した誤りをすべて行番号順に stderr に出し、終了コードは非 0 になります。局面単位の誤りには、その局面が最初に現れた行番号を付けます。UTF-8 ではない行は行番号つきで報告し、次の行の検査を続けます。列数が不正な行は、セルを確定できないため行の内容の検査を省きます。局面を読み込めない場合は、その局面の合法手検査を省きます。

誤りが無ければ、局面数・手の数、tier ごとの局面数と手の数、role ごとの行数、入力に現れた source ごとの行数を stdout に出し、終了コード 0 で終わります。同じ局面が両方の tier にあれば、それぞれの局面数に数えます。評価値の検査は `master audit` などの読み込み側と同じ規則です。

### 成れる角・飛・歩の不成

角・飛・歩 (先手・後手とも) の不成のうち、同じ移動の成る手も合法な手は、元データの `main`・`alt` にしません (`record` なら置けます)。探索の手の生成はこの不成を作らないので読みからは出ず、成った場合と比べて得になる局面もほぼありません。歩の一段目 (後手は九段目) のように不成が反則の所は合法手でないので対象外、桂・銀・香の不成と、成った駒の手も対象外です。

`master audit` (元データを読むどの実行でも) と `tree register` は、この手の行を `record` にして (行は消さず、note に `<since>: 成れる角・飛・歩の不成なので main から record` などを足す) から処理します。`--apply` が無ければ元データは書き換えません。`main` を `record` にした局面は、残る `main`・`alt` のうち、curated の手が残っていればその中から、合法な手、`evals` の平均の分かる手、その値の高い手、元データの順の先を `main` にします (`master cycles` の替わりの main と同じ順。値は伝播前の値なので、`master audit` はこの後の点検で main をさらに替えることがあります)。手が残らなければ、その局面は指さない手だけの局面になります。`master audit --revive-bench` は、この手の `bench` を alt に戻しません。record にした行の数 (先手番・後手番)、うち main の数、main を選び直した局面、手が残らない局面を stderr に出します。`borrow value` は、大きい定跡から登録する自分の手番の手からこの手を外し、外した数を stderr に出します。

### 元データの schema

古い 11 列の見出しは移行が必要というエラーになります。旧書式を読むモードはありません。
新しい列の構造は全ての読み込み・書き込みで検査します。

| 列 | 意味と規則 |
|---|---|
| `cond` | evals が空なら空。それ以外は `[A-Za-z0-9._-]+` の条件名、`unknown` (確定できない)、`mixed` (異なる条件が混ざったと確認済み) |
| `value_kind` | evals が空なら空。それ以外は `single` (evals は 1 個)、`mean:N` (N≥2、evals は N 個)、`tree:K` (K≥1、evals は 1 個)、`unknown` (個数を問わない)。N・K は ASCII の十進数字 |
| `lineage` | 空、または `<種類>:<名前>` のカンマ区切り。種類は `[a-z][a-z0-9_-]*`。名前は空でなく、カンマ・タブ・改行・空白を含まない |
| `nodes` | 読みのノード数の目安。cond がノード停止なら要求した上限、時間停止なら実測の丸め。複数回で異なる場合は最大。cond が unknown / mixed なら意味も不明な参考値。書式の検査は行わない |

`master validate`・`export`・`add`・`add-reads`・`hold-early`・`tree register`・`borrow value` は
`--schema <dir>` を受け取ります。指定したディレクトリには次の全ての TSV が必要です。
見出しは以下の順 (区切りはタブ)。行順を保ち、各表の最初の列の重複を拒否します。

| 表 | 見出し |
|---|---|
| `sources.tsv` | `source description replaceable` |
| `conditions.tsv` | `name fingerprint stop nodes_counting threads hash_mb multipv draw_black draw_white engine_sha256 eval machine note` |
| `lineage_kinds.tsv` | `kind description` |

sources の source と description は空不可。replaceable は `0` / `1` で、`1` は読みから作られ、
木の結果で main と評価値を更新してよい source です (curated の main を保護する規則は別に適用)。
conditions の name は cond の条件名の構造を満たし、予約値 unknown / mixed は使えません。
fingerprint は空 (指紋の無い古い条件) か 64 桁の十六進です。空でない指紋は大文字・小文字を区別せず重複不可。
他の列は不明なら空にできます。lineage_kinds の kind は lineage の種類の構造に従います。

schema 指定時は、source・cond の名前・lineage の種類が対応する表にあることを検査します。
省略時は語彙の照合だけを省き、構造の検査は行います。指定された dir や表の欠落・不正を
既定値で補うことはありません。読み由来として更新できる既定の source は `read`・`ext` です。

```bash
book_mine master validate --master moves.tsv --schema schema
```

## `master add`: 元データへの行の追加と置換

```bash
book_mine master add --master moves.tsv --add add.tsv
book_mine master add --master moves.tsv --add add.tsv --apply --schema schema
book_mine master add --master moves.tsv --add replacements.tsv --replace --apply
```

`--master` と `--add` は同じ 13 列の TSV です。同じ `(key, move)` が既にあれば追加を省き、
role・tier・source に差があれば `skipped conflict: ...` に列ごとの差を出します。
追加表の中の `(key, move)` の重複は、既存行と一致する場合もエラーです。
元データと追加後の全体を `master validate` と同じ規則で検査し、検出した誤りをすべて報告します。
`--schema <dir>` があれば、下記の 3 表で source・cond・lineage の種類も検査します。

`--replace` は curated の行も全列置き換えます。古い表から作った置き換え表で新しい note や evals を上書きしないよう注意してください。
既存の `(key, move)` に一致する行が対象です。
新しい main の行と、元の main を alt にした行を同じ追加表に書くと、まとめて入れ替えられます。
一致した行の数を `replaced existing rows`、実際に値が変わった列の行数を `changed <列>: <数>` に出します。
tier が変わる行と、curated の行を置き換える場合は、
`replaced: <key> | <move>: <列>: <旧> -> <新>, ...` の形で各行の差も出します。
値が同じ行も置換数に含め、変化のない列は出しません。追加数には新しい行だけを数えます。

stdout には既存行の省略数、新しい局面と手の数、tier・role・source・since ごとの件数、
`added <tier>/<role>` を出します。source の集計は export と共通で、schema 指定時は sources.tsv の行順、
表に無い値はその後ろに辞書順で並べます。schema 省略時は出現した値の辞書順です。
行は key、role (main・alt・bench・record)、move の順で並べます。
既定は dry run で `OK (dry run): rows A -> B; no files written` と終わります。
`--apply` の場合だけ一時ファイルへ全行を書き、元データを置き換えて `applied: rows A -> B` と報告します。
検査に失敗したら書き換えません。元データと追加表が同じファイルなら、dry run でも拒否します。

## `master hold-early`: 序盤の main の替わりを保留に戻す

```bash
book_mine master hold-early --before before/moves.tsv --master moves.tsv \
  --since release-tag --out release.main_changes_held.tsv
book_mine master hold-early --before before/moves.tsv --master moves.tsv \
  --since release-tag --out release.main_changes_held.tsv --schema schema \
  --continuation 6 --hold-keys hold.keys --apply
book_mine master hold-early --before before/moves.tsv --master moves.tsv \
  --since release-tag --out release.main_changes_held.tsv --schema schema \
  --continuation 6 --deep-opp-freq strong.db --hold-keys hold.keys \
  --cuts-out release.held_cuts.tsv --cuts-enqueue-out release.held_cuts.jsonl \
  --cuts-exclude-queue targets.jsonl --apply
```

`tree register`・`master audit` の後で、版を作る前からある序盤の局面の main を戻し、
替わろうとした手と継続手数による分類を一覧にします。`--continuation` を省くと、対象をすべて保留します。
`master cycles --apply` より前 (`tree register`・`master audit --apply` の直後) に流します。cycles の後に流すと、
cycles が循環を塞ぐために外した手を main に戻すことがあります。

| 引数 | 既定 | 意味 |
|---|---|---|
| `--before <PATH>` | 必須 | 版を作る前の元データ (13 列 TSV) |
| `--master <PATH>` | 必須 | 登録・監査後の元データ |
| `--below-ply <N>` | `20` | before の main の ply が N 未満の局面を対象にする |
| `--continuation <N>` | なし | 旧手・新手の後で main がともに N 手以上続く局面は replace とし、新しい main を残す |
| `--deep-opp-freq <PATH>` | なし | 相手のよくある応手も継続判定に含める頻度定跡 (YaneuraOu 形式 .db) |
| `--deep-min-games <N>` | `3` | 継続判定で辿る相手の手の count (局数) の下限 |
| `--hold-keys <FILE>` | なし | 分類によらず hold にする局面キー (1 行 1 キー) |
| `--schema <DIR>` | なし | 他の master コマンドと同じ語彙・読み条件の表 |
| `--since <TAG>` | 必須 | 書き戻す 2 行の note に付けるタグ |
| `--out <PATH>` | 必須 | 対象の変更と分類の一覧 TSV (既存なら置換) |
| `--apply` | 無効 | 検査後、hold の局面だけ master に書き戻す |
| `--cuts-out <PATH>` | なし | hold の局面の、継続が N 手に届かない枝の途切れた局面の一覧 TSV。`--continuation` が要る |
| `--cuts-enqueue-out <JSONL>` | なし | 途切れた局面を根にした `tree enqueue --from` の行。`--continuation` が要る |
| `--cuts-depth <N>` | `6` | キューの行の段数 (0〜20) |
| `--cuts-priority <N>` | `100` | キューの行の priority の基準。順位 (0 始まり、99 で頭打ち) を足す |
| `--cuts-opp-n` / `--cuts-stop` / `--cuts-rounds` / `--cuts-draw-black` / `--cuts-draw-white` | なし | キューの行の `opts` の `opp_n`・`stop`・`rounds`・`draw_black`・`draw_white`。指定した項目だけを書く |
| `--cuts-tag <TAG>` | `held` | キューの行の id `<tag>_<根の手数>_<局面の hash>_<b\|w>` に入れる名前 ([A-Za-z0-9_-] の 1〜40 文字) |
| `--cuts-exclude-queue <PATH>` | なし、繰り返し可 | 既存のキュー (JSON Lines)。`master walk --exclude-queue` と同じ除外で、同じ局面・同じ自分の側の木がある途切れはキューの行にしない |

両方の表に main があり、その手が異なる局面だけを対象にします。before にない局面、
before の main の ply が空の局面は対象外です。既定では 19 手目までが対象で、20 手目は含みません。

頻度定跡を省略した場合の継続手数は、変更前の `--master` を使い、旧手・新手をそれぞれ指した後の局面から main を
辿って数えます。最初の旧手・新手自身は含まず、最大 60 手までです。盤・手番・持ち駒のキーを
そのまま照合し、手数は含めず、先後反転もしません。main がない局面か、すでに通った局面に戻ったところ (循環) で止まります (`master audit --deep-plies` と同じ扱い)。
`--continuation N` 指定時は両方が N 手以上なら `replace`、片方でも不足なら `hold` です。
N は非負整数で、0 ならすべて replace、頻度定跡なしでは 60 より大きければすべて hold になります。
`--deep-opp-freq` を渡すと、替わった局面の手番側を自分として、自分の手番では main だけ、
相手の手番では main と count が `--deep-min-games` 以上の全応手を辿り、最短の継続手数を使います。
相手の手を指した先に自分の main がなければ、その相手の 1 手分で止まります。循環はそこで止めます。
`--continuation N` の N 手で計数を打ち切り、省略時は 60 手まで数えます。
一覧の `old_depth`・`new_depth` もこの値です。頻度定跡は読み込み時に下限以上の手だけを残し、局面キーと指し手だけを保持します。
直接キーがなければ反転キーを検索して手を元の座標に戻します。元データのキーは反転しません。
標準出力の要約に使用した頻度定跡のパスを出します。
`--deep-min-games` の明示指定には `--deep-opp-freq` が必要です。根の手が非合法ならエラーとし、頻度定跡の非合法な応手はその手だけを飛ばします。

`--hold-keys` に含む局面はこの判定に優先して hold にします。空行と、前後の空白を除いた先頭が
`#` の行は無視します。ファイルにあるだけでは対象を増やさず、main が替わっていない局面には何もしません。
全対象の分類は書き戻し前に確定します。

hold で戻す手が master にあれば、読みの値などを保持して role を main、tier を before の main と
同じにします。なければ before の行を復元します。替わろうとした手の role は alt にし、
before の main が curated なら tier を wide にします。両行の note の末尾に
`<TAG>: 20 手目より前、読みが浅いので保留 (今の main)` を追記します
(既存 note との区切りは `; `、手数は `--below-ply` に従います)。since 列は変更しません。
replace の局面の行は変更しません。他の候補手の行も保持します。

### 継続が足りない枝の途切れ (`--cuts-out`・`--cuts-enqueue-out`)

hold を解くには、継続が足りない枝の先を木で読む必要があります。`--cuts-out` は、hold の局面ごとに、
旧手・新手のうち継続手数が N 未満の側について、最短の枝が途切れた局面を 1 行ずつ出します。
継続手数は上の分類と同じ計数で求めるので、`depth` は一覧の `old_depth`・`new_depth` と一致します。
最短の枝が複数あるときは、その局面で辿る手 (main と相手の応手) の USI の文字列の順で先の枝を選びます。
`--deep-opp-freq` なしでは計数が 60 手で止まり途切れを持たないので、`--continuation` は 60 以下にしてください (超えるとエラー)。
`--hold-keys` で hold にした局面でも、両方が N 手以上なら行を出しません。

| 列 | 意味 |
|---|---|
| `ply`・`side`・`key` | hold の局面の手数・手番・局面キー |
| `which`・`move` | 旧手 (`old`) か新手 (`new`) か、その手 |
| `depth` | その手の後に続いた手数 |
| `cut_ply`・`cut_key` | 途切れた局面の手数 (`ply` + 1 + 根の手の後の手の数) と局面キー |
| `stop` | 途切れた理由。`own` (自分の手番で main がない)、`opp` (相手の手番で main も辿る応手もない)、`cycle` (経路上の局面に戻った。`cut_key` は戻った先)、`illegal` (main が指せない)、`no_legal` (main がなく、辿る応手がどれも指せない) |
| `moves` | hold の局面から途切れた局面までの手順 (USI。旧手・新手を含む) |

`--cuts-enqueue-out` は、途切れた局面を根にした `tree enqueue --from` の行を書きます。`own` は hold の局面の手番の側で、
同じ (途切れた局面, `own`) は 1 行にまとめ、根の手数はそのうち最も小さい手数にします。`memo` に
`hold-early hold_ply=<最も小さい hold の局面の手数>` と、まとめた枝ごとの
`ply=<hold の局面の手数> key=<hold の局面キー> <old|new>=<手> depth=<続いた手数>` を `; ` 区切りで入れます。
行は hold の局面の手数・根の手数・局面キーの順に並べ、その順位を priority に足します。重みでの絞り込みはしません。
`--cuts-exclude-queue` で除いた組の数は標準出力に出します。`--apply` の有無によらず、どちらの出力も書き戻す前の元データで数えます。

一覧の列は `ply side key old_main old_source old_tier old_value_black new_main new_value_black old_depth new_depth class`
です (区切りはタブ、side は `b` / `w`)。既存の列順を保ち、末尾の 3 列に継続手数と分類を出します。
old 側は before、new 側は変更前の master から取り、ply・key の順に並べます。
値は evals の平均を既存の規則で偶数丸めし、後手番では符号を反転した先手から見た値です。
値がなければ空欄にします。stdout には対象局面数 (先手番・後手番別)、hold・replace の局面数と
書き戻した局面数を出します。

両入力と変更後の表を `master validate` と同じ規則で検査します。検査で失敗した場合は `--apply` の有無によらず一覧も master も書き込みません。
`--apply` を省くと一覧だけを書き、master は変更しません。保存には一時ファイルからの原子的な
置換を使い、master が読み込み時から変わっていれば保存を拒否します。既存行の順序は保持し、
復元した行は末尾に追加します。改行は LF になります。master を書き換える他のコマンドと同時に
実行しないでください。一覧は master より先に保存するため、master の保存に失敗しても一覧が残る場合があります。
既存の master の編集経路と同様、表と索引をメモリに保持します。

## `master add-reads`: 読みから追加候補を作る

```bash
book_mine master add-reads --master moves.tsv --out out/add.tsv --since candidate \
  --min-nodes 900000000 --draw-pin 3 reads.jsonl more-reads
```

`--master`、`--out`、`--since` と 1 つ以上のログパスが必須です。
`--since` は空白だけの文字列やタブ・CR・LF を含む文字列を拒否します。
ノード数の下限は既定 9 億、draw-pin は既定 3 で、どちらも非負整数です。
ログの読み方・除外規則は [共通の読みのログ形式](#読みのログ形式-export--add-reads-共通) に従います。
元データを検査してから読みを比較し、候補を追加した全体にも同じ検査を行います。
同じログファイルを 2 回指定しても、パスを正規化して 1 回だけ読みます。
全局面の読みを一度に比較するため、メモリ使用量は取り込む読みの件数と各 MultiPV のサイズ、
追加候補の行数、報告の行数に比例します。

局面ごとに深い読みだけを残し、最小の MultiPV 本数を n として各回の先頭 n 手を比べます。
各回の先頭値からの差を求め、全回で比べる範囲にあり、差の平均が 20 未満で、
どの回の差も 40 未満の合法手を選びます (`consensus`)。
深い読みが 1 回だけなら先頭手だけ (`single`)、条件を満たす手がなければ
nodes・ply・順序付きの (move, cp) が最大の読みの先頭手 (`fallback`) を選びます。
選んだ手は差の平均の昇順、1 位の回数の降順、USI の順に並べ、
新しい局面では先頭を main、残りを alt にします。
既存局面では新しい手を record にし、既存の手は変更しません。
複数回の読みで選択から外れた手も、出現した回での差の平均が 40 未満なら record に残します。
非合法手は行にせず、報告だけに残します。

条件の記録がないログなので `cond=unknown`、lineage は空です。各手の evals が 1 個なら `value_kind=single`、N 個なら `mean:N` です (その手が現れた読みの回数)。`--schema` で入力と追加候補の語彙も検査します。

候補の ply は浅い読みも含めた最小値です。evals は深い読みの順の値、nodes は深い読みの最大値です。
追加する行の cp は整数値が必須です。小数・指数表記でも整数値なら十進整数で evals に保存します。
整数にならない cp や、evals の合計・平均の範囲外は局面と手を示すエラーとし、
候補・報告のどちらも書かずに非ゼロで終了します。
比較は有理数で行い、差の平均は f64 に変換して有効 12 桁で表示します。
nodes は有効 2 桁の指数表記、note の億ノード数は有効 2 桁に丸めてから表示します。
note には読みの回数・1 位の回数・差の平均と、record にした理由を書きます。

出力は `--out` の 13 列 TSV と、その名前に `.report.tsv` を付けた報告です。
報告の列は key・reads・shallow_dropped・move・role・mean_gap・firsts・status・reason・
existing_moves・compared_reads・multipv_lengths・compare_n・gaps・reread です。
status は add / existing / illegal / not_selected、reason は single / consensus / fallback、
gaps の未出現の回は `NA` です。1 回だけの読みで先頭 2 手の差が 10 未満なら reread は `True`、
それ以外は `False` です。報告は key・move 順、候補は add と同じ行順になります。

出力がどちらか 1 つでも既にあれば拒否し、入力と同じファイルにも書けません。
親ディレクトリは必要なら作り、両方を一時ファイルに書き終えてから確定します。
stdout には読みの取込・除外数、浅い読みの除外数、対象局面数・追加数・非合法手数、
先後別の手数帯 (1–20、21–40、41–60、61 以上) ごとの追加局面数・手数を出します。

## `master export`: 定跡・プリセット・注釈の書き出し

```sh
book_mine master export --master moves.tsv --out out/book --tier wide
book_mine master export --master moves.tsv --out out/book --schema schema \
  --reads reads.jsonl more-reads --min-nodes 900000000 \
  --journal state/journal.jsonl more-journals --force
```

出力先の親ディレクトリは、書き出すときに必要なら作成します。元データ全体に `master validate` と同じ検査を行い、
誤りがあれば全件報告して書き出しません。検査後に tier で絞り、局面の key、role (main、alt、bench、record)、
USI の手の順に並べます。入力は変更しません。

bench は record と同じく `.db` に value=-30000/count=0 で残し、どのプリセットでも選びません。

| 引数 | 既定 | 意味 |
|---|---|---|
| `--master <tsv>` | 必須 | 13 列の元データ |
| `--out <prefix>` | 必須 | 下記 4 ファイルの接頭辞 |
| `--tier wide\|curated` | `wide` | wide は全行、curated は curated の行だけ |
| `--schema <dir>` | なし | 3 表による source・cond・lineage の語彙検査 |
| `--reads <path>...` | なし | JSONL ファイル、または再帰的に `*.jsonl` を探すディレクトリ。複数指定可 |
| `--journal <path>...` | なし | 深さを照合する tree 台帳の JSONL ファイル、または再帰的に `*.jsonl` を探すディレクトリ。tree 台帳でないファイルは飛ばす。複数指定可 |
| `--min-nodes <n>` | `900000000` | 取り込む reads のノード数の下限 (非負整数) |
| `--draw-pin <cp>` | `3` | draw の指定が空の読みで、先頭の評価値の絶対値がこの値以下なら除く (非負整数) |
| `--bad-eval <cp>` | `-150` | 到達局面の値がこの値以下なら報告 (有限値) |
| `--strict-bad-eval` | 無効 | 悪い到達局面が 1 つでもあれば、非ゼロで終了し書き出さない |
| `--base <db>` | なし | 局面の追加・削除と、手の集合が変わった局面の数を表示 |
| `--force` | 無効 | 既存の出力を置き換える |

schema の表と検査は [元データの schema](#元データの-schema) を参照してください。
`"sources"` には `sources.tsv` の source → description を行順で書きます。省略時は空のオブジェクトです。
`"conditions"` は `conditions.tsv` の name を鍵とし、各行の name 以外の列を文字列で写します。
`fingerprint`、`eval`、`stop`、`threads`、`hash_mb`、`multipv`、`machine`、`note` のほか、
`engine_sha256`、`nodes_counting`、`draw_black`、`draw_white` も含みます。空欄は空文字列です。
`--schema` 省略時は `sources` と同じく空のオブジェクトです。手の `cond` から条件の説明を引けます。

出力は次の 4 ファイルです。いずれかが既存なら `--force` が必要です。ディレクトリへの出力や、
元データ・読みのファイル・台帳・base・schema の各表と同じファイルへの出力は `--force` でも拒否します。
全ファイルを出力先と同じディレクトリの一時ファイルに書き終え、既存の出力をそれぞれ
同じディレクトリの一時名へ rename で退避してから、新しいファイルを順に確定します。
途中で失敗したら、確定済みの新しい出力を削除し、退避した出力を元の名前に戻します。
復元にも失敗した場合はエラーに退避先のパスを含めます。全て成功したら退避先を削除します。新しい出力を全て確定した後に退避先の削除だけが失敗した場合は、新しい出力を残したまま、退避先のパスを含むエラーで終わります。
失敗時は残った一時ファイルを削除し、新しく作った親ディレクトリも空の場合だけ削除します。
4 ファイル全体は単一の原子的操作ではありません。`--force` では退避から確定までの間、元の名前に出力が無い (退避先の一時名にだけある) 時間があり、クラッシュや電源断ではその状態や一部の出力が残り得ます。

| ファイル | 内容 |
|---|---|
| `<prefix>.db` | `#YANEURAOU-DB2016 1.00`。局面は `sfen <key> 0`、手は `<move> none <value> 0 <count>`。main は value=100/count=2、alt は 100/1、bench・record は -30000/0 |
| `<prefix>.default.explore.txt` | main・alt が合計 2 手以上ある局面で main のみを選ぶプリセット |
| `<prefix>.random.explore.txt` | 同じ局面で main・alt の全手を選ぶプリセット |
| `<prefix>.annot.json` | UTF-8、2 空白インデント、末尾改行付きの注釈 |

各プリセットの冒頭には名前と DB のファイル名・SHA-256 が入ります。
注釈の最上位の鍵は `format` (`rshogi-book-annotations`)、`version` (2)、
`book` (`file` と `sha256`)、`sources`、`conditions`、`positions` の順です。
条件の説明と深さは欄の追加であり既存の意味を変えないため、version は 2 のままです。`master check` は version 1 と 2 の両方を受け付け、それ以外と version の欠落を拒否します。
`positions` の鍵は手数を除いた SFEN で、各局面には次の値があります。

- `ply`: 元データの同じ局面にある空でない ply の最小値。全て空なら null。
- `moves`: USI の手を鍵とするオブジェクト。各手は `role`、`source`、`nodes`、`cond`、`value_kind`、`note`、
  `since`、`tier` の文字列、カンマで分けた `lineage` の配列、数値の配列 `evals`、
  相手の返信の配列 `replies`、整数 `depth_after` を持ちます。
  `--journal` を指定し、`cond` から指紋を特定できた手には、探索深さの整数配列 `depths` も追加します。
- `read`: 採用した読みがある場合だけ追加する集計。
  `n` は読みの回数、`nodes` は最大ノード数を有効数字 2 桁の指数表記にした文字列
  (例 `1.2e9`)、`top` は各読みの先頭の評価値の平均、`main` は元データの main が
  読みに含まれた回の評価値の平均 (無ければ null)、`main_n` はその回数です。
  平均は有理数で計算した後に浮動小数へ変換し、小数点以下 1 桁に偶数丸めします。
  `12.0` のように整数値でも浮動小数として書きます。

`depths` は同じ条件・局面の読みの MultiPV にその手があった回ごとの `depth` を小さい順に並べます。
同じ指紋・局面では最初の有効な読みだけを使います。対象の読みが無い手は `[]` です。
`--journal` 省略時や `cond=unknown` など指紋を特定できない手では欄を省略します。
欄なしは未照合、空配列は照合したが該当する読みなしを表します。
schema 指定時は条件名から指紋を解決し、指紋が空欄の条件には `depths` を書きません。
schema 未指定時だけ台帳の指紋の先頭 8 桁から解決し、先頭が衝突する場合はエラーにします。

台帳は `tree serve` の形式を使います (`kind` は形式の版、`provenance=engine` が engine の読み)。
最初の確定行が JSON でない、`kind` が無い、または `kind` が `book-tree-` で始まらないファイルと、
確定行の無いファイル (空ファイルを含む) は丸ごと飛ばし、
`journal: skipped (not a tree journal): <path>` を stderr に出します。
tree 台帳と判定したファイルでは、不正な確定行や異なる形式の版はエラーにします。
engine の行は出力対象外の局面も読みの妥当性と指紋・条件の一致を検査し、不正な行は数えて飛ばします。
末尾の改行の無い未完了行だけを無視します。入力の変更やロック取得はしません。
他の台帳から取り込んだ行 (`provenance` が `engine` でない行) は数えません。元の台帳も `--journal` に渡してください。
各ファイルを開いた時点のサイズまで行ごとに読み、出力対象の局面の読みだけを保持します。
重複判定は台帳と同じく MultiPV 全体の move/cp/depth と bestmove を使い、
同じ指紋・手数を除いて正規化した局面の重複はファイルをまたいでも 1 回に数えます。
nodes・計測時間・手数だけが違う読みも重複です。同じ指紋・局面で値や深さが異なる後続行は、衝突として数えて飛ばします。
engine の行数、engine 以外の行数、採用数、重複数、不正行数 (`invalid`)、値の衝突数 (`conflicting`)、未完了の末尾を無視したファイル数、
飛ばしたファイル数 (`skipped-files`)、エラー数 (不正な確定行では最初のエラーで終了) を stderr に出します。
`--reads` のノード数・draw の絞り込みは `--journal` には適用しません。
`.db` の値・深さ・採択数には探索深さを使いません。

`replies` は、その手を指した後の相手の全合法手のうち、選択した tier の定跡局面に着く手を
USI 順に並べたものです。`depth_after` は main・alt の辺だけから作る 2 手単位のグラフで、
強連結成分を縮約して求めます。成分内の局面数を n とすると重みは `2 × (n − 1)`、
成分間の辺の重みは 2 とし、最長路を求めます。同じ局面への返信だけなら 0、
同じ成分の別局面ならその成分の最長路、別成分なら 2 と相手成分の最長路の和を候補にし、
手ごとの最大値を使います。bench・record は常に 0 です。実際に指せる手数や評価値を保証する値ではありません。

到達検査は初期局面から先手・後手を別々に辿り、自分は main・alt、
相手は全合法手のうち定跡の局面へ着く手を使います。自分の手番の値は
`read.main`、main の evals の平均、`read.top` の順に選びます。
stdout には curated main と wide alt が混在する局面の警告、悪い到達局面と経路、
値が無い局面数、base との差、tier・role・source・since の件数、
プリセット局面数・返信リンク数・最大 depth_after、DB の SHA-256 をこの順に出します。

### 読みのログ形式 (export / add-reads 共通)

UTF-8 の JSON Lines (1 行 1 オブジェクト) を使います。先頭の BOM と空行は許容します。
ファイルを複数指定でき、ディレクトリは再帰的に `*.jsonl` を探します。

```json
{"key":"lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b -","ply":1,"nodes":900000000,"multipv":[{"move":"7g7f","cp":25}],"draw":{"b":0,"w":0}}
```

`key` は手数を除いた SFEN、`ply` は局面の手数、`nodes` は探索ノード数です。
`multipv[].move` は USI の手、`multipv[].cp` は**手番の側から見たセンチポーン**です。
`draw` は読みで指定した引き分け設定のオブジェクトです。値そのものは比較に使わず、
空でないオブジェクトかどうかで draw 未指定を判定します。

reads はパスを正規化して重複を除き、パス順・行順で取り込みます。
`master export` では、`--tier` で絞った後の定跡にある局面の読みは、MultiPV と数値を丸ごとメモリに保持します。
それ以外の局面の行は key の正規化直後に除外し、合法手の検査や読みの保持をしません。
メモリは対象局面の読みの総サイズに比例し、対象外の読みは注釈に含めません。
`master add-reads` は全局面が対象で、読みと件数を集計します。
取り込む行については、以下を検査します。
key は両玉を持つ SFEN の 3 フィールドを正規化し、ply は 1 以上の整数、
multipv は重複のない文字列 move と有限な数値 cp の配列であることを検査します。
nodes・cp は整数または小数です。整数はそのまま、小数の字面は f64 に読み、
その最短の十進表現を正確な有理数にして比較・合計します。
JSON の規格の外の値 (NaN・Infinity) を含む行は、使わない欄にある場合も malformed に数えます。
不正な行、ノード数不足、評価値が降順でない読み、先頭が非合法手の読み、
draw が未指定・null・空オブジェクトで先頭値が draw-pin の範囲内の読みを除きます。
draw は null またはオブジェクトだけを受け付けます。
key・nodes・ply・順序付きの (move, cp) が一致する読みは 1 回にまとめます。
局面ごとの最大ノード数を N とし、`nodes × 3 > N` の読みだけを集計に使います。

## `master diff`: 2 つの定跡の差分

```sh
book_mine master diff old.db new.db
```

手数を除いた SFEN の 3 フィールドをキーに、局面数・追加・削除・手の変更の数と
Markdown の表を stdout に出します。手の集合が同じでも記載順が異なれば変更です。
評価値・深さ・count だけの違いは表示しません。手のない局面は比較から除きます。
表は変更、追加、削除の順で、各区分は局面キー順です。変更・追加の手数は new、
削除の手数は old の値です。同じ局面の複数のブロックは手を記載順に連結し、
手数は最後のブロックの値を使います。

## `master check`: 書き出した定跡とプリセットの検査

```bash
book_mine master check book.db book.annot.json book.default.explore.txt --threads 2
book_mine master check book.db book.annot.json book.random.explore.txt --bad-eval -150
```

入力は順に YaneuraOu 形式の `.db`、手の注釈 `.annot.json`、プリセット `.explore.txt` です。入力は変更せず、結果を標準出力へ出します。`--bad-eval` は有限の数値 (既定 `-150`)、`--threads` はグラフ構築のスレッド数 (既定 `0` = 全コア) です。

- 注釈の `book.sha256` と DB の SHA-256、局面ごとの手の集合の一致を確認します。DB の重複局面・重複手・空の局面はエラーです。
- `preset membership errors` は DB に無い局面・手、`preset record selections` は role が `bench` または `record` の手の選択です。それぞれ該当する局面・手を一覧にします。
- 先手・後手それぞれを自分として初期局面から辿ります。自分はプリセットの手 (指定の無い局面は `main`) と DB の手の共通部分を使い、相手は合法手のうち DB 内の局面へ戻る手を使います。自分の非合法手は辺にせず、側ごとに `illegal own moves b/w` で除外数を出します。先後反転も行いません。プリセット行は `sfen <盤面> <手番> <持ち駒> <手>...` で、同じ局面の行は手の集合を併合します。
- `bad-eval reachable positions` は、自分の手番で DB にある到達局面のうち、値が閾値以下の局面数です。値は `read.main`、main の `evals` の平均 (閾値判定は丸めなし、表示は有効 6 桁)、`read.top` の優先順で取り、いずれも無ければ `positions without a value` に数えます。循環の内部も検査し、悪い局面ごとに直前の自分の手と根からの手順例を出します。
- 手順数は合流前の異なる手順を別々に数えます。辺が無い局面への到達を `exit ply=N`、循環する強連結成分への最初の到達を `cycle-entry ply=N` として集計します。`N` は到達局面までの初期局面からの手数です。相手が定跡外へ出る手は数えません。
- 循環は 2 局面以上の強連結成分または自己ループです。成分ごとに局面数、内部の辺数、16 手以内の最短循環の手数、`within-16` (16 手以内か)、入口局面と根からの手順例を出します。入口例は別の循環成分を経由する場合もあります。`within-16` は遡る手数の目安で、実際の千日手成立や連続王手の判定ではありません。
- 循環のある側の `total` は出口と循環入口の手順数の合計です。循環内の手順や最初の同一局面再訪までの手順数ではありません。循環の無い側では全出口までの総数・手数分布を正確に数え、`repetitions=0` を出します。カウントは `u128` で、上限を超える加算はエラーにします。

終了コードは membership error または bad-eval の局面があれば **1**、無ければ **0** です。record の選択・値の無い局面・循環だけでは失敗にしません。入力不正や読み込み失敗も **1** です。

メモリは入力の局面・手とグラフの頂点・辺に比例します。先後は順番に処理し、手順数は手数ごとの二面の配列で伝播するため、手順ごとの訪問済み集合は保持しません。SCC 分解は `O(V + E)`、分布は最大手数を `L` として最悪 `O((V + E) L)` (実際にはその手数で届く頂点・辺だけ) です。最短循環は成分内の辺だけを使い、各頂点から深さ 16 まで幅優先探索します。16 手以内に循環が無ければ `shortest-cycle=>16 within-16=false` とし、それ以上の長さは求めません。探索予算は成分ごとに頂点の取り出しと辺の検査の合計 10,000,000 回です (成分外への辺の検査も含む)。予算内に最短性を確定できなくても、16 手以内の循環が見つかっていれば、そのうち最も短い手数を `N` として `shortest-cycle=<=N within-16=true` とします。循環が 1 つも見つかっていない場合だけ `shortest-cycle=unknown within-16=unknown` とします。探索用の距離は頂点ごとに 1 バイトで、作業メモリは `O(V)` です。

出力は各見出しの後に明細を置き、所属エラー、record 選択、bad-eval (先手・後手の順)、値の無い局面数、`paths b`、`paths w` の順です。循環成分の一覧と非合法手の数は各側の paths 節に含めます。bad-eval と paths の明細は一時ファイルに蓄えてから出力し、両側のグラフを同時に保持しません。

## `master audit`: 定跡の元データの点検

逆伝播の保証検査で「千日手へ逃げられる」などのエラーや「反転キーで合流した」衝突が出た場合、エラーには該当局面の総数と先頭 5 局面のキーを表示します。反転衝突では `(保存 -15・反対 -16)` のように各向きの値も表示します。値はそれぞれの向きの手番の側から見た値です。対局で定跡を直接のキーだけで引くなら、`master cycles`・`master rejoin` は `--direct-lookup` で反転キーの衝突では止まりません。それ以外で、木の登録によって発生した反転キーの衝突は、`tree register --auto-skip-flip` で除く局面を自動で探せます。手で直す場合は、登録前の元データを用意し、エラーに出たキーだけ (値の括弧は除く) を 1 行ずつ `skip.txt` に書いて `tree register --skip-keys skip.txt` で登録し直し、再度 `master audit` を実行してください。6 局面以上ある場合や、新たに問題が見つかった場合は除外キーを追加して繰り返します。指定局面の子の局面 (木の先) の行は今までどおり足されます。列挙された局面が木の行ではなく既存の元データの局面なら、反転キーも木に無い限り「指定したが木に無かった局面」に数えられ、`--skip-keys` に書いても解消しません。原因は木が足した先の局面の手にあるため、別に調べる必要があります。除外した局面が再登録の後も同じエラーに列挙されるときも、原因はその先の局面の手にあります。除外は既存の行を削除しないため、既に登録済みの元データに同じ指定で再登録するだけでは問題の行は取り除けません。

```bash
book_mine master audit --master moves.tsv --table audit.tsv \
  --draw-value-black -70 --draw-value-white 70 [--switch-rule line [--line 40] [--deep-plies N] | --switch-rule propagated [--single-enqueue-out single.jsonl [--exclude-queue queue.jsonl]... [--walk-table walk.tsv [--single-min-weight 0.001] [--single-priority-band 0.05=1]...]]] [--opp-records] [--opp-bench] [--out-db audited.db] [--fill-replies | --bridge-replies [--bridge-table bridge.tsv] [--bridge-enqueue-out bridge.jsonl]] \
  [--refutation-table refutation.tsv] [--enqueue-out refutation.jsonl] \
  [--opp-freq strong_games.db --min-weight 0.001] [--schema schema] \
  [--apply --since TAG] [--replace-curated] [--demote-alts] [--revive-bench] [--demote-line 100] [--bad-eval -150]
```

定跡の元データの表 (上の 13 列) の main・alt の成れる角・飛・歩の不成を `record` にしてから (上の「成れる角・飛・歩の不成」、`--apply` のときだけ保存)、読みの値を `book_backprop` と同じ後退解析で伝播し、main が伝播後の 1 位から `--line` 以上負ける局面を点検の表に書きます。`--apply` を省くと元データは書き換えません。出力先の親ディレクトリが無ければ作ります。出力先が元データ・頻度の定跡・schema の表と同じファイル、または出力先どうしが同じファイルなら止まります。`key` が正規の SFEN の表記 (持ち駒の並びなど) でなければ止まります (`book_backprop` は正規でない key の局面に子局面の辺を張らないため)。

| オプション | 既定値 | 説明 |
|---|---:|---|
| `--master <PATH>` | 必須 | 定跡の元データの表 |
| `--table <PATH>` | 必須 | 点検の表 (TSV) の出力先 |
| `--apply` | なし | 条件を満たす局面の main を置換し、元データを原子的に保存する |
| `--replace-curated` | なし | curated の行がある局面でも main の置換・alt の降格を行う |
| `--demote-alts` | なし | main の置換判定後、悪い alt を bench (控え) にする。`--apply` なしでは件数だけ確認する |
| `--demote-line <CP>` | `100` | alt の降格・bench の復帰に使う差の閾値 (1 以上)。点検・main 置換の `--line` とは独立 |
| `--revive-bench` | なし | 値が戻った bench を alt に戻す。`--apply` なしでは件数だけ確認する。`--fill-replies` とは併用不可 |
| `--bad-eval <CP>` | `-150` | alt の降格に使う値の上限 (手番側からの整数値) |
| `--since <TAG>` | `--apply` 時は必須 | 置換の note に付けるタグ。空白だけ・タブ・改行は不可 |
| `--schema <DIR>` | なし | 語彙と読み条件の表。sources.tsv の replaceable=1 の source だけ置換できる。省略時は read・ext |
| `--switch-rule <line\|propagated>` | `line` | main を替える局面の決め方 (下の「main を替える規則」)。`line` は `--line`・`--deep-plies` で決め、`propagated` は値の出どころで決める |
| `--line <CP>` | `40` | 点検の線。main が 1 位からこの値以上負ける局面を表に出す。`--deep-plies` を指定したときは線未満の局面も対象にする。`--switch-rule propagated` では要約の区切りと `--bridge-table` の線だけに使う |
| `--single-enqueue-out <PATH>` | なし | `--switch-rule propagated` で、main か 1 位の値が 1 回の読みなので替えなかった局面ごとに、その手を伸ばす `tree enqueue --from` 用 JSON Lines |
| `--single-depth <N>` / `--single-max <N>` / `--single-priority <N>` / `--single-tag <TAG>` | `6` / 制限なし / `100` / `single` | 木の段数 (1〜20)、行数上限、priority の基準 (並べた順位 (0 始まり、99 で頭打ち) を足す)、id `<tag>_<根の手数>_<局面の hash>_<b\|w>` の名前 |
| `--single-opp-n <N>` / `--single-stop <CP>` / `--single-rounds <N>` / `--single-draw-black <CP>` / `--single-draw-white <CP>` | `3` / `300` / `3` / `-70` / `70` | 行の opts に書く応手数・打ち切り値・再読の回数・先手番と後手番の循環の値 |
| `--exclude-queue <PATH>` | なし | 繰り返し可。既に積んだ木のキュー (`master walk --exclude-queue` と同じく id ごとに最初の行の根と own に固定して game を合わせ、最後の行の段数が 0 なら使わない)。1 回の読みの手を、その局面の手番の側で `game` の depth 手以内に指す木 (段数 depth の局面は伸ばさないため) か、その手を指した後の局面を根 (`roots` を含む) に同じ側で読む木があれば、その手を行に入れない。手が全部除かれた局面は行を出さない。局面を根にしただけの木 (`game` にその手が無い) では除かない |
| `--walk-table <TSV>` | なし | 繰り返し可 (重みは足す)。`master walk --table` の表 (`side`・`weight`・`moves` の列を使う。先手・後手の両方を含めてよい)。`--single-enqueue-out` の行の重みを辿りの重みにする (下の「main を替える規則」) |
| `--single-min-weight <W>` | `0` | `--walk-table` の重みがこれ未満の局面は行にしない。`--walk-table` が要る |
| `--single-priority-band <W>=<P>` | なし | 繰り返し可。`--walk-table` の重みが W 以上の行の priority を P (0〜1000000) にする。複数の帯に当たる行は W が一番大きい帯を使い、どの帯にも当たらない行は `--single-priority` と順位のまま。`--walk-table` が要る |
| `--deep-plies <N>` | なし | `--switch-rule line` だけ。差が正で `--line` 未満でも、main と 1 位の手を指した後から、元データの main が両方とも N 手以上続けば表・置換の対象にする。N は 1 以上。直接のキーが無ければ反転キーでも続きを辿り、同じ局面に戻ったら止める |
| `--deep-opp-freq <PATH>` | なし | 相手のよくある応手も継続判定に含める頻度定跡 (YaneuraOu 形式 .db) |
| `--deep-min-games <N>` | `3` | 継続判定で辿る相手の手の count (局数) の下限 |
| `--draw-value-black <CP>` | `-70` | 先手を自分として解くときの千日手の値。対局の `DrawValueBlack` に揃え、明示を勧める |
| `--draw-value-white <CP>` | `70` | 後手を自分として解くときの千日手の値。対局の `DrawValueWhite` に揃え、明示を勧める |
| `--out-db <PATH>` | なし | 値の分かる手だけを載せた、伝播後の値つき `.db` |
| `--fill-replies` | なし | 元データに無い相手の手番の局面を補って伝播する (下の「伝播の仕方」)。`--apply`・`--demote-alts`・`--revive-bench` とは併用不可 (確認だけの場合も含む) |
| `--bridge-replies` | なし | `--fill-replies` と違い、読みの値のある手の行き先を補い、その手の読みの値を残して伝播する (下の「伝播の仕方」)。`--apply`・`--demote-alts`・`--revive-bench` と併用でき、点検・main の置換・降格・復帰を補った伝播の値で決める。読みの値の無い手の行き先は補わない。`--fill-replies` とは併用不可 |
| `--bridge-table <PATH>` | なし | `--bridge-replies` で、main の値が補わないときより `--line` 以上悪くなった局面の表 (TSV) |
| `--bridge-enqueue-out <PATH>` | なし | bridge の各局面を根にし、手番側を自分として main を必ず伸ばす `tree enqueue --from` 用 JSON Lines。`--bridge-replies` が必要。`--bridge-table` は省略可 |
| `--bridge-depth <N>` / `--bridge-max <N>` / `--bridge-priority <N>` | `6` / 制限なし / `100` | 木の深さ (0〜20)、行数上限、priority の基準。表の順から既存キューで覆われた局面を除き、残った順位 (0 始まり、99 で頭打ち) を priority に足す |
| `--bridge-opp-n <N>` / `--bridge-stop <CP>` / `--bridge-rounds <N>` | なし | 木の応手数 (1〜8)、打ち切り値 (1〜30000)、再読の回数 (0〜10) を行の opts に指定 |
| `--bridge-draw-black <CP>` / `--bridge-draw-white <CP>` | なし | 木の先手番・後手番の循環の値 (-30000〜30000、それぞれの手番側から) を opts に指定 |
| `--bridge-exclude-queue <PATH>` / `--bridge-exclude-min-depth <N>` | なし / `6` | 繰り返し可。指定順に読み id ごとの最後の行を採用。深さが下限以上、own が局面の手番側を含む active な行の root・roots と同じ局面をキューから除く。表には残し、hold だけで覆われた局面は除かない |
| `--opp-records` | なし | 相手の手番の record のうち、evals に読みの値がある手も伝播に使う。自分の手番の record は除外し、反転した局面へ辺を張らない。role は変更しない。元データに record だけを持つ相手の手番の局面には、着く手の読みの値のうち手を指した側に一番悪い値を、record に無く、行き先が伝播の局面に無く、行き先からどの応手も伝播の局面に着かない (`--bridge-replies` に補われない) 合法手 1 つ (生成順の最初) の値として置くので、手の値は読みの値と record の先の値の悪い方になる。置ける手が無い局面の record は伝播に使わない (読みの値がそのまま効く)。`--bridge-replies` の有無によらない |
| `--opp-bench` | なし | 相手の手番の bench (控え) のうち、evals に読みの値がある手も伝播に使う。規則は `--opp-records` の record と同じ (bench だけを持つ相手の手番の局面の扱いも同じ) で、`--opp-records` と併用できる。role は変更しない |
| `--refutation-table <PATH>` | なし | 反駁の表 (TSV) の出力先 (下の「反駁の点検」) |
| `--refutation-margin <CP>` | `1` | 反駁の線。伸ばしていない手の最善が伸ばした手の最善をこの値以上上回る局面を出す (1 以上) |
| `--enqueue-out <PATH>` | なし | 反駁の局面を読み足す、`book_mine tree enqueue --from` に渡す行 (JSON Lines) の出力先 |
| `--refutation-depth <N>` | `2` | キューの行の `depth` (0〜20) |
| `--refutation-max <N>` | なし | キューの行の数の上限 (表の順で先から) |
| `--priority <N>` | `100` | キューの行の priority の基準 (`borrow cut` の `--priority` と同じ意味)。表の順位を足す |
| `--opp-freq <PATH>` / `--min-games <N>` / `--max-ply <N>` / `--weight-floor <W>` / `--min-weight <W>` | なし / `3` / `80` / `1e-5` / `0` | 反駁・bridge の出力の重み。`master rejoin` と同じ意味 (`--min-weight` は表・キューから落とす下限)。`--opp-freq` は反駁・bridge・`--single-enqueue-out` の出力と一緒に使う |
| `--opp-book <PATH>` / `--opp-book-diff <CP>` / `--opp-book-best <P>` / `--opp-book-first` | なし / `30` / `0.9` / 無効 | 重みの辿りで、`--opp-freq` の局数が足りない局面 (と局数の少ない手に行く分) を大きい定跡の手で指す。`master rejoin` と同じ意味 |
| `--threads <N>` | `0` | スレッド数 (0 = 全コア)。結果はスレッド数によらない |

bridge の表とキューは、重みがあれば手番側を自分とした到達重みの大きい順、同じなら main の悪化量の大きい順、さらに同じなら key 順です。重みが無ければ悪化量、key の順です。表の末尾の `weight` 列は、重みを指定しない場合は空欄です。除外キューとの出力先の衝突も拒否します。キュー出力時は「補った切れ目のキューの行: N (除外 M)」を表示します。

`--out-db` は同値の -1 補正後の値を書きます。手番側の値を下げるので、先手から見た値では後手の手が 1 高くなることがあります。点検の表と main の置換判定は、補正を戻した値で main と 1 位を比べ、同じ値なら main を保ちます (補正は出口まで短い手を残すので、そのまま比べると同値の局面で main が替わり、木ごとに main が行き来します)。`--apply` は、点検の表に入った局面のうち、現在の main の source が置換可能、かつ 1 位の値の由来が `propagated` の局面だけを替えます。1 回の読みだけの `eval` は替えません。curated の行が 1 行でもある局面は、bench・record も含めて、`--replace-curated` が無ければ替えません。既存の alt を main にして元の main の tier を引き継ぎ、元の main は alt・wide にします。両方の note にタグと変更理由を追記し、curated を外した場合も記します。`evals` とその他の列は変更しません。`--schema` による各行の語彙と読み条件の検査は、`--apply` の有無によらず行います。

`--opp-records` / `--opp-bench` は RAMU の側を固定した別々のグラフで解きます。自分の指す手は main・alt、相手の手は指定した record・bench も含みます。`--out-db` には各局面で手番側を RAMU とした結果だけを載せるため、親の手と、子の局面の DB の最善手は異なる側のグラフの値です。相手の record・bench の値や、その先で効く自分側の指さない手が異なるので、一致するとは限りません。同じ側のグラフ内では、同値補正前の親の手の値は、相手の record・bench を含む子の最善値を符号反転したものです。循環の成分全体を一律の値にはせず、出口と千日手の選択を後退解析で解きます。千日手 -70 / +70 は各自の側から見た値で、どちらも先手からは -70 です。

頻度定跡を省略した場合の `--deep-plies` の継続手数には、最初の main・1 位の手を含めず、その先の元データで role が main の手だけを数えます。伝播後の最善手や `--fill-replies` で補った手は辿りません。指定時は、線未満でも対象にした局面数を先手番・後手番別に要約へ出します。
`--deep-opp-freq` 指定時は、点検する局面の手番側を自分として、自分の手番では main だけ、
相手の手番では main と頻度定跡の count が `--deep-min-games` (既定 3) 以上の全応手を辿り、
最短の継続手数で判定します。最初の手は数えず、`--deep-plies N` の N 手で打ち切ります。
相手の手を指した先に自分の main がなければ相手の 1 手分だけ数え、循環はそこで止めます。
頻度定跡は読み込み時に下限以上の手だけを残し、局面キーと指し手だけを保持します。
直接キーがなければ反転キーを検索します。
元データも直接キーがなければ反転キーを引き、手を元の座標に戻して辿ります。
`--deep-opp-freq` には `--deep-plies`、`--deep-min-games` の明示指定には `--deep-opp-freq` が必要です。
根の手が非合法ならエラーとし、頻度定跡の非合法な応手はその手だけを飛ばします。
使用した頻度定跡のパスを標準出力の要約に出します。省略時の判定・出力は変わりません。

通常の点検と反駁出力 (`--refutation-table` / `--enqueue-out`) は、元データを 1 回読み、record を含む全行の局面キーと手の重複を検査します。record・bench 行は数えて列を破棄し、main・alt 行、重複検査用の局面キーと手、curated の保護に必要な局面キーを保持します (`--opp-records` では record 行、`--opp-bench` では bench 行も保持します)。`--apply`・`--demote-alts`・`--revive-bench` のいずれかを指定した場合は全行を保持します。

`--apply` は開始時に全行を一度読み、key と move の組で更新します。保存前に表の整合性を検査し、ファイルのバイト列を読み直して、開始時と異なれば保存を拒否します。保存は原子的に行い、見出し・列順・行順を保持し、改行は `master cycles --apply` と同じく LF に統一します。master の書換えに共通のロックはなく、バイト比較後から保存までの変更は検出できません。`master add`・`tree register --apply`・`master cycles --apply`・`borrow value --apply`・`master audit --apply` など、master を書き換えるコマンドは一度に 1 つだけ実行してください。

置換する局面の先手番・後手番別の数、うち元の main が curated の数、curated なので替えなかった局面、source が置換対象でない数、1 位が伸ばしていない手なので替えなかった数を出します。対象の手の行が無いか bench・record だった場合も、替えずに数を出します。`--apply` が無ければ「確認だけ (--apply なし)」として同じ判定の件数を出します。点検の表・`--out-db`・反駁の表・キューは、すべて書き換え前の状態に基づく出力です。

main・alt の役割を交換しても、局面の値は変わらないため、main の置換は一度の伝播で判定できます。この説明は役割の交換についてであり、alt を bench にして伝播対象から除く処理には当てはまりません。

`--demote-alts` は main の置換判定後、全局面の alt を次の条件で bench (控え) にします。値は点検で求めた手ごとの伝播値を使い、同値の -1 補正を戻して比較するため、同値の手を差だけで降格しません。

参照する最善値は、由来が `propagated` の手と main (由来を問わない) のうち最大の値です。伸ばしていない alt の 1 回の読みは参照に使いません。参照値が無い局面は降格を見送ります。

- (a) 手番側からの値が `--bad-eval` 以下、かつ参照する最善値より厳密に低い。同値は残し、最善値自体が `--bad-eval` 以下でも、より悪い alt は降格します。
- (b) 参照する最善値との差が `--demote-line` (既定 100) 以上。両方の条件を満たす手は (b) に数えます。

互角の範囲では 40〜70 の差は評価の揺れに収まるため、差による降格は明確に悪い 100 以上を既定にします。`--bad-eval` 以下の手は引き続き条件 (a) で降格します。点検と main の置換には `--line` (既定 40) を使います。

置換後の main と、この実行で main から alt に替えた手は降格せず、record は変更しません。curated の行が 1 行でもある局面は、bench・record も含めて、`--replace-curated` が無ければ降格せず、その局面数を別に出します。値が不明な alt と schema で置換不可の source の降格候補は残し、別々に数えます。tier・evals などは変更せず、note に `<since>: 伝播後の値が悪いので alt から bench (控え) (基準との差 N / 値 V / demote-line D / bad-eval B)` (基準は伸ばした手と main の値の最大) を追記します。降格する alt の数は手番別・理由別に出し、判定に使う demote-line と bad-eval も表示します。`--demote-alts` だけなら元データは保存せず、main の置換候補も考慮した件数を確認できます。`--demote-alts` と `--fill-replies` は、確認だけの場合も併用できません。

`--revive-bench` は、開始時に bench だった手を判定します。控えの手は伝播グラフに加えず、子局面 (直接の局面が無ければ先後反転した局面) への辺を、その手を指す側の千日手の値で解いた後退解析の結果から評価します。先後で千日手の値が違う場合、子局面の最善値を単純に反転した値とは異なります。子局面の値が無ければ、その手の読みの値を使います。値が分かり、`--bad-eval` より大きく、降格と同じ基準の最善値との差が `--demote-line` (既定 100) 未満なら alt に戻します。基準値が不明なら戻しません。schema の source 保護と `--replace-curated` は降格と同じです。`--replace-curated` があっても、その局面に curated の main がない curated の控えは復帰させず、別に件数を出します。note に `<since>: 値が戻ったので bench から alt (基準との差 N / 値 V / demote-line D / bad-eval B)` を追記し、先手番・後手番別の件数を出します。同じ実行で降格した手は復帰させず、復帰した手を再び降格させません。保存には `--apply` が必要です。`master cycles --apply` による循環防止の降格先は引き続き record です。

伝播の仕方:

- 既定では main・alt の手だけを使います。`bench`・`record` (指さない手) は除きます。`--opp-records`・`--opp-bench` を指定したときだけ、下記の規則で相手の record・bench を加えます。
- 手の値は `evals` の平均 (0.5 は偶数の側へ丸める) です。値は手番の側から見た値です。
- 値の分かる手は、読みの値のある合法手と、book 内の子局面 (直接、または先後反転した局面) に値の分かる手がある手です (読みの値の無い手が続く線も、先に読みの値があれば分かる手になります)。分からない手は伝播にも、1 位の候補にも、main との比較にも使いません。
- 伝播は `book_backprop --merge replace --skip-unusable-moves` と同じです。book 内に子局面のある手は子局面からの値、無い手は読みの値になります。
- `--fill-replies` を渡すと、読みの値の無い手 (多数手など) を指した後の局面が元データに無ければ (先後反転した局面も無ければ)、その局面を、相手の合法手のうち元データの局面に着く手だけを持つ相手の手番の局面として補います。元データは自分の手番の局面にしか行を持たないことが多く、補わないと先の局面の読みの値が相手の手番の局面で途切れるためです。補った局面の値は定跡に入っている応手だけで決まるので、定跡に無い応手を見ない分だけ自分の側に甘い値です。この甘さは補った局面の上の局面の最大値を通って、さらに上の局面へ伝わります。読みの値のある手が直接か反転で着く局面は補いません (全ての応手を見た読みの値を直接は置き換えない)。反転した局面が元データにある局面へは応手を張りません (千日手の値が先後で違うと、反転で合流した局面を含む循環を伝播できないため)。補った局面そのものは点検の表・`--out-db` に出しませんが、補った局面を通って付いた値 (上の局面の読みの値のある手に伝わった値も含む) は、表・`--out-db`・反駁の点検 (`--refutation-table`・`--enqueue-out`) に普通の伝播の値として入り、区別されません。補った局面を通って、千日手の値が先後で違う循環に反転で合流すると止まることがあります (「反転キーで合流した…」のエラー)。そのときは `--fill-replies` を外してください。`master rejoin`・`master cycles`・`master hold-early`・`master export`・`borrow`・`tree register` の伝播では補いません。
- `--bridge-replies` を渡すと、`--fill-replies` と違い、読みの値のある手の行き先を補います (読みの値の無い手の行き先は、上限を付けられず甘くなるので補いません)。元データが自分の手番の局面だけを持つ区間 (外部定跡から借りた行など) では、手の値はその手を登録したときの読みの値のままで、先の局面の値が手前に伝わらないためです。補った局面には、着く手の読みの値を、元データの局面にも反転した局面にも着かない合法手 1 つ (生成順の最初) の値として残します。読みの値は相手の全部の応手を見た値で、相手は手を指した側に悪い方を選ぶので、手の値は読みの値と、定跡の応手の先の値の悪い方になり、読みの値より甘くなりません。同じ局面に読みの値のある手が複数の局面から着くときは、手を指した側に一番悪い読みの値を残します (同じ局面に読みの値の無い手からも着けば、その手にも同じ値が効きます)。残す合法手は、補う候補の局面とその反転にも着かないものに限ります (着くと残した値が補った局面の値で置き換わるため)。残す先の合法手が無い局面は補いません。補う伝播では反転した局面へ辺を張りません (直接の子局面が無い手は、反転した局面があっても読みの値を使い、点検・置換・降格・復帰もその値で判定します。読みの値の無い手は値の分からない手になります)。補った辺が反転で合流する局面を千日手の値が先後で違う循環に入れると、1 行で両方の向きの値を表せず止まるためで、これにより千日手の値は対局と同じ (先後で違う値) のまま流せます。要約に、補った局面の数と、そのうち読みの値を残した局面の数を出します。`--apply` と併用すると、点検の表・main の置換・`--demote-alts`・`--revive-bench` は補った伝播の値で決まります (`--revive-bench` の控えの手の値も、直接の子局面が無ければ反転した局面を使わず読みの値で判定します)。`master rejoin`・`master cycles`・`master hold-early`・`master export`・`borrow` の伝播と、`--bridge-replies` を付けない `tree register` の比べる伝播は補わないので、後の登録で main を補わない値で選び直すことがあります (版の手順では登録の後に audit を流すので、最後は補った値で決まります)。
- `--opp-records` を渡すと、先手用と後手用に分けて伝播します。先手用では後手番の record を相手の手として加え、先手番の record は除外します。後手用では先手番の record を加え、後手番の record は除外します。record は `evals` の読みの値 (複数あれば通常と同じ平均) がある手だけを使い、その先に子局面があれば値を伝播します。読みの値の無い record は、子局面があっても使いません。record だけの局面も相手の応手として使えます。点検・main の置換・alt の降格・bench の復帰・各出力の値には、局面の手番に合う伝播を使います。record 自体を main・alt に戻したり、`--out-db` に出したりはしません。`--bridge-replies`・`--deep-plies`・`--deep-opp-freq` などと併用でき、`--bridge-table` の比較元も同じ record の規則で伝播します。千日手の値は既存の規則に従います。`--opp-records` のときは反転した局面へ辺を張らないため、反転先の record や main・alt の値は使いません。要約には、合法かつ読みの値のある record を相手の手として使った手数と局面数を別に表示します (同じ局面の複数の record は一局面)。使った record は「bench・record (除外)」から差し引き、「非合法手」は main・alt の非合法手だけを数えます。循環の件数は先手用・後手用の伝播の合計です。省略時は従来どおり main・alt だけを使い、要約も変わりません。他のサブコマンドの伝播には影響しません。
- `--opp-bench` を渡すと、上の `--opp-records` の record と同じ規則で、相手の手番の局面の bench (控え) のうち読みの値がある手を相手の手として加えます (自分の手番の bench は除外、反転した局面へ辺を張らない、bench だけの相手の手番の局面には着く手の読みの値を残す)。控えは「自分が指さない手」ですが、相手はその手を指せるので、控えの先を深く読んだ値を手前の局面に伝えるための指定です (`tree register --bench-expanded` が、木で先を読んだ差の大きい手を控えで残します)。`--opp-records` と併用でき、要約の見出しは使った役割 (`record`・`bench`・`record・bench`) になり、手数と局面数は両方の合計です。`--revive-bench`・`--demote-alts` の判定もこの伝播の値を使います (控えを伝播に使っても役割は変えず、alt に戻すのは `--revive-bench` の条件を満たした手だけ)。省略時の動きは変わりません。
- `--bridge-table` は、補わないで伝播したときと補ったときの main の値を局面ごとに比べ、補ったときに `--line` 以上悪くなった局面を、`--opp-freq` があれば重みの大きい順 (同じなら悪化量の大きい順、次に key の順)、無ければ悪化量の大きい順 (同じなら key の順) に書きます。列は `key ply side main plain bridged drop best best_value plain_black bridged_black weight` で、`plain`・`bridged`・`best_value` は手番の側から、`plain_black`・`bridged_black` は先手から見た値、`best` は補ったときの 1 位の手です。補わない側の伝播も、補う側と同じく反転した局面へ辺を張らずに解くので、表の差は補ったことによる差だけです。

点検の表は、main が 1 位から線以上負ける局面 (`--switch-rule propagated` では 1 以上負ける全局面) を差の大きい順 (同じ差は key 順) に並べます。値は同値の -1 補正を戻した値で、同じ値なら main を 1 位とみなします。列は `key ply side source main main_value main_from main_eval best best_value best_from best_eval gap main_value_black best_value_black` です。`*_value` は手番の側から見た伝播後の値、`*_from` は値の出どころ (`eval` = 1 回の読みの値、`propagated` = 子局面からの伝播)、`*_eval` は元の読みの値 (無ければ空)、`source` は main の行の source、`*_value_black` は先手から見た値 (プラス = 先手良し) です。

標準出力に要約を出します: 局面数 (手番ごと)、手の数・値の分かる手の数・非合法手・除いた bench・record、読みの値から伝播で変わった手の数と差の分布、読みの値が無く伝播で値が付いた手の数、main・alt が 2 手以上の局面数、main が 1 位から線未満・線以上負ける局面数、main の値が分からない局面数、補った相手の手番の局面の数、循環 (自明でない強連結成分) の数。

### main を替える規則 (`--switch-rule`)

`--switch-rule line` (既定) は、main が 1 位から `--line` 以上負ける局面 (`--deep-plies` では両方の先が続く線未満の局面も) を表に出し、1 位の値が子局面からの伝播 (`best_from=propagated`) なら main を替えます。

`--switch-rule propagated` は線を使わず、値の出どころで決めます。main が 1 位から 1 でも負ける局面を全部表に出し、main と 1 位の値がどちらも手の先の局面の値から来ていれば main を 1 位に替えます (同じ値なら main を残すのは `line` と同じです)。どちらかの値が 1 回の読みなら替えません。木を根から読み直さないと 1 回の読みと深い伝播の値は比べられないので、`--single-enqueue-out` でその局面を根にして 1 回の読みの手 (両方なら両方) を `game` に入れ、局面の手番の側を `own` にした木の行を出します。`--bridge-replies` で補った相手の手番の局面と `--opp-records` の record だけの局面では、残した着く手の読みの値が最善なら手の値は読みそのものなので、表の `*_from` も `eval` にして 1 回の読みとして扱います (record の読みが最善なら先の局面の値です)。要約の置換の行は、1 位と main のどちらの値が 1 回の読みだったかで分けて数えます。並びは `--opp-freq` があれば重みの大きい順 (`--min-weight` 未満は除く)、無ければ差の大きい順です。`--deep-plies` とは併用できません。 20 手目より前の局面の替わりは、続く `master hold-early` が元の main との比較で戻すので、audit では分けません。

`--walk-table` を渡すと、`--single-enqueue-out` の行の重みは `--opp-freq` の重みの代わりに辿りの重みになります (下限も `--min-weight` でなく `--single-min-weight`)。局面の辿りの重みは、walk の表の行のうち `side` が局面の手番の側と同じ行 (その局面で手を選ぶ側を自分とした辿り) について、`moves` の手順がその局面を通るか、その局面で止まる行の `weight` の和です。手順が同じ局面に戻る行でも 1 行で 1 回だけ数えます。相手の側を自分とした辿りがその局面を通る重みは数えません。walk の表は同じ手数・同じ局面への合流を最初の手順の行にまとめるので、合流の手前の局面の重みは、まとめられた手順の分だけ小さくなります。`master audit` の `--opp-freq`・`--opp-book` の重みは元データを辿り直して求めるので合流でも欠けません。walk の表は、版の辿りと同じ相手の手の選び方の重みをそのまま使いたいときと、`--opp-freq` を受けない `tree register` で使います。`moves` は初期局面からの手順として読みます (`master walk --start-moves` の表も開始手順を含みます)。複数の表の重みは足すので、同じ側の重なる表 (全体の walk と、その一部を辿り直した `--start-moves` の walk) を両方は渡しません。行は重みの大きい順 (同じ重みは差の大きい順、次に key の順) に並べ、memo に `weight=` を書きます。辿りが通らない局面は重み 0 です。`--single-min-weight` で除いた局面の数と残りの数を標準出力に出します。`--single-priority-band` は重みの帯ごとに priority を決めます。たとえば `--single-priority-band 0.05=1 --single-priority-band 0=7` は重み 0.05 以上を 1、ほかを 7 にします

### 反駁の点検

読みの MultiPV の 2 位以下の値は 1 位より甘く出やすく、伝播で 1 位の手の先が掘られて値が下がると、先を伸ばしていない甘い手が伝播後の 1 位に繰り上がります。`--refutation-table` か `--enqueue-out` を渡すと、次の局面を出します。どちらも渡さなければ、上の表・要約・`--out-db` の出力は変わりません。

- 伸ばした手 (`*_from` が `propagated`、book 内の子局面から値が来る手) と伸ばしていない手 (`eval`、1 回の読みの値) の両方に値の分かる手がある局面で、伸ばしていない手の最善 `best` が伸ばした手の最善 `expanded_best` を `--refutation-margin` 以上上回る局面 (同じ値なら元データの順で先の手を最善とします)。
- 値は同値の -1 補正の前の値で比べます。伝播の書き出し (`--out-db` と `book_backprop`) は 1 位と同値で選ばなかった手を -1 するので、補正後の値で比べると同値の手が margin 1 で当たるためです。そのため反駁の表と点検の表の値は、`--out-db` の値と 1 違うことがあります。
- 伝播前の順位は深さの違う読みが混ざるので条件にしません。伝播前の読みの値に `best` の読みの値より大きい手があれば `swapped` を 1 にします (同値なら入れ替わっていないとみなします)。`main_is_best` は main が `best` なら 1 です。

反駁の表の列は `key ply side main main_value main_from best best_value best_eval expanded_best expanded_best_value expanded_best_eval margin swapped main_is_best weight best_value_black expanded_best_value_black source best_source` です。`margin = best_value - expanded_best_value`、値は手番の側から、`*_black` は先手から見た値 (プラス = 先手良し)、`source` は main の行の source、`best_source` は `best` の行の source です。main の値が分からなければ `main_value`・`main_from` だけ空にします (`main` には手を書きます)。重み `weight` は `--opp-freq` を渡したときだけ埋まり、局面の手番の側を自分として `master rejoin` と同じ規則で初期局面から辿った重みです (main だけで辿るので、alt からしか届かない局面は 0)。重みがあれば重みの大きい順、無ければ `margin` の大きい順 (同じなら key 順) に並べ、`--min-weight` 未満の局面を除きます。

`--enqueue-out` は表の順で 1 局面 1 行、`book_mine tree enqueue --from` にそのまま渡せる行を書きます (書く前に enqueue と同じ検査を通します。キューには触りません)。

- `root` は `best` を指した後の局面 (相手の手番、手数は元データの `ply` + 1)、`own` は `best` を指す側 (`key` の手番、定跡の自分の側)、`depth` は `--refutation-depth` です。相手の手番の根では木が相手の応手を `opp_n` 手まで伸ばすので、`best` が相手の応手に耐えるかを読めます。
- `id` は `<key と best の SHA-256 の先頭 16 桁>_refut_<b|w>` です。同じ局面・同じ `best` を足し直すと同じ id (キューは同じ id の最後の行が有効)、`best` が変われば別の id になります。
- `priority` は `--priority + min(表の順位, 99)` です (表の順位は 0 始まりで、重みがあれば重みの順、無ければ `margin` の順)。小さいほど先です。
- `memo` は `refutation margin=<margin> best=<best> expanded_best=<expanded_best>` です。

標準出力の要約の後に、値の分かる手が 2 手以上の局面数、伸ばした手と伸ばしていない手の両方がある局面数、当たる局面数 (`swapped`・`main_is_best` の数)、`margin` の分布を出します。重みがあれば表の行数と重みの下限で除いた数、`--enqueue-out` があればキューの行数も出します。出力先が元データ・`--opp-freq`・他の出力先と同じファイルなら止まります。

注意:

- 点検の表は読み直しの候補です。木で伝播した値と 1 回の読みの値を比べるので、深さの違う値が混ざります。main を自動では替えません。
- 相手の手も定跡の手だけで見るので、定跡の外の相手の手で同じ局面に戻る循環 (先手の千日手) と、定跡の外の相手の手で値の低い局面に合流する所は、この伝播では見つかりません。それぞれ `master cycles` と `master rejoin` で調べます。
- 元データは全件を読み、局面数と手の数に比例するメモリを使います。伝播用のグラフは一時 `.db` を介さずメモリ内で1回だけ構築します。key の検査・索引の作成には読み取り専用の軽い局面を使い、利きの全升集計・NNUE の駒表・駒割の計算を省きます。手を指す経路では完全な `Position` を使います。頂点の展開では詰めた局面から完全な局面を直接構築し、在庫検査とハッシュ計算を1回だけ行います。値の分からない手と、手が無くなる局面を同じ配列内で除き、消えた直接の子局面に残る反転局面があれば辺を張り替えます。`rejoin` / `cycles` / `borrow` の元データの値の伝播もこの経路を使います。

## `master rejoin`: 相手の手で定跡に戻ると値が下がる局面

元データは逐次読み込み、`--opp-records`・`--opp-bench` の対象行も全列を保持せず、読みの値がある手の key・ply・move・平均評価値だけを伝播用に集約します。

```bash
book_mine master rejoin --master moves.tsv --table rejoin.tsv \
  --draw-value-black -70 --draw-value-white 70 [--direct-lookup [--direct-lookup-table flip.tsv]] [--line 40] [--side both] [--preset random] \
  [--opp-records] [--opp-bench] [--bridge-replies] \
  [--opp-freq strong_games.db --min-weight 0.001]
```

自分 (RAMU) が定跡の手を指した後、相手が定跡に無い合法手を指して別の定跡の局面に戻るとき、戻った先の値がその手前で期待した値より `--line` 以上下がる所を一覧にします。`master audit` の伝播は相手の手も定跡の手だけで見るので、相手が定跡に無い手で合流すると手前の値が実際より高く見えます。一覧の「掘る局面」で相手の手を読み足すのが対処です。元データは書き換えません。

| オプション | 既定値 | 説明 |
|---|---:|---|
| `--master <PATH>` | 必須 | 定跡の元データの表 |
| `--table <PATH>` | 必須 | 一覧 (TSV) の出力先 |
| `--line <CP>` | `40` | 線。戻った先の値が手前で期待した値からこの値以上下がる所を出す |
| `--side <b\|w\|both>` | `both` | 自分の側 |
| `--preset <default\|random>` | `random` | 自分の手番で辿る手。`default` = main だけ、`random` = main と alt (書き出しのプリセットと同じ) |
| `--draw-value-black <CP>` / `--draw-value-white <CP>` | `-70` / `70` | 値の伝播に使う千日手の値 (`master audit` と同じ) |
| `--direct-lookup` | なし | 対局で定跡を直接のキーだけで引く前提で値を伝播し、反転キーで合流した局面で止めない (`master cycles` の「反転キーで合流した局面で止めない」) |
| `--direct-lookup-table <PATH>` | なし | `--direct-lookup` で確かめなかった局面 (反転した向きの値を保たない・保証が破れる) の一覧 (TSV) の出力先 |
| `--opp-records` / `--opp-bench` | なし | 相手の手番の読みの値のある record / bench を audit と同じ規則で伝播に含め、定跡に無い手として数えない。自分の手番の指す手は増やさない |
| `--bridge-replies` | なし | 元データに無い相手局面を audit と同じ規則で補い、着く手の読みの値を上限として残す |
| `--opp-freq <PATH>` | なし | 相手の手の頻度の定跡。`book_from_csa --no-instant-filter --min-rating 3800 --max-ply 80` などで強い対局から作った `.db` (手の count を局数として使う)。渡すと重みを付ける |
| `--min-games <N>` | `3` | 重み: 相手の局数がこれ未満の局面と手では頻度で指さない (`--opp-book` が無ければそこで止める) |
| `--max-ply <N>` | `80` | 重み: 初期局面からこの手数まで辿る |
| `--weight-floor <W>` | `1e-5` | 重み: これ未満の重みの局面では辿るのを止める |
| `--min-weight <W>` | `0` | 一覧と局面の展開に使う重みの下限。下限未満の自分の手番の局面の先は辿らない (`--opp-freq` を渡したときだけ使う) |
| `--opp-book <PATH>` | なし | 重み: 相手の手の大きい定跡 (`.db` か `.ybb`。`.db` は手数を除いた SFEN の順に並んでいること)。ファイルから引くので丸読みしない。`--opp-freq` が要る。手の選び方は `master walk --opp-book` と同じ (下の「調べ方」) |
| `--opp-book-diff <CP>` / `--opp-book-best <P>` | `30` / `0.9` | 重み: 大きい定跡で指すとき、最善から差以内の手を指し、最善 (同じ値の手は等分) に `P`、残りを差以内の他の手で等分する |
| `--opp-book-first` | 無効 | 重み: 相手の手番では大きい定跡を先に引き、局面があればその手だけで指す |
| `--threads <N>` | `0` | スレッド数 (0 = 全コア)。結果はスレッド数によらない |

点検で `--opp-records --opp-bench --bridge-replies` を使う版は、rejoin にも同じ 3 つを渡してください。千日手の値も揃えます。これらを省略した既定の動作は main・alt だけの伝播です。いずれかを指定すると先後反転への辺を張らず、RAMU の側を固定して期待値と戻った先の値を同じグラフで求めます (`--direct-lookup` は併用できますが効果はなく、`--direct-lookup-table` は併用不可)。record だけの相手局面の上限も audit と共通です。相手の未収録の合法手で戻る組は引き続き全て調べます。audit と違い、rejoin は `--since` による指し手の降格 (不成の行を record にする処理) をしないので、降格される行がある元データでは `--opp-bench` で拾う手の集合が audit と少しずれます。`--bridge-replies` で補っただけの応手は元データの手にはならず、その組の値も比較します。

調べ方:

- 初期局面から、自分の手番では `--preset` の手、相手の手番では合法手 (不成を含む) のうち元データの局面に戻る手を辿ります (先後反転した局面は引きません。対局の `FlippedBook=false` と同じ)。辿れた自分の手番の局面 `key` で手 `move` を指した後の、相手の手番の局面を `dig` とします。`dig` で相手が伝播に入っている手 (値の分かる main・alt、指定時は record・bench も) 以外の合法手 `rejoin_move` を指して元データの局面 `rejoin` に戻る組を見ます。
- 手前で期待した値 `expected` は `move` の伝播後の値、戻った先の値 `rejoin_value` は `rejoin` の main の伝播後の値です。どちらも自分の側から見た値で、伝播は `master audit` と同じです。`expected - rejoin_value` (`drop`) が `--line` 以上の組を出します。値の分からない手は数だけ要約に出します。
- 重み `weight` は `key` に来る重みです。自分は main を指し、相手は `--opp-freq` の局面ごとの手の頻度で指すとして、初期局面から辿ります。
  `--opp-book` があれば、局数が `--min-games` 未満の局面は全部を、局数が足りる局面でも局数の少ない手に行く分を、大きい定跡の手 (`--opp-book-diff`・`--opp-book-best` の割り振り) で指します。
  `--opp-book-first` なら大きい定跡に手のある局面はその手だけで指します。大きい定跡の手は書かれたままの局面で引きます (先後反転では引きません)。同じ手数で同じ局面に来た重みは足し合わせ、局面の重みは手数をまたいで足します。そのため重みは確率ではなく、来る回数の期待値です。自分の手番で元データに無い局面、相手の局数が `--min-games` 未満の局面 (`--opp-book` があれば、大きい定跡にも手が無い局面)、`--weight-floor` 未満の重みで止めます。`--preset random` でも重みは main だけで辿るので、alt からしか届かない局面の重みは 0 になり、`--min-weight` を 0 より大きくすると一覧から落ちます。

`--opp-freq` と正の `--min-weight` を併用すると、自分の手番の局面の重みが下限未満なら、その局面の手の生成・展開を止めます。下限と等しい局面は展開します。一覧は `key` の重みで絞るので、相手の手番の局面は重みによらず展開します (alt の手の先の相手の局面は重みが 0 でも、`key` の重みが下限以上なら一覧に出ます)。これは一覧を最後に絞るだけの場合と結果が異なることがあります。重みを求める経路 (自分は main、相手は対局の頻度) と調査で辿る経路 (自分は preset、相手は元データに戻る全合法手) は異なるため、下限未満の自分の手番の局面の先に、別の経路の寄与で重みが下限以上になる局面があっても、調査の経路がそこを通らなければ一覧に出ません。下限を省く (既定 0) か `--opp-freq` を省くと、従来どおり調査の全経路を辿ります。値の伝播・元データと頻度の定跡の読み込み・索引作成は絞りません。

一覧の列は `side key ply move role expected dig rejoin_move rejoin rejoin_main rejoin_value drop expected_black rejoin_value_black weight dig_games rejoin_games` です。`role` は `move` の役割 (`main` / `alt`)、`*_black` は先手から見た値 (プラス = 先手良し)、`dig_games` は `dig` での相手の局数、`rejoin_games` はそのうち `rejoin_move` の局数です (重みの列は `--opp-freq` を渡したときだけ埋まります)。重みがあれば重みの大きい順、無ければ `drop` の大きい順に並べます。一覧の出力先が元データか `--opp-freq` と同じファイルなら止まります。標準出力には手番ごとに、辿れた自分の手番の局面の数、定跡に無い手で戻る組の数と掘る局面の数、値の分からない手の数、一覧の件数を出します。重みで展開を止めた局面があれば、その数も出します。重みの下限で一覧から除いた件数は、展開した範囲内だけの集計です。

範囲と計算量:

- 範囲は自分の定跡の手で初期局面から届く局面だけです。`--preset default` で狭く、`random` で広くなります。重みで絞るには `--opp-freq` と `--min-weight` を使います。
- 相手の手番の局面ごとに全合法手 (平均 80 前後) について、局面を進めずに行き先のハッシュ値を求め、元データの局面と一致した手だけ局面を進めて、32 バイトに詰めた局面で確かめます。根からの段ごとに局面を並べて展開し、先手と後手も並べて解きます。時間は辿れる相手の手番の局面の数 × 合法手の数と、`master audit` と同じ伝播に比例します。
- 元データと頻度の定跡は全件を読み、局面数に比例するメモリを使います (頻度の定跡は辿らない局面の分も読みます)。辿るグラフは局面を 32 バイトに詰めて持ち (玉の欠けた局面は SFEN で保持します)、key の文字列は出力のときだけ作ります。通常局面では、局面ごとに約 70 バイト、辺ごとに 12 バイトです。強い対局の頻度の定跡から作った 88 万局面の元データ (全局面が初期局面から届く) で、両方の手番・`random` で 4 スレッド約 35 秒・ピーク約 1.4 GB でした (別の計算が並走する 32 論理コアの機械)。

## `master cycles`: 先手の千日手の循環を塞ぐ手

```bash
book_mine master cycles --master moves.tsv --table cycles.tsv \
  --draw-value-black -70 --draw-value-white 70 [--direct-lookup [--direct-lookup-table flip.tsv]] [--side b] [--removable-since 20261028-x18] \
  [--entry-depth 4] [--apply --note "計画の名前"]
```

自分の定跡の手と、相手の合法手で定跡に戻る手でできる循環 (千日手になり得る、同じ局面に戻る道) を見つけ、自分の手番の手だけを外して (指さない手 = `record` にして) 循環を無くす手の組を出します。後手の千日手は許しているので、既定は先手だけです。`--side w` / `both` で後手も出せます。既定は一覧と要約だけで、`--apply` を付けると元データを書き換えます。

| オプション | 既定値 | 説明 |
|---|---:|---|
| `--master <PATH>` | 必須 | 定跡の元データの表 |
| `--table <PATH>` | 必須 | 外す手の一覧 (TSV) の出力先 |
| `--side <b\|w\|both>` | `b` | 循環を塞ぐ側 |
| `--draw-value-black <CP>` / `--draw-value-white <CP>` | `-70` / `70` | 手の値の伝播に使う千日手の値 (`master audit` と同じ) |
| `--direct-lookup` | なし | 対局で定跡を直接のキーだけで引く前提で手の値を伝播し、反転キーで合流した局面で止めない (下の「反転キーで合流した局面で止めない」) |
| `--direct-lookup-table <PATH>` | なし | `--direct-lookup` で確かめなかった局面 (反転した向きの値を保たない・保証が破れる) の一覧 (TSV) の出力先 |
| `--removable-since <TAG>` | なし | 外してよい手を、元データの `since` がこの値の行に限る (複数回指定可)。新しく登録した木の手だけで塞ぎ、前からある手 (丁寧な定跡・上位 AI の手など) を残したいときに使う。省くと全部の手を外してよい |
| `--entry-depth <N>` | `4` | `--removable-since` を渡したとき、外してよい手だけでは塞げない循環が残ると、その循環へ入る手を、外してよい手でなくても (curated の手を除く) 外してよくする範囲 (下の「循環へ入る手を外す」)。循環の局面から自分の手で数えて N 手前まで。0 なら外してよい手だけで塞ぐ |
| `--apply` | なし | 元データを書き換える。外す手を `record` に、main を外した局面の替わりの手を `main` にし、どちらの行も note の末尾に `--note` を付ける。塞げない循環が残るときは書き換えずにエラーで止まる (一覧と要約は出す) |
| `--note <TEXT>` | `千日手の循環を塞ぐため指さない手にした` | 書き換えるときに note に付ける文 |
| `--threads <N>` | `0` | スレッド数 (0 = 全コア)。結果はスレッド数によらない |

反転キーで合流した局面で止めない (`--direct-lookup`):

手の値の伝播は、手の行き先の局面が元データに無く、先後反転した局面 (盤を 180 度回して先後を入れ替えた局面) があれば、その局面の値を反転した向き (手番を入れ替えた向き) で解いて使います。千日手の値が先後で違うと、同じ局面でも向きによって最善の手が変わることがあります。

- **既定の検査が守るもの**: 1 つの局面の行を、反転した局面でも引く定跡 (対局で、局面が定跡に無いとき先後反転した局面の項目を引く設定) の整合です。局面の値の一番高い行が反転した向きでもその向きの値を保つことと、反転した向きで指しても千日手・連続王手の循環で値が破れないことを確かめ、保てなければ「反転キーで合流した N 局面」のエラーで止まります。
- **`--direct-lookup` を付けると**: 対局で定跡を直接のキーだけで引く (反転した局面の項目を引かない) 前提で解きます。反転した局面への手の値は同じく反転した向きで解いた値を使いますが、その手を指した先は定跡の外とみて、上の 2 つで止めません。局面の手の値は保存されている向きの値です。
- **既定と変わり得る値**: 同値の手のうちどれを残してどれを -1 するかは、既定では反転した向きでも値を保つ手を優先して決めますが、`--direct-lookup` では保存されている向きの結果だけで決めます。保証検査で絞る局面も反転した向きを数えません。このため、同値の手の -1 の付き方が既定と違うことがあり、手の値の順で決める `master cycles` の外す手・替わりの main、`master rejoin` の一覧も変わり得ます。同値でない手の値は変わりません。
- **保証されなくなること**: 反転した局面の項目を引く設定でこの元データの定跡を使うと、反転した向きで引かれた行の手が、その向きの最善の手でなかったり、値の約束を破る循環に入ったりし得ます。反転した局面を引く運用では付けないでください。保存されている向きの保証 (直接の子局面だけでできる循環) は `--direct-lookup` でも確かめ、破れればエラーで止まります。
- **値の意味**: 反転した局面への手の値は「その先も両者が定跡の手の中で最善を指す」とみた見積もりです。直接のキーだけで引く対局では、その手の先は定跡の外なので、定跡の手で実現する値ではありません (定跡に無い手の読みの値と同じ扱いです)。

付けたときは、確かめずに済ませた局面を止めずに数え、標準エラーに 1 行で出します: 反転した局面へ着く手の数・その手を持つ局面の数・値の一番高い手がどれも反転した局面へ着く局面の数と、種類別の局面の数 (値の一番高い手のどれかが反転した向きではその向きの値を保たない局面 / 値の一番高い手を反転した向きで指すと千日手・連続王手の循環で値の約束が破れる局面)。`--direct-lookup-table <PATH>` を付けると、その局面の一覧を TSV に書きます。列は `kind` (`value` = 値を保たない、`guarantee` = 保証が破れる)・`key`・`stored` (保存されている向きの値)・`flipped` (反転した向きの値)。値はその向きの手番の側からで、`guarantee` の行は値の列が空です。種類ごとに key 順で、両方に当たる局面は 2 行になります。既定の検査は最初に見つけた局面で止まるので、エラーに出る数とは一致しないことがあります。

`tree register --auto-skip-flip` は既定の検査 (反転した向きも保つこと) で止まる局面を登録から除く機能です。`--direct-lookup` は別のコマンドの指定なので同時に使ってもエラーにはなりませんが、`--direct-lookup` で解くなら除く必要は無く、除いた局面の木の読みが登録されなくなるだけです。直接のキーだけで引く運用では `--auto-skip-flip`・`--skip-keys` を付けずに登録してください。

グラフと循環:

- 初期局面から、自分の手番では main と alt、相手の手番では合法手のうち元データの局面に戻る手を辿ります (`master rejoin` と同じグラフ、先後反転なし)。初期局面から届く自明でない強連結成分 (局面 2 つ以上) を循環として数えます。main と alt の全部で塞ぐので、main だけで指すとき (書き出しの default プリセット) の循環も無くなります。

外す手の選び方:

外す候補を次の 3 通りで作り、それぞれ下の「戻す」「main を替える」を通して、根から届く循環に残る局面の少ない方、同じなら外す手の少ない方、外す main の少ない方 (同じなら A、B、C の順) を採ります。どれを採ったかを要約に出します。`--removable-since` を渡すと、どの外し方も外してよい手だけを外します (外せない手だけでできた循環は、手前の入口も外してよい手で切れないときに塞げない循環として残ります)。

- **A (循環ごと)**: 循環ごとに、その中の自分の手のうち外せる手 (その局面に 2 手以上残る手) を外します。小さい循環では、外した後に循環に残る局面の数の一番少ない手を 1 手ずつ選びます。大きい循環 (元の循環ごとの、候補の手の数 × 循環の中の辺の数の累計が 2,000 万を超えた循環) では、残したい手から先に辿る深さ優先探索で、循環を閉じるたびにその循環の中の手を外します。どちらも同じ条件なら alt を main より先に、値の低い手 (値の分からない手が先) を先に外します。外す手が少なくなりやすい反面、自分の手が 1 手ずつの局面だけでできた循環の手前 (入口) は切らないので、塞げる循環が残ることがあります。
- **B (抜けるまでの段数)**: 定跡を抜ける局面 (行き先の無い局面) から逆に辿り、定跡を抜けられる局面と、抜けるまでの段数を求めます (ゲームのアトラクタ)。自分の手番の局面は抜けられる局面へ行く手が 1 つあれば、相手の手番の局面は全部の手が抜けられる局面へ行くなら抜けられます。`--removable-since` で外せない手のある自分の手番の局面は、その手が必ず残るので、外せない手の行き先が全部抜けられる局面のときだけ抜けられます。抜けられる自分の手番の局面で、抜けられない局面へ行く手と、同じ循環の中で段数の減らない局面へ行く手を外します。根が抜けられる局面なら、各局面に 1 手ずつ残して循環を無くす組がある限り、必ず塞げます。
- **C (最小)**: 循環 (元の強連結成分) ごとに、循環を全部無くし、各局面に手を 1 つ以上残す組のうち、外す手の数が一番少ない組 (同じなら外す main の少ない組) を整数計画で求めます。変数は循環の中の局面へ行く外せる手で、循環ごとに「その循環の外せる手を 1 つ以上外す」制約を、解いた組で残る循環を見つけるたびに足して解き直します (足すのは見つけた循環の最短の道。残る循環が無くなった解がそのまま最小です)。手前の入口を切って根から届かなくする組は考えません (それは B が扱います)。循環の中の外せる手だけでは塞げない循環 (手が 1 手ずつの局面だけでできた循環など) と、上限 (外せる手が 1,000 を超える、足した制約が 2,000 を超える、解き直しの全部で分枝 1 万、解き直し 200 回、解き直しの前の時点でピボットの合計が 30 万以上 (1 回の解く途中では止めない)) に当たった循環は A の組を使います。上限は 1 つの循環の整数計画のメモリ (未探索の分枝ごとに変数と制約の数に比例する) と時間 (解き直すたびに制約が増えて重くなる。乱数で作った密な循環 (変数 188〜811) で上限に当たるまで 6〜13 秒の実測。最大の時間の保証ではありません) を抑えるためです。上限は時間でなく回数なので、結果は機械によりません。要約に、循環ごとの結果 (解けた / 循環の中では塞げない / 上限) の数を出します。

その後の処理 (A・B・C 共通):

- **戻す**: 外した手を main・値の高い順 (値の分からない手は後) に 1 手ずつ戻し、戻しても初期局面から届く循環ができない手は戻したままにします。
- **main を替える**: main を外した局面は残る手のうち値の一番高い手 (合法な手を先に、同じなら元データの順) を main にします。curated の手が残っている局面では curated の手から選びます (curated の行だけで書き出す定跡にも main が 1 つ要るため)。`--apply` は、書き換えた元データが `tree register` と同じ検査 (局面ごとに main がちょうど 1 つ、curated の行だけでも 1 つ) を通るときだけ書きます。

根から、自分がどの手を残しても相手が循環に持ち込める (B で根が抜けられない) ときは、循環が残ります。要約に「塞げない循環」として局面の列を出し、`--apply` でも元データを書き換えずにエラーで止まります。

循環へ入る手を外す (`--entry-depth`):

- 局面は元データから消しません。読まない経路の局面も DB としての値があり、後の読みで変わり得るためです。循環を塞ぐのは手を `record` (指さない手、DB では -30000) にすることだけで行い、循環の中の局面とその行は残します (record にした手も行は残ります)。
- `--removable-since` の手だけでは根から届く循環が残るとき (循環の中の自分の手番の局面が、この版で入った 1 手だけの局面などのとき)、その循環へ入る手を外してよい手に足して解き直します。候補は、B で抜けられない自分の手番の局面 (手が 2 つ以上) の手のうち、抜けられない局面へ行く手で、curated の手を除きます。手が 1 つの局面の手は、外してよくしても抜けられるかが変わらないので候補にしません。候補の段数は、手の行き先から抜けられない循環の局面まで自分の手を何手指して着くか + 1 です (相手の手は数えません)。循環の局面へ相手の手で入る局面の手 (循環の局面の親の局面の手) と、循環の中の手が 1 段、その手前の局面の手が 2 段です。
- 段数 1 から `--entry-depth` まで、その段数までの候補を全部外してよくして B で初期局面が抜けられる最初の段数を探します。手が 1 つしか無い局面はその手を外せないので抜けられないままで、そこへ入る手 (1 段上の手) が次の段の候補になります (上へたどる)。見つかった段数で、候補を全部外してよくしても抜けられない局面の手を除き、残りを残したい順 (段数の深い順、main、値の高い順 (値の分からない手は後)、元データの順) に 1 つずつ外してよい手から戻し、戻すと初期局面が抜けられなくなる手だけを外してよいままにします (どの 1 つを戻しても塞げない組で、最小とは限りません)。外してよくした手を実際に外すかと、main の替えは、上の外し方 (A・B・C、戻す、main を替える) がそのまま決めます。親の局面の main を外すと、残る手のうち値の一番高い手を main にするので、「各局面に main はちょうど 1 つ」は保たれます。
- `--entry-depth` の段数まで外してよくしても塞げなければ、要約に「循環へ入る手を --entry-depth の段数まで外してよくしても塞げない」と「塞げない循環」を出し、`--apply` でも元データを書き換えずにエラーで止まります。段数を増やすか、木の登録 (`tree register --skip-keys`) で循環に効く局面を外してください。
- 外した入る手は、一覧の `entry_depth` 列にその段数が入ります。要約に段数の上限と、外してよくした手の数・そのうち外す手の数 (main) を出します。
- `tree register` は `record` の手を登録し直さないので、次の版で同じ木を登録しても、record にした手がまた指す手に戻ることはありません (別の手や別の木の手が新しい循環を作ることはあり得るので、版ごとに `master cycles` を流します)。

局面ごと消す `--drop-new-positions <PATH>` はありません (指定すると未知のオプションのエラーで止まります)。`--removable-since <TAG> --drop-new-positions dropped.txt --apply` と呼んでいた所は `--drop-new-positions dropped.txt` を外して呼べば、既定の `--entry-depth 4` で入る手を record にして塞ぎます。消した key のファイルはできないので、`skip_keys` への追記も要りません。

一覧の列は `side key ply move role value eval new_main new_main_value entry_depth` です。`role` は外す手の役割、`value` は伝播後の値、`eval` は元データの読みの値、`new_main` / `new_main_value` は main を外した局面の替わりの main とその値、`entry_depth` は循環へ入る手として外したときの段数 (外してよい手なら空) です。標準出力には手番ごとに、外す前の循環の数と局面数 (default・random)、外す手の数 (main / alt)・main を替える局面の数・採った外し方、外した後の循環の数を出します。外した後の数は、同じグラフで外した手を除き main を替えて数え直した値です (自分の手番の辺を減らすだけなので、外した後の元データでグラフを作り直して数えるのと同じです)。一覧の出力先が元データと同じファイルなら止まります。

計算量: グラフは `master rejoin` と同じで、手番ごとに 1 回作り、先手と後手は並べて解きます。B と戻す段の準備は、グラフの大きさに比例します。A は 1 手 (大きい循環では 1 回の探索) ごとにその循環の中だけを分解し直し、厳密な採点の量は元の循環ごとに累計で上限を設けます。深さ優先探索は循環を閉じる辺ごとに探索の経路をなぞるので、長い経路に循環を閉じる辺が多いと循環の大きさの 2 乗程度になり得ます。戻す段は 1 手ごとに、その手の元の循環の中と、新しく根から届く局面を辿ります (戻せた手で届いた局面は以後辿り直しません)。A と C は循環ごとに並べて解きます。C の 1 回の整数計画は、循環の中の外せる手の数の変数と、局面ごとの制約、足した循環の制約でできます。上の 88 万局面の元データ (循環が先手 1,278・後手 1,803) で、両方の手番で 4 スレッド約 20 秒・ピーク約 0.9 GB でした (C を足す前と同じ。同じ機械)。循環へ入る手を外すときは、段数ごとに B の判定を 1 回 (グラフの大きさに比例) し、戻す候補はまとめて試して戻せない範囲だけを半分に分けて判定し (最悪で候補の数の約 2 倍の回数)、外してよい手を足して外し方を 1 回やり直します (元データは読み直しません)。約 21 万行の元データで外してよい手だけでは塞げなかった例 (1 段で塞げた) では、先手だけで入る手を外して解き直しまで約 6 秒・約 0.4 GB でした。元データの行 (書き換え用) は `--apply` のときだけ読みます。

## `master walk`: 自分が読み始める局面と重み

```bash
book_mine master walk --master moves.tsv --table walk.tsv \
  [--opp-freq strong_games.db] [--opp-book big.ybb --opp-book-diff 30 --opp-book-best 0.9 [--opp-book-first]] \
  [--min-weight 0.005 --enqueue-out queue.jsonl --depth 6 --priority 300 --tag x19 --game-from-book] \
  [--start-moves "2g2f"]
```

自分 (RAMU) は元データの main を指し、相手は強い対局の手の頻度か大きい定跡の手で指すとして初期局面から辿り、止まった局面とそこに来る重みを一覧にします。自分の手番で元データに行の無い局面 (自分が定跡を出て読み始める局面) が、木の根の候補です。大きい定跡を渡すと、その局面から大きい定跡の最善を両者が指し続けた手順の長さと終わりの値も出すので、相手が定跡でどこまで進めてくるかが分かります。元データは書き換えません。

| オプション | 既定値 | 説明 |
|---|---:|---|
| `--master <PATH>` | 必須 | 定跡の元データの表 |
| `--table <PATH>` | 必須 | 一覧 (TSV) の出力先 |
| `--side <b\|w\|both>` | `both` | 自分の側 |
| `--opp-freq <PATH>` | なし | 相手の手の頻度の定跡。手の count を局数として使うテキスト `.db` (`book_from_csa --no-instant-filter --min-rating 3800 --max-ply 80` など)。`--opp-book` と同じくファイルから引くので、`sfen` 行が手数を除いた SFEN の順に並んでいること (`book_from_csa` の出力はこの順)。`.ybb` は count を持たないので止まる |
| `--min-games <N>` | `3` | 頻度で指すのは局数がこれ以上の局面で、局数がこれ以上の手だけ |
| `--max-ply <N>` | `80` | 初期局面からこの手数で辿るのを止める (`max`) |
| `--weight-floor <W>` | `1e-5` | これ未満の重みの局面で辿るのを止める (`tiny`) |
| `--min-weight <W>` | `0` | 要約で数える局面とキューの行に書く局面の重みの下限 (同じ局面に別の手数で来た重みは足す) |
| `--opp-book <PATH>` | なし | 相手の手の大きい定跡 (`.db` か `.ybb`。`.db` は `borrow cut` と同じく手数を除いた SFEN の順に並んでいること)。ファイルから引くので丸読みしない |
| `--opp-book-diff <CP>` | `30` | 大きい定跡で指すとき、最善の値からこの差以内の手を指す |
| `--opp-book-best <P>` | `0.9` | 大きい定跡で指すとき、最善 (同じ値の手は等分) の重み。残りを差以内の他の手で等分する (差以内の手が最善だけなら最善に 1) |
| `--opp-book-first` | 無効 | 相手の手番では `--opp-book` を先に引き、局面があればその手だけで指す。定跡に無い局面だけ `--opp-freq` の頻度で指す。相手が自分の定跡を持つときに、その定跡を外れた後に手順の入れ替わりで定跡に戻る線も辿るため。`--opp-book` が要る |
| `--cont-max <N>` | `100` | 大きい定跡の最善を両者が指し続けた手順を辿る手数の上限 |
| `--enqueue-out <PATH>` | なし | 自分が読み始める局面のうち重みが `--min-weight` 以上のものを、`tree enqueue --from` の行 (JSON Lines) に書く |
| `--max-root-ply <N>` | なし | キューに書く根の手数 (SFEN の手数、一覧の `ply + 1`) の上限。同じ局面に別の手数で来た行をまとめた後の最小値で判定。`--enqueue-out` が要る |
| `--exclude-queue <PATH>` | なし | 既存のキュー (JSON Lines) の木と同じ局面・同じ自分の側の根を除く (木の根と、`game` の最初の `depth+1` 手で通る局面)。繰り返して複数。`--enqueue-out` が要る |
| `--opp-n <N>` / `--stop <N>` / `--rounds <N>` | なし | キューの行の `opts` に指定した値だけを書く。範囲は 1〜8 / 1〜30000 / 0〜10。`--enqueue-out` が要る |
| `--draw-black <CP>` / `--draw-white <CP>` | なし | キューの行の `opts` に先手番 / 後手番で循環に入る手の値を書く (各手番側から見た値、-30000〜30000)。指定した項目だけを書き、省くと serve の既定で解決する。`--enqueue-out` が要る |
| `--depth <N>` / `--priority <N>` / `--tag <NAME>` | `6` / `300` / `walk` | キューの行の段数・priority の始まり (重みの順位 0〜99 を足す)・id `<tag>_<根の手数>_<局面の hash 16 桁>_<b\|w>` の名前 (`[A-Za-z0-9_-]` の 1〜40 文字) |
| `--game-from-book` | 無効 | キューの行の `game` に、大きい定跡の最善を両者が指し続けた手順の最初の `depth+1` 手を入れる (木がその手を読みの順位によらず伸ばす)。`--opp-book` が要る |
| `--start-moves <USI>` | なし | 初期局面からこの手順 (空白区切り) を指した局面から、重み 1 で辿り始める。手は元データや定跡に無くてもよい。合法でない手があるか、手数が `--max-ply` を超えるとエラーで終わる。main 以外の作戦 (初手の alt など) の側で読み始める局面を出すのに使う。別の手順の結果を同じキューに積むなら `--tag` を分ける (同じ局面・同じ根の手数は同じ id になる) |
| `--opp-records <PATH>` | なし | 強い対局の 1 局 1 行の JSON Lines (`.gz` 可)。止まりの局面とその 10・20 手前に着いた局数・得点・勝敗の列を一覧の末尾に足す |
| `--records-min-rate <R>` / `--records-max-ply <N>` | `3800` / `130` | `--opp-records` で数える対局の両者のレートの下限と、1 局を見る手数。`--opp-records` が要る |

辿り方:

- 自分の手番では元データの main を指します (対局の default プリセットと同じ)。行が無ければ `own` で止めます。
- `--start-moves` を渡すと、その手順を指した局面から同じ規則で辿ります (その局面が元データに無く自分の手番なら、そこが `own`)。一覧の `ply`・`moves` とキューの行の根の手数・`memo` の手順は、初期局面からの手数・手順 (`--start-moves` を前に含む) です。
- 相手の手番では、`--opp-freq` の局数が `--min-games` 以上の局面なら、局数が `--min-games` 以上の手を局数の割合で指します。局数の少ない手に行った分と、局数の足りない局面は、`--opp-book` があればその手で指します (同じ手は重みを足します)。局数が足りず大きい定跡にも無ければ `out`、局数の少ない手のうち大きい定跡で埋まらなかった分 (局数が足りていて全部の手が少ない局面を含みます) と指せない手の分は `rare` で止めます。重みが 0 になる手 (`--opp-book-best` が 0 か 1 のとき) は辿りません。`--opp-book-first` では順が逆で、大きい定跡に手のある局面はその手だけで指し、無い局面だけ上の頻度の規則で指します (頻度で指した後の局数の少ない手の分は、大きい定跡にも無いので `rare`)。大きい定跡に手はあるがどれも指せない局面は、頻度に戻らずその分を `rare` にします。
- 重みが `--weight-floor` 未満なら `tiny`、`--max-ply` 手に達したら `max` で止めます。同じ手数で同じ局面に来た重みは足し合わせ、手順は最初に辿ったものを残します。止まった重みの合計は手番ごとに 1 です。
- 大きい定跡の手は書かれたままの局面で引きます (先後反転では引きません)。`--opp-freq` だけなら `master rejoin` の重みと同じ辿り方で、相手の局数が足りずに止まった分が `out` と `rare` に出ます。

一覧の列は `side stop ply weight games book_moves cont_plies cont_end_black key moves cont entry_move entry_value_black entry_value_kind entry_source` です。`ply` は初期局面からの手数、`games` は `--opp-freq` のその局面の局数、`book_moves` は大きい定跡のその局面の手の数、`cont` は `own` の局面から大きい定跡の最善を両者が指し続けた手順 (局面が繰り返すか、定跡に無くなるか、定跡の手が指せないか、`--cont-max` 手で止めます)、`cont_plies` はその手数、`cont_end_black` は最後に引けた局面の最善の値を先手から見た値 (プラス = 先手良し) です。`entry_move` は止まった局面に入った最後の手 (USI)、`entry_value_black` は元データのその手の `evals` の先頭を先手から見た値に直したもの、`entry_value_kind` と `entry_source` はその行の `value_kind` と `source` です。どの止まり方にも付け、手が元データに無ければ値・由来・source は空、`evals` が空なら値は空、根で止まって手順が空なら 4 列とも空です。
`--opp-records <PATH>` (強い対局の 1 局 1 行の JSON Lines、拡張子 `.gz` なら gzip。各行は `br`・`wr` (両者のレート)・`res` (`b`・`w`・`d`)・`mv` (手ごとの配列で先頭が USI の手) を持つ) を渡すと、
列 `n0 score0 n10 score10 n20 score20 median_len_after black_wins0 white_wins0 draws0` を末尾に足します。
止まりの局面 (0)・その 10 手前・20 手前の局面のそれぞれについて、着いた対局の数 n と、行の side から見た得点 (勝ち 1・引き分け 0.5) の平均 score (小数 2 桁、0 局なら空) です。
手順が足りず手前の局面が無ければ 0 局です。median_len_after は止まりの局面から終局までの手数の中央値 (偶数個なら大きい方)、black_wins0・white_wins0・draws0 は止まりの局面に着いた対局の勝敗の内訳です。
数えるのは両者のレートが `--records-min-rate` (既定 3800) 以上で `res` が `b`・`w`・`d` の対局で、平手から `--records-max-ply` (既定 130) 手までの局面を局面で照らします (手順違いの合流も数え、1 局で同じ行・同じ手前の局面は 1 回)。
棋譜の途中に指せない手があれば、その対局はそこまでの局面だけを数えます。棋譜は 1 行ずつ読み、行の局面だけを覚えます。既存の 15 列の順と意味は変わりません (`--opp-records` の列はその後ろに足します)。手番ごとに重みの大きい順に並べます。標準出力には手番ごとに、止まり方ごとの重み、読み始める重みの手数の帯ごとの合計、読み始める局面の数 (同じ局面に別の手数で来たものは 1 つ) を出します。

キューの行は、同じ局面に別の手数で来た `own` の行の重みを足して 1 行にし (根の手数は最も小さい手数、`memo` の手順はその手数の手順)、重みの大きい順に priority を付けます。キューは同じ id で根の手数が変わる行を受け付けないので、id は局面と根の手数で決めます。同じ `--tag` で作り直すと、重みが入れ替わっても同じ局面・同じ根の手数は同じ id になります。出力先が入力のどれかと同じか、`--enqueue-out` と `--table` が同じファイルなら (別の書き方のパスでも) 止まります。

`--max-root-ply` と `--exclude-queue` はキューの行だけに適用し、一覧と読み始める局面・重みの要約は変えません。例えば根の手数が 30 未満のものだけを積むには `--max-root-ply 29` を渡します。priority の順位は条件で絞った後に付けます。`opts` は指定した項目だけを書き、全て省けば `opts` 自体を書きません。

`--exclude-queue` は `tree enqueue --from` の入力や serve の `<state>/targets.jsonl` を読めます。指定したファイルの順、各ファイルの行順に読みます。同じ id は serve と同じく最初の行の `root` (局面と手数) と `own` に固定し、それと違う行 (serve が拒否する行) は飛ばします。有効な行の `game` は最初に現れた順で重複なく合わせ、段数・`roots` などそれ以外の欄は最後の有効な行の値を使います (同じ id の前の行で足した手順も除外の対象に入る)。全行を enqueue と同じ関数で検査し、不正な行があれば (後の行で上書きされるものや、使う範囲より先の `game` の手も) エラーで止まります。段数が 1 以上の行について、`root`・`roots` と自分の側の組、および `root` から各 `game` の最初の `depth+1` 手を指して通る自分の手番の局面と側の組を除外対象にします。手数は一致条件に含めず、`own=bw` は両方の側を対象にします。段数 0 の行は除外対象にしません。serve の `targets.jsonl` には serve が拒否した行も残りますが、根と `own` を変えた行は上の規則で飛ばします。それ以外の理由で拒否された行があるキューでは `targets.status.json` の `rejected` を先に確かめてください。改行で終わらない書きかけの行があると、検査で止まります。標準出力には既存の木で除いた行数を側ごとに出し、既存 id ごとの件数も出します。複数の id に一致した行は側ごとの合計では 1 行、id ごとの件数では各 id に数えます。

計算量: 辿る局面の数は重みの下限で決まり、局面ごとに局面を 1 回進めます。頻度の定跡と大きい定跡は、辿った局面ごとに 1 回だけファイルから引き、丸読みしません。メモリは元データと辿った局面の数、および除外キューの id ごとの有効な定義 (同じ id の有効な行の別々の `game` の手順を合わせた分を含む) と除外対象の局面・側・id の組数で決まり、定跡ファイルの大きさによりません。除外キューを使わない場合は、約 7,800 局面の元データ、強い対局 (1 年分) から作った 88 万局面の頻度の定跡、1,900 万局面の `.ybb` で、両方の手番 1 スレッド約 2 秒・ピーク約 45 MB でした。

## `master exits`: 元データを辿って出口を列挙する

```bash
book_mine master exits --master moves.tsv --side b --table exits.tsv \
  --target-black 100 --enqueue-out exits.jsonl --exclude-queue targets.jsonl
book_mine tree enqueue --state tree-state --from exits.jsonl
```

自分 (RAMU) は main だけ、相手は main・alt の全ての手を指すとして幅優先で辿り、main・alt の行が無い局面を出口として 1 局面 1 行にまとめます。record・bench は指しません。それらの行だけがある局面も出口です。元データは書き換えません。

探索の各層と到達済みのキーは一時ファイルに置きます。各層で次の層の候補を局面キー・発見順で外部ソートし、同じキーの最初の入口を残します。到達済みの全キーを整列したファイルと逐次マージして、新しいキーだけを次の層にし、到達済みのファイルも置き換えます。次の層は発見順に外部ソートしてから展開します。到達済みとの照合は 1 層につき到達済みキー数と候補数に比例する逐次 I/O で、候補ごとの鎖の走査は行いません。出口と全ての入口、およびキューの候補は外部ソートし、表は局面キー順、キューは class の指定順 → 最短手数 → 局面キー順に逐次書き込みます。出力先と同じディレクトリの一時ファイルから rename して置き換えます。

探索と整列のメモリは元データ全体・到達局面数・出口数・入口総数には比例しません。元データの固定数の索引先頭位置、ソートごとに最大 1 MiB または 2,048 行の塊 (先に達した方。文字列・配列の管理領域と最大 1 行分を別途使用)、固定数のマージ用バッファ、および 1 局面分の元データ行を保持します。同じ出口への入口も全件をメモリに集めません。一時ディスクの使用量は元データと辿った局面・辺・出力の量に比例し、一時ファイルの置き場は OS の設定 (`TMPDIR` など) に従います。`--exclude-queue` は例外で、id ごとの最終行と照合する根の集合をメモリに保持するため、受付行とそこに含まれる根の量に比例しますが、元データの大きさには比例しません。

同じ局面 (手数を除いた SFEN) は最初に到達したときだけ展開します。一覧の手数と代表の入口は最短の経路のもので、同じ手数の経路が複数あれば幅優先で最初に見つけた入口 (親の局面に着いた順、同じ親では元データの行順) を使います。後から同じ出口に入る手も `entries` に集めます。先後反転では照合しません。自分の手番に alt だけがある局面では指す手がないため止まりますが、出口には数えません。

| オプション | 既定値 | 説明 |
|---|---:|---|
| `--master <PATH>` | 必須 | 定跡の元データ (13 列 TSV) |
| `--schema <DIR>` | なし | 語彙・読み条件の表。指定時は既存の master の道具と同じ検査を行う |
| `--table <PATH>` | 必須 | 出口一覧 (TSV) の出力先 |
| `--side <b\|w>` | 必須 | RAMU の側 |
| `--max-ply <N>` | `200` | 初期局面からの手数の上限。上限ちょうどの出口も出すが、そこから先は辿らない。上限で止まっただけの局面は出口にしない |
| `--start-moves <USI>` | なし | 初期局面から空白区切りの手順を指した局面から開始する。手は元データに無くてもよい。手数はこの手順を含み、不合法な手や上限を超える手順はエラー |
| `--target-black <CP>` | なし | RAMU が先手なら、入口の先手視点の値がこれ未満の出口を `short` にする |
| `--target-white <CP>` | なし | RAMU が後手なら、入口の先手視点の値がこれ以上の出口を `short` にする |
| `--shallow-ply <N>` | `20` | `novalue-shallow`・`weak-shallow` にする手数の上限 (この手数を含む) |
| `--enqueue-out <PATH>` | なし | 選んだ出口を `tree enqueue --from` の JSON Lines に書く |
| `--enqueue-class <CLASSES>` | `short,novalue-shallow,weak-shallow` | キューにする class をカンマ区切りで指定。重複不可。`other` も指定でき、指定順を優先する |
| `--priority <N>` | `10` | キューの priority の始まり。絞り込み後の順位を 0 から 1 ずつ足す。1000000 を超えるとエラー |
| `--tag <NAME>` | `exits` | id `<tag>_<根の手数>_<局面の hash 16 桁>_<b\|w>` の名前 (`[A-Za-z0-9_-]` の 1〜40 文字) |
| `--depth <N>` | `6` | キューの行の段数 (0〜20) |
| `--opp-n <N>` / `--stop <N>` / `--rounds <N>` | なし | `master walk` と同じ opts。範囲は 1〜8 / 1〜30000 / 0〜10。指定した項目だけを書く |
| `--draw-black <CP>` / `--draw-white <CP>` | なし | `master walk` と同じ opts。先手番 / 後手番で循環に入る手の値 (各手番側から見た値、-30000〜30000)。省略時は serve の既定で解決する |
| `--exclude-queue <PATH>` | なし、繰り返し可 | 既存キューを受付順に読み、id ごとの最後の行を使う。根と own が同じ木を照合する。一覧だけ出す場合も使える |
| `--exclude-min-depth <N>` | `6` | 照合する木の段数の下限。この段数未満の行は hold の照合にも使わない |
| `--skip-covered-hold` | 無効 | `covered_hold` が空でない出口もキューから除く |

class は次の上から最初に当たるものです。

| class | 条件 |
|---|---|
| `short` | 自分の側の目標を指定していて、入口の値が目標に届かない (先手は `< target-black`、後手は `>= target-white`) |
| `novalue-shallow` | 入口の値が無く、`ply <= shallow-ply` |
| `weak-shallow` | `ply <= shallow-ply` で、入口の value_kind が `single`・`unknown`・空・`mean:` で始まるもの・`tree:N` の N が 6 未満 |
| `other` | 上のいずれにも当たらない |

一覧の列:

| 列 | 内容 |
|---|---|
| `side` | RAMU の側 (`b` / `w`) |
| `ply` | 初期局面から出口までの最短の手数 (開始手順を含む) |
| `key` | 出口の手数を除いた SFEN |
| `entry_key` | 代表の入口の、入る直前の局面 (手数を除いた SFEN) |
| `entry_move` | 代表の入口の手 (USI) |
| `entry_value_black` | 入る手の行の evals の平均を先手視点に直した値。既存の平均計算と同じく整数に丸め、ちょうど半分なら偶数側へ丸める。入る手を指した側が後手なら符号を反転する。値が無ければ空 |
| `entry_value_kind` / `entry_source` | 入る手の行の value_kind / source |
| `entries` | 辿った全ての入口の `move:value_black` をカンマ区切り。同じ entry_key と move は 1 つにまとめ、entry_key → move 順。値が無ければ `move:` |
| `class` | 上の分類 |
| `covered_by` | 段数の下限以上で hold でない木の id (複数なら id 順のカンマ区切り) |
| `covered_hold` | 段数の下限以上で同じ根・同じ own の木があり、全て hold のときの id (同じくカンマ区切り)。`covered_by` は空のまま |

開始局面自体が出口で入口が無ければ、入口の欄は空です。`--start-moves` の最後の手が元データにある場合はその行を入口に使い、無ければ entry_key・entry_move だけを記し、値・種類・source は空にします。

既存キューの全行を enqueue と同じ関数で検査してから使います。同じ id の行はファイル内・ファイル間を通して最後の行で置き換えます。`root` と `roots` の局面と `own` で照合し、手数は照合に使いません。`own=bw` は両側に当たります。`game` の途中の局面は除外に使いません。hold だけの木は既定では再度読む対象に残します。

キューは `--enqueue-class` の指定順 → ply の浅い順 → key 順に並べ、`covered_by` が空の出口だけを書きます。根の手数は `ply + 1`、own は `--side` です。memo に class・entry_value_black・entry_value_kind・entry_move を書きます。標準出力には側ごとの辿った局面数、出口数、class ごとの数、除外数、hold の出口数、キューの行数を出します。`--skip-covered-hold` を指定した場合は hold の出口も除外数に含めます。

出力はそれぞれ原子的に書きます。出力先が master・schema・除外キューの入力と同じ場合や、一覧とキューの出力先が同じ場合はエラーで止まります。元データは既存のディスク索引で必要な局面の行だけを読み、丸読みしません。到達済みの局面キー、各層の展開待ちの局面と入口、出口と全ての入口、およびキュー候補は一時ファイルに置きます。メモリに入力件数に応じて残るのは、除外キューの id ごとの最後の定義と、根・own から id へ照合する集合で、受付行とそこに含まれる根の量に比例します。それ以外の作業メモリは固定上限のソート・マージ用バッファ、元データの固定数の索引先頭位置、および 1 局面分の元データ行です。

## `master main-line`: main 対 main で定跡が切れる所

初期局面から、手番の側の main (先手の局面では先手の main、後手の局面では後手の main) を交互に指して辿り、
元データの定跡が切れる局面と理由を出します。局面は直接キーで引きます。対局で FlippedBook を使わない場合、
反転キーでだけ元データにある局面ではそこで定跡が切れるので、切れ目として扱います。

```sh
book_mine master main-line --master moves.tsv --table main-line.tsv \
  --enqueue-out main-line.jsonl --tag mainline --depth 6 --priority 10
```

| 引数 | 既定 | 内容 |
| --- | --- | --- |
| `--master <TSV>` | 必須 | 元データ (`moves.tsv`) |
| `--schema <DIR>` | なし | 語彙の表。行の検査に使う |
| `--table <TSV>` | なし | 辿った手順の表 |
| `--max-ply <N>` | 400 | 辿る手数の上限 |
| `--enqueue-out <JSONL>` | なし | 切れた局面を根にした own=bw の木の行 (`tree enqueue --from` の形) |
| `--tag` / `--depth` / `--priority` | mainline / 6 / 10 | 木の行の id の先頭 (`[A-Za-z0-9_-]` の 1〜40 文字)・段数・priority |
| `--opp-n` / `--stop` / `--rounds` / `--draw-black` / `--draw-white` | なし | 木の行の `opts` |

切れる理由 (reason) は次のとおりです。

| reason | 内容 | 木の行 |
| --- | --- | --- |
| `missing` | 局面が元データに無い (反転キーでも無い) | 出す |
| `flipped-only` | 反転キーでだけ元データにある | 出す |
| `no-main` | 局面はあるが main の行が無い (標準出力に、ある role を出す) | 出す |
| `cycle` | 同じ局面 (手数を除く) に戻った | 出さない |
| `max-ply` | `--max-ply` 手まで辿った | 出さない |

標準出力には、切れた手数・理由・次の手番、初期局面からの手順 (`moves=`)、切れた局面 (`sfen <局面> <手数>`) を出します。
表は `ply key move value_black reason` の列を持ちます。ply はその局面までに指した手数 (初期局面が 0) で、辿った手ごとに 1 行 (value_black は main の行の読みの値の平均を先手から見た値、無ければ空) で、
最後に切れた局面の行 (move・value_black が空、reason が理由) を置きます。
木の行は、根が切れた局面 (手数は初期局面からの手数 + 1)、id が `<tag>_<根の手数>_<局面の sha256 の先頭 16 桁>_bw`、
memo が `main-line reason=<reason> moves=<手順>` です。木の行を出さない理由のときは、空の JSONL を書きます。
元データはディスクのバケットに索引化して、辿る局面の行だけを読みます。辿った局面に main が 2 行以上あればエラーにします。

## `master exit-report`: 版ごとの出口の報告

`master walk` の止まりと、伝播の切れ目 (`master audit --bridge-replies`)・定跡に戻る手 (`master rejoin`) の表を合わせて、
出口を有望・塞ぎたい・浅い・値の出所が弱いに分けた表と Markdown の報告を出します。

```sh
book_mine master exit-report --master moves.tsv --walk walk.tsv \
  --rejoin rejoin.tsv --bridge bridge.tsv --bridge-audit bridge_audit.tsv \
  --box box-a=/path/to/box-a --title v59 --table exits.tsv --markdown exits.md
```

| 引数 | 既定 | 内容 |
| --- | --- | --- |
| `--master <TSV>` | 必須 | 元データ。RAMU の最後の手の値と出所を引く |
| `--schema <DIR>` | なし | 語彙の表 |
| `--walk <TSV>` | 必須 | `master walk --table` の表 (先手・後手の両方を含めてよい) |
| `--rejoin <TSV>` | なし | `master rejoin --table` の表 |
| `--bridge <TSV>` / `--bridge-audit <TSV>` | なし | 同じ `master audit --bridge-replies` の `--bridge-table` と `--table`。両方で指定 |
| `--box <NAME>=<DIR>` | なし、繰り返し可 | 受付と進みの写し。`tree queue-review --box` と同じ形・同じ検査 |
| `--min-weight <W>` | 0.008 | walk の止まりを載せる重みの下限 |
| `--gap-min-weight <W>` | 0.001 | 伝播の切れ目と rejoin を載せる重み (来る割合) の下限 |
| `--gap-line <N>` | 40 | main が 1 位から負ける差がこれ以上なら、補いで main が替わる局面とみなす |
| `--target-black <cp>` / `--target-white <cp>` | 50 / 80 | 出口の目標。先手は先手から見た値がこれ以上、後手はこれ未満で届く |
| `--table <TSV>` / `--markdown <MD>` | どちらか必須 | 全部の行の表と報告 |
| `--title <NAME>` | 空 | 報告の見出しの版の名前 |

walk の止まり (stop が own・out・rare・max、重み `--min-weight` 以上) は、手順の中で RAMU (表の side) が最後に指した手を元データで引き、
その手の evals の平均 (先手から見た値) と value_kind で分けます。

| class | 条件 |
| --- | --- |
| `good` | stop が own 以外で、value_kind が `tree` で始まり目標に届く |
| `bad` | stop が own 以外で、value_kind が `tree` で始まり目標に届かない (値が無いものを含む) |
| `shallow` | stop が own (相手のよくある応手が定跡に無い)。値は直前の RAMU の手の値で、出口の値ではない |
| `weak` | stop が own 以外で、value_kind が `tree` で始まらない |
| `gap` | bridge の表の局面のうち、walk の重みが `--gap-min-weight` 以上で、補った値 (bridged_black) が目標に届かないか、audit の gap が `--gap-line` 以上 |
| `rejoin` | rejoin の表の行のうち、戻った先の値が目標に届かず、来る割合 (weight × rejoin_games / dig_games) が `--gap-min-weight` 以上 |

gap は RAMU の側 (walk の side) が局面の手番と一致する組だけを判定します。bridge の値は、その局面の手番側を RAMU としたグラフの値だからです。重みは、その側の行のうち、その局面を通るか止まりの局面とする行の重みの和 (1 行で同じ局面は 1 回) で、手順もその側の最初の行から取ります。手番でない側の辿りの重みや経路は使いません。目標と audit の main と 1 位の差は、どちらも局面の手番側で判定します。audit の表は、重みの下限以上で gap が `--gap-line` 以上の局面の行だけを持ちます。
rejoin の表は手順を持たないので、手順は walk の表でその局面を通る行 (RAMU の側の行を優先、同じ側の中では最初の行) から取ります。walk が通らない局面は手順なしです。

局面の番号は、表でも報告でも全部の class で同じ数え方です: 何手目 (`move_number`) = その局面で次に指す手の番号 (初期局面の手が 1)。先手番の局面は奇数、後手番の局面は偶数です。
局面は、walk の止まりでは止まった局面、rejoin では RAMU が指す局面、gap では bridge の局面 (RAMU の手番) です。止まった局面の手番は、own が RAMU、out・rare が相手で、max は `master walk --max-ply` の偶奇によります。
手順のある行は「手順の手数 + 1」を何手目にします (同じ局面に別の手数で着く手順があるので、rejoin の表の ply とは同じ偶奇で違う数になりえます)。手順のない rejoin の行は表の ply です。
bridge・rejoin の表の ply は空でもよく (元データの ply が空の局面)、手順があれば手順から数えます。手順も ply も無い rejoin の行は何手目が不明で、表の `move_number` は `-`、報告は `後手 ? 手目` と手番だけ書きます。
手番と何手目の偶奇が合わない行、何手目が 0 の行、手順の手数と合わない行、walk の ply が手順の手数と違う行、bridge・rejoin の空でない ply が 0 か、偶奇が局面の手番と合わない行があれば、何も書かずにエラーで止まります。

表は `class side turn move_number weight value_black kind target queued key moves extra` の列を持ち、class の順に重みの大きい順 (同じ重みは入力の順) です。
side は RAMU の側、turn は key の局面の手番 (b / w)、move_number は上の何手目 (不明なら `-`) です。moves は初期局面から key の局面までの手順 (USI) で、手順なしの行は `-` (空は初期局面) です。
queued は同じ根・同じ側の段 1 以上の受付の木 (`id (台, 状態)`、状態は DONE・failed・hold・read/requested、bw の木は両方の側) で、無ければ「なし」。rejoin の行は、局面か戻った先の局面を根にした木です。
extra は class ごとの欄 (walk は stop、gap は main・plain_black と、gap が `--gap-line` 以上なら best・best_value_black、rejoin は move・expected_black・rejoin_move・rejoin_main・rejoin (戻った先の局面)) です。
報告は 1 (good)・2a (bad)・2b (gap、`--bridge` のときだけ)・2c (rejoin、`--rejoin` のときだけ)・3 (shallow)・4 (weak) の節で、手順を漢字の棋譜で書きます。
walk の止まりの節は「出口の局面」の列に手番と何手目と手番の側を (`後手 14 手目 (RAMU)`、`先手 15 手目 (相手)`)、2b・2c は「局面 (RAMU の手番)」の列に手番と何手目を書きます。
2c は RAMU の手・相手の手・戻った先の main も漢字の棋譜で書き、手順なしの行は手順の欄に「手順なし」と書いて手を USI のまま載せます。
標準出力には class ごとの行の数を出します。

## `master opening-report`: 戦型の候補を比べる表

戦型の候補ごとに、RAMU の手番の決めた局面では main の代わりに決めた手を指し、相手は `master walk` と同じ頻度で初期局面から辿って、
出口の値・値の確度・強い対局の実績・危ない局面を通る重み・分岐の後の相手の手を比べる表と Markdown の報告を出します。辿りは `master walk` と同じ実装で、候補ごとに RAMU の側だけを辿ります。元データは書き換えません。

```sh
book_mine master opening-report --master moves.tsv --schema schema \
  --candidates candidates.tsv --opp-freq strong_games.db --opp-book big.ybb \
  --opp-records games.jsonl.gz --danger danger_positions.tsv \
  [--exclude-queue targets.jsonl] --title v66 --table opening.tsv --markdown opening.md
```

| 引数 | 既定 | 内容 |
| --- | --- | --- |
| `--master <TSV>` | 必須 | 元データ。RAMU の手番で指す main と、出口の値・出所を引く |
| `--schema <DIR>` | なし | 語彙の表 |
| `--candidates <TSV>` | 必須 | 候補の表 (下記) |
| `--opp-freq <PATH>` / `--opp-book <PATH>` | なし | 相手の手。`master walk` と同じ |
| `--min-games` / `--max-ply` / `--weight-floor` / `--opp-book-diff` / `--opp-book-best` / `--opp-book-first` / `--cont-max` | `master walk` と同じ | 辿り方。`master walk` と同じ意味と既定 |
| `--opp-records <PATH>` | なし | 強い対局の棋譜 (`master walk --opp-records` と同じ形)。局面ごとの局数と得点、危ない局面の判定に使う |
| `--records-min-rate <R>` / `--records-max-ply <N>` | 3800 / 130 | 数える対局の両者のレートの下限と、1 局を見る手数 |
| `--danger <TSV>` | なし | 危ない局面の表。列 `side`・`key` (手数を除いた SFEN) を名前で引き、ほかの列は読まない。`#` で始まる行は注 |
| `--danger-games <N>` / `--danger-score <S>` | 10 / 0.2 | 強い対局で局数が N 以上、RAMU の側の得点が S 未満の局面を危ない局面とみなす |
| `--black-score <S>` | 0.8 | 先手の候補で別に数える、出口かその 10・20 手前の局面 (局数 `--danger-games` 以上) の先手の得点の上限 |
| `--exclude-queue <PATH>` | なし、繰り返し可 | 既に積んだ木のキュー。読みを足す候補に、同じ根・同じ側の木の id を書く (`master walk --exclude-queue` と同じ照合) |
| `--top <N>` | 20 | 危ない局面の一覧に候補ごとに、読みを足す候補の一覧に側ごとに載せる数 |
| `--list-min-weight <W>` | 0.001 | 危ない局面の一覧に載せる重みの下限 |
| `--reply-min-share <P>` | 0.01 | 分岐の後の相手の手で、割合がこれ未満の手を「その他」にまとめる |
| `--table <TSV>` / `--markdown <MD>` | どちらか必須 | 候補ごとの表と報告 |
| `--title <NAME>` | 空 | 報告の見出しの版の名前 |

候補の表は見出し `name side override` のタブ区切りです。`#` で始まる行と空行は読み飛ばします。
`side` は RAMU の側 (`b` / `w`)、`override` は `<手順>:<手>` を `;` で区切った並びで、初期局面から手順 (USI を空白区切り、空なら初期局面) を指した局面 (RAMU の手番) では main の代わりにその手を指します。
手順違いで同じ局面に来ても同じ手を指します。`override` が空なら今の main で、列ごと省いてもよい (末尾のタブが無い 2 列の行)。名前の重なり、RAMU の手番でない局面、指せない手、同じ局面の重なりはエラーで止まります。

```text
name	side	override
今の main	b	
初手 ▲７六歩	b	:7g7f
▲２六歩 △８四歩 で ▲７六歩	b	2g2f 8c8d:7g7f
▲２六歩 に △３二金	w	2g2f:4a3b
```

数え方 (重みは候補ごとの辿り全体を 1 とした割合):

- 値は出口の局面までに RAMU が最後に指した手の、元データの行の evals の平均 (先手から見た値) です。その手 (override の手) に元データの行が無ければ値なしで、手前の手の値は使いません (main だけの辿りでは `master exit-report` と同じ値)。値の無い出口は平均・分位に入れず、`no_value` に数えます。
- 値の出所は value_kind で、`tree:N` の N が 6 以上・3〜5・1〜2、`single` (1 回の読み)、`mean:N`、`unknown` (出所の記録なし)、値なしに分けます。止まりの種類は `master walk` と同じ own・out・rare・tiny・max です。
- 千日手は、辿りの循環 (1 回目の辿りで指した手の行き来の強連結成分) の中の局面で `--max-ply` に達した重み (循環を回り続けた重みの目安。ちょうど max の手数で初めて循環に入った重みも入る) と、own の出口で外部定跡の最善を両者が指し続けた手順 (`master walk` の `cont`) が同じ局面に戻る重みです。1 度だけ回って循環を抜けた重みは数えません。
- 危ない局面を通る重みは、辿りの途中か出口で、強い対局の判定 (`--danger-games`・`--danger-score`) か `--danger` の表の局面を通った重みです。両方・片方ずつも出します。`--danger` の局面は手数が付いていてもよく、辿りと同じ書き方に直して照らします。標準出力に、表の局面のうち同じ側の候補の辿りで来るものの数を側ごとに出します。
- 分岐は override の手の行き先の局面 (相手の手番) です。override が無ければ、main の辿りで最初に来る相手の手番の局面 (後手なら初期局面) です。別の override が手順違いで同じ局面に着くなら 1 つの分岐にまとめます。分岐ごとに、来た重み、相手の手ごとの割合とその先の出口の値の中央値を出し、どれかの分岐で最も多い手以外に進んだ重み (1 本の重みにつき 1 回) を「外す重み」にします。
- 出口の値・危ない局面・分岐の重みは、辿りの重みを、RAMU の最後の手・通った危ない局面・分岐と相手の手ごとに分けて持って数えます (同じ手数で同じ局面に来た重みを足し合わせても混ざらない)。そのため辿りの行は `master walk` より多くなります。`--weight-floor` は分けた重みを同じ局面で足した重みで判定するので、止まる所は `master walk` と同じです。足した重みが下限以上の局面で下限未満に分かれた重みは、最初に通った危ない局面を忘れて足し合わせます (分かれ方が増え続けないようにするため。通ったかどうかは残すので危ない局面を通る重みの列は変わらず、危ない局面ごとの一覧からだけ抜ける)。
- 出口の局数と得点は、同じ局面の出口を 1 回だけ数えた和です。`black_below` は行の手順 (同じ局面に来た手順のうち 1 つ) で 10・20 手前の局面を決めます。

表は候補ごとに 1 行で、列は `name side override override_weight mean_black p10 p25 median p75 p90 ge50 ge100 le80 le0 draw tree6 tree3_5 tree1_2 single mean_kind unknown no_value own out rare tiny max own_lt20 own_lt30 exit_games exit_score branch_games branch_score danger_strong danger_listed danger_both danger_any black_below deviate deviate_median` です。
値 (`mean_black`・分位・`deviate_median`) は先手から見た評価値、ほかの重みの列は割合 (0〜1)、`exit_score`・`branch_score` は RAMU の側の得点です。
`ge50`・`ge100` (先手 +50 以上・+100 以上に着く重み) と `black_below` は先手の候補だけ、`le80`・`le0` (先手から見て +80 以下・0 以下に着く重み) は後手の候補だけに書き、他方は空です。
`own_lt20`・`own_lt30` は出口の局面までに指した手の数が 20・30 未満 (出口の局面で次に指す手が 20 手目・30 手目まで) の own の重み、`override_weight` は override の分岐の局面を通る重み (override の無い候補は空、報告では main の分岐に来る重み) です。

報告の局面の番号は全部の表で同じ数え方です: 何手目 = その局面で次に指す手の番号 (初期局面の手が 1)。先手番の局面は奇数、後手番の局面は偶数で、合わない行があればエラーで止まります。
報告は日本語で、RAMU が先手・後手の候補ごとに出口の値・値の確度・強い対局の実績と危ない局面を通る重み・分岐の後の相手の手の表、
候補ごとに最初に通る危ない局面の一覧 (何手目・通る重み・強い対局の局数と得点・判定 (両方・強い対局だけ・表だけ)・手順)、
締めまでに読みを足すと効く own の出口の一覧 (候補をまたいで同じ局面をまとめ、何手目・重み・候補・値・出所・受付・手順・SFEN) を書きます。手順は `master exit-report` と同じ漢字の棋譜です。
標準出力には候補ごとの行の数・中央値・危ない局面を通る重みと、局面の数・時間を出します。

計算量: 候補ごとに 2 回辿ります (1 回目で局面と手の行き来を集め、強い対局を数える局面と循環を決める)。2 回目の辿りの行は、分けた重みの数だけ増えます。強い対局の棋譜は 1 回だけ 1 行ずつ読み、辿った局面だけを数えます。
約 22 万行の元データ、86 MB のテキスト `.db` の頻度の定跡、1,900 万局面の `.ybb`、両者 R3800 以上が約 3.9 万局の棋譜で、候補 10 本 (先手 4・後手 6) が release build の 1 スレッドで約 50 秒・ピーク約 310 MB でした。

## `master compare`: 自前の定跡と外部定跡の突き合わせ

自分は元データの main、相手は外部定跡の最善、または最善から指定点差以内の手を指すとして、両側を辿ります。
`master walk` と違い、強い対局の頻度で相手の手を補いません。元データと外部定跡は読み取り専用です。

```sh
book_mine master compare --master moves.tsv --schema schema \
  --opp-book external.ybb --opp-book-diff 0 \
  --table compare.tsv --markdown compare.md \
  --opp-records games.jsonl.gz --records-min-rate 4100
```

| オプション | 既定値 | 説明 |
|---|---:|---|
| `--master` / `--opp-book` | 必須 | 元データの TSV と外部定跡 (`.db` / `.ybb`)。テキスト定跡は手数を除いた SFEN 順に並べる |
| `--table` / `--markdown` | 必須 | 機械処理用の TSV と日本語の一覧 |
| `--schema` | なし | 元データの語彙の検査に使う表のディレクトリ |
| `--side` | `both` | 自分の側 (`b` / `w` / `both`) |
| `--opp-book-diff` | `0` | 外部定跡の最善から許す点差。境界を含む。0 は同値の最善をすべて辿る |
| `--opp-book-best` | `0.9` | 0 より大きく 1 より小さい仮の重み。同値の最善で等分し、残りは点差以内の他の手で等分する。他の対象手が無ければ最善に 100% |
| `--max-ply` | `160` | 初期局面から辿る手数の上限 |
| `--weight-floor` | `1e-5` | 小さい重みの枝を打ち切る下限 |
| `--cont-max` | `100` | 自分の定跡が切れた後、両者が外部定跡の最善で指し続ける手数の上限 |
| `--start-moves` | なし | 初期局面から指す USI 手順。その先を重み 1 から比較する。指定部分は定跡との一致を要求しない |
| `--opp-records` | なし | 強い対局の JSON Lines (`.gz` 可)。`master walk` と同じ形式 |
| `--records-min-rate` / `--records-max-ply` | `3800` / `200` | 両者のレートの下限と、棋譜を調べる手数の上限 |

重みは分岐に置いた仮定によるものです。実戦で来る確率や棋譜の頻度ではありません。
最善以外の候補も辿るときは `--opp-book-diff 30` などを指定します。
手数・重みで打ち切った行を、定跡が尽きた出口と区別して表示します。
同じ手数で同じ局面に合流した重みは足し合わせ、手順は代表の 1 本を残します。

評価値はすべてプラスが先手良しです。自前の値は最後に指した自分の手の元データの値で、
その手の番号・出所・`value_kind` (`tree:N` は木の伝播、`single` は 1 回の読み、`unknown` は記録なし) を併記します。これは定跡が切れた局面そのものの読みではありません。
値が無い場合は空欄にし、0 で補いません。外部定跡の値も別のエンジンの尺度なので、差から手の優劣を決めないでください。

自分が先に定跡を出て、外部定跡が続く行を準備の線の候補として目立たせます。
続きは同値の最善のうち代表の 1 手で辿る参考手順であり、実戦の進行を保証しません。
続きの最後の値と止まった理由を併記し、手数上限や循環を外部定跡の末端と取り違えないようにします。
出口の値が互角でも、長い準備の安全性は判断できません。
強い対局の実績は出口と 10・20 手前の局面に着いた局数と自分の側の得点率です。
得点は勝ち 1・引き分け 0.5、同じ対局が同じ局面を繰り返しても 1 局と数えます。
レートの帯を一覧に記し、未指定・手前の局面が無い場合・該当 0 局を区別します。他者の実績は読みを確かめる手がかりです。

外部定跡は必要な局面をファイルから引き、元データはディスク上の索引を使います。
棋譜は 1 局ずつ読み、対象の出口と手前の局面だけを集計します。
入力と同じファイルや出力どうしを同じ場所に指定するとエラーにします。

## `master opp-book-diff`: 外部定跡と食い違う局面

`master walk` と同じ辿りで RAMU の手番に来る局面を集め、元データの main の伝播の値と、外部定跡 (`--opp-book`) のその局面の値を比べて、食い違う局面を表と報告にします。外部定跡の値は別のエンジン・別の物差しの値なので、手を決めるのには使わず、RAMU の読みで確かめる所の当たりにだけ使います。元データ・受付は書き換えません。

```sh
book_mine master opp-book-diff --master moves.tsv --schema schema \
  --opp-book big.ybb --opp-freq strong_games.db --opp-records games.jsonl.gz \
  [--exclude-queue targets.jsonl] --table diff.tsv --markdown diff.md --enqueue-out queue.jsonl
```

| 引数 | 既定 | 内容 |
| --- | --- | --- |
| `--master <TSV>` | 必須 | 元データ |
| `--schema <DIR>` | なし | 語彙の表 |
| `--opp-book <PATH>` | 必須 | 比べる外部定跡 (`.db` か `.ybb`)。辿りでも `master walk --opp-book` と同じく相手の手に使う |
| `--side <b\|w\|both>` | `both` | RAMU の側 |
| `--opp-freq`・`--min-games`・`--max-ply`・`--weight-floor`・`--opp-book-diff`・`--opp-book-best`・`--opp-book-first` | walk と同じ | 辿り方 (`master walk` と同じ意味・既定) |
| `--cont-max <N>` | `100` | 比べた局面から定跡が切れる所まで、と、そこからの外部定跡の続きを辿る手数の上限 |
| `--min-weight <W>` | `0.002` | 比べる局面の重みの下限 |
| `--min-gap <CP>` | `80` | 差か逆向きの差がこれ以上の局面を出す |
| `--draw-value-black <CP>` / `--draw-value-white <CP>` | `-70` / `70` | 伝播の千日手の値 (`master audit` と同じ) |
| `--threads <N>` | `0` | 伝播のスレッド数 (0 = 全コア) |
| `--opp-records <PATH>` | なし | 強い対局の棋譜 (`master walk --opp-records` と同じ形)。比べた局面に着いた局数と得点の列を足す |
| `--records-min-rate <R>` / `--records-max-ply <N>` | `3800` / `130` | `--opp-records` で数える対局のレートの下限と手数 |
| `--exclude-queue <PATH>` | なし、繰り返し可 | 既に積んだ木のキュー。`master walk --exclude-queue` と同じ照合 (根・`roots` と、`game` の最初の `depth+1` 手で通る局面) で、定跡が切れる所と同じ局面・同じ側の木の id を `queued` に出し、キューの行から除く |
| `--enqueue-out <PATH>` | なし | 差が `--min-gap` 以上の局面から定跡が切れた所を根にした `tree enqueue --from` の行 (JSON Lines) |
| `--depth`・`--priority`・`--tag` | `6`・`300`・`oppdiff` | キューの行の段数・priority の始まり (順位 0〜99 を足す)・id `<tag>_<根の手数>_<局面の hash 16 桁>_<b\|w>` の名前 |
| `--opp-n`・`--stop`・`--rounds`・`--draw-black`・`--draw-white` | `3`・`300`・`3`・`-70`・`70` | キューの行の `opts` |
| `--table <TSV>` / `--markdown <MD>` | なし | 表と報告。`--enqueue-out` と合わせてどれかが必須 |
| `--title <NAME>` | 空 | 報告の見出しの版の名前 |

比べる局面と重み:

- 辿りは `master walk` と同じです (RAMU は元データの main、相手は `--opp-freq` の頻度か外部定跡の手)。対象は、RAMU の手番で元データに main がある局面のうち、重みが `--min-weight` 以上のものです。相手の手番に来る重みは混ぜません。
- 重みは、その局面を通る辿りの割合です。1 本の辿りで同じ局面に何度来ても (千日手の繰り返し) 1 回だけ数え、別の手数で来た辿りの重みは足します。止まり方 (`--weight-floor`・`--max-ply`) は walk と同じく、同じ手数・同じ局面の重みの合計で決めます。手順は最も少ない手数で最初に辿ったものです。

比べる値:

- 私たちの値は、元データ全体を `master audit --bridge-replies` と同じ計算で伝播した main の値です (同値の手の -1 の補正は audit と同じく戻します)。main の値が分からない局面と、外部定跡に無い局面は比べず、要約に数だけ出します。
- 外部定跡の値は、その局面の同じ手の値 (無ければ空) と、1 位の手 (同じ値なら定跡ファイルで先の手) とその値です。
- 差 `gap` = 私たちの値 − 外部定跡の同じ手の値 (外部定跡に main が無ければ 1 位の値) で、外部定跡が RAMU の側に悪いと見る量です。逆向きの差 `reverse` = 外部定跡の 1 位の値 − 私たちの値です。どちらかが `--min-gap` 以上の局面を出します。

定跡が切れる所: 比べた局面から、RAMU は元データの main、相手は外部定跡の 1 位 (無いか指せなければ元データの main) を指して進み、RAMU の手番で元データに無い局面に着いた所です (`cut_end=own`)。相手の手が無ければ `out`、局面が繰り返せば `cycle`、`--cont-max` 手で `max` です。`own` の局面からは、walk の `cont` と同じく外部定跡の 1 位を両者が指し続けた手順と、最後に引けた局面の 1 位の値を出します。

表の列は `side ply weight gap reverse main ours ours_black main_from main_kind ext_same ext_same_black ext_best ext_best_value ext_best_value_black cut_end cut_ply cut_key cut_moves cont cont_end_black queued n0 score0 key moves` です。`ply` は main が何手目か、値は手番の側 (RAMU) から見た値と、`_black` の付く列は先手から見た値 (プラス = 先手良し) です。`main_from` は audit の `eval` (1 回の読み) か `propagated` (先の局面から伝播)、`main_kind` は元データの main の行の `value_kind` (`tree:N` は N 段の木) です。`cut_ply` は定跡が切れた局面で RAMU が指す手が何手目か、`cut_moves` は比べた局面からそこまでの手順です。`n0`・`score0` は `--opp-records` の対局がその局面に着いた局数と RAMU の側から見た得点です。手番ごとに、差と逆向きの差の大きい方の大きい順、同じなら重みの大きい順に並べます。

報告は RAMU の先手・後手ごとに「外部定跡の方が RAMU に悪い」(`gap`) と「外部定跡の 1 位の方が RAMU に良い」(`reverse`) の節に分け、それぞれ差の大きい順 (同じなら重みの大きい順) に、手数・重み (%)・差 (点)・手 (漢字)・値 (先手から見た値、点)・定跡が切れる所・外部定跡の続き・受付・強い対局の局数・手順 (漢字の棋譜) を書きます。

キューの行は、`gap` が `--min-gap` 以上で定跡が切れた所 (`own`) のある局面から作ります。同じ根・同じ側は 1 行にまとめ (根の手数は最も小さい手数)、`game` に外部定跡の続きの最初の `depth+1` 手、`memo` にまとめた局面の手数・差・重みを入れます。並びと priority の順位は、まとめた局面のうち差の最も大きい局面 (同じなら重みの大きい局面) の差・重みの順です。`--exclude-queue` で受付に同じ根・同じ側の木がある所は積みません。

標準出力には手番ごとに、比べた局面の数と除いた数、差・逆向きの差が `--min-gap` 以上の局面の数と差の帯 (`--min-gap` から 100・150・200・300・500 で区切る) ごとの数、キューの行の数を出します。

計算量: 伝播は `master audit` と同じで、元データ全体の大きさで決まります。辿りは walk と同じ局面を通り、外部定跡は辿った局面ごとに 1 回だけファイルから引きます。辿った手に循環 (千日手で同じ辿りに 2 回来うる局面の組、手の向きの強連結成分) があれば、もう 1 回辿り、その成分の中だけで通った RAMU の手番の局面の組ごとに重みを分けて持ちます (成分を出た辿りは戻れないので、成分を出るときに組を空にする)。分ける組の数は循環の大きさで決まり、元データや定跡の大きさにはよりません。

## `borrow`: 大きい定跡の木を借りる

大きい定跡から、どの手を伸ばすかだけを借ります。値は自分のエンジンの読みで付け、大きい定跡の値は手を選ぶ当たりにだけ使います (登録する値には混ぜません)。読むべき出口が最初から全部分かるので、`tree serve` の並列を使い切って同時に読めます。

1. `borrow cut`: 根ごとに部分木を切り出し、読む局面 (出口と確認点) とキューの行を書く。
2. `tree enqueue --from <cut>/targets.jsonl` → `tree serve` で読む (同じ台帳を他の木と使い回せる)。
3. `borrow value`: 台帳の読みを部分木で根まで伝播し、元データに登録する行を作る。

### 切り出し

```bash
book_mine borrow cut --book big.db --roots roots.jsonl --out cut1 \
  [--own-diff 30] [--opp-diff 100] [--opp-top 2] [--max-plies 20] [--max-positions 20000] \
  [--check-every 2] [--master moves.tsv [--master-exit use|read]] [--extra marks.tsv] [--tag ext] [--priority 100] \
  [--parts 55,65,78 [--skip-journal journal-copy.jsonl [--fingerprint <指紋>]]]
```

| オプション | 既定 | 説明 |
|---|---:|---|
| `--book` | 必須 | 大きい定跡。テキスト `.db` か YaneuraOu のバイナリ定跡 `.ybb` (ファイル先頭で見分ける)。丸読みせず、ファイル上で二分探索して引く (`rshogi-book` の BookOnTheFly)。`.db` は `sfen` 行が手数を除いた SFEN のバイト順に並んでいる必要があり、並んでいなければ開くときに止まる (並べ替えた写しを渡す)。`.ybb` は並べ替え不要。count を持たず、depth は持つ版 (flags の bit0) と持たない版があるが、切り出しは値しか使わないので結果は `.db` と同じ。ファイルは読むだけ |
| `--roots` | 必須 | 根の一覧 (JSON Lines)。1 行に `id` (`[A-Za-z0-9_-]`)・`root` (`sfen ...` か `startpos moves ...`)・`own` (`b` / `w`、根の手番と同じ)。他の項目は読まないので、`tree` のキューの行もそのまま渡せる |
| `--out` | 必須 | 出力先。まだ無い dir |
| `--own-diff` | 30 | 自分の手番: 大きい定跡の最善の値からこの差以内の手を伸ばす |
| `--opp-diff` / `--opp-top` | 100 / 2 | 相手の手番: 値の上位 `--opp-top` 手と、最善からの差が `--opp-diff` 以内の手の和を伸ばす |
| `--max-plies` | 20 | 根からの手数の上限。この手数の局面は出口 |
| `--max-positions` | 20000 | 根ごとの局面数の上限。伸ばすとこれを超える局面は出口 |
| `--check-every` | 2 | 確認点の間隔。自分の手番の途中の局面を、自分の手のこの数おき (根を含む) に読む。0 で読まない |
| `--master` | なし | 定跡の元データの表。ここにある局面 (根を除く) を出口にする |
| `--master-exit` | `use` | `use`: 元データの値を使い、読まない (自分の手番は main、相手の手番は値の分かる手の 1 位。値は `master audit` と同じ伝播の値)。値が分からない局面は読む。`read`: 止めて読む |
| `--extra` | なし | 足す手 (TSV、見出しに `key`・`move`。他の列は読まない)。`borrow value` の `marks.tsv` をそのまま渡せる。どの根でも、部分木の途中の自分の手番の局面に着いたら足す |
| `--tag` / `--priority` | `ext` / 100 | キューの行の id (`<根の id>_<tag>_<b\|w>`) と priority |
| `--parts` | なし | 読む局面を複数の台に分けるときの、台ごとの速さの比 (カンマ区切り)。局面ごとにどれか 1 つの分け先に入れて `parts/part-<i>.jsonl` に書く (下の出力) |
| `--skip-journal` | なし | `--parts` の分け先から、この台帳に読みのある局面を除く。serve が書いている台帳は複製して渡す |
| `--fingerprint` | なし | `--skip-journal` の台帳で使う読みの条件の指紋 (台帳に条件が複数あるとき) |

切り出し方:

- 根から幅優先で伸ばし、同じ局面 (手数を除く key) は 1 つにまとめます (合流。先後反転の局面は別の局面)。局面の手数は最初に着いた (根から最短の) 手数です。
- 大きい定跡に合法な手が無い局面、手数の上限の局面、局面数の上限で伸ばせない局面、元データにある局面が出口です。非合法手は伸ばしません。
- 確認点は、伸ばした自分の手番の局面のうち、根からの自分の手の数が `--check-every` の倍数の局面です。
- メモリは根 1 本の部分木の局面数と、読む局面の一覧に比例します。

出力 (`--out`):

| ファイル | 内容 |
|---|---|
| `summary.tsv` | 根ごとの局面数・途中の局面・出口の種類ごとの数・確認点・読む局面の数。読む前の見積もりに使う (標準出力にも出す) |
| `nodes.tsv` | `id key ply level side turn kind read`。`turn` は `own` / `opp`、`kind` は `inner` / `exit_nobook` / `exit_depth` / `exit_cap` / `exit_master`、`read` は読む局面なら 1 |
| `edges.tsv` | `id key move child book_value origin`。`book_value` は大きい定跡の値 (手番の側から)、`origin` は `book` / `extra` |
| `reads.tsv` | 読む局面 (根をまたいで重複を除く) の `key ply` |
| `targets.jsonl` | `tree enqueue --from` に渡すキューの行。根ごと・手番ごとに 1 行で、段 0 (`depth: 0`) と `roots` に読む局面すべてを持つ |
| `parts/part-<i>.jsonl` | `--parts` のときだけ。`i` 番目の台に `tree enqueue --from` で渡すキューの行 (段 0、手番ごとに 1 行、id `<tag>_p<i>_<b\|w>`)。根をまたいで重複を除いた読む局面を、根の順 (根の中は局面の文字列順) に初めて出た順で、(割り当て済みの数 + 1) / 速さ が一番小さい分け先 (同じなら番号の小さい方) に入れる。どの局面も 1 つの分け先だけに入るので、別々の台帳を持つ台で同じ局面を 2 回読まない。行の `own` は局面の手番 (段 0 なので読みには影響しない)。`targets.jsonl` とは id が違うので、どちらか一方だけを積む。速さの比や `--skip-journal` の台帳を変えると分け先の 1 局面目が変わり得るので、切り直すときは `--tag` を変える |
| `cut.json` | 大きい定跡の名前・大きさ・SHA-256、条件、根の一覧 (`borrow value` が読む) |

同じ入力なら同じ出力になります。読む局面の組を変えて (条件や `--extra` を変えて) 切り直したときは `--tag` を変えてください。キューは id ごとに `root` を固定するので、同じ id で組が変わると拒否されることがあります。

### 読み

`tree serve` は段 0 の行の根 (`root` と `roots`) を読むだけで伸ばしません。既定では条件を問わず、他の木や別の切り出しの読みも同じ台帳で使い回します。特定条件の読みだけが必要な木では `opts.require_condition:true` を指定し、MultiPV・nodes も指定できます (例: `--nodes 1000000000 --multipv 3 --hash 1024`、`DrawValueBlack=-70` / `DrawValueWhite=70`、Threads は 1 に固定)。

```bash
receipt=$(book_mine tree enqueue --state "$STATE" --from cut1/targets.jsonl)
book_mine tree wait --state "$STATE" --receipt "$receipt"
```

serve の 1 周ごとの処理 (0.1 秒ごと) は、前の周から読みが来ていない木ではまだ読んでいない局面の数に比例します。`progress.json` の集計 (1 秒に 1 回まで) は、終わっていない木の局面数に比例します (2 万局面で数 ms)。

### 値付けと登録

```bash
book_mine borrow value --cut cut1 --journal journal-copy.jsonl --master moves.tsv \
  --out value1 --since 20261101-ext [--note "..."] [--alt-gap 40] [--replace-curated] [--line 40] [--mark-all] \
  [--draw-value-black -70] [--draw-value-white 70] [--fingerprint <指紋>] [--apply]
```

台帳は共有 lock で読むので、serve が書いている台帳は複製して渡します。複数条件がある場合、`--fingerprint` の指定があればその条件だけを使い、指定がなければ局面ごとに nodes_limit 最大、PV 行数最多、指紋の文字列最小の順で選びます。台帳からは読む局面の読みだけを残します (他の局面も行の検査と重複の矛盾の検査はする)。読む局面のどれかの読みが台帳に無ければ、何も書かずに止まります (足りない数と例を出す)。読む局面が 1 つも無ければ台帳は読みません。

伝播:

- 部分木を `.db` にして `book_backprop --merge replace` と同じ後退解析で解きます。切り出しと同じく、先後反転した局面へは辺を張りません。先手を自分として解く回は `--draw-value-black`、後手は `--draw-value-white` を千日手の値にします (部分木の中の循環)。
- 出口の手は読みの MultiPV の手と値です。0 行の読みは `bestmove resign` なら −30000、宣言勝ちなら勝ちの値です。`tree` の値 (model の伝播) は resign の葉を 0 にするので、詰みの絡む根では `borrow` と `tree` の 1 位が食い違うことがあります。読みの手が部分木の局面に着けば、その局面の値が伝わります。元データの値を使う出口は、その値だけを持ちます。
- 途中の局面の手は部分木の手だけです。確認点の読みは値に混ぜません。
- 同じ値の手は、`book_backprop` の同値手の補正 (勝ちは出口へ短く届く手を残し、遠回りの手を 1 下げる) を受けます。

全体の伝播: 根ごとに解いた後、登録する局面の手を全部 (alt にしない手も) 元データに入れ、`master audit` と同じ規則で伝播し直した値を登録する値にします。main・alt はこの値で選びます。

- 根ごとの伝播では、相手の手番の局面は大きい定跡から借りた手しか持たないので、値は木の持ち主に甘くなります。同じ局面を別の根が自分の手番として読んでいれば、その読みの手 (大きい定跡で低く見られて切り出しで外れた手など) が全体の伝播で入ります。元データに前からある局面の手も入ります。
- 根ごとの 1 位を main にすると、登録後の `master audit` (元データ全体の伝播) で main が 1 位から負けることがあります。全体の値で選ぶので、点検と同じ値で main が決まります。
- 値の替わった手は、何段先の値かが決まらないので `value_kind` を `unknown` にします。`cond` は、その局面の全候補が台帳の条件だけで決まると確かめられたときだけ残し、そうでなければ `unknown` にします。
- main を替えてよくない局面 (curated の局面や、置き換えてよい出所でない main) は、これまでどおり main を替えず、全体の値の 1 位を食い違いとして `conflicts.tsv` に出します。全体の 1 位が必ず main になるわけではありません。
- 元データに前からある手が木の手より良い局面では、main は木の手のまま選びます (別の条件の読みの値と混ぜて自動で決めない)。登録後の点検の表に出ます。
- `conflicts.tsv` の値は全体の伝播の値です。`values.tsv` と `roots.tsv` は根ごとの伝播の値のままです。全体の伝播で替わった手と、1 位が替わった局面の数を標準出力に出します。

確認点の印: 確認点の読みで、部分木の候補の外の手の値が、候補の伝播後の最善より `--line` 以上良ければ印を付けます (印は 1 局面に 1 手で、候補の外の手のうち読みの値が一番良い手)。`--mark-all` を付けると、候補の外の手があれば差によらず印を付けます (伸ばして値を確かめる手を集めるとき。`--line` は登録後の点検の線としてだけ使う)。印の付いた局面は登録しないので、`--mark-all` と `--apply` を一緒に使うと登録する局面が大きく減ります。`master audit` の反駁の点検と同様に、伸ばしていない手が伝播後の候補を上回る局面を調べます。印の付いた局面は、どの根から着いても登録せず、`marks.tsv` に出します。その手を足して伸ばし直すときは、`--extra value1/marks.tsv` を付けて新しい `--tag` で切り直し、足りない局面を読ませてから値付けをやり直します。

`--schema <dir>` で語彙と読み条件の名前を指定できます。台帳の条件名は指紋から決め、schema があれば対応する条件名、なければ先頭 8 文字です。対応する条件名がなければエラーです。

`nodes` は自身の読みがあればそのノード上限です。読みが無い途中の局面は、自分の手番では根を合流した後の最善手、相手の手番ではその根の伝播で選んだ手を辿り、子の上限を再帰的に使います。その手の先に由来する読みが無い場合だけ他の子から到達できる読みの最大値を使います。由来となる読みが無い場合だけ 0 とします。`cond` は登録する手ごとに決めます。依存する読みが複数条件にまたがる場合は `unknown` とします。その手の行き先から辿る全候補の値の由来を確かめ、台帳の条件だけで決まると確認できた手に条件名を付けます。元データの出口を使う場合は、出口の行の cond と、元データ内で値を伝播した先の条件も同じ名前だと確認できることが必要です。確認できない出口に依存する手は `unknown` にします。同じ部分木でも、その出口に依存しない手には台帳の条件名を付けられます。最善手の選択や同値補正も候補の値に依存するため、行き先の全候補を保守的に検査します。依存先に循環がある手は、他の部分木に台帳の読みがあっても `unknown` です。循環だけの部分木も `unknown` になります。

value_kind は後退解析のグラフで子局面への辺を持つ手なら `tree:K`、辺がなく読みの値を使う手なら `single` です。K は行き先の局面の残り段数 + 1 (`max_plies − 行き先の level + 1`)。出口の読みの手も部分木へ合流する場合は tree になります。K は切り出しの残り段数の目安であり、循環の長さや最善手順の長さではありません。

登録の規則は `tree register` と同じです (`--alt-gap`、`--replace-curated`、読み由来でない main は替えずに食い違いを一覧に出す、`record` の手は足さない)。違いは次のとおりです。

- 部分木の自分の手番の局面を、伸ばした局面は伝播後の値、出口は読みの値で並べて main・alt として登録します。元データで止めた出口と印の付いた局面は登録しません。
- 部分木の相手の手番の局面の手は `record` (指さない手) の行で残します (evals は伝播後の値、出口は読みの値、`source=ext`)。元データに同じ局面・同じ手の行が無ければ足し、`source=ext`・`tier=wide` の record があり値が違えば、値 (evals・nodes・cond・value_kind・lineage・since) を今回のものに替えます。それ以外の既存の行には触りません。今回自分の手番として登録する局面の `source=ext`・`tier=wide` の record (前の回に相手の手番として残した行など) は、外してから登録します。
  切り出しで伸ばした大きい定跡の手 (上位 `--opp-top` 手と差 `--opp-diff` 以内) で、読んで確かめていないので、逆の手番で対局するときに定跡として指さないためです。
  既定の `master audit` は record を使わないので、これらの局面は `--bridge-replies` で今までどおり補われます。`master audit --opp-records` を付けると record で先の局面の値が手前に伝わります (下の上限つき)。
  同じ局面を別の根が自分の手番として持つときは、その根の自分の手番の局面として登録し、record は足しません。標準出力に、record で足した手・値を替えた手・自分の手番として登録するので外した手の数を出します。
- 足す行は `source=ext`、`lineage=book:<大きい定跡のファイル名>@<内容の sha256 の先頭 8 文字>`、`nodes` は台帳の読みのノード数です。schema を省くと `ext` の行は読み由来として、後の `tree register` / `borrow value` で main を替え、`evals` を更新してよい行に数えます。
- 同じ局面が複数の根にあるときは、その局面から先の手数の余り (`--max-plies` − 段) の大きい根を使います。

`--apply` の前に、登録後の元データを `master audit` と同じ伝播に通し (通らなければ止まる)、点検の数 (main が 1 位から `--line` 以上負ける局面) の前後を出します。`--apply` を省くと元データは変えません。

出力 (`--out`、まだ無い dir):

| ファイル | 内容 |
|---|---|
| `roots.tsv` | 根ごとの伝播後の 1 位・2 位とその値、先手から見た 1 位の値、印の数。`tree` の木の 1 位と比べるときに使う |
| `values.tsv` | 全局面の手の伝播後の値。`id key ply level side turn kind move value value_black expanded book_value` (`value` は手番の側、`value_black` は先手から見た値、`expanded` は部分木の手なら 1) |
| `marks.tsv` | 印。`id key ply side move cp best best_value gap` |
| `conflicts.tsv` | 食い違いの一覧 (`tree register` の `tree` までの列と同じで、最後の列は根の id。元の main の値が上なので残す判定はしない) |

注意:

- 相手の手も部分木の手だけで見るので、部分木の外の相手の手で戻る循環 (先手の千日手) は、この伝播では見つかりません (`master audit` と同じ)。
- 元データの局面で止めた出口の値は、`value` を実行したときの元データから求めます。切り出しの後に元データが変わり、値が分からなくなった局面があれば止まります。


## `master line-db`: 注目の線を辿る定跡

```bash
book_mine master line-db --master moves.tsv \
  --moves "7g7f 3c3d 2g2f" \
  --tree-out tree-out-a tree-out-b --out line.db
```

初期局面から根までの手順を指定し、根から到達する元データと完了した木を合わせた
YaneuraOu 形式の `.db` を作ります。ビュアーで注目の線を辿るための資料です。
元データと木は書き換えません。

| 引数 | 既定値 | 意味 |
| --- | --- | --- |
| `--master` | 必須 | 13 列の元データ TSV |
| `--moves` | 必須 | 初期局面から根までの USI の手順。初期局面を根にするなら空文字列。非合法手や局面の繰り返しはエラー |
| `--tree-out` | 必須、複数指定・繰り返し可 | `tree serve --out-root` のディレクトリ。再帰的に木を探す |
| `--out` | 必須 | 出力する `.db` のファイル名 (接頭辞ではない) |
| `--schema` | 省略 | 他の master コマンドと同じ語彙・読み条件の表。指定時は source・cond・lineage の語彙も検査 |
| `--draw-value-black` | `-70` | 先手を自分として解くときの千日手の値 (手番側から) |
| `--draw-value-white` | `70` | 後手を自分として解くときの千日手の値 (手番側から) |
| `--force` | なし | 既存の出力を置き換える。入力と同じファイルへの出力は許可しない |

根より前の各局面には、指定した手順の 1 手だけを入れます。根以降では元データの
`main` と `alt` を辿り、`record` と `bench` は入れません。読みの値は `evals` の先頭
(手番側からの整数値。空なら仮の値 0)、採択数は `main` が 2、`alt` が 1 です。
木から入れた手は `tree.db` の値・採択数をそのまま使います。

`STATUS` の先頭の語が `DONE` の木について、`d0.jsonl` の最初の行の `key` を根として
索引化します。根以降の局面と、採用したどの手の行き先の局面も対象にし、その局面を根にした
木の `tree.db` を全て合わせます。木の内部にしかない局面や手の先からも元データと別の木を
辿り続けます。同じ局面の同じ手は元データを優先し、元データに無ければ `tree.db` の
更新時刻が新しい木を使います。同時刻ならディレクトリのパスが辞書順で後の木を使います。
局面は書かれたままの盤面キーで引きます (先後反転した局面では元データと木を探しません)。
根より前の局面と同じ局面が木にあっても、その局面の他の手の先は辿りません。

非合法手・`none` などの使えない手を除き、全体を既存の `book_backprop --merge replace`
と同じ関数で伝播して書き出します。根までの手順にも伝播後の値が付きます。
標準出力は `局面数`、`手の数`、使用した木のディレクトリ (`木`)、根の各手の
先手から見た伝播値 (`根<TAB>USI<TAB>値`) です。

木ごとの DONE 判定と根の読み取りは 1 回で、`tree.db` は必要になった木だけ 1 行ずつ読みます。
元データは一時ファイルに固定数のハッシュバケットで索引化し、必要な局面の行だけを読みます。
組み合わせる定跡のグラフをメモリに保持するため、メモリ使用量は採用した局面・手の数に比例します。
一時ファイルには元データの索引と伝播前後の定跡を置くので、一時ディレクトリの空き容量を確保してください。
