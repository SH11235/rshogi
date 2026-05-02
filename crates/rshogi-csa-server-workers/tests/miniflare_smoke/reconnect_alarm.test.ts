import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { CsaClient, createMiniflare, makeTempPersistRoot } from "./harness.ts";
import type { Miniflare } from "miniflare";

describe("miniflare smoke: 再接続 grace と turn alarm の優先順位", () => {
  let mf: Miniflare;
  let cleanupPersist: () => Promise<void>;

  beforeEach(async () => {
    const persist = await makeTempPersistRoot();
    cleanupPersist = persist.cleanup;
    mf = await createMiniflare({
      persistRoot: persist.path,
      reconnectGraceSeconds: 30,
      allowFloodgateFeatures: true,
      totalTimeSec: 1,
      byoyomiSec: 0,
    });
  });

  afterEach(async () => {
    await mf.dispose();
    await cleanupPersist();
  });

  it("off-turn 切断時に既存 turn alarm が早い場合は TimeUp として処理する", async () => {
    const roomId = "reconnect-alarm-room-1";
    const gameName = "fg-1-0";
    const blackName = `alice+${gameName}+black`;
    const whiteName = `bob+${gameName}+white`;

    const black = await CsaClient.connect(mf, roomId);
    black.send(`LOGIN ${blackName} pw`);
    expect(await black.recvLine()).toBe(`LOGIN:${blackName} OK`);

    const white = await CsaClient.connect(mf, roomId);
    white.send(`LOGIN ${whiteName} pw`);
    expect(await white.recvLine()).toBe(`LOGIN:${whiteName} OK`);

    await black.drainGameSummary();
    await white.drainGameSummary();
    black.send("AGREE");
    white.send("AGREE");
    await black.recvLine();
    await white.recvLine();

    black.send("+7776FU");
    await black.recvUntil((l) => l.startsWith("+7776FU"));
    await white.recvUntil((l) => l.startsWith("+7776FU"));

    await black.close();

    const whiteEnd = await white.recvUntil((l) => l === "#LOSE", 10_000);
    expect(whiteEnd.includes("#TIME_UP"), `white stream: ${JSON.stringify(whiteEnd)}`).toBe(true);
    expect(whiteEnd.includes("#ABNORMAL"), `white stream: ${JSON.stringify(whiteEnd)}`).toBe(false);

    await white.close();
  });
});
