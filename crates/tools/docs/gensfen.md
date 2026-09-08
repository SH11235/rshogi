# gensfen — NNUE 学習用教師局面 (PSV/pack/hcpe3) 生成ツール

NativeBackend で `--eval-file` 指定の評価関数を使い、エンジン同士の対局を回しながら
`PackedSfenValue` 形式の教師局面を生成する。棋力評価（Elo 比較・SPRT 等）には
`tournament` バイナリを使うこと。

## ビルド

```bash
cargo build -p tools --bin gensfen --release
```

リリースビルドのバイナリは `target/release/gensfen` に生成される。

## クイックスタート

```bash
# 基本（NativeBackend、1000局、nodes=80000）
./target/release/gensfen \
  --eval-file eval/halfkp_256x2-32-32_crelu/suisho5.bin \
  --games 1000 --nodes 80000

# 30 並列で大規模生成
./target/release/gensfen \
  --eval-file eval/model.bin \
  --startpos-file start_sfens_ply24.txt \
  --games 100000 --nodes 80000 --concurrency 30 --max-moves 512 --hash-mb 128
```

## 出力ファイル

`--out-dir` を指定しない場合、タイムスタンプ付きディレクトリが自動生成される:

```
runs/gensfen/20260317-120000/
  gensfen.jsonl          # 実行条件 meta と対局結果 result の JSONL
  gensfen.psv            # 学習データ（PackedSfenValue, 40バイト/局面）
  gensfen.info.jsonl     # info ログ（--log-info 指定時のみ）
  gensfen.eval.txt       # 評価値推移（--emit-eval-file 指定時のみ）
  gensfen.metrics.jsonl  # 対局メトリクス（--emit-metrics 指定時のみ）
```

`gensfen.psv` の move16 は実 YaneuraOu 形式 (A: bit14=駒打ち、bit15=成り) で出力されます。

`--out-dir path/to/dir` を指定した場合は、そのディレクトリ内に上記ファイルが生成される。

棋譜（KIF）・サマリファイル・全手 move ログは出力されない（教師局面生成に不要なため）。
KIF が必要な場合は `tournament` バイナリで対局を回し、その出力 jsonl を
[`jsonl_to_kif`](tools-reference.md) で変換する。

### result JSONL の入玉メタ

`gensfen.jsonl` の `type=result` 行には、終局局面の入玉関連メタが含まれる。これらは
記録用のメタ列であって終局裁定には影響せず、`--max-moves` 到達時は引き分けとして記録される。

| フィールド | 説明 |
|-----------|------|
| `start_pos_index` | `--startpos-file` の元行番号（ファイル由来でない場合は 1-origin の開始局面番号） |
| `start_sfen` | 対局開始時点の SFEN |
| `final_points_black` / `final_points_white` | CSA 27点法の宣言点数 |
| `king_in_enemy_black` / `king_in_enemy_white` | 自玉が敵陣三段内にいるか |
| `enemy_zone_pieces_black` / `enemy_zone_pieces_white` | 敵陣三段内の自駒数（玉除く） |
| `adopted` | 終局理由が教師データ採用対象か。異常終局では `false`。`true` でも dedup や出力形式の制約により書き出し局面数が 0 の場合がある |
| `diversions` | `--random-multi-pv` / `--random-move-count` で PV1 以外を選んだ来歴配列。`--omit-diversions` 時はこのキー自体が省かれ、代わりに件数 (`multipv_diversions` / `random_moves` / `diversions_total`) が出る |

`diversions` の各要素は次の形:

```json
{"ply": 17, "kind": "multipv", "chosen_move": "2g2f", "best_move": "7g7f", "score_gap_cp": 25}
```

`ply` は乱択した手自体の手数（1-origin、対局 1 手目 = 1）。`kind` は `multipv` または
`random`。`random` は探索を行わないため `best_move` と `score_gap_cp` が `null` になる。
乱択が無い対局でも `diversions: []` を出力する。

## 動作モード

デフォルトは **NativeBackend**（`rshogi-core` を直接呼び出すマルチスレッド単一プロセス）。
`--eval-file` で評価関数ファイルの指定が必須。
LayerStacks 系ネットを native で使う場合は routing を明示する。progresskpabs なら
`--bucket-mode progresskpabs --progress-buckets <学習時の数> --progress-file <係数>`、
KingRank9 なら `--bucket-mode kingrank9` を指定する。不足・構造不一致は起動時にエラーにする。
係数の指定パスは meta 行の `settings.progress_file` に、内容の SHA-256 は
`settings.progress_file_sha256` に記録され、`--resume` 時はパスと内容の両方を照合する
（同一パスへの係数差し替えも検出する）。
native LayerStacks 実行時は stderr に
`NativeBackend: LayerStacks mode=... stored_buckets=... routing_buckets=...` と、ワーカー終了時の
`progress bucket distribution: [...] (used X/N)` を出力するため、短時間ランでも bucket 使用状況を確認できる。

FV_SCALE は arch 文字列の `fv_scale` トークンから自動判定される。実際の学習スケールと
食い違うネット（例: nnue2score を変えた学習で arch 文字列が旧値のままのもの）では
`--fv-scale N` で上書きする。指定値は meta 行の `settings.fv_scale` と fingerprint に
記録され、resume 時に同一指定を要求する。

USI モードを使う場合は `--native=false --engine-path /path/to/usi-engine` を指定する。
このとき `--engine-path-black/white` で先後を別エンジンにすることも可能。
USI モードでは native 用の `--bucket-mode` / `--progress-buckets` / `--progress-file` は使わない。
LayerStacks では `--usi-option LS_BUCKET_MODE=progresskpabs --usi-option LS_PROGRESS_BUCKETS=8
--usi-option LS_PROGRESS_COEFF=/path/to/progress.bin` のように、ルーティング設定をエンジン側へ渡す。

### USI 単一エンジン最適化

USI モードかつ先後同一エンジン・同一引数なら、自動で 1 プロセスで兼用される（プロセス数が半減）。
TT/履歴が共有されるため棋力評価には不向きだが、教師局面生成では問題ない。

## CLI オプション一覧

### 対局制御

