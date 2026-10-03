//! YANEURAOU-DB2016 テキスト `.db` の評価値を negamax 逆伝播するツール。
//!
//! 処理本体は `tools::book_backprop` にあり、`book_mine run` と共有する。

use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::Parser;
use tools::book_backprop::*;

#[derive(Parser, Debug)]
#[command(about = "YANEURAOU-DB2016 テキスト定跡 .db の評価値を negamax 逆伝播する")]
struct Cli {
    #[arg(long)]
    book: PathBuf,
    #[arg(long)]
    out: PathBuf,
    #[arg(long, default_value_t = 0)]
    draw_value: i32,
    #[arg(long)]
    report: Option<PathBuf>,
    #[arg(long, default_value_t = 1000)]
    max_iters: usize,
    #[arg(long, value_enum, default_value_t = MergeMode::Min)]
    merge: MergeMode,
    /// 非合法手と `none` 行を局面の best から除く (行は値を変えずに書き出す)
    #[arg(long, default_value_t = false)]
    skip_unusable_moves: bool,
    /// 全候補手の depth が 0 の子局面を book 外として扱い、親の手の値を保持する (候補手無しは除外しない)
    #[arg(long, default_value_t = false)]
    skip_unsearched_children: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let report: Option<&Path> = cli.report.as_deref();
    let options = BackpropOptions {
        skip_unusable_moves: cli.skip_unusable_moves,
        skip_unsearched_children: cli.skip_unsearched_children,
    };
    backprop_file_with(
        &cli.book,
        &cli.out,
        report,
        cli.draw_value,
        cli.max_iters,
        cli.merge,
        options,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    const START: &str = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
    const KINGS: &str = "4k4/9/9/9/9/9/9/9/4K4 b - 1";

    type FixtureMove<'a> = (&'a str, i32, i32, u64);
    type FixtureEntry<'a> = (&'a str, &'a [FixtureMove<'a>]);

    #[test]
    fn skip_unsearched_children_is_opt_in() {
        let args = ["book_backprop", "--book", "in.db", "--out", "out.db"];
        assert!(!Cli::try_parse_from(args).unwrap().skip_unsearched_children);
        assert!(
            Cli::try_parse_from(args.into_iter().chain(["--skip-unsearched-children"]))
                .unwrap()
                .skip_unsearched_children
        );
    }

    #[test]
    fn dead_piece_edges_are_excluded_even_when_child_exists() {
        // 不正着手の結果に相当する子SFENも配置し、辺を除外しないと値が変わる条件にする。
        for (parent, usi, child) in [
            ("4k4/9/9/9/9/9/9/9/4K4 b P 1", "P*4a", "4kP3/9/9/9/9/9/9/9/4K4 w - 2"),
            ("4k4/9/9/9/9/9/9/9/4K4 b L 1", "L*4a", "4kL3/9/9/9/9/9/9/9/4K4 w - 2"),
            ("4k4/9/9/9/9/9/9/9/4K4 b N 1", "N*4a", "4kN3/9/9/9/9/9/9/9/4K4 w - 2"),
            ("4k4/9/9/9/9/9/9/9/4K4 b N 1", "N*4b", "4k4/5N3/9/9/9/9/9/9/4K4 w - 2"),
            ("4k4/5P3/9/9/9/9/9/9/4K4 b - 1", "4b4a", "4kP3/9/9/9/9/9/9/9/4K4 w - 2"),
            ("4k4/5L3/9/9/9/9/9/9/4K4 b - 1", "4b4a", "4kL3/9/9/9/9/9/9/9/4K4 w - 2"),
            ("4k4/9/4N4/9/9/9/9/9/4K4 b - 1", "5c4a", "4kN3/9/9/9/9/9/9/9/4K4 w - 2"),
            ("4k4/9/9/4N4/9/9/9/9/4K4 b - 1", "5d4b", "4k4/5N3/9/9/9/9/9/9/4K4 w - 2"),
        ] {
            for flip in [false, true] {
                let (parent, usi, child) = if flip {
                    (
                        rshogi_book::flipped_key(parent).unwrap(),
                        rshogi_book::flip_usi_move(usi).unwrap(),
                        rshogi_book::flipped_key(child).unwrap(),
                    )
                } else {
                    (parent.into(), usi.into(), child.into())
                };
                let input = line_book(&[
                    (&parent, &[(&usi, 123, 9, 7)]),
                    (&child, &[("none", 500, 1, 1)]),
                ]);
                let dir = tempdir().unwrap();
                let input_path = write_input(dir.path(), "in.db", &input);
                let book = read_book_db(&input_path).unwrap();
                for merge in [MergeMode::Min, MergeMode::Replace] {
                    let mut graph = build_graph(&book).unwrap();
                    assert_eq!(graph.illegal_moves, 1, "{parent}: {usi}");
                    let parent_idx =
                        graph.keys.iter().position(|key| key == strip_ply(&parent)).unwrap();
                    assert!(graph.moves[parent_idx][0].edge.is_none());
                    assert!(graph.adjacency[parent_idx].is_empty());
                    propagate_values(&book, &mut graph, 0, 1000, merge).unwrap();
                    assert_eq!(graph.moves[parent_idx][0].new, 123);
                    let out = dir.path().join("out.db");
                    write_backprop_book(&book, &graph, &out).unwrap();
                    assert!(
                        std::fs::read_to_string(out)
                            .unwrap()
                            .contains(&format!("{usi} none 123 9 7\n"))
                    );
                }
            }
        }
    }

    #[test]
    fn legal_non_promotion_still_propagates() {
        let parent = "4k4/9/9/5P3/9/9/9/9/4K4 b - 1";
        let child = after(parent, &["4d4c"]);
        let input = line_book(&[
            (parent, &[("4d4c", 123, 9, 7)]),
            (&child, &[("none", 500, 1, 1)]),
        ]);
        assert!(backprop_text(&input).contains("4d4c none -500 9 7\n"));
    }
    #[test]
    fn output_is_deterministic_byte_for_byte() {
        let input = line_book(&[
            (START, &[("7g7f", 0, 1, 10), ("2g2f", 0, 1, 10)]),
            (&after(START, &["7g7f"]), &[("3c3d", 15, 1, 1)]),
        ]);
        let dir = tempdir().unwrap();
        let in_path = write_input(dir.path(), "in.db", &input);
        let out1 = dir.path().join("a.db");
        let out2 = dir.path().join("b.db");

        run_backprop(&in_path, &out1, 0, 1000, MergeMode::Min).unwrap();
        run_backprop(&in_path, &out2, 0, 1000, MergeMode::Min).unwrap();

        assert_eq!(std::fs::read(&out1).unwrap(), std::fs::read(&out2).unwrap());
    }

    #[test]
    fn straight_line_propagates_with_negamax_signs() {
        let after_76 = after(START, &["7g7f"]);
        let after_76_34 = after(START, &["7g7f", "3c3d"]);
        let input = line_book(&[
            (START, &[("7g7f", 0, 1, 1)]),
            (&after_76, &[("3c3d", 0, 1, 1)]),
            (&after_76_34, &[("2g2f", 80, 1, 1)]),
        ]);
        let output = backprop_text_with_merge(&input, MergeMode::Replace);

        assert!(output.contains("sfen "));
        assert!(output.contains("7g7f none 80 1 1\n"));
        assert!(output.contains("3c3d none -80 1 1\n"));
        assert!(output.contains("2g2f none 80 1 1\n"));
    }

    #[test]
    fn transposed_child_updates_both_parents() {
        let p1 = after(START, &["7g7f", "3c3d"]);
        let p2 = after(START, &["2g2f", "3c3d"]);
        let child = after(START, &["7g7f", "3c3d", "2g2f"]);
        let input = line_book(&[
            (&p1, &[("2g2f", 0, 1, 4)]),
            (&p2, &[("7g7f", 0, 1, 4)]),
            (&child, &[("8c8d", 42, 1, 1)]),
        ]);
        let output = backprop_text(&input);

        assert!(output.contains("2g2f none -42 1 4\n"));
        assert!(output.contains("7g7f none -42 1 4\n"));
    }

    #[test]
    fn flipped_child_key_is_used_when_direct_key_misses() {
        let child = child_position_after_move(START, "7g7f").unwrap().to_sfen();
        let flipped = rshogi_book::flipped_key(&child).unwrap();
        let input = line_book(&[
            (START, &[("7g7f", 0, 1, 1)]),
            (&flipped, &[("7g7f", 33, 1, 1)]),
        ]);
        let output = backprop_text(&input);

        assert!(output.contains("sfen "));
        assert!(output.contains("7g7f none -33 1 1\n"));
    }

    #[test]
    fn min_merge_keeps_existing_value_when_propagation_would_raise_it() {
        let after_76 = after(START, &["7g7f"]);
        let input = line_book(&[
            (START, &[("7g7f", -81, 1, 1)]),
            (&after_76, &[("3c3d", 53, 1, 1)]),
        ]);
        let output = backprop_text(&input);

        assert!(output.contains("7g7f none -81 1 1\n"));
    }

    #[test]
    fn min_merge_lowers_existing_value_when_propagation_is_lower() {
        let after_76 = after(START, &["7g7f"]);
        let input = line_book(&[
            (START, &[("7g7f", 10, 1, 1)]),
            (&after_76, &[("3c3d", 50, 1, 1)]),
        ]);
        let output = backprop_text(&input);

        assert!(output.contains("7g7f none -50 1 1\n"));
    }

    #[test]
    fn cycle_uses_exit_above_draw_and_draw_when_exit_is_below_draw() {
        let above = cycle_book(20);
        let above_out = backprop_text(&above);
        assert!(above_out.contains("5a4a none 20 1 2\n"));
        assert!(above_out.contains("5a5b none 0 1 3\n"));

        let below = cycle_book(-20);
        let below_out = backprop_text(&below);
        assert!(below_out.contains("5a4a none -20 1 2\n"));
        assert!(below_out.contains("5a5b none 0 1 3\n"));
    }

    #[test]
    fn leaf_value_is_preserved() {
        let input = line_book(&[(START, &[("7g7f", 123, 9, 1)])]);
        let output = backprop_text(&input);

        assert!(output.contains("7g7f none 123 9 1\n"));
    }

    #[test]
    fn output_roundtrips_through_book_reader() {
        let input = line_book(&[
            (START, &[("7g7f", 0, 1, 1)]),
            (&after(START, &["7g7f"]), &[("3c3d", 15, 1, 1)]),
        ]);
        let dir = tempdir().unwrap();
        let in_path = write_input(dir.path(), "in.db", &input);
        let out_path = dir.path().join("out.db");

        run_backprop(&in_path, &out_path, 0, 1000, MergeMode::Min).unwrap();
        let book = rshogi_book::Book::from_path(&out_path, true).unwrap();

        assert_eq!(book.len(), 2);
    }

    fn run_backprop(
        input: &Path,
        output: &Path,
        draw_value: i32,
        max_iters: usize,
        merge: MergeMode,
    ) -> Result<()> {
        let book = read_book_db(input)?;
        let mut graph = build_graph(&book)?;
        propagate_values(&book, &mut graph, draw_value, max_iters, merge)?;
        write_backprop_book(&book, &graph, output)
    }

    fn backprop_text(input: &str) -> String {
        backprop_text_with_merge(input, MergeMode::Min)
    }

    fn backprop_text_with_merge(input: &str, merge: MergeMode) -> String {
        let dir = tempdir().unwrap();
        let in_path = write_input(dir.path(), "in.db", input);
        let out_path = dir.path().join("out.db");
        run_backprop(&in_path, &out_path, 0, 1000, merge).unwrap();
        std::fs::read_to_string(out_path).unwrap()
    }

    fn write_input(dir: &Path, name: &str, text: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, text).unwrap();
        path
    }

    fn line_book(entries: &[FixtureEntry<'_>]) -> String {
        let mut out = String::from(BOOK_HEADER);
        out.push('\n');
        for (sfen, moves) in entries {
            out.push_str("sfen ");
            out.push_str(sfen);
            out.push('\n');
            for (move_usi, value, depth, count) in *moves {
                out.push_str(&format!("{move_usi} none {value} {depth} {count}\n"));
            }
        }
        out
    }

    fn after(start: &str, moves: &[&str]) -> String {
        let mut sfen = start.to_string();
        for move_usi in moves {
            sfen = child_position_after_move(&sfen, move_usi).unwrap().to_sfen();
        }
        sfen
    }

    fn cycle_book(exit_value: i32) -> String {
        let a = KINGS.to_string();
        let b = after(&a, &["5i5h"]);
        let c = after(&a, &["5i5h", "5a5b"]);
        let d = after(&a, &["5i5h", "5a5b", "5h5i"]);
        line_book(&[
            (&a, &[("5i5h", 0, 1, 3)]),
            (&b, &[("5a5b", 0, 1, 3), ("5a4a", exit_value, 1, 2)]),
            (&c, &[("5h5i", 0, 1, 3)]),
            (&d, &[("5b5a", 0, 1, 3)]),
        ])
    }
}
