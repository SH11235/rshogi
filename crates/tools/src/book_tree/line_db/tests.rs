use super::*;
use crate::book_db::{BookDb, position_from_sfen};
use crate::book_tree::master_table::{COLUMNS, test_line};

fn after(moves: &str) -> String {
    let mut pos = position_from_sfen(SFEN_HIRATE).unwrap();
    for mv in moves.split_whitespace() {
        crate::book_db::apply_usi_move(&mut pos, mv).unwrap();
    }
    pos.to_sfen()
}

fn row(sfen: &str, mv: &str, role: &str, eval: &str) -> String {
    test_line(&[
        ("key", strip_ply(sfen).into()),
        ("ply", sfen.split_whitespace().last().unwrap().into()),
        ("move", mv.into()),
        ("role", role.into()),
        ("source", "read".into()),
        ("evals", eval.into()),
    ])
}

fn tree(dir: &Path, root: &str, entries: &[(&str, &str)]) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("STATUS"), "DONE\n").unwrap();
    std::fs::write(
        dir.join("d0.jsonl"),
        format!("{}\n", serde_json::json!({"key": strip_ply(root)})),
    )
    .unwrap();
    let mut db = format!("{BOOK_HEADER}\n");
    for (sfen, moves) in entries {
        db.push_str(&format!("sfen {sfen}\n{moves}\n"));
    }
    std::fs::write(dir.join("tree.db"), db).unwrap();
}

fn args(dir: &Path) -> LineDbArgs {
    LineDbArgs {
        master: dir.join("moves.tsv"),
        moves: "7g7f 3c3d".into(),
        tree_out: vec![dir.join("trees")],
        out: dir.join("line.db"),
        schema: None,
        draw_value_black: -70,
        draw_value_white: 70,
        force: false,
    }
}

#[test]
fn combines_master_trees_and_successor_roots_then_uses_replace() {
    let dir = tempfile::tempdir().unwrap();
    let a = args(dir.path());
    let root = after(&a.moves);
    let child = after("7g7f 3c3d 2g2f");
    let master_only = after("7g7f 3c3d 6g6f");
    let tree_only = after("7g7f 3c3d 2g2f 4c4d");
    let next_root = after("7g7f 3c3d 2g2f 4c4d 2f2e");
    let shared = after("7g7f 3c3d 2g2f 8c8d");
    let rows = [
        row(&root, "2g2f", "main", "100,200"),
        row(&root, "6g6f", "alt", "30"),
        row(&root, "5g5f", "record", "900"),
        row(&root, "4g4f", "bench", "800"),
        row(&child, "8c8d", "main", "60"),
        row(&master_only, "8c8d", "main", "55"),
        row(&shared, "2f2e", "main", "80"),
        row(&shared, "7f7e", "alt", "90"),
    ];
    std::fs::write(&a.master, format!("{}\n{}\n", COLUMNS.join("\t"), rows.join("\n"))).unwrap();
    let first = a.tree_out[0].join("box/a/g0");
    let second = a.tree_out[0].join("box/b/g0");
    tree(
        &first,
        &child,
        &[
            (&child, "8c8d none 999 12 1\n4c4d none -20 12 1\n1a1a none 9999 12 1"),
            (&shared, "2f2e none 999 12 1"),
            (&tree_only, "2f2e none 45 12 1"),
        ],
    );
    tree(&second, &next_root, &[(&next_root, "8c8d none -123 12 1")]);
    let unused = a.tree_out[0].join("unused");
    tree(&unused, SFEN_HIRATE, &[(SFEN_HIRATE, "5g5f none 42 1 1")]);
    let unfinished = a.tree_out[0].join("unfinished");
    std::fs::create_dir_all(&unfinished).unwrap();
    std::fs::write(unfinished.join("STATUS"), "RUNNING\n").unwrap();
    let mut inputs = vec![];
    let trees = discover(&a.tree_out, &mut inputs).unwrap();
    let mut index = MasterIndex::open(&a.master, &Schema::load(None).unwrap()).unwrap();
    let (selected, _, used) = assemble(&a, &mut index, trees).unwrap();
    assert_eq!(used, [first.clone(), second.clone()]);
    assert_eq!(selected[strip_ply(&root)].moves["2g2f"].mv.value, 100);
    assert_eq!(selected[strip_ply(&root)].moves["2g2f"].mv.count, 2);
    assert_eq!(selected[strip_ply(&root)].moves["6g6f"].mv.count, 1);
    assert_eq!(selected[strip_ply(&child)].moves["8c8d"].mv.value, 60);
    assert_eq!(selected[strip_ply(&shared)].moves["2f2e"].mv.value, 80);
    assert!(!selected[strip_ply(&child)].moves.contains_key("1a1a"));
    let raw = dir.path().join("expected-raw.db");
    write_raw(&selected, &mut File::create(&raw).unwrap()).unwrap();
    let expected = dir.path().join("expected.db");
    backprop_file_with(
        &raw,
        &expected,
        None,
        DrawValues {
            black: -70,
            white: 70,
        },
        MergeMode::Replace,
        BackpropOptions {
            skip_unusable_moves: true,
            ..BackpropOptions::default()
        },
    )
    .unwrap();
    let mut stdout = Vec::new();
    run_to(a, &mut stdout).unwrap();
    let output = dir.path().join("line.db");
    assert_eq!(std::fs::read(&output).unwrap(), std::fs::read(expected).unwrap());
    let db = BookDb::read(&output).unwrap();
    assert_eq!(db.entries[strip_ply(SFEN_HIRATE)].moves.len(), 1);
    assert_eq!(db.entries[strip_ply(SFEN_HIRATE)].moves[0].move_usi.as_deref(), Some("7g7f"));
    let prefix = after("7g7f");
    assert_eq!(db.entries[strip_ply(&prefix)].moves.len(), 1);
    assert_eq!(db.entries[strip_ply(&prefix)].moves[0].move_usi.as_deref(), Some("3c3d"));
    for sfen in [&master_only, &tree_only, &next_root] {
        assert!(db.entries.contains_key(strip_ply(sfen)));
    }
    assert_eq!(db.entries[strip_ply(&root)].moves.len(), 2);
    let stdout = String::from_utf8(stdout).unwrap();
    assert!(stdout.contains(&first.display().to_string()));
    assert!(stdout.contains(&second.display().to_string()));
    assert!(!stdout.contains(&unused.display().to_string()));
    for mv in &db.entries[strip_ply(&root)].moves {
        assert!(stdout.contains(&format!("根\t{}\t{}", mv.move_usi.as_deref().unwrap(), mv.value)));
    }
    assert!(run_to(args(dir.path()), &mut Vec::new()).is_err());
    let mut force = args(dir.path());
    force.force = true;
    run_to(force, &mut Vec::new()).unwrap();
}