| オプション | デフォルト | 説明 |
|-----------|-----------|------|
| `--games N` | 1 | 対局数 |
| `--max-moves N` | 512 | 1局の最大手数（超過で引き分け） |
| `--concurrency N` | 1 | 並行ワーカー数 |

### 時間制御

| オプション | デフォルト | 説明 |
|-----------|-----------|------|
| `--byoyomi MS` | 0 | 秒読み（ミリ秒） |
| `--btime MS` / `--wtime MS` | 0 | 持ち時間（ミリ秒） |
| `--binc MS` / `--winc MS` | 0 | インクリメント（ミリ秒） |
| `--depth N` | なし | 探索深さ制限 |
| `--nodes N` | なし | 探索ノード数制限 |
| `--timeout-margin-ms MS` | 1000 | タイムアウト検出の安全マージン |

`--depth`/`--nodes` 指定時は `NetworkDelay`, `NetworkDelay2`, `MinimumThinkingTime` を
自動で 0 に設定する（USI エンジンの時間管理パラメータが nodes モードに干渉するのを防ぐため）。

### バックエンド・エンジン設定

| オプション | デフォルト | 説明 |
|-----------|-----------|------|
| `--native[=BOOL]` | true | NativeBackend を使用（`--eval-file` 必須） |
| `--eval-file PATH` | (native 時必須) | NNUE 評価関数ファイル |
| `--bucket-mode MODE` | (LS native 時必須) | `progresskpabs` または `kingrank9` |
| `--progress-buckets N` | (progresskpabs 時必須) | 学習時と同じ routing bucket 数 |
| `--progress-file PATH` | (progresskpabs 時必須) | native LayerStacks 用 progresskpabs 係数ファイル |
| `--fv-scale N` | 0（自動判定） | FV_SCALE オーバーライド（NativeBackend 専用）。arch 文字列の fv_scale が実際の学習スケールと食い違うネットで指定する。USI モードでは `--usi-option FV_SCALE=N` を使う |
| `--keep-tt[=BOOL]` | false | NativeBackend では TT・EvalHash・履歴を対局間で保持。false ではすべてクリア（実験用） |
| `--engine-path PATH` | (USI 時必須) | エンジンバイナリパス |
| `--engine-path-black/white PATH` | — | 先後別エンジン |
| `--engine-args ARG...` | — | エンジンに渡す追加引数 |
| `--usi-option "Name=Value"` | — | USI オプション（複数指定可）。NativeBackend では無視され、`EnteringKingRule` 指定時は警告 |
| `--threads N` | 1 | USI モードの Threads オプション。NativeBackend では無視 |
| `--hash-mb N` | 1024 | ハッシュサイズ（MiB） |
| `--network-delay N` / `--network-delay2 N` | — | NetworkDelay USI オプション |
| `--minimum-thinking-time N` | — | MinimumThinkingTime USI オプション |
| `--slowmover N` | — | SlowMover USI オプション |
| `--ponder` | false | USI_Ponder を有効化 |

USI backend では `--keep-tt=true` は `isready` のみ、false は `usinewgame` と
`isready` を送信する。実際のキャッシュ保持は接続先エンジンに依存し、rshogi は
`isready` でも TT・EvalHash をクリアするため、true でも両キャッシュは保持されない。
EvalHash の保持は、評価キャッシュの利用が有効な場合にキャッシュのウォーム状態へ影響する。

### 開始局面

| オプション | デフォルト | 説明 |
|-----------|-----------|------|
| `--startpos-file FILE` | — | 開始局面ファイル（1行1局面、USI position 形式） |
| `--sfen SFEN` | — | 単一の開始局面 |
| `--random-startpos` | false | 開始局面をランダムに選択（順番巡回ではなく） |
| `--startpos-no-repeat[=BOOL]` | true | 開始局面を重複なしで消費（シャッフル + pop） |
| `--shuffle-seed N` | 自動生成 | 開始局面シャッフルの乱数シード |

開始局面ファイルの形式:
```
position startpos
position startpos moves 7g7f 3c3d
position sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1
```

### 教師局面の取捨

| オプション | デフォルト | 説明 |
|-----------|-----------|------|
| `--output-training-data PATH` | `<out-dir>/gensfen.psv` | 学習データ出力先 |
| `--emit-game-id-sidecar PATH` | — | PSV 各レコードと 1:1・同順の `game_id` を u32 little-endian で出力（PSV 専用） |
| `--training-data-format FORMAT` | psv | `psv`（40バイト固定）/ `pack`（32バイト + メタ）/ `hcpe3`（可変長棋譜 + policy） |
| `--hcpe3-policy-total N` | 65535 | hcpe3 の policy 分布に割り当てる visit 総票数 |
| `--hcpe3-policy-temp F` | 100.0 | hcpe3 の policy softmax 温度（centipawn 単位、大きいほど分布を均す。0 以下は最善候補へ全票） |
| `--hcpe3-eval-drop-threshold N` | 500 | 最善候補から N cp を超えて劣る候補を除外（負値で無効、実着手は常に保持） |
| `--skip-initial-ply N` | 0 | 序盤 1〜N 手目をスキップ（hcpe3 でも prefix 連続なので可） |
| `--skip-in-check BOOL` | false | 王手局面をスキップ（**pack/hcpe3 では不可** = 中間スキップが replay を壊す） |

### 重複回避（gensfen 固有）

| オプション | デフォルト | 説明 |
|-----------|-----------|------|
| `--dedup-hash-size N` | 67108864 (64M / 512MB) | 局面 Zobrist ハッシュ重複検出テーブル（0 で無効） |
| `--random-multi-pv N` | 0（無効） | MultiPV ランダム選択の候補数 |
| `--random-multi-pv-diff N` | MultiPV 有効時に必須 | MultiPV 評価値差閾値（centipawns） |
| `--random-move-count N` | 0 | ランダムムーブ回数 |
| `--random-move-min-ply N` | 1 | ランダムムーブ開始手数 |
| `--random-move-max-ply N` | 24 | ランダムムーブ終了手数 |
| `--dedup-warn-interval N` | 1000 | dedup rate 警告のチェック間隔（ゲーム数） |
| `--dedup-warn-rate F` | 0.1 | dedup rate 警告閾値（10%） |

### 補助出力（opt-in）

