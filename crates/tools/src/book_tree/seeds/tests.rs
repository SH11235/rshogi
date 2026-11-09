use super::*;
use crate::book_db::{apply_usi_move, position_from_sfen};
use crate::book_tree::reply_graph::HIRATE_KEY;
use serde_json::json;
use tempfile::TempDir;

const PATH: &[&str] = &[
    "7g7f", "3c3d", "2g2f", "8c8d", "2f2e", "8d8e", "6i7h", "4a3b", "5g5f", "5c5d",
];

fn args(dir: &Path) -> SeedsArgs {
    SeedsArgs {
        games: vec![dir.join("games.jsonl")],
        out: dir.join("queue.jsonl"),
        table: dir.join("table.tsv"),
        tag: "test".into(),
        depth: 6,
        opp_n: None,
        stop: None,
        rounds: None,
        draw_black: None,
        draw_white: None,
        offsets: vec![0, 6, 12],
        min_games_offset: 2,
        max_lines: 3,
        collapse_plies: 0,
        collapse_cp: 150,
        priority_early: 2,
        priority_late: 100,
        sweep_loser: false,
        chance_cp: None,
        chance_back: 6,
        opp_freq: None,
        opp_book: None,
        exclude_queue: vec![],
        update_out: None,
        update_sources: None,
    }
}

fn game(
    id: u32,
    initial: &str,
    path: &[&str],
    eval_start: usize,
    outcome: &str,
) -> Vec<serde_json::Value> {
    let mut position = position_from_sfen(initial).unwrap();
    let mut rows = vec![];
    for (i, mv) in path.iter().enumerate() {
        let sfen = position.to_sfen();
        rows.push(json!({"type":"move", "game_id":id,"ply":i+1,"side_to_move":sfen.split_whitespace().nth(1),
            "sfen_before":sfen,"move_usi":mv,"eval":if i >= eval_start {json!({"last_exact_primary":{"score":{"cp":0}}})} else {serde_json::Value::Null}}));
        apply_usi_move(&mut position, mv).unwrap();
    }
    rows.push(json!({"type":"result","game_id":id,"outcome":outcome,"plies":path.len(),
        "pair_index":id/2,"pair_slot":id%2,"attempt":0}));
    rows
}

fn initial() -> String {
    format!("{HIRATE_KEY} 1")
}
fn after(path: &[&str]) -> String {
    let mut p = position_from_sfen(&initial()).unwrap();
    for mv in path {
        apply_usi_move(&mut p, mv).unwrap();
    }
    p.to_sfen()
}
fn write_games(dir: &Path, rows: &[serde_json::Value]) {
    let text = rows.iter().map(|r| format!("{r}\n")).collect::<String>();
    std::fs::write(dir.join("games.jsonl"), text).unwrap();
}
fn lines(dir: &Path) -> Vec<Line> {
    std::fs::read_to_string(dir.join("queue.jsonl"))
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect()
}

#[test]
fn exit_sweep_and_sides() {
    let dir = TempDir::new().unwrap();
    let mut rows = game(0, &initial(), PATH, 2, "black_win");
    rows.extend(game(1, &initial(), PATH, 2, "black_win"));
    write_games(dir.path(), &rows);
    run(args(dir.path())).unwrap();
    assert!(lines(dir.path()).is_empty());
    rows.last_mut().unwrap()["outcome"] = json!("white_win");
    write_games(dir.path(), &rows);
    run(args(dir.path())).unwrap();
    let out = lines(dir.path());
    let key = parse_root(&format!("sfen {}", after(&PATH[..2]))).unwrap().key;
    let root: Vec<_> = out.iter().filter(|l| parse_root(&l.root).unwrap().key == key).collect();
    assert_eq!(root.len(), 2);
    assert_eq!(root[0].own, "b");
    assert_eq!(root[1].own, "w");
    let table = std::fs::read_to_string(dir.path().join("table.tsv")).unwrap();
    assert!(table.lines().any(|l| l.starts_with("b\t3\t100\tlate\t2\t")));
    assert!(table.lines().any(|l| l.starts_with("w\t3\t100\tlate\t1\t")));
    rows.last_mut().unwrap()["outcome"] = json!("draw");
    write_games(dir.path(), &rows);
    run(args(dir.path())).unwrap();
    assert_eq!(lines(dir.path()).iter().filter(|l| l.own == "w").count(), 1);
}

#[test]
fn offset_requires_shared_prefix_and_outputs_are_deterministic() {
    let dir = TempDir::new().unwrap();
    let mut rows = game(0, &initial(), PATH, 0, "draw");
    rows.extend(game(1, &initial(), PATH, 0, "draw"));
    write_games(dir.path(), &rows);
    run(args(dir.path())).unwrap();
    assert_eq!(lines(dir.path()).len(), 4);
    let first = std::fs::read(dir.path().join("queue.jsonl")).unwrap();
    let first_table = std::fs::read(dir.path().join("table.tsv")).unwrap();
    run(args(dir.path())).unwrap();
    assert_eq!(first, std::fs::read(dir.path().join("queue.jsonl")).unwrap());
    assert_eq!(first_table, std::fs::read(dir.path().join("table.tsv")).unwrap());
    let mut changed = PATH.to_vec();
    changed[4] = "6i7h";
    changed[6] = "2f2e";
    rows = game(0, &initial(), PATH, 0, "draw");
    rows.extend(game(1, &initial(), &changed, 0, "draw"));
    write_games(dir.path(), &rows);
    run(args(dir.path())).unwrap();
    assert_eq!(lines(dir.path()).len(), 2);
}

#[test]
fn transpositions_merge_and_lines_follow_counts() {
    let dir = TempDir::new().unwrap();
    let a = format!("{} 101", parse_root(&format!("sfen {}", after(&["7g7f"]))).unwrap().key);
    let b = after(&["2g2f"]);
    let pa = &["3c3d", "2g2f", "8c8d", "2f2e"];
    let pb = &["3c3d", "7g7f", "8c8d", "6i7h"];
    let mut rows = game(0, &a, pa, 0, "draw");
    rows.extend(game(1, &a, pa, 0, "draw"));
    rows.extend(game(2, &b, pb, 0, "draw"));
    rows.extend(game(3, &b, pb, 0, "black_win"));
    write_games(dir.path(), &rows);
    let mut opts = args(dir.path());
    opts.offsets = vec![2];
    opts.min_games_offset = 1;
    run(opts).unwrap();
    let key = parse_root(&format!("sfen {}", after(&PATH[..3]))).unwrap().key;
    let out = lines(dir.path());
    let merged: Vec<_> = out.iter().filter(|l| parse_root(&l.root).unwrap().key == key).collect();
    assert_eq!(merged.len(), 2);
    let white = merged.iter().find(|l| l.own == "w").unwrap();
    assert_eq!(white.game, vec![vec!["8c8d", "2f2e"], vec!["8c8d", "6i7h"]]);
    assert_eq!(parse_root(&white.root).unwrap().ply, 4);
}

