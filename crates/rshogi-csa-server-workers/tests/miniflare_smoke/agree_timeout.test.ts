import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { CsaClient, createMiniflare, makeTempPersistRoot } from "./harness.ts";
import type { Miniflare } from "miniflare";

/// Issue #600: LOGIN OK 後に AGREE 完了前に片方が刺さると、対局が live API
/// にも出ず DO 上に無限残存する edge case の回帰テスト。
///
/// `start_match` で Game_Summary 送出直後に AGREE 待ち TTL (`AGREE_TIMEOUT_SECONDS`)
/// で `set_alarm` が予約され、両者 AGREE 受領 (`HandleOutcome::GameStarted`) で
/// `clear_agree_timeout_tag` が `KEY_PENDING_ALARM_KIND` を消すことで cancel
/// される。発火時は `handle_agree_timeout_alarm` が `##[ERROR] agree_timeout`
/// を両 player に送って WS を close し、`KEY_FINISHED` をセットして以後の
/// LOGIN を弾く。
describe("miniflare smoke: AGREE 待ち TTL (Issue #600)", () => {
  let mf: Miniflare;
  let cleanupPersist: () => Promise<void>;

  beforeEach(async () => {
    const persist = await makeTempPersistRoot();
    cleanupPersist = persist.cleanup;
    mf = await createMiniflare({
      persistRoot: persist.path,
      // 1 秒で発火させて test を高速化する。production では 60 秒既定。
      agreeTimeoutSeconds: 1,
      // 時計切れ alarm が先に張られないよう、十分長い手番制限を確保する
      // (AGREE 待ち TTL が turn alarm より先に処理されることを pin)。
      totalTimeSec: 600,
      byoyomiSec: 10,
    });
  });

  afterEach(async () => {
    await mf.dispose();
    await cleanupPersist();
  });

  it("両 LOGIN OK 後 AGREE せず放置すると agree_timeout で部屋が解放される", async () => {
    const roomId = "agree-timeout-room-1";
    const gameName = "byoyomi-600-10";
    const blackName = `alice+${gameName}+black`;
    const whiteName = `bob+${gameName}+white`;

    const black = await CsaClient.connect(mf, roomId);
    black.send(`LOGIN ${blackName} pw`);
    expect(await black.recvLine()).toBe(`LOGIN:${blackName} OK`);

    const white = await CsaClient.connect(mf, roomId);
    white.send(`LOGIN ${whiteName} pw`);
    expect(await white.recvLine()).toBe(`LOGIN:${whiteName} OK`);

    // 双方が Game_Summary を受信するところまで進める。AGREE は意図的に送らない。
    await black.drainGameSummary();
    await white.drainGameSummary();

    // alarm 発火 (AGREE_TIMEOUT_SECONDS=1 + ALARM_SAFETY_MS=200ms 程度) を
    // 待って `##[ERROR] agree_timeout` を観測する。timeout は wall-clock 依存
    // のため余裕を持たせる。
    const blackErr = await black.recvLine(10_000);
    expect(blackErr).toBe("##[ERROR] agree_timeout");
    const whiteErr = await white.recvLine(10_000);
    expect(whiteErr).toBe("##[ERROR] agree_timeout");

    // server-initiated close を待つ (close ack)。
    await black.close();
    await white.close();
  });

  it("両 AGREE が間に合えば agree_timeout は cancel され通常対局を続行できる", async () => {
    const roomId = "agree-timeout-room-2";
    const gameName = "byoyomi-600-10";
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

    // 1 秒の TTL より十分早く AGREE を返す。
    black.send("AGREE");
    white.send("AGREE");
    // START を双方が受信できれば、agree alarm は cancel されている。
    await black.recvUntil((l) => l.startsWith("START:"), 5_000);
    await white.recvUntil((l) => l.startsWith("START:"), 5_000);

    // 後続で TTL 経過時間 (1 秒 + 余裕) を待っても `##[ERROR] agree_timeout`
    // が届かないことを pin する。turn alarm は 600 秒先なので発火しない。
    await new Promise((r) => setTimeout(r, 1_500));

    // 着手を 1 手通せば対局が正常進行している証拠になる。
    black.send("+7776FU");
    await black.recvUntil((l) => l.startsWith("+7776FU"), 5_000);
    await white.recvUntil((l) => l.startsWith("+7776FU"), 5_000);

    await black.close();
    await white.close();
  });
});