| オプション | デフォルト | 説明 |
|-----------|-----------|------|
| `--log-info` | false | エンジンの info 出力を `gensfen.info.jsonl` に記録 |
| `--omit-diversions` | false | result 行の `diversions` 配列を省き件数のみ記録（実測: multipv 8-100 + random 4 / depth 9 のレシピで 5.0KB/局 → 0.7KB/局）。**`relabel_psv --deblunder` は diversions の ply 位置を必要とするため、deblunder を掛ける可能性のある run では使わない**。fingerprint 対象で、resume での全量/省略の混在は拒否される |
| `--emit-eval-file` | false | 評価値推移を `gensfen.eval.txt` に出力 |
| `--emit-metrics` | false | 対局メトリクス JSONL を出力 |
| `--flush-each-move` | false | 毎手フラッシュ（安全だが低速） |
| `--fsync-interval-games N` | 1 | worker ごとの fsync 間隔。1 は毎対局、0 は fsync 無効 |

### 中断・再開

| オプション | 説明 |
|-----------|------|
| `--out-dir DIR` | 出力ディレクトリ（resume 時必須） |
| `--resume` | 前回中断したセッションを再開する |
| `--force-unlock` | 残留した out-dir lock を削除して取得し直す。記録 PID の終了確認後だけ使用する |

`--emit-game-id-sidecar` と `--resume` は併用できる。sidecar の有無は生成条件 fingerprint で
照合し、PSV と sidecar のコミット済みレコード数が一致しない checkpoint は再開を拒否する。
sidecar のパスは meta 行の
`settings.game_id_sidecar` にも記録される。並行実行時は PSV と sidecar の両方をワーカー別の
一時ファイルへ書き、同じワーカー順で最終ファイルへ連結する。sidecar には JSONL・PSV・
info log・eval file・metrics の最終出力パス、およびそれらと sidecar の内部ワーカーパスを
指定できず、衝突時は処理開始前にエラーにする。パスは絶対化し、存在する最深の祖先を
canonicalize してから残りの未存在コンポーネントを正規化するため、symlink 直後の `..` を
含む同じパスの別表記も衝突として扱う。

## 重複回避の詳細

### ハッシュ重複検出（`--dedup-hash-size`）

局面の Zobrist ハッシュを使って既出局面を検出する。対局中はキーを対局ローカルの pending 集合に
保持し、共有テーブルまたは pending 集合に同じキーがあれば重複とする。正常終局で教師データへの
採用が確定した対局だけ、各キーを初回遭遇順に共有テーブルへ反映する。pending 集合は exact set
なので、対局内では direct-map の衝突による取りこぼしがなく、従来より厳密に重複を検出する。
`timeout`、`illegal_move`、`no_bestmove` で破棄した対局の pending 集合はそのまま捨てるため、
後続の正常対局から未出力局面を除外しない。pending のメモリ使用量は 1 対局の手数に比例し、
対局終了時に解放される。

dedup テーブル自体は checkpoint に永続化しない。数千万局面の既存 PSV を走査しても元の
Zobrist key を復元できず、direct-map テーブルの完全な再構築には別 sidecar が必要になるためである。
`--resume` は空の dedup テーブルから再開し、中断前のデータとの重複を検出できない旨を stderr に
警告する。同一プロセス内と resume 後それぞれの重複検出は有効で、教師レコード自体は破損しない。

通常手の局面で重複検出した場合は:
1. それまでに蓄積した学習エントリを全クリア
2. 重複局面自体は記録しない
3. 対局は続行（以降のユニーク局面は通常通り記録）

全ワーカーで 1 つの direct-map テーブルを共有する（tanuki- と同じ構成）。`AtomicU64` で
ロックフリーアクセスし、publish が競合した場合も衝突時上書き方式のため、最悪ケースは従来どおり
重複の見逃しとなる。また、採用確定前の in-flight 対局どうしは互いの pending キーを検出できず、
同じ局面が最大で同時対局数ぶん出力され得る。デフォルト 64M エントリ × 8バイト = 512MB。

### 開始局面シャッフル消費（`--startpos-no-repeat`）

開始局面プールをシャッフルし、順番に 1 つずつ消費する。同じ開始局面が 2 回使われない。
プール枯渇時は再シャッフルして 2 周目に入る。

シャッフルの乱数シードは meta 行に `shuffle_seed` として保存される。resume 時は同じ
seed で順列を再構築し、完了済み対局数分だけ進めることで正確な位置を復元する。
`--shuffle-seed` で seed を明示指定することも可能（再現性が必要な場合）。

### MultiPV ランダム選択（`--random-multi-pv`）

探索時に N 候補を評価し、PV1 のスコアとの差が `--random-multi-pv-diff` 以内の候補から
ランダムに選択してプレイする。学習データには PV1 のスコアと手を記録する（局面の真の評価値）。
多様な局面を自然に生成できる。
PV1 以外が選ばれた手は result 行の `diversions` に `kind=multipv` として記録される。

`--random-multi-pv` を 2 以上にする場合、`--random-multi-pv-diff` の明示指定が必須になる。
局面・探索条件によって安全な評価値差が異なるため、危険な全候補許容値を暗黙に使わず
fail-closed にする。たとえば 100 cp 以内だけを候補にする場合は
`--random-multi-pv 8 --random-multi-pv-diff 100` と指定する。

**推奨ユースケース**: 対局数が開始局面数を大幅に上回る場合（例: 50万局 vs 3万局面プール）。
開始局面の no-repeat だけでは 2 周目以降に同一対局が再現されるため、MultiPV ランダム
またはランダムムーブとの併用を推奨する。

### ランダムムーブ（`--random-move-count`）

序盤の `--random-move-min-ply` 〜 `--random-move-max-ply` の範囲から N 手をランダムに選び、
その手数では合法手からランダムに 1 手選択する（エンジン探索をスキップ）。
ランダムムーブ前の蓄積エントリは全クリアされる（tanuki- 方式）。
選ばれた手は result 行の `diversions` に `kind=random` として記録される。

### dedup rate 警告

`--dedup-warn-interval`（デフォルト 1000）ゲームごとに直近区間の dedup rate をチェックし、
`--dedup-warn-rate`（デフォルト 0.1 = 10%）を超えると stderr に警告を出力する。
長時間実行中に MultiPV の不足をリアルタイムで検知できる。interval はワーカー数で
自動分割される（`interval / concurrency`、最小 1）。

