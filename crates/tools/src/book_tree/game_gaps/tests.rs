use super::super::master_table::{COLUMNS, Row, format_master};
use super::super::reply_graph::HIRATE_KEY;
use super::*;
use clap::Parser;
use serde_json::{Value, json};

const MOVES: &[&str] = &[
    "7g7f", "3c3d", "2g2f", "8c8d", "2f2e", "8d8e", "6i7h", "4a3b", "5g5f", "5c5d",
];

fn args(dir: &Path) -> GameGapsArgs {
    GameGapsArgs {
        master: dir.join("moves.tsv"),
        schema: None,
        games: vec![dir.join("games.jsonl")],
        player: "mine".into(),
        until: Until::All,
        max_ply: None,
        skip_plies: 0,
        exit_secs: 2,
        exclude_queue: vec![],
        table: dir.join("table.tsv"),
        enqueue_out: Some(dir.join("queue.jsonl")),
        tag: Some("test".into()),
        priority: Some(20),
        depth: 3,
        opp_n: 3,
        stop: 300,
        rounds: 3,
        draw_black: -70,
        draw_white: 70,
    }
}

fn moves() -> Vec<Value> {
    moves_for(MOVES)
}

fn moves_for(path: &[&str]) -> Vec<Value> {
    let mut pos = position_from_sfen(&format!("{HIRATE_KEY} 1")).unwrap();
    path.iter().enumerate().map(|(i, m)| {
        let sfen = pos.to_sfen();
        let side = sfen.split_whitespace().nth(1).unwrap();
        let row = json!({"type":"move", "game_id":0, "ply":i+1, "side_to_move":side,
            "sfen_before":sfen, "move_usi":m, "engine":if side == "b" {"mine"} else {"other"}, "eval":null});
        apply_usi_move(&mut pos, m).unwrap();
        row
    }).collect()
}

fn write_json(path: &Path, rows: &[Value]) {
    std::fs::write(path, rows.iter().map(|r| format!("{r}\n")).collect::<String>()).unwrap();
}

fn client(path: &Path, moves: &[Value]) {
    let mut rows =
        vec![json!({"type":"meta","engine_cmd":{"label_black":"mine","label_white":"other"}})];
    rows.extend_from_slice(moves);
    rows.push(json!({"type":"result"}));
    write_json(path, &rows);
}

fn master(path: &Path, ms: &[Value], missing: &[usize], wrong: &[usize]) {
    let rows: Vec<_> = ms
        .iter()
        .enumerate()
        .filter(|(i, _)| !missing.contains(i))
        .map(|(i, m)| {
            let root = parse_root(&format!("sfen {}", m["sfen_before"].as_str().unwrap())).unwrap();
            Row::from_fields(&[
                ("key", root.key),
                ("ply", root.ply.to_string()),
                (
                    "move",
                    if wrong.contains(&i) {
                        "5i5h".into()
                    } else {
                        m["move_usi"].as_str().unwrap().into()
                    },
                ),
                ("role", "record".into()),
            ])
        })
        .collect();
    let text = format_master(&rows);
    assert!(text.starts_with(&COLUMNS.join("\t")));
    std::fs::write(path, text).unwrap();
}

fn queue(path: &Path) -> Vec<Line> {
    let mut lines: Vec<Line> = std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    lines.sort_by_key(|line| parse_root(&line.root).unwrap().ply);
    lines
}

#[test]
fn classifies_all_roles_and_covers_depth_endpoint_without_extra_root() {
    let dir = tempfile::tempdir().unwrap();
    let a = args(dir.path());
    let ms = moves();
    client(&a.games[0], &ms);
    master(&a.master, &ms, &[1, 2, 3, 4, 5], &[0]);
    run(a).unwrap();
    let text = std::fs::read_to_string(dir.path().join("table.tsv")).unwrap();
    assert_eq!(text.lines().count(), 7);
    assert!(text.contains("games\t1\tb\t7g7f\tno-move\t\t"));
    assert!(text.contains("games\t2\tw\t3c3d\tno-position\t\t"));
    let q = queue(&dir.path().join("queue.jsonl"));
    assert_eq!(q.len(), 2);
    assert!(q[0].id.contains("_1_"));
    assert!(q[1].id.contains("_4_"));
    assert_eq!(q[0].game[0].len(), 8);
    assert_eq!(q[0].depth, 3);
    assert_eq!(q[0].opts.stop, Some(300));
    for line in q {
        line.check().unwrap();
    }
}

