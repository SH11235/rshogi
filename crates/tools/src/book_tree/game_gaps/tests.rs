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
        line: false,
        line_sides: LineSides::Both,
        collapse_cp: 150,
        after_collapse: 3,
        games: vec![dir.join("games.jsonl")],
        player: Some("mine".into()),
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
        assert_eq!(
            csa_comments(&format!("+7776FU\n{terminal}\n'** 10 +7776FU\n")).unwrap(),
            [CsaComment::default()]
        );
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
fn additional_roots_without_game_do_not_cover_root_move_or_continuations() {
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
    assert_eq!(text.matches("\textra\t").count(), 0);
    let rows: Vec<_> = text.lines().skip(1).map(|s| s.split('\t').nth(5).unwrap()).collect();
    assert_eq!(&rows[..2], ["", ""]);
    assert!(queue(&dir.path().join("queue.jsonl"))[0].id.contains("_1_"));
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

#[test]
fn csa_inline_comments_use_the_same_exit_detection_as_separate_lines() {
    for separator in [",", "\n"] {
        for (pv, expected_rows) in [("+7776FU", 1), ("+2726FU", 2)] {
            let dir = tempfile::tempdir().unwrap();
            let mut a = args(dir.path());
            a.games = vec![dir.path().join("inline.csa")];
            a.until = Until::Exit;
            let time = if pv == "+7776FU" { 0 } else { 2 };
            std::fs::write(
                &a.games[0],
                format!(
                    "N+mine\nN-other\nPI\n+\n+7776FU{separator}T{time}{separator}'** 10 {pv}\n%TORYO\n"
                ),
            )
            .unwrap();
            master(&a.master, &[], &[], &[]);
            run(a).unwrap();
            let table = std::fs::read_to_string(dir.path().join("table.tsv")).unwrap();
            assert_eq!(table.lines().count(), expected_rows, "{separator:?} {pv}");
        }
    }
}

fn pave_args(dir: &Path) -> pave::GamePaveArgs {
    #[derive(Parser)]
    struct PaveCli {
        #[command(flatten)]
        args: pave::GamePaveArgs,
    }
    PaveCli::parse_from([
        "test",
        "--master",
        dir.join("moves.tsv").to_str().unwrap(),
        "--audit-db",
        dir.join("audit.db").to_str().unwrap(),
        "--games",
        dir.join("games.jsonl").to_str().unwrap(),
        "--table",
        dir.join("table.tsv").to_str().unwrap(),
        "--enqueue-out",
        dir.join("queue.jsonl").to_str().unwrap(),
        "--tag",
        "pave",
        "--priority",
        "20",
    ])
    .args
}

fn pave_pair(dir: &Path, games: &[(&[Value], &str)]) {
    let mut rows =
        vec![json!({"type":"meta", "engine_cmd":{"label_black":"mine","label_white":"other"}})];
    for (id, (ms, outcome)) in games.iter().enumerate() {
        for m in *ms {
            let mut m = m.clone();
            m["game_id"] = json!(id);
            rows.push(m);
        }
        rows.push(json!({"type":"result","game_id":id,"pair_index":id/2,"pair_slot":id%2,"outcome":outcome}));
    }
    write_json(&dir.join("games.jsonl"), &rows);
    std::fs::write(dir.join("audit.db"), "#YANEURAOU-DB2016 1.00\n").unwrap();
}

fn pave_table(dir: &Path) -> Vec<BTreeMap<String, String>> {
    let text = std::fs::read_to_string(dir.join("table.tsv")).unwrap();
    let mut lines = text.lines();
    let header: Vec<_> = lines.next().unwrap().split('\t').collect();
    lines
        .map(|line| {
            header
                .iter()
                .zip(line.split('\t'))
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        })
        .collect()
}

fn scored_moves() -> Vec<Value> {
    let mut ms = moves();
    for (i, m) in ms.iter_mut().enumerate() {
        m["eval"] = json!({"last_exact_primary":{"score":{"cp":if i%2==0 {-200} else {200}},"pv":[MOVES[i]]}});
    }
    ms
}

#[test]
fn pave_collapse_uses_both_sides_recovers_and_ignores_scores_without_pv() {
    let dir = tempfile::tempdir().unwrap();
    let mut ms = scored_moves();
    ms[2]["eval"] = json!({"score_cp":0,"pv":[MOVES[2]]});
    ms[3]["eval"] = json!({"score_cp":200});
    ms[4]["eval"] = Value::Null;
    ms[5]["eval"] = json!({"score_cp":200,"pv":[MOVES[5]]});
    let mut recovered = ms.clone();
    recovered[9]["eval"] = json!({"last_exact_primary":{"score":{"cp":0},"pv":[MOVES[9]]}});
    pave_pair(dir.path(), &[(&ms, "white_win"), (&recovered, "white_win")]);
    master(&dir.path().join("moves.tsv"), &[], &[], &[]);
    pave::run(pave_args(dir.path())).unwrap();
    let table = pave_table(dir.path());
    assert_eq!(table[0]["collapse"], "6");
    assert_eq!(table[0]["target_end"], "9");
    assert_eq!(table[0]["state"], "open");
    assert_eq!(table[0]["own"], "b");
    assert_eq!(table[1]["state"], "no-collapse");
    assert_eq!(table[1]["rows"], "0");
    assert!(queue(&dir.path().join("queue.jsonl")).iter().all(|q| q.own == "b"));
}

#[test]
fn pave_collapse_ignores_bounds_and_prefers_exact_primary() {
    let dir = tempfile::tempdir().unwrap();
    master(&dir.path().join("moves.tsv"), &[], &[], &[]);
    for bound in ["lowerbound", "upperbound"] {
        let mut ms = scored_moves();
        ms[9]["eval"] = json!({"score_cp":0,"score_bound":bound,"pv":[MOVES[9]]});
        let mut primary = ms.clone();
        primary[9]["eval"]["last_exact_primary"] = json!({"score":{"cp":0},"pv":[MOVES[9]]});
        let mut bounded_loss = moves();
        bounded_loss[0]["eval"] = json!({"score_cp":-200,"score_bound":bound,"pv":[MOVES[0]]});
        pave_pair(
            dir.path(),
            &[
                (&ms, "white_win"),
                (&primary, "white_win"),
                (&bounded_loss, "white_win"),
                (&bounded_loss, "white_win"),
            ],
        );
        pave::run(pave_args(dir.path())).unwrap();
        let table = pave_table(dir.path());
        assert_eq!(table[0]["collapse"], "1");
        assert_eq!(table[1]["state"], "no-collapse");
        assert_eq!(table[2]["state"], "no-collapse");
    }
}

#[test]
fn pave_settled_uses_played_edge_tree_provenance_and_reports_main_match() {
    let dir = tempfile::tempdir().unwrap();
    let mut ms = scored_moves();
    ms[0]["eval"] = json!({"score_cp":0,"pv":[MOVES[0]]});
    for (kind, main_same, expected) in [
        ("tree:1", true, "settled"),
        ("single", true, "open"),
        ("tree:1", false, "settled"),
    ] {
        pave_pair(dir.path(), &[(&ms, "white_win"), (&ms, "white_win")]);
        let root = parse_root(&format!("sfen {}", ms[0]["sfen_before"].as_str().unwrap())).unwrap();
        let mut rows = vec![Row::from_fields(&[
            ("key", root.key.clone()),
            ("ply", "1".into()),
            ("move", MOVES[0].into()),
            ("role", if main_same { "main" } else { "record" }.into()),
            ("value_kind", kind.into()),
            ("evals", "-200".into()),
            ("cond", "unknown".into()),
        ])];
        if !main_same {
            rows.push(Row::from_fields(&[
                ("key", root.key.clone()),
                ("ply", "1".into()),
                ("move", "2g2f".into()),
                ("role", "main".into()),
            ]));
        }
        std::fs::write(dir.path().join("moves.tsv"), format_master(&rows)).unwrap();
        std::fs::write(
            dir.path().join("audit.db"),
            format!("sfen {} 999\n7g7f none -150 3 1\n2g2f none 50 3 1\n", root.key),
        )
        .unwrap();
        let result = pave::run(pave_args(dir.path()));
        result.unwrap();
        let table = pave_table(dir.path());
        assert_eq!(table[0]["state"], expected);
        if expected == "settled" {
            assert_eq!(table[0]["settled_ply"], "1");
            assert_eq!(table[0]["settled_main_same"], main_same.to_string());
            assert!(queue(&dir.path().join("queue.jsonl")).is_empty());
        }
    }
}

#[test]
fn pave_settled_uses_loser_turn_value_threshold_and_collapse_limit() {
    let dir = tempfile::tempdir().unwrap();
    for (outcome, own, before, after) in [("white_win", "b", 0, 4), ("black_win", "w", 1, 3)] {
        let mut ms = scored_moves();
        for (i, m) in ms.iter_mut().enumerate() {
            let score = if i < 2 {
                0
            } else if m["side_to_move"] == own {
                -200
            } else {
                200
            };
            m["eval"] = json!({"score_cp":score,"pv":[MOVES[i]]});
        }
        for (index, value, expected) in [
            (before, -150, "settled"),
            (before, -149, "open"),
            (before, 150, "open"),
            (after, -150, "open"),
        ] {
            pave_pair(dir.path(), &[(&ms, outcome), (&ms, outcome)]);
            let root = parse_root(&format!("sfen {}", ms[index]["sfen_before"].as_str().unwrap()))
                .unwrap();
            let rows = [Row::from_fields(&[
                ("key", root.key.clone()),
                ("ply", root.ply.to_string()),
                ("move", MOVES[index].into()),
                ("role", "main".into()),
                ("value_kind", "tree:1".into()),
                ("evals", "200".into()),
                ("cond", "unknown".into()),
            ])];
            std::fs::write(dir.path().join("moves.tsv"), format_master(&rows)).unwrap();
            std::fs::write(
                dir.path().join("audit.db"),
                format!("sfen {} 1\n{} none {value} 3 1\n", root.key, MOVES[index]),
            )
            .unwrap();
            pave::run(pave_args(dir.path())).unwrap();
            let table = pave_table(dir.path());
            assert_eq!(table[0]["own"], own);
            assert_eq!(table[0]["collapse"], "3");
            assert_eq!(table[0]["state"], expected, "{outcome}, index={index}, value={value}");
            assert_eq!(
                table[0]["settled_ply"],
                if expected == "settled" {
                    (index + 1).to_string()
                } else {
                    String::new()
                }
            );
        }
    }
}

#[test]
fn pave_target_extends_to_end_disagrees_and_queue_requires_actual_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let ms = scored_moves();
    pave_pair(dir.path(), &[(&ms, "white_win"), (&ms, "white_win")]);
    for (reach, target, state) in [(0, 4, "open"), (4, 10, "open"), (10, 10, "disagree")] {
        master(&dir.path().join("moves.tsv"), &ms[..reach], &[], &[]);
        pave::run(pave_args(dir.path())).unwrap();
        let table = pave_table(dir.path());
        assert_eq!(table[0]["book_reach"], reach.to_string());
        assert_eq!(table[0]["target_end"], target.to_string());
        assert_eq!(table[0]["state"], state);
    }
    master(&dir.path().join("moves.tsv"), &[], &[], &[]);
    let excluded = dir.path().join("excluded.jsonl");
    let root = format!("sfen {}", ms[0]["sfen_before"].as_str().unwrap());
    write_json(
        &excluded,
        &[json!({"id":"accepted","root":root,"depth":8,"own":"b","game":[MOVES[..8]]})],
    );
    let mut a = pave_args(dir.path());
    a.exclude_queue.push(excluded.clone());
    pave::run(a).unwrap();
    assert_eq!(pave_table(dir.path())[0]["state"], "queued");
    assert_eq!(pave_table(dir.path())[0]["covered_by"], "accepted");
    assert!(queue(&dir.path().join("queue.jsonl")).is_empty());
    write_json(
        &excluded,
        &[json!({"id":"wrong","root":root,"depth":8,"own":"b","game":[["2g2f"]]})],
    );
    let mut a = pave_args(dir.path());
    a.exclude_queue.push(excluded);
    pave::run(a).unwrap();
    assert_eq!(pave_table(dir.path())[0]["state"], "open");
}