### MultiPV 値の選定ガイド（実験結果）

NativeBackend、nodes=5000〜10000 での実測値。

**10 局面での周回テスト（局面/game）:**

| MultiPV | 5周 | 10周 |
|---|---|---|
| 0（無効） | 33.8 | — |
| 2 | 78.7 | — |
| 4 | 85.3 | 83.9（微減） |
| 8 | 102.3 | 111.9（維持） |

**1000 局面での周回テスト（MultiPV=8）:**

| 周回 | games | PSV局面数 | 局面/game | 効率 |
|---|---|---|---|---|
| 5周 | 5,000 | 540,750 | 108.2 | ≈100% |
| 10周 | 10,000 | 1,085,122 | 108.5 | ≈100% |

**推奨:**

| games / startpos 比率 | MultiPV |
|---|---|
| ≤ 1倍 | 0（不要） |
| 2-5倍 | 4 |
| 5倍以上 | 8 |
| 10倍以上 | 8 + ランダムムーブ |

## 学習データ形式

PackedSfenValue 形式（40バイト/局面）で、Nodchip learner 互換。

| オフセット | サイズ | フィールド |
|-----------|--------|-----------|
| 0 | 32 | PackedSfen（局面データ） |
| 32 | 2 | score（探索評価値、手番視点、cp） |
| 34 | 2 | move16（gensfen が直接出力する PSV では最善手） |
| 36 | 2 | game_ply（手数） |
| 38 | 1 | game_result（1=勝ち, 0=引き分け, -1=負け、手番視点） |
| 39 | 1 | padding |

### 終局理由と教師データへの採否

| `reason` | 裁定 | 教師データ |
|----------|------|--------------|
| `mate` | 詰まされた側の負け | 採用 |
| `resign` | 投了側の負け | 採用 |
| `win` | 宣言側の勝ち | 採用 |
| `sennichite` | 通常千日手の引き分け | 採用（`game_result=0`） |
| `perpetual_check` | 連続王手側の負け | 採用 |
| `max_moves` | 最大手数到達の引き分け | 採用（`game_result=0`） |
| `timeout` | 調査用ログでは相手勝ち | 対局全体を破棄 |
| `illegal_move` | 調査用ログでは相手勝ち | 対局全体を破棄 |
| `no_bestmove` | 合法手があるのに指し手なし。調査用ログでは相手勝ち | 対局全体を破棄 |

破棄対象では、その対局中に収集済みの全局面を捨て、部分採用しない。result JSONL 行は
調査用に残り、`adopted=false` で判別できる。終了時の Training Data stdout サマリには、
異常終局が1局以上あった場合だけ `timeout`、`illegal_move`、`no_bestmove` ごとの破棄対局数と
破棄局面数を出す。破棄局面数は `finish_game` 時点で収集器に残っていたエントリ数であり、
途中の dedup hit やランダムムーブによって既に捨てられた局面は含まない。

通常千日手は、対局開始局面を含む局内の全局面履歴を対象に、4回目の同一局面成立直後に
終局する。周期の長さによる遡及制限はない。循環中に一方がすべての自手で王手を続けていた
場合は連続王手千日手とし、王手側の負けとして裁定する。

### 宣言勝ち局面

PSV では `bestmove win` を受け取った現在局面を終端局面として追加する。`score` は探索値に
よらず宣言側勝ちの飽和値 `+10000` に固定する。宣言勝ちはローカル検証で確定した勝ちであり、
探索値より強い情報だからである。通常の指し手が存在しないため `move16=0`、`game_result=1` になる。
通常局面と同じ共有 dedup を通すが、dedup hit 時は終端局面だけを記録せず、それまでに蓄積した
局面は採用する。通常手の dedup hit 後は対局を続けて局面を再収集できるのに対し、終端では
再収集の機会がなく、蓄積分まで破棄するとその入玉譜を丸ごと失うためである。終了時の
Training Data stdout サマリには、終端局面が重複のため記録されなかった対局数を出す。

`move16=0` は policy 手ではなく「有効な着手を持たないレコード」を表す。宣言勝ち終端のほか、
局面置換によって元の指し手が無効になった既存 PSV でも使われる。局面単位で score / 勝敗を
扱う `rescore_psv`、`relabel_psv`、フィルタ・検証・JSONL 変換・PSV replay 表示の各経路は
この値を保持または着手なし表示として扱える。`psv_to_hcpe3` の hcpe3 / hcpe は有効な policy 手を
必須とするため、有効な着手を持たないレコードを変換対象外としてスキップし、専用件数を
サマリへ出す。

pack / hcpe3 は move16 の手列を replay する形式なので、`win` という擬似着手は追加せず、
記録しない終端局面の dedup 判定も行わない。したがって終端局面が共有テーブルに挿入されたり、
その重複を理由に収集済みの対局局面が破棄されたりすることはない。
終局以前の収集済み手列と勝敗は通常どおり出力する。native / USI のどちらでも `bestmove win` は
同じ側のルール（native は CSA 27点法、USI は `EnteringKingRule` オプション、未指定時は
CSA 27点法）を使って `Position::declaration_win()` で検証する。不成立なら `illegal_move` として
対局全体を破棄する。`NoEnteringKing` では宣言勝ちが発生しない。`TryRule` は成立時に
`Move::WIN` ではなく玉の実移動を返す。その成立手を実際に指した直後に着手側の勝ちとして
終局し、終局前に記録した局面へ勝敗を付与する。`TryRule` で宣言可能でも `bestmove win` は
正しい応答ではなく、`NoEnteringKing` の `bestmove win` と同様に `illegal_move` として対局全体を
破棄する。

NativeBackend の入玉ルールは CSA 27点法固定で、`--usi-option EnteringKingRule=...` は適用されない。
NativeBackend でこの指定を検出した場合は stderr に警告する。別ルールを使う場合は
`--native=false` で、そのルールを実装する USI エンジンに同オプションを渡す。

### pack 形式

