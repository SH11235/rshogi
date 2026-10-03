//! 外部 YO の実出力との value/depth 差分検証。大きい DB は repo に含めない。
use std::collections::BTreeMap;
use std::path::Path;

use tools::book_backprop::{BackpropOptions, MergeMode, backprop_file_with, read_book_db};

fn rows(path: &Path) -> BTreeMap<(String, String), (i32, i32)> {
    let book = read_book_db(path).unwrap();
    let mut rows = BTreeMap::new();
    for (key, entry) in book.entries {
        for mv in entry.moves {
            let id = (key.clone(), mv.move_usi.unwrap_or_else(|| "none".into()));
            assert!(rows.insert(id, (mv.value, mv.depth)).is_none(), "重複した局面・手");
        }
    }
    rows
}

fn compare(input: &Path, reference: &Path) {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("out_rs.db");
    backprop_file_with(
        input,
        &output,
        None,
        0,
        1000,
        MergeMode::Replace,
        BackpropOptions {
            yo_compat: true,
            ..BackpropOptions::default()
        },
    )
    .unwrap();
    let yo = rows(reference);
    let rs = rows(&output);
    let mut value_equal = 0;
    let mut depth_equal = 0;
    let mut both_equal = 0;
    let mut differences = Vec::new();
    for (key, &(value, depth)) in &yo {
        let actual = rs.get(key);
        value_equal += usize::from(actual.is_some_and(|v| v.0 == value));
        depth_equal += usize::from(actual.is_some_and(|v| v.1 == depth));
        both_equal += usize::from(actual == Some(&(value, depth)));
        if actual != Some(&(value, depth)) && differences.len() < 10 {
            differences.push(format!("{key:?}: YO=({value}, {depth}) Rust={actual:?}"));
        }
    }
    println!(
        "moves total: YO={}, Rust={}; value equal={value_equal}; depth equal={depth_equal}; both equal={both_equal}",
        yo.len(),
        rs.len()
    );
    assert_eq!(yo.len(), rs.len(), "出力の手集合の件数が異なる");
    assert_eq!(both_equal, yo.len(), "{}", differences.join("\n"));
}

#[test]
fn real_yaneuraou_leaf_distance_and_convergence() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    compare(
        &fixture.join("book_backprop_yo_in.db"),
        &fixture.join("book_backprop_yo_expected.db"),
    );
}

#[test]
#[ignore = "YO_BOOK_INPUT / YO_BOOK_REFERENCE に外部 DB を指定する実機差分検証"]
fn external_yaneuraou_reference() {
    let input = std::env::var_os("YO_BOOK_INPUT").expect("YO_BOOK_INPUT が必要です");
    let reference = std::env::var_os("YO_BOOK_REFERENCE").expect("YO_BOOK_REFERENCE が必要です");
    compare(Path::new(&input), Path::new(&reference));
}