#[test]
fn exact_scores_sign_mate_and_terminal_boundary() {
    let dir = TempDir::new().unwrap();
    let mut rows = game(0, &initial(), PATH, 0, "draw");
    rows.extend(game(1, &initial(), PATH, 0, "draw"));
    rows[1]["eval"]["last_exact_primary"]["score"] = json!({"cp":150});
    write_games(dir.path(), &rows);
    let mut opts = args(dir.path());
    opts.collapse_plies = 1;
    opts.offsets = vec![0];
    run(opts).unwrap();
    let out = lines(dir.path());
    assert_eq!(out.iter().find(|l| l.own == "b").unwrap().priority, 2);
    assert_eq!(out.iter().find(|l| l.own == "w").unwrap().priority, 2);
    rows[1]["eval"]["last_exact_primary"]["score"] = json!({"mate":-3});
    write_games(dir.path(), &rows);
    let mut opts = args(dir.path());
    opts.collapse_plies = 1;
    opts.offsets = vec![0];
    run(opts).unwrap();
    assert_eq!(lines(dir.path()).iter().find(|l| l.own == "w").unwrap().priority, 2);
    let result: GameResult =
        serde_json::from_value(json!({"game_id":0,"outcome":"white_win","plies":30})).unwrap();
    let moves: Vec<Move> = game(0, &initial(), PATH, 0, "draw")
        .into_iter()
        .take(PATH.len())
        .map(|v| serde_json::from_value(v).unwrap())
        .collect();
    let mut boundary = args(dir.path());
    boundary.collapse_plies = 30;
    assert!(early(&result, &moves, 0, &boundary).unwrap());
    let result = GameResult {
        plies: Some(31),
        ..result
    };
    assert!(!early(&result, &moves, 0, &boundary).unwrap());
    let result = GameResult {
        plies: None,
        ..result
    };
    assert!(early(&result, &moves, 0, &boundary).unwrap());
}

#[test]
fn failed_duplicate_and_incomplete_attempts_are_excluded() {
    let dir = TempDir::new().unwrap();
    let mut rows = game(0, &initial(), PATH, 0, "white_win");
    rows.last_mut().unwrap()["error"] = json!(true);
    rows.extend(game(1, &initial(), PATH, 0, "draw"));
    for id in [2, 3] {
        let mut retry = game(id, &initial(), PATH, 0, "black_win");
        let r = retry.last_mut().unwrap();
        r["pair_index"] = json!(0);
        r["attempt"] = json!(1);
        rows.extend(retry);
    }
    rows.extend(game(4, &initial(), PATH, 0, "white_win"));
    rows.push(json!({"type":"result","game_id":99,"pair_index":0,"pair_slot":0,"attempt":1,"outcome":"white_win","plies":10}));
    write_games(dir.path(), &rows);
    let valid = valid_game_ids(&dir.path().join("games.jsonl")).unwrap();
    assert_eq!(valid.keys().copied().collect::<Vec<_>>(), vec![2, 3]);
    run(args(dir.path())).unwrap();
    assert!(lines(dir.path()).is_empty());
}

#[test]
fn exclusion_reuses_roots_and_game_positions() {
    let dir = TempDir::new().unwrap();
    let mut rows = game(0, &initial(), PATH, 0, "draw");
    rows.extend(game(1, &initial(), PATH, 0, "draw"));
    write_games(dir.path(), &rows);
    run(args(dir.path())).unwrap();
    let out = lines(dir.path());
    let mut excluded = out
        .iter()
        .find(|l| l.own == "b" && parse_root(&l.root).unwrap().ply == 1)
        .unwrap()
        .clone();
    excluded.own = "bw".into();
    let path = dir.path().join("exclude.jsonl");
    std::fs::write(&path, format_queue(&[excluded]).unwrap()).unwrap();
    let mut opts = args(dir.path());
    opts.exclude_queue.push(path);
    opts.update_out = Some(dir.path().join("updates.jsonl"));
    run(opts).unwrap();
    assert!(std::fs::read_to_string(dir.path().join("updates.jsonl")).unwrap().is_empty());
    let remaining = lines(dir.path());
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].own, "w");
    assert_eq!(parse_root(&remaining[0].root).unwrap().ply, 7);
}

#[test]
fn late_weight_ranking_uses_book_counts_and_membership() {
    let dir = TempDir::new().unwrap();
    let mut rows = game(0, &initial(), PATH, 0, "draw");
    rows.extend(game(1, &initial(), PATH, 0, "draw"));
    write_games(dir.path(), &rows);
    let key = parse_root(&format!("sfen {}", after(&PATH[..6]))).unwrap().key;
    let freq = dir.path().join("freq.db");
    std::fs::write(&freq, format!("#YANEURAOU-DB2016 1.00\nsfen {key} 1\n6i7h none 0 1 20\n"))
        .unwrap();
    let mut opts = args(dir.path());
    opts.opp_freq = Some(freq.clone());
    opts.opp_book = Some(freq);
    run(opts).unwrap();
    let out = lines(dir.path());
    assert_eq!(out[0].priority, 100);
    assert_eq!(parse_root(&out[0].root).unwrap().ply, 7);
    assert_eq!(out[1].priority, 101);
    let table = std::fs::read_to_string(dir.path().join("table.tsv")).unwrap();
    assert!(table.lines().nth(1).unwrap().ends_with("\t21"));
}

