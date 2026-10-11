import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { type Miniflare, Response as MfResponse } from "miniflare";
import { createMiniflare, getKifuBucket, makeTempPersistRoot } from "./harness.ts";

const ADMIN_TOKEN = "kifu-import-admin-token";
const ORIGIN = "https://example.com";
const MAX_CSA_BYTES = 512 * 1024;

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

// wdoor (shogi-server) 形式。
const WDOOR_ID = "wdoor+floodgate-300-10F+RAMU+Panda+20261009000001";
const WDOOR_GAME_ID = "wdoor-20261009000001-93ef4d36a6";
const WDOOR_URL = `https://wdoor.c.u-tokyo.ac.jp/shogi/x/2026/10/09/${WDOOR_ID}.csa`;
const WDOOR_HEAD = [
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
];
const WDOOR_CSA = [
  ...WDOOR_HEAD,
  "%TORYO",
  "'summary:toryo:RAMU lose:Panda win",
  "'$END_TIME:2026/10/09 00:21:55",
  "",
].join("\n");
// 対局中 (終局コマンドも summary も無い)。
const WDOOR_IN_PROGRESS_CSA = [...WDOOR_HEAD, ""].join("\n");
// `'$END_TIME` の無いアーカイブ。終了時刻は開始 + 消費時間 (10 秒) になる。
const WDOOR_ARCHIVE_WITHOUT_END_TIME = [
  ...WDOOR_HEAD,
  "%TORYO",
  "'summary:toryo:RAMU lose:Panda win",
  "",
].join("\n");

const WDOOR_STARTED_AT = Date.UTC(2026, 9, 8, 15, 0, 0);
const WDOOR_META = {
  game_id: WDOOR_GAME_ID,
  started_at_ms: WDOOR_STARTED_AT,
  ended_at_ms: Date.UTC(2026, 9, 8, 15, 21, 55),
  black_handle: "RAMU",
  white_handle: "Panda",
  result_kind: "WIN_WHITE",
  end_reason: "RESIGN",
  moves_count: 2,
  clock: { kind: "fischer", total_sec: 300, increment_sec: 10 },
  source: "wdoor",
  event: "floodgate-300-10F",
};

interface OutboundCall {
  url: string;
  method: string;
  userAgent: string | null;
}

/// 上限を超える量を chunk に分けて流す本文 (`Content-Length` が付かない)。
function oversizedStream(totalBytes: number): ReadableStream<Uint8Array> {
  const chunk = new TextEncoder().encode("'".padEnd(64 * 1024 - 1, "x") + "\n");
  let sent = 0;
  return new ReadableStream<Uint8Array>({
    pull(controller) {
      if (sent >= totalBytes) {
        controller.close();
        return;
      }
      controller.enqueue(chunk);
      sent += chunk.byteLength;
    },
  });
}

