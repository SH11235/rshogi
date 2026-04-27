import { afterEach, beforeEach, describe, expect, it } from "vitest";
import {
  CsaClient,
  createMiniflare,
  getKifuBucket,
  makeTempPersistRoot,
  pollR2ForGameId,
} from "./harness.ts";
import type { Miniflare } from "miniflare";

// `CLOCK_KIND = "countdown_msec"` で MillisecondsCountdownClock 経路を通電させ、
// Game_Summary が `Time_Unit:1msec`、棋譜が CSA V2 で R2 に書かれることを assert
// する短時間対局 smoke。staging の既定値 (BYOYOMI_MS=100, TOTAL_TIME_MS=10000)
// に揃え、本番互換でない短 byoyomi の挙動を CI で固定する。
describe("miniflare smoke: 1 対局 E2E (countdown_msec)", () => {
  let mf: Miniflare;
  let cleanupPersist: () => Promise<void>;

  beforeEach(async () => {
    const persist = await makeTempPersistRoot();
    cleanupPersist = persist.cleanup;
    mf = await createMiniflare({
      persistRoot: persist.path,
      clockKind: "countdown_msec",
      totalTimeMs: 10_000,
      byoyomiMs: 100,
    });
  });

  afterEach(async () => {
    await mf.dispose();
    await cleanupPersist();
  });

  it("Game_Summary に Time_Unit:1msec が出て、TORYO 後 R2 棋譜が書かれる", async () => {
    const roomId = "smoke-msec-room-1";
    const gameName = "rapid-10-100ms";

    const black = await CsaClient.connect(mf, roomId);
    const blackName = `alice+${gameName}+black`;
    black.send(`LOGIN ${blackName} pw`);
    expect(await black.recvLine()).toBe(`LOGIN:${blackName} OK`);

    const white = await CsaClient.connect(mf, roomId);
    const whiteName = `bob+${gameName}+white`;
    white.send(`LOGIN ${whiteName} pw`);
    expect(await white.recvLine()).toBe(`LOGIN:${whiteName} OK`);

    const blackSummary = await black.drainGameSummary();
    const whiteSummary = await white.drainGameSummary();
    // `countdown_msec` バリアントが選択されていれば Time_Unit:1msec が出る。
    // countdown (sec) 互換のままだと 1sec が出てしまうので、ここで両者を区別できる。
    expect(blackSummary.some((l) => l === "Time_Unit:1msec")).toBe(true);
    expect(whiteSummary.some((l) => l === "Time_Unit:1msec")).toBe(true);
    // ms 値そのままで Total_Time / Byoyomi が出る。
    expect(blackSummary.some((l) => l === "Total_Time:10000")).toBe(true);
    expect(blackSummary.some((l) => l === "Byoyomi:100")).toBe(true);

    black.send("AGREE");
    white.send("AGREE");
    const startBlack = await black.recvLine();
    const startWhite = await white.recvLine();
    expect(startBlack.startsWith("START:")).toBe(true);
    expect(startBlack).toBe(startWhite);
    const gameId = startBlack.slice("START:".length);

    black.send("+7776FU");
    await black.recvUntil((l) => l.startsWith("+7776FU"));
    await white.recvUntil((l) => l.startsWith("+7776FU"));

    white.send("-3334FU");
    await black.recvUntil((l) => l.startsWith("-3334FU"));
    await white.recvUntil((l) => l.startsWith("-3334FU"));

    black.send("%TORYO");
    const blackEnd = await black.recvUntil((l) => l === "#LOSE");
    expect(blackEnd.some((l) => l === "#RESIGN")).toBe(true);

    const r2 = await getKifuBucket(mf);
    const list = await pollR2ForGameId(r2, gameId);
    expect(list.length).toBeGreaterThan(0);
    const obj = await r2.get(list[0]!.key);
    const body = await obj!.text();
    // 棋譜にも Time_Unit:1msec が入る。
    expect(body).toContain("V2.2");
    expect(body).toContain(gameId);
    expect(body).toContain("Time_Unit:1msec");
    expect(body).toContain("Total_Time:10000");
    expect(body).toContain("Byoyomi:100");

    await black.close();
    await white.close();
  });
});