`--training-data-format pack` は 1 対局を可変長で書く（開始局面 hcp + 各手の move16/score
+ 終局マーカー）。局面は開始局面から指し手を replay して復元するため、手列は連続している
必要がある。中間局面を間引く `--skip-in-check` は使用できない。pack の move16 は実際に
着手した手を記録する。pack は 1 つの move16 フィールドで replay とラベルを兼ねるため、
replay に必要な実着手と最善手ラベルを同時には保持できない。したがって `pack_to_psv` で
展開した PSV の move16 も実着手ラベルとなり、gensfen が直接出力する PSV の最善手ラベルとは
意味が異なる。

### replay ギャップによるセグメント破棄（pack / hcpe3 共通）

pack と hcpe3 は手列を replay して局面を復元するため、途中の 1 手が欠けると以降を復元できない。
USI エンジンが `score cp` と `score mate` のどちらも返さない手があると、その局面は記録できない。
このとき**その対局でそれまでに収集したセグメント全体を破棄し、次の有効な局面を新しい開始局面と
して再開する**。破棄せずに継ぎ足すと、1 手飛んだ手列がそのまま書き出されて replay が壊れる。

破棄した局面数はサマリの `Discarded positions at replay gaps` に出る（0 のときは表示しない）。
教師局面数が想定より少ないときは、この値と対局単位の破棄件数を切り分けて確認する。
native backend は評価値を必ず返すためこの経路には入らない。

### hcpe3 形式

`--training-data-format hcpe3` は 1 対局を可変長で書き、**各手に MultiPV の policy 分布**を
持たせる（value 専用の psv/pack に対し policy も学習できる）。policy 候補は
`--random-multi-pv N`（N>1）を指定したときに収集される。`--random-multi-pv-diff 0` を併用すると
着手を PV1 と同評価の候補に限定できる（同評価が PV1 だけなら実着手は PV1 になる）。なお
selectedMove16 には実際に着手した手を記録するため、ランダム着手を使っても replay は崩れない。
dlshogi の hcpe3 読み込み側は `moveNum <= 513` を前提とするため、`--max-moves` は 513 以下にすること。

レコード（局面は開始局面 hcp から `selectedMove16` を辿って復元する = 手列が連続している必要）:

| フィールド | サイズ | 内容 |
|-----------|--------|------|
| hcp | 32 | 開始局面 |
| moveNum | 2 | 手数 |
| result | 1 | 下位2bit: 0=引き分け / 1=先手勝ち / 2=後手勝ち。bit2=千日手、bit3=入玉宣言、bit4=最大手数 |
| gameInfo | 1 | bit0-1=opponent（自己対局は0）、bit2-3=max_moves code（256=1、320=2、512=3、その他=0） |
| 以下を moveNum 回 | | |
| selectedMove16 | 2 | 実着手（hcpe move16） |
| eval | 2 | 手番側視点 cp。詰みは 32000-ply 符号化 |
| candidateNum | 2 | policy 候補数 |
| 以下を candidateNum 回 | | |
| move16 | 2 | 候補手（hcpe move16） |
| visitNum | 2 | softmax 量子化した票数 |

policy 候補は量子化前に、最善候補から `--hcpe3-eval-drop-threshold` を超えて劣る候補を
除外する（負値で無効）。実際に着手した手は閾値を超えていても保持する。
policy 候補数は `--random-multi-pv` の MultiPV 幅を超えない。残った全候補へ
最低 1 票を与え、残りを各候補の eval と温度 `--hcpe3-policy-temp` の softmax で配分して
`--hcpe3-policy-total` 票へ量子化する。総票数が候補数未満なら候補数まで引き上げる。
詰み候補は eval と同じ `±(32000-ply)` を softmax にも使用する。
`--random-multi-pv` 未指定（候補なし）のときは実着手の one-hot（visit=1）になる。

gensfen が書く eval は rshogi の cp スケールである。一方、dlshogi の `score_to_value` は
`sigmoid(score / 756.086)` 固定で、`train.py` は `--use_evalfix` を指定した場合だけデータから
係数を推定して補正する。dlshogi で学習するときは `--use_evalfix` を指定すること。

参照実装との差異として、`MoveInfo.eval` の非詰み cp は psv 出力と共有する経路の規約を
踏襲して ±10000 に clip する（参照実装は ±32767）。この経路を変えると psv 出力も変わる一方、
`sigmoid(score / 756.086)` は 10000cp でほぼ飽和するため、学習への影響は小さい。
また、`MoveInfo.eval` には実着手のスコアではなく PV1 の評価値を書く。

## 中断・再開（Resume）

長時間実行を中断して後で再開できる。

### 仕組み

1. 各 worker は `.wN.jsonl` と `.wN.psv` / `.wN.pack` / `.wN.hcpe3`、必要なら
   `.wN.game_ids.bin` を checkpoint として追記する。既存の非空 checkpoint を新規実行で
   truncate せず、`--resume` なしなら退避を求めて停止する。
2. 1 対局の教師データ、sidecar、info、eval、metrics を先に書き、各ファイルの byte offset と
   `fsync_boundary` を含む JSONL result を最後に書く。`fsync_boundary=true` は、その result までの
   全 worker 成果物を fsync した後に result を書き、worker JSONL も fsync する境界を表す。
3. `--resume` は最終 JSONL と全 worker JSONL の `game_id` を bitset に復元する。最大 ID ではなく
   集合で判定するため、並列中断で `100` が欠け `101` が完了していても `100` を再実行する。
   bitset は 1 対局 1 bit（100万局で約 122 KiB）で、result 全体をメモリに保持しない。
4. 全 result に記録された offset の単調性、PSV 40 byte / sidecar 4 byte 境界、PSV と sidecar の
   1:1 件数を先に検証する。全成果物に存在し、かつ `fsync_boundary=true` の最後の result までを
   復旧境界とし、それ以降の result suffix と各成果物を truncate し、
   その game_id を未完了へ戻す。result は全成果物の後に書かれ、offset は単調増加するため、同じ境界へ
   戻してから再対局すれば教師レコードは二重化も欠落もしない。
   各 offset は worker checkpoint 内のコミット境界で worker ごとに 0 から始まる。最終 JSONL は
   監査用に result をそのまま連結するため、最終 PSV / sidecar / 補助出力内の offset ではない。
