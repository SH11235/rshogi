import { createHash } from "node:crypto";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { type Miniflare, Response as MfResponse } from "miniflare";
import { createMiniflare, makeTempPersistRoot } from "./harness.ts";

const TOKEN = "isolated-backup-admin-token";
const ORIGIN = "https://example.com";
const STATE_KEY = "control/kifu-backup-v1.json";
const DATE = "2026-10-21";
const START = Date.parse(`${DATE}T10:00:00Z`);
const CSA = "V2.2\nN+alice\nN-bob\nPI\n+\n%TORYO\n";
const sha = (bytes: string | Uint8Array) => createHash("sha256").update(bytes).digest("hex");
const WDOOR_ID = "wdoor+floodgate-300-10F+RAMU+Panda+20261009000001";
const WDOOR_URL = `https://wdoor.c.u-tokyo.ac.jp/shogi/x/2026/10/09/${WDOOR_ID}.csa`;

interface Bucket {
  put(key: string, value: string): Promise<unknown>;
  get(key: string): Promise<{ text(): Promise<string>; json<T>(): Promise<T>; arrayBuffer(): Promise<ArrayBuffer> } | null>;
  delete(keys: string | string[]): Promise<void>;
  list(options?: { prefix?: string; limit?: number }): Promise<{ objects: { key: string }[] }>;
}
interface State {
  after_key: string | null;
  cycle_started_ms: number;
  completed_at_ms: number;
  last_run_ms: number;
  lease_until_ms: number;
  verified: number;
  unverified_legacy: number;
  last_error: string | null;
  last_alert_ms: number;
  notification: string;
}
interface Outcome {
  enabled: boolean;
  busy: boolean;
  processed: number;
  cycle_complete: boolean;
  state: State;
}

function meta(id: string, source = "kifu", started = START, digest = true, csa = CSA) {
  return {
    game_id: id, started_at_ms: started, ended_at_ms: started + 1000,
    black_handle: "alice", white_handle: "bob", moves_count: 0,
    result_kind: "WIN_WHITE", end_reason: "RESIGN", source,
    ...(digest ? { csa_sha256: sha(csa) } : {}),
  };
}

