# floodgate_record

`csa_client` が per-game に出力する JSONL(`meta` / `move` / `result` 行)から、
あるエンジンの戦績を集計する CLI。相手が毎局変わる floodgate 連続対局のログ群を、
先後別・相手別・非勝一覧・実戦 NPS で要約する。

`analyze_selfplay` が 2 エンジンの tournament(Elo/SPRT)を対象にするのに対し、本ツールは
「1 エンジン vs 多数の相手」を集計する点が異なる。

## 入力

`--dir` 配下の `*.jsonl`(1 局 1 ファイル、`csa_client` の `[record] save_jsonl=true` 出力)。
各局の判定に使う行:

- `move` 行: `engine`(指した側の名前)/ `ply` / `eval.nps` / `eval.time_ms`。
  **両対局者の手が engine 名付きで記録される**ため、`ply==1` の `engine` が先手、もう一方が後手。
  後手が 1 手も指さず負けた局だけは move に相手が出ないので、その相手はファイル名
  `..._vs_{gote}` から補完する(取りこぼし防止)。
- `result` 行: `winner`(勝者名。引き分けは無し)/ `reason` / `plies`。

`result` 行が無いファイル(進行中の局)は自動でスキップする。

## ビルドと実行

```bash
# ビルド(以降の例はビルド済みバイナリ名で表記する)
cargo build --release -p tools --bin floodgate_record
# → target/release/floodgate_record

# ビルドせず直接実行する場合は次の形に読み替える
cargo run --release -p tools --bin floodgate_record -- <args>
```

## 使い方

```bash
# csa_client と同じ config から dir / --me / キャッシュ・履歴の既定を導出(推奨)
floodgate_record --config ~/floodgate/active.toml --fetch-ratings

# 環境変数に config を置けば引数はさらに減る(明示引数はいつでも config に優先)
export CSA_CLIENT_CONFIG=~/floodgate/active.toml
floodgate_record --fetch-ratings --watch Suisho,dlshogi,nshogi

# config なしの従来形。対象エンジンは自動判定(全局に最も多く出現するエンジン=自分)
floodgate_record --dir ~/floodgate/records/jsonl

# 自分/相手の現在レートを wdoor から取得して併記(キャッシュ併用で障害時フォールバック)
floodgate_record --dir ~/floodgate/records/jsonl --fetch-ratings --ratings-cache ratings.tsv
```

| オプション | 説明 |
|-----------|------|
| `--config <TOML>` | `csa_client` と同一の設定ファイル。`record` 設定から集計 dir、`server.id` から `--me`、`record.dir` からキャッシュ/履歴の既定パスを導出する(対応する明示引数が優先)。省略時は環境変数 `CSA_CLIENT_CONFIG` を参照(空値は未設定扱い)。適用時は導出結果を stderr に 1 行表示する。TOML 内の相対パスは **config ファイルのあるディレクトリ基準**で解決(`csa_client` 実行時 cwd 基準とは異なるので運用 config は絶対パス推奨)。`csa_client` 側の CLI 上書き(`--record-dir` 等)は見えず TOML の値のみ使う。config の `record.enabled` / `record.save_jsonl` が無効で集計元を導出できない場合はエラー(`--dir` 明示で回避可) |
| `--dir <PATH>` | 集計対象 JSONL のあるディレクトリ(再帰なし)。省略時は config から導出、それも無ければ `.` |
| `--me <NAME>` | 集計対象エンジン名。省略時は config の `server.id`、それも無ければ全局に最も多く出現するエンジンを自動判定(別ハンドルのログが混在する dir では config / 明示指定が確実) |
| `--watch <NAMES>` | 注目相手(カンマ区切り・部分一致)。指定時のみ「注目相手との対戦」節を出力 |
| `--fetch-ratings` | wdoor floodgate の現在レート表を取得し、自分/相手に ` (R<rate>)` を併記(要ネットワーク)。wdoor は per-game レートを出さないので**現在値**(対局時点ではない)。取得・解析は `tools::common::floodgate` を再利用。取得失敗時はレート併記だけ諦め、集計は続行 |
| `--ratings-cache <FILE>` | `--fetch-ratings` 併用。fetch 成功時は `name<TAB>rate` を FILE へ書き出し、失敗時は FILE を読み戻してフォールバック併記する(一時障害でも直近値を維持)。config 指定時の既定は `<record.dir>/ratings_cache.tsv` |
| `--ratings-max-age <SEC>` | `--fetch-ratings` 併用。キャッシュの mtime が SEC 秒以内なら**ネットワーク取得をスキップ**してキャッシュを直接使う。既定 0(常に取得)。レートページは日次生成なので数時間(例: `21600` = 6h)で十分。履歴併用時、履歴に当日(JST)分が無ければ鮮度内でも取得する(スキップで履歴の日付が抜けないように) |
| `--ratings-history <FILE>` | `--fetch-ratings` 併用。fetch 成功時に自分(`--me`)の現在レートを `ページ日付<TAB>名前<TAB>レート` で FILE へ追記する(R 推移のローカル記録)。同一(日付, 名前)は再追記せず**その日最初の観測値**を保持。書き込みは tmp→rename の全置換(並行実行でも行が壊れない)。config 指定時の既定は `<record.dir>/ratings_history.tsv` |

