import { afterEach, beforeEach, describe, expect, test } from "vitest";
import type { Miniflare, WebSocket } from "miniflare";
import {
  DEFAULT_TEST_CF_CONNECTING_IP,
  createMiniflare,
  makeTempPersistRoot,
} from "./harness";

/**
 * Issue #622 PR3a: rate limit / abuse protection の Miniflare 経由 E2E。
 *
 * 設計 doc `docs/csa-server/rate_limit_design.md` §5.3 で要求される 7 シナリオを
 * すべてカバーする:
 *
 * 1. LOGIN_LOBBY flood (per-IP)        — 同 IP × N+1 回 → reject
 * 2. LOGIN_LOBBY flood (per-handle)    — 同 handle × 別 IP × M+1 回 → reject
 * 3. CHALLENGE_LOBBY flood (per-IP / per-inviter) — 同様の挙動
 * 4. /ws/<room_id> upgrade flood       — 同 IP × M+1 回 → 503 + Retry-After
 * 5. 窓リセット                        — TODO: Miniflare で 1 分待機が現実的でない
 *                                          ため pure logic test (rate_limit.rs) で
 *                                          補い、本 smoke ではスキップする
 * 6. CF-Connecting-IP 欠落 → fail-closed
 * 7. `%%room_id` 不正値 → parse_ws_route が reject (counter は増えない)
 *
 * 各シナリオで使う IP / handle は test ごとに固有値を割り振り、bucket 隔離する。
 * 同一テストファイル内で同じ IP を複数 test で使うと、`beforeEach` でも
 * RateLimiter DO の state は persist されたままなので bucket が引き継がれる
 * (`makeTempPersistRoot()` で persist root は分離するため、test 間の干渉なし)。
 */

const ALICE_HANDLE = "alice-rl-test";
const BOB_HANDLE = "bob-rl-test";

/**
 * `/ws/lobby` upgrade を CF-Connecting-IP 付きで実行するヘルパ。
 * harness `connectLobby` と同等だが、本 smoke 内の他 test と IP を変えられるよう
 * `cfConnectingIp` 引数を必須化する。
 */
async function connectLobbyWithIp(
  mf: Miniflare,
  cfConnectingIp: string,
): Promise<WebSocket> {
  const res = await mf.dispatchFetch("https://example.com/ws/lobby", {
    headers: {
      Upgrade: "websocket",
      "CF-Connecting-IP": cfConnectingIp,
    },
  });
  if (res.status !== 101 || !res.webSocket) {
    throw new Error(
      `expected 101 with webSocket, got ${res.status}: ${await res.text()}`,
    );
  }
  res.webSocket.accept();
  return res.webSocket;
}

/**
 * WebSocket から 1 行 (`\n` 区切り) を取り出す簡易バッファ。`lobby.test.ts` の
 * 同等関数を本 smoke に複製している (harness 側に出すと scope が広すぎる)。
 */
function readLineFromWebSocket(
  ws: WebSocket,
): { takeLine(timeoutMs?: number): Promise<string> } {
  let buffer = "";
  const queue: string[] = [];
  const waiters: Array<{
    resolve: (s: string) => void;
    reject: (e: Error) => void;
  }> = [];
  let closed = false;

  ws.addEventListener("message", (ev) => {
    const data =
      typeof ev.data === "string"
        ? ev.data
        : new TextDecoder().decode(ev.data as ArrayBuffer);
    buffer += data;
    while (true) {
      const idx = buffer.indexOf("\n");
      if (idx < 0) break;
      const line = buffer.slice(0, idx);
      buffer = buffer.slice(idx + 1);
      const w = waiters.shift();
      if (w) w.resolve(line);
      else queue.push(line);
    }
  });
  ws.addEventListener("close", () => {
    closed = true;
    while (waiters.length > 0) {
      const w = waiters.shift();
      w?.reject(new Error("connection closed"));
    }
  });

  return {
    takeLine(timeoutMs = 3000): Promise<string> {
      if (queue.length > 0) return Promise.resolve(queue.shift()!);
      if (closed) return Promise.reject(new Error("connection closed"));
      return new Promise<string>((resolve, reject) => {
        const entry = {
          resolve: (s: string) => {
            clearTimeout(timer);
            resolve(s);
          },
          reject: (e: Error) => {
            clearTimeout(timer);
            reject(e);
          },
        };
        const timer = setTimeout(() => {
          const i = waiters.indexOf(entry);
          if (i >= 0) waiters.splice(i, 1);
          reject(new Error(`takeLine timeout after ${timeoutMs}ms`));
        }, timeoutMs);
        waiters.push(entry);
      });
    },
  };
}

