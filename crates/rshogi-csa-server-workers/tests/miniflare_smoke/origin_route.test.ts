import { request } from "node:http";
import { afterEach, beforeEach, describe, expect, test } from "vitest";
import type { Miniflare, WebSocket } from "miniflare";
import { DEFAULT_TEST_CF_CONNECTING_IP, createMiniflare, makeTempPersistRoot } from "./harness";

/**
 * Origin allowlist が WS Upgrade route で正しく機能するかを route レベルで固定する。
 *
 * `OriginDecision` の単体テストは `crates/rshogi-csa-server-workers/src/origin.rs` 側
 * にあるが、router → evaluate → 403 / 101 の繋ぎ込みが回帰しないように
 * Miniflare 経由で 101 / 403 ステータスを直接確認する。
 */
function closeAcceptedSocket(ws: WebSocket | null | undefined): void {
  // Miniflare は `accept()` を呼ばずに `close()` すると例外を投げる。Origin
  // 許可ケースで Upgrade を確認した後の cleanup を 1 行に揃えるためのヘルパ。
  ws?.accept();
  ws?.close();
}

/**
 * Upgrade を拒否する応答を読み、その接続を閉じてから返す。`mf.dispatchFetch` は 101 以外の応答の後も
 * 接続を開いたまま残し、dispose で workerd が終わるときに Windows で ECONNRESET の未処理例外になる。
 */
function fetchRejectedUpgrade(
  url: URL,
  headers: Record<string, string>,
): Promise<{ status: number; body: string }> {
  return new Promise((resolve, reject) => {
    const options = { agent: false, headers: { ...headers, Connection: "Upgrade" } };
    const req = request(url, options, (res) => {
      const { socket } = res;
      let body = "";
      res.setEncoding("utf8");
      res.on("data", (chunk) => { body += chunk; });
      res.once("error", reject);
      res.once("end", () => {
        socket.destroy();
        resolve({ status: res.statusCode!, body });
      });
    });
    req.once("upgrade", (res, socket) => {
      socket.destroy();
      reject(new Error(`expected the upgrade to be rejected, got ${res.statusCode}`));
    });
    req.once("error", reject);
    req.end();
  });
}

describe.each(["https://example.com", ""])("Origin allowlist %j", (allowlist) => {
  let mf: Miniflare;
  let cleanup: () => Promise<void>;
  beforeEach(async () => {
    const persist = await makeTempPersistRoot();
    cleanup = persist.cleanup;
    mf = await createMiniflare({ persistRoot: persist.path, wsAllowedOrigins: allowlist });
  });
  afterEach(async () => { await mf.dispose(); await cleanup(); });

  test("native access and browser allow/deny use the configured policy", async () => {
    for (const [index, origin] of [undefined, "https://example.com", "https://evil.example"].entries()) {
      const headers: Record<string, string> = {
        Upgrade: "websocket", "CF-Connecting-IP": DEFAULT_TEST_CF_CONNECTING_IP,
      };
      if (origin) headers.Origin = origin;
      const allowed = origin === undefined || origin === allowlist;
      if (allowed) {
        const res = await mf.dispatchFetch(`https://example.com/ws/origin-${index}`, { headers });
        expect(res.status).toBe(101);
        expect(res.webSocket).toBeTruthy(); closeAcceptedSocket(res.webSocket);
      } else {
        const res = await fetchRejectedUpgrade(new URL(`/ws/origin-${index}`, await mf.ready), headers);
        expect(res.status).toBe(403);
        expect(res.body).toContain("Forbidden Origin");
      }
    }
  });
});
