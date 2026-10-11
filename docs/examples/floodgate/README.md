# floodgate 運用スクリプト例

wdoor floodgate まわりの日常運用（観戦・戦績集計・自分の記録の閲覧・再ビルド）を
1 コマンド化するスクリプト例。すべて `$HOME` 相対パス + 環境変数で上書き可能な形で
書いてあり、標準配置（repo が `~/development/rshogi`、運用 dir が `~/floodgate/`）なら
無修正で動く。好みの場所（例: `~/floodgate/`）へコピーして使う。

| スクリプト | 用途 | 前提 |
|---|---|---|
| `watch.sh` / `watch.ps1` | **wdoor の任意の対局をライブ観戦**（他人同士の対局も可） | ビルド済みバイナリのみ。config・認証・運用 dir 一切不要 |
| `tui.sh` | 自エンジンの対局記録を TUI で閲覧（live 追従） | csa_client 運用レイアウト（下記） |
| `stats.sh` | 自エンジンの戦績集計 + wdoor レート併記 | 同上 |
| `rebuild_tools.sh` | 関連 4 バイナリの一括再ビルド（csa_client 稼働中はガード） | repo clone |

## まず観戦だけしたい（config 不要）

```bash
# repo root で実行（置き場所は任意）
cp docs/examples/floodgate/watch.sh ~/floodgate/
~/floodgate/watch.sh                 # 当日の全対局
~/floodgate/watch.sh Sora_Ginko      # 指定 AI の対局のみ（対局者名の部分一致、, 区切りで複数）
```

裏で `floodgate_pipeline live-mirror`（[詳細](../../../crates/tools/docs/floodgate_pipeline.md)）が
wdoor から当日の棋譜をミラーし続け、前面で `kifu_player`
（[詳細](../../../crates/tools/docs/kifu_player.md)）の TUI が開く。TUI 内で進行中の対局を
選んで `f` を押すとライブ追従（最新手を表示し続ける）。TUI を閉じる（`q`）とミラーも
自動停止する。ミラーした棋譜は `~/floodgate-mirror/<対象>/` に残るので後から見返せる。

Windows は `watch.ps1` を使う（**PowerShell 7 = pwsh 必須**。Windows PowerShell 5.1 は
BOM なし UTF-8 の日本語コメントを化けさせるため対象外。Windows Terminal 推奨）。動作は
同じで、複数指定も `.\watch.ps1 名前,名前` のままでよい。

## csa_client 運用レイアウト（stats.sh / tui.sh の前提）

自エンジンを floodgate に参加させている場合の推奨レイアウト。config は認証情報
（trip）を含むため repo 外に置く（[設定例](../csa-client-floodgate.toml.example)、
各項目は [csa-client.md](../../csa-client.md) を参照）:

```
~/floodgate/
├── active.toml          # csa_client config（モデル別 config への symlink にすると切替が楽）
├── records/             # [record].dir。CSA/SFEN 棋譜 + ratings_cache.tsv
│   └── jsonl/           # per-game JSONL（[record].save_jsonl。tui.sh が読む）
└── logs/
```

`.bashrc` 等で `export CSA_CLIENT_CONFIG=~/floodgate/active.toml` しておくと、
`stats.sh`（= `floodgate_record`）が集計 dir・自分の名前・レートキャッシュのパスを
config から自動導出する。

## 終局後に棋譜を公開する

対局が終わったら `kifu_publish wdoor`（[詳細](../../../crates/tools/docs/kifu_publish.md)）で、
records にある Floodgate 対局を rshogi の棋譜サーバーへ取り込む。サーバーが wdoor から棋譜を
1 局 1 回だけ取得して保存するので、両対局者の評価値・読み筋が入る。保存済みの局は何もせず
飛ばすため、同じ records に対して手動でも定期実行でも繰り返し実行できる。

```bash
export RSHOGI_API_BASE=https://<host>/api/v1
export RSHOGI_ADMIN_TOKEN=...   # 管理トークン。config やスクリプトに直接書かない
kifu_publish wdoor --records-dir ~/floodgate/records
```

wdoor に棋譜が残っていない古い対局は、wdoor 版 CSA のアーカイブを `--archive-dir` に指定すると
そこから本文をアップロードする。失敗した局があると終了コードが 1 になる。

## 環境変数での上書き

標準配置でない場合は各スクリプトの env で差し替える:

| 変数 | 既定 | 使うスクリプト |
|---|---|---|
| `RSHOGI_REPO` | `~/development/rshogi` | watch / tui / stats / rebuild_tools |
| `KIFU_PLAYER` | `$RSHOGI_REPO/target/release/kifu_player` | watch / tui |
| `FLOODGATE_PIPELINE` | 同 `floodgate_pipeline` | watch |
| `FLOODGATE_RECORD` | 同 `floodgate_record` | stats |
| `FLOODGATE_WATCH_DIR` | `~/floodgate-mirror` | watch |
| `FLOODGATE_RATINGS` | `~/floodgate/records/ratings_cache.tsv` | watch |
| `FLOODGATE_RECORDS` | `~/floodgate/records` | tui |
| `CSA_CLIENT_CONFIG` | `~/floodgate/active.toml`（stats.sh が補完） | stats |

シェルスクリプトは bash 前提（Linux / WSL / macOS。空配列展開は bash 3.2 でも動く形で
ガード済み）。ただし `rebuild_tools.sh` の csa_client 稼働中検出は GNU pgrep の
`-a`（full command line 出力）前提のため、macOS（BSD pgrep）では効かない。macOS で
floodgate を常駐させる場合は手動で停止を確認してからビルドすること。

## 注意

- **csa_client 稼働中のマシンで重ビルドしない**（対局エンジンと CPU を取り合い持ち時間に
  影響する）。`rebuild_tools.sh` は稼働を検出して確認を求める。
- バイナリは glibc の新しいマシンでビルドした物を古いマシンへコピーできない。各機でビルドする。