#[test]
fn final_offset_and_terminal_actions() {
    let dir = TempDir::new().unwrap();
    let mut rows = game(0, &initial(), &PATH[..6], 0, "draw");
    rows.extend(game(1, &initial(), &PATH[..6], 0, "draw"));
    write_games(dir.path(), &rows);
    run(args(dir.path())).unwrap();
    let out = lines(dir.path());
    assert_eq!(out.len(), 4);
    assert!(
        out.iter()
            .filter(|l| parse_root(&l.root).unwrap().ply == 7)
            .all(|l| l.game.is_empty())
    );
    let mut terminal = game(2, &initial(), PATH, 0, "white_win");
    terminal[0]["timed_out"] = json!(true);
    let mv: Move = serde_json::from_value(terminal.remove(0)).unwrap();
    assert!(!played(&mv));
    assert!(root_at(&[mv], 1).unwrap().is_none());
}

#[test]
fn missing_evals_and_legacy_pair_metadata() {
    let dir = TempDir::new().unwrap();
    let mut rows = game(0, &initial(), PATH, PATH.len(), "draw");
    rows.extend(game(1, &initial(), PATH, PATH.len(), "draw"));
    for row in &mut rows {
        if row["type"] == "result" {
            row.as_object_mut().unwrap().remove("pair_index");
            row.as_object_mut().unwrap().remove("pair_slot");
            row.as_object_mut().unwrap().remove("plies");
        }
    }
    write_games(dir.path(), &rows);
    assert_eq!(valid_game_ids(&dir.path().join("games.jsonl")).unwrap().len(), 2);
    run(args(dir.path())).unwrap();
    assert!(lines(dir.path()).is_empty());
}

#[test]
fn queue_option_and_output_collision_checks() {
    let dir = TempDir::new().unwrap();
    write_games(dir.path(), &[]);
    let mut opts = args(dir.path());
    opts.opp_n = Some(0);
    assert!(run(opts).is_err());
    let mut opts = args(dir.path());
    opts.out = opts.games[0].clone();
    assert!(run(opts).is_err());
    let mut opts = args(dir.path());
    opts.out = opts.table.clone();
    assert!(run(opts).is_err());
    for target in ["games.jsonl", "queue.jsonl", "table.tsv", "exclude.jsonl"] {
        let mut opts = args(dir.path());
        let path = dir.path().join("exclude.jsonl");
        std::fs::write(&path, "").unwrap();
        opts.exclude_queue.push(path);
        opts.update_out = Some(dir.path().join(target));
        assert!(run(opts).is_err());
    }
}

#[test]
fn sweep_selection_spans_files_and_file_order_is_stable() {
    let dir = TempDir::new().unwrap();
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    std::fs::create_dir(&a).unwrap();
    std::fs::create_dir(&b).unwrap();
    for (path, outcome) in [(&a, "black_win"), (&b, "draw")] {
        let mut rows = game(0, &initial(), PATH, 0, outcome);
        rows.extend(game(1, &initial(), PATH, 0, outcome));
        write_games(path, &rows);
    }
    let mut opts = args(dir.path());
    opts.games = vec![b.join("games.jsonl"), a.join("games.jsonl")];
    opts.offsets = vec![0];
    run(opts).unwrap();
    let first = std::fs::read(dir.path().join("queue.jsonl")).unwrap();
    let table = std::fs::read_to_string(dir.path().join("table.tsv")).unwrap();
    assert!(table.lines().any(|line| line.starts_with("b\t1\t100\tlate\t4\t")));
    assert!(table.lines().any(|line| line.starts_with("w\t1\t100\tlate\t2\t")));
    let mut opts = args(dir.path());
    opts.games = vec![
        a.join("games.jsonl"),
        b.join("games.jsonl"),
        a.join("games.jsonl"),
    ];
    opts.offsets = vec![0];
    run(opts).unwrap();
    assert_eq!(first, std::fs::read(dir.path().join("queue.jsonl")).unwrap());
    assert_eq!(table, std::fs::read_to_string(dir.path().join("table.tsv")).unwrap());
    let mut opts = args(dir.path());
    opts.games = vec![a.join("games.jsonl"), b.join("games.jsonl")];
    opts.offsets = vec![0];
    opts.sweep_loser = true;
    run(opts).unwrap();
    assert_eq!(first, std::fs::read(dir.path().join("queue.jsonl")).unwrap());
    assert_eq!(table, std::fs::read_to_string(dir.path().join("table.tsv")).unwrap());
}

#[test]
fn in_progress_game_is_excluded_but_completed_partner_is_used() {
    let dir = TempDir::new().unwrap();
    let mut rows = game(0, &initial(), PATH, 0, "in_progress");
    rows.last_mut().unwrap()["error"] = json!(false);
    rows.extend(game(1, &initial(), PATH, 0, "draw"));
    write_games(dir.path(), &rows);
    let valid = valid_game_ids(&dir.path().join("games.jsonl")).unwrap();
    assert_eq!(valid.keys().copied().collect::<Vec<_>>(), vec![1]);
    run(args(dir.path())).unwrap();
    let out = lines(dir.path());
    assert_eq!(out.len(), 2);
    assert!(out.iter().all(|line| parse_root(&line.root).unwrap().ply == 1));
    let table = std::fs::read_to_string(dir.path().join("table.tsv")).unwrap();
    assert!(table.lines().skip(1).all(|line| line.split('\t').nth(6) == Some("0:1;total=1")));
}

#[test]
fn offset_black_sweep_is_excluded_from_mixed_origin() {
    let dir = TempDir::new().unwrap();
    let mut rows = game(0, &initial(), PATH, 0, "black_win");
    rows.extend(game(1, &initial(), PATH, 0, "black_win"));
    let mut other = PATH.to_vec();
    other[0] = "6g6f";
    rows.extend(game(2, &initial(), &other, 0, "draw"));
    rows.extend(game(3, &initial(), &other, 0, "white_win"));
    write_games(dir.path(), &rows);
    run(args(dir.path())).unwrap();
    let out = lines(dir.path());
    let swept = parse_root(&format!("sfen {}", after(&PATH[..6]))).unwrap().key;
    assert!(!out.iter().any(|line| parse_root(&line.root).unwrap().key == swept));
    for own in ["b", "w"] {
        assert!(
            out.iter()
                .any(|line| line.own == own && parse_root(&line.root).unwrap().ply == 1)
        );
        assert!(
            out.iter()
                .any(|line| line.own == own && parse_root(&line.root).unwrap().ply == 7)
        );
    }
}

