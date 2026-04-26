import { afterEach, beforeEach, describe, expect, it } from "vitest";
import {
  CsaClient,
  createMiniflare,
  getFloodgateHistoryBucket,
  makeTempPersistRoot,
  pollFloodgateHistoryForGameId,
} from "./harness.ts";
import type { Miniflare } from "miniflare";

describe("miniflare smoke: Floodgate 履歴 R2 永続化 E2E", () => {
  let mf: Miniflare;
  let cleanupPersist: () => Promise<void>;

  afterEach(async () => {
    await mf.dispose();
    await cleanupPersist();
  });

  it("ALLOW_FLOODGATE_FEATURES=true で 1 対局終局後に floodgate-history/ オブジェクトが書かれる", async () => {
    const persist = await makeTempPersistRoot();
    cleanupPersist = persist.cleanup;
    mf = await createMiniflare({
      persistRoot: persist.path,
      allowFloodgateFeatures: true,
      totalTimeSec: 60,
      byoyomiSec: 1,
    });

    const roomId = "floodgate-history-room-1";
    const gameName = "fg-60-1";
    const blackHandle = "alice";
    const whiteHandle = "bob";
    const blackName = `${blackHandle}+${gameName}+black`;
    const whiteName = `${whiteHandle}+${gameName}+white`;

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
    const startBlack = await black.recvLine();
    await white.recvLine();
    const gameId = startBlack.slice("START:".length);
    expect(gameId.length).toBeGreaterThan(0);

    black.send("+7776FU");
    await black.recvUntil((l) => l.startsWith("+7776FU"));
    await white.recvUntil((l) => l.startsWith("+7776FU"));

    white.send("-3334FU");
    await black.recvUntil((l) => l.startsWith("-3334FU"));
    await white.recvUntil((l) => l.startsWith("-3334FU"));

    black.send("%TORYO");
    await black.recvUntil((l) => l === "#LOSE");

    const r2 = await getFloodgateHistoryBucket(mf);
    const matched = await pollFloodgateHistoryForGameId(r2, gameId);
    expect(matched.length).toBe(1);
    const key = matched[0]!.key;
    // キー命名規則 `floodgate-history/{YYYY}/{MM}/{DD}/{HHMMSS}-{game_id}.json` の
    // 全要素が揃っていることを 1 段で固定する。
    expect(key).toMatch(
      /^floodgate-history\/\d{4}\/\d{2}\/\d{2}\/\d{6}-[\w-]+\.json$/,
    );
    expect(key.endsWith(`-${gameId}.json`)).toBe(true);

    const obj = await r2.get(key);
    expect(obj).not.toBeNull();
    const body = await obj!.text();
    const entry = JSON.parse(body) as {
      game_id: string;
      game_name: string;
      black: string;
      white: string;
      start_time: string;
      end_time: string;
      result_code: string;
      winner?: "Black" | "White";
    };
    expect(entry.game_id).toBe(gameId);
    expect(entry.game_name).toBe(gameName);
    // entry の black / white は CSA LOGIN ハンドルの末尾 `+game_name+color` を
    // 落とした handle 部分（TCP `JsonlFloodgateHistoryStorage::append` と同じ
    // 契約）。
    expect(entry.black).toBe(blackHandle);
    expect(entry.white).toBe(whiteHandle);
    expect(entry.result_code).toBe("#RESIGN");
    expect(entry.winner).toBe("White");
    // RFC3339 (UTC, `Z` サフィックス、秒精度) を `format_rfc3339_utc` 経由で出すため
    // タイムゾーン ofset 表記は混入しない契約。
    expect(entry.start_time).toMatch(/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$/);
    expect(entry.end_time).toMatch(/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$/);

    await black.close();
    await white.close();
  });

  it("ALLOW_FLOODGATE_FEATURES=false では floodgate-history/ オブジェクトが書かれない", async () => {
    const persist = await makeTempPersistRoot();
    cleanupPersist = persist.cleanup;
    // opt-in しない構成では履歴永続化を skip する契約を回帰防止する。
    mf = await createMiniflare({
      persistRoot: persist.path,
      allowFloodgateFeatures: false,
      totalTimeSec: 60,
      byoyomiSec: 1,
    });

    const roomId = "floodgate-history-room-2";
    const gameName = "fg-60-1";
    const blackName = `alice+${gameName}+black`;
    const whiteName = `bob+${gameName}+white`;

    const black = await CsaClient.connect(mf, roomId);
    black.send(`LOGIN ${blackName} pw`);
    await black.recvLine();
    const white = await CsaClient.connect(mf, roomId);
    white.send(`LOGIN ${whiteName} pw`);
    await white.recvLine();
    await black.drainGameSummary();
    await white.drainGameSummary();
    black.send("AGREE");
    white.send("AGREE");
    await black.recvLine();
    await white.recvLine();

    black.send("%TORYO");
    await black.recvUntil((l) => l === "#LOSE");

    const r2 = await getFloodgateHistoryBucket(mf);
    // 終局直後に DO put が走り得るので、書かれないことを観測するには append 経路が
    // 走る期間を一定時間 wait する必要がある。`pollFloodgateHistoryForGameId` の
    // 既定 5000ms より短い 1500ms で確定させ、念のため list 結果が空であることを assert。
    await new Promise((r) => setTimeout(r, 1500));
    const list = await r2.list({ prefix: "floodgate-history/" });
    expect(list.objects.length).toBe(0);

    await black.close();
    await white.close();
  });
});