#[test]
fn next_gap_after_endpoint_gets_its_own_root() {
    let dir = tempfile::tempdir().unwrap();
    let a = args(dir.path());
    let ms = moves();
    client(&a.games[0], &ms);
    master(&a.master, &ms, &[0, 1, 2, 3], &[]);
    run(a).unwrap();
    let q = queue(&dir.path().join("queue.jsonl"));
    assert_eq!(q.len(), 2);
    assert!(q[0].id.contains("_1_"));
    assert!(q[1].id.contains("_4_"));
}

#[test]
fn excluded_root_ignores_sfen_ply_and_covers_both_sides_only_within_depth() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = args(dir.path());
    let ms = moves();
    client(&a.games[0], &ms);
    master(&a.master, &ms, &[0, 1, 2, 3, 4], &[]);
    let root = parse_root(&format!("sfen {}", ms[0]["sfen_before"].as_str().unwrap())).unwrap();
    let line = json!({"id":"accepted", "root":format!("sfen {} 999", root.key), "depth":3, "own":"w", "game":[MOVES[..3]]});
    let exclude = dir.path().join("exclude.jsonl");
    write_json(&exclude, &[line]);
    a.exclude_queue.push(exclude);
    run(a).unwrap();
    let text = std::fs::read_to_string(dir.path().join("table.tsv")).unwrap();
    assert_eq!(text.matches("\taccepted\t").count(), 3);
    let q = queue(&dir.path().join("queue.jsonl"));
    assert_eq!(q.len(), 1);
    assert!(q[0].id.contains("_4_"));
}

#[test]
fn client_exit_requires_own_move_and_matching_pv_and_excludes_exit_move() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = args(dir.path());
    let mut ms = moves();
    ms[0]["eval"] = json!({"score_cp":200});
    ms[1]["eval"] = json!({"score_cp":200,"pv":["3c3d"]});
    ms[2]["eval"] = json!({"score_cp":200,"pv":["5i5h"]});
    ms[4]["eval"] = json!({"score_cp":200,"pv":["2f2e"]});
    client(&a.games[0], &ms);
    master(&a.master, &ms, &(0..10).collect::<Vec<_>>(), &[]);
    a.until = Until::Exit;
    run(a).unwrap();
    let text = std::fs::read_to_string(dir.path().join("table.tsv")).unwrap();
    assert_eq!(text.lines().count(), 5);
    let mut a = args(dir.path());
    a.max_ply = Some(2);
    run(a).unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("table.tsv")).unwrap().lines().count(),
        3
    );
}

#[test]
fn csa_keeps_king_shuffles_and_uses_time_for_own_exit() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = args(dir.path());
    a.games = vec![dir.path().join("match.data")];
    a.until = Until::Exit;
    std::fs::write(&a.games[0], "V2.2\nN+mine\nN-other\nPI\n+\n+5958OU\nT0\n-5152OU\nT5\n+5859OU\nT1\n-5251OU\nT0\n+7776FU\nT2\n%TORYO\n").unwrap();
    master(&a.master, &[], &[], &[]);
    run(a).unwrap();
    let text = std::fs::read_to_string(dir.path().join("table.tsv")).unwrap();
    assert_eq!(text.lines().count(), 5);
    assert!(text.contains("match\t4\tw\t5b5a"));
    let q = queue(&dir.path().join("queue.jsonl"));
    assert_eq!(q.len(), 2);
    assert!(q[1].id.contains("_4_"));
}

