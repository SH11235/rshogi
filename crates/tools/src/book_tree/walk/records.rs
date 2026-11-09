//! 強い対局の棋譜が、walk の止まりの局面とその手前の局面に着いたときの勝敗。

use super::Row;
use crate::book_db::{apply_usi_move, position_from_sfen, strip_ply};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::path::Path;

/// 止まりの局面から何手前の局面で数えるか。
pub(super) const OFFSETS: [usize; 3] = [0, 10, 20];

#[derive(Deserialize)]
struct Game {
    br: Option<f64>,
    wr: Option<f64>,
    res: Option<String>,
    /// 手ごとの配列。先頭が USI の手。
    mv: Vec<Vec<serde_json::Value>>,
}

/// 1 つの局面に着いた対局の集計。
#[derive(Default, Clone)]
pub(super) struct Reach {
    pub games: u64,
    /// 行の側から見た得点 (勝ち 1・引き分け 0.5) の合計。
    pub score: f64,
    pub black_wins: u64,
    pub white_wins: u64,
    pub draws: u64,
    /// 着いた局面から終局までの手数 (止まりの局面だけ)。
    pub after: Vec<usize>,
}
impl Reach {
    pub fn score_text(&self) -> String {
        if self.games == 0 {
            String::new()
        } else {
            format!("{:.2}", self.score / self.games as f64)
        }
    }
    /// 上側の中央値 (要素数が偶数なら大きい方)。
    pub fn median_after(&self) -> String {
        let mut after = self.after.clone();
        after.sort_unstable();
        after.get(after.len() / 2).map(|v| v.to_string()).unwrap_or_default()
    }
}

pub(super) struct RecordOptions {
    pub min_rate: f64,
    pub max_ply: usize,
}

