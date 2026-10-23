//! 実 YaneuraOu makebook peta_shock の値を固定した差分テスト。
//! fixture の生成手順・出典は fixtures/book_backprop_check.md を参照。
use tools::book_backprop::*;

fn assert_matches_yaneuraou(name: &str, merges: &[MergeMode]) {
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let input = fixtures.join(format!("{name}_in.db"));
    let expected = read_book_db(&fixtures.join(format!("{name}_expected.db"))).unwrap();
    let original = read_book_db(&input).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("out.db");
    for &merge in merges {
        backprop_file(&input, &output, None, 0, 1000, merge).unwrap();
        let actual = read_book_db(&output).unwrap();
        assert_eq!(actual.entries.len(), expected.entries.len());
        for (key, entry) in &expected.entries {
            let rows = &actual.entries[key].moves;
            assert_eq!(rows.len(), entry.moves.len());
            for mv in &entry.moves {
                let row = rows.iter().find(|r| r.move_usi == mv.move_usi).unwrap();
                let old =
                    original.entries[key].moves.iter().find(|r| r.move_usi == mv.move_usi).unwrap();
                // 入力ラベルが伝播値を抑えない fixture では、min の出力は min(入力, YO の値) になる。
                let expected_value = if merge == MergeMode::Min {
                    old.value.min(mv.value)
                } else {
                    mv.value
                };
                assert_eq!(row.value, expected_value, "{name} {merge:?} {key} {:?}", mv.move_usi);
                assert_eq!(
                    (row.depth, row.count, &row.ponder_usi),
                    (old.depth, old.count, &old.ponder_usi)
                );
            }
        }
    }
}

#[test]
fn native_perpetual_check_matches_real_yaneuraou_values_and_preserves_metadata() {
    assert_matches_yaneuraou("book_backprop_check", &[MergeMode::Replace, MergeMode::Min]);
}

#[test]
fn cycle_through_root_with_nonzero_exit_matches_real_yaneuraou_values() {
    // min は循環内で子の値を入力ラベルで抑えてから親へ伝えるので、出力が min(入力, YO の値) にならない。
    assert_matches_yaneuraou("book_backprop_cycle_exit", &[MergeMode::Replace]);
}