#[test]
fn pair_reader_selects_valid_pairs_and_uses_per_game_engine_and_matching_pv() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = args(dir.path());
    a.until = Until::Exit;
    let ms = moves();
    master(&a.master, &[], &[], &[]);
    let mut rows = vec![];
    for id in 0..4 {
        for (i, m) in ms.iter().enumerate() {
            let mut m = m.clone();
            m["game_id"] = json!(id);
            if id % 2 == 1 {
                m["engine"] = json!(if i % 2 == 0 { "other" } else { "mine" });
            }
            if i == 0 {
                m["eval"] = json!({"last_exact_primary":{"score":{"cp":10}}});
            }
            if i == 1 {
                m["eval"] = json!({"last_exact_primary":{"score":{"cp":10},"pv":["5a5b"]}});
            }
            if i == 4 {
                m["eval"] = json!({"score_cp":10,"pv":[MOVES[i]]});
            }
            if i >= 5 {
                m["eval"] = json!({"last_exact_primary":{"score":{"cp":10},"pv":[MOVES[i]]}});
            }
            rows.push(m);
        }
        rows.push(json!({"type":"result","game_id":id,"pair_index":id/2,"pair_slot":id%2,"outcome":"draw","error":id == 3}));
    }
    write_json(&a.games[0], &rows);
    run(a).unwrap();
    let text = std::fs::read_to_string(dir.path().join("table.tsv")).unwrap();
    assert_eq!(text.lines().count(), 10);
    assert!(!text.contains("games_2\t"));
    assert!(!text.contains("games_3\t"));
    let q = queue(&dir.path().join("queue.jsonl"));
    assert!(q.iter().any(|r| r.own == "b"));
    assert!(q.iter().any(|r| r.own == "w"));
}

#[test]
fn identical_roots_are_deduplicated_and_outputs_are_repeatable() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = args(dir.path());
    let ms = moves();
    client(&a.games[0], &ms);
    let other = dir.path().join("another.jsonl");
    client(&other, &ms);
    a.games.push(other.clone());
    a.games.push(other.clone());
    master(&a.master, &ms, &[0, 1, 2, 3], &[]);
    run(a).unwrap();
    let q = std::fs::read(dir.path().join("queue.jsonl")).unwrap();
    let table = std::fs::read(dir.path().join("table.tsv")).unwrap();
    assert_eq!(queue(&dir.path().join("queue.jsonl")).len(), 2);
    let mut a = args(dir.path());
    a.games.push(other);
    run(a).unwrap();
    assert_eq!(std::fs::read(dir.path().join("queue.jsonl")).unwrap(), q);
    assert_eq!(std::fs::read(dir.path().join("table.tsv")).unwrap(), table);
}

#[test]
fn invalid_input_does_not_replace_outputs_and_aliases_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let a = args(dir.path());
    let mut ms = moves();
    master(&a.master, &[], &[], &[]);
    ms[1]["move_usi"] = json!("5e5d");
    client(&a.games[0], &ms);
    std::fs::write(&a.table, "keep").unwrap();
    assert!(run(a).is_err());
    assert_eq!(std::fs::read_to_string(dir.path().join("table.tsv")).unwrap(), "keep");
    let mut a = args(dir.path());
    a.table = a.master.clone();
    assert!(run(a).is_err());
    assert!(own_side("mine", "mine-too", "mine").is_err());
}

#[derive(Parser)]
struct Cli {
    #[command(flatten)]
    args: GameGapsArgs,
}

#[test]
fn cli_requires_queue_priority_and_tag() {
    let base = [
        "test", "--master", "m", "--games", "g", "--player", "p", "--table", "t",
    ];
    assert!(Cli::try_parse_from(base).is_ok());
    assert!(Cli::try_parse_from(base.into_iter().chain(["--enqueue-out", "q"])).is_err());
    assert!(
        Cli::try_parse_from(base.into_iter().chain([
            "--enqueue-out",
            "q",
            "--tag",
            "test",
            "--priority",
            "2"
        ]))
        .is_ok()
    );
}

#[test]
fn legacy_pair_without_metadata_reuses_seeds_pairing() {
    let dir = tempfile::tempdir().unwrap();
    let a = args(dir.path());
    master(&a.master, &[], &[], &[]);
    let mut rows = Vec::new();
    for id in 0..2 {
        for mut m in moves() {
            m["game_id"] = json!(id);
            rows.push(m);
        }
        rows.push(json!({"type":"result", "game_id":id,"outcome":"draw"}));
    }
    write_json(&a.games[0], &rows);
    run(a).unwrap();
    let table = std::fs::read_to_string(dir.path().join("table.tsv")).unwrap();
    assert_eq!(table.lines().count(), 21);
    assert!(table.contains("games_0\t"));
    assert!(table.contains("games_1\t"));
}