#[test]
fn pave_spacing_last_extend_merge_and_repeatability() {
    let dir = tempfile::tempdir().unwrap();
    let mut short = scored_moves();
    let mut long = short.clone();
    for i in 0..5 {
        long[i]["eval"] = json!({"score_cp":0,"pv":[MOVES[i]]});
    }
    short[0]["eval"] = json!({"score_cp":0,"pv":[MOVES[0]]});
    pave_pair(dir.path(), &[(&short, "white_win"), (&long, "white_win")]);
    master(&dir.path().join("moves.tsv"), &[], &[], &[]);
    let mut a = pave_args(dir.path());
    a.extend = Some(12);
    pave::run(a).unwrap();
    let q = queue(&dir.path().join("queue.jsonl"));
    let plies: Vec<_> = q.iter().map(|q| parse_root(&q.root).unwrap().ply).collect();
    assert_eq!(plies, [1, 4, 7]);
    assert_eq!(q[0].opts.extend, None);
    assert_eq!(q[1].opts.extend, Some(12));
    assert_eq!(q[2].opts.extend, Some(12));
    assert_eq!(q[0].game[0].len(), 8);
    assert_eq!(q[0].memo.as_deref(), Some("games_0 の崩れまでの舗装の根 1 手目"));
    let table = std::fs::read(dir.path().join("table.tsv")).unwrap();
    let output = std::fs::read(dir.path().join("queue.jsonl")).unwrap();
    let mut a = pave_args(dir.path());
    a.extend = Some(12);
    a.games.push(a.games[0].clone());
    pave::run(a).unwrap();
    assert_eq!(std::fs::read(dir.path().join("table.tsv")).unwrap(), table);
    assert_eq!(std::fs::read(dir.path().join("queue.jsonl")).unwrap(), output);
}

