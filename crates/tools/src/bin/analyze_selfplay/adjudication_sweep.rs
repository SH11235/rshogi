//! `--adjudication-sweep`: 両側の評価の一致による裁定 (`--adjudicate-agreement`) を、記録済みの
//! 自己対局ログに後から当てはめ、閾値 X と手数 N の組ごとに「裁定が実際の結果と食い違う率」と
//! 「縮む手数の割合」を出す。判定は対局と同じ `ScoreAdjudicator` を使う。

use std::collections::HashMap;
use std::io::{BufRead, BufReader};

use anyhow::{Context, Result};
use rshogi_core::types::{Color, Move};
use serde::{Deserialize, Serialize};

use tools::selfplay::EvalLog;
use tools::selfplay::adjudication::{ResignRule, ScoreAdjudicator};

#[derive(Deserialize)]
struct SweepMove {
    game_id: u32,
    ply: u32,
    side_to_move: String,
    #[serde(default)]
    move_usi: Option<String>,
    #[serde(default)]
    timed_out: bool,
    #[serde(default)]
    eval: Option<EvalLog>,
}

#[derive(Deserialize)]
struct SweepResult {
    #[serde(default)]
    game_id: Option<u32>,
    outcome: String,
    #[serde(default)]
    plies: u32,
    #[serde(default)]
    error: Option<bool>,
}

/// 盤面に適用された指し手 1 手分の、着手側と自己視点の評価。
struct PlayedMove {
    ply: u32,
    side: Color,
    eval: Option<EvalLog>,
}

/// 1 組 (X, N) の集計。
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct SweepRow {
    pub score: i32,
    pub movecount: u32,
    pub games: u64,
    /// 裁定が出た局の数。
    pub adjudicated: u64,
    /// 裁定の勝者が実際には負けた局 (逆転)。
    pub reversed: u64,
    /// 裁定が出たが実際は引分の局。
    pub drawn: u64,
    /// 全局の手数の合計と、裁定で縮む手数の合計。
    pub plies: u64,
    pub plies_saved: u64,
}

impl SweepRow {
    fn rate(numerator: u64, denominator: u64) -> f64 {
        if denominator == 0 {
            0.0
        } else {
            numerator as f64 / denominator as f64
        }
    }
    /// 裁定した局のうち、実際の結果と食い違う (逆転 + 引分) 割合。
    pub fn mismatch_rate(&self) -> f64 {
        Self::rate(self.reversed + self.drawn, self.adjudicated)
    }
    pub fn reversal_rate(&self) -> f64 {
        Self::rate(self.reversed, self.adjudicated)
    }
    pub fn saved_fraction(&self) -> f64 {
        Self::rate(self.plies_saved, self.plies)
    }
}

/// 全ファイルを 1 局ずつ読みながら集計する。保持するのは結果待ちの局の手だけ。
pub fn sweep(files: &[&str], scores: &[i32], movecounts: &[u32]) -> Result<Vec<SweepRow>> {
    let mut rows: Vec<SweepRow> = scores
        .iter()
        .flat_map(|&score| {
            movecounts.iter().map(move |&movecount| SweepRow {
                score,
                movecount,
                ..SweepRow::default()
            })
        })
        .collect();
    for path in files {
        let file =
            std::fs::File::open(path).with_context(|| format!("ファイルを開けません: {path}"))?;
        let mut pending: HashMap<u32, Vec<PlayedMove>> = HashMap::new();
        for line in BufReader::new(file).lines() {
            let line = line?;
            let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
                continue;
            };
            if value["type"] == "move" {
                let mv: SweepMove = serde_json::from_value(value)
                    .with_context(|| format!("moveパースエラー: {path}"))?;
                let moves = pending.entry(mv.game_id).or_default();
                // 同じ game_id の指し直しは手数が戻るので、前の試行の手を捨てる。
                if moves.last().is_some_and(|last| mv.ply <= last.ply) {
                    moves.clear();
                }
                if is_applied_move(&mv) {
                    moves.push(PlayedMove {
                        ply: mv.ply,
                        side: if mv.side_to_move == "w" {
                            Color::White
                        } else {
                            Color::Black
                        },
                        eval: mv.eval,
                    });
                }
            } else if value["type"] == "result" {
                let result: SweepResult = serde_json::from_value(value)
                    .with_context(|| format!("resultパースエラー: {path}"))?;
                let moves = result.game_id.and_then(|id| pending.remove(&id)).unwrap_or_default();
                let actual = match result.outcome.as_str() {
                    "black_win" => Some(Color::Black),
                    "white_win" => Some(Color::White),
                    "draw" => None,
                    _ => continue,
                };
                if result.error.unwrap_or(false) {
                    continue;
                }
                for row in &mut rows {
                    record_game(row, &moves, actual, result.plies);
                }
            }
        }
    }
    Ok(rows)
}

fn is_applied_move(mv: &SweepMove) -> bool {
    !mv.timed_out
        && mv
            .move_usi
            .as_deref()
            .and_then(Move::from_usi)
            .is_some_and(|m| m.is_normal() || m.is_pass())
}