describe("Miniflare: versioned kifu backup and restore", () => {
  let mf: Miniflare;
  let primary: Bucket;
  let backup: Bucket;
  let cleanup: () => Promise<void>;
  let alerts: { url: string; body: string; auth: string | null }[];
  let alertStatus: number;
  let wdoorCsa: Uint8Array | undefined;
  let wdoorRequests: string[];

  beforeEach(async () => {
    const persist = await makeTempPersistRoot();
    cleanup = persist.cleanup;
    alerts = [];
    alertStatus = 200;
    wdoorCsa = undefined;
    wdoorRequests = [];
    mf = await createMiniflare({
      persistRoot: persist.path, kifuBackup: true, adminApiToken: TOKEN,
      kifuBackupAlertUrl: "https://notification.invalid/backup",
      scriptPath: resolve(import.meta.dirname, "backup-worker.mjs"),
      outboundService: async (request) => {
        if (request.url === WDOOR_URL) {
          wdoorRequests.push(request.url);
          return new MfResponse(wdoorCsa ?? "missing fixture", { status: wdoorCsa ? 200 : 404 });
        }
        alerts.push({ url: request.url, body: await request.text(), auth: request.headers.get("Authorization") });
        return new MfResponse("notification fixture", { status: alertStatus });
      },
    });
    primary = await mf.getR2Bucket("KIFU_BUCKET") as unknown as Bucket;
    backup = await mf.getR2Bucket("KIFU_BACKUP_BUCKET") as unknown as Bucket;
  });

  afterEach(async () => { await mf?.dispose(); await cleanup?.(); });

  async function api(action: string, method = "GET", params: Record<string, string> = {}) {
    const url = new URL(`/api/v1/admin/kifu-backup/${action}`, ORIGIN);
    url.search = new URLSearchParams(params).toString();
    return mf.dispatchFetch(url.toString(), { method, headers: { Authorization: `Bearer ${TOKEN}` } });
  }
  async function run(): Promise<Outcome> {
    const response = await api("run", "POST");
    expect(response.status).toBe(200);
    return response.json() as Promise<Outcome>;
  }
  async function snapshots(): Promise<string[]> {
    return (await backup.list({ prefix: "snapshots/", limit: 1000 })).objects.map(({ key }) => key);
  }
  async function seed(id: string, source = "kifu", started = START, digest = true) {
    const metadata = JSON.stringify(meta(id, source, started, digest));
    await primary.put(`kifu-by-id/${id}.csa`, CSA);
    await primary.put(`kifu-by-id/${id}.meta.json`, metadata);
    if (source === "kifu" || source === "floodgate") {
      await primary.put(`${new Date(started).toISOString().slice(0, 10).replaceAll("-", "/")}/${id}.csa`, CSA);
    }
    return metadata;
  }
  async function faults(value: Record<string, unknown>) {
    const response = await mf.dispatchFetch(`${ORIGIN}/__test/faults`, { method: "POST", body: JSON.stringify(value) });
    expect(response.status).toBe(200);
  }
  async function removePrimary() {
    const keys = (await primary.list({ limit: 1000 })).objects.map(({ key }) => key);
    if (keys.length) await primary.delete(keys);
  }
  async function onlySnapshot(): Promise<string> {
    const keys = await snapshots();
    expect(keys).toHaveLength(1);
    return keys[0]!;
  }

  it("authenticates every action, rejects browser origins and unsafe keys", async () => {
    const denied = await mf.dispatchFetch(`${ORIGIN}/api/v1/admin/kifu-backup/status`);
    expect(denied.status).toBe(403);
    const browser = await mf.dispatchFetch(`${ORIGIN}/api/v1/admin/kifu-backup/status`, {
      headers: { Authorization: `Bearer ${TOKEN}`, Origin: ORIGIN },
    });
    expect(browser.status).toBe(403);
    expect((await api("restore", "POST", { key: "control/overwrite", apply: "true" })).status).toBe(400);
    expect((await api("snapshots", "GET", { date: "2026-02-30" })).status).toBe(400);
    expect((await api("status")).headers.get("Cache-Control")).toBe("no-store");
  });

  it("copies one consistent version and retries without overwriting locked snapshots", async () => {
    const metadata = await seed("game-1");
    const first = await run();
    expect(first.cycle_complete).toBe(true);
    expect(first.state.verified).toBe(1);
    const key = await onlySnapshot();
    const stored = await (await backup.get(key))!.json<{ csa: string; meta_json: string; verification: string }>();
    expect(stored).toMatchObject({ csa: CSA, meta_json: metadata, verification: "digest" });
    const repeated = await run();
    expect(repeated.state.last_error).toBeNull();
    expect(await snapshots()).toEqual([key]);
    expect(alerts).toHaveLength(0);
  });

  it("backs up and restores non-UTF-8 bytes through import-wdoor without stopping later games", async () => {
    const text = ["V2", "N+RAMU", "N-Panda", "'Increment:10", `$EVENT:${WDOOR_ID}`,
      "$START_TIME:2026/10/09 00:00:00", "PI", "+", "+2726FU", "T0", "-8384FU", "T10",
      "%TORYO", "'summary:toryo:RAMU lose:Panda win", ""].join("\n");
    wdoorCsa = Buffer.concat([Buffer.from(text), Buffer.from([0x27, 0x81, 0xff, 0x00, 0x0a])]);
    const imported = await mf.dispatchFetch(`${ORIGIN}/api/v1/admin/games/import-wdoor`, {
      method: "POST", headers: { Authorization: `Bearer ${TOKEN}`, "Content-Type": "application/json" },
      body: JSON.stringify({ wdoor_game_id: WDOOR_ID }),
    });
    expect(imported.status).toBe(200);
    expect(wdoorRequests).toEqual([WDOOR_URL]);
    const { game_id: id } = await imported.json() as { game_id: string };
    const csaKey = `kifu-by-id/${id}.csa`;
    const metaKey = `kifu-by-id/${id}.meta.json`;
    const metadata = await (await primary.get(metaKey))!.text();
    expect(JSON.parse(metadata).csa_sha256).toBe(sha(wdoorCsa));
    expect(Buffer.from(await (await primary.get(csaKey))!.arrayBuffer())).toEqual(wdoorCsa);
    await seed("zz-after-nonutf8", "upload");
    const outcome = await run();
    expect(outcome.state.last_error).toBeNull();
    expect(outcome.state.verified).toBe(2);
    expect(outcome.cycle_complete).toBe(true);
    const keys = await snapshots();
    expect(keys).toHaveLength(2);
    const key = keys.find(candidate => candidate.includes(`/${id}/`))!;
    const stored = await (await backup.get(key))!.json<{ csa: { base64: string }; meta_json: string }>();
    expect(Buffer.from(stored.csa.base64, "base64")).toEqual(wdoorCsa);
    expect(stored.meta_json).toBe(metadata);
    expect((await run()).state.last_error).toBeNull();
    expect(await snapshots()).toEqual(keys);
    await removePrimary();
    const db = await mf.getD1Database("GAMES_SEARCH_DB");
    await db.prepare("DELETE FROM games_search_index WHERE game_id = ?").bind(id).run();
    expect((await api("restore", "POST", { key })).status).toBe(200);
    expect((await primary.list()).objects).toHaveLength(0);
    expect((await api("restore", "POST", { key, apply: "true" })).status).toBe(200);
    expect(Buffer.from(await (await primary.get(csaKey))!.arrayBuffer())).toEqual(wdoorCsa);
    expect(await (await primary.get(metaKey))!.text()).toBe(metadata);
    expect(await db.prepare("SELECT game_id FROM games_search_index WHERE game_id = ?").bind(id).first()).toMatchObject({ game_id: id });
    expect(alerts).toHaveLength(0);
  });

  it("legacy notifications do not throttle a new failure or lag alert", async () => {
    await seed("a-legacy", "wdoor", START, false);
    await run();
    expect(alerts).toHaveLength(1);
    await seed("zz-failure", "upload");
    await faults({ backupPut: true });
    const failed = await run();
    expect(failed.state.last_error).toBeTruthy();
    expect(alerts).toHaveLength(2);
    const stale = Date.now() - 25 * 60 * 60_000;
    await backup.put(STATE_KEY, JSON.stringify({ ...failed.state, completed_at_ms: stale }));
    await faults({ backupPut: true });
    expect((await run()).state.last_error).toBeTruthy();
    expect(alerts).toHaveLength(3);
    expect(alerts[2]?.body).toContain("lagging=true");
    expect((await run()).state.last_error).toBeNull();
  });

  it("limits persistent legacy reminders to one per day", async () => {
    await seed("legacy-reminder", "wdoor", START, false);
    const first = await run();
    expect(alerts).toHaveLength(1);
    const recent = Date.now() - 2 * 60 * 60_000;
    await backup.put(STATE_KEY, JSON.stringify({ ...first.state, last_alert_ms: recent, last_legacy_alert_ms: recent }));
    const repeated = await run();
    expect(repeated.state.last_error).toBeNull();
    expect(alerts).toHaveLength(1);
    const expired = Date.now() - 25 * 60 * 60_000;
    await backup.put(STATE_KEY, JSON.stringify({ ...repeated.state, last_alert_ms: expired, last_legacy_alert_ms: expired }));
    await run();
    expect(alerts).toHaveLength(2);
  });

  it("retries a failed snapshot before advancing the cursor and throttles alerts", async () => {
    await seed("retry-1");
    await faults({ backupPut: true });
    const failed = await run();
    expect(failed.state.last_error).toBeTruthy();
    expect(failed.state.after_key).toBe("kifu-by-id/retry-1.csa");
    expect(failed.state.notification).toBe("delivered");
    expect(alerts).toHaveLength(1);
    expect(alerts[0]?.auth).toBeNull();
    expect(alerts[0]?.body).not.toContain(TOKEN);
    await faults({ backupPut: true });
    expect((await run()).state.last_error).toBeTruthy();
    expect(alerts).toHaveLength(1);
    expect((await run()).state.last_error).toBeNull();
    await onlySnapshot();
  });

  it("keeps a missing source visible and retries it before later games", async () => {
    await seed("a-missing", "upload");
    await primary.delete("kifu-by-id/a-missing.csa");
    await seed("z-later", "upload");
    const first = await run();
    expect(first.state.last_error).toContain("source CSA missing");
    expect(first.state.after_key).toBeNull();
    const repeated = await run();
    expect(repeated.state.last_error).toContain("source CSA missing");
    expect(repeated.state.after_key).toBe(first.state.after_key);
    expect(await snapshots()).toHaveLength(0);
    await primary.put("kifu-by-id/a-missing.csa", CSA);
    const recovered = await run();
    expect(recovered.state.last_error).toBeNull();
    expect(recovered.state.notification).toBe("idle");
    expect(recovered.state.verified).toBe(2);
    expect(recovered.cycle_complete).toBe(true);
    expect(await snapshots()).toHaveLength(2);
  });

  it("refuses mixed force-import versions and keeps a later verified version", async () => {
    await seed("external-1", "wdoor");
    await run();
    const first = await onlySnapshot();
    const changed = `${CSA}'new version\n`;
    await primary.put("kifu-by-id/external-1.csa", changed);
    expect((await run()).state.last_error).toContain("digest mismatch");
    expect(await snapshots()).toEqual([first]);
    await primary.put("kifu-by-id/external-1.meta.json", JSON.stringify(meta("external-1", "wdoor", START, true, changed)));
    expect((await run()).state.last_error).toBeNull();
    expect(await snapshots()).toHaveLength(2);
  });

  it("records failed notifications without losing the retry position", async () => {
    await seed("alert-1");
    alertStatus = 500;
    await faults({ backupPut: true });
    const failed = await run();
    expect(failed.state.notification).toBe("failed");
    expect(failed.state.last_error).toBeTruthy();
    expect(failed.state.after_key).toBe("kifu-by-id/alert-1.csa");
    expect((await run()).state.last_error).toBeNull();
    await onlySnapshot();
  });

  it("preserves legacy external data but refuses automatic restore", async () => {
    await seed("legacy-external", "wdoor", START, false);
    const copied = await run();
    expect(copied.state.unverified_legacy).toBe(1);
    expect(alerts).toHaveLength(1);
    const key = await onlySnapshot();
    await removePrimary();
    const response = await api("restore", "POST", { key, apply: "true" });
    expect(response.status).toBe(409);
    expect((await primary.list()).objects).toHaveLength(0);
  });

  it("checks both legacy native copies and restores a single game plus D1 idempotently", async () => {
    const metadata = await seed("native-legacy", "kifu", START, false);
    await primary.put("2026/10/21/native-legacy.csa", "different");
    expect((await run()).state.last_error).toContain("copies disagree");
    await primary.put("2026/10/21/native-legacy.csa", CSA);
    expect((await run()).state.verified).toBe(1);
    const key = await onlySnapshot();
    await removePrimary();
    const dry = await api("restore", "POST", { key });
    expect(await dry.json()).toMatchObject({ applied: false, game_id: "native-legacy" });
    expect((await primary.list()).objects).toHaveLength(0);
    const applied = await api("restore", "POST", { key, apply: "true" });
    expect(applied.status).toBe(200);
    expect((await primary.list()).objects).toHaveLength(4);
    const metaKey = "kifu-by-id/native-legacy.meta.json";
    const indexKey = (await primary.list({ prefix: "games-index/" })).objects[0]!.key;
    expect(await (await primary.get(metaKey))!.text()).toBe(metadata);
    expect(await (await primary.get(indexKey))!.text()).toBe(metadata);
    const db = await mf.getD1Database("GAMES_SEARCH_DB");
    expect(await db.prepare("SELECT game_id FROM games_search_index WHERE game_id = ?").bind("native-legacy").first()).toMatchObject({ game_id: "native-legacy" });
    expect((await api("restore", "POST", { key, apply: "true" })).status).toBe(200);
  });

  it("does not change other targets on conflict and resumes partial restore failure", async () => {
    await seed("restore-1");
    await run();
    const key = await onlySnapshot();
    await removePrimary();
    await primary.put("kifu-by-id/restore-1.csa", "conflict");
    expect((await api("restore", "POST", { key, apply: "true" })).status).toBe(409);
    expect((await primary.list()).objects).toHaveLength(1);
    await removePrimary();
    await faults({ primaryPut: "kifu-by-id/restore-1.meta.json" });
    expect((await api("restore", "POST", { key, apply: "true" })).status).toBe(502);
    expect((await primary.list()).objects).toHaveLength(2);
    expect((await api("restore", "POST", { key, apply: "true" })).status).toBe(200);
    expect((await primary.list()).objects).toHaveLength(4);
  });

  it("rejects an older snapshot when a force-imported index and D1 row survive", async () => {
    const event = "wdoor+floodgate-300-10F+RAMU+Panda+20261009000001";
    const initial = ["V2", "N+RAMU", "N-Panda", `'Increment:10`, `$EVENT:${event}`,
      "$START_TIME:2026/10/09 00:00:00", "PI", "+", "+2726FU", "T0", "-8384FU", "T10",
      "%TORYO", "'summary:toryo:RAMU lose:Panda win", ""].join("\n");
    const upload = (csa: string, force: boolean) => mf.dispatchFetch(`${ORIGIN}/api/v1/admin/games/upload`, {
      method: "POST", headers: { Authorization: `Bearer ${TOKEN}`, "Content-Type": "application/json" },
      body: JSON.stringify({ csa, source: "wdoor", force }),
    });
    const imported = await upload(initial, false);
    expect(imported.status).toBe(200);
    const { game_id: id } = await imported.json() as { game_id: string };
    await run();
    const key = await onlySnapshot();
    const forced = await upload(`${initial}'$END_TIME:2026/10/10 00:21:55\n`, true);
    expect(forced.status).toBe(200);
    const db = await mf.getD1Database("GAMES_SEARCH_DB");
    const row = await db.prepare("SELECT * FROM games_search_index WHERE game_id = ?").bind(id).first();
    const surviving = (await primary.list({ prefix: "games-index/" })).objects;
    expect(surviving).toHaveLength(1);
    const indexBytes = await (await primary.get(surviving[0]!.key))!.text();
    await primary.delete([`kifu-by-id/${id}.csa`, `kifu-by-id/${id}.meta.json`]);
    for (const apply of ["false", "true"]) {
      expect((await api("restore", "POST", { key, apply })).status).toBe(409);
      expect((await primary.list()).objects).toEqual(surviving);
      expect(await (await primary.get(surviving[0]!.key))!.text()).toBe(indexBytes);
      expect(await db.prepare("SELECT * FROM games_search_index WHERE game_id = ?").bind(id).first()).toEqual(row);
    }
    await primary.delete(surviving[0]!.key);
    for (const apply of ["false", "true"]) {
      expect((await api("restore", "POST", { key, apply })).status).toBe(409);
      expect((await primary.list()).objects).toHaveLength(0);
      expect(await db.prepare("SELECT * FROM games_search_index WHERE game_id = ?").bind(id).first()).toEqual(row);
    }
    await primary.put(surviving[0]!.key, indexBytes);
    const orphan = (await primary.list()).objects;
    await db.prepare("DELETE FROM games_search_index WHERE game_id = ?").bind(id).run();
    for (const apply of ["false", "true"]) {
      expect((await api("restore", "POST", { key, apply })).status).toBe(409);
      expect((await primary.list()).objects).toEqual(orphan);
      expect(await (await primary.get(surviving[0]!.key))!.text()).toBe(indexBytes);
      expect(await db.prepare("SELECT * FROM games_search_index WHERE game_id = ?").bind(id).first()).toBeNull();
    }
  });
  it("fails closed on D1 read errors and retries after storage recovery", async () => {
    await seed("d1-failure");
    await run();
    const key = await onlySnapshot();
    await removePrimary();
    await faults({ d1Read: true });
    for (const apply of ["false", "true"]) {
      expect((await api("restore", "POST", { key, apply })).status).toBe(502);
      expect((await primary.list()).objects).toHaveLength(0);
    }
    await faults({});
    expect((await api("restore", "POST", { key, apply: "true" })).status).toBe(200);
  });

  it("checks D1 fields even when all R2 restore targets match", async () => {
    await seed("d1-fields", "upload");
    await run();
    const key = await onlySnapshot();
    expect((await api("restore", "POST", { key, apply: "true" })).status).toBe(200);
    const db = await mf.getD1Database("GAMES_SEARCH_DB");
    for (const [column, value] of [["event", "new-event"], ["black_player_id", "unexpected-player"], ["result_kind", "new-result"]]) {
      await db.prepare(`UPDATE games_search_index SET ${column} = ? WHERE game_id = ?`).bind(value, "d1-fields").run();
      const current = await db.prepare("SELECT * FROM games_search_index WHERE game_id = ?").bind("d1-fields").first();
      for (const apply of ["false", "true"]) {
        expect((await api("restore", "POST", { key, apply })).status).toBe(409);
        expect(await db.prepare("SELECT * FROM games_search_index WHERE game_id = ?").bind("d1-fields").first()).toEqual(current);
      }
      await db.prepare("DELETE FROM games_search_index WHERE game_id = ?").bind("d1-fields").run();
      expect((await api("restore", "POST", { key, apply: "true" })).status).toBe(200);
    }
  });

  it("rejects an incomplete index scan without restoring into a large bucket", async () => {
    await seed("bounded-restore");
    await run();
    const key = await onlySnapshot();
    await removePrimary();
    for (let batch = 0; batch < 20; batch++) {
      await Promise.all(Array.from({ length: 50 }, (_, offset) => primary.put(
        `games-index/99999999999999-unrelated-${String(batch * 50 + offset).padStart(4, "0")}.json`, "{}")));
    }
    expect((await api("restore", "POST", { key })).status).toBe(200);
    await primary.put("games-index/99999999999999-unrelated-1000.json", "{}");
    for (const apply of ["false", "true"]) {
      const response = await api("restore", "POST", { key, apply });
      expect(response.status).toBe(409);
      expect(await response.text()).toContain("1000 objects");
      expect(await primary.get("kifu-by-id/bounded-restore.csa")).toBeNull();
    }
    const db = await mf.getD1Database("GAMES_SEARCH_DB");
    expect(await db.prepare("SELECT COUNT(*) AS n FROM games_search_index").first()).toMatchObject({ n: 0 });
  });

  it("rejects an expired index preflight before any restore writes", async () => {
    await seed("slow-restore");
    await run();
    const key = await onlySnapshot();
    await removePrimary();
    await faults({ restoreIndexDelayMs: 20_100 });
    const response = await api("restore", "POST", { key, apply: "true" });
    expect(response.status).toBe(409);
    expect(await response.text()).toContain("timed out");
    expect((await primary.list()).objects).toHaveLength(0);
    const db = await mf.getD1Database("GAMES_SEARCH_DB");
    expect(await db.prepare("SELECT COUNT(*) AS n FROM games_search_index").first()).toMatchObject({ n: 0 });
  }, 40_000);
  it("continues a bounded scan across pages and respects an active lease", async () => {
    for (let n = 0; n < 65; n++) await seed(`page-${String(n).padStart(3, "0")}`, "upload");
    const first = await run();
    expect(first.cycle_complete).toBe(false);
    expect(first.processed).toBeLessThanOrEqual(64);
    const stateObject = (await backup.get(STATE_KEY))!;
    const state = await stateObject.json<State>();
    await backup.put(STATE_KEY, JSON.stringify({ ...state, lease_until_ms: Date.now() + 60_000 }));
    expect((await run()).busy).toBe(true);
    await backup.put(STATE_KEY, JSON.stringify({ ...state, lease_until_ms: 1 }));
    let outcome = await run();
    for (let n = 0; n < 5 && !outcome.cycle_complete; n++) outcome = await run();
    expect(outcome.cycle_complete).toBe(true);
    expect(outcome.state.verified).toBe(65);
    expect(await snapshots()).toHaveLength(65);
  }, 60_000);

  it("runs through the scheduled handler and restores a UTC date range through the CLI", async () => {
    await seed("day-a");
    await seed("day-b", "upload", START + 86_400_000);
    expect((await mf.dispatchFetch(`${ORIGIN}/__test/cron`)).status).toBe(200);
    if ((await snapshots()).length < 2) await run();
    expect(await snapshots()).toHaveLength(2);
    await removePrimary();
    const cli = await import(pathToFileURL(resolve(import.meta.dirname, "../../../../scripts/kifu-backup.mjs")).href);
    const output: string[] = [];
    const args = ["restore", "--url", ORIGIN, "--from", DATE, "--through", "2026-10-22"];
    const fetchLocal = (url: URL, init: { method: string; headers: Record<string, string> }) =>
      mf.dispatchFetch(url.toString(), { method: init.method, headers: init.headers });
    await cli.main(args, { ADMIN_API_TOKEN: TOKEN }, (line: string) => output.push(line), fetchLocal);
    expect(output.map(line => JSON.parse(line).applied)).toEqual([false, false]);
    expect((await primary.list()).objects).toHaveLength(0);
    await cli.main([...args, "--apply"], { ADMIN_API_TOKEN: TOKEN }, () => {}, fetchLocal);
    expect((await primary.list()).objects).toHaveLength(7);
    const db = await mf.getD1Database("GAMES_SEARCH_DB");
    expect(await db.prepare("SELECT COUNT(*) AS n FROM games_search_index").first()).toMatchObject({ n: 2 });
  });

  it("stops starting copies at the scheduled budget and resumes next run", async () => {
    for (let n = 0; n < 4; n++) await seed(`slow-${n}`, "upload");
    await faults({ snapshotDelayMs: 1700 });
    expect((await mf.dispatchFetch(`${ORIGIN}/__test/cron`)).status).toBe(200);
    const keys = await snapshots();
    expect(keys.length).toBeGreaterThan(0);
    expect(keys.length).toBeLessThan(4);
    const state = await (await backup.get(STATE_KEY))!.json<State>();
    expect(state.after_key).not.toBeNull();
    expect(state.completed_at_ms).toBe(0);
    await faults({});
    expect((await run()).cycle_complete).toBe(true);
    expect(await snapshots()).toHaveLength(4);
  });

  it("rejects a tampered snapshot before any primary writes", async () => {
    await seed("corrupt-1");
    await run();
    const key = await onlySnapshot();
    const snapshot = await (await backup.get(key))!.json<Record<string, unknown>>();
    snapshot.meta_version = "tampered";
    await backup.put(key, JSON.stringify(snapshot));
    await removePrimary();
    expect((await api("restore", "POST", { key, apply: "true" })).status).toBe(409);
    expect((await primary.list()).objects).toHaveLength(0);
  });
});

it("reports a missing optional backup binding as disabled", async () => {
  const persist = await makeTempPersistRoot();
  const mf = await createMiniflare({ persistRoot: persist.path, adminApiToken: TOKEN });
  try {
    const response = await mf.dispatchFetch(`${ORIGIN}/api/v1/admin/kifu-backup/status`, { headers: { Authorization: `Bearer ${TOKEN}` } });
    expect(await response.json()).toMatchObject({ enabled: false });
  } finally { await mf.dispose(); await persist.cleanup(); }
});