#[test]
fn pave_draw_repetition_includes_final_position_and_reports_first_entry() {
    let dir = tempfile::tempdir().unwrap();
    let ms = moves_for(&["5i5h", "5a5b", "5h5i", "5b5a"]);
    pave_pair(dir.path(), &[(&ms, "draw"), (&moves(), "draw")]);
    master(&dir.path().join("moves.tsv"), &[], &[], &[]);
    pave::run(pave_args(dir.path())).unwrap();
    let table = pave_table(dir.path());
    assert_eq!(table[0]["state"], "repetition");
    assert_eq!(table[0]["repetition_entry"], "1");
    assert_eq!(table[0]["own"], "");
    assert_eq!(table[1]["state"], "draw-other");
    assert!(queue(&dir.path().join("queue.jsonl")).is_empty());
}

#[test]
fn pave_white_loser_mates_and_primary_precedence() {
    let dir = tempfile::tempdir().unwrap();
    let mut ms = moves();
    for (i, m) in ms.iter_mut().enumerate() {
        m["eval"] = json!({"score_mate_sign": if i%2==0 {1} else {-1},"pv":[MOVES[i]]});
    }
    ms[0]["eval"] = json!({"score_cp":200,"pv":[MOVES[0]],"last_exact_primary":{"score":{"cp":0},"pv":[MOVES[0]]}});
    ms[1]["eval"] = json!({"score_cp":0,"pv":[MOVES[1]]});
    ms[2]["eval"] = json!({"score":{"mate":3},"pv":[MOVES[2]]});
    pave_pair(dir.path(), &[(&ms, "black_win"), (&ms, "black_win")]);
    master(&dir.path().join("moves.tsv"), &[], &[], &[]);
    pave::run(pave_args(dir.path())).unwrap();
    assert_eq!(pave_table(dir.path())[0]["collapse"], "3");
    assert!(queue(&dir.path().join("queue.jsonl")).iter().all(|q| q.own == "w"));
    ms[9]["eval"] = json!({"score_cp":0,"pv":[MOVES[9]],"last_exact_primary":{"score":{"cp":0}}});
    pave_pair(dir.path(), &[(&ms, "black_win"), (&ms, "black_win")]);
    pave::run(pave_args(dir.path())).unwrap();
    assert_eq!(pave_table(dir.path())[0]["collapse"], "3");
}

