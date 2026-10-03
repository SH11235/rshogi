# book_backprop

`book_backprop` は YANEURAOU-DB2016 テキスト定跡 `.db` の候補手評価値を、book 内の子局面から negamax で親方向へ逆伝播するツールです。既定では `count` / `ponder` / `depth` / 局面構造を保持し、`value` だけを更新します。opt-in の `--yo-compat` では YaneuraOu peta_shock の規則で `value` と `depth` を更新します。

## 使い方

```bash
cargo run -p tools --release --bin book_backprop -- \
  --book in.db \
  --out out.db \
  --draw-value 0 \
  --merge min \
  --report report.md \
  --max-iters 1000
```

必須オプション:

| オプション | 説明 |
|---|---|
| `--book <PATH>` | 入力 `.db` |
| `--out <PATH>` | 出力 `.db` |

任意オプション:

| オプション | 既定値 | 説明 |
|---|---:|---|
| `--draw-value <CP>` | `0` | 循環 SCC の千日手値。手番側視点 |
| `--merge <MODE>` | `min` | book 内子局面からの伝播値と既存ラベル値の合成。`min` または `replace` |
| `--yo-compat` | off | YaneuraOu peta_shock 互換の伝播。`--merge replace` 必須 |
| `--report <PATH>` | なし | Markdown レポートの出力先 |
| `--max-iters <N>` | `1000` | 非自明 SCC の値反復ガード。到達時はエラー終了 |
| `--skip-unusable-moves` | off | 非合法手と `none` 行を局面の best (と best 変化の集計) から除く。行自体は値を変えずに書き出す |
| `--skip-unsearched-children` | off | 候補手が 1 件以上あり全行の `depth=0` の子局面を、直接キー・反転キーとも book 外として扱う。親の手は `merge` によらず元の値を保持する。`value` は判定に使わず、候補手の無い局面は除外しない |

## 伝播規則

局面キーは SFEN の ply を除いた board / turn / hands の 3 フィールドです。出力の `sfen` 行には入力側の ply を残し、同一キーが複数ある場合は最小 ply の行を代表として使います。

各 book 手は次の規則で更新します。

| 子局面 | 伝播値 |
|---|---|
| book 内に直接存在 | `propagated = -best(子)` |
| 直接 miss だが先後反転キーが book 内に存在 | `propagated = -best(反転子)` |
| book 外 | 既存 `value` を維持 |
| 非合法手 | stderr に警告し、既存 `value` を維持 |

歩・香の最終段、桂の最終二段への打ち・不成も非合法手として除外します。子局面がbook内に存在していても、その手の既存 `value` を維持します。合法な不成は通常の手と同様に逆伝播します。

`best(N)` は局面 `N` の候補手 `value` の最大値です。既定では非合法手と `none` 行の既存 `value` も含みます。`--skip-unusable-moves` を指定するとこれらを除き、合法手だけの最大値にします (合法手が無い局面は `--draw-value`)。book 内子局面が見つかった手の最終値は `--merge` で決まります。

| mode | 更新 |
|---|---|
| `replace` | `value = propagated` |
| `min` | `value = min(既存ラベル値, propagated)` |

既定の `min` は値を下げる方向にだけ伝播します。probe 時の相手は自分の book 内候補に制限されないため、子局面での相手の取り分は、既存ラベル値が持つ制限なし探索の見積りと book 内 best の大きい方以上です。したがって親手の値は、既存ラベル値と `-best(子)` の小さい方を上界として扱います。

出力は決定的で、局面は SFEN key 昇順、手は `count` 降順から USI 昇順で書き出します。

## 循環 SCC

ply を除いた局面グラフは千日手相当の循環を持つことがあります。`book_backprop` は SCC を縮約し、縮約 DAG を子側から処理します。

非自明 SCC では、SCC 外への手は確定済みの `-best(子)` とし、SCC 内に留まる手は `--draw-value` を下界として値反復します。`--merge min` ではこの伝播値をさらに既存ラベル値との `min` で合成するため、book 内辺の値は単調非増加です。値集合は既存の葉値、SCC 外の確定値、`draw-value` とその negamax 合成に限られるため有限で、不動点に達すると停止します。`--max-iters` に達した場合は実装バグまたは入力条件の見直しが必要な状態としてエラー終了します。

