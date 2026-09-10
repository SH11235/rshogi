# tournament — 並列トーナメント・SPRT 検定

複数エンジン間の総当たり (round-robin) 対局、base-vs-N 対局、または SPRT（逐次確率比検定）を並列実行するツール。
対局ログは analyze_selfplay 互換の JSONL 形式で出力される。

## ビルド

```bash
cargo build -p tools --bin tournament --release
```

## モード

| モード | 概要 | 主なオプション |
|--------|------|---------------|
| **総当たり** | N エンジン間の全 C(N,2) ペアを対局 | `--engine` × N |
| **base-vs-N** | 基準 1 体 vs 他 N 体のみ対局 | `--base-label` |
| **SPRT** | A/B 2 エンジンで有意差を逐次判定、境界到達で自動停止 | `--sprt` |

## クイックスタート

### 総当たり（2 エンジン、各方向 100 局 = 合計 200 局）

```bash
./target/release/tournament \
  --engine target/release/rshogi-usi-v1 --engine-label v1 \
  --engine target/release/rshogi-usi-v2 --engine-label v2 \
  --games 100 --byoyomi 1000 --hash-mb 256 --threads 1 --concurrency 8 \
  --seed 42 \
  --startpos-file data/startpos/start_sfens_ply32.txt \
  --out-dir runs/selfplay/$(date +%Y%m%d_%H%M%S)-v1-vs-v2
```

### SPRT（有意差の早期判定）

H0=0、H1=+5 nElo、名目 α=β=0.05 の SPRT を行う例:
差が明確なら早期に判定終了し、差が微妙なら `--games` 上限まで対局を続ける。

- `--sprt-nelo0 0`: H0 = 差なし（帰無仮説）
- `--sprt-nelo1 5`: H1 の正規化 Elo = +5（対立仮説）
- `--sprt-alpha 0.05`: 名目第一種過誤率 5%
- `--sprt-beta 0.05`: 名目第二種過誤率 5%
- `--games 5000`: SPRT が境界に達しなかった場合の対局数上限（保険）

`--sprt-base-label` を省略すると `--base-label` の値が自動で使われる。

```bash
./target/release/tournament \
  --engine /path/to/base-engine --engine-label base \
  --engine /path/to/test-engine --engine-label test \
  --games 5000 --byoyomi 1000 --hash-mb 256 --threads 1 --concurrency 16 \
  --seed 42 \
  --startpos-file data/startpos/start_sfens_ply32.txt \
  --base-label base \
  --sprt --sprt-test-label test \
  --sprt-nelo0 0 --sprt-nelo1 5 --sprt-alpha 0.05 --sprt-beta 0.05 \
  --out-dir runs/selfplay/$(date +%Y%m%d_%H%M%S)-sprt-base-vs-test
```

## CLI オプション

### 対局制御

| オプション | デフォルト | 説明 |
|-----------|-----------|------|
| `--engine PATH` | (必須、2 つ以上) | エンジンバイナリパス。`--engine` の数だけエンジン登録される |
| `--engine-label LABEL` | パスから自動生成 | エンジンラベル（`--engine` と同数・同順で指定）。同一パスを複数回指定する場合は区別のため必須 |
| `--games N` | 100 | 各方向の対局数（双方向で 2×N 局/ペア） |
| `--max-moves N` | 512 | 1 局の最大手数（到達で引分）。真の長手数局のみ（千日手は自動終局） |
| `--adjudicate-resign "movecount=3,score=600"` | off | 同一側の劣勢評価の連続による投了裁定 |
| `--adjudicate-draw "movenumber=34,movecount=8,score=20"` | off | 両側を通じた均衡評価の連続による引分裁定。手数は ply |
| `--concurrency N` | 1 | 並列対局数。1 対局は手番制で約 1 CPU スレッド消費 |
| `--report-interval N` | 10 | N 局ごとに進捗を表示 |

### エンジンに渡す対局履歴

各探索には `position sfen <対局開始局面> moves <対局開始後の全指し手>` を送る。
初手は `moves` を省略する。毎手の現在局面だけを SFEN で送っていた旧 driver と異なり、
エンジンは現在局面に加えて過去の反復・連続王手を復元し、探索で利用できる。

履歴の基点は、千日手裁定と同じく開始局面ファイルの `moves` を適用した後の局面。
開始局面ファイルの手順は基点 SFEN に反映済みなので、送信する手順に重ねて追加しない。
生の SFEN しかない場合も、それ以前の履歴は復元しない。
pass 使用時は基点の `passrights` を `moves` より前に送り、手順再生で権利を消費する。

