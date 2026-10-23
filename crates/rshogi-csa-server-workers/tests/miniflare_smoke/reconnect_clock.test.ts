import { afterEach, beforeEach, describe, expect, it } from "vitest";
import {
  CsaClient,
  createMiniflare,
  getKifuBucket,
  makeTempPersistRoot,
  pollR2ForGameId,
} from "./harness.ts";
import type { Miniflare } from "miniflare";

describe("miniflare smoke: 秒読み中の再接続時計補償", () => {
  let mf: Miniflare;
  let persistRoot: string;
  let cleanupPersist: () => Promise<void>;

  beforeEach(async () => {
    const persist = await makeTempPersistRoot();
    persistRoot = persist.path;
    cleanupPersist = persist.cleanup;
    mf = await spawnMiniflare();
  });

  afterEach(async () => {
    await mf.dispose();
    await cleanupPersist();
  });

  it("再接続後は秒読み 1 回分まで戻り、元 deadline 後の着手も継続する", async () => {
    const roomId = "reconnect-clock-room-1";
    const gameName = "fg-1-1";
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
    const blackToken = blackSummary
      .find((line) => line.startsWith("Reconnect_Token:"))
      ?.slice("Reconnect_Token:".length);
    expect(blackToken).toBeDefined();
    const whiteToken = whiteSummary
      .find((line) => line.startsWith("Reconnect_Token:"))
      ?.slice("Reconnect_Token:".length);
    expect(whiteToken).toBeDefined();

    black0.send("AGREE");
    white.send("AGREE");
    const start = await black0.recvLine();
    await white.recvLine();
    const gameId = start.slice("START:".length);

    // 先手本体 1 秒を使い切り、後手着手後の先手を純粋な 1 秒秒読みにする。
    await sleep(1_100);
    black0.send("+7776FU");
    await black0.recvUntil((line) => line.startsWith("+7776FU"));
    await white.recvUntil((line) => line.startsWith("+7776FU"));
    white.send("-3334FU");
    await white.recvUntil((line) => line.startsWith("-3334FU"));
    await black0.recvUntil((line) => line.startsWith("-3334FU"));

    // 秒読み開始から 1.3 秒で切断する。秒粒度時計は 1,999ms まで受理するため、
    // この時点ではまだ alarm 前だが残りは 1 秒未満。再接続で不足分を補償する。
    await sleep(1_300);
    // Miniflare は client close ack を返さず既定 2 秒 timeout まで待つ場合がある。
    // grace 登録には十分な 100ms だけ待ち、元 turn alarm より前に再接続する。
    await black0.close(100);
    const black1 = await CsaClient.connect(mf, roomId);
    black1.send(`LOGIN ${blackName} pw reconnect:${gameId}+${blackToken}`);
    expect(await black1.recvLine()).toBe(`LOGIN:${blackName} OK`);
    await black1.drainGameSummary();
    expect(await black1.recvLine()).toBe("BEGIN Reconnect_State");
    const snapshot = await black1.recvUntil((line) => line === "END Reconnect_State");
    const remaining = Number(snapshot.find((line) => line.startsWith("Current_Turn_Remaining_Ms:"))?.split(":")[1]);
    expect(remaining).toBeGreaterThan(0);
    expect(remaining).toBeLessThanOrEqual(1_000);

    // さらに 0.9 秒後は raw 経過が 2 秒を超え、旧実装なら着手時に %TIME_UP。
    // 補償後は実効経過が 2 秒未満なので着手を受理し、両側へ同じ T 行を送る。
    await sleep(900);
    black1.send("+2726FU");
    const blackMove = await black1.recvUntil((line) => line.startsWith("+2726FU"));
    const whiteMove = await white.recvUntil((line) => line.startsWith("+2726FU"));
    expect(blackMove.at(-1)).toMatch(/^\+2726FU,T1$/);
    expect(whiteMove.at(-1)).toBe(blackMove.at(-1));

    // 補償済みの黒着手後に DO を再起動する。replay は ply ごとの credit を再適用
    // できないと黒手を TimeUp にして失敗するため、両 token の再参加成功が永続化の
    // E2E assertion になる（grace registry が消えた場合は cold rejoin fallback）。
    await mf.dispose();
    mf = await spawnMiniflare();
    const black2 = await reconnect(mf, roomId, blackName, gameId, blackToken!);
    const white2 = await reconnect(mf, roomId, whiteName, gameId, whiteToken!);

    white2.send("%TORYO");
    await white2.recvUntil((line) => line === "#LOSE");
    await black2.recvUntil((line) => line === "#WIN");

    // export も live wire と同じ補償後の T を使う。raw at_ms 差分のままなら
    // ここが T2 になり、クライアント表示と棋譜が食い違う。
    const r2 = await getKifuBucket(mf);
    const list = await pollR2ForGameId(r2, gameId);
    const obj = await r2.get(list[0]!.key);
    expect(await obj!.text()).toContain("+2726FU,T1");

    await black2.close();
    await white2.close();
  });

  function spawnMiniflare(): Promise<Miniflare> {
    return createMiniflare({
      persistRoot,
      reconnectGraceSeconds: 30,
      allowFloodgateFeatures: true,
      totalTimeSec: 1,
      byoyomiSec: 1,
    });
  }
});

async function reconnect(
  mf: Miniflare,
  roomId: string,
  playerName: string,
  gameId: string,
  token: string,
): Promise<CsaClient> {
  const client = await CsaClient.connect(mf, roomId);
  client.send(`LOGIN ${playerName} pw reconnect:${gameId}+${token}`);
  expect(await client.recvLine()).toBe(`LOGIN:${playerName} OK`);
  await client.drainGameSummary();
  expect(await client.recvLine()).toBe("BEGIN Reconnect_State");
  await client.recvUntil((line) => line === "END Reconnect_State");
  return client;
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