#[test]
fn master_row_with_empty_ply_still_wins_over_tree() {
    let dir = tempfile::tempdir().unwrap();
    let a = args(dir.path());
    let root = after(&a.moves);
    let line = row(&root, "2g2f", "main", "100")
        .replace(&format!("\t{}\t", root.split_whitespace().last().unwrap()), "\t\t");
    std::fs::write(&a.master, format!("{}\n{line}\n", COLUMNS.join("\t"))).unwrap();
    tree(&a.tree_out[0].join("t"), &root, &[(&root, "2g2f none 999 12 1")]);
    let mut inputs = vec![];
    let trees = discover(&a.tree_out, &mut inputs).unwrap();
    let mut index = MasterIndex::open(&a.master, &Schema::load(None).unwrap()).unwrap();
    let (selected, _, _) = assemble(&a, &mut index, trees).unwrap();
    assert_eq!(selected.len(), 3);
    assert_eq!(selected[strip_ply(&root)].moves["2g2f"].mv.value, 100);
}

#[test]
fn row_sfen_falls_back_to_ply_one_when_ply_is_unreadable() {
    let key = strip_ply(SFEN_HIRATE);
    assert_eq!(row_sfen(key, "7"), format!("{key} 7"));
    assert_eq!(row_sfen(key, ""), format!("{key} 1"));
    assert_eq!(row_sfen(key, "99999999999999999999"), format!("{key} 1"));
}

#[test]
fn newer_tree_wins_even_when_read_before_older_tree() {
    let dir = tempfile::tempdir().unwrap();
    let mut positions = Positions::new();
    let mut pending = VecDeque::new();
    let old = TreeInput {
        dir: dir.path().join("old"),
        modified: SystemTime::UNIX_EPOCH,
    };
    let new = TreeInput {
        dir: dir.path().join("new"),
        modified: SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1),
    };
    for (tree, value) in [(&new, 200), (&old, 100)] {
        insert_move(
            &mut positions,
            &mut pending,
            &Prefix::new(),
            SFEN_HIRATE,
            parse_move_line(&format!("7g7f none {value} 10 1")).unwrap(),
            Some(tree),
        )
        .unwrap();
    }
    assert_eq!(positions[strip_ply(SFEN_HIRATE)].moves["7g7f"].mv.value, 200);
    insert_move(
        &mut positions,
        &mut pending,
        &Prefix::new(),
        SFEN_HIRATE,
        parse_move_line("7g7f none 30 0 2").unwrap(),
        None,
    )
    .unwrap();
    insert_move(
        &mut positions,
        &mut pending,
        &Prefix::new(),
        SFEN_HIRATE,
        parse_move_line("7g7f none 400 10 1").unwrap(),
        Some(&new),
    )
    .unwrap();
    assert_eq!(positions[strip_ply(SFEN_HIRATE)].moves["7g7f"].mv.value, 30);
}

