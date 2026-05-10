import { afterEach, beforeEach, describe, expect, test } from "vitest";
import type { Miniflare, WebSocket } from "miniflare";
import { DEFAULT_TEST_CF_CONNECTING_IP, createMiniflare, makeTempPersistRoot } from "./harness";

/**
 * `/ws/lobby` route と `Lobby` Durable Object のマッチング動作を Miniflare で
 * 直接検証する。LOGIN_LOBBY → MATCHED → handoff の流れが期待通り動くかを確認する。
 *
 * 既存 `GameRoom` DO への引き渡しまでの完全 E2E (1 局完走) は本 smoke のスコープ外
 * (csa_client `--lobby` mode を含む実装が揃ってから別 smoke で検証する)。
 */

interface LobbyLineBuffer {
  takeLine(timeoutMs?: number): Promise<string>;
}

function readLineFromWebSocket(ws: WebSocket): LobbyLineBuffer {
  let buffer = "";
  const queue: string[] = [];
  const waiters: Array<{ resolve: (s: string) => void; reject: (e: Error) => void }> = [];
  let closed = false;

  ws.addEventListener("message", (ev) => {
    const data =
      typeof ev.data === "string" ? ev.data : new TextDecoder().decode(ev.data as ArrayBuffer);
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

async function connectLobby(
  mf: Miniflare,
  cfConnectingIp: string = DEFAULT_TEST_CF_CONNECTING_IP,
): Promise<WebSocket> {
  // CF-Connecting-IP は LobbyDO 側で per-IP rate limit (issue #622 PR3a) に
  // 使われる。Miniflare は edge proxy をシミュレートしないので test 側で
  // 必ず注入する。同 IP で多数 LOGIN_LOBBY を投げると `LOBBY_LOGIN_RATE_PER_IP_PER_MIN`
  // バケットを共有する点に注意。
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

describe("Lobby DO matching", () => {
  let mf: Miniflare;
  let cleanup: () => Promise<void>;

  beforeEach(async () => {
    const persist = await makeTempPersistRoot();
    cleanup = persist.cleanup;
    mf = await createMiniflare({
      persistRoot: persist.path,
    });
  });

  afterEach(async () => {
    await mf.dispose();
    await cleanup();
  });

  test("2 client が complementary color で LOGIN_LOBBY → MATCHED が両方に届く", async () => {
    const wsBlack = await connectLobby(mf);
    const blackBuf = readLineFromWebSocket(wsBlack);
    wsBlack.send("LOGIN_LOBBY alice+game-eval+black anything\n");
    expect(await blackBuf.takeLine()).toBe("LOGIN_LOBBY:alice OK");

    const wsWhite = await connectLobby(mf);
    const whiteBuf = readLineFromWebSocket(wsWhite);
    wsWhite.send("LOGIN_LOBBY bob+game-eval+white anything\n");
    expect(await whiteBuf.takeLine()).toBe("LOGIN_LOBBY:bob OK");

    const blackMatched = await blackBuf.takeLine();
    const whiteMatched = await whiteBuf.takeLine();

    expect(blackMatched).toMatch(/^MATCHED lobby-game-eval-[0-9a-f]{32} black$/);
    expect(whiteMatched).toMatch(/^MATCHED lobby-game-eval-[0-9a-f]{32} white$/);
    // 同じ room_id にマッチングされる
    const blackRoom = blackMatched.split(" ")[1];
    const whiteRoom = whiteMatched.split(" ")[1];
    expect(blackRoom).toBe(whiteRoom);

    wsBlack.close();
    wsWhite.close();
  });

  test("game_name が異なるとマッチしない (待機継続)", async () => {
    const ws1 = await connectLobby(mf);
    const buf1 = readLineFromWebSocket(ws1);
    ws1.send("LOGIN_LOBBY alice+pool-a+black anything\n");
    expect(await buf1.takeLine()).toBe("LOGIN_LOBBY:alice OK");

    const ws2 = await connectLobby(mf);
    const buf2 = readLineFromWebSocket(ws2);
    ws2.send("LOGIN_LOBBY bob+pool-b+white anything\n");
    expect(await buf2.takeLine()).toBe("LOGIN_LOBBY:bob OK");

    // MATCHED は来ない (300ms 待機して timeout を確認)。
    await expect(buf1.takeLine(300)).rejects.toThrow("timeout");
    await expect(buf2.takeLine(300)).rejects.toThrow("timeout");

    ws1.close();
    ws2.close();
  });

  test("不正な LOGIN_LOBBY format は LOGIN_LOBBY:incorrect で reject", async () => {
    const ws = await connectLobby(mf);
    const buf = readLineFromWebSocket(ws);
    // game_name に invalid char `.` を含める
    ws.send("LOGIN_LOBBY alice+game.bad+black anything\n");
    expect(await buf.takeLine()).toBe("LOGIN_LOBBY:incorrect bad_game_name");
    // 接続は close される
    ws.close();
  });

  test("LOGOUT_LOBBY で queue から離脱、後続のマッチング対象に含まれない", async () => {
    const ws1 = await connectLobby(mf);
    const buf1 = readLineFromWebSocket(ws1);
    ws1.send("LOGIN_LOBBY alice+pool-c+black anything\n");
    expect(await buf1.takeLine()).toBe("LOGIN_LOBBY:alice OK");

    ws1.send("LOGOUT_LOBBY\n");
    // (close は LOGOUT 後に発火する)

    const ws2 = await connectLobby(mf);
    const buf2 = readLineFromWebSocket(ws2);
    ws2.send("LOGIN_LOBBY bob+pool-c+white anything\n");
    expect(await buf2.takeLine()).toBe("LOGIN_LOBBY:bob OK");

    // alice は LOGOUT 済みなので bob は MATCHED されない。
    await expect(buf2.takeLine(300)).rejects.toThrow("timeout");
    ws2.close();
  });
});

/**
 * Issue #631: stale public queue entry を Alarm で purge する経路の smoke。
 * `LOBBY_QUEUE_ENTRY_TTL_SEC = 1` で短時間に stale 判定させ、`mf.runDurableObjectAlarm`
 * で alarm を駆動して `LOGIN_LOBBY:incorrect queue_expired` + WS close を検証する。
 */
describe("Lobby DO queue TTL purge (Issue #631)", () => {
  let mf: Miniflare;
  let cleanup: () => Promise<void>;

  beforeEach(async () => {
    const persist = await makeTempPersistRoot();
    cleanup = persist.cleanup;
    mf = await createMiniflare({
      persistRoot: persist.path,
      lobbyQueueEntryTtlSec: 1,
    });
  });

  afterEach(async () => {
    await mf.dispose();
    await cleanup();
  });

  test("LOBBY_PONG が来ない queued entry は alarm で queue_expired + close される", async () => {
    const ws = await connectLobby(mf);
    const buf = readLineFromWebSocket(ws);
    ws.send("LOGIN_LOBBY alice+pool-z+black anything\n");
    expect(await buf.takeLine()).toBe("LOGIN_LOBBY:alice OK");

    // TTL = 1 秒なので 1.2 秒後に alarm 発火させる。Miniflare の DO alarm は
    // 仮想時計を使わないため、wall-clock を進めるかわりに sleep で待つ。
    await new Promise((r) => setTimeout(r, 1200));

    // Lobby DO id は固定 ("default")。`getDurableObjectId` で取得して alarm を
    // 強制発火する。
    const ns = await mf.getDurableObjectNamespace("LOBBY");
    const id = ns.idFromName("default");
    const stub = ns.get(id);
    // miniflare の DurableObjectStub は `alarm()` を直接公開しない。代替として
    // 設定済の alarm が経過時刻になっていれば自動発火するため、再度 sleep して
    // alarm の処理結果 (queue_expired ライン受信) を待つ。
    void stub; // 参照は警告抑止のため確保

    expect(await buf.takeLine(3000)).toBe("LOGIN_LOBBY:incorrect queue_expired");
    // close 後の takeLine は connection closed で reject される。
    await expect(buf.takeLine(500)).rejects.toThrow();
  });

  test("LOBBY_PONG を送り続ける queued entry は purge されず待機を継続する", async () => {
    const ws = await connectLobby(mf);
    const buf = readLineFromWebSocket(ws);
    ws.send("LOGIN_LOBBY alice+pool-z+black anything\n");
    expect(await buf.takeLine()).toBe("LOGIN_LOBBY:alice OK");

    // 0.4 秒間隔で LOBBY_PONG を送り、TTL=1 秒を更新し続ける。
    const pongTimer = setInterval(() => {
      ws.send("LOBBY_PONG\n");
    }, 400);

    try {
      // 1.5 秒経っても queue_expired は来ないことを確認。
      await expect(buf.takeLine(1500)).rejects.toThrow("timeout");
    } finally {
      clearInterval(pongTimer);
      ws.close();
    }
  });
});
