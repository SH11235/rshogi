# Cloudflare Workers フロントエンド 受入シナリオ

`rshogi-csa-server-workers` を Miniflare (`wrangler dev --local`) 上で
起動し、WebSocket 経由で 1 対局を通過させる E2E 受入スクリプトと、その
観測結果の記録。後続の拡張（観戦・x1・Fischer clock など）が入るまえの
「最小機能で E2E が通る」証跡として残す。

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

### Origin 許可リスト検査

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

### 対局 E2E

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
  await sleep(200);

  // ---- Evidence output (再現可能な受入ログ) ----
  const dump = (label, msgs) => {
    console.log(`\n===== RECEIVED BY ${label} =====`);
    msgs.forEach(m => process.stdout.write(m));
  };
  dump('BLACK', A.messages);
  dump('WHITE', B.messages);

  // ---- Acceptance gates (fail = exit code 1) ----
  const checks = [
    ['BLACK saw LOGIN:OK',      A.messages.some(m => m.includes('LOGIN:alice+g1+black OK'))],
    ['BLACK saw Game_Summary',  A.messages.some(m => m.includes('BEGIN Game_Summary'))],
    ['BLACK saw Your_Turn:+',   A.messages.some(m => m.includes('Your_Turn:+'))],
    ['WHITE saw LOGIN:OK',      B.messages.some(m => m.includes('LOGIN:bob+g1+white OK'))],
    ['WHITE saw Your_Turn:-',   B.messages.some(m => m.includes('Your_Turn:-'))],
    ['both saw START',          A.messages.some(m => m.startsWith('START:')) &&
                                 B.messages.some(m => m.startsWith('START:'))],
    ['both saw +7776FU move',   A.messages.some(m => m.includes('+7776FU')) &&
                                 B.messages.some(m => m.includes('+7776FU'))],
    ['both saw -3334FU move',   A.messages.some(m => m.includes('-3334FU')) &&
                                 B.messages.some(m => m.includes('-3334FU'))],
    ['BLACK saw #RESIGN/#LOSE', A.messages.some(m => m.includes('#RESIGN')) &&
                                 A.messages.some(m => m.includes('#LOSE'))],
    ['WHITE saw #RESIGN/#WIN',  B.messages.some(m => m.includes('#RESIGN')) &&
                                 B.messages.some(m => m.includes('#WIN'))],
  ];
  console.log('\n===== ACCEPTANCE GATES =====');
  const failed = checks.filter(([, ok]) => !ok);
  checks.forEach(([name, ok]) => console.log(`${ok ? '[OK]  ' : '[FAIL]'} ${name}`));
  process.exit(failed.length ? 1 : 0);
})().catch(e => { console.error(e); process.exit(1); });
```

実行して exit 0 で終われば受入合格。再現例:

```bash
(cd /tmp && npm init -y >/dev/null && npm i ws >/dev/null)
node /tmp/e2e_smoke.cjs
# ... RECEIVED BY BLACK / WHITE の dump ...
# ===== ACCEPTANCE GATES =====
# [OK]   BLACK saw LOGIN:OK
# ...
# echo $? # → 0
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

`OK` は E2E スクリプトが成功パスを観測したもの、`OK (実装済み・未実走)` は
コード経路が実装され (ホスト / wasm32 ビルド済み)、本スクリプトでは当該 trigger を
誘発していないもの。

| 項目 | 確認内容 | 結果 |
|------|----------|------|
| WS 受付 + DO 決定論ルーティング | `/ws/:room_id` が `id_from_name` で同一 DO に届き、Hibernation 受理で接続維持 | OK |
| Origin 許可リスト | 許可外の Origin / 欠落 / 不一致は 403 で拒否 | OK |
| accept_web_socket ベースの配信 | 手動 `accept()` を呼ばない、runtime-managed WebSocket の経路 | OK (コード設計) |
| 対局状態の永続化 | LOGIN→マッチ→Game_Summary→AGREE→指し手→終局までを DO が駆動。slots / config / moves を `state.storage()` と SQLite に書き込み | OK |
| DO 再起動復元 | `ensure_core_loaded` で `play_started_at_ms` と moves replay を実装。Miniflare では isolate 破棄を誘発できないため本スクリプトでは未観測 | OK (実装済み・未実走) |
| Alarm 時間切れ駆動 | `set_alarm(Duration)` → `force_time_up(current_turn)` 経路は実装済み。本スクリプトは time-up を誘発しない | OK (実装済み・未実走) |
| R2 棋譜エクスポート | `put(YYYY/MM/DD/<game_id>.csa)` が終局で呼ばれ、`local-kifu-dev` バケットに書き出される | OK |
| フロントエンド間のランタイム分離 | TCP と Workers は `workers` / `tokio-transport` feature を互いに `compile_error!` で排他 | OK |

## 後続で詰めるもの

- **Alarm 実発火の time-up**: 本スクリプトは time-up を誘発していない。
  手動の長時間待機または短時間クロックの差し込みで追加検証する。
- **DO 再起動時の replay**: isolate 破棄を強制する手段が Miniflare にないので、
  本番 Cloudflare で明示的なデプロイ入れ替えを伴う確認が必要。
- **Rate limit / auth storage 互換**: Workers 側は現状 accept-all stub。
  TCP 版相当の `RateStorage` + PasswordHasher 互換実装と組み合わせて再検証する。
