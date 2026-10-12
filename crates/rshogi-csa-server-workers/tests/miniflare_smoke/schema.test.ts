import { readFileSync } from "node:fs";
import { DatabaseSync } from "node:sqlite";
import { expect, test } from "vitest";

const root = new URL("../../", import.meta.url);
const file = (path: string) => readFileSync(new URL(path, root), "utf8");

test("GameRoom schema can be reapplied without losing existing moves or clock credits", () => {
  const schema = file("src/game_room.rs").match(/const SCHEMA_SQL: &str = r#"([\s\S]*?)"#;/)?.[1];
  expect(schema).toBeTruthy();
  const db = new DatabaseSync(":memory:");
  try {
    db.exec("CREATE TABLE moves (ply INTEGER PRIMARY KEY, color TEXT NOT NULL, line TEXT NOT NULL, at_ms INTEGER NOT NULL)");
    db.exec("INSERT INTO moves VALUES (1, '+', '+7776FU,T3', 1234)");
    db.exec(schema!);
    db.exec("INSERT INTO reconnect_clock_credits VALUES (1, 250)");
    db.exec(schema!);
    expect(db.prepare("SELECT * FROM moves").all()).toEqual([{ ply: 1, color: "+", line: "+7776FU,T3", at_ms: 1234 }]);
    expect(db.prepare("SELECT * FROM reconnect_clock_credits").all()).toEqual([{ ply: 1, credited_ms: 250 }]);
  } finally {
    db.close();
  }
});

test("D1 migrations retain old games and initialize the rating generation state", () => {
  const db = new DatabaseSync(":memory:");
  try {
    db.exec(file("migrations/0001_games_search_index.sql"));
    db.exec("INSERT INTO games_search_index VALUES ('g1', 'alice', 'bob', 1, 2, 'resignation', 'kifu', 1, 'WIN_BLACK', 'RESIGN', '{}')");
    for (const name of ["0002_games_index_backfill_state.sql", "0003_games_search_index_player_ids.sql", "0004_player_rating_materialization.sql"]) {
      db.exec(file(`migrations/${name}`));
    }
    expect(db.prepare("SELECT game_id, sente_name, black_player_id, white_player_id FROM games_search_index").all()).toEqual([
      { game_id: "g1", sente_name: "alice", black_player_id: null, white_player_id: null },
    ]);
    expect(db.prepare("SELECT active_generation, rebuild_required, data_revision FROM player_rating_state").all()).toEqual([
      { active_generation: null, rebuild_required: 1, data_revision: 0 },
    ]);
    db.exec("INSERT INTO player_id_aliases VALUES ('old', 'new')");
    expect(db.prepare("SELECT alias_id FROM player_id_aliases WHERE canonical_id = ?").all("new")).toEqual([{ alias_id: "old" }]);
  } finally {
    db.close();
  }
});

test("event migration keeps old rows untagged and the event tag query is served by its index", () => {
  const eventTagsSql = file("src/games_search_index.rs").match(/pub const EVENT_TAGS_SQL: &str = "([^"]+)";/)?.[1];
  expect(eventTagsSql).toBeTruthy();
  const db = new DatabaseSync(":memory:");
  try {
    db.exec(file("migrations/0001_games_search_index.sql"));
    db.exec("INSERT INTO games_search_index VALUES ('g1', 'alice', 'bob', 1, 2, 'resignation', 'kifu', 1, 'WIN_BLACK', 'RESIGN', '{}')");
    for (const name of [
      "0002_games_index_backfill_state.sql",
      "0003_games_search_index_player_ids.sql",
      "0004_player_rating_materialization.sql",
      "0005_games_search_index_event.sql",
      "0006_event_meta.sql",
    ]) {
      db.exec(file(`migrations/${name}`));
    }
    // 既存の対局は event / origin_url を持たないまま残り、タグ一覧には出ない。
    expect(db.prepare("SELECT game_id, event, origin_url FROM games_search_index").all()).toEqual([
      { game_id: "g1", event: null, origin_url: null },
    ]);
    expect(db.prepare(eventTagsSql!).all()).toEqual([]);

    const insert = db.prepare(
      `INSERT INTO games_search_index
       (game_id, sente_name, gote_name, started_at_ms, ended_at_ms, result_kind, source,
        moves_count, wire_result_kind, end_reason, clock_json, event)
       VALUES (?, 'a', 'b', 1, 2, 'resignation', ?, 1, 'WIN_BLACK', 'RESIGN', 'null', ?)`,
    );
    const rows: Array<[string, string, string | null]> = [
      ["u1", "upload", "WCSC36"],
      ["u2", "upload", "WCSC36"],
      ["u3", "upload", "WCSC36"],
      ["u4", "upload", "Denryu"],
      ["u5", "upload", "Alpha"],
      ["u6", "upload", ""],
      ["u7", "upload", null],
      ["w1", "wdoor", "floodgate-300-10F"],
      ["w2", "wdoor", "floodgate-300-10F"],
      // 同じ event 名でも source が違えば別の行になる。
      ["w3", "wdoor", "Alpha"],
    ];
    for (const row of rows) insert.run(...row);

    // 件数の多い順、同数は event 昇順 (さらに source 昇順)。空文字と NULL は数えない。
    expect(db.prepare(eventTagsSql!).all()).toEqual([
      { event: "WCSC36", source: "upload", games: 3 },
      { event: "floodgate-300-10F", source: "wdoor", games: 2 },
      { event: "Alpha", source: "upload", games: 1 },
      { event: "Alpha", source: "wdoor", games: 1 },
      { event: "Denryu", source: "upload", games: 1 },
    ]);

    // 集計と event 完全一致検索はどちらも部分 index で引ける (全表走査しない)。
    const plan = (sql: string) =>
      (db.prepare(`EXPLAIN QUERY PLAN ${sql}`).all() as Array<{ detail: string }>)
        .map((row) => row.detail)
        .join("\n");
    expect(plan(eventTagsSql!)).toContain("USING COVERING INDEX games_search_index_event_source");
    expect(plan("SELECT COUNT(*) FROM games_search_index WHERE event = 'WCSC36'")).toContain(
      "games_search_index_event_source",
    );
  } finally {
    db.close();
  }
});