describe("miniflare smoke: external kifu import API", () => {
  let mf: Miniflare;
  let cleanupPersist: () => Promise<void>;
  /// Worker が外へ出した fetch の記録。wdoor 以外へ出ていないこと、回数、ヘッダを確認する。
  let outboundCalls: OutboundCall[];
  /// wdoor 役の応答。テストごとに差し替える。既定は「呼ばれたら失敗」。
  let wdoorResponse: () => MfResponse;

  beforeEach(async () => {
    const persist = await makeTempPersistRoot();
    cleanupPersist = persist.cleanup;
    outboundCalls = [];
    wdoorResponse = () => new MfResponse("unexpected outbound fetch", { status: 500 });
    mf = await createMiniflare({
      persistRoot: persist.path,
      allowViewerApi: true,
      adminApiToken: ADMIN_TOKEN,
      wsAllowedOrigins: ORIGIN,
      outboundService: (request) => {
        outboundCalls.push({
          url: request.url,
          method: request.method,
          userAgent: request.headers.get("User-Agent"),
        });
        return wdoorResponse();
      },
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

  const upload = (body: unknown) => post("/api/v1/admin/games/upload", body);
  const importWdoor = (body: unknown = { wdoor_game_id: WDOOR_ID }) =>
    post("/api/v1/admin/games/import-wdoor", body);

  function viewerGet(path: string) {
    return mf.dispatchFetch(`http://localhost${path}`, { headers: { Origin: ORIGIN } });
  }

  // miniflare の R2 binding は put / delete も持つが、harness の duck-type は読み取りだけなので
  // 障害を作るテストに限ってここで広げる。
  async function kifuBucket() {
    const bucket = await getKifuBucket(mf);
    return bucket as typeof bucket & {
      delete(key: string): Promise<void>;
      put(key: string, value: string): Promise<unknown>;
    };
  }

  async function indexKeys(): Promise<string[]> {
    const bucket = await getKifuBucket(mf);
    return (await bucket.list({ prefix: "games-index/" })).objects.map((o) => o.key);
  }

  async function storedMeta(gameId: string): Promise<Record<string, unknown>> {
    const bucket = await getKifuBucket(mf);
    return JSON.parse(await (await bucket.get(`kifu-by-id/${gameId}.meta.json`))!.text());
  }

  async function searchRows(): Promise<Array<Record<string, unknown>>> {
    const db = await mf.getD1Database("GAMES_SEARCH_DB");
    const result = await db
      .prepare("SELECT game_id, source, event, origin_url, ended_at_ms FROM games_search_index ORDER BY game_id")
      .all();
    return result.results as Array<Record<string, unknown>>;
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
    expect(outboundCalls).toEqual([]);
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
      const res = await importWdoor({ wdoor_game_id: wdoorGameId });
      expect(res.status, wdoorGameId).toBe(400);
      await res.arrayBuffer();
    }
    const malformed = await importWdoor("{not json");
    expect(malformed.status).toBe(400);
    await malformed.arrayBuffer();
    const missing = await importWdoor({ force: true });
    expect(missing.status).toBe(400);
    await missing.arrayBuffer();
    const empty = await importWdoor("");
    expect(empty.status).toBe(400);
    await empty.arrayBuffer();
    expect(outboundCalls).toEqual([]);
  });

  it("import-wdoor fetches the kifu once from the fixed wdoor URL and stores it with origin_url", async () => {
    wdoorResponse = () => new MfResponse(WDOOR_CSA, { headers: { "Content-Type": "text/plain" } });
    const imported = await importWdoor();
    expect(imported.status).toBe(200);
    expect(imported.headers.get("Cache-Control")).toBe("no-store");
    expect(await imported.json()).toEqual({ game_id: WDOOR_GAME_ID, created: true });

    // 取得先は ID から組み立てた固定 URL で、連絡先つきの UA を名乗る (リダイレクトを
    // 追従しないことは下の 302 のケースで確認する)。
    expect(outboundCalls).toEqual([
      {
        url: WDOOR_URL,
        method: "GET",
        userAgent: "rshogi-csa-server (+https://github.com/SH11235/rshogi)",
      },
    ]);

    const expectedMeta = { ...WDOOR_META, origin_url: WDOOR_URL };
    const game = await viewerGet(`/api/v1/games/${WDOOR_GAME_ID}`);
    expect(game.status).toBe(200);
    expect(await game.json()).toEqual({ game_id: WDOOR_GAME_ID, csa: WDOOR_CSA, meta: expectedMeta });
    const search = await viewerGet("/api/v1/games/search?source=wdoor");
    expect(((await search.json()) as { games: unknown[] }).games).toEqual([expectedMeta]);

    // 保存済みなら wdoor に取りに行かない。
    const again = await importWdoor();
    expect(await again.json()).toEqual({ game_id: WDOOR_GAME_ID, created: false });
    expect(outboundCalls).toHaveLength(1);

    // force のときだけ再取得する。
    const forced = await importWdoor({ wdoor_game_id: WDOOR_ID, force: true });
    expect(await forced.json()).toEqual({ game_id: WDOOR_GAME_ID, created: true });
    expect(outboundCalls).toHaveLength(2);
    expect(await indexKeys()).toHaveLength(1);
  });

  it("import-wdoor maps upstream outcomes to 404 / 409 / 413 / 502 without storing", async () => {
    const big = `${"'".padEnd(1023, "x")}\n`.repeat(MAX_CSA_BYTES / 1024 + 1);
    const otherGame = WDOOR_CSA.replace(WDOOR_ID, "wdoor+floodgate-300-10F+Panda+RAMU+20261009000001");
    const cases: Array<[string, () => MfResponse, number, string | null]> = [
      // この本文はクライアントがアーカイブへのフォールバック判定に使う固定文字列。
      ["missing on wdoor", () => new MfResponse("nginx 404", { status: 404 }), 404, "Not Found on wdoor"],
      ["game in progress", () => new MfResponse(WDOOR_IN_PROGRESS_CSA), 409, null],
      ["declared length over limit", () => new MfResponse(big), 413, null],
      ["stream over limit without Content-Length", () => new MfResponse(oversizedStream(MAX_CSA_BYTES + 1)), 413, null],
      ["upstream error", () => new MfResponse("oops", { status: 503 }), 502, null],
      [
        "redirect is not followed",
        () => new MfResponse(null, { status: 302, headers: { Location: "https://evil.example/x.csa" } }),
        502,
        null,
      ],
      ["kifu of another game", () => new MfResponse(otherGame), 502, null],
      ["not a kifu", () => new MfResponse("<html>maintenance</html>"), 502, null],
      ["unplayable kifu", () => new MfResponse(WDOOR_CSA.replace("-8384FU", "-8385FU")), 422, null],
      ["malformed move line", () => new MfResponse(WDOOR_CSA.replace("-8384FU", "-8384F?")), 422, null],
    ];
    for (const [label, response, status, body] of cases) {
      outboundCalls = [];
      wdoorResponse = response;
      const res = await importWdoor();
      expect(res.status, label).toBe(status);
      expect(res.headers.get("Cache-Control"), label).toBe("no-store");
      const text = await res.text();
      if (body !== null) expect(text, label).toBe(body);
      // どのケースも wdoor へは 1 回だけ (リダイレクト先には行かない)。
      expect(outboundCalls.map((call) => call.url), label).toEqual([WDOOR_URL]);
    }
    const bucket = await getKifuBucket(mf);
    expect((await bucket.list()).objects).toEqual([]);
    expect(await searchRows()).toEqual([]);

    // 経路が無いときの 404 は本文が違うので、クライアントは取り違えない。
    const unknownRoute = await post("/api/v1/admin/games/import-wdoor-typo", { wdoor_game_id: WDOOR_ID });
    expect(unknownRoute.status).toBe(404);
    expect(await unknownRoute.text()).not.toBe("Not Found on wdoor");
  });

  it("import-wdoor skips the fetch when the game is already stored from an archive", async () => {
    const uploaded = await upload({ csa: WDOOR_CSA, source: "wdoor", event: "ignored" });
    expect(uploaded.status).toBe(200);
    expect(await uploaded.json()).toEqual({ game_id: WDOOR_GAME_ID, created: true });

    const imported = await importWdoor();
    expect(imported.status).toBe(200);
    expect(await imported.json()).toEqual({ game_id: WDOOR_GAME_ID, created: false });
    expect(outboundCalls).toEqual([]);

    // アーカイブ経由では origin_url を付けない。event はリクエスト値ではなくゲーム名。
    const game = await viewerGet(`/api/v1/games/${WDOOR_GAME_ID}`);
    expect(await game.json()).toEqual({ game_id: WDOOR_GAME_ID, csa: WDOOR_CSA, meta: WDOOR_META });
  });

  it("archive force upload keeps the origin_url of a previously fetched game", async () => {
    wdoorResponse = () => new MfResponse(WDOOR_CSA);
    await (await importWdoor()).arrayBuffer();
    expect((await storedMeta(WDOOR_GAME_ID)).origin_url).toBe(WDOOR_URL);

    const forced = await upload({ csa: WDOOR_CSA, source: "wdoor", force: true });
    expect(await forced.json()).toEqual({ game_id: WDOOR_GAME_ID, created: true });
    expect((await storedMeta(WDOOR_GAME_ID)).origin_url).toBe(WDOOR_URL);
    expect(await searchRows()).toMatchObject([{ game_id: WDOOR_GAME_ID, origin_url: WDOOR_URL }]);
  });

  it("force with a changed end time replaces the games-index entry instead of duplicating it", async () => {
    const first = await upload({ csa: WDOOR_ARCHIVE_WITHOUT_END_TIME, source: "wdoor" });
    expect(await first.json()).toEqual({ game_id: WDOOR_GAME_ID, created: true });
    const beforeKeys = await indexKeys();
    expect(beforeKeys).toHaveLength(1);
    expect((await storedMeta(WDOOR_GAME_ID)).ended_at_ms).toBe(WDOOR_STARTED_AT + 10_000);

    // 同じ対局 ID で本文が違う (終了時刻つき) ものを force で上書きする。
    const forced = await upload({ csa: WDOOR_CSA, source: "wdoor", force: true });
    expect(await forced.json()).toEqual({ game_id: WDOOR_GAME_ID, created: true });

    const afterKeys = await indexKeys();
    expect(afterKeys).toHaveLength(1);
    expect(afterKeys[0]).not.toBe(beforeKeys[0]);
    expect(await storedMeta(WDOOR_GAME_ID)).toEqual(WDOOR_META);
    const list = await viewerGet("/api/v1/games");
    expect(((await list.json()) as { games: unknown[] }).games).toEqual([WDOOR_META]);
    expect(await searchRows()).toMatchObject([
      { game_id: WDOOR_GAME_ID, ended_at_ms: WDOOR_META.ended_at_ms },
    ]);
  });

  it("stores an uploaded kifu once and serves it through list, search and single-game APIs", async () => {
    const csa = uploadCsa();
    const first = await upload({ csa, event: " WCSC36 " });
    expect(first.status).toBe(200);
    expect(first.headers.get("Cache-Control")).toBe("no-store");
    const { game_id: gameId, created } = (await first.json()) as {
      game_id: string;
      created: boolean;
    };
    expect(created).toBe(true);
    expect(gameId).toMatch(/^upload-[0-9a-f]{16}$/);

    // 同じ本文は同じ ID になり、2 回目は保存をスキップする (event も書き換えない)。
    const second = await upload({ csa, event: "other" });
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
      // 空の event は絞り込みなし。
      ["event=", 1],
      ["source=upload&event=", 1],
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
    const first = await upload({ csa, event: "before" });
    const { game_id: gameId } = (await first.json()) as { game_id: string };

    const forced = await upload({ csa, event: "after", force: true });
    expect(await forced.json()).toEqual({ game_id: gameId, created: true });

    expect(await indexKeys()).toHaveLength(1);
    expect((await storedMeta(gameId)).event).toBe("after");
    expect(await searchRows()).toMatchObject([{ game_id: gameId, event: "after", source: "upload" }]);
  });

  it("returns 502 when the search index write fails and repairs it on the next identical request", async () => {
    const db = await mf.getD1Database("GAMES_SEARCH_DB");
    const csa = uploadCsa();

    // D1 の書き込みだけを失敗させる (表を一時的に別名へ退避)。
    await db.prepare("ALTER TABLE games_search_index RENAME TO games_search_index_offline").run();
    const failed = await upload({ csa, event: "WCSC36" });
    expect(failed.status).toBe(502);
    expect(failed.headers.get("Cache-Control")).toBe("no-store");
    await failed.arrayBuffer();
    await db.prepare("ALTER TABLE games_search_index_offline RENAME TO games_search_index").run();
    expect(await searchRows()).toEqual([]);

    // 同じリクエストの再実行 (force なし) で D1 が補われる。本文と meta は書けていたので
    // created は false。
    const retried = await upload({ csa, event: "WCSC36" });
    expect(retried.status).toBe(200);
    const { game_id: gameId, created } = (await retried.json()) as {
      game_id: string;
      created: boolean;
    };
    expect(created).toBe(false);
    expect(await searchRows()).toMatchObject([{ game_id: gameId, source: "upload", event: "WCSC36" }]);
    const events = await viewerGet("/api/v1/games/events");
    expect(await events.json()).toEqual({ events: [{ event: "WCSC36", source: "upload", games: 1 }] });

    // games-index と D1 の両方が欠けた状態 (過去日付の取り込みで cron backfill が再訪しない
    // ケース) も、再実行で両方戻る。
    const bucket = await kifuBucket();
    const indexKey = (await indexKeys())[0]!;
    await bucket.delete(indexKey);
    await db.prepare("DELETE FROM games_search_index").run();
    const repaired = await upload({ csa });
    expect(await repaired.json()).toEqual({ game_id: gameId, created: false });
    expect(await indexKeys()).toEqual([indexKey]);
    expect(await searchRows()).toMatchObject([{ game_id: gameId, event: "WCSC36" }]);
    const list = await viewerGet("/api/v1/games");
    expect(((await list.json()) as { games: unknown[] }).games).toHaveLength(1);
  });

  it("repeating a stored request does not rewrite the search index row", async () => {
    // 保存経路と修復経路で D1 に渡す値 (clock_json の文字列を含む) が同じなら、再実行は
    // 1 行も書かない。INSERT / UPDATE を trigger で数えて直接確かめる。
    const requests = [
      { csa: uploadCsa(), event: "WCSC36" }, // countdown の clock
      { csa: uploadCsa("'no clock").replace("$TIME_LIMIT:00:15+05\n", "") }, // clock なし
      { csa: WDOOR_CSA, source: "wdoor" }, // fischer の clock
    ];
    for (const request of requests) {
      const res = await upload(request);
      expect(((await res.json()) as { created: boolean }).created).toBe(true);
    }
    const db = await mf.getD1Database("GAMES_SEARCH_DB");
    const rowsSql =
      "SELECT game_id, sente_name, gote_name, black_player_id, white_player_id, started_at_ms, ended_at_ms, result_kind, source, moves_count, wire_result_kind, end_reason, clock_json, event, origin_url FROM games_search_index ORDER BY game_id";
    const before = (await db.prepare(rowsSql).all()).results;
    expect(before).toHaveLength(3);
    // キー昇順の正規形で保存されている。
    expect(before.map((row: Record<string, unknown>) => row.clock_json).sort()).toEqual([
      "null",
      '{"byoyomi_sec":5,"kind":"countdown","total_sec":900}',
      '{"increment_sec":10,"kind":"fischer","total_sec":300}',
    ]);

    await db.prepare("CREATE TABLE search_index_writes (game_id TEXT NOT NULL, op TEXT NOT NULL)").run();
    await db
      .prepare(
        "CREATE TRIGGER search_index_update_audit AFTER UPDATE ON games_search_index BEGIN INSERT INTO search_index_writes VALUES (NEW.game_id, 'update'); END",
      )
      .run();
    await db
      .prepare(
        "CREATE TRIGGER search_index_insert_audit AFTER INSERT ON games_search_index BEGIN INSERT INTO search_index_writes VALUES (NEW.game_id, 'insert'); END",
      )
      .run();

    for (const request of requests) {
      const res = await upload(request);
      expect(((await res.json()) as { created: boolean }).created).toBe(false);
    }
    expect((await db.prepare("SELECT * FROM search_index_writes").all()).results).toEqual([]);
    expect((await db.prepare(rowsSql).all()).results).toEqual(before);

    // trigger が効いていることの確認: 行を消してから再実行すると INSERT が 1 回記録される。
    await db.prepare("DELETE FROM games_search_index WHERE game_id = ?").bind(WDOOR_GAME_ID).run();
    await (await upload(requests[2])).arrayBuffer();
    expect((await db.prepare("SELECT * FROM search_index_writes").all()).results).toEqual([
      { game_id: WDOOR_GAME_ID, op: "insert" },
    ]);
    expect((await db.prepare(rowsSql).all()).results).toEqual(before);
  });

  it("force over an unreadable meta finds and removes the stale games-index entry", async () => {
    const first = await upload({ csa: WDOOR_ARCHIVE_WITHOUT_END_TIME, source: "wdoor" });
    expect(await first.json()).toEqual({ game_id: WDOOR_GAME_ID, created: true });
    const staleKeys = await indexKeys();
    expect(staleKeys).toHaveLength(1);
    // 別の対局の index は走査で巻き込まれない。
    const other = await upload({ csa: uploadCsa(), event: "WCSC36" });
    const { game_id: otherGameId } = (await other.json()) as { game_id: string };
    expect(await indexKeys()).toHaveLength(2);

    // meta を壊す。前回の終了時刻が読めないので、旧 index の key は計算できない。
    const bucket = await kifuBucket();
    await bucket.put(`kifu-by-id/${WDOOR_GAME_ID}.meta.json`, "{broken");

    // force なしでは直せない (修復の元になる meta が読めない)。
    const stuck = await upload({ csa: WDOOR_CSA, source: "wdoor" });
    expect(stuck.status).toBe(502);
    expect(await stuck.text()).toBe("Stored meta is unreadable; retry with force");

    // force は games-index を走査して旧 key を消してから書き直す。終了時刻が変わっても
    // 一覧に同じ対局が 2 件残らない。
    const forced = await upload({ csa: WDOOR_CSA, source: "wdoor", force: true });
    expect(await forced.json()).toEqual({ game_id: WDOOR_GAME_ID, created: true });
    const keys = await indexKeys();
    expect(keys).toHaveLength(2);
    expect(keys).not.toContain(staleKeys[0]);
    expect(keys.filter((key) => key.endsWith(`-${WDOOR_GAME_ID}.json`))).toHaveLength(1);
    expect(keys.filter((key) => key.endsWith(`-${otherGameId}.json`))).toHaveLength(1);
    expect(await storedMeta(WDOOR_GAME_ID)).toEqual(WDOOR_META);
    const list = await viewerGet("/api/v1/games");
    const games = ((await list.json()) as { games: Array<{ game_id: string }> }).games;
    expect(games.map((game) => game.game_id).sort()).toEqual([otherGameId, WDOOR_GAME_ID].sort());
  });

  it("import-wdoor repairs a stored game without refetching", async () => {
    wdoorResponse = () => new MfResponse(WDOOR_CSA);
    await (await importWdoor()).arrayBuffer();
    const db = await mf.getD1Database("GAMES_SEARCH_DB");
    await db.prepare("DELETE FROM games_search_index").run();

    const repaired = await importWdoor();
    expect(await repaired.json()).toEqual({ game_id: WDOOR_GAME_ID, created: false });
    expect(outboundCalls).toHaveLength(1);
    expect(await searchRows()).toMatchObject([
      { game_id: WDOOR_GAME_ID, source: "wdoor", origin_url: WDOOR_URL },
    ]);
  });

  it("rejects unplayable, oversized and malformed uploads without storing anything", async () => {
    const cases: Array<[string, unknown, number]> = [
      ["illegal move", { csa: uploadCsa().replace("-3334FU,T4", "-3335FU,T4") }, 422],
      ["malformed piece code", { csa: uploadCsa().replace("+7776FU,T3", "+7776F?,T3") }, 422],
      ["short move line", { csa: uploadCsa().replace("-3334FU,T4", "-3334") }, 422],
      ["non-digit square", { csa: uploadCsa().replace("-3334FU,T4", "-33a4FU,T4") }, 422],
      ["unknown piece code", { csa: uploadCsa().replace("-3334FU,T4", "-3334XX,T4") }, 422],
      // 1 行に連結した 2 手目は読まれないので、1 手の棋譜として受理しない。
      ["chained moves", { csa: uploadCsa().replace("+7776FU,T3\n-3334FU,T4", "+7776FU,T3,-3334FU,T4") }, 422],
      ["garbage after move", { csa: uploadCsa().replace("-3334FU,T4", "-3334FU,Tabc") }, 422],
      ["no start time", { csa: uploadCsa().replace("$START_TIME:2026/05/03 10:00:00\n", "") }, 422],
      ["empty csa", { csa: "" }, 422],
      ["wdoor source without wdoor event", { csa: uploadCsa(), source: "wdoor" }, 422],
      ["unknown source", { csa: uploadCsa(), source: "kifu" }, 400],
      ["event too long", { csa: uploadCsa(), event: "x".repeat(101) }, 400],
      ["missing csa", { event: "WCSC36" }, 400],
      ["oversized csa", { csa: uploadCsa(`'${"x".repeat(MAX_CSA_BYTES)}`) }, 413],
    ];
    for (const [label, body, status] of cases) {
      const res = await upload(body);
      expect(res.status, label).toBe(status);
      expect(res.headers.get("Cache-Control"), label).toBe("no-store");
      await res.arrayBuffer();
    }

    // `Content-Length` の無い (chunked) リクエストでも、上限を超えた時点で 413。
    const streamed = await mf.dispatchFetch("http://localhost/api/v1/admin/games/upload", {
      method: "POST",
      headers: { "Content-Type": "application/json", Authorization: `Bearer ${ADMIN_TOKEN}` },
      // リクエスト本文の上限 (CSA 512KB の JSON エスケープ分を見込んだ 6 倍 + 4KB) を最後の
      // chunk でちょうど超える量。送信の途中で応答が返って接続が切れるのを避ける。
      body: oversizedStream(6 * MAX_CSA_BYTES + 4 * 1024 + 1),
      duplex: "half",
    });
    expect(streamed.status).toBe(413);
    await streamed.arrayBuffer();

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
      const res = await upload({ csa: uploadCsa(comment), ...extra });
      expect(res.status).toBe(200);
      await res.arrayBuffer();
    }
    const wdoor = await upload({ csa: WDOOR_CSA, source: "wdoor" });
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
