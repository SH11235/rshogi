import { afterEach, beforeEach, describe, expect, it } from "vitest";
import {
  CsaClient,
  createMiniflare,
  getKifuBucket,
  makeTempPersistRoot,
  pollR2ForGameId,
} from "./harness.ts";
import type { Miniflare } from "miniflare";

describe("miniflare smoke: 1 対局 E2E", () => {
  let mf: Miniflare;
  let cleanupPersist: () => Promise<void>;

  beforeEach(async () => {
    const persist = await makeTempPersistRoot();
    cleanupPersist = persist.cleanup;
    mf = await createMiniflare({
      persistRoot: persist.path,
      totalTimeSec: 60,
      byoyomiSec: 1,
    });
  });

  afterEach(async () => {
    await mf.dispose();
    await cleanupPersist();
  });

  it("LOGIN → AGREE → 数手 → TORYO → R2 棋譜オブジェクトが書かれる", async () => {
    const roomId = "smoke-room-1";
    const gameName = "floodgate-60-1";

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
    expect(blackSummary.some((l) => l === "Your_Turn:+")).toBe(true);
    expect(whiteSummary.some((l) => l === "Your_Turn:-")).toBe(true);

    black.send("AGREE");
    white.send("AGREE");
    const startBlack = await black.recvLine();
    const startWhite = await white.recvLine();
    expect(startBlack.startsWith("START:")).toBe(true);
    expect(startBlack).toBe(startWhite);
    const gameId = startBlack.slice("START:".length);
    expect(gameId.length).toBeGreaterThan(0);

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
    const key = list[0]!.key;
    const obj = await r2.get(key);
    expect(obj).not.toBeNull();
    const body = await obj!.text();
    expect(body).toContain("V2.2");
    expect(body).toContain(gameId);

    await black.close();
    await white.close();
  });
});
