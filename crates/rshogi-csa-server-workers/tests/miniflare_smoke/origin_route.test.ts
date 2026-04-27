import { afterEach, beforeEach, describe, expect, test } from "vitest";
import type { Miniflare } from "miniflare";
import { createMiniflare, makeTempPersistRoot } from "./harness";

/// Origin allowlist が WS Upgrade route で正しく機能するかを route レベルで固定する。
///
/// `OriginDecision` の単体テストは `crates/rshogi-csa-server-workers/src/origin.rs` 側
/// にあるが、router → evaluate → 403 / 101 の繋ぎ込みが回帰しないように
/// Miniflare 経由で 101 / 403 ステータスを直接確認する。
describe("Origin allowlist route behavior", () => {
  let mf: Miniflare;
  let cleanup: () => Promise<void>;

  beforeEach(async () => {
    const persist = await makeTempPersistRoot();
    cleanup = persist.cleanup;
    mf = await createMiniflare({
      persistRoot: persist.path,
      wsAllowedOrigins: "https://example.com",
    });
  });

  afterEach(async () => {
    await mf.dispose();
    await cleanup();
  });

  test("Origin ヘッダ欠落 → 素通しで 101 Upgrade を返す（ネイティブクライアント経路）", async () => {
    const res = await mf.dispatchFetch("https://example.com/ws/origin-missing-room", {
      headers: {
        Upgrade: "websocket",
      },
    });
    expect(res.status).toBe(101);
    expect(res.webSocket).toBeTruthy();
    // close() の前に accept() を呼ぶ契約。Miniflare 4 の `WebSocket` は
    // accept 前 close を拒否する。
    res.webSocket?.accept();
    res.webSocket?.close();
  });

  test("Origin が allowlist に完全一致 → 101 Upgrade", async () => {
    const res = await mf.dispatchFetch("https://example.com/ws/origin-match-room", {
      headers: {
        Upgrade: "websocket",
        Origin: "https://example.com",
      },
    });
    expect(res.status).toBe(101);
    expect(res.webSocket).toBeTruthy();
    // close() の前に accept() を呼ぶ契約。Miniflare 4 の `WebSocket` は
    // accept 前 close を拒否する。
    res.webSocket?.accept();
    res.webSocket?.close();
  });

  test("Origin が allowlist に含まれない → 403 Forbidden Origin", async () => {
    const res = await mf.dispatchFetch("https://example.com/ws/origin-mismatch-room", {
      headers: {
        Upgrade: "websocket",
        Origin: "https://evil.example",
      },
    });
    expect(res.status).toBe(403);
    expect(await res.text()).toContain("Forbidden Origin");
  });
});

describe("Origin allowlist route behavior (空 allowlist)", () => {
  let mf: Miniflare;
  let cleanup: () => Promise<void>;

  beforeEach(async () => {
    const persist = await makeTempPersistRoot();
    cleanup = persist.cleanup;
    mf = await createMiniflare({
      persistRoot: persist.path,
      wsAllowedOrigins: "",
    });
  });

  afterEach(async () => {
    await mf.dispose();
    await cleanup();
  });

  test("空 allowlist + Origin 欠落 → 101 Upgrade（production 既定でネイティブが通る）", async () => {
    const res = await mf.dispatchFetch("https://example.com/ws/empty-allow-missing-room", {
      headers: {
        Upgrade: "websocket",
      },
    });
    expect(res.status).toBe(101);
    expect(res.webSocket).toBeTruthy();
    // close() の前に accept() を呼ぶ契約。Miniflare 4 の `WebSocket` は
    // accept 前 close を拒否する。
    res.webSocket?.accept();
    res.webSocket?.close();
  });

  test("空 allowlist + Origin 付き → 403（ブラウザ経由は CSRF 防御で全拒否）", async () => {
    const res = await mf.dispatchFetch("https://example.com/ws/empty-allow-origin-room", {
      headers: {
        Upgrade: "websocket",
        Origin: "https://example.com",
      },
    });
    expect(res.status).toBe(403);
    expect(await res.text()).toContain("Forbidden Origin");
  });
});