5. 今回 worker を起動して正常終了した場合は、JSONL、教師データ、有効な sidecar / info / eval /
   metrics が全 worker 分存在することと、各ファイル長が最後の result の offset に一致することを
   journal 作成前に検証する。全局完了済み resume は worker を起動しないため checkpoint を要求しない。
6. worker エラー時も、全 worker checkpoint を手順 4 で復旧してから staging する。このため result
   書き込み、flush/fsync、sidecar 部分書き込みの途中で失敗しても、未コミット末尾は最終成果物へ
   入らない。コミット済み分を最終化した後、worker 別エラーを表示して非ゼロ終了する。
   エラー原因には部分書き込みや fsync 失敗も含まれ、flush 済みの整合状態と安全に区別できないため、
   `--fsync-interval-games N` が `N > 1` なら worker ごとに最大 `N - 1` 局、`0` ならその worker が
   今回の起動で完了した全局を破棄する。破棄した `game_id` は次の `--resume` で再対局する。
7. resume 開始時は `gensfen.finalized.json` の確定長と最終成果物の実長を照合する。PSV と sidecar は
   最終成果物同士のレコード件数も照合する。短縮、欠落、件数不一致は worker を起動する前に停止する。

self-heal するのは、不完全な JSON 末尾、result のない成果物末尾、または最後の fsync 境界より後の
末尾 result の連続 suffix である。result が非ゼロ offset で参照する成果物自体が欠落している場合は、
電源断による短い末尾とは区別して、どのファイルも truncate せず停止する。完全な JSON 行の構文/必須フィールド不正、offset の後退、PSV の
`PackedSfenValue::SIZE` 境界不正、sidecar の 4-byte 境界不正、PSV と sidecar の件数不一致、確定済み
final の不一致、旧形式で offset がない checkpoint は安全な切り戻し境界を証明できないため停止する。

### finalization journal

複数成果物の rename は単独ではトランザクションにならないため、次の journal 方式で冪等化する。

1. 既存 final と全 worker checkpoint から同一ディレクトリの `.merge.tmp` を生成する。既存 final の
   コピー中に prefix の SHA-256 と長さを旧 `gensfen.finalized.json` と照合し、不一致なら worker
   checkpoint を追加せず停止する。続けて完成内容の SHA-256 と長さを計算し、各 merge file を fsync する。
2. final path、merge path、SHA-256、長さ、削除対象 worker checkpoint を
   `.gensfen.finalization.json` に書き、file と親 directory を fsync する。
3. 各 merge file を final へ rename して親 directory を fsync する。通常経路では手順 1 でコピーと
   同時に確定した長さと SHA-256 を使い、merge file を再読込しない。起動時に journal があれば、
   merge が残る項目は長さと SHA-256 を検証して rename を再実行し、merge が無い項目は final の長さと
   SHA-256 が journal と一致することを確認して rename 済みと判定する。
4. 全項目の確定後、最終成果物の確定長と SHA-256 を `gensfen.finalized.json` へ atomic に更新する。
   worker checkpoint を削除・directory fsync した後、最後に journal を削除する。

クラッシュ点ごとの復旧動作は次のとおり。

- journal 永続化前: final は未変更。worker checkpoint から merge を再生成する。
- journal 永続化後、最初の rename 前: journal の全 rename を実行する。
- 任意の rename 間: merge の有無と SHA-256 で各項目の完了を識別し、未完了項目だけ rename する。
  既に final へ入った worker checkpoint を再連結しないため、二重化しない。
- 全 rename 後、finalized state 更新前: 全 final の SHA-256 を確認して state 更新から続行する。
- finalized state 更新後、worker checkpoint 削除中: final を確認し、残った checkpoint だけ削除する。
- worker checkpoint 削除後、journal 削除前: final を確認して journal を削除する。checkpoint の欠落は
  正常な cleanup 済みとして扱う。
- journal 削除後: `gensfen.finalized.json` が確定世代を表す。resume は確定長を事前検証し、staging の
  既存 final コピー中に確定 SHA-256 も検証してから追記する。

通常の resume は確定長を事前検証し、staging に必要な既存 final の read と同時に確定 SHA-256 を検証する。
検証専用の追加 read は行わない。rename 途中からの復旧で「merge が無い = rename 済み」を判定するときも、
該当 final を SHA-256 検証する。
staging 中のピーク追加ディスクは「既存 final 全体 + 今回の worker checkpoint 全体」と同量になる。
PSV は 1 局面 40 bytes、game_id sidecar は 1 局面 4 bytes なので、補助出力を除く固定費は 1 億局面で
4,400,000,000 bytes（約 4.10 GiB）の空きと 8,800,000,000 bytes の read+write、10 億局面では
44,000,000,000 bytes（約 40.98 GiB）の空きと 88,000,000,000 bytes の read+write である。aggregate の
実効 I/O 帯域を 100〜500 MB/s と置くと、固定部分だけで 1 億局面は約 18〜88 秒、10 億局面は約
3〜15 分を要する。JSONL、info、eval、metrics を有効にした場合は各実ファイル長をこの値へ加算する。
通常時の I/O は各入力の逐次 read 1 回と merge への write 1 回で、SHA-256 はコピーと同時に計算するため
追加 read はない。

この実装は final への in-place append よりディスクを使うが、journal が固定した完全な merge を rename し、
final 自体を途中状態へ変更しない復旧モデルを維持する。数日 run の障害復旧経路を同時に変更しないことを
優先してフルコピーを採用している。必要な空き容量と finalization 時間を事前に確保できない構成では使用せず、
補助出力を含む実長から上記の式で見積もる。

### 生成条件 fingerprint

meta の `fingerprint` には次を構造化して保存する。`--resume` では全フィールドを照合し、差がある
フィールド名（例: `search.nodes`, `model.eval_file_sha256`）を列挙して停止する。条件変更を強制する
上書きフラグは設けていない。

- native/USI モード、native 自身の実行ファイル SHA-256、native eval file のパスと SHA-256、
  progress file のパスと SHA-256
