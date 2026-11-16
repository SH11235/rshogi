# compare_nodes

`compare_nodes` は、2つの USI エンジンを同じ局面・探索条件で実行し、深度別の
探索ノード数、最善手、探索時間を比較するツールです。YaneuraOu との alignment 調査や、
同一エンジンの A/B 比較に使用します。

## クイックスタート

設定ファイルの例をコピーし、エンジンと評価関数のパスを環境に合わせて編集します。

```bash
cp compare_nodes.toml.example compare_nodes.toml
cargo run --release -p tools --bin compare_nodes -- \
  --sfens /path/to/positions.txt \
  --depth 20 \
  --nodes-a 100000000 \
  --nodes-b 100000000 \
  --workers 8
```

単一局面は `--sfens` の代わりに `--sfen` で指定できます。

```bash
cargo run --release -p tools --bin compare_nodes -- \
  --sfen startpos \
  --depth 20 \
  --workers 1
```

## CLI オプション

| オプション | 既定 | 説明 |
|---|---|---|
| `--config <FILE>` | `compare_nodes.toml` | TOML 設定ファイル。存在しない場合は設定なしとして扱う。存在するファイルを読み込めない、または解析できない場合はエラー終了 |
| `--engine-a <FILE>` | 設定ファイル値 | エンジンAのバイナリ |
| `--engine-b <FILE>` | 設定ファイル値 | エンジンBのバイナリ |
| `--options-a <LIST>` | 設定ファイル値または空 | エンジンA固有の USI オプション。`NAME=VALUE` のカンマ区切り |
| `--options-b <LIST>` | 設定ファイル値または空 | エンジンB固有の USI オプション。`NAME=VALUE` のカンマ区切り |
| `--hash <MB>` | 64 | 両エンジンの置換表サイズ |
| `--eval-a <PATH>` | 設定ファイル値または未指定 | エンジンAに `EvalFile` として設定するパス |
| `--eval-b <PATH>` | 設定ファイル値または未指定 | エンジンBに `EvalDir` として設定するパス |
| `--sfens <FILE>` | なし | 1行1局面の SFEN ファイル。`--sfen` と排他 |
| `--sfen <SFEN>` | なし | 単一局面。`startpos` または SFEN 文字列。`--sfens` と排他 |
| `--depth <N>` | 10 | 探索深度 |
| `--nodes-a <N>` | 未指定 | エンジンAの追加ノード上限（1以上）。指定時は `go depth N nodes M` を送信し、要求深度の完了前に打ち切られた局面を `a_truncated` として記録 |
| `--nodes-b <N>` | 未指定 | エンジンBの追加ノード上限（1以上）。指定時は `go depth N nodes M` を送信し、要求深度の完了前に打ち切られた局面を `b_truncated` として記録 |
| `--sample <N>` | 0 | 入力から選ぶ局面数。0 は全件。`--seed` に基づき決定的に選択 |
| `--workers <N>` | 利用可能コア数の半分 | 並列ワーカー数。最小値は1 |
| `--seed <N>` | 42 | サンプリング用乱数シード |
| `--output-base <DIR>` | `results` | 結果ディレクトリの親 |
| `--strict` | 無効 | 全局面の nodes・score・PV・bestmove の一致と要求 depth の完了を検証し、乖離・未完了・欠損で非ゼロ終了（CLI のみ） |
| `--reuse-engine` | 無効 | エンジンを局面間で使い回し、TT を蓄積して逐次処理 |

`--engine-a` と `--engine-b` は CLI または設定ファイルでの指定が必須です。
`--sfens` と `--sfen` は設定ファイルでは指定できず、いずれかを CLI で指定します。
CLI と設定ファイルの両方にある項目は CLI が優先されます。

## config.toml

設定例は `compare_nodes.toml.example` にあります。

```toml
engine_a = "./target/release/rshogi-usi"
engine_b = "/path/to/YaneuraOu/source/YaneuraOu-by-gcc"

options_a = ["Threads=1"]
options_b = ["FV_SCALE=24", "Threads=1", "PvInterval=0"]

hash = 512
eval_a = "/path/to/eval/model.bin"
eval_b = "/path/to/eval/model-dir"
depth = 25
nodes_a = 100000000
nodes_b = 100000000
seed = 42
output_base = "results"
```