#[test]
fn shared_root_merges_distinct_actual_lines() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = args(dir.path());
    let one = moves();
    let two = moves_for(&[
        "2g2f", "3c3d", "7g7f", "8c8d", "2f2e", "8d8e", "6i7h", "4a3b", "5g5f", "5c5d",
    ]);
    client(&a.games[0], &one);
    let other = dir.path().join("other.jsonl");
    client(&other, &two);
    a.games.push(other);
    master(&a.master, &[], &[], &[]);
    run(a).unwrap();
    let q = queue(&dir.path().join("queue.jsonl"));
    assert_eq!(q.len(), 4);
    let root = &q[0];
    assert_eq!(root.game.len(), 2);
    assert!(root.game.iter().any(|moves| moves[0] == "7g7f"));
    assert!(root.game.iter().any(|moves| moves[0] == "2g2f"));
    for row in q {
        row.check().unwrap();
    }
}

#[test]
fn csa_exit_uses_own_matching_pv_despite_timed_king_shuffles_and_book_scores() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = args(dir.path());
    a.games = vec![dir.path().join("match.csa")];
    a.until = Until::Exit;
    std::fs::write(&a.games[0], "V2.2\nN+mine\nN-other\nPI\n+\n+5958OU\nT2\n'** 0\n-5152OU\nT2\n'** 0 -5152OU\n+5859OU\nT2\n'** 0\n-5251OU\nT2\n+7776FU\nT0\n'** 10\n-3334FU\nT0\n+2726FU\nT0\n'** 20 +2726FU -8384FU\n%TORYO\n").unwrap();
    master(&a.master, &[], &[], &[]);
    run(a).unwrap();
    let text = std::fs::read_to_string(dir.path().join("table.tsv")).unwrap();
    assert_eq!(text.lines().count(), 7);
    assert!(text.contains("match\t5\tb\t7g7f"));
}

#[test]
fn skip_plies_omits_initial_csa_moves_from_table_and_roots() {
    let dir = tempfile::tempdir().unwrap();
    for (skip, expected) in [(0, vec![1, 2, 3, 4, 5, 6]), (4, vec![5, 6]), (8, vec![])] {
        let mut a = args(dir.path());
        a.games = vec![dir.path().join("match.csa")];
        a.until = Until::Exit;
        a.skip_plies = skip;
        std::fs::write(&a.games[0], "N+mine\nN-other\nPI\n+\n+5958OU\nT2\n-5152OU\nT2\n+5859OU\nT2\n-5251OU\nT2\n+7776FU\nT0\n-3334FU\nT0\n+2726FU\nT0\n'** 20 +2726FU\n%TORYO\n").unwrap();
        master(&a.master, &[], &[], &[]);
        run(a).unwrap();
        let table = std::fs::read_to_string(dir.path().join("table.tsv")).unwrap();
        let plies: Vec<u32> = table
            .lines()
            .skip(1)
            .map(|line| line.split('\t').nth(1).unwrap().parse().unwrap())
            .collect();
        assert_eq!(plies, expected);
        let roots: Vec<u32> = queue(&dir.path().join("queue.jsonl"))
            .iter()
            .map(|line| parse_root(&line.root).unwrap().ply)
            .collect();
        let expected_roots = match skip {
            0 => vec![1, 4],
            4 => vec![5],
            _ => vec![],
        };
        assert_eq!(roots, expected_roots);
    }
}

#[test]
fn csa_mismatching_own_pv_disables_time_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = args(dir.path());
    a.games = vec![dir.path().join("match.csa")];
    a.until = Until::Exit;
    std::fs::write(
        &a.games[0],
        "N+mine\nN-other\nPI\n+\n+7776FU\nT2\n'** 10 +2726FU\n%TORYO\n'** 10 +7776FU\n",
    )
    .unwrap();
    master(&a.master, &[], &[], &[]);
    run(a).unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("table.tsv")).unwrap().lines().count(),
        2
    );
}