fn record_game(row: &mut SweepRow, moves: &[PlayedMove], actual: Option<Color>, plies: u32) {
    row.games += 1;
    row.plies += u64::from(plies);
    let mut scores = ScoreAdjudicator::new(None, None, None).with_agreement(Some(ResignRule {
        movecount: row.movecount,
        score: row.score,
    }));
    let Some((ply, verdict)) = moves
        .iter()
        .find_map(|m| scores.after_move(m.side, m.eval.as_ref(), m.ply).map(|v| (m.ply, v)))
    else {
        return;
    };
    row.adjudicated += 1;
    row.plies_saved += u64::from(plies.saturating_sub(ply));
    let winner = match verdict.outcome.label() {
        "black_win" => Color::Black,
        _ => Color::White,
    };
    match actual {
        None => row.drawn += 1,
        Some(actual) if actual != winner => row.reversed += 1,
        Some(_) => {}
    }
}

pub fn print_text(rows: &[SweepRow]) {
    println!("両側の評価の一致による裁定の後付け集計 (X = 閾値 cp、N = 各側の連続手数)");
    println!(
        "{:>6} {:>3} {:>8} {:>10} {:>9} {:>9} {:>8} {:>9}",
        "X", "N", "games", "adjudged", "mismatch", "reversed", "drawn", "saved"
    );
    for row in rows {
        println!(
            "{:>6} {:>3} {:>8} {:>9.1}% {:>8.2}% {:>8.2}% {:>8} {:>8.1}%",
            row.score,
            row.movecount,
            row.games,
            SweepRow::rate(row.adjudicated, row.games) * 100.0,
            row.mismatch_rate() * 100.0,
            row.reversal_rate() * 100.0,
            row.drawn,
            row.saved_fraction() * 100.0
        );
    }
    println!(
        "mismatch = (reversed + drawn) / adjudged、reversed = 裁定の勝者が実際は負けた局、saved = 縮む手数 / 全局の手数"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_log(lines: &[String]) -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("games.jsonl");
        std::fs::write(&path, lines.join("\n")).unwrap();
        let path = path.to_string_lossy().into_owned();
        (dir, path)
    }

    fn mv(game_id: u32, ply: u32, cp: i32) -> String {
        let side = if ply % 2 == 1 { "b" } else { "w" };
        format!(
            r#"{{"type":"move","game_id":{game_id},"ply":{ply},"side_to_move":"{side}","move_usi":"7g7f","elapsed_ms":1,"think_limit_ms":1,"timed_out":false,"engine":"e","eval":{{"score_cp":{cp}}}}}"#
        )
    }

    fn result(game_id: u32, outcome: &str, plies: u32) -> String {
        format!(r#"{{"type":"result","game_id":{game_id},"outcome":"{outcome}","plies":{plies}}}"#)
    }

    #[test]
    fn sweep_counts_reversals_draws_and_saved_plies() {
        // 局 1: 先手 +800 / 後手 -800 が 4 手目で揃い、実際も先手勝ち (10 手)。
        // 局 2: 同じ評価だが実際は後手勝ち (逆転)。局 3: 実際は引分。局 4: 揃わない。
        let mut lines = Vec::new();
        for (game_id, outcome) in [(1, "black_win"), (2, "white_win"), (3, "draw")] {
            for ply in 1..=4 {
                lines.push(mv(game_id, ply, if ply % 2 == 1 { 800 } else { -800 }));
            }
            lines.push(result(game_id, outcome, 10));
        }
        for ply in 1..=4 {
            lines.push(mv(4, ply, if ply % 2 == 1 { 800 } else { -100 }));
        }
        lines.push(result(4, "black_win", 10));
        let (_dir, path) = write_log(&lines);
        let rows = sweep(&[path.as_str()], &[800, 900], &[2]).unwrap();
        assert_eq!(
            rows[0],
            SweepRow {
                score: 800,
                movecount: 2,
                games: 4,
                adjudicated: 3,
                reversed: 1,
                drawn: 1,
                plies: 40,
                plies_saved: 18,
            }
        );
        assert_eq!(rows[1].adjudicated, 0);
        assert!((rows[0].mismatch_rate() - 2.0 / 3.0).abs() < 1e-12);
        assert!((rows[0].saved_fraction() - 18.0 / 40.0).abs() < 1e-12);
    }

    #[test]
    fn sweep_drops_moves_of_a_retried_game_and_skips_error_results() {
        let mut lines = vec![mv(1, 1, 800), mv(1, 2, -800), mv(1, 3, 800)];
        lines.push(
            r#"{"type":"result","game_id":1,"outcome":"draw","plies":3,"error":true}"#.to_string(),
        );
        // 指し直しの局は 1 手目から記録し直される。前の試行の手と連結しない。
        lines.extend([mv(1, 1, 800), mv(1, 2, 0), mv(1, 3, 800), mv(1, 4, -800)]);
        lines.push(result(1, "black_win", 4));
        let (_dir, path) = write_log(&lines);
        let rows = sweep(&[path.as_str()], &[800], &[2]).unwrap();
        assert_eq!(rows[0].games, 1);
        assert_eq!(rows[0].adjudicated, 0);
    }
}
