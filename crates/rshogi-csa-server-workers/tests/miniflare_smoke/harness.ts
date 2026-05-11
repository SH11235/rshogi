import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { Miniflare, type WebSocket } from "miniflare";

/// `mf.getR2Bucket(...)` の戻り値は miniflare 4 が `ReplaceWorkersTypes<R2Bucket>`
/// (大きな conditional type) として返し、tsc の構造的推論で `Request_2` などに
/// 潰れる既知のエッジケースがある。`R2Bucket` 自体は miniflare の barrel export
/// 対象外でもあり、`@cloudflare/workers-types` を別途 devDep に入れたくない
/// (テスト用途には過剰)。本ハーネスは `list` / `get` の最小サブセットしか
/// 使わないため、duck-type interface を 1 か所定義して `getKifuBucket` で
/// キャストを隠蔽する。
///
/// 引数 / 戻り値を最小だけ広げてあるのは、将来 miniflare 4 が `list()` の
/// 既定挙動を変えても (例: page size 規定値変更 / `truncated` 必須化) 局所
/// 改修で追随できるようにするため。
export interface R2ListOptions {
  prefix?: string;
  cursor?: string;
  limit?: number;
}

export interface R2ListResult {
  objects: Array<{ key: string }>;
  truncated?: boolean;
  cursor?: string;
}

export interface R2BucketLike {
  list(options?: R2ListOptions): Promise<R2ListResult>;
  get(key: string): Promise<R2ObjectLike | null>;
}

export interface R2ObjectLike {
  text(): Promise<string>;
}

export async function getKifuBucket(mf: Miniflare): Promise<R2BucketLike> {
  return getR2BucketByBinding(mf, "KIFU_BUCKET");
}

export async function getFloodgateHistoryBucket(mf: Miniflare): Promise<R2BucketLike> {
  return getR2BucketByBinding(mf, "FLOODGATE_HISTORY_BUCKET");
}

async function getR2BucketByBinding(mf: Miniflare, binding: string): Promise<R2BucketLike> {
  const raw = (await mf.getR2Bucket(binding)) as unknown as R2BucketLike;
  // 将来 miniflare のメジャーアップで `list` / `get` が rename された場合に
  // 型崩落で silent に壊れる経路を避けるため、duck-type 違反を runtime で
  // 早期検出する。型 cast (`as unknown as R2BucketLike`) を使っている以上、
  // 整合性検証は実装側で持つ責務。
  if (typeof raw.list !== "function" || typeof raw.get !== "function") {
    throw new Error(
      `miniflare R2Bucket API (${binding}) に list/get が見つからない: ` +
        "miniflare メジャーアップで API rename された可能性。harness の duck-type を更新する必要あり",
    );
  }
  return raw;
}

const WORKER_ROOT = resolve(import.meta.dirname, "../..");
const SHIM_PATH = resolve(WORKER_ROOT, "build/worker/shim.mjs");