履歴は各局・再試行ごとに作り直し、両エンジンに同じ手順を渡す。これは TT・EvalHash・
探索履歴テーブルなどのキャッシュを対局間で保持するかとは別の処理。
`resign` / `win` / 不正手 / 時間切れ応答は手順に加えず、合法手だけを正規化して記録する。
エンジンの指し手と開始局面ファイルの手順は完全合法手集合で検査する。
move 行の `sfen_before` は従来どおり着手直前の現在局面を表す。

この変更は探索条件を変えるため、旧 driver の対局結果と混ぜず、実験記録に driver の
コミットを残す。共有対局ループを使う `spsa` にも適用する。

### ルールによる自動終局

千日手は常時有効。同一局面（盤・手番・持駒、pass 使用時は両側の pass 権も）が
4 回出現した時点で引分にする。開始局面を 1 回目と数え、開始局面ファイルの
手順を適用した後の局面から履歴を保持する。1 回目から連続して王手をかけていた
側があれば、その側の反則負け（連続王手の千日手）になる。

評価値による裁定は **既定 off**。chess の閾値（600cp / 20cp 等）は将棋で未較正であり、
有効化は保存済み棋譜で誤裁定率を測ってから行う。

- `--adjudicate-resign "movecount=3,score=600"`: 着手側の自己視点評価が -600cp 以下の
  着手を、同一側で 3 回連続するとその側の負け。相手の着手はカウントに含めない。
  負の mate score は劣勢として数え、正の mate score・条件外の cp・評価値欠落でリセットする。
- `--adjudicate-draw "movenumber=34,movecount=8,score=20"`: 両側を通じて絶対値 20cp 以下が
  8 手連続し、対局内の手数が 34 手以上なら引分。`movenumber` と `movecount` はいずれも
  将棋の手数（ply）であり、chess の full move ではない（fastchess の `movecount=8` は
  16 ply 相当）。連続回数は 34 手未満でも数え、mate score・条件外の cp・
  評価値欠落でリセットする。

`lowerbound` / `upperbound` 付きの score は確定評価ではないため、投了・引分裁定の
両方で評価値欠落と同様に連続回数をリセットする。move 行の `eval.score_bound` に
`lowerbound` / `upperbound` を記録する（確定評価と旧ログでは省略）。

各フラグの key はすべて必須で、`,` または空白区切りに対応する（値全体を引用符で囲む）。
未知・重複 key はエラー。`movecount` は 1 以上、`score` は非負の i32、`movenumber` は
非負の u32 とする。有効な設定は meta の `settings.adjudicate_resign` /
`settings.adjudicate_draw` に記録し、無効な設定は省略する。

合法手の適用後に千日手 → 投了裁定 → 引分裁定の順に判定し、終局手も move 行に記録する。
千日手・引分裁定・最大手数による引分は通常の引分として WLD / pentanomial に算入する。
連続王手の千日手と投了裁定には勝者を記録する。

結果 JSONL の `reason` は次のとおり。

| reason | 意味 |
|--------|------|
| `resign` | エンジンの投了 |
| `win` | エンジンの勝ち宣言 |
| `sennichite` | 千日手による引分 |
| `sennichite_perpetual_check` | 連続王手をかけた側の反則負け |
| `adjudication_resign` | 評価値による投了裁定 |
| `adjudication_draw` | 評価値による引分裁定 |
| `max_moves` | 最大手数到達による引分 |
| `timeout` | 時間切れ負け |
| `illegal_move` | 不正な指し手による負け |
| `no_bestmove` | bestmove 欠落による負け |
| `error: ...` | ワーカー・エンジン等のエラー（再対局対象） |

### 時間・エンジン設定

時間管理オプション (`--byoyomi`, `--btime`/`--binc`, `--depth`, `--nodes`, `--engine-nodes`) のいずれかの指定が必須。
すべて省略するとエラーになる。