#[test]
fn pave_rejects_other_formats_and_preserves_outputs_on_error() {
    let dir = tempfile::tempdir().unwrap();
    master(&dir.path().join("moves.tsv"), &[], &[], &[]);
    std::fs::write(dir.path().join("audit.db"), "").unwrap();
    std::fs::write(dir.path().join("table.tsv"), "keep").unwrap();
    client(&dir.path().join("games.jsonl"), &moves());
    let error = pave::run(pave_args(dir.path())).unwrap_err().to_string();
    assert!(error.contains("pair JSON Lines"), "{error}");
    assert!(error.contains("csa_client"), "{error}");
    std::fs::write(dir.path().join("games.jsonl"), "PI\n+\n%TORYO\n").unwrap();
    assert!(
        pave::run(pave_args(dir.path()))
            .unwrap_err()
            .to_string()
            .contains("pair JSON Lines")
    );
    assert_eq!(std::fs::read_to_string(dir.path().join("table.tsv")).unwrap(), "keep");
    let ms = scored_moves();
    pave_pair(dir.path(), &[(&ms, "white_win"), (&ms, "white_win")]);
    let mut a = pave_args(dir.path());
    a.table = a.audit_db.clone();
    assert!(pave::run(a).is_err());
}

#[test]
fn pave_sorts_games_by_id_and_caps_root_spacing_at_eight() {
    let dir = tempfile::tempdir().unwrap();
    let ms = scored_moves();
    pave_pair(dir.path(), &[(&ms, "white_win"), (&ms, "white_win")]);
    let rows: Vec<Value> = std::fs::read_to_string(dir.path().join("games.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let mut reordered = vec![rows[0].clone()];
    reordered.extend(rows.iter().filter(|v| v["game_id"] == 1).cloned());
    reordered.extend(rows.iter().filter(|v| v["game_id"] == 0).cloned());
    write_json(&dir.path().join("games.jsonl"), &reordered);
    master(&dir.path().join("moves.tsv"), &[], &[], &[]);
    let mut a = pave_args(dir.path());
    a.depth = 9;
    a.after_collapse = 20;
    pave::run(a).unwrap();
    assert_eq!(pave_table(dir.path())[0]["game"], "games_0");
    assert_eq!(pave_table(dir.path())[1]["game"], "games_1");
    assert_eq!(pave_table(dir.path())[0]["target_end"], ms.len().to_string());
    let q = queue(&dir.path().join("queue.jsonl"));
    assert_eq!(q.iter().map(|q| parse_root(&q.root).unwrap().ply).collect::<Vec<_>>(), [1, 9]);
}

#[test]
fn merged_queue_takes_maximum_extend_and_rejects_other_option_differences() {
    let make = |extend, stop| -> Line {
        serde_json::from_value(json!({"id":"test","root":"startpos","own":"b","depth":3,"game":[MOVES[..8]],"opts":{"extend":extend,"stop":stop}})).unwrap()
    };
    for (stop, succeeds) in [(300, true), (400, false)] {
        let mut sorter = Sorter::new();
        for line in [make(Some(15), 300), make(Some(4), stop), make(None, 300)] {
            sorter
                .push(
                    vec![
                        HIRATE_KEY.into(),
                        "b".into(),
                        serde_json::to_string(&line.game[0]).unwrap(),
                    ],
                    serde_json::to_string(&line).unwrap(),
                )
                .unwrap();
        }
        let result = merge_queue(sorter);
        assert_eq!(result.is_ok(), succeeds);
        if let Ok(mut file) = result {
            use std::io::{Read, Seek};
            file.rewind().unwrap();
            let mut text = String::new();
            file.read_to_string(&mut text).unwrap();
            let line: Line = serde_json::from_str(text.trim()).unwrap();
            assert_eq!(line.opts.extend, Some(15));
            assert_eq!(line.game.len(), 1);
        }
    }
}

fn line_args(dir: &Path) -> GameGapsArgs {
    let mut a = args(dir);
    a.line = true;
    a.until = Until::Exit;
    a.depth = 2;
    a.games = vec![dir.join("line.csa")];
    a
}

fn line_csa(path: &Path, scores: &[Option<i32>], ending: &str) {
    let (mut pos, _, _) = rshogi_csa::parse_csa_full(
        "PI
+
",
    )
    .unwrap();
    let mut text = String::from("N+mine\nN-other\nPI\n+\n");
    for (mv, score) in MOVES.iter().zip(scores) {
        let csa = rshogi_csa::usi_move_to_csa(mv, &pos).unwrap();
        text.push_str(&format!("{csa}\n"));
        if let Some(score) = score {
            text.push_str(&format!("'** {score}\n"));
        }
        pos.apply_csa_move(&csa).unwrap();
    }
    text.push_str(ending);
    std::fs::write(path, text).unwrap();
}

fn line_fixture(dir: &Path) {
    line_csa(
        &dir.join("line.csa"),
        &[
            Some(400),
            Some(0),
            Some(200),
            None,
            Some(300),
            Some(300),
            Some(300),
            Some(300),
            Some(300),
            Some(300),
        ],
        "%TORYO\n'summary:toryo:mine win:other lose\n",
    );
    master(&dir.join("moves.tsv"), &moves(), &[], &[]);
}

#[test]
fn line_both_sides_sustained_endpoints_and_actual_game_moves() {
    let dir = tempfile::tempdir().unwrap();
    line_fixture(dir.path());
    run(line_args(dir.path())).unwrap();
    let q = queue(&dir.path().join("queue.jsonl"));
    let table = pave_table(dir.path());
    assert_eq!(
        q.iter().map(|q| parse_root(&q.root).unwrap().ply).collect::<Vec<_>>(),
        [1, 1, 3, 3, 5, 5]
    );
    assert_eq!(
        table.iter().map(|r| r["end_reason"].as_str()).collect::<Vec<_>>(),
        ["stop", "collapse", "stop", "collapse", "stop", "collapse"]
    );
    for row in &q {
        let ply = parse_root(&row.root).unwrap().ply as usize;
        assert_eq!(row.game, vec![MOVES[ply - 1..ply + 1].to_vec()]);
        assert_eq!(
            table
                .iter()
                .find(|r| r["ply"] == ply.to_string() && r["own"] == row.own)
                .unwrap()["own"],
            row.own
        );
    }
}

#[test]
fn line_side_selection_unknown_and_draw_use_loser_rule() {
    let dir = tempfile::tempdir().unwrap();
    line_fixture(dir.path());
    for (sides, expected) in [
        (LineSides::Both, vec![1, 1, 3, 3, 5, 5]),
        (LineSides::Loser, vec![1, 3, 5]),
        (LineSides::Winner, vec![1, 3, 5]),
    ] {
        let mut a = line_args(dir.path());
        a.line_sides = sides;
        run(a).unwrap();
        assert_eq!(
            queue(&dir.path().join("queue.jsonl"))
                .iter()
                .map(|q| parse_root(&q.root).unwrap().ply)
                .collect::<Vec<_>>(),
            expected
        );
    }
    for ending in [
        "%SENNICHITE\n",
        "%CHUDAN\n",
        "",
        "%TORYO\n'summary:sennichite:mine draw:other draw\n",
    ] {
        line_csa(&dir.path().join("line.csa"), &[Some(200); 10], ending);
        let mut a = line_args(dir.path());
        a.line_sides = LineSides::Loser;
        run(a).unwrap();
        let table = pave_table(dir.path());
        assert_eq!(
            table.iter().map(|r| r["ply"].as_str()).collect::<Vec<_>>(),
            ["1", "1", "3", "3", "5", "7", "9"]
        );
        assert!(table.iter().filter(|r| r["own"] == "b").all(|r| r["end_reason"] == "game-end"));
        assert!(table.iter().filter(|r| r["own"] == "w").all(|r| r["end_reason"] == "collapse"));
        let mut a = line_args(dir.path());
        a.line_sides = LineSides::Winner;
        run(a).unwrap();
        assert!(queue(&dir.path().join("queue.jsonl")).is_empty());
    }
}

#[test]
fn line_exclusion_requires_every_move_and_matching_prefix() {
    let dir = tempfile::tempdir().unwrap();
    line_fixture(dir.path());
    let excluded = dir.path().join("excluded.jsonl");
    for (game, depth, expected) in [
        (vec!["7g7f"], 4, vec![1, 1, 3, 3, 5, 5]),
        (MOVES[..4].to_vec(), 1, vec![1, 1, 3, 3, 5, 5]),
        (vec!["2g2f", "3c3d", "7g7f", "8c8d"], 4, vec![1, 1, 3, 3, 5, 5]),
        (MOVES[..4].to_vec(), 4, vec![1, 3, 5, 5]),
    ] {
        write_json(
            &excluded,
            &[json!({"id":"accepted","root":"startpos","own":"w","depth":depth,"game":[game]})],
        );
        let mut a = line_args(dir.path());
        a.exclude_queue.push(excluded.clone());
        run(a).unwrap();
        assert_eq!(
            queue(&dir.path().join("queue.jsonl"))
                .iter()
                .map(|q| parse_root(&q.root).unwrap().ply)
                .collect::<Vec<_>>(),
            expected
        );
        assert_eq!(pave_table(dir.path())[0]["covered_by"], "");
    }
}

#[test]
fn line_skip_max_depth_spacing_and_terminal_truncation() {
    let dir = tempfile::tempdir().unwrap();
    line_csa(&dir.path().join("line.csa"), &[None; 10], "%CHUDAN\n");
    master(&dir.path().join("moves.tsv"), &[], &[], &[]);
    let mut a = line_args(dir.path());
    a.skip_plies = 2;
    a.max_ply = Some(8);
    run(a).unwrap();
    assert_eq!(
        pave_table(dir.path()).iter().map(|r| r["ply"].as_str()).collect::<Vec<_>>(),
        ["3", "3", "5", "5", "7", "7"]
    );
    assert!(pave_table(dir.path()).iter().all(|r| r["end_reason"] == "max-ply"));
    let mut a = line_args(dir.path());
    a.depth = 20;
    run(a).unwrap();
    let q = queue(&dir.path().join("queue.jsonl"));
    assert_eq!(q.len(), 4);
    assert_eq!(q[0].game[0], MOVES);
    assert_eq!(q[1].game[0], MOVES);
    assert_eq!(q[2].game[0], MOVES[8..]);
    assert_eq!(q[3].game[0], MOVES[8..]);
    let mut a = line_args(dir.path());
    a.depth = 3;
    run(a).unwrap();
    assert_eq!(
        pave_table(dir.path()).iter().map(|r| r["ply"].as_str()).collect::<Vec<_>>(),
        ["1", "1", "4", "4", "7", "7", "10", "10"]
    );
}

#[test]
fn line_ids_include_basename_and_game_sequence_and_outputs_repeat() {
    let dir = tempfile::tempdir().unwrap();
    line_fixture(dir.path());
    let other = dir.path().join("other");
    std::fs::create_dir(&other).unwrap();
    std::fs::copy(dir.path().join("line.csa"), other.join("line.csa")).unwrap();
    run(line_args(dir.path())).unwrap();
    let first = queue(&dir.path().join("queue.jsonl"))[0].id.clone();
    let mut a = line_args(dir.path());
    a.games = vec![other.join("line.csa")];
    run(a).unwrap();
    let second = queue(&dir.path().join("queue.jsonl"))[0].id.clone();
    assert_eq!(first, second);
    line_csa(&other.join("line.csa"), &[None; 9], "%TORYO\n");
    let mut a = line_args(dir.path());
    a.games = vec![other.join("line.csa")];
    run(a).unwrap();
    assert_ne!(second, queue(&dir.path().join("queue.jsonl"))[0].id);
    let mut a = line_args(dir.path());
    a.games.push(other.join("line.csa"));
    run(a).unwrap();
    let table = std::fs::read(dir.path().join("table.tsv")).unwrap();
    let output = std::fs::read(dir.path().join("queue.jsonl")).unwrap();
    let mut a = line_args(dir.path());
    a.games.insert(0, other.join("line.csa"));
    a.games.push(a.games[1].clone());
    run(a).unwrap();
    assert_eq!(std::fs::read(dir.path().join("table.tsv")).unwrap(), table);
    assert_eq!(std::fs::read(dir.path().join("queue.jsonl")).unwrap(), output);
}

#[test]
fn line_json_client_and_pair_share_exact_score_rules() {
    let dir = tempfile::tempdir().unwrap();
    master(&dir.path().join("moves.tsv"), &moves(), &[], &[]);
    let mut ms = scored_moves();
    for (i, m) in ms.iter_mut().enumerate() {
        m["eval"] = json!({"score_cp": if i < 2 { 0 } else if i % 2 == 0 { 300 } else { -300 }, "pv":[MOVES[i]]});
    }
    ms[3]["eval"] = json!({"score_cp":0,"score_bound":"lowerbound","pv":[MOVES[3]]});
    for pair in [false, true] {
        if pair {
            pave_pair(dir.path(), &[(&ms, "black_win"), (&ms, "black_win")]);
        } else {
            let mut rows = vec![
                json!({"type":"meta","engine_cmd":{"label_black":"mine","label_white":"other"}}),
            ];
            rows.extend(ms.clone());
            rows.push(json!({"type":"result","outcome":"black_win"}));
            write_json(&dir.path().join("games.jsonl"), &rows);
        }
        let mut a = line_args(dir.path());
        a.games = vec![dir.path().join("games.jsonl")];
        a.player = None;
        run(a).unwrap();
        let q = queue(&dir.path().join("queue.jsonl"));
        assert_eq!(
            q.iter().map(|q| parse_root(&q.root).unwrap().ply).collect::<Vec<_>>(),
            [1, 1, 3, 3, 5]
        );
        assert!(
            pave_table(dir.path())
                .iter()
                .filter(|r| r["own"] == "b")
                .all(|r| r["end_reason"] == "stop")
        );
    }
}

#[test]
fn line_csa_terminal_winner_and_inline_scores() {
    let dir = tempfile::tempdir().unwrap();
    master(&dir.path().join("moves.tsv"), &[], &[], &[]);
    for (terminal, own) in [
        ("%TORYO", "w"),
        ("%TIME_UP", "w"),
        ("%ILLEGAL_MOVE", "w"),
        ("%KACHI", "b"),
    ] {
        line_csa(&dir.path().join("line.csa"), &[Some(0); 10], terminal);
        let mut a = line_args(dir.path());
        a.line_sides = LineSides::Winner;
        run(a).unwrap();
        assert!(queue(&dir.path().join("queue.jsonl")).iter().all(|q| q.own == own));
    }
    line_fixture(dir.path());
    let path = dir.path().join("line.csa");
    let text = std::fs::read_to_string(&path).unwrap().replace("\n'**", ",'**");
    std::fs::write(path, text).unwrap();
    run(line_args(dir.path())).unwrap();
    assert_eq!(
        pave_table(dir.path()).iter().map(|r| r["ply"].as_str()).collect::<Vec<_>>(),
        ["1", "1", "3", "3", "5", "5"]
    );
}

#[test]
fn line_spacing_caps_at_eight_plies_and_table_only_matches() {
    let dir = tempfile::tempdir().unwrap();
    let path: Vec<_> = ["5i5h", "5a5b", "5h5i", "5b5a"].into_iter().cycle().take(40).collect();
    client(&dir.path().join("games.jsonl"), &moves_for(&path));
    master(&dir.path().join("moves.tsv"), &[], &[], &[]);
    let mut a = line_args(dir.path());
    a.games = vec![dir.path().join("games.jsonl")];
    a.depth = 20;
    run(a).unwrap();
    let table = std::fs::read(dir.path().join("table.tsv")).unwrap();
    assert_eq!(
        pave_table(dir.path()).iter().map(|r| r["ply"].as_str()).collect::<Vec<_>>(),
        ["1", "1", "9", "9", "17", "17", "25", "25", "33", "33"]
    );
    let mut a = line_args(dir.path());
    a.games = vec![dir.path().join("games.jsonl")];
    a.depth = 20;
    a.enqueue_out = None;
    a.tag = None;
    a.priority = None;
    run(a).unwrap();
    assert_eq!(std::fs::read(dir.path().join("table.tsv")).unwrap(), table);
}

#[test]
fn line_cli_parses_flags_and_rejects_invalid_side() {
    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        args: GameGapsArgs,
    }
    let base = [
        "test",
        "--master",
        "moves.tsv",
        "--games",
        "game.csa",
        "--table",
        "table.tsv",
        "--line",
        "--line-sides",
    ];
    for (text, side) in [
        ("both", LineSides::Both),
        ("loser", LineSides::Loser),
        ("winner", LineSides::Winner),
    ] {
        let parsed = Cli::try_parse_from(base.into_iter().chain([text])).unwrap().args;
        assert!(parsed.line);
        assert!(parsed.player.is_none());
        assert_eq!(parsed.line_sides, side);
        assert_eq!(parsed.collapse_cp, 150);
        assert_eq!(parsed.after_collapse, 3);
    }
    assert!(Cli::try_parse_from(base.into_iter().chain(["invalid"])).is_err());
    assert!(
        Cli::try_parse_from([
            "test",
            "--master",
            "moves.tsv",
            "--games",
            "game.csa",
            "--table",
            "table.tsv"
        ])
        .is_err()
    );
}

#[test]
fn line_white_winner_and_after_collapse_zero() {
    let dir = tempfile::tempdir().unwrap();
    line_fixture(dir.path());
    let mut a = line_args(dir.path());
    a.after_collapse = 0;
    run(a).unwrap();
    assert_eq!(
        pave_table(dir.path()).iter().map(|r| r["ply"].as_str()).collect::<Vec<_>>(),
        ["1", "1", "3", "3", "5"]
    );
    line_csa(
        &dir.path().join("line.csa"),
        &[
            Some(-400),
            Some(0),
            Some(-200),
            None,
            Some(-300),
            Some(-300),
            Some(-300),
            Some(-300),
            Some(-300),
            Some(-300),
        ],
        "%TORYO\n",
    );
    run(line_args(dir.path())).unwrap();
    let table = pave_table(dir.path());
    assert_eq!(
        table.iter().map(|r| r["ply"].as_str()).collect::<Vec<_>>(),
        ["1", "1", "3", "3", "5", "5"]
    );
    assert_eq!(
        table.iter().map(|r| r["end_reason"].as_str()).collect::<Vec<_>>(),
        ["collapse", "stop", "collapse", "stop", "collapse", "stop"]
    );
}

#[test]
fn line_signed_illegal_action_overrides_turn_but_not_summary() {
    let dir = tempfile::tempdir().unwrap();
    master(&dir.path().join("moves.tsv"), &[], &[], &[]);
    for (plies, terminal, winner) in [
        (9, "%+ILLEGAL_ACTION,T0\n", "w"),
        (10, "%-ILLEGAL_ACTION,T0\n", "b"),
        (9, "%+ILLEGAL_ACTION\n'summary:illegal:mine win:other lose\n", "b"),
        (10, "%-ILLEGAL_ACTION\n'summary:illegal:mine lose:other win\n", "w"),
    ] {
        line_csa(&dir.path().join("line.csa"), &vec![None; plies], terminal);
        let mut a = line_args(dir.path());
        a.line_sides = LineSides::Winner;
        run(a).unwrap();
        let q = queue(&dir.path().join("queue.jsonl"));
        assert!(!q.is_empty());
        assert!(q.iter().all(|row| row.own == winner));
    }
}

#[test]
fn line_player_is_optional_and_ignored() {
    let dir = tempfile::tempdir().unwrap();
    line_fixture(dir.path());
    run(line_args(dir.path())).unwrap();
    let expected = std::fs::read(dir.path().join("queue.jsonl")).unwrap();
    for player in [None, Some("absent".into()), Some(String::new())] {
        let mut a = line_args(dir.path());
        a.player = player;
        run(a).unwrap();
        assert_eq!(std::fs::read(dir.path().join("queue.jsonl")).unwrap(), expected);
    }
}

#[test]
fn line_tag_limit_is_checked_before_inputs() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = line_args(dir.path());
    a.tag = Some("x".repeat(17));
    assert!(run(a).unwrap_err().to_string().contains("16 文字"));
    assert!(!dir.path().join("table.tsv").exists());
    line_fixture(dir.path());
    let mut a = line_args(dir.path());
    a.tag = Some("x".repeat(16));
    run(a).unwrap();
    assert!(queue(&dir.path().join("queue.jsonl")).iter().all(|q| q.id.len() <= 64));
}

