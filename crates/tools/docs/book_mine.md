# book_mine

`book_mine` は YANEURAOU-DB2016 テキスト定跡 `.db` を自エンジンの探索で book 外へ展開するツールです。YaneuraOu-ScriptCollection の BookMiner (`peta_next` → 掘る → peta_shock → 周回) に相当する処理を `frontier`・`expand`・`run` の 3 つのサブコマンドで行います。このほか、台帳を共有する定跡の木 (`tree`)、定跡の元データの点検 (`master validate`・`master export`・`master diff`・`master audit`・`master rejoin`・`master cycles`・`master check`)、相手の見立てで辿って読み始める局面を出す `master walk`、大きい定跡の木を借りた登録 (`borrow`) を後の節で説明します。

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
| `roots` | 追加の根の配列 (形は `root` と同じ、`root` と同じ手番に限る)。CLI の引数は無く `--from` の行で渡す。`borrow cut` が段 0 の行で使う |
| `own` | `b` / `w` / `bw`。`bw` は先手と後手の 2 本の木 (読みは台帳で共有)。2 本は一緒に受け付けるか一緒に拒否する |
| `depth` | 0〜20。0 は根 (と `roots`) を読むだけで伸ばさない |
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

serve は出力の根の `serve.lock` を終了まで保持し、根の下はその serve だけが書きます (対象の数によらず lock は 1 つ)。同じ出力の根で 2 本目の serve は起動できません。`tree replay` は出力先の `tree.lock` を取り、上の階層に `serve.lock` があればそれも取ってから書くので、serve が動いている間は根の下へ replay できません。`serve.lock` がシンボリックリンクなど通常のファイルでなければ、serve も replay も拒否します。serve は世代の dir を用意する (前の世代を SUPERSEDED にする) ときにその dir の `tree.lock` を一時的に取るので、serve より前に始まった replay が終わるまでその対象を用意しません。前の世代の dir の SUPERSEDED は書けなければ次の周回でやり直し、消された前の世代の dir は作り直しません。入力の照合に成功した後のエンジン起動失敗は局面の失敗に数えず、読みが成功するまで ALERT に 1 回だけ書いて、次の周回で起動し直します。入力ファイルの消失や読取り失敗 (開けるファイル数の上限を含む) は再試行せず、serve 全体を非0で停止します。出力の根が台帳・state の中にあれば起動を拒否し、対象の出力先が台帳・state と重なる行と、出力先の途中・生成物がシンボリックリンクの行は拒否します。

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
- worker の初回起動・再生成では、engine と全ファイル型 USI option (`EvalFile` / `EvalDir`・係数・明示 SPSA ファイルなど)、暗黙の `spsa.params` の有無・内容を開始時の SHA-256 と照合します。起動前、USI 初期化後、初回 `usinewgame` / `isready` 後に検査し、不一致や読取り不能は局面の失敗として再試行せず、ALERT を記録して serve 全体を非0で停止します。その worker の読みは journal に追記しません。原因を解消し、元と同じ内容で再開してください。fingerprint・journal・checkpoint の形式は変わりません。
- serve 稼働中の engine・モデル・パラメータは不変に保ってください。照合は worker 起動時のみで、大きなモデルにも全内容の読取りコストが発生します。ファイルシステムを排他ロックする仕組みではないため、検査の間だけ書き換えて元へ戻す競合や、検査後・稼働中の書換えとエンジン側の遅延読取りを完全には検出できません。内容を変更する場合は serve を終了し、新条件の state を使ってください。
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
# fingerprint = "..."  # serve が表示する読み条件。省略時はプロセスの条件
# draw = 0            # 手順上の循環に付ける手番側の値。USI DrawValue とは別
```

paths.tsv はヘッダ無しで `key<TAB>ply<TAB>USIの手順<TAB>メモ`。key は手数を除く SFEN の3フィールドで、同じ根をまとめ、根の手番をそろえます。GAME は各手順の最初の `depth+1` 手を局面別に合わせます。`own` は `b` / `w`。こちらの根の EXTRA は MultiPV 内だけ、相手の根の EXTRA は合法なら採用します。

条件の指紋は、engine とファイル型 USI option の内容 SHA-256、全 option、明示 `SPSAParamsFile` が無い場合のバイナリ隣接 `spsa.params` の有無と内容、nodes、MultiPV、局面ごとの初期化・info 解析の版から作ります。

replay は engine を起動せず、serve と同じ木の処理を台帳の読みだけで進めます。不足や ready:false は非0で終了します。出力先と journal・paths.tsv の衝突を検査し、出力先と journal の lock を取る前には既存の出力を変更しません。複数条件が台帳にある場合は `--fingerprint` で選びます。tree 用台帳は版付きで、既存 `expand / run` の台帳とは共用できません。 台帳は行単位で読み、serve は起動時の条件の fingerprint、replay は選択した fingerprint の読みだけをメモリに保持します。serve はその読みを直接参照し、別のキャッシュへ複製しません。replay は同じ排他 lock を保持したまま条件（Settings）の一覧だけを収集し、条件を選んでから対象の読みを読み込みます。他の fingerprint の行も形式・版を検査し、改行の無い末尾を回復しますが、読みの妥当性と重複の矛盾検査は対象の fingerprint 内だけで行います。保持量は対象条件の局面数に比例し、過去条件の総レコード数には比例しません（全行の走査時間は必要です）。局面ごとには読み (MultiPV 数行) だけを持ちます。条件の同一性は指紋で決まり、内容が同じモデルを別のパスで指定した読みも同じ条件として使い回します。台帳の保持は 1 局面あたり 1 KiB 前後で、10 万局面で約 100 MiB です (これとは別に、各木が自分の局面の読みを持ちます)。定跡の局面を深く読む用途ではこの規模を超えない想定で、ディスク上の索引は持ちません。

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

木ごとの出力先に `registration_status.json` と使用した読みの `d<段>.jsonl` を書きます。ready の場合は `own_nodes.tsv`、両方の手番の局面の手ごとの伝播後の値 `node_moves.tsv`、`tree.db`、`tree_rule.db`、参考値の `tree_bp_min.db`、根の表と最後の集計を含む `report.txt` を出します。min はライブラリの `backprop_file_with` を呼びます。参考値の生成・読み込みに失敗した場合は報告し、min 欄を空値として他の成果物を完成させます。report.txt は根の候補、伸ばし直し、警告と最後の集計を含みます。

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
- 元データの main が木の 1 位と違うとき、その main が読みから作った行 (`source` が `read` / `deep` / `ext`) で、局面に `curated` の行が無ければ、木の 1 位を main にし、元の main を alt に残します。それ以外 (`pin` / `top` / `team` などの main、`curated` の局面) は main を残し、木の 1 位を alt で足して `--conflicts` の一覧に出します。一覧の列は `key ply side main main_source main_tier main_tree_value tree_best tree_value tree` (値は手番の側から) です。
- `source` が `read` / `deep` / `ext` の main・alt の行で木に値のある手は、`evals` と `nodes` を木の値に更新します。
- 同じ局面が複数の木にあるときは、`remaining` の大きい (その局面から深く伸ばした) 木を使い、同じなら先に渡した木を使います。
- 結果は局面・役割 (main・alt・record の順)・手の順に並べ、局面ごとに main がちょうど 1 つ (`curated` の行だけでも 1 つ) で、同じ手の行が重ならないことを確かめてから書きます。


## `master validate`: 元データの全件検査

```bash
book_mine master validate --master moves.tsv
```

`--master` は必須です。入力を変更せず、次の 11 列をこの順で持つ TSV を検査します。

```text
key ply move role source evals nodes lineage tier since note
```

- 見出しの完全一致、各行の列数、1 行以上の手があること。区切りはタブ、行末は LF または CRLF です。セル内のタブ・CR・LF は使えません。
- `since` が空白だけでないこと、同じ `key` と `move` の重複がないこと。
- `role` は `main` / `alt` / `record`、`tier` は `curated` / `wide`。
- `ply` は空か ASCII の 10 進数字だけ。`evals` は空かカンマ区切りの符号付き整数 (各値と加算途中の合計が i64 に収まり、偶数丸めした平均が i32 に収まること)。小数・空の要素・空白つきの値は使えません。`record` の行も検査します (読み込み側は `record` の `evals` を読まないので、`validate` の方が厳しくなります)。
- 局面に `main` / `alt` がある場合、`main` がちょうど 1 行あること。全行と `curated` の行だけでそれぞれ検査します。`record` だけの局面も受け付けます。
- `key` は両玉を 1 枚ずつ含む SFEN の盤面・手番・持ち駒の 3 フィールドで、読み込んだ局面を書き出した正規形と一致すること。
- `move` はその局面の合法手を表す USI 文字列であること (`record` も検査します)。

`source` の語彙は検査しません。誤りがあれば、検出した誤りをすべて行番号順に stderr に出し、終了コードは非 0 になります。局面単位の誤りには、その局面が最初に現れた行番号を付けます。UTF-8 ではない行は行番号つきで報告し、次の行の検査を続けます。列数が不正な行は、セルを確定できないため行の内容の検査を省きます。局面を読み込めない場合は、その局面の合法手検査を省きます。

誤りが無ければ、局面数・手の数、tier ごとの局面数と手の数、role ごとの行数、入力に現れた source ごとの行数を stdout に出し、終了コード 0 で終わります。同じ局面が両方の tier にあれば、それぞれの局面数に数えます。`master audit` などの読み込み側も、`evals` の合計が i64 を超える場合はエラーになります。

## `master export`: 定跡・プリセット・注釈の書き出し

```sh
book_mine master export --master moves.tsv --out out/book --tier wide
book_mine master export --master moves.tsv --out out/book --schema schema \
  --reads reads.jsonl more-reads --min-nodes 900000000 --force
