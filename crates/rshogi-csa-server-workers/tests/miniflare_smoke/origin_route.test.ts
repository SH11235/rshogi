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
  // Miniflare 4 は `accept()` を呼ばずに `close()` すると例外を投げる。Origin
  // 許可ケースで Upgrade を確認した後の cleanup を 1 行に揃えるためのヘルパ。
  ws?.accept();
  ws?.close();
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
        expect(res.webSocket).toBeTruthy(); closeAcceptedSocket(res.webSocket);
      } else { expect(await res.text()).toContain("Forbidden Origin"); }
    }
  });
});
