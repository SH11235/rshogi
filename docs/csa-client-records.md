# CSA クライアントの棋譜・JSONL・出力

[接続と実行のガイド](csa-client.md)に対する記録リファレンス。設定と出力形式、再接続後の記録を説明する。

## `[record]` — 棋譜保存

```toml
[record]
enabled = true
dir = "./records"
filename_template = "{datetime}_{sente}_vs_{gote}"
save_csa = true   # CSA形式
save_sfen = true  # SFEN局面列（学習データ生成用）
save_jsonl = true # analyze_selfplay / kifu_player 互換 JSONL（dir/jsonl/ に 1 局 1 ファイル）
# jsonl_out = "./runs/csa-jsonl" # JSONL 保存先を変える場合
live_jsonl = false # 対局中に JSONL を手単位で live 追記（下記）
```

テンプレート変数: `{datetime}`, `{game_id}`, `{sente}`, `{gote}`

ファイル名に使う対局 ID・対局者名は、Unicode の文字・数字・`-`・`_` を保持し、それ以外を `_` に置き換える。
元の名前と ID は棋譜内に保持する。`{datetime}` は対局開始時刻の秒までなので、
同じ保存先・同じテンプレートで同秒に始まる対局はファイル名が重なる場合がある。

JSONL の `engine_cmd.label_black` / `label_white`、`move.engine`、`result.winner` は、
CSA のプレイヤー名を同じ規則で使用する。`.` や `@` なども保持し、ファイル名だけを
安全な文字へ置き換える。空名は `unknown`、両者のラベルが同じ場合はそれぞれ
` (black)` / ` (white)` を付け、対局内で区別する。同名対局のラベルは先後を含むため、
複数対局を集計する際は実際の meta ラベルを確認する。

通常の英数字・`-`・`_` の異なる名前はそのまま保持する。meta の `source: "csa_client"` は
単局ログを対局ペアのログと区別する。
過去の JSONL は書き換えない。過去ログでは meta が `_` 置換済み、winner が元の名前に
なっている場合があるため、新旧の特殊文字入りラベルを集計するときは元の CSA 名と
勝敗の先後を照合する。置換後に衝突したラベルから元の名前を一意に復元することはできない。

`analyze_selfplay` は CSA の1局ファイルを1局単位で集計する。`source` のない過去ログは
既存の `engine_cmd.note` にある CSA client の識別文で判別する。明示的な `source` が
あればそちらを優先する。旧ログの置換済み先後ラベルが衝突する場合は集計を拒否する。
CSA は先後交換の
独立したペアではないため、`--sprt` は `--allow-partial` の有無によらず拒否する。
`floodgate_record` は過去ログとの照合で名前を正規化するため、特殊文字の異なる名前が
同じ集計キーになる場合がある。JSONL 内の元のラベルで区別する場合は注意する。

`live_jsonl = true` にすると、対局中に JSONL ファイル（`save_jsonl` と同じパス）へ
確定した手を 1 手ずつ追記する。`kifu_player --tournament-dir <dir>/jsonl --live` で
開いておくと、**自エンジンの進行中対局を評価値付き・手単位でリアルタイム観戦**できる
（wdoor を経由しないので低遅延）。追記中のファイルには result 行が無く、読者は
進行中対局（勝敗不明）として扱う。終局時は従来どおりの完全な内容（result 行込み）へ
置き換わる。追記の失敗は警告のみで対局は続行する。`save_jsonl = false` なら効果なし。

### JSONL の集計

JSONL はローカル解析用で、サーバーへは送信しない。CSA の `T<秒>` からは復元できない
ミリ秒の消費時間、`nodes` / `nps` / `seldepth` を保持するため、既定で保存する。
`--jsonl-out DIR` または `record.jsonl_out` で出力先を変えられる。
`record.save_jsonl = false` で JSONL だけを、`record.enabled = false` で記録全体を止める。

```bash
cargo run -p tools --release --bin analyze_selfplay -- records/jsonl/*.jsonl
cargo run -p tools --release --bin analyze_selfplay -- --json records/jsonl/*.jsonl
```