#[test]
fn canonical_input_aliases_do_not_inflate_game_counts() {
    let dir = TempDir::new().unwrap();
    let mut rows = game(0, &initial(), PATH, 0, "draw");
    rows.extend(game(1, &initial(), PATH, 0, "draw"));
    write_games(dir.path(), &rows);
    run(args(dir.path())).unwrap();
    let queue = std::fs::read(dir.path().join("queue.jsonl")).unwrap();
    let table = std::fs::read(dir.path().join("table.tsv")).unwrap();
    let mut opts = args(dir.path());
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    opts.games.push(dir.path().join("sub/../games.jsonl"));
    #[cfg(unix)]
    {
        let alias = dir.path().join("alias.jsonl");
        std::os::unix::fs::symlink(&opts.games[0], &alias).unwrap();
        opts.games.push(alias);
    }
    run(opts).unwrap();
    assert_eq!(queue, std::fs::read(dir.path().join("queue.jsonl")).unwrap());
    assert_eq!(table, std::fs::read(dir.path().join("table.tsv")).unwrap());
}

#[test]
fn cyclic_roots_match_original_bytes_with_bounded_samples() {
    let dir = TempDir::new().unwrap();
    let cycle = ["5i6h", "5a6b", "6h5i", "6b5a"].repeat(3);
    let mut rows = vec![
        json!({"type":"result","game_id":100,"outcome":"white_win","pair_index":0,"pair_slot":0,"error":true}),
        json!({"type":"result","game_id":101,"outcome":"draw","pair_index":0,"pair_slot":1}),
    ];
    let mut delayed = vec![];
    for id in [10, 2, 0, 8, 6, 4, 11, 3, 1, 9, 7, 5] {
        let mut entries = game(id, &initial(), &cycle, 0, "draw");
        let mut result = entries.pop().unwrap();
        result["attempt"] = json!(1);
        rows.extend(entries);
        if id % 2 == 0 {
            rows.push(result);
        } else {
            delayed.push(result);
        }
    }
    rows.extend(delayed);
    rows.push(json!({"type":"result","game_id":999,"outcome":"white_win","pair_index":0,"pair_slot":0,"attempt":1}));
    write_games(dir.path(), &rows);
    let mut opts = args(dir.path());
    opts.offsets = vec![0, 4, 8, 12];
    run(opts).unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("queue.jsonl")).unwrap(),
        include_str!("seeds-original-queue.jsonl")
    );
    let original_table = include_str!("seeds-original-table.tsv");
    let expected = original_table
        .lines()
        .map(|row| {
            let mut fields: Vec<_> = row.split('\t').map(str::to_owned).collect();
            if fields[6] != "game_ids" {
                let samples = fields[6].split(',').take(ID_SAMPLES).collect::<Vec<_>>().join(",");
                fields[6] = format!("{samples};total={}", fields[4]);
            }
            fields.join("\t") + "\n"
        })
        .collect::<String>();
    assert_eq!(std::fs::read_to_string(dir.path().join("table.tsv")).unwrap(), expected);
    let mut opts = args(dir.path());
    opts.offsets = vec![12, 8, 4, 0];
    run(opts).unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("queue.jsonl")).unwrap(),
        include_str!("seeds-original-queue.jsonl")
    );
    assert_eq!(std::fs::read_to_string(dir.path().join("table.tsv")).unwrap(), expected);
}

#[test]
fn selected_result_is_used_even_when_an_excluded_result_with_same_id_comes_first() {
    let dir = TempDir::new().unwrap();
    let mut rows = game(0, &initial(), PATH, 0, "black_win");
    rows.last_mut().unwrap()["error"] = json!(true);
    rows.push(json!({"type":"result","game_id":99,"outcome":"draw","pair_index":0,"pair_slot":1}));
    rows.push(json!({"type":"result","game_id":0,"outcome":"white_win","plies":10,"pair_index":0,"pair_slot":0,"attempt":1}));
    rows.extend(game(1, &initial(), PATH, 0, "draw"));
    rows.last_mut().unwrap()["attempt"] = json!(1);
    write_games(dir.path(), &rows);
    // バイト位置は空行と CRLF を含む入力でも結果行の先頭を指す必要がある。
    let path = dir.path().join("games.jsonl");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, format!("\r\n{}", text.replace('\n', "\r\n"))).unwrap();
    let selected = valid_game_ids(&path).unwrap();
    let mut seen = vec![];
    visit_games(&path, &selected, |result, moves| {
        seen.push((result.game_id, result.outcome.clone(), moves.len()));
        Ok(())
    })
    .unwrap();
    assert_eq!(
        seen,
        vec![
            (0, "white_win".into(), PATH.len()),
            (1, "draw".into(), PATH.len())
        ]
    );
    let mut opts = args(dir.path());
    opts.collapse_plies = 30;
    run(opts).unwrap();
    assert!(lines(dir.path()).iter().any(|line| line.own == "b" && line.priority == 2));
}

#[test]
fn early_uses_final_loser_for_both_seed_sides() {
    let dir = TempDir::new().unwrap();
    for (outcome, cp, expected) in [
        ("white_win", 150, false),
        ("white_win", -150, true),
        ("black_win", -150, false),
        ("black_win", 150, true),
        ("draw", 150, true),
        ("draw", -150, true),
        ("draw", 149, false),
    ] {
        let mut rows = game(0, &initial(), PATH, 0, outcome);
        rows[0]["eval"]["last_exact_primary"]["score"] = json!({"cp":cp});
        let result: GameResult = serde_json::from_value(rows.pop().unwrap()).unwrap();
        let moves: Vec<Move> =
            rows.into_iter().map(|r| serde_json::from_value(r).unwrap()).collect();
        assert_eq!(early(&result, &moves, 0, &args(dir.path())).unwrap(), expected);
    }
    for (cp, priority) in [(-150, 2), (150, 100)] {
        let mut rows = game(0, &initial(), PATH, 0, "white_win");
        rows[0]["eval"]["last_exact_primary"]["score"] = json!({"cp":cp});
        rows.extend(game(1, &initial(), PATH, 0, "white_win"));
        write_games(dir.path(), &rows);
        run(args(dir.path())).unwrap();
        let out = lines(dir.path());
        assert_eq!(out.len(), 4);
        assert!(out.iter().all(|l| l.priority == priority));
    }
    for outcome in ["black_win", "white_win", "draw"] {
        let result: GameResult =
            serde_json::from_value(json!({"game_id":0,"outcome":outcome,"plies":1})).unwrap();
        let moves: Vec<Move> = game(0, &initial(), PATH, 0, outcome)
            .into_iter()
            .take(PATH.len())
            .map(|r| serde_json::from_value(r).unwrap())
            .collect();
        let mut opts = args(dir.path());
        opts.collapse_plies = 1;
        assert!(early(&result, &moves, 0, &opts).unwrap());
    }
}