## YaneuraOu peta_shock 互換モード

```bash
cargo run -p tools --release --bin book_backprop -- \
  --book in.db --out peta.db --yo-compat --merge replace --report peta.md
```

`--yo-compat` は `source/book/makebook2025.cpp` の leaf depth 初期化、全合法手からの合流補完、`ValueDepth` 比較、const node の除去、通常循環の初期化、連続王手ループの抽出と DFS、順次更新による伝播、出力時の補正を移植したモードです。指定しなければ従来の SCC negamax の動作・出力を維持します。`--merge min` との併用はエラーです。

`--yo-compat` は `book_mine run` の周回には使えません (`run` に同名の option は無く、使う予定もありません)。YO は入力の depth を捨てて「葉からの距離」を depth に書くため、探索済みで全候補が value 0 の局面が `value=0 depth=0` で出力され、`book_mine` の「未探索」の印と区別できなくなります。周回は従来の逆伝播 (`--skip-unsearched-children` 付き) で行い、最終の db を YO と同じ peta shock 化したいときだけ `book_backprop --yo-compat --merge replace` を単独で実行してください。

- 入力の探索 depth は無視し、全ての葉の depth を **0** から始めます。YO の `.db` / `.ybb` 両方の `read_book` と同じ規則です。book 外の葉は `(入力 value, 0)`、子局面が book 内なら `(-best.value, min(best.depth + 1, 9999))`。出力 depth は探索深さではなく、後退解析で選ばれた葉からの距離です。best 比較・同値補正にもこの距離を使います。
- YO の `convergence_check()` と同じく全合法手を調べ、入力に登録されていなくても book 内の既知局面に至る手を補完します。補完した辺も best・循環判定・伝播に参加し、出力されます。
- best は value の大きい手。同値なら非負は短い depth、負は長い depth を優先します。ただし depth が `PERPETUAL_CHECK=9997` の手は同値の他の depth より劣後します。`PERPETUAL_CHECKED=9998`、無限 depth は `BOOK_DEPTH_MAX=9999` です。
- 全ての手が葉または const node に至る局面を const node として確定し、残りを両手番とも `(0, 9999)` で初期化します。残りには循環へ至る祖先も含まれます。
- SFEN を `Position` で読み、手番側の `checkers()` が空でない局面を候補にします。2 手先に候補のない局面を反復除外し、候補間の中間局面を加えて check-loop 集合を作ります。この集合は通常パスで更新せず、経路上の再訪問を検知する DFS で評価します。
- 内部の node 値は YO と同じ「親手番視点」です。check-loop の初期値は王手されていれば `(-MATE, 9999)`、そうでなければ `(MATE, 9999)`。DFS 再訪問時はそれぞれ `(-MATE, 9997)` / `(MATE, 9998)` を返します。王手している側が負けとなる符号です。
- `BOOK_MAX_PLY=256` は参照実装と同値です。const 除去・候補除外は最大 256 回、伝播は最大 356 回。通常パスで更新後の depth が 256 を超えたら 9999 にします。YO と同じく通常パスの更新数が 0 なら、その回の DFS 後に停止します（DFS の更新数は停止判定に含めません）。上限到達時はその時点の値を書き出します。`--draw-value` と `--max-iters` はこのアルゴリズムには使いません（共通の `max-iters >= 1` 検査は残ります）。

出力時には、王手されている check-loop node に入る手の depth を 9997 にします。ValueDepth 順の best と同値で depth が異なる非 best 手は、**書き出す value を 1 下げます**。葉もこの補正の対象です。補正値は親への伝播には戻しません。迂回や連続王手ループを同値候補として選び続けることを避けるため、出力 book の PV を辿ると値が 1 ずれる場合があります。

データモデル・出力形式の意図的な違い:

