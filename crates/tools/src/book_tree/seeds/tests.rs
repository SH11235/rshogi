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
        collapse_plies: 30,
        collapse_cp: 150,
        priority_early: 2,
        priority_late: 100,
        opp_freq: None,
        opp_book: None,
        exclude_queue: vec![],
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
    assert!(table.lines().any(|l| l.starts_with("b\t3\t2\tearly\t2\t")));
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
    assert_eq!(out.iter().find(|l| l.own == "w").unwrap().priority, 100);
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
    assert!(early(&result, &moves, 0, "b", &args(dir.path())).unwrap());
    let result = GameResult {
        plies: Some(31),
        ..result
    };
    assert!(!early(&result, &moves, 0, "b", &args(dir.path())).unwrap());
    let result = GameResult {
        plies: None,
        ..result
    };
    assert!(early(&result, &moves, 0, "b", &args(dir.path())).unwrap());
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
    let valid = valid_results(&dir.path().join("games.jsonl")).unwrap();
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
    run(opts).unwrap();
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
    assert_eq!(valid_results(&dir.path().join("games.jsonl")).unwrap().len(), 2);
    run(args(dir.path())).unwrap();
    assert!(lines(dir.path()).is_empty());
}

#[test]
fn queue_option_and_output_collision_checks() {
    let dir = TempDir::new().unwrap();
    let mut opts = args(dir.path());
    opts.opp_n = Some(0);
    assert!(run(opts).is_err());
    let mut opts = args(dir.path());
    opts.out = opts.games[0].clone();
    assert!(run(opts).is_err());
    let mut opts = args(dir.path());
    opts.out = opts.table.clone();
    assert!(run(opts).is_err());
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
}