設定できる項目は `engine_a`、`engine_b`、`options_a`、`options_b`、`hash`、
`eval_a`、`eval_b`、`depth`、`nodes_a`、`nodes_b`、`seed`、`output_base` です。

## 出力ファイル

結果は `<output_base>/YYYYMMDD-HHMMSS/` に保存されます。

| ファイル | 内容 |
|---|---|
| `meta.json` | エンジン、評価関数、深度、ノード上限、ワーカー数、入力条件などの実行メタデータ |
| `results.jsonl` | 局面ごとの深度別ノード数、評価値、PV、最善手、差分、処理時間、`a_truncated` / `b_truncated` |
| `summary.txt` | 設定ヘッダ、未完了局面数、深度別集計、最善手一致率、乖離分布、乖離が大きい局面 |
| `divergent_sfens.txt` | 深度別ノード数に差があった局面の SFEN。乖離局面がある場合のみ作成 |

ノード上限によって要求深度の探索が完了しなかった局面は `truncated` として記録され、
全 depth 完全一致統計から除外されます。エンジンが中断された深度の `info` を出力しない
場合、`results.jsonl` に残るのは最後に完了した深度までの `nodes` だけです。この値は実際に
消費したノード数を表さず、ノード上限までの範囲で過小評価されることがあります。

エンジンが `usiok` / `readyok` / `bestmove` を返す前に標準出力を閉じると、その局面はエラーになります。1 件でも比較に失敗した場合は、成功分の結果とサマリを保存して非ゼロで終了します。保存済み結果は部分結果です。応答を出さず生存するエンジンの受信期限は設けていません。

JSONL・サマリ・乖離局面ファイルの書き込みや flush に失敗した場合も非ゼロで終了します。途中まで書けたファイルは残るため、エラー終了した出力を完全な比較結果として扱わないでください。

## strict モードで探索結果を検証する

リファクタリング前後などの探索結果を機械的に比較するには `--strict` を指定します。

```bash
cargo run --release -p tools --bin compare_nodes -- \
  --engine-a /path/to/before-engine \
  --engine-b /path/to/after-engine \
  --sfens /path/to/positions.txt --depth 12 --workers 1 --strict
```

全入力局面で次の条件を満たすと終了コードは 0、1 件でも違反すると 1 になります。

- 両エンジンの比較結果が揃っている。起動・探索失敗などで結果が残らなかった局面は「欠損」。
- ノード上限の有無にかかわらず、両方の取得済み最大 depth が要求 depth 以上である。
  depth 情報がない場合も「未完了」。
- 取得した depth の集合が一致し、同じ depth の nodes、score の種別（cp / mate）と値、PV が一致する。
  片側だけに存在する depth は「欠損」。score の片側欠落も不一致として扱う。
- 最終 depth の PV と bestmove が一致する。

判定理由は局面番号・SFEN とともに stderr、stdout、`summary.txt` の末尾へ出力します。
`nodes` / `score` / `PV` / `bestmove` / 「未完了」/ 「欠損」を区別します。
`results.jsonl` の既存の `a_depths` / `b_depths`（`score_cp`、`score_mate`、`pv` を含む）と
`a_bestmove` / `b_bestmove` に比較対象が保存されるため、JSONL の項目は追加しません。
失敗した局面の JSONL 行は残りませんが、strict サマリには欠損として記載されます。

比較は取得できた `info` に基づきます。各 depth では最後の `multipv=1` 相当の行を使用し、
両側とも未取得の中間 depth や score・PV は検証できません。
`a_truncated` / `b_truncated`、既存のノード数統計、`divergent_sfens.txt` の意味は通常モードと同じです。
strict の未完了判定や score・PV・bestmove だけの乖離は strict サマリで確認してください。

`--strict` を省略すると、従来どおり乖離・未完了自体ではエラー終了しません。
通信・設定・ファイル入出力のエラーや局面結果の欠損は、通常モードでも非ゼロ終了します。