- YO の `VALUE_MATE` には rshogi の既存エンジン定数 `Value::MATE`（32000）を対応させます。`book_mine` が探索結果をラベル化する上限 30000 とは別です。空の候補集合は YO の best 初期値 `-32767`（親視点では `32767`）を用います。
- 全局面を先手化せず、従来の「直接キーを優先、miss なら反転キー」のグラフを使います。`via_flip` 辺も同じ node index を辿り、王手判定・循環・符号反転に追加の色変換は不要です。両向きのキーが入力に存在する場合に新たな統合はしません。局面の走査は key 順で決定的です。
- `--skip-unusable-moves` は非合法手・`none` を best と出力補正から除き、入力の value/depth を含めて行を保持します。`--skip-unsearched-children` は **depth を 0 に初期化する前の入力 depth** で辺を除き、登録済みの手を葉 `(入力 value, 0)` として扱います（出力時の同値補正は適用）。未探索の子に至る未登録手は補完しません。YO 自体にはこの 2 オプションはなく、実機との parity は両方 off で検証します。
- 入力手の `count` / `ponder` と SFEN を保持し、補完手は `count=0` / `ponder=none` で出力します。writer の既存規約である **count 降順→USI 昇順**を使います。ValueDepth 比較は best 選択と value 補正に使い、ファイル順には使いません。YO の shrink / fast / `.ybb` 出力は対象外です。

### 実 YaneuraOu との差分検証

`tests/book_backprop_yo.rs` の ignored test は外部の入力 DB と YO 出力を読み、Rust の出力を一時ファイルに生成して照合します。比較キーは `(SFEN の先頭 3 フィールド, USI 手)` で、行順・ply・count・ponder は比較しません。手集合の一致と value/depth の完全一致を検査し、総手数・value 一致数・depth 一致数・両方の一致数を表示します。大きい DB は repo に追加しません。

PowerShell での再現例（worktree を作業ディレクトリにする）:

```powershell
# target/yo-diff/in.db に検証入力を置く。
$yoBookDir = (Resolve-Path target/yo-diff).Path
@"
usi
setoption name BookDir value $yoBookDir
setoption name BookFile value no_book
setoption name FlippedBook value true
makebook peta_shock in.db out_yo.db
quit
"@ | & /path/to/YO-MATERIAL.exe
# NNUE 版を使う場合は、そのモデルの EvalDir / FV_SCALE / isready 設定も前置する。
$env:YO_BOOK_INPUT = Join-Path $yoBookDir in.db
$env:YO_BOOK_REFERENCE = Join-Path $yoBookDir out_yo.db
cargo test -p tools --release --test book_backprop_yo -j 8 -- --ignored --nocapture
```

BookDir は絶対パスにします（YO が起動時に作業ディレクトリを変更する場合にも対応）。比較の前提は YO が元の向きの SFEN を出力することです。別の出力設定で向きが変わる場合は、局面と手の両方を同じ向きに揃えてから比較してください。

通常実行される `real_yaneuraou_leaf_distance_and_convergence` は、実エンジン `YaneuraOu NNUE 9.80git 64AVX512VNNI TOURNAMENT` の `makebook peta_shock` で生成した小さい参照ペア `tests/fixtures/book_backprop_yo_{in,expected}.db` を使います。3 局面・入力 4 手から 1 手が補完され、入力 depth の破棄と距離 0/1 の同値補正を検査します。この入力を上記の `in.db` として使えば参照を再生成できます。

## レポート

`--report` を指定すると Markdown で以下を出力します。

| 項目 | 内容 |
|---|---|
| Summary | merge mode、ノード数、手数、更新手数、book 内辺数、flip 合流辺数、非合法手数 |
| Value deltas | `|Δ|` の p50 / p90 / max とヒストグラム |
| Propagation depth | 縮約 DAG で葉から何段伝播したかの分布 |
| SCC | 非自明 SCC 数、最大サイズ、draw-value になった SCC 内手数、値反復回数 |
| Top changed nodes | 旧 best と新 best の差が大きい上位 20 局面 |

互換モードでは Summary に check-loop nodes と cycle nodes（const 除去後の非 const ノード数）を追加します。SCC の反復回数欄には全局面伝播の反復回数を 1 件記録し、draw 手数は値 0 の SCC 内手数です。他の集計は既存形式を保ち、値の変化は出力補正後で集計します。