```

出力先の親ディレクトリは、書き出すときに必要なら作成します。元データ全体に `master validate` と同じ検査を行い、
誤りがあれば全件報告して書き出しません。検査後に tier で絞り、局面の key、role (main、alt、record)、
USI の手の順に並べます。入力は変更しません。

| 引数 | 既定 | 意味 |
|---|---|---|
| `--master <tsv>` | 必須 | 11 列の元データ |
| `--out <prefix>` | 必須 | 下記 4 ファイルの接頭辞 |
| `--tier wide\|curated` | `wide` | wide は全行、curated は curated の行だけ |
| `--schema <dir>` | なし | `sources.tsv` による source の語彙検査と説明 |
| `--reads <path>...` | なし | JSONL ファイル、または再帰的に `*.jsonl` を探すディレクトリ。複数指定可 |
| `--min-nodes <n>` | `900000000` | 取り込む reads のノード数の下限 (非負整数) |
| `--draw-pin <cp>` | `3` | draw の指定が空の読みで、先頭の評価値の絶対値がこの値以下なら除く (非負整数) |
| `--bad-eval <cp>` | `-150` | 到達局面の値がこの値以下なら報告 (有限値) |
| `--strict-bad-eval` | 無効 | 悪い到達局面が 1 つでもあれば、非ゼロで終了し書き出さない |
| `--base <db>` | なし | 局面の追加・削除と、手の集合が変わった局面の数を表示 |
| `--force` | 無効 | 既存の出力を置き換える |

schema は見出しが `source	description` の `sources.tsv` を持ちます (区切りはタブ)。
source と description は空でない値にし、source を重複させないでください。説明は日本語も使えます。
指定時は、選択する tier にかかわらず元データの全行の source を検査し、
注釈の `sources` に表の行順で書きます。ディレクトリや表が無い・不正ならエラーです。
省略時は source の語彙を制限せず、注釈の `sources` は `{}` になります。
この場合、stdout の `source:` の行は source の値の辞書順になります。

出力は次の 4 ファイルです。いずれかが既存なら `--force` が必要です。ディレクトリへの出力や、
元データ・読みのファイル・base・sources.tsv と同じファイルへの出力は `--force` でも拒否します。
全ファイルを出力先と同じディレクトリの一時ファイルに書き終え、既存の出力をそれぞれ
同じディレクトリの一時名へ rename で退避してから、新しいファイルを順に確定します。
途中で失敗したら、確定済みの新しい出力を削除し、退避した出力を元の名前に戻します。
復元にも失敗した場合はエラーに退避先のパスを含めます。全て成功したら退避先を削除します。新しい出力を全て確定した後に退避先の削除だけが失敗した場合は、新しい出力を残したまま、退避先のパスを含むエラーで終わります。
失敗時は残った一時ファイルを削除し、新しく作った親ディレクトリも空の場合だけ削除します。
4 ファイル全体は単一の原子的操作ではありません。`--force` では退避から確定までの間、元の名前に出力が無い (退避先の一時名にだけある) 時間があり、クラッシュや電源断ではその状態や一部の出力が残り得ます。

| ファイル | 内容 |
|---|---|
| `<prefix>.db` | `#YANEURAOU-DB2016 1.00`。局面は `sfen <key> 0`、手は `<move> none <value> 0 <count>`。main は value=100/count=2、alt は 100/1、record は -30000/0 |
| `<prefix>.default.explore.txt` | main・alt が合計 2 手以上ある局面で main のみを選ぶプリセット |
| `<prefix>.random.explore.txt` | 同じ局面で main・alt の全手を選ぶプリセット |
| `<prefix>.annot.json` | UTF-8、2 空白インデント、末尾改行付きの注釈 |

