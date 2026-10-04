//! 実 YaneuraOu makebook peta_shock の値を固定した差分テスト。
//! fixture の生成手順・出典は fixtures/book_backprop_check.md を参照。
use tools::book_backprop::*;

#[test]
fn native_perpetual_check_matches_real_yaneuraou_values_and_preserves_metadata() {
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let input = fixtures.join("book_backprop_check_in.db");
    let expected = read_book_db(&fixtures.join("book_backprop_check_expected.db")).unwrap();
    let original = read_book_db(&input).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("out.db");
    for merge in [MergeMode::Replace, MergeMode::Min] {
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
                let expected_value = if merge == MergeMode::Min {
                    old.value.min(mv.value)
                } else {
                    mv.value
                };
                assert_eq!(row.value, expected_value, "{merge:?} {key} {:?}", mv.move_usi);
                assert_eq!(
                    (row.depth, row.count, &row.ponder_usi),
                    (old.depth, old.count, &old.ponder_usi)
                );
            }
        }
    }
}