- USI engine のパスと実行ファイル SHA-256、先後の args / USI options / threads、Hash、TT 設定
- nodes/depth、時間制御、max_moves、timeout と USI 時間管理設定、ponder、concurrency
- startpos file のパスと SHA-256、読み込み後の開始局面列 SHA-256、単一 SFEN、選択方式、seed
- skip 条件、training format、hcpe3 policy 条件、sidecar の有無、dedup table size
- info / eval / metrics の有無（worker checkpoint の復旧対象を固定するため）
- MultiPV/random move の全条件

`--games` は合計目標を増やして続きを生成できるよう fingerprint から除外する。ただし既存の最大
`game_id` より小さい値への変更は拒否する。ログ flush/fsync 頻度は教師ラベルを変えないため fingerprint
には含めない。`--output-training-data` と `--emit-game-id-sidecar` の出力パスは fingerprint ではなく、
`gensfen.finalized.json` の確定出力 path として resume 時に照合する。

起動時は JSONL、training、sidecar、info、eval、metrics の全 final path、全 worker checkpoint、
全 `.merge.tmp`、finalization journal と atomic temporary file、finalized state、lock を、存在する
最深の親まで canonicalize して比較する。symlink や `..` を介した別表記を含め、同じ実体 path を二つの
用途へ割り当てた構成はディレクトリ作成や lock 取得より前に拒否する。final path と internal path は
`symlink_metadata` で検査し、dangling symlink、directory、特殊ファイルをリンク先へ辿らず拒否する。
atomic temporary file と新規 `.merge.tmp` は create-new で作成する。journal と journal temporary file が
無い resume で残っている通常ファイルの `.merge.tmp` は、journal 永続化前の中断物として削除して worker
checkpoint から再生成する。journal 本体が無い状態で不完全・不正な journal temporary file が残っている場合も、
final は未変更なので temporary file を削除し、worker checkpoint から staging と journal を再生成する。
worker checkpoint の存在検査は `symlink_metadata`、読み書き・追記・truncate は
`O_NOFOLLOW` 付き open を使い、journal、finalized state、復旧対象 `.merge.tmp` を読む場合も symlink を辿らない。

USI option は `名前=値` の形式を解析し、名前を大小文字を無視して判定する。名前が `File`、`Dir`、
`Path` で終わる option、または `LS_PROGRESS_COEFF` は値をファイルまたはディレクトリのパスとして扱う。
相対パスは USI engine が継承する gensfen の起動時 working directory を基準に絶対化する。実在する
ファイルは内容、ディレクトリは相対パス順のファイル名と全内容を SHA-256 化する。`BookFile=no_book`
のように実在しない値は `content_sha256: null` として記録し、値文字列と解決先は引き続き fingerprint で
照合する。したがって `EvalFile`、`EvalDir`、`BookFile` など同じパスの内容差し替えも resume を拒否する。別名の option、
engine args に埋め込まれたパス、engine が内部で暗黙に読む資源までは識別できない。USI モードでは
モデルを `EvalFile` / `EvalDir` / `NNUE` / `ModelFile` 系 option で必ず明示する。両側のいずれかに
明示 option が無い場合は起動時に警告する。暗黙モデルを使った run は、engine binary が同一でも
resume 前後のモデル同一性を証明できないため、本番の無人実行には使用しない。

### flush / fsync 境界

既定の `--fsync-interval-games 1` は、各対局で教師データ、sidecar、info、eval、metrics を
`sync_all` し、その後に `fsync_boundary=true` の result JSONL を書いて `sync_all` する。worker
checkpoint の新規作成後は親 directory も fsync するため、通常の fsync 契約を満たす filesystem では
各完了対局までのファイル内容と directory entry を復旧境界として扱える。
`N > 1` は worker ごとに N 局を一つの fsync 世代にまとめる。境界 result が確認できればその N 局を
まとめて採用し、次の未完了世代はファイル長が残っていても全て rollback する。したがって最大 N-1 局を
再対局する。`0` は実行中の対局単位 fsync 境界を一度も作らないため、クラッシュ後は worker checkpoint
の全 result を未証明として rollback する。正常終了から finalization へ進む経路では flush 済み内容を
使用するが、OS クラッシュ・電源断に対する途中進捗保証はない。
ファイル長が短い状態は offset 検証で検出できるが、長さは正しく未永続ブロックだけがゼロ埋めされた状態は
検出できない。既定の ext4 `data=ordered` では想定しない前提であり、異なる filesystem/mount option では
耐障害性を別途確認する。

Windows には directory fsync に相当する API が無いため親 directory の fsync はスキップされる。
ファイル内容の `sync_all` は同一だが、電源断ではディレクトリエントリの変更 (新規作成・rename・削除)
が失われ得る (プロセス crash のみなら影響しない)。

`--flush-each-move` は、その時点までの result buffer と `--log-info` の info 行を OS buffer へ flush
する。対局中の教師局面は勝敗確定までメモリ上にあるため、このフラグは進行中対局の教師データを保護せず、
fsync も行わない。対局完了の耐久性は `--fsync-interval-games` が制御する。

fsync コストの目安として、即時応答する mock USI engine、200局、1 worker、1手引き分け、NVMe 上の
ext4 一時ディレクトリ（tmpfs ではない）で 3 回測定した wall time は、interval 0 が各 0.03 秒、
interval 1 が 0.18〜0.19 秒、interval 10 が各 0.05 秒だった。これは同じ filesystem 内の比較であり、
別 filesystem、controller cache、mount option では barrier コストが変わる。探索時間がほぼゼロの fsync
最悪寄り測定で、実運用の比率は本番と同じ filesystem で再測定する。耐久性を優先して既定値は 1 とする。

sidecar fault 判定のコストは、release ビルドの PSV + sidecar 書き出しループで 200 万 record を
NVMe/ext4 上へ出力し、fault 無効で各 5 回測定した。判定ありは 49.794〜57.382 ns/record
（中央値 52.308）、判定を除いた比較版は 47.995〜63.781 ns/record（中央値 52.895）で、除去による
改善は測定できなかった。このため fault 点は現状の record loop 内に維持する。

### 注意事項

- `--resume` には `--out-dir` の指定が必須
- out-dir には PID を記録した `.gensfen.lock` を O_EXCL で作成し、二重起動を拒否する。正常終了と通常の
  error/panic では削除するが、強制終了（Ctrl-C 2 回）・SIGKILL・電源断では残る。lock の PID が存在しないことを確認してから
  `--force-unlock` で回収する。PID 再利用や同時回収の race を避けるため自動 stale 削除はしない