各プリセットの冒頭には名前と DB のファイル名・SHA-256 が入ります。
注釈の最上位の鍵は `format` (`rshogi-book-annotations`)、`version` (1)、
`book` (`file` と `sha256`)、`sources`、`positions` の順です。
`positions` の鍵は手数を除いた SFEN で、各局面には次の値があります。

- `ply`: 元データの同じ局面にある空でない ply の最小値。全て空なら null。
- `moves`: USI の手を鍵とするオブジェクト。各手は `role`、`source`、`nodes`、`note`、
  `since`、`tier` の文字列、カンマで分けた `lineage` の配列、数値の配列 `evals`、
  相手の返信の配列 `replies`、整数 `depth_after` を持ちます。
- `read`: 採用した読みがある場合だけ追加する集計。
  `n` は読みの回数、`nodes` は最大ノード数を有効数字 2 桁の指数表記にした文字列
  (例 `1.2e9`)、`top` は各読みの先頭の評価値の平均、`main` は元データの main が
  読みに含まれた回の評価値の平均 (無ければ null)、`main_n` はその回数です。
  平均は有理数で計算した後に浮動小数へ変換し、小数点以下 1 桁に偶数丸めします。
  `12.0` のように整数値でも浮動小数として書きます。

`replies` は、その手を指した後の相手の全合法手のうち、選択した tier の定跡局面に着く手を
USI 順に並べたものです。`depth_after` は main・alt の辺だけから作る 2 手単位のグラフで、
強連結成分を縮約して求めます。成分内の局面数を n とすると重みは `2 × (n − 1)`、
成分間の辺の重みは 2 とし、最長路を求めます。同じ局面への返信だけなら 0、
同じ成分の別局面ならその成分の最長路、別成分なら 2 と相手成分の最長路の和を候補にし、
手ごとの最大値を使います。record は常に 0 です。実際に指せる手数や評価値を保証する値ではありません。

reads はパスを正規化して重複を除き、パス順・行順で取り込みます。
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

到達検査は初期局面から先手・後手を別々に辿り、自分は main・alt、
相手は全合法手のうち定跡の局面へ着く手を使います。自分の手番の値は
`read.main`、main の evals の平均、`read.top` の順に選びます。
stdout には curated main と wide alt が混在する局面の警告、悪い到達局面と経路、
値が無い局面数、base との差、tier・role・source・since の件数、
プリセット局面数・返信リンク数・最大 depth_after、DB の SHA-256 をこの順に出します。

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
- `preset membership errors` は DB に無い局面・手、`preset record selections` は role が `record` の手の選択です。それぞれ該当する局面・手を一覧にします。
- 先手・後手それぞれを自分として初期局面から辿ります。自分はプリセットの手 (指定の無い局面は `main`) と DB の手の共通部分を使い、相手は合法手のうち DB 内の局面へ戻る手を使います。自分の非合法手は辺にせず、側ごとに `illegal own moves b/w` で除外数を出します。先後反転も行いません。プリセット行は `sfen <盤面> <手番> <持ち駒> <手>...` で、同じ局面の行は手の集合を併合します。
- `bad-eval reachable positions` は、自分の手番で DB にある到達局面のうち、値が閾値以下の局面数です。値は `read.main`、main の `evals` の平均 (閾値判定は丸めなし、表示は有効 6 桁)、`read.top` の優先順で取り、いずれも無ければ `positions without a value` に数えます。循環の内部も検査し、悪い局面ごとに直前の自分の手と根からの手順例を出します。
- 手順数は合流前の異なる手順を別々に数えます。辺が無い局面への到達を `exit ply=N`、循環する強連結成分への最初の到達を `cycle-entry ply=N` として集計します。`N` は到達局面までの初期局面からの手数です。相手が定跡外へ出る手は数えません。
- 循環は 2 局面以上の強連結成分または自己ループです。成分ごとに局面数、内部の辺数、16 手以内の最短循環の手数、`within-16` (16 手以内か)、入口局面と根からの手順例を出します。入口例は別の循環成分を経由する場合もあります。`within-16` は遡る手数の目安で、実際の千日手成立や連続王手の判定ではありません。
- 循環のある側の `total` は出口と循環入口の手順数の合計です。循環内の手順や最初の同一局面再訪までの手順数ではありません。循環の無い側では全出口までの総数・手数分布を正確に数え、`repetitions=0` を出します。カウントは `u128` で、上限を超える加算はエラーにします。

終了コードは membership error または bad-eval の局面があれば **1**、無ければ **0** です。record の選択・値の無い局面・循環だけでは失敗にしません。入力不正や読み込み失敗も **1** です。

メモリは入力の局面・手とグラフの頂点・辺に比例します。先後は順番に処理し、手順数は手数ごとの二面の配列で伝播するため、手順ごとの訪問済み集合は保持しません。SCC 分解は `O(V + E)`、分布は最大手数を `L` として最悪 `O((V + E) L)` (実際にはその手数で届く頂点・辺だけ) です。最短循環は成分内の辺だけを使い、各頂点から深さ 16 まで幅優先探索します。16 手以内に循環が無ければ `shortest-cycle=>16 within-16=false` とし、それ以上の長さは求めません。探索予算は成分ごとに頂点の取り出しと辺の検査の合計 10,000,000 回です (成分外への辺の検査も含む)。予算内に最短性を確定できなくても、16 手以内の循環が見つかっていれば、そのうち最も短い手数を `N` として `shortest-cycle=<=N within-16=true` とします。循環が 1 つも見つかっていない場合だけ `shortest-cycle=unknown within-16=unknown` とします。探索用の距離は頂点ごとに 1 バイトで、作業メモリは `O(V)` です。

出力は各見出しの後に明細を置き、所属エラー、record 選択、bad-eval (先手・後手の順)、値の無い局面数、`paths b`、`paths w` の順です。循環成分の一覧と非合法手の数は各側の paths 節に含めます。bad-eval と paths の明細は一時ファイルに蓄えてから出力し、両側のグラフを同時に保持しません。


