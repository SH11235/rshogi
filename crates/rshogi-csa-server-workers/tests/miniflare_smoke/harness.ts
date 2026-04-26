import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { Miniflare, type R2Bucket, type WebSocket } from "miniflare";

const WORKER_ROOT = resolve(import.meta.dirname, "../..");
const SHIM_PATH = resolve(WORKER_ROOT, "build/worker/shim.mjs");

export interface HarnessOptions {
  /// Miniflare の persist 先ディレクトリ。テスト並列実行や 2 回目の `vitest run`
  /// で R2 / DO storage が交差汚染しないよう、呼び出し側で一時ディレクトリを
  /// 切って必ず指定する契約。`makeTempPersistRoot()` のヘルパで作るのが基本経路。
  persistRoot: string;
  reconnectGraceSeconds?: number;
  allowFloodgateFeatures?: boolean;
  totalTimeSec?: number;
  byoyomiSec?: number;
  totalTimeMin?: number;
  byoyomiMin?: number;
  clockKind?: "countdown" | "fischer" | "stopwatch";
  corsOrigins?: string;
  adminHandle?: string;
}

export async function createMiniflare(opts: HarnessOptions): Promise<Miniflare> {
  const mf = new Miniflare({
    scriptPath: SHIM_PATH,
    modules: true,
    modulesRules: [
      { type: "ESModule", include: ["**/*.js", "**/*.mjs"], fallthrough: true },
      { type: "CompiledWasm", include: ["**/*.wasm"], fallthrough: true },
    ],
    compatibilityDate: "2026-04-21",
    durableObjects: {
      GAME_ROOM: { className: "GameRoom", useSQLite: true },
    },
    r2Buckets: ["KIFU_BUCKET", "FLOODGATE_HISTORY_BUCKET"],
    bindings: {
      CLOCK_KIND: opts.clockKind ?? "countdown",
      TOTAL_TIME_SEC: String(opts.totalTimeSec ?? 600),
      BYOYOMI_SEC: String(opts.byoyomiSec ?? 10),
      TOTAL_TIME_MIN: String(opts.totalTimeMin ?? 10),
      BYOYOMI_MIN: String(opts.byoyomiMin ?? 1),
      ADMIN_HANDLE: opts.adminHandle ?? "admin",
      RECONNECT_GRACE_SECONDS: String(opts.reconnectGraceSeconds ?? 0),
      ALLOW_FLOODGATE_FEATURES: opts.allowFloodgateFeatures ? "true" : "false",
      CORS_ORIGINS: opts.corsOrigins ?? "https://example.com",
    },
    defaultPersistRoot: opts.persistRoot,
  });
  await mf.ready;
  return mf;
}

export async function makeTempPersistRoot(): Promise<{
  path: string;
  cleanup: () => Promise<void>;
}> {
  const path = await mkdtemp(join(tmpdir(), "miniflare-smoke-"));
  return {
    path,
    cleanup: async () => {
      await rm(path, { recursive: true, force: true });
    },
  };
}

/// 棋譜 R2 オブジェクトを `game_id` 部分一致で待機列挙する。`KIFU_BUCKET` の
/// キー命名規則 (`YYYY/MM/DD/<game_id>.csa` 等) は `game_id` を prefix にしない
/// 階層形なので `R2.list({ prefix })` は使えず、全件列挙 + 後段 substring 一致で
/// 拾う。`game_id` は `<room_id>-<epoch_ms>` 形式で偶発的に他キーへ混入する
/// 可能性が実質ないため、substring 一致で十分。テスト用途で件数は数件想定、
/// page 跨ぎ (>1000 件) は視野外。
export async function pollR2ForGameId(
  bucket: R2Bucket,
  gameId: string,
  { timeoutMs = 5000, intervalMs = 100 }: { timeoutMs?: number; intervalMs?: number } = {},
): Promise<{ key: string }[]> {
  const deadline = Date.now() + timeoutMs;
  while (true) {
    const list = await bucket.list();
    const matched = list.objects
      .filter((o: { key: string }) => o.key.includes(gameId))
      .map((o: { key: string }) => ({ key: o.key }));
    if (matched.length > 0) return matched;
    if (Date.now() > deadline) {
      const seen = list.objects.map((o: { key: string }) => o.key);
      throw new Error(
        `R2 object for game_id=${gameId} not found within ${timeoutMs}ms; current keys: ${JSON.stringify(seen)}`,
      );
    }
    await new Promise((r) => setTimeout(r, intervalMs));
  }
}