`--config` に渡す TOML は csa_client の設定そのもので、本ツールが読むのは次の部分
(完全な floodgate 用 config の例は
[docs/examples/csa-client-floodgate.toml.example](../../../docs/examples/csa-client-floodgate.toml.example)、
解説は [docs/csa-client.md](../../../docs/csa-client.md) を参照):

```toml
[server]
id = "YourEngineName"      # --me の既定値になる

[record]
dir = "/path/to/floodgate/records"  # 集計 dir は <dir>/jsonl/、キャッシュ・履歴の既定は <dir>/ 直下
save_jsonl = true                   # false だと集計元が無くエラー(--dir 明示で回避可)
# jsonl_out = "/path/to/elsewhere"  # JSONL 出力先を上書きしている場合はそちらを集計
```

**名前の突き合わせはすべて正規化キー**(英数字と `-` `_` 以外を `_` に置換。JSONL のファイル名・meta ラベルと同じ規則)で行う。JSONL の `move.engine` / `result.winner` は raw の CSA 名、ファイル名は正規化済みという混在があるため、集計・`--me`・`--watch`・レート表・キャッシュのキーを入口で同一空間に揃えている。レート併記は正規化キーの**完全一致**で引く(表に無い相手は併記なし)。

## 出力

- 通算 W-L-D と勝率(全体 / 引分除く)
- 先手番 / 後手番 別の W-L-D と勝率(最上位帯は先手ほぼ必勝・後手勝ちの価値が大きい)
- 対象エンジンの実戦 NPS(`time_ms>=500` の本探索の median)
- 相手別 W-L-D
- 後手勝ち一覧(相手名・reason・手数。`--watch` 該当相手は `★上位AI` 表示)
- 負け一覧 / 引分一覧(それぞれ手番・相手・reason・手数)
- `--watch` 指定時: 注目相手との対戦一覧

勝敗判定は `result.winner` を基準にする(`winner==me`→勝ち、他名→負け)。winner が無い局は
`reason` で判別し、`sennichite` / `max_moves` / `jishogi` のみ引き分けに数える。`reason` の値は
[csa_client の終局理由](../../../docs/csa-client.md#終局理由の記録)を参照。中断・検閲・error
など未完了局(csa_client は `outcome="draw"` / `reason="interrupted"` で書く)は勝率を歪めないよう
**集計対象外**。対象エンジンが参加していない局(別ハンドルのログ混在)も除外する。除外があれば
ヘッダに件数(対象不参加 / 中断・未完了)を表示する。
