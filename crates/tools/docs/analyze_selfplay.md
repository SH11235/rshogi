# analyze_selfplay

`tournament` 等が出力した JSONL を読み、エンジン別勝敗、直接対決、Elo/nElo、
NPS や深さなどの追加統計を表示する。

## 使い方

```bash
./target/release/analyze_selfplay runs/selfplay/{DIR}/*.jsonl
```

SPRT の post-hoc 判定も表示する場合:

```bash
./target/release/analyze_selfplay --sprt runs/selfplay/{DIR}/*.jsonl
```

`--sprt-base-label` / `--sprt-test-label` で役割を明示できる。省略時は SPRT meta、
`base_label` 記録、ラベル名などから推定し、推定根拠を標準エラー出力に表示する。

Wald パラメータは `--sprt-nelo0` / `--sprt-nelo1` / `--sprt-alpha` /
`--sprt-beta` で上書きできる。nelo は `--sprt-nelo0 -10` のようにスペース区切りの
負値をそのまま渡せる。

## 表示の視点

- エンジンの `A(...)`, `B(...)` ラベルはエンジン ID の辞書順で決まる。
- 通常の直接対決は辞書順で左側のエンジン視点で、勝率と Elo/nElo の符号を表示する。
- `--sprt` 時の SPRT 対象ペアは `test vs base` の順に表示し、直接対決と SPRT
  レポートをどちらも test 視点に揃える。
- JSON 出力の `head_to_head` は互換性のため従来どおり辞書順を維持する。SPRT の
  `test` / `base` および統計値は test 視点である。

`nElo` と SPRT の pentanomial 集計は、同じ開始局面を先後入れ替えた 2 局を
1 ペアとして扱う。`pair_index` と `attempt` が同じ 2 局だけを組にし、error を含む
世代は正常終了した相方も含めて勝敗・直接対決・SPRT から除外する。片スロットしか
完了していない世代も除外し、件数を情報表示する。`attempt` が無い旧ログは 0 として扱う。

追加統計には `error局`、`errorペア`、`再試行ペア`、`枯渇ペア` を表示する。
`枯渇ペア` が 1 以上なら、そのテストはインフラ障害により invalid である。

## 終局理由

「終局理由」には `reason` 別の件数と、入力ファイルを通じた全 result 行数に対する
割合を表示する。error 局・再試行前の世代・重複行・未完了ペアの行も各 1 件として数える。
これは終局状況の監視用であり、WLD / SPRT のペア除外条件は変わらない。
`error` で始まる reason は `error` にまとめ、reason の無い旧ログは `unknown` とする。

表示順は `resign`, `win`, `sennichite`, `sennichite_perpetual_check`,
`adjudication_resign`, `adjudication_win`, `adjudication_draw`, `max_moves`, `timeout`, `illegal_move`,
`no_bestmove`, `error`、続いてその他の理由を辞書順とし、0 件は省略する。
`max_moves 到達率` は 0 件でも別行で明示する。result 行の無い入力ではこのセクションは表示しない。
`--json` では `extra.reasons` に `{ "理由": 件数 }` を出力する。

`sennichite` / `adjudication_draw` / `max_moves` は通常の引分として、
`sennichite_perpetual_check` / `adjudication_resign` / `adjudication_win` は勝者の勝ちとして集計する。

### LLR の版差を調べる場合