- supervisor で無人再起動する場合は、再起動ラッパーを単一起動に制限し、`.gensfen.lock` の JSON に
  記録された PID の生存を確認する。PID が存在する間は再起動せず、存在しない場合だけ同じ引数へ
  `--resume --force-unlock` を追加して再起動する。PID の生存を確認せず常に `--force-unlock` すると、
  稼働中プロセスの lock を奪って二重起動するため禁止する
- `--games` は合計の目標対局数を指定する（追加分ではない）
- `--resume` なしでは有効な全 final path にファイル、ディレクトリ、symlink のいずれかが既にあれば、
  空でも上書きせず副作用前に停止する。worker checkpoint、`.merge.tmp`、journal、finalized state、
  atomic temporary file も上書きしない。internal path に symlink、directory、特殊ファイルがあれば
  `--resume` の有無にかかわらず停止する。前回分を続行する場合は同じ条件で `--resume`、別 run にする場合は
  出力を退避する
- seed は meta から自動復元される。開始局面選択と game_id ごとの MultiPV/random move を再現する。
  CLI で異なる `--shuffle-seed` を指定するとエラー
- native mode は gensfen 実行ファイル自身の SHA-256 も照合し、条件変更を強制する上書きフラグは設けない。
  長時間 run は開始前に release バイナリを run 専用の固定パスへコピーし、初回と resume の両方をその
  コピーから起動する。run 中の再ビルドや別バイナリへの差し替えは resume を fail-closed で拒否する
- 学習データ（.psv）、info ログ、eval、metrics はすべて result と同じ offset 境界で追記・復旧される。
  未完了対局の途中ログは resume 前に除去される
- 終了時の `Positions written in this invocation` は今回のプロセスが新規に書いた局面数であり、resume 前に
  確定済みだった局面を含まない。PSV の確定総局面数は最終ファイル長を 40 bytes で割って確認できる
- Ctrl-C を 2 回押すと強制終了する（進行中の対局は破棄される）
- 同一 seed の通し実行と resume は完了 `game_id` 集合と各 game_id の開始局面・乱択列を一致させる。
  並列 scheduling と resume 時に空へ戻る共有 dedup table のため、成果物全体の bit 一致は保証しない

## 実行中の動的制御（control.json）

`<out-dir>/control.json` を書き換えると、再起動せず対局境界で同時対局数と目標対局数を
変更できる（500ms 間隔でポーリング。フィールドは任意で、存在するものだけ反映）:

```bash
echo '{"concurrency":8}' > <out-dir>/control.json          # 並列度を絞る
echo '{"target_games":2000000}' > <out-dir>/control.json   # 目標対局数を引き上げ
echo '{"target_games":0}' > <out-dir>/control.json         # 安全な drain（下記）
```

- `concurrency` の上限は起動時の `--concurrency`（worker スレッド数と per-worker checkpoint
  数は固定で、超過指定は上限へ clamp）。絞られた worker は ticket 待ちでブロックし CPU を
  消費しない。`0` は無視
- `target_games` の引き上げは無制限。引き下げは送信済み game_id を取り消せないため
  送信済み game_id の最大値へ clamp する。つまり現在値未満（`0` 等）を書くと **安全な drain**
  になる: 新規対局の供給を止め、in-flight を完走させ、通常どおり finalize して終了する
  （単発 Ctrl-C も同じ finalize 経路を通るが、進行中の対局を放棄する点が異なる。drain は
  in-flight を完走させる）
- drain 後も `--resume` で続きを生成できる。ただし `target_games` を CLI の `--games` より
  引き上げていた run の resume は、**引き上げ後の値以上を `--games` に指定する**こと
  （resume 時の checkpoint 検証は `--games` を game_id 上限として使うため）。最終的な有効
  target は終了サマリーと `control_history.jsonl` に出力される
- パース不能な内容は無視して現状維持。変更は `<out-dir>/control_history.jsonl` に追記される
- **プロセス開始より古い mtime の control.json は無視される**（drain 後の `--resume` が
  前回の指定を拾って即終了しないため）。判定は秒粒度で、開始と同一秒の書き込みは
  有効として扱う。restart 後にも反映したい指定は書き直す
- resume 時の fingerprint 照合対象は CLI の `--concurrency` のみ（`--games` は従来どおり対象外）。
  restart 後に絞りたい場合は同じ `--concurrency` で resume し、control.json で下げる

## 使用例

### YaneuraOu USI で学習データ生成

```bash
./target/release/gensfen \
  --native=false \
  --engine-path /path/to/YaneuraOu-halfkp_256x2-32-32 \
  --usi-option "EvalDir=/path/to/eval_dir" \
  --usi-option "FV_SCALE=24" \
  --usi-option "PvInterval=0" \
  --startpos-file start_sfens_ply24.txt \
  --games 100000 \
  --depth 9 --nodes 80000 \
  --concurrency 30 --max-moves 512 --hash-mb 128
```

### 再開可能な大規模生成

```bash
# 初回
./target/release/gensfen \
  --eval-file eval/model.bin \
  --startpos-file start_sfens_ply24.txt \
  --games 100000 --nodes 80000 --concurrency 30 \
  --out-dir data/gensfen/train

# 中断後に再開（同じ引数 + --resume）
./target/release/gensfen \
  --eval-file eval/model.bin \
  --startpos-file start_sfens_ply24.txt \
  --games 100000 --nodes 80000 --concurrency 30 \
  --out-dir data/gensfen/train \
  --resume
```

## JSONL 出力形式

各行が独立した JSON オブジェクト。`type` フィールドで種別を判別:

- `"meta"`: セッション設定（1行目に1回のみ）
- `"result"`: 対局結果（`outcome`: `"black_win"` / `"white_win"` / `"draw"`、
  `adopted`: 終局理由が教師データ採用対象なら `true`）。`adopted=true` は出力件数を保証せず、
  dedup で蓄積局面が消えた対局、PSV の宣言勝ち終端が `--skip-initial-ply` の対象になった対局、
  pack / hcpe3 で終局前の記録対象局面がない対局でも `true` になり得る。
