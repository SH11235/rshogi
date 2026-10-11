import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { Miniflare } from "miniflare";
import { createMiniflare, getKifuBucket, makeTempPersistRoot } from "./harness.ts";

const ADMIN_TOKEN = "kifu-import-admin-token";
const ORIGIN = "https://example.com";

// 平手から 2 手指して先手番で投了 (後手勝ち)。`$START_TIME` は JST として解釈される。
function uploadCsa(extraHeader = ""): string {
  return [
    "V2.2",
    "N+alice",
    "N-bob",
    "$EVENT:local",
    "$START_TIME:2026/05/03 10:00:00",
    "$TIME_LIMIT:00:15+05",
    ...(extraHeader ? [extraHeader] : []),
    "PI",
    "+",
    "+7776FU,T3",
    "-3334FU,T4",
    "%TORYO",
    "",
  ].join("\n");
}

// wdoor (shogi-server) 形式。手元アーカイブからの本文アップロード (`source: "wdoor"`) 用。
const WDOOR_ID = "wdoor+floodgate-300-10F+RAMU+Panda+20261009000001";
const WDOOR_GAME_ID = "wdoor-20261009000001-93ef4d36a6";
const WDOOR_CSA = [
  "V2",
  "N+RAMU",
  "N-Panda",
  "'Increment:10",
  `$EVENT:${WDOOR_ID}`,
  "$START_TIME:2026/10/09 00:00:00",
  "PI",
  "+",
  "+2726FU",
  "T0",
  "-8384FU",
  "T10",
  "'** 136 +7776FU -4132KI",
  "%TORYO",
  "'summary:toryo:RAMU lose:Panda win",
  "'$END_TIME:2026/10/09 00:21:55",
  "",
].join("\n");

