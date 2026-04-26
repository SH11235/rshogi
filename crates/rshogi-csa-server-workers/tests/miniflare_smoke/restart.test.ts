import { afterEach, beforeEach, describe, expect, it } from "vitest";
import {
  CsaClient,
  createMiniflare,
  getKifuBucket,
  makeTempPersistRoot,
  pollR2ForGameId,
} from "./harness.ts";
import type { Miniflare } from "miniflare";

describe("miniflare smoke: DO restart 永続化", () => {
  let persistRoot: string;
  let cleanupPersist: () => Promise<void>;
  const instances: Miniflare[] = [];

  beforeEach(async () => {
    const persist = await makeTempPersistRoot();
    persistRoot = persist.path;
    cleanupPersist = persist.cleanup;
  });

  afterEach(async () => {
    while (instances.length > 0) {
      const mf = instances.pop();
      if (mf) await mf.dispose();
    }
    await cleanupPersist();
  });

  it("1 対局を終局まで進めた後、Miniflare を dispose → 再起動しても R2 棋譜が読める", async () => {
    const roomId = "restart-room-1";
    const gameName = "fg-60-1";

    const first = await spawnMiniflare();
    const { gameId, kifuKey, kifuBody } = await playOneGame(first, roomId, gameName);

    expect(kifuBody).toContain("V2.2");
    expect(kifuBody).toContain(gameId);

    await first.dispose();
    instances.length = 0;

    const second = await spawnMiniflare();
    const r2 = await getKifuBucket(second);
    const obj = await r2.get(kifuKey);
    expect(obj, `R2 object ${kifuKey} should survive Miniflare restart`).not.toBeNull();
    const body = await obj!.text();
    expect(body).toBe(kifuBody);
  });

  // `spawn` という名前は Node.js `child_process.spawn` と紛らわしいので
  // Miniflare instance を生成するヘルパであることを明示する命名に揃える。
  async function spawnMiniflare(): Promise<Miniflare> {
    const mf = await createMiniflare({
      persistRoot: persistRoot,
      totalTimeSec: 60,
      byoyomiSec: 1,
    });
    instances.push(mf);
    return mf;
  }
});

interface GameOutcome {
  gameId: string;
  kifuKey: string;
  kifuBody: string;
}

async function playOneGame(
  mf: Miniflare,
  roomId: string,
  gameName: string,
): Promise<GameOutcome> {
  const black = await CsaClient.connect(mf, roomId);
  const blackName = `alice+${gameName}+black`;
  black.send(`LOGIN ${blackName} pw`);
  await black.recvLine();

  const white = await CsaClient.connect(mf, roomId);
  const whiteName = `bob+${gameName}+white`;
  white.send(`LOGIN ${whiteName} pw`);
  await white.recvLine();

  await black.drainGameSummary();
  await white.drainGameSummary();

  black.send("AGREE");
  white.send("AGREE");
  const startBlack = await black.recvLine();
  await white.recvLine();
  const gameId = startBlack.slice("START:".length);

  black.send("+7776FU");
  await black.recvUntil((l) => l.startsWith("+7776FU"));
  await white.recvUntil((l) => l.startsWith("+7776FU"));

  white.send("-3334FU");
  await black.recvUntil((l) => l.startsWith("-3334FU"));
  await white.recvUntil((l) => l.startsWith("-3334FU"));

  black.send("%TORYO");
  await black.recvUntil((l) => l === "#LOSE");

  const r2 = await getKifuBucket(mf);
  const list = await pollR2ForGameId(r2, gameId);
  const key = list[0]!.key;
  const body = await (await r2.get(key))!.text();

  await black.close();
  await white.close();

  return { gameId, kifuKey: key, kifuBody: body };
}
