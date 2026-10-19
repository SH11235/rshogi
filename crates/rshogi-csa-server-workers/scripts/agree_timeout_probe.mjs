// Live probe for the AGREE wait TTL of a deployed Worker: logs in black and white
// to the same room and never sends AGREE, so the server must abort the match with
// `##[ERROR] agree_timeout` and close both sockets after AGREE_TIMEOUT_SECONDS.
// csa_client cannot do this because it sends AGREE automatically.
//
// usage (from this crate after `pnpm install`):
//   CSA_TEST_PASSWORD=... node scripts/agree_timeout_probe.mjs wss://<host>/ws <room_id> [preset] [wait_sec]
import WebSocket from "ws";

const [baseUrl, room, preset = "byoyomi-msec-10-100", waitArg = "90"] = process.argv.slice(2);
if (!baseUrl || !room) {
  console.error("usage: node scripts/agree_timeout_probe.mjs wss://<host>/ws <room_id> [preset] [wait_sec]");
  process.exit(2);
}
const waitSec = Number(waitArg);
if (!Number.isFinite(waitSec) || waitSec <= 0) {
  console.error(`wait_sec must be a positive number: ${waitArg}`);
  process.exit(2);
}
const password = process.env.CSA_TEST_PASSWORD ?? "testpw";

const closed = new Map();
const sawAgreeTimeout = new Set();
const sockets = [];

function startSession(handle, color) {
  const ws = new WebSocket(`${baseUrl}/${room}`);
  ws.on("open", () => ws.send(`LOGIN ${handle}+${preset}+${color} ${password}\n`));
  ws.on("message", (data) => {
    const text = data.toString();
    process.stdout.write(`[${color}] ${text}`);
    if (text.includes("##[ERROR] agree_timeout")) {
      sawAgreeTimeout.add(color);
    }
  });
  ws.on("error", (err) => console.error(`[${color}] error: ${err.message}`));
  ws.on("close", (code, reason) => {
    console.log(`[${color}] closed code=${code} reason=${reason}`);
    closed.set(color, code);
    if (closed.size === 2) {
      const ok = sawAgreeTimeout.size === 2;
      if (!ok) {
        console.error("both sockets closed without `##[ERROR] agree_timeout` on each side");
      }
      process.exit(ok ? 0 : 1);
    }
  });
  sockets.push(ws);
}

startSession("alice", "black");
startSession("bob", "white");

setTimeout(() => {
  console.error(`no server close within ${waitSec}s; AGREE timeout did not fire`);
  for (const ws of sockets) {
    ws.close();
  }
  process.exit(1);
}, waitSec * 1000);
