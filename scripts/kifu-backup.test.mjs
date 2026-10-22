import assert from "node:assert/strict";
import { test } from "node:test";
import { main } from "./kifu-backup.mjs";

const env = { ADMIN_API_TOKEN: "isolated-test-token" };
const common = ["--url", "https://example.invalid"];
const range = ["--from", "2026-10-21"];
const key = (timestamp, digest = "a") => `snapshots/v1/2026/10/21/game-1/${String(timestamp).padStart(16, "0")}-${digest.repeat(64)}.json`;
const ok = body => Response.json(body);

test("管理 token を非 HTTPS や資格情報付き URL へ送らない", async () => {
  let calls = 0;
  const fetch = async () => { calls++; return ok({}); };
  for (const url of ["http://example.invalid", "https://user:password@example.invalid", "https://example.invalid/path"]) {
    await assert.rejects(main(["status", "--url", url], env, () => {}, fetch));
  }
  assert.equal(calls, 0);
});

test("明示された空 selector は dry run と apply の通信前に拒否する", async () => {
  let calls = 0;
  const fetch = async () => { calls++; return ok({ keys: [], truncated: false }); };
  for (const flag of ["game-id", "snapshot"]) {
    for (const selector of [[`--${flag}=`], [`--${flag}`, ""]]) {
      for (const apply of [[], ["--apply"]]) {
        await assert.rejects(main(["restore", ...common, ...range, ...selector, ...apply], env, () => {}, fetch), /invalid|empty/);
      }
    }
  }
  assert.equal(calls, 0);
});

test("複数版には明示選択が必要で、latest も同時刻の版を拒否する", async () => {
  let writes = 0;
  let keys = [key(1), key(2)];
  const fetch = async url => {
    if (url.pathname.endsWith("snapshots")) return ok({ keys, truncated: false });
    writes++;
    return ok({});
  };
  await assert.rejects(main(["restore", ...common, ...range], env, () => {}, fetch), /multiple snapshots/);
  keys = [key(1, "a"), key(1, "b")];
  await assert.rejects(main(["restore", ...common, ...range, "--latest"], env, () => {}, fetch), /ambiguous/);
  assert.equal(writes, 0);
});

test("latest の既定は dry run で最新 meta 版を選ぶ", async () => {
  const requested = [];
  const fetch = async (url, init) => {
    assert.equal(init.headers.Authorization, `Bearer ${env.ADMIN_API_TOKEN}`);
    assert.equal(init.redirect, "error");
    if (url.pathname.endsWith("snapshots")) return ok({ keys: [key(1), key(2)], truncated: false });
    requested.push(url);
    return ok({ applied: false });
  };
  await main(["restore", ...common, ...range, "--latest"], env, () => {}, fetch);
  assert.equal(requested.length, 1);
  assert.equal(requested[0].searchParams.get("key"), key(2));
  assert.equal(requested[0].searchParams.get("apply"), "false");
});

test("run の回数上限と busy 応答で継続を止める", async () => {
  let calls = 0;
  const fetch = async () => { calls++; return ok({ enabled: true, cycle_complete: false, state: {} }); };
  await assert.rejects(main(["run", ...common, "--until-complete", "--max-runs", "2"], env, () => {}, fetch), /incomplete/);
  assert.equal(calls, 2);
  await assert.rejects(main(["run", ...common], env, () => {}, async () => ok({ enabled: true, busy: true })), /busy/);
});

test("進まないページと不正日付を拒否する", async () => {
  const fetch = async () => ok({ keys: [key(1)], truncated: true });
  await assert.rejects(main(["list", ...common, ...range], env, () => {}, fetch), /did not advance/);
  let calls = 0;
  await assert.rejects(main(["list", ...common, "--from", "2026-02-30"], env, () => {}, async () => { calls++; return ok({}); }), /invalid date/);
  assert.equal(calls, 0);
});
