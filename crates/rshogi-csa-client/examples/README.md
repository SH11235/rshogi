# `csa_client` の実行例

USI エンジンを TCP / WebSocket の CSA 対局サーバーへ接続する。
詳細は[接続と設定](../../../docs/csa-client.md)、[棋譜と JSONL](../../../docs/csa-client-records.md)、
[コメント付き TOML](csa_client.toml.example)を参照。

## Workers で自動マッチングする

```bash
cargo run -p rshogi-csa-client --release -- \
  --target staging --lobby \
  --handle alice --color black --game-name floodgate-600-10 \
  --engine /path/to/your/usi-engine \
  --options "EvalFile=/path/to/your-nnue.bin,USI_Hash=256" \
  --max-games 5
```

別ターミナルで `--handle bob --color white` に変え、同じコマンドを実行する。
同じ `game_name` と逆の手番の相手が揃うと `MATCHED` が通知され、割り当てられた対局へ接続する。
各局終了後はロビーへ戻り、`--max-games` まで繰り返す。Ctrl-C で待機・対局を中止する。
`--room-id` は不要。

`--game-name` は必須で、ASCII 英数字・`-`・`_` の 1〜32 文字。
同梱環境では `CLOCK_PRESETS` に登録された `floodgate-600-10`（10 分 + 10 秒読み）、
`byoyomi-120-5`（2 分 + 5 秒読み）、`byoyomi-msec-10-100`（10 秒 + 100ms 秒読み）から選ぶ。
未登録名は拒否される。モデルの配布元で必要な USI オプションを確認すること。
エンジンのビルドは[ビルドガイド](../../../docs/build.md)と
[対応 NNUE アーキテクチャ](../../../docs/nnue-supported-architectures.md)を参照。

`--target production` に変えると公開の本番環境に接続する。
通常のネイティブ接続では Origin を送る必要はない。`--target staging` は同梱環境の
許可済み Origin を設定する。自前の Worker へは TOML または `--host` で URL を指定する。
自前ロビーの場合も `--lobby --handle ... --color ... --game-name ...` を付け、
`host` を `wss://<ホスト>/ws/lobby` にする。
プリセットを使わない場合は、TOML の `server.id` または `--id` にも通常の LOGIN ID を設定する。

## 相手を指定した直接ルームで対局する

上記コマンドの `--lobby` を `--room-id myroom-001` に置き換え、`--max-games 1` にする。
相手も同じ room ID・game name と逆の手番で接続する。
終了済みのルームは再利用せず、次の対局では新しい ID を指定する。
連続対局では `host` とログイン ID 内の `{game_seq}` が 0 始まりの対局番号へ置換されるため、
TOML の URL を `wss://<ホスト>/ws/myroom-{game_seq}` とし、両者で番号を揃える方法もある。

実機でのログと棋譜確認は[ロビー E2E 手順](../../../docs/csa-server/lobby_e2e_runbook.md)を参照。

## JSONL を集計する

JSONL は既定で `./records/jsonl/` に 1 局 1 ファイルを保存する。
`--jsonl-out DIR` で保存先を変えるか、TOML の `record.save_jsonl = false` で停止する。

```bash
cargo run -p tools --release --bin analyze_selfplay -- records/jsonl/*.jsonl
```

スキーマ、JSON 出力、相手側データの制約は[JSONL の集計](../../../docs/csa-client-records.md#jsonl-の集計)を参照。