#[test]
fn update_lines_preserve_fields_order_and_compare_depth_prefixes() {
    let dir = TempDir::new().unwrap();
    let mut rows = game(0, &initial(), PATH, 0, "draw");
    let mut other = PATH.to_vec();
    other[0] = "6g6f";
    rows.extend(game(1, &initial(), &other, 0, "draw"));
    rows.extend(game(2, &initial(), &other, 0, "draw"));
    rows.extend(game(3, &initial(), PATH, 0, "draw"));
    write_games(dir.path(), &rows);
    run(args(dir.path())).unwrap();
    let mut existing = lines(dir.path())
        .into_iter()
        .find(|l| l.own == "b" && parse_root(&l.root).unwrap().ply == 1)
        .unwrap();
    existing.id = "existing".into();
    existing.depth = 1;
    existing.root = format!("sfen {HIRATE_KEY} 99");
    existing.opts.opp_n = Some(4);
    existing.opts.draw_black = Some(-70);
    existing.game = vec![PATH.iter().map(|s| s.to_string()).collect()];
    existing.priority = 321;
    existing.hold = true;
    existing.memo = Some("保持するメモ".into());
    let original = existing.clone();
    let old_path = dir.path().join("old-machine.jsonl");
    let path = dir.path().join("machine.jsonl");
    let mut old = existing.clone();
    old.game.clear();
    old.priority = 999;
    std::fs::write(&old_path, format_queue(&[old]).unwrap()).unwrap();
    std::fs::write(&path, format_queue(&[existing]).unwrap()).unwrap();
    let make_opts = || {
        let mut opts = args(dir.path());
        opts.exclude_queue = vec![old_path.clone(), path.clone()];
        opts.update_out = Some(dir.path().join("updates.jsonl"));
        opts.update_sources = Some(dir.path().join("sources.tsv"));
        opts
    };
    let loaded = read_updates(&make_opts().exclude_queue).unwrap();
    assert_eq!(loaded["existing"].source, path);
    assert_eq!(loaded["existing"].line, original);
    run(make_opts()).unwrap();
    let text = std::fs::read_to_string(dir.path().join("updates.jsonl")).unwrap();
    let updated: Line = serde_json::from_str(text.trim()).unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("sources.tsv")).unwrap(),
        format!("id\tsource\tadded\nexisting\t{}\t1\n", path.display())
    );
    assert_eq!(updated.game[0], original.game[0]);
    assert_eq!(updated.game[1], vec!["6g6f", "3c3d"]);
    assert_eq!(updated.game.len(), 2);
    let mut expected = original;
    expected.game = updated.game.clone();
    expected.memo = Some("保持するメモ; seeds: game に 1 本追加".into());
    assert_eq!(updated, expected);
    run(make_opts()).unwrap();
    assert_eq!(text, std::fs::read_to_string(dir.path().join("updates.jsonl")).unwrap());
    std::fs::write(&path, text).unwrap();
    run(make_opts()).unwrap();
    assert!(std::fs::read_to_string(dir.path().join("updates.jsonl")).unwrap().is_empty());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("sources.tsv")).unwrap(),
        "id\tsource\tadded\n"
    );
}

#[test]
fn update_merges_all_games_and_uses_last_fields() {
    let dir = TempDir::new().unwrap();
    let mut rows = game(0, &initial(), PATH, 0, "draw");
    rows.extend(game(1, &initial(), PATH, 0, "draw"));
    rows.extend(game(2, &initial(), &["6g6f", "3c3d"], 0, "draw"));
    rows.extend(game(3, &initial(), &["2g2f", "8c8d"], 0, "draw"));
    write_games(dir.path(), &rows);
    run(args(dir.path())).unwrap();
    let mut first = lines(dir.path())
        .into_iter()
        .find(|l| l.own == "b" && parse_root(&l.root).unwrap().ply == 1)
        .unwrap();
    first.id = "existing".into();
    first.depth = 1;
    first.game = vec![vec!["6g6f".into(), "3c3d".into()]];
    first.hold = false;
    let mut last = first.clone();
    last.game = vec![vec!["2g2f".into(), "8c8d".into()]];
    last.hold = true;
    last.priority = 321;
    last.opts.opp_n = Some(4);
    last.memo = Some("保持するメモ".into());
    let first_path = dir.path().join("first.jsonl");
    let last_path = dir.path().join("last.jsonl");
    std::fs::write(&first_path, format_queue(&[first.clone(), first.clone()]).unwrap()).unwrap();
    let mut repeated = last.clone();
    repeated.game.extend(first.game.clone());
    std::fs::write(&last_path, format_queue(&[repeated]).unwrap()).unwrap();
    let mut opts = args(dir.path());
    opts.exclude_queue = vec![first_path.clone(), last_path.clone()];
    let output = dir.path().join("updates.jsonl");
    opts.update_out = Some(output.clone());
    run(opts).unwrap();
    let text = std::fs::read_to_string(output).unwrap();
    let updated: Line = serde_json::from_str(text.trim()).unwrap();
    let mut expected = last;
    expected.game = vec![
        first.game[0].clone(),
        expected.game[0].clone(),
        vec!["7g7f".into(), "3c3d".into()],
    ];
    expected.memo = Some("保持するメモ; seeds: game に 1 本追加".into());
    assert_eq!(updated, expected);
    let reversed = read_updates(&[last_path, first_path]).unwrap();
    assert!(!reversed["existing"].line.hold);
    assert_eq!(
        reversed["existing"].line.game,
        vec![expected.game[1].clone(), first.game[0].clone()]
    );
}

