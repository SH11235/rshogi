//! 負けた局の崩れと、元データ・伝播後の実戦手の値を照合する。

use super::*;

pub(super) mod pair_disk;

use super::super::exits::disk::Record;

#[derive(Args, Debug)]
pub(in crate::book_tree) struct GamePaveArgs {
    #[arg(long)]
    pub(super) master: PathBuf,
    #[arg(long)]
    pub(super) schema: Option<PathBuf>,
    #[arg(long)]
    pub(super) audit_db: PathBuf,
    #[arg(long, required = true)]
    pub(super) games: Vec<PathBuf>,
    #[arg(long)]
    pub(super) exclude_queue: Vec<PathBuf>,
    #[arg(long)]
    pub(super) table: PathBuf,
    #[arg(long, requires_all = ["priority", "tag"])]
    pub(super) enqueue_out: Option<PathBuf>,
    #[arg(long)]
    pub(super) priority: Option<u32>,
    #[arg(long, requires = "enqueue_out")]
    pub(super) tag: Option<String>,
    #[arg(long, default_value_t = 150)]
    pub(super) collapse_cp: i32,
    #[arg(long, default_value_t = 3)]
    pub(super) after_collapse: u32,
    #[arg(long, default_value_t = 3)]
    pub(super) depth: usize,
    #[arg(long)]
    pub(super) extend: Option<usize>,
    #[arg(long, default_value_t = 3)]
    pub(super) opp_n: usize,
    #[arg(long, default_value_t = 300)]
    pub(super) stop: i32,
    #[arg(long, default_value_t = 3)]
    pub(super) rounds: usize,
    #[arg(long, default_value_t = -70, allow_hyphen_values = true)]
    pub(super) draw_black: i32,
    #[arg(long, default_value_t = 70, allow_hyphen_values = true)]
    pub(super) draw_white: i32,
}

impl GamePaveArgs {
    fn opts(&self) -> Opts {
        Opts {
            opp_n: Some(self.opp_n),
            stop: Some(self.stop),
            rounds: Some(self.rounds),
            draw_black: Some(self.draw_black),
            draw_white: Some(self.draw_white),
            ..Opts::default()
        }
    }
}

fn read_audit(path: &Path, wanted: Sorter) -> Result<BufReader<File>> {
    let mut audit = Sorter::new();
    let mut key = None;
    for (n, line) in BufReader::new(File::open(path)?).lines().enumerate() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(sfen) = line.strip_prefix("sfen ") {
            key = Some(parse_root(&format!("sfen {sfen}"))?.key);
        } else if let Some(key) = &key {
            // 未使用局面の手の行は検査対象外なので、照合後まで解析を遅らせる。
            let mv = line.split_whitespace().next().context("audit-db の手がありません")?;
            audit.push(vec![key.clone(), mv.into()], serde_json::to_string(&(n + 1, line))?)?;
        }
    }
    let mut audit = audit.finish()?;
    let mut wanted = wanted.finish()?;
    let mut request = read_record(&mut wanted)?;
    let mut requested_position = None;
    let mut previous: Option<(Vec<String>, i64)> = None;
    let mut values = Sorter::new();
    while let Some(row) = read_record(&mut audit)? {
        while request.as_ref().is_some_and(|r| r.key[..2] < row.key[..]) {
            requested_position = request.as_ref().map(|r| r.key[0].clone());
            request = read_record(&mut wanted)?;
        }
        if requested_position.as_ref() != Some(&row.key[0])
            && request.as_ref().is_none_or(|r| r.key[0] != row.key[0])
        {
            continue;
        }
        let (n, line): (usize, String) = serde_json::from_str(&row.value)?;
        let fields: Vec<_> = line.split_whitespace().collect();
        ensure!(fields.len() == 5, "audit-db の手の行が不正です: {n} 行目");
        let value = fields[2].parse::<i64>().context("audit-db の値が不正です")?;
        if let Some((key, old)) = &previous {
            ensure!(
                *key != row.key || *old == value,
                "audit-db の同じ手の値が不一致です: {} {}",
                row.key[0],
                row.key[1]
            );
        }
        previous = Some((row.key.clone(), value));
        while request.as_ref().is_some_and(|r| r.key[..2] == row.key[..]) {
            let r = request.take().context("照合する実戦手がありません")?;
            requested_position = Some(r.key[0].clone());
            values.push(r.key[2..].to_vec(), value.to_string())?;
            request = read_record(&mut wanted)?;
        }
    }
    values.finish()
}