| オプション | デフォルト | 説明 |
|-----------|-----------|------|
| `--byoyomi MS` | — | 秒読み（ミリ秒）。`--btime`/`--binc` と排他 |
| `--btime MS` | 0 | 持ち時間（ミリ秒、フィッシャー時計）。`--byoyomi` と排他 |
| `--binc MS` | 0 | 1 手ごとの加算時間（ミリ秒、フィッシャー時計） |
| `--depth N` | — | 深さ制限（`go depth N` を送出） |
| `--nodes N` | — | ノード制限（`go nodes N` を送出）。全エンジン共通 |
| `--engine-nodes "IDX:NODES"` | — | エンジン個別の固定ノード数（0 始まりインデックス、複数指定可）。指定したエンジンは global `--nodes` を上書きし、未指定のエンジンは `--nodes` にフォールバックする。エンジンごとに異なるノード数を割り当てたい場合（ノード正規化対局・ハンディキャップ対局など）に使う |
| `--threads N` | 1 | Threads USI オプション |
| `--hash-mb N` | 256 | USI_Hash（MiB） |
| `--usi-option "Name=Value"` | — | 全エンジン共通の USI オプション（複数指定可） |
| `--engine-usi-option "IDX:Name=Value"` | — | エンジン個別の USI オプション（0 始まりインデックス）。共通 `--usi-option` にマージされ、同じキーは engine 個別指定が上書きする |
| `--strict-engine-usi-option` | false | `--engine-usi-option` を指定したエンジンでは共通 `--usi-option` を完全に置換する（旧挙動） |
| `--engine-params-file "IDX:FILE"` | — | SPSA `.params` ファイルから USI オプションを読み込む。`--engine-usi-option` と併用可（マージ） |

### 開始局面

| オプション | 説明 |
|-----------|------|
| `--startpos-file FILE` | 開始局面ファイル（1 行 1 局面、USI position 形式）。省略時は平手初期局面 1 局面のみ使用（全対局が同一局面になるため棋力評価には不向き）。**棋力評価では必須** |
| `--seed N` | 開始局面選択の seed。同じ seed・エンジン構成（順序を含む）・開始局面ファイルでは、各 matchup の同じペア通番に同じ局面を割り当てる。`control.json` で対局数を途中変更しても局面列は変わらない。省略時は entropy から生成し、起動ログへ表示する |

### 出力

| オプション | 説明 |
|-----------|------|
| `--out-dir DIR` | (必須) 出力ディレクトリ（既存の対局出力があれば拒否）。`pair-{i}-{j}.jsonl` と、開始局面選択の seed を含む `meta.json` が生成される |

### base-vs-N モード

| オプション | 説明 |
|-----------|------|
| `--base-label LABEL` | 基準エンジンのラベル。このエンジンと他全エンジンのペアのみ対局する |

### SPRT モード

| オプション | デフォルト | 説明 |
|-----------|-----------|------|
| `--sprt` | — | SPRT を有効化。境界到達で新規対局の供給を停止し、進行中ゲームを完了待ちで drain |
| `--sprt-test-label LABEL` | (必須) | H1 側（challenger）のエンジンラベル。正の nelo = このエンジンが強い |
| `--sprt-base-label LABEL` | `--base-label` | H0 側（base）のエンジンラベル。省略時は `--base-label` を流用 |
| `--sprt-nelo0 F` | 0.0 | H0 仮説の正規化 Elo（通常 0 = 差なし） |
| `--sprt-nelo1 F` | 5.0 | H1 仮説の正規化 Elo（検出したい最小効果量） |
| `--sprt-alpha F` | 0.05 | 第一種過誤率 α（H0 が真なのに H1 を採択する確率の上限） |
| `--sprt-beta F` | 0.05 | 第二種過誤率 β（H1 が真なのに H0 を採択する確率の上限） |
| `--sprt-report-interval N` | 10 | ペア何単位ごとに SPRT レポートを出力 |

#### SPRT 出力例

```
[SPRT pair=1014 | test vs base] LLR=+2.950 (bounds -2.94..+2.94)  nelo=+38.05 ± 15.12  penta=[184, 15, 508, 17, 290]  state=accept_h1
[SPRT] terminal decision reached; draining 15 in-flight game(s)...

=== SPRT Summary (test vs base) ===
bounds: LLR ∈ [-2.944, +2.944]  (alpha=0.05, beta=0.05)
nelo hypotheses: H0=+0.0  H1=+5.0
stopped_at:  pairs=1014, LLR=+2.950, decision=accept_h1
             nelo=+38.05 ± 15.12  penta=[184, 15, 508, 17, 290]
final:       pairs=1022, LLR=+2.889, decision=running
             nelo=+37.02 ± 15.06  penta=[187, 15, 512, 17, 291]
================================
```

`stopped_at` は境界到達時点のスナップショット。`final` は drain 完了後の最終値で、
進行中ゲームの結果を含むため LLR・ペア数が微差する。判定は `stopped_at` の時点で
確定済みのため、`final` の `decision` が `running` と表示されることがあるが、
判定結果が覆ることはない。

#### game error の再対局