export interface HarnessOptions {
  /// Miniflare の persist 先ディレクトリ。テスト並列実行や 2 回目の `vitest run`
  /// で R2 / DO storage が交差汚染しないよう、呼び出し側で一時ディレクトリを
  /// 切って必ず指定する契約。`makeTempPersistRoot()` のヘルパで作るのが基本経路。
  persistRoot: string;
  reconnectGraceSeconds?: number;
  /// AGREE 待ち TTL (秒)。`start_match` 直後に予約され、両者 AGREE で cancel
  /// される (Issue #600)。テストで stuck 経路を再現する際は短めの値 (例: 1) を
  /// 渡し、`alarm()` 発火で部屋解放されることを assert する。未指定時は server
  /// 側既定 ([`config::DEFAULT_AGREE_TIMEOUT_SEC`] = 60) にフォールバック。
  agreeTimeoutSeconds?: number;
  allowFloodgateFeatures?: boolean;
  totalTimeSec?: number;
  byoyomiSec?: number;
  totalTimeMs?: number;
  byoyomiMs?: number;
  totalTimeMin?: number;
  byoyomiMin?: number;
  clockKind?: "countdown" | "countdown_msec" | "fischer" | "stopwatch";
  wsAllowedOrigins?: string;
  adminApiToken?: string;
  /// `WORKERS_HANDLE_AUTH` whitelist (issue #664) のオーバーライド。
  /// 既定は空配列 (`"[]"`) で「whitelist 未宣言」として self-claim 既定挙動を
  /// 維持する (一般 smoke は無影響)。検証時は
  /// `[{"handle":"alice","password_sha256":"<lowercase hex 64 chars>"}, ...]`
  /// 形式 JSON 文字列を渡す。env JSON 不正経路 (fail-closed) を確認したいとき
  /// は `"broken"` 等を渡す。
  workersHandleAuth?: string;
  lobbyQueueSizeLimit?: number;
  /// 公開 lobby queue entry の TTL (秒、Issue #631)。`LOBBY_PONG` 受信から本値
  /// を超えた entry は alarm で stale 判定され `LOGIN_LOBBY:incorrect queue_expired`
  /// + WS close される。未指定時は server 既定 (300 秒) にフォールバック。
  /// stale purge を smoke で再現するときは短い値 (例: 1) を渡す。
  lobbyQueueEntryTtlSec?: number;
  /// 私的対局 (`CHALLENGE_LOBBY` / `LOGIN_LOBBY+private-...+free`) feature gate
  /// (Issue #635)。未指定時は production と同じ `"false"` (= 早期 reject)。
  /// 私的対局経路を smoke で通電させるテストでは `true` を渡す。
  privateChallengeEnabled?: boolean;
  /// Rate limit 閾値オーバーライド (issue #622 PR3a, 1 IP / 1 handle あたり 1 分間)。
  /// 未指定時は **緩和済 default** ([`DEFAULT_LOOSENED_RATE_LIMIT_PER_MIN`]) を
  /// 全 6 keys に適用し、rate limit を検証していない既存 smoke (`smoke.test.ts`
  /// 等) が偶発的に denial に当たらないようにする。`rate_limit.test.ts` のみ
  /// 意図的に小さい値を渡してテストする。
  rateLimitOverrides?: {
    lobbyLoginPerIpPerMin?: number;
    lobbyLoginPerHandlePerMin?: number;
    lobbyChallengePerIpPerMin?: number;
    lobbyChallengePerHandlePerMin?: number;
    roomCreatePerIpPerMin?: number;
    wsRoomUpgradePerIpPerMin?: number;
  };
}

/// 緩和済 default 閾値 (`rateLimitOverrides` 未指定時に harness が注入する値)。
/// 10_000/分 = 通常の smoke (1 test ~10 接続) では絶対に denial に当たらない上限。
/// `rate_limit.test.ts` 側で意図的に小さな値 (`2`-`6` 程度) を渡してテストする。
export const DEFAULT_LOOSENED_RATE_LIMIT_PER_MIN = 10_000;

/// テスト用デフォルト CF-Connecting-IP (`CsaClient.connect` / `connectLobby` の
/// 既定値)。CF-Connecting-IP 欠落で fail-closed に倒れる経路を rate_limit smoke
/// test 以外では踏まないよう、harness 側で必ず注入する契約 (issue #622 PR3a)。
/// IP 値そのものは任意 (本テスト環境では loopback 想定の `127.0.0.1`)。
export const DEFAULT_TEST_CF_CONNECTING_IP = "127.0.0.1";

