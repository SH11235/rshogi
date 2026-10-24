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
async function closeAcceptedSocket(ws: WebSocket | null | undefined): Promise<void> {
  if (!ws) return;
  ws.accept();
  await new Promise<void>((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error("WebSocket close acknowledgement timed out")), 5000);
    ws.addEventListener("close", () => { clearTimeout(timer); resolve(); }, { once: true });
    ws.close();
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
      const res = await mf.dispatchFetch(`https://example.com/ws/origin-${index}`, { headers });
      const allowed = origin === undefined || origin === allowlist;
      expect(res.status).toBe(allowed ? 101 : 403);
      if (allowed) {
        expect(res.webSocket).toBeTruthy(); await closeAcceptedSocket(res.webSocket);
      } else { expect(await res.text()).toContain("Forbidden Origin"); }
    }
  });
});