同一開始局面を先後入れ替えた 2 局のどちらかが engine 起動・通信・worker panic
などの game error になった場合、その世代の 2 局は統計から除外される。同じ
`pair_index`、開始局面、先後割当のまま最大 2 回再対局し、再対局は目標局数を消化しない。
JSONL の result 行には初回 0 の `attempt` が記録される。
通信・`usinewgame`・worker panic の error 後はその worker を退役させ、再対局は新しく
起動した engine process で行う。

最終サマリと `meta.json` の `error_pairs`、`retried_pairs`、`exhausted_pairs` で状況を
確認できる。2 回の再試行後も error を含むペアがあれば `invalid: true` となり、SPRT
サマリにも警告が出る。これは統計的敗北ではなくインフラ障害を示し、終了コードは変わらない。

#### SPRT 用語

| 用語 | 意味 |
|------|------|
| **LLR** (Log-Likelihood Ratio) | 対数尤度比。H1 と H0 のどちらがデータをよく説明するかを示すスコア |
| **nelo** (Normalized Elo) | pentanomial ペアスコアの分散で正規化した Elo。引分率に依存しない棋力差推定 |
| **penta** (Pentanomial) | 2 局ペア（同一開始局面・先後入替）の結果を 5 カテゴリ [LL, LD, DD/WL, WD, WW] に分類した分布。中央の DD/WL は「両局引き分け」または「test の勝ち/負けが 1 局ずつ」でペアスコアが同値 (0.5) になるため同カテゴリ |
| **accept_h1** | LLR が上側境界に到達し、H1 を採択 |
| **accept_h0** | LLR が下側境界に到達し、H0 を採択 |

#### LLR の計算と保存ログの再計算

LLR は先後ペアの5カテゴリ件数を使う。ゼロのカテゴリだけに0.001件を補い、
補正後の合計で確率を正規化する。LLR の乗数も事前件数を含む合計を使う。
実際のペア数・WLD・表示用 nElo は観測件数のままで、架空の対局は追加しない。
pentanomial の仮説変換には `sqrt(2)` を使う。