#[test]
fn update_skips_rows_that_change_the_root_or_side() {
    let dir = TempDir::new().unwrap();
    let base = Line {
        group: None,
        id: "changed".into(),
        root: "startpos".into(),
        roots: vec![],
        own: "b".into(),
        depth: 1,
        priority: 2,
        hold: false,
        game: vec![vec!["7g7f".into(), "3c3d".into()]],
        opts: Opts::default(),
        memo: None,
    };
    let mut other_side = base.clone();
    other_side.own = "w".into();
    other_side.game = vec![vec!["2g2f".into(), "8c8d".into()]];
    other_side.hold = true;
    let mut other_root = base.clone();
    other_root.root = "startpos moves 7g7f".into();
    other_root.game = vec![vec!["3c3d".into(), "2g2f".into()]];
    let mut again = base.clone();
    again.game = vec![vec!["2g2f".into(), "3c3d".into()]];
    again.priority = 5;
    let path = dir.path().join("queue.jsonl");
    std::fs::write(
        &path,
        format_queue(&[base.clone(), other_side, other_root, again.clone()]).unwrap(),
    )
    .unwrap();
    let updates = read_updates(std::slice::from_ref(&path)).unwrap();
    let row = &updates["changed"].line;
    assert_eq!(row.game, vec![base.game[0].clone(), again.game[0].clone()]);
    assert_eq!(row.priority, 5);
    assert!(!row.hold);
}

#[test]
fn update_ignores_a_last_row_that_serve_rejects() {
    let dir = TempDir::new().unwrap();
    let mut rows = game(0, &initial(), PATH, 0, "draw");
    rows.extend(game(1, &initial(), PATH, 0, "draw"));
    write_games(dir.path(), &rows);
    run(args(dir.path())).unwrap();
    let mut first = lines(dir.path())
        .into_iter()
        .find(|l| l.own == "b" && parse_root(&l.root).unwrap().ply == 1)
        .unwrap();
    first.id = "existing".into();
    first.depth = 1;
    first.game = vec![vec!["6g6f".into(), "3c3d".into()]];
    let mut rejected = first.clone();
    rejected.own = "w".into();
    let first_path = dir.path().join("first.jsonl");
    let rejected_path = dir.path().join("rejected.jsonl");
    std::fs::write(&first_path, format_queue(std::slice::from_ref(&first)).unwrap()).unwrap();
    std::fs::write(&rejected_path, format_queue(&[rejected]).unwrap()).unwrap();
    let mut opts = args(dir.path());
    opts.exclude_queue = vec![first_path, rejected_path];
    let output = dir.path().join("updates.jsonl");
    opts.update_out = Some(output.clone());
    run(opts).unwrap();
    let text = std::fs::read_to_string(output).unwrap();
    let updated: Line = serde_json::from_str(text.trim()).unwrap();
    assert_eq!(updated.id, "existing");
    assert_eq!(updated.own, "b");
    assert_eq!(updated.game[0], first.game[0]);
    assert_eq!(updated.game.len(), 2);
}

#[test]
fn additions_follow_counts_then_usi_order() {
    let mut line = Line {
        group: None,
        id: "order".into(),
        root: "startpos".into(),
        roots: vec![],
        own: "b".into(),
        depth: 1,
        priority: 2,
        hold: false,
        game: vec![vec!["7g7f".into()]],
        opts: Opts::default(),
        memo: None,
    };
    let mut data = RootData::default();
    data.lines.insert((1, vec!["2g2f".into()]), 2);
    data.lines.insert((1, vec!["6g6f".into()]), 3);
    data.lines.insert((1, vec!["1g1f".into()]), 2);
    assert_eq!(add_lines(&mut line, &data, 3), 3);
    assert_eq!(line.game, vec![vec!["7g7f"], vec!["6g6f"], vec!["1g1f"], vec!["2g2f"]]);
}

#[test]
fn update_notice_records_source_and_count() {
    assert_eq!(
        update_notice("tree-a", Path::new("machine-a/targets.jsonl"), 2),
        "積み直し id=tree-a: 元ファイル=machine-a/targets.jsonl / 追加 2 本"
    );
}

#[test]
fn update_uses_existing_depth_and_ignores_transit_roots() {
    let dir = TempDir::new().unwrap();
    let mut changed = PATH.to_vec();
    changed.swap(6, 8);
    let mut rows = game(0, &initial(), &changed, 0, "draw");
    rows.extend(game(1, &initial(), &changed, 0, "draw"));
    write_games(dir.path(), &rows);
    run(args(dir.path())).unwrap();
    let mut existing = lines(dir.path())
        .into_iter()
        .find(|l| l.own == "b" && parse_root(&l.root).unwrap().ply == 1)
        .unwrap();
    existing.game = vec![PATH.iter().map(|s| s.to_string()).collect()];
    let path = dir.path().join("machine.jsonl");
    std::fs::write(&path, format_queue(&[existing.clone()]).unwrap()).unwrap();
    let make_opts = || {
        let mut opts = args(dir.path());
        opts.depth = 1;
        opts.exclude_queue = vec![path.clone()];
        opts.update_out = Some(dir.path().join("updates.jsonl"));
        opts
    };
    run(make_opts()).unwrap();
    let text = std::fs::read_to_string(dir.path().join("updates.jsonl")).unwrap();
    let updated: Line = serde_json::from_str(text.trim()).unwrap();
    assert_eq!(updated.game.len(), 2);
    assert_eq!(updated.game[1], changed[..7]);
    assert_eq!(updated.root, existing.root);
    assert_eq!(updated.depth, 6);
    assert!(!lines(dir.path()).iter().any(|l| l.own == "b"));
    existing.depth = 0;
    std::fs::write(&path, format_queue(&[updated, existing]).unwrap()).unwrap();
    assert!(read_updates(std::slice::from_ref(&path)).unwrap().is_empty());
    run(make_opts()).unwrap();
    assert!(std::fs::read_to_string(dir.path().join("updates.jsonl")).unwrap().is_empty());
    assert!(lines(dir.path()).iter().any(|l| l.own == "b"));
}