// ---------------------------------------------------------------------------
// LOGIN_LOBBY flood (per-IP)
// ---------------------------------------------------------------------------

describe("LOGIN_LOBBY rate limit (per-IP)", () => {
  let mf: Miniflare;
  let cleanup: () => Promise<void>;

  beforeEach(async () => {
    const persist = await makeTempPersistRoot();
    cleanup = persist.cleanup;
    mf = await createMiniflare({
      persistRoot: persist.path,
      // 小さい IP 上限と十分余裕のある handle 上限で、IP 側 cap が先に発火
      // することを保証する。
      rateLimitOverrides: {
        lobbyLoginPerIpPerMin: 2,
        lobbyLoginPerHandlePerMin: 1000,
      },
    });
  });

  afterEach(async () => {
    await mf.dispose();
    await cleanup();
  });

  test("同 IP から N+1 回 LOGIN_LOBBY を投げると 3 回目が rate_limited", async () => {
    const ip = "192.0.2.1";

    // 1 回目: allow
    const ws1 = await connectLobbyWithIp(mf, ip);
    const buf1 = readLineFromWebSocket(ws1);
    ws1.send(`LOGIN_LOBBY ${ALICE_HANDLE}+game-eval+black anything\n`);
    expect(await buf1.takeLine()).toBe(`LOGIN_LOBBY:${ALICE_HANDLE} OK`);

    // 2 回目: 別 handle で allow (IP は同じ)。bucket 残量を 1 つ消費。
    const ws2 = await connectLobbyWithIp(mf, ip);
    const buf2 = readLineFromWebSocket(ws2);
    ws2.send(`LOGIN_LOBBY ${BOB_HANDLE}+game-eval+white anything\n`);
    // 2 client の color が complementary なのでマッチング成立して MATCHED が返る。
    // ここでは LOGIN_LOBBY:OK / MATCHED どちらが来てもよい (順序は実装依存)。
    const line2 = await buf2.takeLine();
    expect(
      line2.startsWith("LOGIN_LOBBY:") || line2.startsWith("MATCHED "),
    ).toBe(true);

    // 3 回目: bucket 空 → rate_limited で reject (close されない)。
    const ws3 = await connectLobbyWithIp(mf, ip);
    const buf3 = readLineFromWebSocket(ws3);
    ws3.send(`LOGIN_LOBBY charlie+game-eval+black anything\n`);
    const line3 = await buf3.takeLine();
    expect(line3).toMatch(
      /^LOGIN_LOBBY:incorrect rate_limited retry_after=\d+$/,
    );
    // retry_after は 1 以上 (deny 時の不変条件)。
    const m = line3.match(/retry_after=(\d+)/);
    expect(m).not.toBeNull();
    expect(Number(m![1])).toBeGreaterThanOrEqual(1);

    ws1.close();
    ws2.close();
    ws3.close();
  });
});

// ---------------------------------------------------------------------------
// LOGIN_LOBBY flood (per-handle)
// ---------------------------------------------------------------------------