## `master audit`: 定跡の元データの点検

```bash
book_mine master audit --master moves.tsv --table audit.tsv \
  --draw-value-black -70 --draw-value-white 0 [--line 40] [--out-db audited.db] [--fill-replies] \
  [--refutation-table refutation.tsv] [--enqueue-out refutation.jsonl] \
  [--opp-freq strong_games.db --min-weight 0.001]
```

定跡の元データの表 (上の 11 列) の読みの値を `book_backprop` と同じ後退解析で伝播し、main が伝播後の 1 位から `--line` 以上負ける局面を点検の表に書きます。元データは書き換えません。出力先の親ディレクトリが無ければ作ります。出力先が元データと同じファイル、または `--table` と `--out-db` が同じファイルなら止まります。`key` が正規の SFEN の表記 (持ち駒の並びなど) でなければ止まります (`book_backprop` は正規でない key の局面に子局面の辺を張らないため)。

| オプション | 既定値 | 説明 |
|---|---:|---|
| `--master <PATH>` | 必須 | 定跡の元データの表 |
| `--table <PATH>` | 必須 | 点検の表 (TSV) の出力先 |
| `--line <CP>` | `40` | 点検の線。main が 1 位からこの値以上負ける局面を表に出す |
| `--draw-value-black <CP>` | `-70` | 先手を自分として解くときの千日手の値。対局の `DrawValueBlack` に揃え、明示を勧める |
| `--draw-value-white <CP>` | `0` | 後手を自分として解くときの千日手の値。対局の `DrawValueWhite` に揃え、明示を勧める |
| `--out-db <PATH>` | なし | 値の分かる手だけを載せた、伝播後の値つき `.db` |
| `--fill-replies` | なし | 元データに無い相手の手番の局面を補って伝播する (下の「伝播の仕方」) |
| `--refutation-table <PATH>` | なし | 反駁の表 (TSV) の出力先 (下の「反駁の点検」) |
| `--refutation-margin <CP>` | `1` | 反駁の線。伸ばしていない手の最善が伸ばした手の最善をこの値以上上回る局面を出す (1 以上) |
| `--enqueue-out <PATH>` | なし | 反駁の局面を読み足す、`book_mine tree enqueue --from` に渡す行 (JSON Lines) の出力先 |
| `--refutation-depth <N>` | `2` | キューの行の `depth` (0〜20) |
| `--refutation-max <N>` | なし | キューの行の数の上限 (表の順で先から) |
| `--priority <N>` | `100` | キューの行の priority の基準 (`borrow cut` の `--priority` と同じ意味)。表の順位を足す |
| `--opp-freq <PATH>` / `--min-games <N>` / `--max-ply <N>` / `--weight-floor <W>` / `--min-weight <W>` | なし / `3` / `80` / `1e-5` / `0` | 反駁の表の重み。`master rejoin` と同じ意味 (`--min-weight` は表から落とす下限)。`--opp-freq` は反駁の出力と一緒に使う |
| `--threads <N>` | `0` | スレッド数 (0 = 全コア)。結果はスレッド数によらない |

伝播の仕方:

- main・alt の手だけを使います。`record` (指さない手) は除きます。
- 手の値は `evals` の平均 (0.5 は偶数の側へ丸める) です。値は手番の側から見た値です。
- 値の分かる手は、読みの値のある合法手と、book 内の子局面 (直接、または先後反転した局面) に値の分かる手がある手です (読みの値の無い手が続く線も、先に読みの値があれば分かる手になります)。分からない手は伝播にも、1 位の候補にも、main との比較にも使いません。
- 伝播は `book_backprop --merge replace --skip-unusable-moves` と同じです。book 内に子局面のある手は子局面からの値、無い手は読みの値になります。
- `--fill-replies` を渡すと、読みの値の無い手 (多数手など) を指した後の局面が元データに無ければ (先後反転した局面も無ければ)、その局面を、相手の合法手のうち元データの局面に着く手だけを持つ相手の手番の局面として補います。元データは自分の手番の局面にしか行を持たないことが多く、補わないと先の局面の読みの値が相手の手番の局面で途切れるためです。補った局面の値は定跡に入っている応手だけで決まるので、定跡に無い応手を見ない分だけ自分の側に甘い値です。この甘さは補った局面の上の局面の最大値を通って、さらに上の局面へ伝わります。読みの値のある手が直接か反転で着く局面は補いません (全ての応手を見た読みの値を直接は置き換えない)。反転した局面が元データにある局面へは応手を張りません (千日手の値が先後で違うと、反転で合流した局面を含む循環を伝播できないため)。補った局面そのものは点検の表・`--out-db` に出しませんが、補った局面を通って付いた値 (上の局面の読みの値のある手に伝わった値も含む) は、表・`--out-db`・反駁の点検 (`--refutation-table`・`--enqueue-out`) に普通の伝播の値として入り、区別されません。補った局面を通って、千日手の値が先後で違う循環に反転で合流すると止まることがあります (「反転キーで合流した…」のエラー)。そのときは `--fill-replies` を外してください。`master rejoin`・`master cycles`・`borrow` の伝播では補いません。

点検の表は、main が 1 位から線以上負ける局面を差の大きい順 (同じ差は key 順) に並べます。同じ値なら main を 1 位とみなします。列は `key ply side source main main_value main_from main_eval best best_value best_from best_eval gap main_value_black best_value_black` です。`*_value` は手番の側から見た伝播後の値、`*_from` は値の出どころ (`eval` = 1 回の読みの値、`propagated` = 子局面からの伝播)、`*_eval` は元の読みの値 (無ければ空)、`source` は main の行の source、`*_value_black` は先手から見た値 (プラス = 先手良し) です。

標準出力に要約を出します: 局面数 (手番ごと)、手の数・値の分かる手の数・非合法手・除いた record、読みの値から伝播で変わった手の数と差の分布、読みの値が無く伝播で値が付いた手の数、main・alt が 2 手以上の局面数、main が 1 位から線未満・線以上負ける局面数、main の値が分からない局面数、補った相手の手番の局面の数、循環 (自明でない強連結成分) の数。

### 反駁の点検

YaneuraOu の `peta refutation` に当たります。読みの MultiPV の 2 位以下の値は 1 位より甘く出やすく、伝播で 1 位の手の先が掘られて値が下がると、先を伸ばしていない甘い手が伝播後の 1 位に繰り上がります。`--refutation-table` か `--enqueue-out` を渡すと、次の局面を出します。どちらも渡さなければ、上の表・要約・`--out-db` の出力は変わりません。