1 対局を 1 JSONL ファイルに書き、複数局は上記の glob でまとめて集計する。
スキーマは selfplay と同じ `meta` / `move` / `result` の 3 種類。
`move.eval` は `score_cp` / `score_mate` / `depth` / `seldepth` / `nodes` / `time_ms` / `nps` / `pv` を含む。
相手手番の `eval` は取得できず、時間情報の `engine_timing` は自エンジン側の値を対象にする。
相手のバイナリパス・USI オプションは CSA から取得できないため、`path_black` / `path_white` の
相手側は `remote:<player_name>` とする。集計用の `label_*` と `winner` は上記のラベル規則に従う。

### 再接続時の棋譜継続

対局中の切断から `Reconnect_Token` で復帰した場合、`csa_client` は切断前にメモリへ
蓄積した指し手を対局開始局面から replay し、再送された `Game_Summary` の現在局面
（盤面・持ち駒・手番）および `Reconnect_State` の `Current_Turn` と照合する。
一致すれば開始時刻と既存の指し手・探索情報を
引き継ぎ、再接続を挟んでも `.csa` / `.sfen` / `.jsonl` を 1 局 1 ファイルの通し記録
として保存する。ファイル名の `{datetime}` も最初の対局開始時刻のまま変わらない。

局面または game ID・対局者・自手番が一致しない場合は `[REC] resume 局面不一致` の
警告を出し、resume 局面以降だけを断片として保存する。この場合は全形式のファイル名に
`_reconnect_fragment` が付き、CSA には `RECONNECT_FRAGMENT` コメントと不一致理由も
記録される。

## 出力

### ログ

```
[2026-03-28T12:00:00.123Z INFO] CSA対局クライアント起動
[2026-03-28T12:00:00.456Z INFO] [CSA] ログイン成功: rshogi_v1
[2026-03-28T12:00:30.789Z INFO] [CSA] 対局開始: START:game123
[2026-03-28T12:10:15.012Z INFO] 対局 #1 結果: Win | 通算: 1勝 0敗 0分
```

`--log-level debug` で CSA 通信と USI 送信を表示し、`trace` では探索 info を含む USI 受信も表示する。

### 棋譜ファイル

`records/` ディレクトリに対局ごとに保存:

- `20260328_120030_rshogi_v1_vs_opponent.csa` — CSA形式（評価値コメント付き）
- `20260328_120030_rshogi_v1_vs_opponent.sfen` — SFEN局面列（タブ区切り: SFEN, 指し手, 評価値）

CSA の書式は wdoor (Floodgate) の公開棋譜と同じ並びにしてある。指し手行 → `T<秒>` 行 →
`'** <先手視点評価値> <読み筋>` 行（コメントは直前の指し手のもの）の順で、末尾に終局の行
（終局コマンド、`'summary:<理由>:<先手名> <win|lose|draw>:<後手名> <win|lose|draw>`、
`'$END_TIME:YYYY/MM/DD HH:MM:SS`）を書く。評価値と読み筋が入るのは自エンジンの手だけで、
相手の手には `'**` 行がない（相手の評価値は対局中に届かない）。両対局者の評価値が入った棋譜を
公開するには [kifu_publish](../crates/tools/docs/kifu_publish.md) の `wdoor` で wdoor 版を取り込む。

- `$START_TIME` / `'$END_TIME:` は、実行マシンのタイムゾーンによらず JST で書く（CSA の時刻には
  タイムゾーン表記がなく、棋譜サーバーは JST として読む）。ファイル名の `{datetime}` と JSONL の
  `timestamp` はローカル時刻のまま。
- 持ち時間は `$TIME_LIMIT:HH:MM+SS`（持ち時間の時:分 + 秒読みの秒）で書き、フィッシャーの加算は
  wdoor と同じ `'Increment:<秒>` コメントに書く。持ち時間の 1 分未満の端数は切り捨てる。
- 読み筋の先頭は実際の指し手そのもの（Floodgate へ送るコメントと同じ）。
- 勝敗は `'summary:` 行が持つ。勝敗が決まった局には必ず書く。

旧版の `csa_client` は評価値を `'* <評価値> <読み筋>` として指し手の**前**に書き、
`'summary:` / `'$END_TIME:` は書かなかった。`kifu_player` などの読み取り側は両方の形式に対応している。