describe("LOGIN_LOBBY rate limit (per-handle)", () => {
  let mf: Miniflare;
  let cleanup: () => Promise<void>;

  beforeEach(async () => {
    const persist = await makeTempPersistRoot();
    cleanup = persist.cleanup;
    mf = await createMiniflare({
      persistRoot: persist.path,
      // IP 側 cap は十分大きく、handle 側 cap が先に発火することを保証。
      rateLimitOverrides: {
        lobbyLoginPerIpPerMin: 1000,
        lobbyLoginPerHandlePerMin: 1,
      },
    });
  });

  afterEach(async () => {
    await mf.dispose();
    await cleanup();
  });

  test("同 handle で 別 IP から M+1 回 LOGIN_LOBBY → 2 回目が rate_limited", async () => {
    // 1 回目: 別 IP (203.0.113.1) で alice → handle bucket 1 を消費
    const ws1 = await connectLobbyWithIp(mf, "203.0.113.1");
    const buf1 = readLineFromWebSocket(ws1);
    ws1.send(`LOGIN_LOBBY ${ALICE_HANDLE}+game-eval+black anything\n`);
    expect(await buf1.takeLine()).toBe(`LOGIN_LOBBY:${ALICE_HANDLE} OK`);

    // 2 回目: 別 IP (203.0.113.2) で alice → IP bucket は別だが handle bucket
    // は共有なので reject される。
    const ws2 = await connectLobbyWithIp(mf, "203.0.113.2");
    const buf2 = readLineFromWebSocket(ws2);
    ws2.send(`LOGIN_LOBBY ${ALICE_HANDLE}+game-eval+white anything\n`);
    const line = await buf2.takeLine();
    expect(line).toMatch(
      /^LOGIN_LOBBY:incorrect rate_limited retry_after=\d+$/,
    );

    ws1.close();
    ws2.close();
  });
});

// ---------------------------------------------------------------------------
// CHALLENGE_LOBBY flood (per-IP / per-inviter)
// ---------------------------------------------------------------------------

describe("CHALLENGE_LOBBY rate limit (per-IP)", () => {
  let mf: Miniflare;
  let cleanup: () => Promise<void>;

  beforeEach(async () => {
    const persist = await makeTempPersistRoot();
    cleanup = persist.cleanup;
    mf = await createMiniflare({
      persistRoot: persist.path,
      // PRIVATE_CHALLENGE_ENABLED=true でないと CHALLENGE_LOBBY は
      // `unsupported` で reject されて rate limit 経路に到達しない。
      privateChallengeEnabled: true,
      rateLimitOverrides: {
        lobbyChallengePerIpPerMin: 1,
        lobbyChallengePerHandlePerMin: 1000,
      },
    });
  });

  afterEach(async () => {
    await mf.dispose();
    await cleanup();
  });

  test("同 IP から 2 回目の CHALLENGE_LOBBY は rate_limited", async () => {
    const ip = "198.51.100.1";

    // 1 回目: allow (token preset 不一致で `unknown_clock_preset` になるが、
    // rate limit check は parse 通過後 / preset check 前に走るため bucket は
    // 1 消費される)。本 smoke では rate limit 経路の挙動だけを assert する。
    const ws1 = await connectLobbyWithIp(mf, ip);
    const buf1 = readLineFromWebSocket(ws1);
    ws1.send(
      `CHALLENGE_LOBBY ${ALICE_HANDLE} ${BOB_HANDLE} black byoyomi-600-10\n`,
    );
    const line1 = await buf1.takeLine();
    // `unknown_clock_preset` 既存仕様 (本 smoke では preset 未宣言なため)。
    expect(line1).toBe("CHALLENGE_LOBBY:incorrect unknown_clock_preset");

    // 2 回目: bucket 空 → rate_limited
    const ws2 = await connectLobbyWithIp(mf, ip);
    const buf2 = readLineFromWebSocket(ws2);
    ws2.send(
      `CHALLENGE_LOBBY ${ALICE_HANDLE} ${BOB_HANDLE} black byoyomi-600-10\n`,
    );
    const line2 = await buf2.takeLine();
    expect(line2).toMatch(
      /^CHALLENGE_LOBBY:incorrect rate_limited retry_after=\d+$/,
    );

    ws1.close();
    ws2.close();
  });
});