- 伸ばした手 (`*_from` が `propagated`、book 内の子局面から値が来る手) と伸ばしていない手 (`eval`、1 回の読みの値) の両方に値の分かる手がある局面で、伸ばしていない手の最善 `best` が伸ばした手の最善 `expanded_best` を `--refutation-margin` 以上上回る局面 (同じ値なら元データの順で先の手を最善とします)。
- 値は同値の -1 補正の前の値で比べます。伝播の書き出し (`--out-db` と `book_backprop`) は 1 位と同値で選ばなかった手を -1 するので、補正後の値で比べると同値の手が margin 1 で当たるためです。そのため反駁の表の値は、点検の表や `--out-db` の値と 1 違うことがあります。
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
- 相手の手も定跡の手だけで見るので、定跡の外の相手の手で同じ局面に戻る循環 (先手の千日手) と、定跡の外の相手の手で値の低い局面に合流する所は、この伝播では見つかりません (YaneuraOu の peta_shock も同じ)。それぞれ `master cycles` と `master rejoin` で調べます。
- 元データは全件を読み、局面数と手の数に比例するメモリを使います。伝播用のグラフは一時 `.db` を介さずメモリ内で1回だけ構築します。key の検査・索引の作成には読み取り専用の軽い局面を使い、利きの全升集計・NNUE の駒表・駒割の計算を省きます。手を指す経路では完全な `Position` を使います。頂点の展開では詰めた局面から完全な局面を直接構築し、在庫検査とハッシュ計算を1回だけ行います。値の分からない手と、手が無くなる局面を同じ配列内で除き、消えた直接の子局面に残る反転局面があれば辺を張り替えます。`rejoin` / `cycles` / `borrow` の元データの値の伝播もこの経路を使います。

## `master rejoin`: 相手の手で定跡に戻ると値が下がる局面

```bash
book_mine master rejoin --master moves.tsv --table rejoin.tsv \
  --draw-value-black -70 --draw-value-white 0 [--line 40] [--side both] [--preset random] \
  [--opp-freq strong_games.db --min-weight 0.001]
```

自分 (RAMU) が定跡の手を指した後、相手が定跡に無い合法手を指して別の定跡の局面に戻るとき、戻った先の値がその手前で期待した値より `--line` 以上下がる所を一覧にします (YaneuraOu の `peta rejoin` に当たります)。`master audit` の伝播は相手の手も定跡の手だけで見るので、相手が定跡に無い手で合流すると手前の値が実際より高く見えます。一覧の「掘る局面」で相手の手を読み足すのが対処です。元データは書き換えません。

| オプション | 既定値 | 説明 |
|---|---:|---|
| `--master <PATH>` | 必須 | 定跡の元データの表 |
| `--table <PATH>` | 必須 | 一覧 (TSV) の出力先 |
| `--line <CP>` | `40` | 線。戻った先の値が手前で期待した値からこの値以上下がる所を出す |
| `--side <b\|w\|both>` | `both` | 自分の側 |
| `--preset <default\|random>` | `random` | 自分の手番で辿る手。`default` = main だけ、`random` = main と alt (書き出しのプリセットと同じ) |
| `--draw-value-black <CP>` / `--draw-value-white <CP>` | `-70` / `0` | 値の伝播に使う千日手の値 (`master audit` と同じ) |
| `--opp-freq <PATH>` | なし | 相手の手の頻度の定跡。`book_from_csa --no-instant-filter --min-rating 3800 --max-ply 80` などで強い対局から作った `.db` (手の count を局数として使う)。渡すと重みを付ける |
| `--min-games <N>` | `3` | 重み: 相手の局数がこれ未満の局面と手では辿るのを止める |
| `--max-ply <N>` | `80` | 重み: 初期局面からこの手数まで辿る |
| `--weight-floor <W>` | `1e-5` | 重み: これ未満の重みの局面では辿るのを止める |
| `--min-weight <W>` | `0` | 一覧と局面の展開に使う重みの下限。下限未満の自分の手番の局面の先は辿らない (`--opp-freq` を渡したときだけ使う) |
| `--threads <N>` | `0` | スレッド数 (0 = 全コア)。結果はスレッド数によらない |

調べ方:

- 初期局面から、自分の手番では `--preset` の手、相手の手番では合法手 (不成を含む) のうち元データの局面に戻る手を辿ります (先後反転した局面は引きません。対局の `FlippedBook=false` と同じ)。辿れた自分の手番の局面 `key` で手 `move` を指した後の、相手の手番の局面を `dig` とします。`dig` で相手が伝播に入っている手 (値の分かる main・alt) 以外の合法手 `rejoin_move` を指して元データの局面 `rejoin` に戻る組を見ます。
- 手前で期待した値 `expected` は `move` の伝播後の値、戻った先の値 `rejoin_value` は `rejoin` の main の伝播後の値です。どちらも自分の側から見た値で、伝播は `master audit` と同じです。`expected - rejoin_value` (`drop`) が `--line` 以上の組を出します。値の分からない手は数だけ要約に出します。
- 重み `weight` は `key` に来る重みです。自分は main を指し、相手は `--opp-freq` の局面ごとの手の頻度で指すとして、初期局面から辿ります。同じ手数で同じ局面に来た重みは足し合わせ、局面の重みは手数をまたいで足します。そのため重みは確率ではなく、来る回数の期待値です。自分の手番で元データに無い局面、相手の局数が `--min-games` 未満の局面、`--weight-floor` 未満の重みで止めます。`--preset random` でも重みは main だけで辿るので、alt からしか届かない局面の重みは 0 になり、`--min-weight` を 0 より大きくすると一覧から落ちます。

`--opp-freq` と正の `--min-weight` を併用すると、自分の手番の局面の重みが下限未満なら、その局面の手の生成・展開を止めます。下限と等しい局面は展開します。一覧は `key` の重みで絞るので、相手の手番の局面は重みによらず展開します (alt の手の先の相手の局面は重みが 0 でも、`key` の重みが下限以上なら一覧に出ます)。これは一覧を最後に絞るだけの場合と結果が異なることがあります。重みを求める経路 (自分は main、相手は対局の頻度) と調査で辿る経路 (自分は preset、相手は元データに戻る全合法手) は異なるため、下限未満の自分の手番の局面の先に、別の経路の寄与で重みが下限以上になる局面があっても、調査の経路がそこを通らなければ一覧に出ません。下限を省く (既定 0) か `--opp-freq` を省くと、従来どおり調査の全経路を辿ります。値の伝播・元データと頻度の定跡の読み込み・索引作成は絞りません。