SPRT はゼロの pentanomial カテゴリに0.001件を補って正規化する。
確率自体に下限を置く版とは LLR や採否が変わる場合がある。
[計算と保存ログの再計算](tournament.md#llr-の計算と保存ログの再計算)の注意に従い、
同じ仮説・観測ペア・停止時点で比較する。算術の修正と逐次検定の較正は別である。

## 不完全入力と部分集計

指定ファイルの欠落・破損・有効データなし、片側しかない未完了ペア、SPRT 集計失敗、隣接する tournament の
meta.json が非完了 / invalid の場合、結果を部分集計として扱う。
JSON の extra.invalid=true と extra.input_issues に理由を出し、
sprt.decision は invalid（採否なし）となる。既定の終了コードは非ゼロ。
--allow-partial を明示すると部分集計の出力後に成功終了するが、正式採否へは変わらない。
すべての入力に有効な対局データがない場合は従来どおり解析を中止する。

meta.json が存在しない旧ログは許容する。存在するのに読めない・破損している場合は
旧ログ扱いへフォールバックしない。run_status が running / interrupted / worker_failed /
未知の値、invalid=true、incomplete_pairs または unreturned_games が正なら不完全入力。
これらの項目を持たない旧 schema は、この検査だけを理由には無効にしない。
JSONL だけを別ディレクトリへコピーすると run 状態を確認できないため、解析時は
対応する meta.json も保持する。

完成ペアだけが残った中断も run_status で区別する。SPRT の境界到達や実行中の目標変更で
正常終了する場合があるため、起動時の settings.games 未満という理由だけでは無効にしない。

winner のない旧形式では slot 0 を meta の先後、slot 1 をその逆として、
通常 WLD と pentanomial の勝者を共通の規則で解決する。winner があれば優先する。
通常 tournament が winner を記録した結果の再解釈を目的とする変更ではない。

先手・後手の「決着局勝率」は引分を分母に含めず、括弧も勝数/決着局数として表示する。
全局数は別に併記する。1 勝 1 分なら 100.0%（1/1 決着局、全 2 局）で、
得点率（勝数 + 0.5 × 引分数）/全局数とは異なる。全引分・0 局は勝率を「-」とする。

旧版の警告付き解析を採否に使っていた場合は、元入力と警告・run 状態を確認する。
過去結果を一括無効とせず、影響がある入力に限って再集計する。

## 開始局面別の集計

```bash
./target/release/analyze_selfplay --by-startpos /path/to/run-a /path/to/run-b
./target/release/analyze_selfplay --by-startpos --json /path/to/run-a
```

ファイルまたは run ディレクトリを複数指定できる。ディレクトリは直下の JSONL を
パス順に読む（制御履歴の `control_history.jsonl` は除外）。通常の集計の後に、各 JSONL と同じディレクトリの `meta.json` にある
`start_positions[startpos_idx]` の文字列をキーとして集計する。別マシン・別 run の同一文字列も
合算する。文字列の辞書順に番号と開始局面の対応を示し、次の表を出す。

- 対局数、先手勝ち、後手勝ち、引き分け、先手得点率（先手勝ち + 0.5 × 引分）/ 対局数。
- 終局理由別の件数、平均手数 (`plies`)、平均所要秒数（1局の move 行の `elapsed_ms` 合計の平均）。
- 開始局面から10・20・40手までの相異なる手順数と、各長さの集計対象局数。
  その長さに届かない局や、先頭から連続した指し手のログがない局はその列に数えない。

先手勝ち / 後手勝ちは盤上の手番（outcome の black/white）で数える。
平手からの開始局面の手順が偶数手なら、候補手（最後の1手）を指した側は後手。
時間切れの move 行は経過時間に加算するが、適用されていない指し手は手順に含めない。

通常の勝敗集計と同じく、error を含むペアの世代・重複スロット・未完了ペアを除外し、
正常な再試行を採用する。この表の終局理由も採用した対局だけを数える（通常の終局理由欄は全 result 行）。
meta が読めない・開始局面一覧がない・`startpos_idx` がないまたは範囲外の対局は、
開始局面別集計からだけ除外し、件数を標準エラー出力の警告と表に示す。
summary のみの入力も開始局面情報がないため除外件数に数える。

`--json` ではトップレベルの `by_startpos` に `excluded_games` と `rows` を追加する。
各行は `startpos`, `games`, `black_wins`, `white_wins`, `draws`, `black_score_rate`（0～1）,
`reasons`, `average_plies`, `average_seconds`, `prefixes` を持つ。
`prefixes` は `{ "plies": 10, "unique": 3, "games": 8 }` 形式で10・20・40手の順。
フラグを省略した場合はこの欄を出力しない。

move 行を全件保持せず、処理中の対局には先頭40手と経過時間合計だけを保持する。
相異なる手順の正確な計数用集合は、開始局面ごとの異なる先頭手順数に応じて増える。