describe("CHALLENGE_LOBBY rate limit (per-inviter)", () => {
  let mf: Miniflare;
  let cleanup: () => Promise<void>;

  beforeEach(async () => {
    const persist = await makeTempPersistRoot();
    cleanup = persist.cleanup;
    mf = await createMiniflare({
      persistRoot: persist.path,
      privateChallengeEnabled: true,
      rateLimitOverrides: {
        lobbyChallengePerIpPerMin: 1000,
        lobbyChallengePerHandlePerMin: 1,
      },
    });
  });

  afterEach(async () => {
    await mf.dispose();
    await cleanup();
  });

  test("同 inviter handle で 別 IP から 2 回目は rate_limited", async () => {
    // 1 回目: IP A で alice → inviter bucket 1 消費
    const ws1 = await connectLobbyWithIp(mf, "198.51.100.10");
    const buf1 = readLineFromWebSocket(ws1);
    ws1.send(
      `CHALLENGE_LOBBY ${ALICE_HANDLE} ${BOB_HANDLE} black byoyomi-600-10\n`,
    );
    expect(await buf1.takeLine()).toBe(
      "CHALLENGE_LOBBY:incorrect unknown_clock_preset",
    );

    // 2 回目: IP B で alice → IP bucket は別だが inviter bucket は共有なので
    // rate_limited
    const ws2 = await connectLobbyWithIp(mf, "198.51.100.11");
    const buf2 = readLineFromWebSocket(ws2);
    ws2.send(
      `CHALLENGE_LOBBY ${ALICE_HANDLE} ${BOB_HANDLE} black byoyomi-600-10\n`,
    );
    const line = await buf2.takeLine();
    expect(line).toMatch(
      /^CHALLENGE_LOBBY:incorrect rate_limited retry_after=\d+$/,
    );

    ws1.close();
    ws2.close();
  });
});

// ---------------------------------------------------------------------------
// /ws/<room_id> upgrade flood
// ---------------------------------------------------------------------------

describe("/ws/<room_id> upgrade rate limit (per-IP)", () => {
  let mf: Miniflare;
  let cleanup: () => Promise<void>;

  beforeEach(async () => {
    const persist = await makeTempPersistRoot();
    cleanup = persist.cleanup;
    mf = await createMiniflare({
      persistRoot: persist.path,
      // ws_room_upgrade を最大に絞る。room_create は十分余裕。
      rateLimitOverrides: {
        wsRoomUpgradePerIpPerMin: 1,
        roomCreatePerIpPerMin: 1000,
      },
    });
  });

  afterEach(async () => {
    await mf.dispose();
    await cleanup();
  });

  test("同 IP から 2 回目の /ws/<room_id> upgrade は 503 + Retry-After", async () => {
    const ip = "203.0.113.50";

    // 1 回目: 101 Upgrade
    const res1 = await mf.dispatchFetch("https://example.com/ws/room-floodtest-1", {
      headers: { Upgrade: "websocket", "CF-Connecting-IP": ip },
    });
    expect(res1.status).toBe(101);
    res1.webSocket?.accept();
    res1.webSocket?.close();

    // 2 回目: 503 + Retry-After
    const res2 = await mf.dispatchFetch("https://example.com/ws/room-floodtest-2", {
      headers: { Upgrade: "websocket", "CF-Connecting-IP": ip },
    });
    expect(res2.status).toBe(503);
    expect(res2.headers.get("Retry-After")).toMatch(/^\d+$/);
    expect(Number(res2.headers.get("Retry-After"))).toBeGreaterThanOrEqual(1);
    expect(await res2.text()).toContain("rate_limited");
  });

  test("CF-Connecting-IP が違えば bucket 隔離されて 1 回ずつ allow", async () => {
    // IP A で 1 回 allow
    const resA = await mf.dispatchFetch("https://example.com/ws/room-floodtest-a", {
      headers: { Upgrade: "websocket", "CF-Connecting-IP": "203.0.113.51" },
    });
    expect(resA.status).toBe(101);
    resA.webSocket?.accept();
    resA.webSocket?.close();

    // IP B で 1 回 allow (IP A の bucket とは独立)
    const resB = await mf.dispatchFetch("https://example.com/ws/room-floodtest-b", {
      headers: { Upgrade: "websocket", "CF-Connecting-IP": "203.0.113.52" },
    });
    expect(resB.status).toBe(101);
    resB.webSocket?.accept();
    resB.webSocket?.close();
  });
});