/// 行ごとに `OFFSETS` の各局面の集計を返す。両者のレートが `min_rate` 以上で勝敗 (b・w・d) のある対局だけを、
/// 初期局面から `max_ply` 手まで局面で数える。1 局で同じ行・同じ手前の局面は 1 回だけ数える。
pub(super) fn reach_stats(
    rows: &[Row],
    initial: &str,
    path: &Path,
    options: &RecordOptions,
) -> Result<Vec<[Reach; 3]>> {
    let mut targets: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
    for (i, row) in rows.iter().enumerate() {
        let mut keys = vec![initial.to_owned()];
        for mv in &row.moves {
            keys.push(super::advance(keys.last().expect("初期局面がある"), mv)?);
        }
        for (j, off) in OFFSETS.iter().enumerate() {
            if let Some(key) = keys.len().checked_sub(off + 1).map(|k| &keys[k]) {
                targets.entry(key.clone()).or_default().push((i, j));
            }
        }
    }
    let mut stats = vec![<[Reach; 3]>::default(); rows.len()];
    let file =
        std::fs::File::open(path).with_context(|| format!("開けません: {}", path.display()))?;
    let reader: Box<dyn BufRead> = if path.extension().is_some_and(|e| e == "gz") {
        Box::new(BufReader::new(flate2::read::MultiGzDecoder::new(file)))
    } else {
        Box::new(BufReader::new(file))
    };
    for (n, line) in reader.lines().enumerate() {
        let line = line.with_context(|| format!("{}:{}", path.display(), n + 1))?;
        if line.trim().is_empty() {
            continue;
        }
        let game: Game = serde_json::from_str(&line)
            .with_context(|| format!("{}:{}: 対局の行が不正です", path.display(), n + 1))?;
        let rated =
            matches!((game.br, game.wr), (Some(b), Some(w)) if b.min(w) >= options.min_rate);
        let res = game.res.as_deref().unwrap_or_default();
        let result = match res {
            "b" => 1.0,
            "w" => 0.0,
            "d" => 0.5,
            _ => continue,
        };
        if !rated {
            continue;
        }
        let limit = game.mv.len().min(options.max_ply);
        let mut pos = position_from_sfen(&format!("{initial} 1"))?;
        let mut seen = HashSet::new();
        for p in 0..=limit {
            let sfen = pos.to_sfen();
            if let Some(list) = targets.get(strip_ply(&sfen)) {
                for &(i, j) in list {
                    if !seen.insert((i, j)) {
                        continue;
                    }
                    let reach = &mut stats[i][j];
                    reach.games += 1;
                    reach.score += if rows[i].side == "b" {
                        result
                    } else {
                        1.0 - result
                    };
                    match res {
                        "b" => reach.black_wins += 1,
                        "w" => reach.white_wins += 1,
                        _ => reach.draws += 1,
                    }
                    if j == 0 {
                        reach.after.push(game.mv.len() - p);
                    }
                }
            }
            if p == limit {
                break;
            }
            // 棋譜の途中に指せない手があれば、そこまでの局面だけを数える。
            let mv = game.mv[p].first().and_then(|m| m.as_str()).unwrap_or_default();
            if apply_usi_move(&mut pos, mv).is_err() {
                break;
            }
        }
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::super::{HIRATE_KEY, Stop};
    use super::*;

    fn row(side: &'static str, moves: &[&str]) -> Row {
        Row {
            side,
            stop: Stop::Out,
            ply: moves.len(),
            weight: 0.1,
            key: String::new(),
            moves: moves.iter().map(|m| (*m).to_owned()).collect(),
            games: None,
            book_moves: None,
            cont: Vec::new(),
            cont_end_black: None,
        }
    }

    fn game(br: Option<f64>, res: &str, moves: &[&str]) -> String {
        let mv: Vec<_> = moves.iter().map(|m| serde_json::json!([m, 0, 0])).collect();
        serde_json::json!({"br": br, "wr": 4000.0, "res": res, "mv": mv}).to_string()
    }

    #[test]
    fn counts_rated_results_by_position_once_per_game() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("games.jsonl");
        let exit = ["7g7f", "3c3d", "2g2f", "8c8d"];
        let lines = [
            game(Some(3900.0), "b", &["7g7f", "3c3d", "2g2f", "8c8d", "2f2e", "8d8e"]),
            // 手順違いで同じ局面に着く。
            game(Some(3900.0), "w", &["2g2f", "8c8d", "7g7f", "3c3d", "6i7h"]),
            game(Some(3700.0), "b", &exit),
            game(None, "b", &exit),
            serde_json::json!({"br": 3900.0, "wr": 3900.0, "res": null, "mv": []}).to_string(),
            game(Some(3900.0), "x", &exit),
            // 最初の手が指せないので、初期局面だけを数える。
            game(Some(3900.0), "d", &["5e5d", "3c3d"]),
        ];
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        let long = [
            "7g7f", "3c3d", "2g2f", "8c8d", "2f2e", "8d8e", "6i7h", "4a3b", "2e2d", "2c2d",
        ];
        let rows = [row("b", &exit), row("w", &long)];
        let options = RecordOptions {
            min_rate: 3800.0,
            max_ply: 130,
        };
        let stats = reach_stats(&rows, HIRATE_KEY, &path, &options).unwrap();
        let at = &stats[0][0];
        assert_eq!((at.games, at.black_wins, at.white_wins, at.draws), (2, 1, 1, 0));
        assert_eq!((at.score_text(), at.median_after()), ("0.50".into(), "2".into()));
        assert_eq!((stats[0][1].games, stats[0][2].games), (0, 0));
        // 10 手前は初期局面なので、勝敗のある 3 局がすべて着く。得点は後手から見る。
        let ten = &stats[1][1];
        assert_eq!((ten.games, ten.score_text(), ten.draws), (3, "0.50".into(), 1));
        assert!(ten.after.is_empty());
        let options = RecordOptions {
            min_rate: 3800.0,
            max_ply: 3,
        };
        let stats = reach_stats(&rows, HIRATE_KEY, &path, &options).unwrap();
        assert_eq!(stats[0][0].games, 0);
    }

    #[test]
    fn reads_gzip_and_rejects_broken_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("games.jsonl.gz");
        let mut encoder = flate2::write::GzEncoder::new(
            std::fs::File::create(&path).unwrap(),
            Default::default(),
        );
        std::io::Write::write_all(
            &mut encoder,
            (game(Some(3900.0), "b", &["7g7f"]) + "\n").as_bytes(),
        )
        .unwrap();
        encoder.finish().unwrap();
        let rows = [row("b", &["7g7f"])];
        let options = RecordOptions {
            min_rate: 3800.0,
            max_ply: 130,
        };
        assert_eq!(reach_stats(&rows, HIRATE_KEY, &path, &options).unwrap()[0][0].games, 1);
        let broken = dir.path().join("broken.jsonl");
        std::fs::write(&broken, "{\"br\":1}\n").unwrap();
        assert!(reach_stats(&rows, HIRATE_KEY, &broken, &options).is_err());
    }
}