export class CsaClient {
  private readonly buffer = new LineBuffer();
  private closed = false;
  private closeReason: { code?: number; reason?: string } | undefined;

  static async connect(mf: Miniflare, roomId: string, origin = "https://example.com"): Promise<CsaClient> {
    const url = `https://example.com/ws/${encodeURIComponent(roomId)}`;
    const res = await mf.dispatchFetch(url, {
      headers: {
        Upgrade: "websocket",
        Origin: origin,
      },
    });
    if (res.status !== 101 || !res.webSocket) {
      throw new Error(`expected 101 with webSocket, got ${res.status}: ${await res.text()}`);
    }
    const client = new CsaClient(res.webSocket);
    res.webSocket.accept();
    return client;
  }

  private constructor(private readonly ws: WebSocket) {
    ws.addEventListener("message", (ev) => {
      const data =
        typeof ev.data === "string" ? ev.data : new TextDecoder().decode(ev.data as ArrayBuffer);
      this.buffer.push(data);
    });
    ws.addEventListener("close", (ev) => {
      this.closed = true;
      this.closeReason = { code: ev.code, reason: ev.reason };
      this.buffer.markClosed();
    });
  }

  send(line: string): void {
    if (this.closed) throw new Error("CsaClient: cannot send on closed connection");
    this.ws.send(`${line}\n`);
  }

  async recvLine(timeoutMs = 5000): Promise<string> {
    return this.buffer.takeLine(timeoutMs);
  }

  async recvUntil(predicate: (line: string) => boolean, timeoutMs = 10_000): Promise<string[]> {
    const collected: string[] = [];
    const deadline = Date.now() + timeoutMs;
    while (true) {
      const remaining = deadline - Date.now();
      if (remaining <= 0) {
        throw new Error(`recvUntil timeout; collected so far: ${JSON.stringify(collected)}`);
      }
      const line = await this.recvLine(remaining);
      collected.push(line);
      if (predicate(line)) return collected;
    }
  }

  async drainGameSummary(timeoutMs = 10_000): Promise<string[]> {
    const lines = await this.recvUntil((l) => l === "END Game_Summary", timeoutMs);
    if (lines[0] !== "BEGIN Game_Summary") {
      throw new Error(`expected BEGIN Game_Summary; got ${JSON.stringify(lines)}`);
    }
    return lines;
  }

  close(): void {
    if (!this.closed) {
      this.ws.close();
      this.closed = true;
    }
  }

  isClosed(): boolean {
    return this.closed;
  }

  closeInfo(): { code?: number; reason?: string } | undefined {
    return this.closeReason;
  }
}

class LineBuffer {
  private text = "";
  private readonly queue: string[] = [];
  private readonly waiters: Array<{
    resolve: (line: string) => void;
    reject: (err: Error) => void;
  }> = [];
  private closed = false;

  push(chunk: string): void {
    this.text += chunk;
    while (true) {
      const idx = this.text.indexOf("\n");
      if (idx < 0) break;
      const line = this.text.slice(0, idx);
      this.text = this.text.slice(idx + 1);
      const w = this.waiters.shift();
      if (w) w.resolve(line);
      else this.queue.push(line);
    }
  }

  markClosed(): void {
    this.closed = true;
    while (this.waiters.length > 0) {
      const w = this.waiters.shift();
      w?.reject(new Error("connection closed"));
    }
  }

  takeLine(timeoutMs: number): Promise<string> {
    if (this.queue.length > 0) return Promise.resolve(this.queue.shift()!);
    if (this.closed) return Promise.reject(new Error("connection closed"));
    return new Promise<string>((resolve, reject) => {
      const entry = {
        resolve: (line: string) => {
          clearTimeout(timer);
          resolve(line);
        },
        reject: (err: Error) => {
          clearTimeout(timer);
          reject(err);
        },
      };
      const timer = setTimeout(() => {
        const idx = this.waiters.indexOf(entry);
        if (idx >= 0) this.waiters.splice(idx, 1);
        reject(new Error(`recvLine timeout after ${timeoutMs}ms`));
      }, timeoutMs);
      this.waiters.push(entry);
    });
  }
}
