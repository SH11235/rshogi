use super::*;

fn endpoint(game: &Game, own: &str, winner: bool, args: &GameGapsArgs) -> (u32, &'static str) {
    let last = game.moves.last().map_or(0, |m| m.ply);
    let mut sustained = None;
    for m in &game.moves {
        if let Some(score) = m.score_black {
            let own_score = if own == "b" { score } else { -score };
            let reached = if winner {
                own_score >= i64::from(args.stop)
            } else {
                own_score <= -i64::from(args.collapse_cp)
            };
            if reached {
                sustained.get_or_insert(m.ply);
            } else {
                sustained = None;
            }
        }
    }
    let (mut end, mut reason) = match sustained {
        Some(ply) if winner => (ply, "stop"),
        Some(ply) => (ply.saturating_add(args.after_collapse).min(last), "collapse"),
        None => (last, "game-end"),
    };
    if let Some(max) = args.max_ply
        && max < end
    {
        end = max;
        reason = "max-ply";
    }
    (end, reason)
}

fn identity(game: &Game, path: &Path) -> String {
    let mut hash = Sha256::new();
    hash.update(path.file_name().unwrap_or_default().as_encoded_bytes());
    hash.update([0]);
    hash.update(&game.name);
    for m in &game.moves {
        hash.update([0]);
        hash.update(&m.sfen_before);
        hash.update([0]);
        hash.update(&m.move_usi);
    }
    let hash = format!("{:x}", hash.finalize());
    format!("{}_{}", short_name(&game.name), &hash[..10])
}

impl Report<'_> {
    pub(super) fn line_game(&mut self, game: &Game, path: &Path) -> Result<()> {
        let args = self.args;
        let winner = match game.outcome.as_str() {
            "black_win" => Some("b"),
            "white_win" => Some("w"),
            _ => None,
        };
        let id = identity(game, path);
        let mut active = Vec::new();
        let mut coverage = Vec::with_capacity(game.moves.len());
        for (i, m) in game.moves.iter().enumerate() {
            let root = parse_root(&format!("sfen {}", m.sfen_before))?;
            active.retain(|(start, row): &(usize, &Coverage)| i - start < row.depth);
            if let Some(rows) = self.excluded.get(&root.key) {
                active.extend(rows.iter().filter(|r| r.depth > 0).map(|r| (i, r)));
            }
            coverage.push(["b", "w"].map(|own| {
                let matching: Vec<_> =
                    active.iter().copied().filter(|(_, row)| row.own == own).collect();
                covered_ids(game, i, &matching)
            }));
        }
        let ends = [
            endpoint(game, "b", winner == Some("b"), args),
            endpoint(game, "w", winner == Some("w"), args),
        ];
        let first = game
            .moves
            .iter()
            .position(|m| m.ply > args.skip_plies)
            .unwrap_or(game.moves.len());
        for i in (first..game.moves.len()).step_by(args.depth.min(8)) {
            let m = &game.moves[i];
            for (side, own) in ["b", "w"].into_iter().enumerate() {
                let is_winner = winner == Some(own);
                if (args.line_sides == LineSides::Winner && !is_winner)
                    || (args.line_sides == LineSides::Loser && is_winner)
                    || m.ply > ends[side].0
                {
                    continue;
                }
                let end = (i + args.depth).min(game.moves.len());
                let game_moves: Vec<_> =
                    game.moves[i..end].iter().map(|m| m.move_usi.clone()).collect();
                let covered = if coverage[i..end].iter().all(|ids| !ids[side].is_empty()) {
                    coverage[i..end]
                        .iter()
                        .flat_map(|ids| &ids[side])
                        .cloned()
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .collect::<Vec<_>>()
                        .join(",")
                } else {
                    String::new()
                };
                let root = parse_root(&format!("sfen {}", m.sfen_before))?;
                writeln!(
                    self.table,
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    game.name,
                    m.ply,
                    m.side_to_move,
                    own,
                    ends[side].1,
                    game_moves.join(" "),
                    covered,
                    root.key
                )?;
                if args.enqueue_out.is_none() || !covered.is_empty() {
                    continue;
                }
                let hash =
                    format!("{:x}", Sha256::digest(serde_json::to_vec(&(&root.key, &game_moves))?));
                let line = Line {
                    id: format!(
                        "{}_{}_{}_{}_{}",
                        args.tag.as_deref().context("--tag が必要です")?,
                        id,
                        m.ply,
                        &hash[..10],
                        own
                    ),
                    root: format!("sfen {}", m.sfen_before),
                    roots: vec![],
                    own: own.into(),
                    depth: args.depth,
                    priority: args.priority.context("--priority が必要です")?,
                    hold: false,
                    game: vec![game_moves],
                    opts: args.opts(),
                    group: None,
                    memo: Some(format!("{} の実戦手順の根 {} 手目", game.name, m.ply)),
                };
                line.check()?;
                self.queue.push(
                    vec![
                        root.key,
                        line.own.clone(),
                        serde_json::to_string(&line.game[0])?,
                    ],
                    serde_json::to_string(&line)?,
                )?;
            }
        }
        Ok(())
    }
}
