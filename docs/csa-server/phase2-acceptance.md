# Phase 2 受入シナリオ (Cloudflare Workers フロントエンド)

`.kiro/specs/rshogi-csa-server/tasks.md` §9.7 に対応する受入検証の記録。
Miniflare (`wrangler dev --local`) 上で `rshogi-csa-server-workers` の
WebSocket E2E を通過させ、Phase 2 → Phase 3 ゲートを解除できる状態を宣言する。

## 前提ツール

- Rust toolchain + `wasm32-unknown-unknown` target
- `cargo install worker-build@^0.8`
- Node.js 20+ と `npx wrangler` (本検証は wrangler 4.83.0)
- 任意の WebSocket クライアント (以下の手順では `ws` npm パッケージを使う Node スクリプト)

## 手順

```bash
# 1. ビルド
cd crates/rshogi-csa-server-workers
cargo install worker-build --version "^0.8"   # 初回のみ
cp wrangler.toml.example wrangler.toml
# ローカル受入用に Origin 許可リストを仮設定
sed -i 's|CORS_ORIGINS = ""|CORS_ORIGINS = "https://test.example"|' wrangler.toml
worker-build --release

# 2. ローカル起動 (Miniflare)
npx wrangler dev --local --port 8788
# 別ターミナルで以降の検証を行う
```

### 静的エンドポイント確認

```bash
curl -s http://localhost:8788/health
# → rshogi-csa-server-workers (phase=2 locked=2 workers)
```

### Origin 許可リスト検査 (§9.2)

```bash
# 許可されない Origin → 403
curl -s -o /dev/null -w "%{http_code}\n" \
  -H 'Origin: https://evil.example' http://localhost:8788/ws/r1
# → 403

# Origin 無し → 403
curl -s -o /dev/null -w "%{http_code}\n" http://localhost:8788/ws/r1
# → 403

# 許可 Origin + Upgrade 無し → 426
curl -s -o /dev/null -w "%{http_code}\n" \
  -H 'Origin: https://test.example' http://localhost:8788/ws/r1
# → 426
```

### 対局 E2E (§9.1 / §9.3 / §9.4 / §9.5 / §9.6)

以下のような Node スクリプトで LOGIN → AGREE → 指し手 → %TORYO を走らせる。

```js
// /tmp/e2e_smoke.cjs
const WebSocket = require('ws');
const URL = 'ws://localhost:8788/ws/smoke-room-' + Date.now();
const ORIGIN = 'https://test.example';

const open = role => new Promise((res, rej) => {
  const ws = new WebSocket(URL, { origin: ORIGIN });
  const messages = [];
  ws.on('open',    () => res({ ws, messages }));
  ws.on('message', data => { messages.push(data.toString()); });
  ws.on('error',   rej);
});
const sleep = ms => new Promise(r => setTimeout(r, ms));

(async () => {
  const A = await open('black');
  const B = await open('white');
  A.ws.send('LOGIN alice+g1+black pass\n'); await sleep(400);
  B.ws.send('LOGIN bob+g1+white pass\n');   await sleep(800);
  A.ws.send('AGREE\n'); await sleep(200);
  B.ws.send('AGREE\n'); await sleep(500);
  A.ws.send('+7776FU,T3\n'); await sleep(300);
  B.ws.send('-3334FU,T5\n'); await sleep(300);
  A.ws.send('%TORYO\n');     await sleep(800);
  A.ws.close(); B.ws.close();
})();
```

### 観測される送信シーケンス（検証日 2026-04-19）

両クライアントのログから確認した受信内容:

```
[black]  LOGIN:alice+g1+black OK
[black]  BEGIN Game_Summary ... Your_Turn:+ ... END Game_Summary
[white]  LOGIN:bob+g1+white OK
[white]  BEGIN Game_Summary ... Your_Turn:- ... END Game_Summary
[black]  START:<game_id>
[white]  START:<game_id>
[black]  +7776FU,T0          ← 自分の手の echo
[white]  +7776FU,T0          ← 相手の手
[black]  -3334FU,T0
[white]  -3334FU,T0
[white]  #RESIGN
[black]  #RESIGN
[white]  #WIN
[black]  #LOSE
(close 1000 game finished)
```

ポイント:

- `LOGIN:<name> OK` → `Game_Summary` の順序がプロトコル準拠。
- `Your_Turn` は Black 側に `+`、White 側に `-` を返す。
- 両 AGREE 後に `START:<game_id>` が両ソケットへ同報される。
- 指し手は CoreRoom の `handle_line` を経由して両方に fanout される。
- `%TORYO` で `#RESIGN` → 勝敗コードの順序で流れ、最終的に両 ws が
  `game finished` (code 1000) で close される。

## 確認済み項目

| タスク | 確認内容 | 結果 |
|--------|----------|------|
| §9.1 | `/ws/:room_id` が `id_from_name` で同一 DO に届き、Hibernation 受理で接続維持 | OK |
| §9.2 | 許可 Origin 以外（別 Origin / 欠落）は 403 で拒否 | OK |
| §9.3 | `accept_web_socket` 経路のみでメッセージ配信、手動 `accept()` 呼ばず | OK (コード設計) |
| §9.4 | LOGIN→マッチ→Game_Summary→AGREE→指し手→終局までを DO が駆動 | OK |
| §9.5 | Alarm 発火は本シナリオには含まないが、`set_alarm(Duration)` 経路は実装済み | OK (コード設計) |
| §9.6 | R2 `put(YYYY/MM/DD/<game_id>.csa)` が終局で呼ばれる。`local-kifu-dev` に書き出し | OK |
| §15.7 Phase ゲート | `phase_gate.rs` が `phase3-features` 有効化を `compile_error!` で拒否 | OK (§10.1) |

## 未検証で Phase 3 以降で詰めるもの

- **Alarm 実発火の time-up**: 本シナリオは time-up を誘発していない。
  手動での長時間待機または短時間クロックの差し込みで追加検証する。
- **DO 再起動時の replay**: isolate 破棄を強制する手段が Miniflare 限定
  なので、本番 Cloudflare で明示的なデプロイ入れ替えを伴う確認が必要。
- **Rate limit / auth storage 互換**: Phase 2 は accept-all stub のまま。
  Phase 4 の `RateStorage` 互換実装と組み合わせて再検証する。

## 付録: Phase 2 ゲート解除の運用

Phase 3 への移行時は以下を同時に行う:

1. `crates/rshogi-csa-server-workers/src/phase_gate.rs` の `CURRENT_PHASE`
   を `3` に更新し、Phase 3 実装を守る新しい compile gate に置き換える。
2. `Cargo.toml` の `phase3-features` フラグを実機能のユースケースに合わせて
   細分化（x1 / buoy / additional clocks など）する。
3. `docs/csa-server/phase3-acceptance.md` 相当を新設し、本ドキュメントと
   合わせて「Phase 2 は合格済み、Phase 3 受入基準が新たに走っている」
   ことを文書化する。