export async function createMiniflare(opts: HarnessOptions): Promise<Miniflare> {
  const rl = opts.rateLimitOverrides ?? {};
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
      LOBBY: { className: "Lobby", useSQLite: true },
      // RateLimiter (issue #622 PR3a): per-(kind, identifier) で sharding する
      // atomic token bucket DO。Miniflare 4 の `useSQLite: true` は production
      // wrangler.toml の `[[migrations]] new_sqlite_classes = ["RateLimiter"]`
      // (tag = "v3") と整合する。
      RATE_LIMITER: { className: "RateLimiter", useSQLite: true },
    },
    r2Buckets: ["KIFU_BUCKET", "FLOODGATE_HISTORY_BUCKET"],
    bindings: {
      CLOCK_KIND: opts.clockKind ?? "countdown",
      TOTAL_TIME_SEC: String(opts.totalTimeSec ?? 600),
      BYOYOMI_SEC: String(opts.byoyomiSec ?? 10),
      TOTAL_TIME_MS: String(opts.totalTimeMs ?? 600_000),
      BYOYOMI_MS: String(opts.byoyomiMs ?? 10_000),
      TOTAL_TIME_MIN: String(opts.totalTimeMin ?? 10),
      BYOYOMI_MIN: String(opts.byoyomiMin ?? 1),
      // `ADMIN_API_TOKEN` 既定値は Worker 側 `wrangler.toml.example` の placeholder
      // と同じで、Miniflare 配下では admin auth は **configured** な状態で動く。
      // 既定のまま test を書くと `verify_admin_token_str` は提供 token と placeholder
      // の比較結果 (`TokenMismatch` または `MissingCredential`) を返す経路に乗る。
      // 強制的に `TokenNotConfigured` (fail-closed) 経路を確認したいときは
      // `adminApiToken: ""` を明示すること。`%%ADMIN <token>` 成功 E2E の場合は
      // 同 token を `adminApiToken` で揃えて binding する。
      ADMIN_API_TOKEN: opts.adminApiToken ?? "local-dev-admin-token-placeholder",
      // `WORKERS_HANDLE_AUTH` 既定値は wrangler.toml.example と揃えた空配列で、
      // whitelist 未宣言モードとして self-claim 既定挙動を維持する。既存 smoke
      // が偶発的に handle_auth_failed に当たる経路を踏まないようにする。
      // 検証時のみ `handle_auth.test.ts` 等から JSON 文字列を渡して whitelist
      // を有効化する。
      WORKERS_HANDLE_AUTH: opts.workersHandleAuth ?? "[]",
      // `reconnectGraceSeconds: 0` 既定は production wrangler.production.toml の
      // `RECONNECT_GRACE_SECONDS = "0"` と整合し、再接続プロトコル無効構成を表す。
      // テストで `30` 等を明示する場合のみ Game_Summary に `Reconnect_Token:` 行が
      // 出て reconnect 経路が有効化される。Issue #591 hotfix 後は server 側 token
      // 配布も grace に応じた gate を通る。
      RECONNECT_GRACE_SECONDS: String(opts.reconnectGraceSeconds ?? 0),
      // 未指定時は空文字を渡して server 側 fallback (= 60 秒既定) を使う。
      // 数値を指定したテストでは start_match 直後に AGREE 待ち alarm が予約される。
      AGREE_TIMEOUT_SECONDS:
        opts.agreeTimeoutSeconds === undefined ? "" : String(opts.agreeTimeoutSeconds),
      ALLOW_FLOODGATE_FEATURES: opts.allowFloodgateFeatures ? "true" : "false",
      WS_ALLOWED_ORIGINS: opts.wsAllowedOrigins ?? "https://example.com",
      LOBBY_QUEUE_SIZE_LIMIT: String(opts.lobbyQueueSizeLimit ?? 100),
      // 未指定時は空文字を渡して server 側 fallback (= 300 秒既定) を使う。
      // 数値を指定したテストでは alarm 経路で stale entry が purge される。
      LOBBY_QUEUE_ENTRY_TTL_SEC:
        opts.lobbyQueueEntryTtlSec === undefined ? "" : String(opts.lobbyQueueEntryTtlSec),
      PRIVATE_CHALLENGE_ENABLED: opts.privateChallengeEnabled ? "true" : "false",
      // Rate limit (issue #622 PR3a): 全 6 keys に緩和済 default を入れて、
      // 既存 smoke が偶発的に denial に当たる経路を踏まないようにする。
      // 個別テストは `rateLimitOverrides` で意図的に小さな値を上書きする。
      LOBBY_LOGIN_RATE_PER_IP_PER_MIN: String(
        rl.lobbyLoginPerIpPerMin ?? DEFAULT_LOOSENED_RATE_LIMIT_PER_MIN,
      ),
      LOBBY_LOGIN_RATE_PER_HANDLE_PER_MIN: String(
        rl.lobbyLoginPerHandlePerMin ?? DEFAULT_LOOSENED_RATE_LIMIT_PER_MIN,
      ),
      LOBBY_CHALLENGE_RATE_PER_IP_PER_MIN: String(
        rl.lobbyChallengePerIpPerMin ?? DEFAULT_LOOSENED_RATE_LIMIT_PER_MIN,
      ),
      LOBBY_CHALLENGE_RATE_PER_HANDLE_PER_MIN: String(
        rl.lobbyChallengePerHandlePerMin ?? DEFAULT_LOOSENED_RATE_LIMIT_PER_MIN,
      ),
      ROOM_CREATE_RATE_PER_IP_PER_MIN: String(
        rl.roomCreatePerIpPerMin ?? DEFAULT_LOOSENED_RATE_LIMIT_PER_MIN,
      ),
      WS_ROOM_UPGRADE_RATE_PER_IP_PER_MIN: String(
        rl.wsRoomUpgradePerIpPerMin ?? DEFAULT_LOOSENED_RATE_LIMIT_PER_MIN,
      ),
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
  bucket: R2BucketLike,
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

/// `FLOODGATE_HISTORY_BUCKET` 配下を `floodgate-history/` prefix で列挙し、
/// `game_id` を含むキーを待機列挙する。終局確定後に DO の `try_persist_floodgate_history`
/// が put を完了するまでの race を吸収するため、polling の deadline / interval を
/// 設ける。`pollR2ForGameId` (`KIFU_BUCKET`) と分離しているのは、prefix を絞った
/// list をデフォルトにして他テストの kifu キーが混ざらないようにするため。
export async function pollFloodgateHistoryForGameId(
  bucket: R2BucketLike,
  gameId: string,
  { timeoutMs = 5000, intervalMs = 100 }: { timeoutMs?: number; intervalMs?: number } = {},
): Promise<{ key: string }[]> {
  const deadline = Date.now() + timeoutMs;
  while (true) {
    const list = await bucket.list({ prefix: "floodgate-history/" });
    const matched = list.objects
      .filter((o: { key: string }) => o.key.includes(gameId))
      .map((o: { key: string }) => ({ key: o.key }));
    if (matched.length > 0) return matched;
    if (Date.now() > deadline) {
      const seen = list.objects.map((o: { key: string }) => o.key);
      throw new Error(
        `floodgate history object for game_id=${gameId} not found within ${timeoutMs}ms; ` +
          `current keys under floodgate-history/: ${JSON.stringify(seen)}`,
      );
    }
    await new Promise((r) => setTimeout(r, intervalMs));
  }
}

export class CsaClient {
  private readonly buffer = new LineBuffer();
  private closed = false;
  private closeReason: { code?: number; reason?: string } | undefined;

  static async connect(
    mf: Miniflare,
    roomId: string,
    origin = "https://example.com",
    /// CF-Connecting-IP のテスト注入。issue #622 PR3a で `/ws/<room_id>` upgrade
    /// は本ヘッダ必須 (欠落で 503 fail-closed)。Miniflare は edge proxy を
    /// シミュレートしないため、test 側で必ず注入する。同 IP からの連続接続は
    /// `WS_ROOM_UPGRADE_RATE_PER_IP_PER_MIN` / `ROOM_CREATE_RATE_PER_IP_PER_MIN`
    /// バケットを共有する点に注意 (test ごとに異なる IP を渡せば bucket 隔離)。
    cfConnectingIp: string = DEFAULT_TEST_CF_CONNECTING_IP,
  ): Promise<CsaClient> {
    const url = `https://example.com/ws/${encodeURIComponent(roomId)}`;
    const res = await mf.dispatchFetch(url, {
      headers: {
        Upgrade: "websocket",
        Origin: origin,
        "CF-Connecting-IP": cfConnectingIp,
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

  /// クライアント側で WS を閉じ、サーバ側 close ハンドラ (`websocket_close`)
  /// が走り終わるまで待つ。再接続シナリオでは「サーバが切断を観測してから
  /// `enter_grace_window` が走る」ことに依存するため、close を fire-and-forget
  /// すると後続の reconnect リクエストが grace 登録より先に到着して race する。
  /// `addEventListener("close", ...)` を constructor で予約済みなので、それと
  /// 同経路の close 通知を await できるよう Promise を返す。
  ///
  /// timeout 時は **resolve** で抜ける (throw しない)。テストの `afterEach` で
  /// `mf.dispose()` が必ず呼ばれて WS は強制破棄されるため、close ack が
  /// 帰らない異常経路でも cleanup は別経路で完結する。close を fail-loud に
  /// すると afterEach 自体が落ちて test の本当の失敗原因が埋もれるので、
  /// cleanup 側の不確実性は本関数で吸収する設計に倒している。
  async close(timeoutMs = 2000): Promise<void> {
    if (this.closed) return;
    // server-initiated close (`game_room.rs` の終局時 close 等) と同じ tick で
    // 呼ばれるケースでは、`addEventListener` を貼る前に WS が CLOSING/CLOSED
    // へ遷移しており、close event は再 dispatch されずに `Promise` が timeout
    // 一杯を空待ちすることがある。`readyState` で早期 return して 2000ms の
    // 浪費を防ぐ。`READY_STATE_CLOSING = 2` / `READY_STATE_CLOSED = 3` は
    // miniflare 4 の static 定数。
    const state = this.ws.readyState;
    if (state === 2 /* CLOSING */ || state === 3 /* CLOSED */) {
      return;
    }
    return await new Promise<void>((resolve) => {
      const timer = setTimeout(() => resolve(), timeoutMs);
      this.ws.addEventListener(
        "close",
        () => {
          clearTimeout(timer);
          resolve();
        },
        { once: true },
      );
      this.ws.close();
    });
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