これは [fishtest の固定版の件数補正](https://github.com/official-stockfish/fishtest/blob/93fe81eb8256b870ab759ec4470f6251fe985827/server/fishtest/stats/LLRcalc.py#L197-L212)
に合わせた算術処理である。MLE/ITP solver の収束条件は異なり、全入力での同値性や
連続監視時の誤判定率を較正したことは意味しない。H1/H0 採択は仮説間の証拠に基づく
判定であり、効果量の下限・上限の保証ではない。

確率自体を0.001以上へ切り上げていた版からは、LLR と境界判定が変わる場合がある。
保存ログを調べる際は、当時の実行版・仮説・ペア集計・停止時点を保持し、
同じ入力の旧新 LLR を比較する。終端の集計だけでは最初の境界通過時点を再現できない。
再計算値は診断として記録し、過去の受理を一括で無効にしたり、仮説を後から選び直したりしない。

### 外部エンジンとの対局

rshogi と YaneuraOu のようにエンジンごとに USI オプションが異なる場合、`--engine-usi-option` で個別指定する。
共通 `--usi-option` を完全に置換したい場合は `--strict-engine-usi-option` を併用する:

```bash
./target/release/tournament \
  --engine target/release/rshogi-usi --engine-label rshogi \
  --engine /path/to/YaneuraOu-binary --engine-label yaneuraou \
  --strict-engine-usi-option \
  --engine-usi-option "0:EvalFile=eval/model.bin" \
  --engine-usi-option "1:EvalDir=/path/to/eval_dir" \
  --engine-usi-option "1:FV_SCALE=28" \
  --engine-usi-option "1:BookFile=no_book" \
  --games 100 --byoyomi 1000 --concurrency 8 \
  --seed 42 \
  --out-dir runs/selfplay/$(date +%Y%m%d_%H%M%S)-rshogi-vs-yo
```

## 結果集計 — analyze_selfplay

tournament の出力 JSONL を読み込み、勝率・Elo 差・NPS 等を集計する:

```bash
./target/release/analyze_selfplay runs/selfplay/{DIR}/*.jsonl
```

### 出力内容

- **エンジン別 勝敗**（先後合算・先後別勝率）
- **直接対決**（Elo 差 ± CI、nElo 差 ± CI）
  - Elo 差: trinomial（1 局ごとの WDL）ベース
  - nElo: pentanomial（ペア単位）ベース。開始局面・先後の交絡を除去した、より正確な推定
  - 通常はエンジンラベルの辞書順で左側の視点。`--sprt` 時は SPRT
    対象ペアを test vs base の順にし、Elo/nElo の符号も test 視点に揃える
- **追加統計**（平均手数、先手勝率、error 局・再試行関連件数、NPS、depth、seldepth 等）

### SPRT post-hoc 判定

完了済みログから SPRT 判定を再現・再検討できる:

```bash
./target/release/analyze_selfplay \
  runs/selfplay/{DIR}/*.jsonl \
  --sprt --sprt-base-label base --sprt-test-label test \
  --sprt-nelo0 0 --sprt-nelo1 5
```

- `--sprt`: SPRT 判定モードを有効化
- `--sprt-base-label` / `--sprt-test-label`: pentanomial の集計方向を指定する（どちらが test 側か）。
  nelo の符号は test 視点で決まる。省略時は tournament が記録した SPRT meta / base
  label 等から自動推定し、推定根拠を標準エラー出力に表示する。自動推定の役割が意図と
  異なる場合は、tournament 実行時の `--engine-label` と一致する値を明示する
- `--sprt-nelo0` / `--sprt-nelo1`: tournament 実行時と異なる閾値を指定して
  「この閾値なら何局で打ち切れたか」を事後検証できる

通常の集計出力の末尾に SPRT レポートが追加される。

## 実行中の目標局数変更と先後交換ペア

control.json の target_games を変更した場合も、片側を発行済みのカードは
先後交換のもう 1 局を発行してから別カードへ移ります。複数カードで目標を増やしても
pair_index と slot 0/1 の対応を保ち、完成ペアの集計や失敗時の同条件再対局へ渡します。
目標を減らした場合も、発行済みペアのもう片側は完結させます。

旧版で複数カードの途中に目標を増やしたログは、先後交換 2 局の開始局面が一致していても
pair_index が分かれ、完成ペア集計から欠落する場合があります。元ログを保持し、
カード・slot・attempt・開始局面・発行順を確認してください。開始局面の一致だけで
ペアを推定し直したり、旧結果を一括無効としたりしないでください。

## 中断時の結果保存と終了状態

Ctrl-C では新規発行を停止し、実行中の対局終了を待って回収した結果と棋譜を保存する。
途中のペアを完結するための追加対局や retry は開始しない。エンジン初期化に失敗した場合も
他 worker から回収できた結果を保存する。どちらも成功完了とは区別し、終了コードは非ゼロとなる。

meta.json の run_status は起動時 running、終了時 completed / interrupted / worker_failed。
interrupted と worker_failed は invalid=true とし、最終 SPRT 採否を表示しない。
incomplete_pairs は発行済み通常ペアのうち有効な 2 局が揃っていない数（失敗ペアを含む）、
unreturned_games は送信済みチケットのうち結果を回収できなかった数を示す。
完成ペアがすべて回収できても、中断 run を通常完走の採否として扱わない。

旧版の中断ログには、終了を待った対局の結果が保存されていない場合がある。
元ログと実行記録を保持し、中断の有無・期待局数・完成ペアを確認する。
記録のない対局を推定して補完したり、通常完走の結果まで一括無効としたりしない。

## 出力先とカード識別

出力先は run ごとに分ける。既存の meta.json、control.json、control_history.jsonl、
または直下の JSONL がある場合は開始前に拒否する。resume / 上書きモードはなく、
失敗した run の成果物も残すため、再実行時には別の出力先を選ぶ。
空のディレクトリや起動前に準備した engine-stderr wrapper は許容する。
出力先そのものの symlink は拒否する。

.tournament.lock の OS ファイルロックを run 中保持して同時起動を防ぐ。
このファイルは終了後も残るが、lock 自体は終了時に解放される。
各カードと最初の meta は既存ファイルを上書きしない作成モードで開く。

カードのファイル名は pair-{i}-{j}.jsonl。i/j は --engine の指定順の 0 始まり index
（i < j）であり、ラベルをファイル名に使わない。ラベルのハイフン連結が衝突しても
別カードは別ファイルになり、ラベルにパス区切りがあっても出力先を変えない。
実際のエンジン名とラベルは各 JSONL の meta と run の meta.json で確認できる。
旧版の {label_i}-vs-{label_j}.jsonl を前提とするスクリプトは、新ファイル名または
meta によるカード選択へ更新する。旧ログ自体は変更しない。

旧版で連結名が衝突するラベルを使った場合や出力先を再利用した場合は、元ファイルと
run 記録を保持し、カード別局数と meta の対応を確認する。失われた記録の復元は保証しない。