一覧の列は `side key ply move role expected dig rejoin_move rejoin rejoin_main rejoin_value drop expected_black rejoin_value_black weight dig_games rejoin_games` です。`role` は `move` の役割 (`main` / `alt`)、`*_black` は先手から見た値 (プラス = 先手良し)、`dig_games` は `dig` での相手の局数、`rejoin_games` はそのうち `rejoin_move` の局数です (重みの列は `--opp-freq` を渡したときだけ埋まります)。重みがあれば重みの大きい順、無ければ `drop` の大きい順に並べます。一覧の出力先が元データか `--opp-freq` と同じファイルなら止まります。標準出力には手番ごとに、辿れた自分の手番の局面の数、定跡に無い手で戻る組の数と掘る局面の数、値の分からない手の数、一覧の件数を出します。重みで展開を止めた局面があれば、その数も出します。重みの下限で一覧から除いた件数は、展開した範囲内だけの集計です。

範囲と計算量:

- 範囲は自分の定跡の手で初期局面から届く局面だけです。`--preset default` で狭く、`random` で広くなります。重みで絞るには `--opp-freq` と `--min-weight` を使います。
- 相手の手番の局面ごとに全合法手 (平均 80 前後) について、局面を進めずに行き先のハッシュ値を求め、元データの局面と一致した手だけ局面を進めて、32 バイトに詰めた局面で確かめます。根からの段ごとに局面を並べて展開し、先手と後手も並べて解きます。時間は辿れる相手の手番の局面の数 × 合法手の数と、`master audit` と同じ伝播に比例します。
- 元データと頻度の定跡は全件を読み、局面数に比例するメモリを使います (頻度の定跡は辿らない局面の分も読みます)。辿るグラフは局面を 32 バイトに詰めて持ち (玉の欠けた局面は SFEN で保持します)、key の文字列は出力のときだけ作ります。通常局面では、局面ごとに約 70 バイト、辺ごとに 12 バイトです。強い対局の頻度の定跡から作った 88 万局面の元データ (全局面が初期局面から届く) で、両方の手番・`random` で 4 スレッド約 35 秒・ピーク約 1.4 GB でした (別の計算が並走する 32 論理コアの機械)。

## `master cycles`: 先手の千日手の循環を塞ぐ手

```bash
book_mine master cycles --master moves.tsv --table cycles.tsv \
  --draw-value-black -70 --draw-value-white 0 [--side b] [--removable-since 20261028-x18] \
  [--apply --note "計画の名前"]
```

自分の定跡の手と、相手の合法手で定跡に戻る手でできる循環 (千日手になり得る、同じ局面に戻る道) を見つけ、自分の手番の手だけを外して (指さない手 = `record` にして) 循環を無くす手の組を出します。後手の千日手は許しているので、既定は先手だけです。`--side w` / `both` で後手も出せます。既定は一覧と要約だけで、`--apply` を付けると元データを書き換えます。

| オプション | 既定値 | 説明 |
|---|---:|---|
| `--master <PATH>` | 必須 | 定跡の元データの表 |
| `--table <PATH>` | 必須 | 外す手の一覧 (TSV) の出力先 |
| `--side <b\|w\|both>` | `b` | 循環を塞ぐ側 |
| `--draw-value-black <CP>` / `--draw-value-white <CP>` | `-70` / `0` | 手の値の伝播に使う千日手の値 (`master audit` と同じ) |
| `--removable-since <TAG>` | なし | 外してよい手を、元データの `since` がこの値の行に限る (複数回指定可)。新しく登録した木の手だけで塞ぎ、前からある手 (丁寧な定跡・上位 AI の手など) を残したいときに使う。省くと全部の手を外してよい |
| `--apply` | なし | 元データを書き換える。外す手を `record` に、main を外した局面の替わりの手を `main` にし、どちらの行も note の末尾に `--note` を付ける |
| `--note <TEXT>` | `千日手の循環を塞ぐため指さない手にした` | 書き換えるときに note に付ける文 |
| `--threads <N>` | `0` | スレッド数 (0 = 全コア)。結果はスレッド数によらない |

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

根から、自分がどの手を残しても相手が循環に持ち込める (B で根が抜けられない) ときは、循環が残ります。要約に「塞げない循環」として局面の列を出します。

一覧の列は `side key ply move role value eval new_main new_main_value` です。`role` は外す手の役割、`value` は伝播後の値、`eval` は元データの読みの値、`new_main` / `new_main_value` は main を外した局面の替わりの main とその値です。標準出力には手番ごとに、外す前の循環の数と局面数 (default・random)、外す手の数 (main / alt)・main を替える局面の数・採った外し方、外した後の循環の数を出します。外した後の数は、同じグラフで外した手を除き main を替えて数え直した値です (自分の手番の辺を減らすだけなので、外した後の元データでグラフを作り直して数えるのと同じです)。一覧の出力先が元データと同じファイルなら止まります。

計算量: グラフは `master rejoin` と同じで、手番ごとに 1 回作り、先手と後手は並べて解きます。B と戻す段の準備は、グラフの大きさに比例します。A は 1 手 (大きい循環では 1 回の探索) ごとにその循環の中だけを分解し直し、厳密な採点の量は元の循環ごとに累計で上限を設けます。深さ優先探索は循環を閉じる辺ごとに探索の経路をなぞるので、長い経路に循環を閉じる辺が多いと循環の大きさの 2 乗程度になり得ます。戻す段は 1 手ごとに、その手の元の循環の中と、新しく根から届く局面を辿ります (戻せた手で届いた局面は以後辿り直しません)。A と C は循環ごとに並べて解きます。C の 1 回の整数計画は、循環の中の外せる手の数の変数と、局面ごとの制約、足した循環の制約でできます。上の 88 万局面の元データ (循環が先手 1,278・後手 1,803) で、両方の手番で 4 スレッド約 20 秒・ピーク約 0.9 GB でした (C を足す前と同じ。同じ機械)。元データの行 (書き換え用) は `--apply` のときだけ読みます。

## `master walk`: 自分が読み始める局面と重み

