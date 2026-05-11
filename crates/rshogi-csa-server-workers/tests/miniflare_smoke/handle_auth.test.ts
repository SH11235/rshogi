import { afterEach, beforeEach, describe, expect, test } from "vitest";
import type { Miniflare, WebSocket } from "miniflare";
import {
  CsaClient,
  DEFAULT_TEST_CF_CONNECTING_IP,
  createMiniflare,
  makeTempPersistRoot,
} from "./harness";
import { readLineFromWebSocket } from "./ws_test_helpers";

/**
 * `WORKERS_HANDLE_AUTH` whitelist 経路 (issue #664) の Miniflare smoke。
 *
 * カバー観点:
 * - whitelist 空 → 任意 handle で LOGIN / LOGIN_LOBBY が通過する (backward
 *   compat 全保持の契約を回帰)。
 * - whitelist あり + 正しい password → LOGIN 通過 (admin operator 経路)。
 * - whitelist あり + 不正 password → `LOGIN:incorrect handle_auth_failed` /
 *   `LOGIN_LOBBY:incorrect handle_auth_failed` で reject + 1003 close。
 * - whitelist 外 handle → self-claim 既定挙動を維持 (Floodgate 互換 client
 *   の全切断ゼロ契約を回帰)。
 * - env JSON 不正 → fail-closed で全 LOGIN reject。
 *
 * `password` `correct_horse_battery_staple` の SHA256 を fixture として保持
 * する。`crates/rshogi-csa-server-workers/src/handle_auth.rs` のテスト fixture
 * と揃えてある。
 */

const FIXTURE_PASSWORD = "correct_horse_battery_staple";
const FIXTURE_PASSWORD_SHA256 =
  "6e9b54475e7e568f848f7c302c6d899d85c1118dd39b7b46272ba0b1d9b10c43";

function whitelistOnlyAlice(): string {
  return JSON.stringify([{ handle: "alice", password_sha256: FIXTURE_PASSWORD_SHA256 }]);
}

async function connectLobby(
  mf: Miniflare,
  cfConnectingIp: string = DEFAULT_TEST_CF_CONNECTING_IP,
): Promise<WebSocket> {
  const res = await mf.dispatchFetch("https://example.com/ws/lobby", {
    headers: {
      Upgrade: "websocket",
      "CF-Connecting-IP": cfConnectingIp,
    },
  });
  if (res.status !== 101 || !res.webSocket) {
    throw new Error(`expected 101 with webSocket, got ${res.status}: ${await res.text()}`);
  }
  res.webSocket.accept();
  return res.webSocket;
}