#[test]
fn sweep_loser_only_adds_white_roots_and_preserves_mixed_rows() {
    let dir = TempDir::new().unwrap();
    let mut rows = game(0, &initial(), PATH, 0, "black_win");
    rows.extend(game(1, &initial(), PATH, 0, "black_win"));
    let mixed = after(&["6g6f", "4c4d"]);
    rows.extend(game(2, &mixed, PATH, 0, "black_win"));
    rows.extend(game(3, &mixed, PATH, 0, "white_win"));
    write_games(dir.path(), &rows);
    run(args(dir.path())).unwrap();
    let original = lines(dir.path());
    assert_eq!(original.len(), 3);
    let original_table = std::fs::read_to_string(dir.path().join("table.tsv")).unwrap();
    let mut opts = args(dir.path());
    opts.sweep_loser = true;
    opts.priority_early = 7;
    run(opts).unwrap();
    let out = lines(dir.path());
    for line in &original {
        assert_eq!(out.iter().find(|row| row.id == line.id).unwrap(), line);
    }
    let added: Vec<_> = out.iter().filter(|row| !original.iter().any(|l| l.id == row.id)).collect();
    assert_eq!(added.len(), 2);
    assert!(added.iter().all(|row| row.own == "w" && row.priority == 7));
    assert!(added.iter().any(|row| parse_root(&row.root).unwrap().ply == 7));
    let table = std::fs::read_to_string(dir.path().join("table.tsv")).unwrap();
    for row in original_table.lines().skip(1) {
        assert!(table.lines().any(|line| line == row));
    }
    assert_eq!(table.lines().filter(|row| row.split('\t').nth(3) == Some("sweep")).count(), 2);
    let first = std::fs::read(dir.path().join("queue.jsonl")).unwrap();
    let mut opts = args(dir.path());
    opts.sweep_loser = true;
    opts.priority_early = 7;
    run(opts).unwrap();
    assert_eq!(first, std::fs::read(dir.path().join("queue.jsonl")).unwrap());
}

#[test]
fn sweep_loser_uses_shared_prefix_threshold_and_existing_updates() {
    let dir = TempDir::new().unwrap();
    let mut other = PATH.to_vec();
    other[0] = "6g6f";
    let mut rows = game(0, &initial(), PATH, 0, "black_win");
    rows.extend(game(1, &initial(), &other, 0, "black_win"));
    write_games(dir.path(), &rows);
    let make_opts = || {
        let mut opts = args(dir.path());
        opts.sweep_loser = true;
        opts
    };
    run(make_opts()).unwrap();
    let out = lines(dir.path());
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].game.len(), 2);
    let mut existing = out[0].clone();
    existing.game.truncate(1);
    existing.priority = 321;
    let path = dir.path().join("existing.jsonl");
    std::fs::write(&path, format_queue(&[existing.clone()]).unwrap()).unwrap();
    let mut opts = make_opts();
    opts.exclude_queue = vec![path];
    let update = dir.path().join("updates.jsonl");
    opts.update_out = Some(update.clone());
    run(opts).unwrap();
    assert!(lines(dir.path()).is_empty());
    let updated: Line =
        serde_json::from_str(std::fs::read_to_string(update).unwrap().trim()).unwrap();
    assert_eq!(updated.id, existing.id);
    assert_eq!(updated.priority, 321);
    assert_eq!(updated.game.len(), 2);
}

#[test]
fn update_sources_requires_update_out_and_rejects_collisions() {
    let dir = TempDir::new().unwrap();
    write_games(dir.path(), &[]);
    let mut opts = args(dir.path());
    opts.update_sources = Some(dir.path().join("sources.tsv"));
    assert!(run(opts).is_err());
    let exclude = dir.path().join("existing.jsonl");
    std::fs::write(&exclude, "").unwrap();
    for target in [
        "games.jsonl",
        "queue.jsonl",
        "table.tsv",
        "existing.jsonl",
        "updates.jsonl",
    ] {
        let mut opts = args(dir.path());
        opts.exclude_queue = vec![exclude.clone()];
        opts.update_out = Some(dir.path().join("updates.jsonl"));
        opts.update_sources = Some(dir.path().join(target));
        assert!(run(opts).is_err());
    }
}

fn set_score(rows: &mut [serde_json::Value], index: usize, score: serde_json::Value) {
    rows[index]["eval"] = json!({"last_exact_primary": {"score": score}});
}

fn table(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("table.tsv")).unwrap()
}

#[test]
fn chance_adds_only_non_winners_first_crossing_without_offset_threshold() {
    for (outcome, own, cp) in [("black_win", "w", -500), ("white_win", "b", 500)] {
        let dir = TempDir::new().unwrap();
        let mut rows = game(0, &initial(), PATH, 1, outcome);
        set_score(&mut rows, 7, json!({"cp": cp}));
        set_score(&mut rows, 8, json!({"cp": cp}));
        set_score(&mut rows, 9, json!({"cp": -cp * 2}));
        rows.extend(game(1, &initial(), PATH, 1, outcome));
        write_games(dir.path(), &rows);
        let mut opts = args(dir.path());
        opts.chance_cp = Some(500);
        opts.min_games_offset = 100;
        opts.priority_early = 7;
        run(opts).unwrap();
        let added: Vec<_> = lines(dir.path())
            .into_iter()
            .filter(|l| parse_root(&l.root).unwrap().ply != 2)
            .collect();
        assert_eq!(added.len(), 1);
        assert_eq!(added[0].own, own);
        assert_eq!(parse_root(&added[0].root).unwrap().ply, 3);
        assert_eq!(added[0].priority, 7);
        assert_eq!(added[0].game[0], PATH[2..9]);
        assert!(
            table(dir.path())
                .lines()
                .any(|l| l.starts_with(&format!("{own}\t3\t7\tchance\t1\t")))
        );
    }
}

#[test]
fn chance_ignores_missing_exact_scores_below_threshold_and_winners() {
    let dir = TempDir::new().unwrap();
    let mut rows = game(0, &initial(), PATH, 0, "black_win");
    set_score(&mut rows, 7, json!({"cp": 500}));
    set_score(&mut rows, 8, json!({"cp": 1000}));
    rows[6]["eval"] = json!({"last_exact_primary": null});
    rows.extend(game(1, &initial(), PATH, 0, "black_win"));
    write_games(dir.path(), &rows);
    let mut opts = args(dir.path());
    opts.chance_cp = Some(501);
    run(opts).unwrap();
    assert!(lines(dir.path()).is_empty());
}

