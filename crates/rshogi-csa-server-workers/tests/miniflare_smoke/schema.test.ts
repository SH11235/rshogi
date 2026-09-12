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