#[test]
fn unknown_and_illegal_json_moves_are_errors_even_after_exit_or_max_ply() {
    for bad in ["nonsense", "7g7fgarbage", "5e5d", "pass"] {
        for until in [Until::Exit, Until::All] {
            let dir = tempfile::tempdir().unwrap();
            let mut a = args(dir.path());
            a.until = until;
            a.max_ply = Some(1);
            let mut ms = moves();
            ms[0]["eval"] = json!({"pv":[MOVES[0]]});
            ms[4]["move_usi"] = json!(bad);
            client(&a.games[0], &ms);
            master(&a.master, &[], &[], &[]);
            assert!(run(a).is_err(), "{bad}");
        }
    }
}

#[test]
fn json_terminals_and_timeouts_end_before_exit_detection_and_reject_later_moves() {
    for marker in ["resign", "win", "timeout", "illegal", "none", MOVES[2]] {
        let dir = tempfile::tempdir().unwrap();
        let mut a = args(dir.path());
        a.until = Until::Exit;
        let mut ms = moves();
        ms[2]["move_usi"] = json!(marker);
        ms[2]["timed_out"] = json!(marker == MOVES[2]);
        ms[2]["eval"] = json!({"pv":[marker]});
        client(&a.games[0], &ms[..3]);
        master(&a.master, &[], &[], &[]);
        run(a).unwrap();
        let text = std::fs::read_to_string(dir.path().join("table.tsv")).unwrap();
        assert_eq!(text.lines().count(), 3);
        assert_eq!(queue(&dir.path().join("queue.jsonl"))[0].game[0].len(), 2);
        let a = args(dir.path());
        client(&a.games[0], &ms[..4]);
        assert!(run(a).is_err(), "{marker}");
    }
}

#[test]
fn csa_rejects_unreadable_illegal_and_post_terminal_moves() {
    for suffix in [
        "+bad",
        "+7776ZZ",
        "+7776FUjunk",
        "+5554FU",
        "bogus",
        "%UNKNOWN",
        "%TORYO\n+7776FU",
        "%TIME_UP\n+7776FU",
        "'summary:time up:mine lose:other win\n+7776FU",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut a = args(dir.path());
        a.games = vec![dir.path().join("match.csa")];
        std::fs::write(&a.games[0], format!("N+mine\nN-other\nPI\n+\n{suffix}\n")).unwrap();
        master(&a.master, &[], &[], &[]);
        assert!(run(a).is_err(), "{suffix}");
    }
    for terminal in [
        "%TORYO",
        "%SENNICHITE",
        "%TIME_UP",
        "%KACHI",
        "%ILLEGAL_MOVE",
        "%+ILLEGAL_ACTION",
        "%-ILLEGAL_ACTION",
        "%HIKIWAKE",
        "%CHUDAN",
        "%JISHOGI",
        "%MAX_MOVES",
    ] {
        assert_eq!(csa_pvs(&format!("+7776FU\n{terminal}\n'** 10 +7776FU\n")).unwrap(), [None]);
    }
}

#[test]
fn coverage_requires_matching_game_prefix_and_sufficient_length() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = args(dir.path());
    let ms = moves();
    client(&a.games[0], &ms);
    master(&a.master, &[], &[], &[]);
    let root = format!("sfen {}", ms[0]["sfen_before"].as_str().unwrap());
    let lines = [
        json!({"id":"wrong", "root":root, "own":"b", "depth":5, "game":[["2g2f"]]}),
        json!({"id":"branch", "root":root, "own":"b", "depth":5, "game":[["7g7f", "8c8d"]]}),
        json!({"id":"short", "root":root, "own":"b", "depth":5, "game":[MOVES[..1]]}),
        json!({"id":"other", "root":root, "own":"w", "depth":3, "game":[MOVES[..5]]}),
        json!({"id":"empty", "root":root, "own":"bw", "depth":5}),
    ];
    let exclude = dir.path().join("exclude.jsonl");
    write_json(&exclude, &lines);
    a.exclude_queue.push(exclude);
    run(a).unwrap();
    let text = std::fs::read_to_string(dir.path().join("table.tsv")).unwrap();
    let rows: Vec<_> = text.lines().skip(1).map(|s| s.split('\t').nth(5).unwrap()).collect();
    assert_eq!(&rows[..5], ["branch,other,short", "other", "other", "", ""]);
}

