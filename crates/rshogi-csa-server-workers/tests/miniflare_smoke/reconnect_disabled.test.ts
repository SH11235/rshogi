import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { CsaClient, createMiniflare, makeTempPersistRoot } from "./harness.ts";
import type { Miniflare } from "miniflare";

/// Issue #591 hotfix の miniflare smoke。production の保守的既定
/// (`RECONNECT_GRACE_SECONDS = 0` + `ALLOW_FLOODGATE_FEATURES = false`) で
/// 以下を pin する:
///
/// - `Game_Summary` 末尾拡張行に `Reconnect_Token:` 行が含まれない (assert 1)
/// - 黒/白いずれが切断しても残存側が `#ABNORMAL` + `#WIN` で終局する (assert 2/3)
/// - 任意 token (32 文字 hex const) で `reconnect:` を投げても
///   `LOGIN:incorrect reconnect_rejected` で拒否される (assert 4)
/// - `RECONNECT_GRACE_SECONDS = 30` + `ALLOW_FLOODGATE_FEATURES = false` の
///   misconfig は `start_match` で `##[ERROR]` を返して match 不成立 (assert 5)
describe("miniflare smoke: 再接続プロトコル無効構成 (Issue #591 hotfix)", () => {
  /// `assert 4` の任意 token fixture。32 文字 hex の const literal で固定し、
  /// 「server 側が token を実際に照合せず unconditional に reject している」
  /// 挙動 (= grace=0 経路では `reconnect_pending` registry が空で必ず弾く) を pin する。
  const ANY_RECONNECT_TOKEN_HEX = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

  describe("grace=0 + allow=false の保守的既定", () => {
    let mf: Miniflare;
    let cleanupPersist: () => Promise<void>;

    beforeEach(async () => {
      const persist = await makeTempPersistRoot();
      cleanupPersist = persist.cleanup;
      mf = await createMiniflare({
        persistRoot: persist.path,
        // production wrangler.production.toml と同じ既定。`Reconnect_Token:` 拡張行を
        // 出さず、disconnect は即時 `#ABNORMAL` に流す保守的既定経路を検証する。
        reconnectGraceSeconds: 0,
        allowFloodgateFeatures: false,
        totalTimeSec: 60,
        byoyomiSec: 1,
      });
    });

    afterEach(async () => {
      await mf.dispose();
      await cleanupPersist();
    });

    it.each(["black", "white"])("%s disconnect ends the game without advertising or accepting reconnect", async (disconnected) => {
      const roomId = `reconnect-disabled-${disconnected}`;
      const blackName = "alice+fg-60-1+black";
      const whiteName = "bob+fg-60-1+white";
      const black = await CsaClient.connect(mf, roomId);
      black.send(`LOGIN ${blackName} pw`);
      expect(await black.recvLine()).toBe(`LOGIN:${blackName} OK`);
      const white = await CsaClient.connect(mf, roomId);
      white.send(`LOGIN ${whiteName} pw`);
      expect(await white.recvLine()).toBe(`LOGIN:${whiteName} OK`);
      for (const summary of [await black.drainGameSummary(), await white.drainGameSummary()]) {
        expect(summary.some((line) => line.startsWith("Reconnect_Token:"))).toBe(false);
      }
      black.send("AGREE"); white.send("AGREE");
      const start = await black.recvLine();
      expect(start).toMatch(/^START:.+/);
      expect(await white.recvLine()).toBe(start);
      const leaving = disconnected === "black" ? black : white;
      const remaining = disconnected === "black" ? white : black;
      await leaving.close();
      const end = await remaining.recvUntil((line) => line === "#WIN");
      expect(end).toContain("#ABNORMAL");
      expect(end.indexOf("#ABNORMAL")).toBeLessThan(end.indexOf("#WIN"));
      const retry = await CsaClient.connect(mf, roomId);
      const name = disconnected === "black" ? blackName : whiteName;
      retry.send(`LOGIN ${name} pw reconnect:${start.slice("START:".length)}+${ANY_RECONNECT_TOKEN_HEX}`);
      expect(["LOGIN:incorrect", "LOGIN:incorrect reconnect_rejected"]).toContain(await retry.recvLine());
      await retry.close(); await remaining.close();
    });

  });

  /// assert 5: misconfig (`grace=30 + allow=false`) は `resolve_reconnect_grace`
  /// の `validate_floodgate_feature_gate` で `Err` を返し、`start_match` が
  /// `abort_pending_match_with_error` 経由で `##[ERROR] reconnect grace config error`
  /// を両 player に送って match を不成立にする。production の保守的既定 (grace=0
  /// + allow=false) ではこの経路に到達しないが、misconfig fail-fast の defensive
  /// measure として pin する。
  describe("grace=30 + allow=false の misconfig fail-fast", () => {
    let mf: Miniflare;
    let cleanupPersist: () => Promise<void>;

    beforeEach(async () => {
      const persist = await makeTempPersistRoot();
      cleanupPersist = persist.cleanup;
      mf = await createMiniflare({
        persistRoot: persist.path,
        reconnectGraceSeconds: 30,
        allowFloodgateFeatures: false,
        totalTimeSec: 60,
        byoyomiSec: 1,
      });
    });

    afterEach(async () => {
      await mf.dispose();
      await cleanupPersist();
    });

    it("assert 5: grace>0 + allow=false は start_match で ##[ERROR] + ws close", async () => {
      const roomId = "reconnect-disabled-misconfig-1";
      const gameName = "fg-60-1";
      const blackName = `alice+${gameName}+black`;
      const whiteName = `bob+${gameName}+white`;

      const black = await CsaClient.connect(mf, roomId);
      black.send(`LOGIN ${blackName} pw`);
      expect(await black.recvLine()).toBe(`LOGIN:${blackName} OK`);
      const white = await CsaClient.connect(mf, roomId);
      white.send(`LOGIN ${whiteName} pw`);
      expect(await white.recvLine()).toBe(`LOGIN:${whiteName} OK`);

      // `start_match` の grace fail-fast 経路で `##[ERROR] reconnect grace config error`
      // が両 player に送られ、ws が close 1011 で閉じられる。`Game_Summary` ではなく
      // `##[ERROR]` 行を観測する点が `assert 1` と異なる。
      const blackErr = await black.recvLine();
      expect(blackErr).toBe("##[ERROR] reconnect grace config error");
      const whiteErr = await white.recvLine();
      expect(whiteErr).toBe("##[ERROR] reconnect grace config error");

      // server side close を待つ。`close()` は内部で readyState チェック / timeout
      // 吸収を行うので、close event が観測できなくても resolve する。
      await black.close();
      await white.close();
      expect(black.isClosed()).toBe(true);
      expect(white.isClosed()).toBe(true);
    });
  });
});