#[test]
fn prefix_position_in_tree_is_not_expanded_and_bad_parent_sfen_fails() {
    let mut positions = Positions::new();
    let mut pending = VecDeque::new();
    let mut prefix = Prefix::new();
    let mv = parse_move_line("2g2f none 0 0 1").unwrap();
    prefix.insert(strip_ply(SFEN_HIRATE).to_owned(), (SFEN_HIRATE.to_owned(), mv.clone()));
    insert_move(&mut positions, &mut pending, &prefix, SFEN_HIRATE, mv.clone(), None).unwrap();
    assert!(positions.is_empty());
    assert!(pending.is_empty());

    let err =
        insert_move(&mut positions, &mut pending, &Prefix::new(), "not-a-sfen b - 1", mv, None);
    assert!(err.is_err());
    let illegal = parse_move_line("1a1b none 0 0 1").unwrap();
    insert_move(&mut positions, &mut pending, &Prefix::new(), SFEN_HIRATE, illegal, None).unwrap();
    assert!(positions.is_empty());
}

#[test]
fn rejects_input_collision_including_unused_tree_and_hardlink() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = args(dir.path());
    std::fs::write(&a.master, format!("{}\n", COLUMNS.join("\t"))).unwrap();
    let unused = a.tree_out[0].join("unused");
    tree(&unused, SFEN_HIRATE, &[]);
    a.force = true;
    a.out = unused.join("tree.db");
    assert!(run_to(a, &mut Vec::new()).unwrap_err().to_string().contains("input"));
    let mut a = args(dir.path());
    a.force = true;
    std::fs::hard_link(&a.master, &a.out).unwrap();
    assert!(run_to(a, &mut Vec::new()).unwrap_err().to_string().contains("input"));
}

#[test]
fn cli_accepts_multiple_tree_roots_and_negative_draw_values() {
    use clap::Parser;
    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        args: LineDbArgs,
    }
    let cli = Cli::try_parse_from([
        "line-db",
        "--master",
        "moves.tsv",
        "--moves",
        "",
        "--out",
        "line.db",
        "--tree-out",
        "a",
        "b",
        "--tree-out",
        "c",
        "--draw-value-black",
        "-120",
    ])
    .unwrap();
    assert_eq!(cli.args.tree_out, [PathBuf::from("a"), PathBuf::from("b"), PathBuf::from("c")]);
    assert_eq!(cli.args.draw_value_black, -120);
    assert_eq!(cli.args.draw_value_white, 70);
}

#[test]
fn white_root_summary_and_schema_validation() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = args(dir.path());
    a.moves = "7g7f".into();
    std::fs::create_dir_all(&a.tree_out[0]).unwrap();
    let root = after(&a.moves);
    std::fs::write(
        &a.master,
        format!("{}\n{}\n", COLUMNS.join("\t"), row(&root, "3c3d", "main", "42")),
    )
    .unwrap();
    let mut stdout = Vec::new();
    run_to(a, &mut stdout).unwrap();
    assert!(String::from_utf8(stdout).unwrap().contains("根\t3c3d\t-42"));
    let schema = dir.path().join("schema");
    std::fs::create_dir(&schema).unwrap();
    super::super::schema::test_schema(&schema);
    std::fs::write(schema.join("sources.tsv"), "source\tdescription\treplaceable\nread\t読み\t1\n")
        .unwrap();
    let mut a = args(dir.path());
    a.schema = Some(schema);
    a.force = true;
    let text = std::fs::read_to_string(&a.master).unwrap().replace("\tread\t", "\tundefined\t");
    std::fs::write(&a.master, text).unwrap();
    let before = std::fs::read(&a.out).unwrap();
    assert!(run_to(a, &mut Vec::new()).unwrap_err().to_string().contains("unknown source"));
    assert_eq!(before, std::fs::read(dir.path().join("line.db")).unwrap());
}