#[test]
fn queue_identity_is_fixed_and_latest_accepted_fields_replace_earlier_fields() {
    let dir = tempfile::tempdir().unwrap();
    let ms = moves();
    let root = format!("sfen {}", ms[0]["sfen_before"].as_str().unwrap());
    let base = json!({"id":"accepted", "root":root, "own":"b", "depth":5, "game":[MOVES[..5]]});
    let mut changed_root = base.clone();
    changed_root["root"] = json!(format!("sfen {}", ms[2]["sfen_before"].as_str().unwrap()));
    changed_root["game"] = json!([MOVES[2..5]]);
    let mut changed_ply = base.clone();
    changed_ply["root"] = json!(format!("sfen {HIRATE_KEY} 9"));
    let mut changed_own = base.clone();
    changed_own["own"] = json!("bw");
    let mut changed_case = base.clone();
    changed_case["id"] = json!("Accepted");
    let mut latest = base.clone();
    latest["depth"] = json!(2);
    latest["priority"] = json!(50);
    latest["hold"] = json!(true);
    latest["game"] = json!([MOVES[..2]]);
    latest["opts"] = json!({"opp_n":1});
    let first = dir.path().join("first.jsonl");
    let second = dir.path().join("second.jsonl");
    write_json(&first, &[base, latest]);
    write_json(&second, &[changed_root, changed_ply, changed_own, changed_case]);
    let excluded = read_excluded(&[first, second]).unwrap();
    assert_eq!(excluded.len(), 1);
    let row = &excluded[HIRATE_KEY][0];
    assert_eq!(row.id, "accepted");
    assert_eq!(row.depth, 2);
    assert_eq!(row.game, [MOVES[..2].to_vec()]);
}

#[test]
fn additional_roots_participate_in_coverage() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = args(dir.path());
    let ms = moves();
    client(&a.games[0], &ms);
    master(&a.master, &[], &[], &[]);
    let exclude = dir.path().join("exclude.jsonl");
    // 玉の往復後も同じ初手が合法なため、追加の根と主根を別の局面にできる。
    let primary = moves_for(&["5i5h", "5a5b", "5h5i"]);
    write_json(
        &exclude,
        &[
            json!({"id":"extra", "root":format!("sfen {}", primary[2]["sfen_before"].as_str().unwrap()), "roots":["startpos"], "own":"w", "depth":2, "game":[MOVES[..2]]}),
        ],
    );
    a.exclude_queue.push(exclude);
    run(a).unwrap();
    let text = std::fs::read_to_string(dir.path().join("table.tsv")).unwrap();
    assert_eq!(text.matches("\textra\t").count(), 2);
}

#[test]
fn generated_game_length_does_not_hide_gaps_beyond_eight_moves() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = args(dir.path());
    let ms = moves();
    client(&a.games[0], &ms);
    master(&a.master, &[], &[], &[]);
    a.depth = 20;
    run(a).unwrap();
    let q = queue(&dir.path().join("queue.jsonl"));
    assert_eq!(q.len(), 2);
    assert!(q[1].id.contains("_9_"));
}

#[test]
fn pair_rejects_moves_after_result() {
    let dir = tempfile::tempdir().unwrap();
    let a = args(dir.path());
    master(&a.master, &[], &[], &[]);
    let mut rows = Vec::new();
    for id in 0..2 {
        for mut m in moves() {
            m["game_id"] = json!(id);
            rows.push(m);
        }
        rows.push(json!({"type":"result","game_id":id,"outcome":"draw"}));
    }
    rows.push(moves()[0].clone());
    write_json(&a.games[0], &rows);
    assert!(run(a).is_err());
}

#[test]
fn csa_early_exit_excludes_the_first_searched_move() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = args(dir.path());
    a.games = vec![dir.path().join("early.csa")];
    a.until = Until::Exit;
    std::fs::write(&a.games[0], "N+mine\nN-other\nPI\n+\n+7776FU\nT0\n'** 10 +7776FU\n%TORYO\n")
        .unwrap();
    master(&a.master, &[], &[], &[]);
    run(a).unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("table.tsv")).unwrap().lines().count(),
        1
    );
    assert!(queue(&dir.path().join("queue.jsonl")).is_empty());
}