#[test]
fn chance_skips_origin_even_when_revisited_and_does_not_seek_later_crossings() {
    for back in [0, 6, usize::MAX] {
        let dir = TempDir::new().unwrap();
        let path = ["5i5h", "5a5b", "5h5i", "5b5a", "7g7f", "3c3d"];
        let mut rows = game(0, &initial(), &path, 0, "black_win");
        set_score(&mut rows, 4, json!({"cp": -500}));
        set_score(&mut rows, 5, json!({"cp": 500}));
        rows.extend(game(1, &initial(), &path, 0, "black_win"));
        write_games(dir.path(), &rows);
        let mut opts = args(dir.path());
        opts.chance_cp = Some(500);
        opts.chance_back = back;
        run(opts).unwrap();
        assert!(lines(dir.path()).is_empty());
    }
}

#[test]
fn chance_draw_uses_both_sides_and_mate_scores() {
    let dir = TempDir::new().unwrap();
    let mut rows = game(0, &initial(), PATH, 0, "draw");
    set_score(&mut rows, 7, json!({"mate": 3}));
    set_score(&mut rows, 8, json!({"mate": 0}));
    set_score(&mut rows, 9, json!({"mate": -2}));
    rows.extend(game(1, &initial(), PATH, 0, "draw"));
    write_games(dir.path(), &rows);
    let mut opts = args(dir.path());
    opts.chance_cp = Some(i32::MAX);
    opts.chance_back = 0;
    run(opts).unwrap();
    let out = lines(dir.path());
    assert!(out.iter().any(|l| l.own == "w" && parse_root(&l.root).unwrap().ply == 8));
    assert!(out.iter().any(|l| l.own == "b" && parse_root(&l.root).unwrap().ply == 10));
    assert_eq!(
        table(dir.path())
            .lines()
            .filter(|l| l.split('\t').nth(3) == Some("chance"))
            .count(),
        2
    );
    assert_eq!(score_value(&PrimaryScore::MateWin), i64::MAX);
    assert_eq!(score_value(&PrimaryScore::MateLoss), -i64::MAX);
}

#[test]
fn chance_merges_existing_classes_without_double_counting() {
    for class in ["late", "early", "sweep"] {
        let dir = TempDir::new().unwrap();
        let outcome = if class == "sweep" {
            "black_win"
        } else {
            "draw"
        };
        let mut rows = game(0, &initial(), PATH, 0, outcome);
        set_score(&mut rows, 8, json!({"cp": -500}));
        rows.extend(game(1, &initial(), PATH, 0, outcome));
        write_games(dir.path(), &rows);
        let mut opts = args(dir.path());
        opts.chance_cp = Some(500);
        opts.chance_back = 2;
        opts.sweep_loser = class == "sweep";
        opts.collapse_plies = if class == "early" { 30 } else { 0 };
        run(opts).unwrap();
        let expected = if class == "late" { "chance" } else { class };
        assert!(
            table(dir.path())
                .lines()
                .any(|l| l.starts_with(&format!("w\t7\t2\t{expected}\t2\t")))
        );
        assert_eq!(
            lines(dir.path())
                .iter()
                .filter(|l| l.own == "w" && parse_root(&l.root).unwrap().ply == 7)
                .count(),
            1
        );
    }
}

#[test]
fn chance_disabled_preserves_original_output_and_rejects_nonpositive_cp() {
    let dir = TempDir::new().unwrap();
    let mut rows = game(0, &initial(), PATH, 0, "draw");
    set_score(&mut rows, 8, json!({"cp": -500}));
    rows.extend(game(1, &initial(), PATH, 0, "draw"));
    write_games(dir.path(), &rows);
    run(args(dir.path())).unwrap();
    let original = std::fs::read(dir.path().join("queue.jsonl")).unwrap();
    let original_table = table(dir.path());
    let mut opts = args(dir.path());
    opts.chance_back = 0;
    run(opts).unwrap();
    assert_eq!(original, std::fs::read(dir.path().join("queue.jsonl")).unwrap());
    assert_eq!(original_table, table(dir.path()));
    for cp in [0, -1] {
        let mut opts = args(dir.path());
        opts.chance_cp = Some(cp);
        assert!(run(opts).is_err());
    }
}

#[test]
fn chance_shares_weights_exclusion_and_update_depth() {
    let dir = TempDir::new().unwrap();
    let mut rows = game(0, &initial(), PATH, 0, "black_win");
    set_score(&mut rows, 8, json!({"cp": -500}));
    let mut other = PATH.to_vec();
    other[6] = "6g6f";
    let mut second = game(1, &initial(), &other, 0, "black_win");
    set_score(&mut second, 8, json!({"cp": -500}));
    rows.extend(second);
    write_games(dir.path(), &rows);
    let key = parse_root(&format!("sfen {}", after(&PATH[..6]))).unwrap().key;
    let freq = dir.path().join("freq.db");
    std::fs::write(&freq, format!("#YANEURAOU-DB2016 1.00\nsfen {key} 1\n6i7h none 0 1 20\n"))
        .unwrap();
    let make_opts = || {
        let mut opts = args(dir.path());
        opts.chance_cp = Some(500);
        opts.chance_back = 2;
        opts.opp_freq = Some(freq.clone());
        opts.opp_book = Some(freq.clone());
        opts
    };
    run(make_opts()).unwrap();
    let out = lines(dir.path());
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].game.len(), 2);
    assert!(table(dir.path()).lines().nth(1).unwrap().ends_with("\t21"));
    let mut existing = out[0].clone();
    existing.depth = 1;
    existing.priority = 321;
    existing.game = vec![vec!["6i7h".into(), "4a3b".into()]];
    let queue = dir.path().join("existing.jsonl");
    std::fs::write(&queue, format_queue(&[existing.clone()]).unwrap()).unwrap();
    let mut opts = make_opts();
    opts.exclude_queue = vec![queue];
    let update = dir.path().join("update.jsonl");
    opts.update_out = Some(update.clone());
    run(opts).unwrap();
    assert!(lines(dir.path()).is_empty());
    let updated: Line =
        serde_json::from_str(std::fs::read_to_string(update).unwrap().trim()).unwrap();
    assert_eq!(updated.priority, 321);
    assert_eq!(updated.depth, 1);
    assert_eq!(updated.game, vec![existing.game[0].clone(), vec!["6g6f".into(), "4a3b".into()]]);
}