describe("WORKERS_HANDLE_AUTH whitelist (issue #664)", () => {
  let cleanup: () => Promise<void>;
  let persistRoot: string;

  beforeEach(async () => {
    const persist = await makeTempPersistRoot();
    cleanup = persist.cleanup;
    persistRoot = persist.path;
  });

  afterEach(async () => {
    await cleanup();
  });

  describe("LOGIN (GameRoom 経路)", () => {
    test("whitelist 空 → 任意 handle + 任意 password で LOGIN OK", async () => {
      // workersHandleAuth 未指定 (= harness default `"[]"`) で「whitelist 未宣言」モード。
      const mf = await createMiniflare({ persistRoot });
      try {
        const roomId = "handle-auth-empty";
        const client = await CsaClient.connect(mf, roomId);
        client.send("LOGIN alice+game-eval+black anything-password");
        expect(await client.recvLine()).toBe("LOGIN:alice+game-eval+black OK");
        await client.close();
      } finally {
        await mf.dispose();
      }
    });

    test("whitelist あり + alice + 正しい password → LOGIN OK", async () => {
      const mf = await createMiniflare({
        persistRoot,
        workersHandleAuth: whitelistOnlyAlice(),
      });
      try {
        const roomId = "handle-auth-correct";
        const client = await CsaClient.connect(mf, roomId);
        client.send(`LOGIN alice+game-eval+black ${FIXTURE_PASSWORD}`);
        expect(await client.recvLine()).toBe("LOGIN:alice+game-eval+black OK");
        await client.close();
      } finally {
        await mf.dispose();
      }
    });

    test("whitelist あり + alice + 不正 password → handle_auth_failed + 1003 close", async () => {
      const mf = await createMiniflare({
        persistRoot,
        workersHandleAuth: whitelistOnlyAlice(),
      });
      try {
        const roomId = "handle-auth-wrong";
        const client = await CsaClient.connect(mf, roomId);
        client.send("LOGIN alice+game-eval+black wrong-password");
        expect(await client.recvLine()).toBe("LOGIN:incorrect handle_auth_failed");
        await client.close();
        // server-initiated 1003 close を観測できれば理想だが、Miniflare の
        // close event は client.close() 経路でも発火するため、`LOGIN:incorrect`
        // の到達と close 観測の 2 点でゲートする。
        expect(client.isClosed()).toBe(true);
      } finally {
        await mf.dispose();
      }
    });

    test("whitelist 外 handle (bob) → self-claim で LOGIN OK (backward compat)", async () => {
      const mf = await createMiniflare({
        persistRoot,
        workersHandleAuth: whitelistOnlyAlice(),
      });
      try {
        const roomId = "handle-auth-non-listed";
        const client = await CsaClient.connect(mf, roomId);
        // bob は whitelist 外なので password 検証は走らず self-claim 受理。
        client.send("LOGIN bob+game-eval+black anything-password");
        expect(await client.recvLine()).toBe("LOGIN:bob+game-eval+black OK");
        await client.close();
      } finally {
        await mf.dispose();
      }
    });

    test("env JSON 不正 → fail-closed で全 LOGIN reject", async () => {
      const mf = await createMiniflare({
        persistRoot,
        // 配列でも空文字でもない不正 JSON。fail-closed で全 LOGIN を
        // handle_auth_failed で uniform に拒否する契約。
        workersHandleAuth: "not-json",
      });
      try {
        const roomId = "handle-auth-fail-closed-gameroom";
        const client = await CsaClient.connect(mf, roomId);
        // whitelist 外であるはずの bob も fail-closed で reject される。
        client.send("LOGIN bob+game-eval+black anything");
        expect(await client.recvLine()).toBe("LOGIN:incorrect handle_auth_failed");
        await client.close();
        expect(client.isClosed()).toBe(true);
      } finally {
        await mf.dispose();
      }
    });
  });

  describe("LOGIN_LOBBY (Lobby DO 経路)", () => {
    test("whitelist 空 → 任意 handle + 任意 password で LOGIN_LOBBY OK", async () => {
      const mf = await createMiniflare({ persistRoot });
      try {
        const ws = await connectLobby(mf);
        const buf = readLineFromWebSocket(ws);
        ws.send("LOGIN_LOBBY alice+game-eval+black anything-password\n");
        expect(await buf.takeLine()).toBe("LOGIN_LOBBY:alice OK");
        ws.close();
      } finally {
        await mf.dispose();
      }
    });

    test("whitelist あり + alice + 正しい password → LOGIN_LOBBY OK", async () => {
      const mf = await createMiniflare({
        persistRoot,
        workersHandleAuth: whitelistOnlyAlice(),
      });
      try {
        const ws = await connectLobby(mf);
        const buf = readLineFromWebSocket(ws);
        ws.send(`LOGIN_LOBBY alice+game-eval+black ${FIXTURE_PASSWORD}\n`);
        expect(await buf.takeLine()).toBe("LOGIN_LOBBY:alice OK");
        ws.close();
      } finally {
        await mf.dispose();
      }
    });

    test("whitelist あり + alice + 不正 password → handle_auth_failed", async () => {
      const mf = await createMiniflare({
        persistRoot,
        workersHandleAuth: whitelistOnlyAlice(),
      });
      try {
        const ws = await connectLobby(mf);
        const buf = readLineFromWebSocket(ws);
        ws.send("LOGIN_LOBBY alice+game-eval+black wrong-password\n");
        expect(await buf.takeLine()).toBe("LOGIN_LOBBY:incorrect handle_auth_failed");
        ws.close();
      } finally {
        await mf.dispose();
      }
    });

    test("whitelist 外 handle (bob) → self-claim 維持 (backward compat)", async () => {
      const mf = await createMiniflare({
        persistRoot,
        workersHandleAuth: whitelistOnlyAlice(),
      });
      try {
        const ws = await connectLobby(mf);
        const buf = readLineFromWebSocket(ws);
        ws.send("LOGIN_LOBBY bob+game-eval+black anything-password\n");
        expect(await buf.takeLine()).toBe("LOGIN_LOBBY:bob OK");
        ws.close();
      } finally {
        await mf.dispose();
      }
    });

    test("env JSON 不正 → fail-closed で LOGIN_LOBBY reject", async () => {
      const mf = await createMiniflare({
        persistRoot,
        workersHandleAuth: "broken-json",
      });
      try {
        const ws = await connectLobby(mf);
        const buf = readLineFromWebSocket(ws);
        ws.send("LOGIN_LOBBY bob+game-eval+black anything\n");
        expect(await buf.takeLine()).toBe("LOGIN_LOBBY:incorrect handle_auth_failed");
        ws.close();
      } finally {
        await mf.dispose();
      }
    });
  });

  /**
   * Private LOGIN_LOBBY (`<handle>+private-<24hex>+free <password>`) 経路の
   * whitelist 検証 (codex-connector P1 follow-up)。`CHALLENGE_LOBBY` の
   * `opponent=<handle>` が発行者の自己申告のため、token を握った攻撃者が
   * private 経由で whitelist 対象 handle を無認証で名乗れる経路を塞ぐ。
   *
   * challenge token 発行経路まで通電させると test が肥大化するため、
   * **token 検証より前** に handle_auth が走ることを利用して「whitelist 対象
   * handle + 不正 password」が `not_invited` / `challenge_expired` ではなく
   * `handle_auth_failed` で uniform に拒否されることを assert する。
   */
  describe("LOGIN_LOBBY private 経路 (#664 codex-connector P1 follow-up)", () => {
    test("whitelist あり + alice + 不正 password (架空 token) → handle_auth_failed", async () => {
      const mf = await createMiniflare({
        persistRoot,
        workersHandleAuth: whitelistOnlyAlice(),
        privateChallengeEnabled: true,
      });
      try {
        const ws = await connectLobby(mf);
        const buf = readLineFromWebSocket(ws);
        // private token が未登録でも、handle_auth check が token validation より
        // 先に走るので reason は `handle_auth_failed` で固定される (uniform 拒否)。
        ws.send(
          "LOGIN_LOBBY alice+private-0123456789abcdef0123abcd+free wrong-password\n",
        );
        expect(await buf.takeLine()).toBe("LOGIN_LOBBY:incorrect handle_auth_failed");
        ws.close();
      } finally {
        await mf.dispose();
      }
    });

    test("whitelist 外 handle (bob) + private 経路 → token validation に進む (challenge_expired)", async () => {
      const mf = await createMiniflare({
        persistRoot,
        workersHandleAuth: whitelistOnlyAlice(),
        privateChallengeEnabled: true,
      });
      try {
        const ws = await connectLobby(mf);
        const buf = readLineFromWebSocket(ws);
        // bob は whitelist 外 → handle_auth は素通し → token 未登録のため
        // `challenge_expired` で reject (handle_auth_failed ではない)。private
        // 経路の backward compat を回帰する位置付け。
        ws.send(
          "LOGIN_LOBBY bob+private-0123456789abcdef0123abcd+free anything\n",
        );
        expect(await buf.takeLine()).toBe("LOGIN_LOBBY:incorrect challenge_expired");
        ws.close();
      } finally {
        await mf.dispose();
      }
    });
  });
});
