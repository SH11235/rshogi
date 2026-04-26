import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { CsaClient, createMiniflare, makeTempPersistRoot } from "./harness.ts";
import type { Miniflare } from "miniflare";

describe("miniflare smoke: 再接続プロトコル E2E", () => {
  let mf: Miniflare;
  let cleanupPersist: () => Promise<void>;

  beforeEach(async () => {
    const persist = await makeTempPersistRoot();
    cleanupPersist = persist.cleanup;
    mf = await createMiniflare({
      persistRoot: persist.path,
      reconnectGraceSeconds: 30,
      allowFloodgateFeatures: true,
      totalTimeSec: 60,
      byoyomiSec: 1,
    });
  });

  afterEach(async () => {
    await mf.dispose();
    await cleanupPersist();
  });

  it("黒切断 → 同 token で再接続 → 状態再送 → 対局継続して終局", async () => {
    const roomId = "reconnect-room-1";
    const gameName = "fg-60-1";
    const blackName = `alice+${gameName}+black`;
    const whiteName = `bob+${gameName}+white`;

    const black0 = await CsaClient.connect(mf, roomId);
    black0.send(`LOGIN ${blackName} pw`);
    expect(await black0.recvLine()).toBe(`LOGIN:${blackName} OK`);

    const white = await CsaClient.connect(mf, roomId);
    white.send(`LOGIN ${whiteName} pw`);
    expect(await white.recvLine()).toBe(`LOGIN:${whiteName} OK`);

    const blackSummary = await black0.drainGameSummary();
    const whiteSummary = await white.drainGameSummary();
    const blackToken = extractReconnectToken(blackSummary);
    const whiteToken = extractReconnectToken(whiteSummary);
    expect(blackToken, "Reconnect_Token (black) は Game_Summary に存在する").toBeDefined();
    expect(whiteToken, "Reconnect_Token (white) は Game_Summary に存在する").toBeDefined();
    expect(blackToken).not.toBe(whiteToken);

    black0.send("AGREE");
    white.send("AGREE");
    const startBlack = await black0.recvLine();
    await white.recvLine();
    const gameId = startBlack.slice("START:".length);
    expect(gameId.length).toBeGreaterThan(0);

    black0.send("+7776FU");
    await black0.recvUntil((l) => l.startsWith("+7776FU"));
    await white.recvUntil((l) => l.startsWith("+7776FU"));

    black0.close();
    await waitFor(() => black0.isClosed(), 2000);

    const black1 = await CsaClient.connect(mf, roomId);
    black1.send(`LOGIN ${blackName} pw reconnect:${gameId}+${blackToken}`);
    expect(await black1.recvLine()).toBe(`LOGIN:${blackName} OK`);

    const resumeSummary = await black1.drainGameSummary();
    const resumedToken = extractReconnectToken(resumeSummary);
    expect(resumedToken, "再送 Game_Summary にも Reconnect_Token を含む").toBeDefined();

    const reconnectStateOpen = await black1.recvLine();
    expect(reconnectStateOpen).toBe("BEGIN Reconnect_State");
    const reconnectStateLines = await black1.recvUntil((l) => l === "END Reconnect_State");
    const turnLine = reconnectStateLines.find((l) => l.startsWith("Current_Turn:"));
    expect(turnLine).toBe("Current_Turn:-");
    expect(reconnectStateLines.some((l) => l.startsWith("Black_Time_Remaining_Ms:"))).toBe(true);
    expect(reconnectStateLines.some((l) => l.startsWith("White_Time_Remaining_Ms:"))).toBe(true);
    // `Last_Move` 行は CoreRoom が最終手 token を露出していない現状では省略される。
    // 将来 last_move 経路が実装されたとき本 assertion を更新する signal になる。
    expect(reconnectStateLines.some((l) => l.startsWith("Last_Move:"))).toBe(false);

    white.send("-3334FU");
    await white.recvUntil((l) => l.startsWith("-3334FU"));
    await black1.recvUntil((l) => l.startsWith("-3334FU"));

    black1.send("%TORYO");
    const blackEnd = await black1.recvUntil((l) => l === "#LOSE");
    expect(blackEnd.some((l) => l === "#RESIGN")).toBe(true);

    black1.close();
    white.close();
  });

  it("不正 token での再接続は LOGIN:incorrect reconnect_rejected で拒否される", async () => {
    const roomId = "reconnect-room-2";
    const gameName = "fg-60-1";
    const blackName = `alice+${gameName}+black`;
    const whiteName = `bob+${gameName}+white`;

    const black0 = await CsaClient.connect(mf, roomId);
    black0.send(`LOGIN ${blackName} pw`);
    await black0.recvLine();
    const white = await CsaClient.connect(mf, roomId);
    white.send(`LOGIN ${whiteName} pw`);
    await white.recvLine();
    await black0.drainGameSummary();
    await white.drainGameSummary();
    black0.send("AGREE");
    white.send("AGREE");
    const startBlack = await black0.recvLine();
    await white.recvLine();
    const gameId = startBlack.slice("START:".length);

    black0.close();
    await waitFor(() => black0.isClosed(), 2000);

    const black1 = await CsaClient.connect(mf, roomId);
    black1.send(`LOGIN ${blackName} pw reconnect:${gameId}+wrong-token-0123abcd`);
    expect(await black1.recvLine()).toBe("LOGIN:incorrect reconnect_rejected");

    white.close();
  });
});

function extractReconnectToken(summaryLines: string[]): string | undefined {
  const line = summaryLines.find((l) => l.startsWith("Reconnect_Token:"));
  return line?.slice("Reconnect_Token:".length);
}

async function waitFor(predicate: () => boolean, timeoutMs: number): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (predicate()) return;
    await new Promise((r) => setTimeout(r, 20));
  }
}