fn collapse(game: &Game, cp: i32) -> Option<u32> {
    let mut start = None;
    for m in &game.moves {
        if let Some(score) = m.score_black {
            let own = if game.own == "b" { score } else { -score };
            if own <= -i64::from(cp) {
                start.get_or_insert(m.ply);
            } else {
                start = None;
            }
        }
    }
    start
}

fn repetition_entry(game: &Game) -> Result<Option<u32>> {
    let mut seen = BTreeMap::new();
    let mut entry: Option<u32> = None;
    for m in &game.moves {
        let key = crate::book_db::strip_ply(&m.sfen_before);
        if let Some(first) = seen.get(key) {
            entry = Some(entry.map_or(*first, |old| old.min(*first)));
        } else {
            seen.insert(key.to_owned(), m.ply);
        }
    }
    // 最後の着手後の局面も繰り返しを成立させる。
    if let Some(last) = game.moves.last() {
        let root = parse_root(&format!("sfen {}", last.sfen_before))?;
        let key = super::super::model::child(&root.key, &last.move_usi)?;
        if let Some(first) = seen.get(&key) {
            entry = Some(entry.map_or(*first, |old| old.min(*first)));
        }
    }
    Ok(entry)
}

struct PaveReport<'a> {
    args: &'a GamePaveArgs,
    master: MasterIndex,
    audit: BufReader<File>,
    audit_next: Option<Record>,
    excluded: Excluded,
    table: File,
    queue: Sorter,
}

impl PaveReport<'_> {
    fn game(&mut self, game_key: &[String], game: Game, outcome: &str) -> Result<()> {
        let args = self.args;
        let plies = game.moves.last().map_or(0, |m| m.ply);
        let collapse = (outcome != "draw").then(|| collapse(&game, args.collapse_cp)).flatten();
        let repetition = if outcome == "draw" {
            repetition_entry(&game)?
        } else {
            None
        };
        let mut reach = 0;
        let mut prefix = true;
        let mut settled = None;
        let mut missing = Vec::new();
        let mut covered_by = BTreeSet::new();
        let mut active = Vec::<(usize, &Coverage)>::new();
        for (i, m) in game.moves.iter().enumerate() {
            let root = parse_root(&format!("sfen {}", m.sfen_before))?;
            let rows = self.master.rows(&root.key)?;
            let played = rows.iter().find(|r| r.get("move") == m.move_usi);
            if prefix && played.is_some() {
                reach = m.ply;
            } else {
                prefix = false;
            }
            let mut value_key = game_key.to_vec();
            value_key.push(format!("{:010}", m.ply));
            while self.audit_next.as_ref().is_some_and(|r| r.key < value_key) {
                self.audit_next = read_record(&mut self.audit)?;
            }
            let audit_value = self
                .audit_next
                .as_ref()
                .filter(|r| r.key == value_key)
                .map(|r| r.value.parse::<i64>())
                .transpose()?;
            if settled.is_none()
                && m.side_to_move == game.own
                && collapse.is_some_and(|c| m.ply <= c)
                && played.is_some_and(|r| r.get("value_kind").starts_with("tree:"))
                && audit_value.is_some_and(|v| v <= -i64::from(args.collapse_cp))
            {
                settled = Some((
                    m.ply,
                    rows.iter().any(|r| r.get("role") == "main" && r.get("move") == m.move_usi),
                ));
            }
            active.retain(|(start, row)| i - start < row.depth);
            if let Some(rows) = self.excluded.get(&root.key) {
                active.extend(rows.iter().filter(|r| r.depth > 0).map(|r| (i, r)));
            }
            if played.is_none() {
                let ids = covered_ids(&game, i, &active);
                missing.push((i, root, ids));
            }
        }
        let target = collapse.map(|c| {
            let end = c.saturating_add(args.after_collapse);
            if reach < end { end.min(plies) } else { plies }
        });
        let gaps: Vec<_> = missing
            .into_iter()
            .filter(|(i, _, _)| {
                target.is_some_and(|end| game.moves[*i].ply > reach && game.moves[*i].ply <= end)
            })
            .collect();
        for (_, _, ids) in &gaps {
            covered_by.extend(ids.iter().cloned());
        }
        let state = if outcome == "draw" {
            if repetition.is_some() {
                "repetition"
            } else {
                "draw-other"
            }
        } else if collapse.is_none() {
            "no-collapse"
        } else if settled.is_some() {
            "settled"
        } else if reach == plies {
            "disagree"
        } else if gaps.iter().all(|(_, _, ids)| !ids.is_empty()) {
            "queued"
        } else {
            "open"
        };
        let mut generated = Vec::new();
        let mut next = 0u64;
        if state == "open" && args.enqueue_out.is_some() {
            for (i, root, ids) in &gaps {
                let m = &game.moves[*i];
                if !ids.is_empty() || u64::from(m.ply) < next {
                    continue;
                }
                next = u64::from(m.ply) + args.depth.min(8) as u64;
                let hash = format!("{:x}", Sha256::digest(&root.key));
                let line = Line {
                    id: format!(
                        "{}_{}_{}_{}_{}",
                        args.tag.as_deref().context("--tag が必要です")?,
                        short_name(&game.name),
                        m.ply,
                        &hash[..10],
                        game.own
                    ),
                    root: format!("sfen {}", m.sfen_before),
                    roots: vec![],
                    own: game.own.clone(),
                    depth: args.depth,
                    priority: args.priority.context("--priority が必要です")?,
                    hold: false,
                    game: vec![
                        game.moves[*i..].iter().take(8).map(|m| m.move_usi.clone()).collect(),
                    ],
                    opts: args.opts(),
                    group: None,
                    memo: Some(format!("{} の崩れまでの舗装の根 {} 手目", game.name, m.ply)),
                };
                generated.push((root.key.clone(), line));
            }
            if let Some((_, last)) = generated.last_mut() {
                last.opts.extend = args.extend;
            }
        }
        let count = generated.len();
        for (key, line) in generated {
            line.check()?;
            self.queue.push(
                vec![key, game.own.clone(), serde_json::to_string(&line.game[0])?],
                serde_json::to_string(&line)?,
            )?;
        }
        let num = |n: Option<u32>| n.map_or_else(String::new, |n| n.to_string());
        writeln!(
            self.table,
            "{}\t{outcome}\t{}\t{plies}\t{}\t{reach}\t{}\t{}\t{}\t{state}\t{count}\t{}\t{}",
            game.name,
            game.own,
            num(collapse),
            num(target),
            num(settled.map(|s| s.0)),
            settled.map_or_else(String::new, |s| s.1.to_string()),
            covered_by.into_iter().collect::<Vec<_>>().join(","),
            num(repetition)
        )?;
        Ok(())
    }
}