```bash
book_mine master walk --master moves.tsv --table walk.tsv \
  [--opp-freq strong_games.db] [--opp-book big.ybb --opp-book-diff 30 --opp-book-best 0.9] \
  [--min-weight 0.005 --enqueue-out queue.jsonl --depth 6 --priority 300 --tag x19 --game-from-book]
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
| `--cont-max <N>` | `100` | 大きい定跡の最善を両者が指し続けた手順を辿る手数の上限 |
| `--enqueue-out <PATH>` | なし | 自分が読み始める局面のうち重みが `--min-weight` 以上のものを、`tree enqueue --from` の行 (JSON Lines) に書く |
| `--depth <N>` / `--priority <N>` / `--tag <NAME>` | `6` / `300` / `walk` | キューの行の段数・priority の始まり (重みの順位 0〜99 を足す)・id `<tag>_<根の手数>_<局面の hash 16 桁>_<b\|w>` の名前 (`[A-Za-z0-9_-]` の 1〜40 文字) |
| `--game-from-book` | 無効 | キューの行の `game` に、大きい定跡の最善を両者が指し続けた手順の最初の `depth+1` 手を入れる (木がその手を読みの順位によらず伸ばす)。`--opp-book` が要る |

辿り方:

- 自分の手番では元データの main を指します (対局の default プリセットと同じ)。行が無ければ `own` で止めます。
- 相手の手番では、`--opp-freq` の局数が `--min-games` 以上の局面なら、局数が `--min-games` 以上の手を局数の割合で指します。局数の少ない手に行った分と、局数の足りない局面は、`--opp-book` があればその手で指します (同じ手は重みを足します)。局数が足りず大きい定跡にも無ければ `out`、局数の少ない手のうち大きい定跡で埋まらなかった分 (局数が足りていて全部の手が少ない局面を含みます) と指せない手の分は `rare` で止めます。重みが 0 になる手 (`--opp-book-best` が 0 か 1 のとき) は辿りません。
- 重みが `--weight-floor` 未満なら `tiny`、`--max-ply` 手に達したら `max` で止めます。同じ手数で同じ局面に来た重みは足し合わせ、手順は最初に辿ったものを残します。止まった重みの合計は手番ごとに 1 です。
- 大きい定跡の手は書かれたままの局面で引きます (先後反転では引きません)。`--opp-freq` だけなら `master rejoin` の重みと同じ辿り方で、相手の局数が足りずに止まった分が `out` と `rare` に出ます。

一覧の列は `side stop ply weight games book_moves cont_plies cont_end_black key moves cont` です。`ply` は初期局面からの手数、`games` は `--opp-freq` のその局面の局数、`book_moves` は大きい定跡のその局面の手の数、`cont` は `own` の局面から大きい定跡の最善を両者が指し続けた手順 (局面が繰り返すか、定跡に無くなるか、定跡の手が指せないか、`--cont-max` 手で止めます)、`cont_plies` はその手数、`cont_end_black` は最後に引けた局面の最善の値を先手から見た値 (プラス = 先手良し) です。手番ごとに重みの大きい順に並べます。標準出力には手番ごとに、止まり方ごとの重み、読み始める重みの手数の帯ごとの合計、読み始める局面の数 (同じ局面に別の手数で来たものは 1 つ) を出します。

キューの行は、同じ局面に別の手数で来た `own` の行の重みを足して 1 行にし (根の手数は最も小さい手数、`memo` の手順はその手数の手順)、重みの大きい順に priority を付けます。キューは同じ id で根の手数が変わる行を受け付けないので、id は局面と根の手数で決めます。同じ `--tag` で作り直すと、重みが入れ替わっても同じ局面・同じ根の手数は同じ id になります。出力先が入力のどれかと同じか、`--enqueue-out` と `--table` が同じファイルなら (別の書き方のパスでも) 止まります。

計算量: 辿る局面の数は重みの下限で決まり、局面ごとに局面を 1 回進めます。頻度の定跡と大きい定跡は、辿った局面ごとに 1 回だけファイルから引き、丸読みしません。メモリは元データと辿った局面の数で決まり、定跡ファイルの大きさによりません。約 7,800 局面の元データ、強い対局 (1 年分) から作った 88 万局面の頻度の定跡、1,900 万局面の `.ybb` で、両方の手番 1 スレッド約 2 秒・ピーク約 45 MB でした。

## `borrow`: 大きい定跡の木を借りる

大きい定跡 (YaneuraOu のペタショック定跡など) から、どの手を伸ばすかだけを借ります。値は自分のエンジンの読みで付け、大きい定跡の値は手を選ぶ当たりにだけ使います (登録する値には混ぜません)。読むべき出口が最初から全部分かるので、`tree serve` の並列を使い切って同時に読めます。

1. `borrow cut`: 根ごとに部分木を切り出し、読む局面 (出口と確認点) とキューの行を書く。
2. `tree enqueue --from <cut>/targets.jsonl` → `tree serve` で読む (同じ台帳を他の木と使い回せる)。
3. `borrow value`: 台帳の読みを部分木で根まで伝播し、元データに登録する行を作る。

### 切り出し

```bash
book_mine borrow cut --book big.db --roots roots.jsonl --out cut1 \
  [--own-diff 30] [--opp-diff 100] [--opp-top 2] [--max-plies 20] [--max-positions 20000] \
  [--check-every 2] [--master moves.tsv [--master-exit use|read]] [--extra marks.tsv] [--tag ext] [--priority 100]
