# CSA サーバーのドキュメント

`rshogi-csa-server` は対局コア、`rshogi-csa-server-tcp` / `rshogi-csa-server-workers` は
TCP / Cloudflare Workers の接続・保存を担当する。

## 対局する・サーバーを動かす

| やりたいこと | 入口 |
|---|---|
| USI エンジンをサーバーへ接続する | [CSA クライアント](../csa-client.md) |
| 自分のマシンで TCP サーバーを動かす | [TCP サーバー](tcp-server.md) |
| Workers のロビーで 2 エンジンを対局させる | [ロビー実行手順](lobby_e2e_runbook.md) |
| Worker を構築・更新する | [デプロイ](deployment.md) |
| 持ち時間を設定する | [時計設定](clock_defaults.md) |

## 通信・認証・運用の詳細

- [プロトコル参照](protocol-reference.md): CSA / x1 / 再接続 / 私的対局の通信契約。
- [ロビーの動作と設定](lobby_design.md): マッチング、認証、キューの上限と休止復帰。
- [管理認証](admin_auth.md): 管理 API とハンドルの本人確認。
- [観戦のアクセス制御](viewer_access_control.md): Origin、観戦 API、公開停止。
- [レート制限](rate_limit.md) / [実装の詳細](rate_limit_design.md): 制限値と調整方法。
- [監視](observability.md): ログ、メトリクス、障害の確認。

型・関数の契約は各 crate の Rust doc も参照。
