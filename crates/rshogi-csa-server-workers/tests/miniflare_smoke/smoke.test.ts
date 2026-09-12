import { afterEach, describe, expect, it } from "vitest";
import {
  CsaClient,
  createMiniflare,
  makeTempPersistRoot,
} from "./harness.ts";
import type { Miniflare } from "miniflare";

describe("miniflare smoke: 1 対局 E2E", () => {
  let mf: Miniflare;
  let cleanupPersist: () => Promise<void>;

  afterEach(async () => {
    await mf.dispose();
    await cleanupPersist();
  });

  it("ENTERING_KING_RULE=CSARule24 で 24 点法を広告する (Declaration 行なし)", async () => {
    const persist = await makeTempPersistRoot();
    cleanupPersist = persist.cleanup;
    mf = await createMiniflare({
      persistRoot: persist.path,
      totalTimeSec: 60,
      byoyomiSec: 1,
      enteringKingRule: "CSARule24",
    });

    const roomId = "smoke-room-ekr24";
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
    // 24 点法は拡張行のみ。CSA 標準 Declaration には 24 点法トークンが無いため出さない。
    expect(blackSummary.some((l) => l === "Entering_King_Rule:CSARule24")).toBe(true);
    expect(blackSummary.some((l) => l.startsWith("Declaration:"))).toBe(false);

    await black.close();
    await white.close();
  });
});
