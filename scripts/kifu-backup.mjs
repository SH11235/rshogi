#!/usr/bin/env node
import { parseArgs } from "node:util";
import { pathToFileURL } from "node:url";

const HELP = `Usage: node scripts/kifu-backup.mjs <status|run|list|restore> --url <worker-origin> [options]
Authentication: ADMIN_API_TOKEN environment variable (never a command-line argument).
  run:     --until-complete --max-runs <positive integer, default 100>
  list:    --from YYYY-MM-DD [--through YYYY-MM-DD] [--game-id ID]
  restore: --snapshot KEY | --from YYYY-MM-DD [--through YYYY-MM-DD] [--game-id ID]
           --latest selects the newest source-meta version when there are multiple snapshots.
           --apply writes missing objects; the default is a dry run. Existing different data is rejected.
Dates are UTC game-start dates. --latest selects within each day and rejects timestamp ties.
Explicit empty --game-id and --snapshot values are rejected before any request.
`;

function day(value) {
  if (!/^\d{4}-\d{2}-\d{2}$/.test(value ?? "")) throw new Error("date must be YYYY-MM-DD");
  const parsed = new Date(`${value}T00:00:00Z`);
  if (!Number.isFinite(parsed.getTime()) || parsed.toISOString().slice(0, 10) !== value) {
    throw new Error("invalid date");
  }
  return parsed.getTime();
}

function snapshotParts(key) {
  const match = /^snapshots\/v1\/\d{4}\/\d{2}\/\d{2}\/([A-Za-z0-9_-]+)\/(\d{16})-([0-9a-f]{64})\.json$/.exec(key);
  if (!match) throw new Error("invalid snapshot key returned by server");
  return { game: match[1], timestamp: match[2] };
}

export async function main(args, env = process.env, output = console.log, fetchImpl = globalThis.fetch) {
  const { values, positionals } = parseArgs({ args, allowPositionals: true, options: {
    help: { type: "boolean" }, url: { type: "string" }, from: { type: "string" },
    through: { type: "string" }, "game-id": { type: "string" }, snapshot: { type: "string" },
    apply: { type: "boolean" }, latest: { type: "boolean" },
    "until-complete": { type: "boolean" }, "max-runs": { type: "string", default: "100" },
  } });
  if (values.help) { output(HELP); return; }
  const [command] = positionals;
  if (positionals.length !== 1 || !["status", "run", "list", "restore"].includes(command)) {
    throw new Error(HELP);
  }
  const base = new URL(values.url ?? "");
  if (base.username || base.password || base.search || base.hash || base.pathname !== "/") {
    throw new Error("--url must be an origin without credentials, path, query, or fragment");
  }
  if (base.protocol !== "https:" && !(base.protocol === "http:" && ["localhost", "127.0.0.1", "[::1]"].includes(base.hostname))) {
    throw new Error("HTTPS is required except for loopback tests");
  }
  const token = env.ADMIN_API_TOKEN;
  if (!token) throw new Error("ADMIN_API_TOKEN is required");
  if (values["game-id"] !== undefined && !/^[A-Za-z0-9_-]{1,256}$/.test(values["game-id"])) throw new Error("invalid --game-id");
  if (values.snapshot !== undefined) snapshotParts(values.snapshot);
  if (values.apply && command !== "restore") throw new Error("--apply is only valid for restore");

  async function request(action, query = {}, method = "GET") {
    const url = new URL(`/api/v1/admin/kifu-backup/${action}`, base);
    for (const [key, value] of Object.entries(query)) url.searchParams.set(key, value);
    const response = await fetchImpl(url, {
      method, headers: { Authorization: `Bearer ${token}` }, redirect: "error",
      signal: AbortSignal.timeout(30_000),
    });
    if (!response.ok) throw new Error(`${action}: HTTP ${response.status}: ${await response.text()}`);
    return response.json();
  }
  const print = (value) => output(JSON.stringify(value));
  if (command === "status") { print(await request("status")); return; }
  if (command === "run") {
    const max = Number(values["max-runs"]);
    if (!Number.isSafeInteger(max) || max < 1 || max > 10000) throw new Error("--max-runs must be 1..10000");
    for (let attempt = 0; attempt < (values["until-complete"] ? max : 1); attempt++) {
      const result = await request("run", {}, "POST");
      print(result);
      if (!result.enabled || result.busy || result.state?.last_error) throw new Error("backup disabled, busy, or failed; inspect status");
      if (result.cycle_complete || !values["until-complete"]) return;
    }
    throw new Error("scan is incomplete; rerun to continue from its checkpoint");
  }
  if (values.snapshot !== undefined) {
    if (command !== "restore" || values.from !== undefined || values.through !== undefined || values["game-id"] !== undefined || values.latest) {
      throw new Error("--snapshot is exclusive with range/version selection");
    }
    print(await request("restore", { key: values.snapshot, apply: String(values.apply ?? false) }, "POST"));
    return;
  }
  const first = day(values.from);
  const last = day(values.through ?? values.from);
  if (last < first || (last - first) / 86_400_000 > 3660) throw new Error("date range must be ordered and no longer than 3661 days");

  async function* keys(date) {
    let after;
    while (true) {
      const result = await request("snapshots", { date, ...(after ? { after } : {}) });
      if (!Array.isArray(result.keys) || (result.truncated && result.keys.length === 0)) throw new Error("invalid snapshot pagination");
      for (const key of result.keys) {
        if (after && key <= after) throw new Error("snapshot cursor did not advance");
        snapshotParts(key);
        after = key;
        yield key;
      }
      if (!result.truncated) return;
    }
  }
  async function restore(key) {
    print(await request("restore", { key, apply: String(values.apply ?? false) }, "POST"));
  }
  for (let dateMs = first; dateMs <= last; dateMs += 86_400_000) {
    const date = new Date(dateMs).toISOString().slice(0, 10);
    let selected;
    let previous;
    for await (const key of keys(date)) {
      const parts = snapshotParts(key);
      if (values["game-id"] !== undefined && parts.game !== values["game-id"]) continue;
      if (command === "list") { print({ key }); continue; }
      if (previous && previous.game !== parts.game) { await restore(selected); selected = undefined; }
      if (selected) {
        if (!values.latest) throw new Error(`multiple snapshots for ${parts.game}; select --snapshot or --latest`);
        if (previous.timestamp === parts.timestamp) throw new Error(`snapshot timestamp is ambiguous for ${parts.game}; select --snapshot`);
      }
      selected = key;
      previous = parts;
    }
    if (selected) await restore(selected);
  }
}

if (process.argv[1] && pathToFileURL(process.argv[1]).href === import.meta.url) {
  main(process.argv.slice(2)).catch((error) => { console.error(error.message); process.exitCode = 1; });
}
