//! D1 上の Elo materialized snapshot を有界 batch で更新する。

pub const PAGE_SIZE: u32 = 30;
pub const MAX_PAGES_PER_RUN: u32 = 6;

/// Only the lease holder may mutate a building generation. D1 reports one
/// changed row for the winner and zero for concurrent cron/admin callers.
#[cfg(target_arch = "wasm32")]
fn lease_was_acquired(changes: usize) -> bool {
    changes == 1
}

/// A page shorter than the query limit proves that the cursor reached the end
/// of the snapshot observed by that query, so the generation can be activated
/// without spending a seventh D1 page request.
#[cfg(target_arch = "wasm32")]
fn page_reached_end(row_count: usize) -> bool {
    row_count < PAGE_SIZE as usize
}

#[cfg(any(target_arch = "wasm32", test))]
fn snapshot_is_ready(
    active_generation: Option<i64>,
    building_generation: i64,
    rebuild_required: bool,
) -> bool {
    !rebuild_required && active_generation == Some(building_generation)
}

#[cfg(any(target_arch = "wasm32", test))]
fn deadline_reached(deadline_at_ms: Option<u64>, now_ms: u64) -> bool {
    deadline_at_ms.is_some_and(|deadline| now_ms >= deadline)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaterializationOutcome {
    pub processed_games: u32,
    pub active_generation: Option<i64>,
    pub rebuild_in_progress: bool,
    pub lease_acquired: bool,
    pub deadline_reached: bool,
}

#[cfg(target_arch = "wasm32")]
mod imp {
    use std::collections::{BTreeMap, BTreeSet};

    use serde::Deserialize;
    use worker::wasm_bindgen::JsValue;
    use worker::{Date, Env, Result};

    use super::{
        MAX_PAGES_PER_RUN, MaterializationOutcome, PAGE_SIZE, deadline_reached, lease_was_acquired,
        page_reached_end, snapshot_is_ready,
    };
    use crate::config::ConfigKeys;
    use crate::games_search_index::SearchRow;
    use crate::player_ratings::{PlayerGame, PlayerSummary, apply_player_games};

    const LEASE_MS: u64 = 60_000;

    #[derive(Debug, Deserialize)]
    struct StateRow {
        active_generation: Option<i64>,
        building_generation: i64,
        cursor_ended_at_ms: i64,
        cursor_game_id: String,
        rebuild_required: i64,
        data_revision: i64,
    }

    #[derive(Debug, Deserialize)]
    struct AliasRow {
        alias_id: String,
        canonical_id: String,
    }

    #[derive(Debug, Deserialize)]
    struct RatingRow {
        player_id: String,
        display_name: String,
        rating: f64,
        wins: u64,
        losses: u64,
        draws: u64,
        games: u64,
        last_played_at_ms: u64,
        legacy: i64,
    }

    impl From<RatingRow> for PlayerSummary {
        fn from(row: RatingRow) -> Self {
            Self {
                player_id: row.player_id,
                display_name: row.display_name,
                rating: row.rating,
                wins: row.wins,
                losses: row.losses,
                draws: row.draws,
                games: row.games,
                last_played_at_ms: row.last_played_at_ms,
                legacy: row.legacy != 0,
            }
        }
    }

    /// cron / admin warmup の双方から呼ぶ bounded materializer。
    pub async fn run_player_rating_materialization(
        env: &Env,
        requested_max_pages: u32,
        deadline_at_ms: Option<u64>,
    ) -> Result<MaterializationOutcome> {
        if deadline_reached(deadline_at_ms, Date::now().as_millis()) {
            return Ok(MaterializationOutcome {
                processed_games: 0,
                active_generation: None,
                rebuild_in_progress: true,
                lease_acquired: false,
                deadline_reached: true,
            });
        }
        let db = env.d1(ConfigKeys::GAMES_SEARCH_DB_BINDING)?;
        let now = Date::now().as_millis();
        let lease_values = [
            JsValue::from_f64(now.saturating_add(LEASE_MS) as f64),
            JsValue::from_f64(now as f64),
        ];
        let lease_result = db
            .prepare("UPDATE player_rating_state SET lease_until_ms = ? WHERE singleton = 1 AND lease_until_ms <= ?")
            .bind(&lease_values)?
            .run()
            .await?;
        let acquired =
            lease_was_acquired(lease_result.meta()?.and_then(|meta| meta.changes).unwrap_or(0));
        if !acquired {
            let state = load_state(&db).await?;
            return Ok(MaterializationOutcome {
                processed_games: 0,
                active_generation: state.active_generation,
                rebuild_in_progress: true,
                lease_acquired: false,
                deadline_reached: false,
            });
        }

        let result =
            run_with_lease(&db, requested_max_pages.clamp(1, MAX_PAGES_PER_RUN), deadline_at_ms)
                .await;
        // error 時も lease を早期解放する。失敗しても元の error を優先する。
        let _ = db
            .prepare("UPDATE player_rating_state SET lease_until_ms = 0 WHERE singleton = 1")
            .run()
            .await;
        result
    }

    async fn run_with_lease(
        db: &worker::D1Database,
        max_pages: u32,
        deadline_at_ms: Option<u64>,
    ) -> Result<MaterializationOutcome> {
        let mut state = load_state(db).await?;
        if state.rebuild_required != 0 {
            let next_generation = state.active_generation.unwrap_or(0).saturating_add(1);
            let delete_values = [JsValue::from_f64(next_generation as f64)];
            let reset_values = [JsValue::from_f64(next_generation as f64)];
            db.batch(vec![
                db.prepare("DELETE FROM player_rating_generations WHERE generation = ?")
                    .bind(&delete_values)?,
                db.prepare("UPDATE player_rating_state SET building_generation = ?, cursor_ended_at_ms = -1, cursor_game_id = '', rebuild_required = 0 WHERE singleton = 1")
                    .bind(&reset_values)?,
            ])
            .await?;
            state = load_state(db).await?;
        }

        let mut processed_games = 0_u32;
        for _ in 0..max_pages {
            if deadline_reached(deadline_at_ms, Date::now().as_millis()) {
                return Ok(deadline_outcome(&state, processed_games));
            }
            let rows = load_game_page(db, &state).await?;
            if deadline_reached(deadline_at_ms, Date::now().as_millis()) {
                return Ok(deadline_outcome(&state, processed_games));
            }
            if rows.is_empty() {
                return activate_if_clean(db, state.building_generation, processed_games).await;
            }

            let (games, player_ids) = canonicalize_page(db, &rows).await?;
            let existing =
                load_existing_players(db, state.building_generation, &player_ids).await?;
            let updated = apply_player_games(existing, &games);
            let last = rows.last().expect("non-empty page").to_player_game();
            let persisted = persist_page(
                db,
                state.building_generation,
                state.data_revision,
                &updated,
                &games,
                &last,
            )
            .await?;
            if !persisted {
                // The page was derived from a stale read. Every page mutation
                // was revision-guarded and therefore discarded; the same D1
                // batch marked the generation for a clean rebuild.
                state.rebuild_required = 1;
                return Ok(MaterializationOutcome {
                    processed_games,
                    active_generation: state.active_generation,
                    rebuild_in_progress: true,
                    lease_acquired: true,
                    deadline_reached: false,
                });
            }
            processed_games = processed_games.saturating_add(rows.len() as u32);
            state.cursor_ended_at_ms = last.ended_at_ms as i64;
            state.cursor_game_id = last.game_id;
            if deadline_reached(deadline_at_ms, Date::now().as_millis()) {
                return Ok(deadline_outcome(&state, processed_games));
            }
            if page_reached_end(rows.len()) {
                return activate_if_clean(db, state.building_generation, processed_games).await;
            }
        }

        let final_state = load_state(db).await?;
        Ok(MaterializationOutcome {
            processed_games,
            active_generation: final_state.active_generation,
            rebuild_in_progress: true,
            lease_acquired: true,
            deadline_reached: false,
        })
    }

    fn deadline_outcome(state: &StateRow, processed_games: u32) -> MaterializationOutcome {
        MaterializationOutcome {
            processed_games,
            active_generation: state.active_generation,
            rebuild_in_progress: true,
            lease_acquired: true,
            deadline_reached: true,
        }
    }

    async fn activate_if_clean(
        db: &worker::D1Database,
        building_generation: i64,
        processed_games: u32,
    ) -> Result<MaterializationOutcome> {
        // A concurrent alias registration or late row marks the build dirty.
        // The WHERE clause then leaves the prior active generation untouched.
        let activation_values = [JsValue::from_f64(building_generation as f64)];
        db.prepare("UPDATE player_rating_state SET active_generation = ?, lease_until_ms = 0 WHERE singleton = 1 AND rebuild_required = 0")
            .bind(&activation_values)?
            .run()
            .await?;
        let final_state = load_state(db).await?;
        Ok(MaterializationOutcome {
            processed_games,
            active_generation: final_state.active_generation,
            rebuild_in_progress: !snapshot_is_ready(
                final_state.active_generation,
                final_state.building_generation,
                final_state.rebuild_required != 0,
            ),
            lease_acquired: true,
            deadline_reached: false,
        })
    }

    async fn load_state(db: &worker::D1Database) -> Result<StateRow> {
        db.prepare("SELECT active_generation, building_generation, cursor_ended_at_ms, cursor_game_id, rebuild_required, data_revision FROM player_rating_state WHERE singleton = 1")
            .first::<StateRow>(None)
            .await?
            .ok_or_else(|| worker::Error::RustError("player_rating_state missing".into()))
    }

    async fn load_game_page(db: &worker::D1Database, state: &StateRow) -> Result<Vec<SearchRow>> {
        let values = [
            JsValue::from_f64(state.cursor_ended_at_ms as f64),
            JsValue::from_f64(state.cursor_ended_at_ms as f64),
            JsValue::from_str(&state.cursor_game_id),
            JsValue::from_f64(f64::from(PAGE_SIZE)),
        ];
        db.prepare("SELECT game_id, started_at_ms, ended_at_ms, sente_name, gote_name, black_player_id, white_player_id, wire_result_kind, end_reason, moves_count, clock_json, source FROM games_search_index WHERE ended_at_ms > ? OR (ended_at_ms = ? AND game_id > ?) ORDER BY ended_at_ms ASC, game_id ASC LIMIT ?")
            .bind(&values)?
            .all()
            .await?
            .results::<SearchRow>()
    }

    async fn canonicalize_page(
        db: &worker::D1Database,
        rows: &[SearchRow],
    ) -> Result<(Vec<PlayerGame>, BTreeSet<String>)> {
        let raw_games: Vec<_> = rows.iter().map(SearchRow::to_player_game).collect();
        let raw_ids: BTreeSet<_> = raw_games
            .iter()
            .flat_map(|game| {
                [
                    game.resolved_black_player_id(),
                    game.resolved_white_player_id(),
                ]
            })
            .collect();
        let aliases = load_aliases(db, &raw_ids).await?;
        let mut canonical_ids = BTreeSet::new();
        let games = raw_games
            .into_iter()
            .map(|mut game| {
                let black = game.resolved_black_player_id();
                let white = game.resolved_white_player_id();
                let black = aliases.get(&black).cloned().unwrap_or(black);
                let white = aliases.get(&white).cloned().unwrap_or(white);
                canonical_ids.insert(black.clone());
                canonical_ids.insert(white.clone());
                game.black_player_id = Some(black);
                game.white_player_id = Some(white);
                game
            })
            .collect();
        Ok((games, canonical_ids))
    }

    async fn load_aliases(
        db: &worker::D1Database,
        ids: &BTreeSet<String>,
    ) -> Result<BTreeMap<String, String>> {
        if ids.is_empty() {
            return Ok(BTreeMap::new());
        }
        let placeholders = std::iter::repeat_n("?", ids.len()).collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT alias_id, canonical_id FROM player_id_aliases WHERE alias_id IN ({placeholders})"
        );
        let values: Vec<_> = ids.iter().map(|id| JsValue::from_str(id)).collect();
        let rows = db.prepare(&sql).bind(&values)?.all().await?.results::<AliasRow>()?;
        Ok(rows.into_iter().map(|row| (row.alias_id, row.canonical_id)).collect())
    }

    async fn load_existing_players(
        db: &worker::D1Database,
        generation: i64,
        ids: &BTreeSet<String>,
    ) -> Result<Vec<PlayerSummary>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = std::iter::repeat_n("?", ids.len()).collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT player_id, display_name, rating, wins, losses, draws, games, last_played_at_ms, legacy FROM player_rating_generations WHERE generation = ? AND player_id IN ({placeholders})"
        );
        let mut values = vec![JsValue::from_f64(generation as f64)];
        values.extend(ids.iter().map(|id| JsValue::from_str(id)));
        Ok(db
            .prepare(&sql)
            .bind(&values)?
            .all()
            .await?
            .results::<RatingRow>()?
            .into_iter()
            .map(PlayerSummary::from)
            .collect())
    }

    async fn persist_page(
        db: &worker::D1Database,
        generation: i64,
        expected_revision: i64,
        players: &[PlayerSummary],
        games: &[PlayerGame],
        last: &PlayerGame,
    ) -> Result<bool> {
        let mut statements = Vec::with_capacity(players.len() + games.len() + 2);
        for player in players {
            let values = [
                JsValue::from_f64(generation as f64),
                JsValue::from_str(&player.player_id),
                JsValue::from_str(&player.display_name),
                JsValue::from_f64(player.rating),
                JsValue::from_f64(player.wins as f64),
                JsValue::from_f64(player.losses as f64),
                JsValue::from_f64(player.draws as f64),
                JsValue::from_f64(player.games as f64),
                JsValue::from_f64(player.last_played_at_ms as f64),
                JsValue::from_f64(if player.legacy { 1.0 } else { 0.0 }),
                JsValue::from_f64(expected_revision as f64),
            ];
            statements.push(
                db.prepare("INSERT INTO player_rating_generations (generation, player_id, display_name, rating, wins, losses, draws, games, last_played_at_ms, legacy) SELECT ?, ?, ?, ?, ?, ?, ?, ?, ?, ? WHERE (SELECT data_revision FROM player_rating_state WHERE singleton = 1) = ? ON CONFLICT(generation, player_id) DO UPDATE SET display_name=excluded.display_name, rating=excluded.rating, wins=excluded.wins, losses=excluded.losses, draws=excluded.draws, games=excluded.games, last_played_at_ms=excluded.last_played_at_ms, legacy=excluded.legacy")
                    .bind(&values)?,
            );
        }
        // 旧 NULL ID 行も同じ bounded pass で canonical ID に補完する。これにより
        // detail API は name hash の逆引きをせず ID index だけで過去局を検索できる。
        for game in games {
            let values = [
                JsValue::from_str(
                    game.black_player_id.as_deref().expect("canonicalized before persist"),
                ),
                JsValue::from_str(
                    game.white_player_id.as_deref().expect("canonicalized before persist"),
                ),
                JsValue::from_str(&game.game_id),
                JsValue::from_f64(expected_revision as f64),
            ];
            statements.push(
                db.prepare("UPDATE games_search_index SET black_player_id = ?, white_player_id = ? WHERE game_id = ? AND (SELECT data_revision FROM player_rating_state WHERE singleton = 1) = ?")
                    .bind(&values)?,
            );
        }
        let cursor_values = [
            JsValue::from_f64(last.ended_at_ms as f64),
            JsValue::from_str(&last.game_id),
            JsValue::from_f64(expected_revision as f64),
        ];
        statements.push(
            db.prepare("UPDATE player_rating_state SET cursor_ended_at_ms = ?, cursor_game_id = ? WHERE singleton = 1 AND data_revision = ?")
                .bind(&cursor_values)?,
        );
        let mismatch_values = [JsValue::from_f64(expected_revision as f64)];
        statements.push(
            db.prepare("UPDATE player_rating_state SET rebuild_required = 1 WHERE singleton = 1 AND data_revision <> ?")
                .bind(&mismatch_values)?,
        );
        let results = db.batch(statements).await?;
        let cursor_result = results.get(results.len().saturating_sub(2)).ok_or_else(|| {
            worker::Error::RustError("player rating cursor result missing".into())
        })?;
        Ok(cursor_result.meta()?.and_then(|meta| meta.changes).unwrap_or(0) == 1)
    }
}

#[cfg(target_arch = "wasm32")]
pub use imp::run_player_rating_materialization;

#[cfg(test)]
mod tests {

    #[test]
    fn initial_generation_is_unavailable_until_clean_activation() {
        assert!(!super::snapshot_is_ready(None, 1, false));
        assert!(!super::snapshot_is_ready(Some(1), 2, false));
        assert!(!super::snapshot_is_ready(Some(2), 2, true));
        assert!(super::snapshot_is_ready(Some(2), 2, false));
    }

    #[test]
    fn shared_deadline_stops_at_exact_boundary_but_admin_is_unlimited() {
        assert!(!super::deadline_reached(Some(25_000), 24_999));
        assert!(super::deadline_reached(Some(25_000), 25_000));
        assert!(super::deadline_reached(Some(25_000), 25_001));
        assert!(!super::deadline_reached(None, u64::MAX));
    }
}