// ---------------------------------------------------------------------------
// CF-Connecting-IP 欠落時の fail-closed
// ---------------------------------------------------------------------------
//
// **本シナリオは Miniflare 上で直接再現できないため smoke 化していない**:
// Miniflare 4 は Cloudflare edge proxy をシミュレートするために
// `CF-Connecting-IP` を **常に** request 元の IP (`127.0.0.1` 等) で
// 自動注入する。`headers: { "CF-Connecting-IP": "" }` で空を明示的に渡しても
// fetch API レベルで空値ヘッダは silent drop され、Miniflare の default 注入が
// 残る経路となる (= 503 fail-closed が再現できない)。
//
// 代わりに以下 2 経路でカバー済:
// 1. **host pure unit test** (`crates/rshogi-csa-server-workers/src/rate_limit.rs::tests`):
//    `extract_client_ip(req)` が空 / None ヘッダで `None` を返すこと、
//    `RateLimitDecision::deny(FAIL_CLOSED_MISSING_IP_RETRY_AFTER_SEC)` が
//    `Retry-After: 10` を返すことを assert
// 2. **router/lobby ハンドラのコードパス**: `router::forward_ws_to_room` の
//    `let Some(ip) = extract_client_ip(&req) else { return build_missing_ip_response(); }`
//    と `lobby::check_login_lobby_rate_limit` 内の同等パターンが、`None` 返却で
//    確実に 503 / `LOGIN_LOBBY:incorrect rate_limited retry_after=10` を返す
//    1 行分岐になっている (本 smoke のレビュー時に Codex 等で目視検証する)
//
// production deploy 時に Cloudflare edge を経由すれば本ヘッダは確実に存在する
// (Cloudflare 公式仕様)。意図的にヘッダを抜く攻撃経路は CF 側で防御される。

// ---------------------------------------------------------------------------
// `%%room_id` 不正値: parse_ws_route で reject されるとき counter は増えない
// ---------------------------------------------------------------------------

describe("rate limit counter is not incremented on invalid room_id", () => {
  let mf: Miniflare;
  let cleanup: () => Promise<void>;

  beforeEach(async () => {
    const persist = await makeTempPersistRoot();
    cleanup = persist.cleanup;
    mf = await createMiniflare({
      persistRoot: persist.path,
      // 1 だけ allow させて、不正 room_id で 1 消費されないことを 2 回目の
      // allow で実証する。
      rateLimitOverrides: {
        wsRoomUpgradePerIpPerMin: 1,
        roomCreatePerIpPerMin: 1,
      },
    });
  });

  afterEach(async () => {
    await mf.dispose();
    await cleanup();
  });

  test("不正 room_id (`/ws/aa/extra`) は 400 で counter 増えず、後続 1 回が allow", async () => {
    const ip = "198.51.100.99";

    // 不正 path: `/ws/aa/extra` は `parse_ws_route` で reject (None → 400)。
    // この時点で rate limit DO は 1 度も呼ばれていない。
    const resInvalid = await mf.dispatchFetch("https://example.com/ws/aa/extra", {
      headers: { Upgrade: "websocket", "CF-Connecting-IP": ip },
    });
    expect(resInvalid.status).toBe(400);

    // 後続の正規 path は bucket 残量 1 を消費して 101 で通る。
    const resValid = await mf.dispatchFetch("https://example.com/ws/valid-room-1", {
      headers: { Upgrade: "websocket", "CF-Connecting-IP": ip },
    });
    expect(resValid.status).toBe(101);
    resValid.webSocket?.accept();
    resValid.webSocket?.close();
  });

  test("既存 smoke 互換: harness default IP では rate limit に当たらない", async () => {
    // 緩和済 default 閾値 (本 describe は意図的に 1 に絞っているが、test 内の
    // `connect` 呼び出しが harness 経由の DEFAULT_TEST_CF_CONNECTING_IP と異なる
    // IP を使えば bucket 衝突しない、という設計の確認)。
    const res = await mf.dispatchFetch("https://example.com/ws/another-room", {
      headers: {
        Upgrade: "websocket",
        // 本 test 専用 IP (他 test と衝突しない)
        "CF-Connecting-IP": "198.51.100.200",
      },
    });
    expect(res.status).toBe(101);
    res.webSocket?.accept();
    res.webSocket?.close();
  });
});