```

| オプション | 既定 | 説明 |
|---|---:|---|
| `--book` | 必須 | 大きい定跡。テキスト `.db` か YaneuraOu のバイナリ定跡 `.ybb` (ファイル先頭で見分ける)。丸読みせず、ファイル上で二分探索して引く (`rshogi-book` の BookOnTheFly)。`.db` は `sfen` 行が手数を除いた SFEN のバイト順に並んでいる必要があり、並んでいなければ開くときに止まる (並べ替えた写しを渡す)。`.ybb` は並べ替え不要。count を持たず、depth は持つ版 (flags の bit0) と持たない版があるが、切り出しは値しか使わないので結果は `.db` と同じ。ファイルは読むだけ |
| `--roots` | 必須 | 根の一覧 (JSON Lines)。1 行に `id` (`[A-Za-z0-9_-]`)・`root` (`sfen ...` か `startpos moves ...`)・`own` (`b` / `w`、根の手番と同じ)。他の項目は読まないので、`tree` のキューの行もそのまま渡せる |
| `--out` | 必須 | 出力先。まだ無い dir |
| `--own-diff` | 30 | 自分の手番: 大きい定跡の最善の値からこの差以内の手を伸ばす (YaneuraOu の BookEvalDiff に当たる) |
| `--opp-diff` / `--opp-top` | 100 / 2 | 相手の手番: 値の上位 `--opp-top` 手と、最善からの差が `--opp-diff` 以内の手の和を伸ばす |
| `--max-plies` | 20 | 根からの手数の上限。この手数の局面は出口 |
| `--max-positions` | 20000 | 根ごとの局面数の上限。伸ばすとこれを超える局面は出口 |
| `--check-every` | 2 | 確認点の間隔。自分の手番の途中の局面を、自分の手のこの数おき (根を含む) に読む。0 で読まない |
| `--master` | なし | 定跡の元データの表。ここにある局面 (根を除く) を出口にする |
| `--master-exit` | `use` | `use`: 元データの値を使い、読まない (自分の手番は main、相手の手番は値の分かる手の 1 位。値は `master audit` と同じ伝播の値)。値が分からない局面は読む。`read`: 止めて読む |
| `--extra` | なし | 足す手 (TSV、見出しに `key`・`move`。他の列は読まない)。`borrow value` の `marks.tsv` をそのまま渡せる。どの根でも、部分木の途中の自分の手番の局面に着いたら足す |
| `--tag` / `--priority` | `ext` / 100 | キューの行の id (`<根の id>_<tag>_<b\|w>`) と priority |

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
| `cut.json` | 大きい定跡の名前・大きさ・SHA-256、条件、根の一覧 (`borrow value` が読む) |

同じ入力なら同じ出力になります。読む局面の組を変えて (条件や `--extra` を変えて) 切り直したときは `--tag` を変えてください。キューは id ごとに `root` を固定するので、同じ id で組が変わると拒否されることがあります。

### 読み

`tree serve` は段 0 の行の根 (`root` と `roots`) を読むだけで伸ばしません。読みの条件 (指紋) が同じなら、他の木や別の切り出しの読みも同じ台帳で使い回します。定跡の値をそろえるため、条件は BookMiner の木と同じにします (`--nodes 1000000000 --multipv 3 --hash 1024`、`DrawValueBlack=-70` / `DrawValueWhite=70`、Threads は 1 に固定)。

```bash
receipt=$(book_mine tree enqueue --state "$STATE" --from cut1/targets.jsonl)
book_mine tree wait --state "$STATE" --receipt "$receipt"
```

serve の 1 周ごとの処理 (0.1 秒ごと) は、前の周から読みが来ていない木ではまだ読んでいない局面の数に比例します。`progress.json` の集計 (1 秒に 1 回まで) は、終わっていない木の局面数に比例します (2 万局面で数 ms)。

### 値付けと登録

```bash
book_mine borrow value --cut cut1 --journal journal-copy.jsonl --master moves.tsv \
  --out value1 --since 20261101-ext [--note "..."] [--alt-gap 40] [--line 40] \
  [--draw-value-black -70] [--draw-value-white 70] [--fingerprint <指紋>] [--apply]
```

台帳は共有 lock で読むので、serve が書いている台帳は複製して渡します。台帳からは読む局面の読みだけを残します (他の局面も行の検査と重複の矛盾の検査はする)。読む局面のどれかの読みが台帳に無ければ、何も書かずに止まります (足りない数と例を出す)。読む局面が 1 つも無ければ台帳は読みません。

伝播:

- 部分木を `.db` にして `book_backprop --merge replace` と同じ後退解析で解きます。切り出しと同じく、先後反転した局面へは辺を張りません。先手を自分として解く回は `--draw-value-black`、後手は `--draw-value-white` を千日手の値にします (部分木の中の循環)。
- 出口の手は読みの MultiPV の手と値です。0 行の読みは `bestmove resign` なら −30000、宣言勝ちなら勝ちの値です (BookMiner の木の伝播は resign の葉を 0 にするので、詰みの絡む根では 1 位が食い違うことがあります)。読みの手が部分木の局面に着けば、その局面の値が伝わります。元データの値を使う出口は、その値だけを持ちます。
- 途中の局面の手は部分木の手だけです。確認点の読みは値に混ぜません。
- 同じ値の手は、`book_backprop` の同値手の補正 (勝ちは出口へ短く届く手を残し、遠回りの手を 1 下げる) を受けます。

確認点の印: 確認点の読みで、部分木の候補の外の手の値が、候補の伝播後の最善より `--line` 以上良ければ印を付けます (BookMiner の「伸ばしていない手が上回る」に当たる)。印の付いた局面は、どの根から着いても登録せず、`marks.tsv` に出します。その手を足して伸ばし直すときは、`--extra value1/marks.tsv` を付けて新しい `--tag` で切り直し、足りない局面を読ませてから値付けをやり直します。

登録の規則は `tree register` と同じです (`--alt-gap`、読み由来でない main は替えずに食い違いを一覧に出す、`record` の手は足さない)。違いは次のとおりです。

- 登録するのは自分の手番の局面だけです。伸ばした局面は伝播後の値、出口は読みの値で並べます。元データで止めた出口と印の付いた局面は登録しません。
- 足す行は `source=ext`、`lineage=book:<大きい定跡のファイル名>@<内容の sha256 の先頭 8 文字>`、`nodes` は台帳の読みのノード数です。`ext` の行は読み由来として、後の `tree register` / `borrow value` で main を替え、`evals` を更新してよい行に数えます。
- 同じ局面が複数の根にあるときは、その局面から先の手数の余り (`--max-plies` − 段) の大きい根を使います。

`--apply` の前に、登録後の元データを `master audit` と同じ伝播に通し (通らなければ止まる)、点検の数 (main が 1 位から `--line` 以上負ける局面) の前後を出します。`--apply` を省くと元データは変えません。

出力 (`--out`、まだ無い dir):

| ファイル | 内容 |
|---|---|
| `roots.tsv` | 根ごとの伝播後の 1 位・2 位とその値、先手から見た 1 位の値、印の数。BookMiner の木の 1 位と比べるときに使う |
| `values.tsv` | 全局面の手の伝播後の値。`id key ply level side turn kind move value value_black expanded book_value` (`value` は手番の側、`value_black` は先手から見た値、`expanded` は部分木の手なら 1) |
| `marks.tsv` | 印。`id key ply side move cp best best_value gap` |
| `conflicts.tsv` | 食い違いの一覧 (`tree register` と同じ列で、最後の列は根の id) |

注意:

- 相手の手も部分木の手だけで見るので、部分木の外の相手の手で戻る循環 (先手の千日手) は、この伝播では見つかりません (`master audit` と同じ)。
- 元データの局面で止めた出口の値は、`value` を実行したときの元データから求めます。切り出しの後に元データが変わり、値が分からなくなった局面があれば止まります。
