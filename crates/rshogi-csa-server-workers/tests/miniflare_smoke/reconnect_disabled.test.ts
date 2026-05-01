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

    /// assert 1: 黒/白の両 `Game_Summary` に `Reconnect_Token:` 行が含まれない。
    /// 修正の本丸 — server が grace=0 のとき token 配布をスキップしている。
    it("assert 1: Game_Summary に Reconnect_Token 行が含まれない", async () => {
      const roomId = "reconnect-disabled-room-1";
      const gameName = "fg-60-1";
      const blackName = `alice+${gameName}+black`;
      const whiteName = `bob+${gameName}+white`;

      const black = await CsaClient.connect(mf, roomId);
      black.send(`LOGIN ${blackName} pw`);
      expect(await black.recvLine()).toBe(`LOGIN:${blackName} OK`);
      const white = await CsaClient.connect(mf, roomId);
      white.send(`LOGIN ${whiteName} pw`);
      expect(await white.recvLine()).toBe(`LOGIN:${whiteName} OK`);

      const blackSummary = await black.drainGameSummary();
      const whiteSummary = await white.drainGameSummary();
      expect(
        blackSummary.some((l) => l.startsWith("Reconnect_Token:")),
        "黒 Game_Summary に Reconnect_Token 行があってはならない",
      ).toBe(false);
      expect(
        whiteSummary.some((l) => l.startsWith("Reconnect_Token:")),
        "白 Game_Summary に Reconnect_Token 行があってはならない",
      ).toBe(false);

      await black.close();
      await white.close();
    });

    /// assert 2: 黒切断 → 残存 white の WS で `#ABNORMAL`+`#WIN` を順序確認。
    /// grace=0 のとき `force_abnormal` 経路に直接落ちる挙動を pin する。
    it("assert 2: 黒切断 → 残存 white は #ABNORMAL → #WIN で終局", async () => {
      const roomId = "reconnect-disabled-room-2";
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
      await black.recvLine(); // START:<game_id>
      await white.recvLine();

      await black.close();

      const whiteEnd = await white.recvUntil((l) => l === "#WIN");
      expect(whiteEnd.includes("#ABNORMAL"), `白 stream に #ABNORMAL が含まれるべき: ${JSON.stringify(whiteEnd)}`).toBe(
        true,
      );
      const abnormalIdx = whiteEnd.indexOf("#ABNORMAL");
      const winIdx = whiteEnd.indexOf("#WIN");
      expect(abnormalIdx).toBeLessThan(winIdx);

      await white.close();
    });

    /// assert 3: 白切断 → 残存 black の WS で `#ABNORMAL`+`#WIN` を順序確認。
    it("assert 3: 白切断 → 残存 black は #ABNORMAL → #WIN で終局", async () => {
      const roomId = "reconnect-disabled-room-3";
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

      await white.close();

      const blackEnd = await black.recvUntil((l) => l === "#WIN");
      expect(blackEnd.includes("#ABNORMAL"), `黒 stream に #ABNORMAL が含まれるべき: ${JSON.stringify(blackEnd)}`).toBe(
        true,
      );
      const abnormalIdx = blackEnd.indexOf("#ABNORMAL");
      const winIdx = blackEnd.indexOf("#WIN");
      expect(abnormalIdx).toBeLessThan(winIdx);

      await black.close();
    });

    /// assert 4: 黒切断後、新 WS で 32 文字 hex const token を入れた `reconnect:`
    /// LOGIN を投げても server は LOGIN を受理しない。
    ///
    /// grace=0 では black0 切断 → 即時 `force_abnormal` で対局が終局 → DO の
    /// `KEY_FINISHED` がセットされ、`handle_login` の冒頭で「既に終局済みの DO」
    /// 経路に入るため、reconnect ブランチに到達する前に `LOGIN:incorrect` で
    /// 弾かれる (`game_room.rs::handle_login` の `load_finished().is_some()` ガード)。
    ///
    /// grace>0 構成での「reconnect 経路まで到達するが registry が空 → reconnect_rejected」
    /// は `reconnect.test.ts` の "不正 token" シナリオ側で pin する。
    it("assert 4: 任意 token の reconnect: は LOGIN:incorrect で拒否される (終局後の DO)", async () => {
      const roomId = "reconnect-disabled-room-4";
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

      await black0.close();
      // 残存 white 側で `#WIN` まで読み切って DO の `finalize_if_ended` が
      // 完了したことを確認してから reconnect 試行を行う。これにより
      // `KEY_FINISHED` が確実にセットされ、`handle_login` の終局済みガードに
      // 到達することを保証する。
      await white.recvUntil((l) => l === "#WIN");

      const black1 = await CsaClient.connect(mf, roomId);
      black1.send(`LOGIN ${blackName} pw reconnect:${gameId}+${ANY_RECONNECT_TOKEN_HEX}`);
      const reply = await black1.recvLine();
      // `LOGIN:incorrect` (終局済み DO ガード) もしくは `LOGIN:incorrect reconnect_rejected`
      // (registry 空ガード) のいずれかで弾かれる。グレース 0 構成ではどちらの経路にも
      // 入りうる (timing 依存) が、reconnect が成功する経路は存在しない点が pin される。
      expect(
        ["LOGIN:incorrect", "LOGIN:incorrect reconnect_rejected"].includes(reply),
        `予期しない reply: ${reply}`,
      ).toBe(true);

      await white.close();
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