#### 終局理由の記録

サーバーが最終結果（`#WIN` / `#LOSE` / `#DRAW` / `#CENSORED` / `#CHUDAN`）の前に送る理由行を、
JSONL の `result.reason` と CSA の終局の行に記録する。

| サーバーの理由行 | JSONL `reason` | CSA の終局コマンド | `'summary:` の理由 |
|---|---|---|---|
| `#RESIGN` | `resign` | `%TORYO` | `toryo` |
| `#TIME_UP` | `time_up` | `%TIME_UP` | `time up` |
| `#ILLEGAL_MOVE` | `illegal_move` | なし | `illegal move` |
| `#JISHOGI` + `#WIN` / `#LOSE`（入玉宣言の成立） | `win_declaration` | `%KACHI` | `kachi` |
| `#JISHOGI` + `#DRAW` | `jishogi` | `%JISHOGI` | `jishogi` |
| `#OUTE_SENNICHITE` | `oute_sennichite` | なし | `oute_sennichite` |
| `#SENNICHITE` | `sennichite` | `%SENNICHITE` | `sennichite` |
| `#MAX_MOVES` | `max_moves` | `%MAX_MOVES` | `max_moves` |
| `#ABNORMAL`（相手の切断など） | `abnormal` | `%CHUDAN` | `abnormal` |
| 知らない理由行 + `#WIN` / `#LOSE` | `abnormal` | `%CHUDAN` | `abnormal` |
| `#CHUDAN` | `interrupted` | `%CHUDAN` | （書かない） |
| 理由行なしの `#WIN` / `#LOSE` | `win` / `lose` | `%TORYO` | `toryo` |
| 理由行なしの `#DRAW` | `sennichite` | `%SENNICHITE` | `sennichite` |

- 反則負けと連続王手の千日手は、wdoor の実物と同じく `%` コマンドを書かずに `'summary:` で終える。
- 自分の手のエコーが返る前に終局した場合（自分の反則手、時間切れ後に届いた手）、その手は
  サーバーに受理されていないので、CSA / SFEN / JSONL の指し手には入れない。反則負けのときは
  CSA に `'ILLEGAL_MOVE(<手>)` コメントとして残す。
- 自分が `%TORYO` / `%KACHI` を送った後にサーバーの結果が届かず切断された場合、JSONL には送った
  コマンド（`resign` / `win_declaration`）を記録するが、勝敗が確定していないので CSA は `%CHUDAN`
  だけで終える（`%TORYO` / `%KACHI` も `'summary:` も書かない）。勝敗が決まっていない局は理由に
  よらず同じ扱い。
- 対局中にサーバーとの接続が切れた局は終局理由を記録せず、CSA は指し手までで終わる。
- `'summary:` の理由は、wdoor の実棋譜（2026 年 6〜7 月の約 2.4 万局）に現れる 9 種（`toryo` /
  `time up` / `sennichite` / `kachi` / `abnormal` / `illegal move` / `oute_sennichite` /
  `max_moves` / `oute_kaihimore`）の表記に合わせている。`oute_kaihimore`（王手放置）は
  プロトコル上 `#ILLEGAL_MOVE` として届き区別できないので `illegal move` と書く。`jishogi` は
  wdoor にない独自のラベル。

内蔵 CSA サーバーについても、ミリ秒・分時計の着手通知と `Time_Unit` の一致を
実 `GameRoom` と TCP 接続による統合テストで検証する。対局者の残時間はサーバーの
課金結果に一致し、保存する CSA V2 棋譜の消費時間は秒単位を維持する。

サマリー本文の受信中も停止要求を確認するため、未完の本文を送るサーバーに対しても Ctrl-C / sink 停止で待機を抜ける。

JSONL の未決結果は `outcome: "interrupted"`（中断）/ `"censored"`（打切り）となり、
`winner` は付かない。`reason` は受信した理由を保持し、記録が空なら outcome を使う。
旧形式の `draw` + `reason: "interrupted"` / `"censored"` は未決と判別できるが、
`draw` / `max_moves` のみが残るログからは引分と打切りを復元できない。