#[test]
fn line_collapse_tail_is_capped_at_game_end() {
    let dir = tempfile::tempdir().unwrap();
    line_fixture(dir.path());
    let mut a = line_args(dir.path());
    a.after_collapse = u32::MAX;
    a.line_sides = LineSides::Loser;
    a.depth = 3;
    run(a).unwrap();
    let table = pave_table(dir.path());
    assert_eq!(
        table.iter().map(|r| r["ply"].as_str()).collect::<Vec<_>>(),
        ["1", "4", "7", "10"]
    );
    assert!(table.iter().all(|r| r["own"] == "w" && r["end_reason"] == "collapse"));
    let q = queue(&dir.path().join("queue.jsonl"));
    assert_eq!(q.last().unwrap().game[0], MOVES[9..]);
}

#[test]
fn line_exclusion_only_covers_matching_own() {
    let dir = tempfile::tempdir().unwrap();
    line_fixture(dir.path());
    for own in ["b", "w"] {
        let excluded = dir.path().join("excluded.jsonl");
        write_json(
            &excluded,
            &[json!({"id":"accepted", "root":"startpos", "own":own,
            "depth":10, "game":[MOVES]})],
        );
        let mut a = line_args(dir.path());
        a.exclude_queue.push(excluded);
        run(a).unwrap();
        let q = queue(&dir.path().join("queue.jsonl"));
        assert_eq!(q.len(), 3);
        assert!(q.iter().all(|row| row.own != own));
        for row in pave_table(dir.path()) {
            assert_eq!(row["covered_by"], if row["own"] == own { "accepted" } else { "" });
        }
    }
}
