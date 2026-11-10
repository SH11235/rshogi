//! 強い対局の棋譜が、walk の止まりの局面とその手前の局面に着いたときの勝敗。

use super::{HIRATE_KEY, Row};
use crate::book_db::{apply_usi_move, position_from_sfen, strip_ply};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap, HashSet};
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
    /// 着いた局面から終局までの手数ごとの局数 (止まりの局面だけ)。局数に比例して増えないよう度数で持つ。
    pub after: BTreeMap<usize, u64>,
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
        let total: u64 = self.after.values().sum();
        let mut seen = 0;
        for (&len, &count) in &self.after {
            seen += count;
            if seen > total / 2 {
                return len.to_string();
            }
        }
        String::new()
    }
}

pub(in super::super) struct RecordOptions {
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
    // 行の手順は walk の初期局面から、棋譜は平手から始まる。
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
    let mut seen = HashSet::new();
    scan_games(path, options, |game| {
        if game.first {
            seen.clear();
        }
        let Some(list) = targets.get(game.key) else {
            return;
        };
        for &(i, j) in list {
            if !seen.insert((i, j)) {
                continue;
            }
            let reach = &mut stats[i][j];
            reach.games += 1;
            reach.score += if rows[i].side == "b" {
                game.black_score
            } else {
                1.0 - game.black_score
            };
            match game.result {
                "b" => reach.black_wins += 1,
                "w" => reach.white_wins += 1,
                _ => reach.draws += 1,
            }
            if j == 0 {
                *reach.after.entry(game.len - game.ply).or_default() += 1;
            }
        }
    })?;
    Ok(stats)
}

/// 対局が 1 つの局面に着いたこと。
pub(in super::super) struct GameAt<'a> {
    /// その対局の最初の局面 (初期局面) なら真。
    pub first: bool,
    /// 手数を除いた SFEN。
    pub key: &'a str,
    /// 初期局面から指した手数。
    pub ply: usize,
    /// 対局の手数。
    pub len: usize,
    /// `b`・`w`・`d`。
    pub result: &'a str,
    /// 先手から見た得点 (勝ち 1・引き分け 0.5)。
    pub black_score: f64,
}

/// 両者のレートが `min_rate` 以上で勝敗 (b・w・d) のある対局の、平手から `max_ply` 手までの各局面を順に渡す。
/// 棋譜の途中に指せない手があれば、そこまでの局面だけを渡す。
pub(in super::super) fn scan_games(
    path: &Path,
    options: &RecordOptions,
    mut visit: impl FnMut(&GameAt),
) -> Result<()> {
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
        let black_score = match res {
            "b" => 1.0,
            "w" => 0.0,
            "d" => 0.5,
            _ => continue,
        };
        if !rated {
            continue;
        }
        let limit = game.mv.len().min(options.max_ply);
        let mut pos = position_from_sfen(&format!("{HIRATE_KEY} 1"))?;
        for p in 0..=limit {
            let sfen = pos.to_sfen();
            visit(&GameAt {
                first: p == 0,
                key: strip_ply(&sfen),
                ply: p,
                len: game.mv.len(),
                result: res,
                black_score,
            });
            if p == limit {
                break;
            }
            let mv = game.mv[p].first().and_then(|m| m.as_str()).unwrap_or_default();
            if apply_usi_move(&mut pos, mv).is_err() {
                break;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::Stop;
    use super::*;

    fn row(side: &'static str, moves: &[&str]) -> Row {
        Row {
            side,
            stop: Stop::Out,
            ply: moves.len(),
            weight: 0.1,
            key: String::new(),
            tag: 0,
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
        // 22 手の行。20 手前は 7g7f 3c3d の後の局面で、その順で指した対局 1 だけが着く。
        let shuffle = ["5i5h", "5a5b", "5h5i", "5b5a"].repeat(5);
        let longer: Vec<&str> = ["7g7f", "3c3d"]
            .into_iter()
            .chain(shuffle.iter().copied().take(18))
            .chain(["2g2f", "8c8d"])
            .collect();
        let rows = [row("b", &exit), row("w", &long), row("b", &longer)];
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
        let twenty = &stats[2][2];
        assert_eq!((twenty.games, twenty.score_text()), (1, "1.00".into()));
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