describe("miniflare smoke: external kifu import API", () => {
  let mf: Miniflare;
  let cleanupPersist: () => Promise<void>;

  beforeEach(async () => {
    const persist = await makeTempPersistRoot();
    cleanupPersist = persist.cleanup;
    mf = await createMiniflare({
      persistRoot: persist.path,
      allowViewerApi: true,
      adminApiToken: ADMIN_TOKEN,
      wsAllowedOrigins: ORIGIN,
    });
  });

  afterEach(async () => {
    if (mf) await mf.dispose();
    if (cleanupPersist) await cleanupPersist();
  });

  function post(path: string, body: unknown, token: string | null = ADMIN_TOKEN) {
    return mf.dispatchFetch(`http://localhost${path}`, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        ...(token === null ? {} : { Authorization: `Bearer ${token}` }),
      },
      body: typeof body === "string" ? body : JSON.stringify(body),
    });
  }

  function viewerGet(path: string) {
    return mf.dispatchFetch(`http://localhost${path}`, { headers: { Origin: ORIGIN } });
  }

  it("rejects requests without a valid admin token", async () => {
    for (const path of ["/api/v1/admin/games/upload", "/api/v1/admin/games/import-wdoor"]) {
      for (const token of [null, "wrong-token"]) {
        const res = await post(path, { csa: uploadCsa(), wdoor_game_id: WDOOR_ID }, token);
        expect(res.status, `${path} token=${token}`).toBe(403);
        expect(res.headers.get("Cache-Control")).toBe("no-store");
        await res.arrayBuffer();
      }
    }
    const bucket = await getKifuBucket(mf);
    expect((await bucket.list()).objects).toEqual([]);
  });

  it("import-wdoor validates the game id before touching the network", async () => {
    for (const wdoorGameId of [
      "",
      "floodgate-300-10F+RAMU+Panda+20261009000001",
      "wdoor+floodgate-300-10F+RAMU+Panda+2026100900000",
      "wdoor+floodgate-300-10F+RA/MU+Panda+20261009000001",
      "wdoor+flood gate+RAMU+Panda+20261009000001",
      "wdoor+floodgate-300-10F+RAMU+Panda+20261309000001",
    ]) {
      const res = await post("/api/v1/admin/games/import-wdoor", { wdoor_game_id: wdoorGameId });
      expect(res.status, wdoorGameId).toBe(400);
      await res.arrayBuffer();
    }
    const malformed = await post("/api/v1/admin/games/import-wdoor", "{not json");
    expect(malformed.status).toBe(400);
    await malformed.arrayBuffer();
    const missing = await post("/api/v1/admin/games/import-wdoor", { force: true });
    expect(missing.status).toBe(400);
    await missing.arrayBuffer();
  });

  it("import-wdoor skips the fetch when the game is already stored", async () => {
    // 手元アーカイブ経由で同じ game_id を先に保存しておくと、参照取り込みは wdoor に
    // 取りに行かず created:false を返す (このテストは外部ネットワークに出ない)。
    const uploaded = await post("/api/v1/admin/games/upload", { csa: WDOOR_CSA, source: "wdoor" });
    expect(uploaded.status).toBe(200);
    expect(await uploaded.json()).toEqual({ game_id: WDOOR_GAME_ID, created: true });

    const imported = await post("/api/v1/admin/games/import-wdoor", { wdoor_game_id: WDOOR_ID });
    expect(imported.status).toBe(200);
    expect(imported.headers.get("Cache-Control")).toBe("no-store");
    expect(await imported.json()).toEqual({ game_id: WDOOR_GAME_ID, created: false });

    const game = await viewerGet(`/api/v1/games/${WDOOR_GAME_ID}`);
    expect(game.status).toBe(200);
    const body = (await game.json()) as { csa: string; meta: Record<string, unknown> };
    expect(body.csa).toBe(WDOOR_CSA);
    // 2026/10/09 00:00:00 JST = 2026-10-08T15:00:00Z
    expect(body.meta).toEqual({
      game_id: WDOOR_GAME_ID,
      started_at_ms: Date.UTC(2026, 9, 8, 15, 0, 0),
      ended_at_ms: Date.UTC(2026, 9, 8, 15, 21, 55),
      black_handle: "RAMU",
      white_handle: "Panda",
      result_kind: "WIN_WHITE",
      end_reason: "RESIGN",
      moves_count: 2,
      clock: { kind: "fischer", total_sec: 300, increment_sec: 10 },
      source: "wdoor",
      event: "floodgate-300-10F",
    });
  });

  it("stores an uploaded kifu once and serves it through list, search and single-game APIs", async () => {
    const csa = uploadCsa();
    const first = await post("/api/v1/admin/games/upload", { csa, event: " WCSC36 " });
    expect(first.status).toBe(200);
    expect(first.headers.get("Cache-Control")).toBe("no-store");
    const { game_id: gameId, created } = (await first.json()) as {
      game_id: string;
      created: boolean;
    };
    expect(created).toBe(true);
    expect(gameId).toMatch(/^upload-[0-9a-f]{16}$/);

    // 同じ本文は同じ ID になり、2 回目は保存をスキップする。
    const second = await post("/api/v1/admin/games/upload", { csa, event: "other" });
    expect(await second.json()).toEqual({ game_id: gameId, created: false });

    const startedAt = Date.UTC(2026, 4, 3, 1, 0, 0);
    const expectedMeta = {
      game_id: gameId,
      started_at_ms: startedAt,
      ended_at_ms: startedAt + 7_000,
      black_handle: "alice",
      white_handle: "bob",
      result_kind: "WIN_WHITE",
      end_reason: "RESIGN",
      moves_count: 2,
      clock: { kind: "countdown", total_sec: 900, byoyomi_sec: 5 },
      source: "upload",
      event: "WCSC36",
    };

    // R2 には本文・meta・games-index の 3 件だけを書く (日付別コピーは書かない)。
    const bucket = await getKifuBucket(mf);
    const keys = (await bucket.list()).objects.map((o) => o.key).sort();
    expect(keys).toHaveLength(3);
    expect(keys).toContain(`kifu-by-id/${gameId}.csa`);
    expect(keys).toContain(`kifu-by-id/${gameId}.meta.json`);
    expect(keys.filter((key) => key.startsWith("games-index/"))).toHaveLength(1);
    expect(await (await bucket.get(`kifu-by-id/${gameId}.csa`))!.text()).toBe(csa);

    const single = await viewerGet(`/api/v1/games/${gameId}`);
    expect(single.status).toBe(200);
    expect(await single.json()).toEqual({ game_id: gameId, csa, meta: expectedMeta });

    const list = await viewerGet("/api/v1/games");
    expect(((await list.json()) as { games: unknown[] }).games).toEqual([expectedMeta]);

    for (const [query, expectedCount] of [
      ["source=upload", 1],
      ["source=kifu,upload", 1],
      ["source=kifu,floodgate", 0],
      ["event=WCSC36", 1],
      ["event=WCSC", 0],
      ["source=upload&event=WCSC36&name=ali", 1],
    ] as const) {
      const search = await viewerGet(`/api/v1/games/search?${query}`);
      expect(search.status, query).toBe(200);
      const body = (await search.json()) as { games: unknown[]; total_count: number };
      expect(body.total_count, query).toBe(expectedCount);
      // 検索結果のメタも単局と同じ形 (player ID なし、event つき)。
      expect(body.games, query).toEqual(expectedCount ? [expectedMeta] : []);
    }
  });

  it("force overwrites and keeps a single games-index entry", async () => {
    const csa = uploadCsa();
    const first = await post("/api/v1/admin/games/upload", { csa, event: "before" });
    const { game_id: gameId } = (await first.json()) as { game_id: string };

    const forced = await post("/api/v1/admin/games/upload", { csa, event: "after", force: true });
    expect(await forced.json()).toEqual({ game_id: gameId, created: true });

    const bucket = await getKifuBucket(mf);
    const indexKeys = (await bucket.list({ prefix: "games-index/" })).objects;
    expect(indexKeys).toHaveLength(1);
    const meta = JSON.parse(
      await (await bucket.get(`kifu-by-id/${gameId}.meta.json`))!.text(),
    ) as { event: string };
    expect(meta.event).toBe("after");
    const db = await mf.getD1Database("GAMES_SEARCH_DB");
    expect(
      await db.prepare("SELECT event, source FROM games_search_index WHERE game_id = ?").bind(gameId).all(),
    ).toMatchObject({ results: [{ event: "after", source: "upload" }] });
  });

  it("rejects unplayable, oversized and malformed uploads without storing anything", async () => {
    const illegal = uploadCsa().replace("-3334FU,T4", "-3335FU,T4");
    const cases: Array<[string, unknown, number]> = [
      ["illegal move", { csa: illegal }, 422],
      ["no start time", { csa: uploadCsa().replace("$START_TIME:2026/05/03 10:00:00\n", "") }, 422],
      ["empty csa", { csa: "" }, 422],
      ["wdoor source without wdoor event", { csa: uploadCsa(), source: "wdoor" }, 422],
      ["unknown source", { csa: uploadCsa(), source: "kifu" }, 400],
      ["event too long", { csa: uploadCsa(), event: "x".repeat(101) }, 400],
      ["missing csa", { event: "WCSC36" }, 400],
      ["oversized csa", { csa: uploadCsa(`'${"x".repeat(512 * 1024)}`) }, 413],
    ];
    for (const [label, body, status] of cases) {
      const res = await post("/api/v1/admin/games/upload", body);
      expect(res.status, label).toBe(status);
      expect(res.headers.get("Cache-Control"), label).toBe("no-store");
      await res.arrayBuffer();
    }
    const bucket = await getKifuBucket(mf);
    expect((await bucket.list()).objects).toEqual([]);
    const db = await mf.getD1Database("GAMES_SEARCH_DB");
    expect(await db.prepare("SELECT COUNT(*) AS n FROM games_search_index").first()).toEqual({ n: 0 });
  });

  it("lists event tags by game count and keeps imported games out of ratings", async () => {
    // 自前サーバーの対局 1 局 (レーティング対象) を D1 に直接置く。
    const db = await mf.getD1Database("GAMES_SEARCH_DB");
    await db
      .prepare(
        `INSERT INTO games_search_index
         (game_id, sente_name, gote_name, black_player_id, white_player_id,
          started_at_ms, ended_at_ms, result_kind, source, moves_count,
          wire_result_kind, end_reason, clock_json)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
      )
      .bind("g1", "alice", "bob", "p_alice", "p_bob", 10, 20, "resignation", "kifu", 40, "WIN_BLACK", "RESIGN", "{}")
      .run();

    // WCSC36 を 2 局、Denryu を 1 局、タグなしを 1 局、wdoor を 1 局取り込む。
    const uploads: Array<[string, Record<string, unknown>]> = [
      ["'a", { event: "WCSC36" }],
      ["'b", { event: "WCSC36" }],
      ["'c", { event: "Denryu" }],
      ["'d", {}],
    ];
    for (const [comment, extra] of uploads) {
      const res = await post("/api/v1/admin/games/upload", { csa: uploadCsa(comment), ...extra });
      expect(res.status).toBe(200);
      await res.arrayBuffer();
    }
    const wdoor = await post("/api/v1/admin/games/upload", { csa: WDOOR_CSA, source: "wdoor" });
    expect(wdoor.status).toBe(200);
    await wdoor.arrayBuffer();

    const events = await viewerGet("/api/v1/games/events");
    expect(events.status).toBe(200);
    expect(events.headers.get("Cache-Control")).toBe(
      "public, s-maxage=60, max-age=0, must-revalidate",
    );
    expect(events.headers.get("Access-Control-Allow-Origin")).toBe(ORIGIN);
    // 件数の多い順、同数は event 昇順。タグなしの対局は数えない。
    expect(await events.json()).toEqual({
      events: [
        { event: "WCSC36", source: "upload", games: 2 },
        { event: "Denryu", source: "upload", games: 1 },
        { event: "floodgate-300-10F", source: "wdoor", games: 1 },
      ],
    });

    // Origin が allowlist 外なら他の一覧 API と同じく 403。
    const blocked = await mf.dispatchFetch("http://localhost/api/v1/games/events", {
      headers: { Origin: "https://evil.example" },
    });
    expect(blocked.status).toBe(403);
    await blocked.arrayBuffer();

    // 取り込みは rating state を汚さない (revision を進めず、rebuild も要求しない)。
    expect(
      await db.prepare("SELECT data_revision FROM player_rating_state WHERE singleton = 1").first(),
    ).toEqual({ data_revision: 0 });

    const warmed = await post("/api/v1/admin/player-ratings/warmup", {});
    expect(warmed.status).toBe(200);
    // 集計対象は自前サーバーの 1 局だけ。取り込んだ 5 局は読まない。
    expect(await warmed.json()).toMatchObject({ processed_games: 1, ready: true });

    const players = await viewerGet("/api/v1/players");
    const playersBody = (await players.json()) as {
      players: Array<{ player_id: string }>;
      total_games: number;
    };
    expect(playersBody.players.map((p) => p.player_id).sort()).toEqual(["p_alice", "p_bob"]);
    expect(playersBody.total_games).toBe(1);

    // 取り込んだ棋譜は player ID を持たない。
    expect(
      await db
        .prepare(
          "SELECT COUNT(*) AS n FROM games_search_index WHERE source IN ('wdoor','upload') AND (black_player_id IS NOT NULL OR white_player_id IS NOT NULL)",
        )
        .first(),
    ).toEqual({ n: 0 });

    // プレイヤー詳細の対局一覧にも外部棋譜は出ない (同名 alice の upload 局があっても)。
    const detail = await viewerGet("/api/v1/players/p_alice");
    expect(detail.status).toBe(200);
    const detailBody = (await detail.json()) as {
      games: Array<{ game_id: string }>;
      total_count: number;
    };
    expect(detailBody.total_count).toBe(1);
    expect(detailBody.games.map((g) => g.game_id)).toEqual(["g1"]);
  });

  it("serves an empty event list when no game has an event", async () => {
    const empty = await viewerGet("/api/v1/games/events");
    expect(await empty.json()).toEqual({ events: [] });
  });
});
