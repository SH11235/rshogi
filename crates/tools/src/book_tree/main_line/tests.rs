use super::super::master_table::{format_master, test_row};
use super::*;
use clap::Parser;
use tempfile::TempDir;

#[derive(Parser)]
struct Cli {
    #[command(flatten)]
    args: MainLineArgs,
}

/// 初期局面から `moves` を辿った局面に `role`・`evals` の行を置く。
fn row(moves: &[&str], mv: &str, role: &str, evals: &str) -> super::super::master_table::Row {
    let mut key = HIRATE_KEY.to_owned();
    for m in moves {
        key = child(&key, m).unwrap();
    }
    test_row(&[
        ("key", key),
        ("move", mv.into()),
        ("role", role.into()),
        ("evals", evals.into()),
    ])
}

fn run_with(dir: &TempDir, rows: &[super::super::master_table::Row], extra: &[&str]) -> String {
    let master = dir.path().join("moves.tsv");
    std::fs::write(&master, format_master(rows)).unwrap();
    let table = dir.path().join("line.tsv");
    let out = dir.path().join("line.jsonl");
    let mut argv = vec![
        "test".to_owned(),
        "--master".into(),
        master.display().to_string(),
        "--table".into(),
        table.display().to_string(),
        "--enqueue-out".into(),
        out.display().to_string(),
    ];
    argv.extend(extra.iter().map(|s| s.to_string()));
    let mut stdout = Vec::new();
    run_to(Cli::try_parse_from(argv).unwrap().args, &mut stdout).unwrap();
    String::from_utf8(stdout).unwrap()
}

fn lines(dir: &TempDir) -> Vec<Line> {
    std::fs::read_to_string(dir.path().join("line.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn missing_position_emits_bw_tree_and_black_view_values() {
    let d = TempDir::new().unwrap();
    let rows = [
        row(&[], "7g7f", "main", "30"),
        row(&[], "2g2f", "alt", "20"),
        row(&["7g7f"], "3c3d", "main", "-20"),
    ];
    let stdout = run_with(&d, &rows, &["--priority", "7", "--depth", "4"]);
    assert!(stdout.starts_with("2 手で切れる。理由: missing"), "{stdout}");
    assert!(stdout.contains("次は先手番"), "{stdout}");
    let table = std::fs::read_to_string(d.path().join("line.tsv")).unwrap();
    let cells: Vec<Vec<_>> = table.lines().map(|l| l.split('\t').collect()).collect();
    assert_eq!(cells[0], ["ply", "key", "move", "value_black", "reason"]);
    assert_eq!((cells[1][2], cells[1][3]), ("7g7f", "30"));
    assert_eq!((cells[2][2], cells[2][3]), ("3c3d", "20"));
    assert_eq!((cells[3][0], cells[3][4]), ("2", "missing"));
    let queued = lines(&d);
    assert_eq!(queued.len(), 1);
    let line = &queued[0];
    let key = child(&child(HIRATE_KEY, "7g7f").unwrap(), "3c3d").unwrap();
    assert_eq!(line.root, format!("sfen {key} 3"));
    assert_eq!((line.own.as_str(), line.depth, line.priority), ("bw", 4, 7));
    assert_eq!(line.memo.as_deref(), Some("main-line reason=missing moves=7g7f 3c3d"));
    assert_eq!(line.id, queue_id("mainline", 3, &key, "bw"));
}

#[test]
fn flipped_only_and_no_main_stop_with_tree_lines() {
    let d = TempDir::new().unwrap();
    let after = child(HIRATE_KEY, "7g7f").unwrap();
    let flipped =
        crate::book_db::strip_ply(&rshogi_book::flipped_key(&format!("{after} 1")).unwrap())
            .to_owned();
    let mut mirror = test_row(&[
        ("key", flipped),
        ("move", "7g7f".into()),
        ("role", "main".into()),
        ("evals", String::new()),
    ]);
    mirror.set("evals", String::new());
    let stdout = run_with(&d, &[row(&[], "7g7f", "main", ""), mirror], &[]);
    assert!(stdout.starts_with("1 手で切れる。理由: flipped-only"), "{stdout}");
    assert_eq!(lines(&d)[0].memo.as_deref(), Some("main-line reason=flipped-only moves=7g7f"));

    let d = TempDir::new().unwrap();
    let rows = [
        row(&[], "7g7f", "main", ""),
        row(&["7g7f"], "3c3d", "alt", ""),
        row(&["7g7f"], "8c8d", "record", ""),
    ];
    let stdout = run_with(&d, &rows, &[]);
    assert!(
        stdout.contains("理由: no-main (局面はあるが main が無い (role: alt,record))"),
        "{stdout}"
    );
    assert_eq!(lines(&d).len(), 1);
}

#[test]
fn cycle_and_max_ply_write_no_tree_line() {
    let d = TempDir::new().unwrap();
    let path = ["5i5h", "5a5b", "5h5i", "5b5a"];
    let rows: Vec<_> = (0..4).map(|i| row(&path[..i], path[i], "main", "")).collect();
    let stdout = run_with(&d, &rows, &[]);
    assert!(stdout.starts_with("4 手で切れる。理由: cycle"), "{stdout}");
    assert!(lines(&d).is_empty());

    let d = TempDir::new().unwrap();
    let stdout = run_with(&d, &rows, &["--max-ply", "2"]);
    assert!(stdout.starts_with("2 手で切れる。理由: max-ply"), "{stdout}");
    assert!(stdout.contains("moves=5i5h 5a5b"), "{stdout}");
    assert!(lines(&d).is_empty());
}

#[test]
fn outputs_must_differ_from_inputs() {
    let d = TempDir::new().unwrap();
    let master = d.path().join("moves.tsv");
    std::fs::write(&master, format_master(&[row(&[], "7g7f", "main", "")])).unwrap();
    let m = master.display().to_string();
    for extra in [["--table", m.as_str()], ["--enqueue-out", m.as_str()]] {
        let argv = ["test", "--master", m.as_str()].into_iter().chain(extra);
        let args = Cli::try_parse_from(argv).unwrap().args;
        assert!(run_to(args, &mut Vec::new()).is_err());
    }
}