pub(in crate::book_tree) fn run(mut args: GamePaveArgs) -> Result<()> {
    ensure!(args.collapse_cp > 0, "collapse-cp は正の値が必要です");
    ensure!(args.depth > 0 && args.depth <= MAX_DEPTH, "depth は 1〜{MAX_DEPTH} が必要です");
    let mut opts = args.opts();
    opts.extend = args.extend;
    opts.check()?;
    ensure!(args.priority.is_none_or(|p| p <= MAX_PRIORITY), "priority が大きすぎます");
    if args.enqueue_out.is_some() {
        check_tag(args.tag.as_deref().context("--enqueue-out には --tag が必要です")?)?;
        ensure!(args.priority.is_some(), "--enqueue-out には --priority が必要です");
    }
    args.games = args.games.iter().map(|p| p.canonicalize()).collect::<std::io::Result<_>>()?;
    args.games.sort();
    args.games.dedup();
    let schema_paths = args.schema.as_deref().map(schema::paths).unwrap_or_default();
    let mut inputs = vec![
        ("--master", args.master.as_path()),
        ("--audit-db", args.audit_db.as_path()),
    ];
    inputs.extend(args.games.iter().map(|p| ("--games", p.as_path())));
    inputs.extend(args.exclude_queue.iter().map(|p| ("--exclude-queue", p.as_path())));
    if args.schema.is_some() {
        inputs.extend(schema_paths.iter().map(|p| ("--schema", p.as_path())));
    }
    reject_same_file(&args.table, &inputs)?;
    inputs.push(("--table", &args.table));
    if let Some(path) = &args.enqueue_out {
        reject_same_file(path, &inputs)?;
    }
    let mut wanted = Sorter::new();
    let mut games = Sorter::new();
    for (index, path) in args.games.iter().enumerate() {
        let name = path
            .file_stem()
            .context("対局名がありません")?
            .to_string_lossy()
            .replace(['\t', '\r', '\n'], "_");
        let first = BufReader::new(File::open(path)?)
            .lines()
            .find_map(|line| match line {
                Ok(s) if s.trim().is_empty() => None,
                line => Some(line),
            })
            .transpose()?
            .context("棋譜が空です")?;
        ensure!(
            first.trim_start().starts_with('{'),
            "game-pave は pair JSON Lines 専用です (CSA は未対応)"
        );
        let mut results = 0;
        let mut pair = false;
        let mut client = false;
        let mut labels = false;
        let mut numeric_ids = true;
        seeds::visit_json(path, |_, v| {
            if v["type"] == "meta" {
                client |= !v["game_id"].is_null() || v["source"] == "csa_client";
                labels |= !v["engine_cmd"].is_null();
            }
            if v["type"] == "result" {
                results += 1;
                pair |= !v["pair_index"].is_null() || !v["pair_slot"].is_null();
                numeric_ids &= v["game_id"].is_u64();
            }
            Ok(())
        })?;
        ensure!(results > 0, "result がありません");
        ensure!(
            !client && (pair || results > 1 || !labels),
            "game-pave は pair JSON Lines 専用です (csa_client は未対応)"
        );
        ensure!(numeric_ids, "pair の数値 game_id がありません");
        pair_disk::visit(path, &name, |mut game, outcome| {
            validate_game(&mut game)?;
            let id: u32 = game.name.rsplit('_').next().context("game_id がありません")?.parse()?;
            let game_key = vec![format!("{index:020}"), format!("{id:010}")];
            for m in &game.moves {
                if m.side_to_move == game.own {
                    wanted.push(
                        vec![
                            parse_root(&format!("sfen {}", m.sfen_before))?.key,
                            m.move_usi.clone(),
                            game_key[0].clone(),
                            game_key[1].clone(),
                            format!("{:010}", m.ply),
                        ],
                        String::new(),
                    )?;
                }
            }
            games.push(game_key, serde_json::to_string(&(game, outcome))?)
        })
        .with_context(|| format!("棋譜: {}", path.display()))?;
    }
    let mut report = PaveReport {
        master: MasterIndex::open(&args.master, &Schema::load(args.schema.as_deref())?)?,
        audit: read_audit(&args.audit_db, wanted)?,
        audit_next: None,
        excluded: read_excluded(&args.exclude_queue)?,
        table: tempfile::tempfile()?,
        queue: Sorter::new(),
        args: &args,
    };
    writeln!(
        report.table,
        "game\toutcome\town\tplies\tcollapse\tbook_reach\ttarget_end\tsettled_ply\tsettled_main_same\tstate\trows\tcovered_by\trepetition_entry"
    )?;
    report.audit_next = read_record(&mut report.audit)?;
    let mut games = games.finish()?;
    while let Some(row) = read_record(&mut games)? {
        let (game, outcome): (Game, String) = serde_json::from_str(&row.value)?;
        report.game(&row.key, game, &outcome)?;
    }
    let mut queue = merge_queue(report.queue)?;
    copy_output(&mut report.table, &args.table)?;
    if let Some(path) = &args.enqueue_out {
        copy_output(&mut queue, path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_join_spills_and_restores_game_order_with_repeated_positions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.db");
        let a = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b -";
        let b = crate::book_tree::model::child(a, "7g7f").unwrap();
        let mut audit = File::create(&path).unwrap();
        let mut wanted = Sorter::new();
        for i in (0..4100).rev() {
            let (key, mv, value) = if i % 2 == 0 {
                (a, "7g7f", -150)
            } else {
                (b.as_str(), "3c3d", -200)
            };
            writeln!(audit, "sfen {key} {}\n{mv} none {value} 3 1", i + 1).unwrap();
            wanted
                .push(
                    vec![
                        key.into(),
                        mv.into(),
                        format!("{i:020}"),
                        "0000000001".into(),
                    ],
                    String::new(),
                )
                .unwrap();
        }
        drop(audit);
        let mut joined = read_audit(&path, wanted).unwrap();
        for i in 0..4100 {
            let row = read_record(&mut joined).unwrap().unwrap();
            assert_eq!(row.key, [format!("{i:020}"), "0000000001".into()]);
            assert_eq!(row.value, if i % 2 == 0 { "-150" } else { "-200" });
        }
        assert!(read_record(&mut joined).unwrap().is_none());
    }

    #[test]
    fn audit_join_checks_unplayed_moves_after_last_request_and_skips_unused_positions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.db");
        let root = parse_root("startpos").unwrap();
        for (first, second, expected) in [(10, 10, true), (10, 20, false)] {
            std::fs::write(&path, format!(
                "sfen {} 10\n2g2f none -150 3 1\n7g7f none {first} 3 1\nsfen {} 2\n7g7f none {second} 3 1\n",
                root.key, root.key
            )).unwrap();
            let mut wanted = Sorter::new();
            wanted
                .push(vec![root.key.clone(), "2g2f".into(), "0".into(), "1".into()], String::new())
                .unwrap();
            assert_eq!(read_audit(&path, wanted).is_ok(), expected);
        }
        assert!(read_audit(&path, Sorter::new()).is_ok());
    }
}
