//! 大会の公式棋譜 (WCSC の CSA、電竜戦の KIF) を `kifu_publish` で正規化・変換した CSA が、
//! 取り込み (`import_upload`) を通ることの確認。
//!
//! 入力は `crates/tools/tests/golden/kifu_publish/` の CSA。これは
//! `crates/tools/tests/fixtures/kifu_publish/` の公式棋譜 (各形式の短い 1 局) を
//! `kifu_publish` が変換した結果で、tools 側のテストが一致を確かめている。

use std::collections::BTreeMap;
use std::path::Path;

use rshogi_csa_server_workers::kifu_import::{ImportedClock, ImportedGame, import_upload};

const FISCHER_15MIN_5SEC: Option<ImportedClock> = Some(ImportedClock::Fischer {
    total_sec: 900,
    increment_sec: 5,
});
const FISCHER_10MIN_2SEC: Option<ImportedClock> = Some(ImportedClock::Fischer {
    total_sec: 600,
    increment_sec: 2,
});

fn import(csa: &str) -> ImportedGame {
    import_upload(csa, Some("test-event")).unwrap_or_else(|e| panic!("import failed: {e}"))
}

#[test]
fn wcsc31_time_lines_joined_to_moves_are_imported() {
    let game = import(include_str!("../../../tools/tests/golden/kifu_publish/wcsc31.csa"));
    assert_eq!((game.black.as_str(), game.white.as_str()), ("TMOQ", "十六式いろは改三"));
    assert_eq!((game.result_kind, game.end_reason), ("WIN_BLACK", "RESIGN"));
    assert_eq!(game.moves_count, 19);
    // 持ち時間の記載が無い棋譜には付けない。
    assert_eq!(game.clock, None);
    // `$END_TIME` が無いので、開始時刻 + 消費時間の合計 (146 秒)。
    assert_eq!(game.ended_at_ms - game.started_at_ms, 146_000);
}

#[test]
fn wcsc33_and_wcsc34_board_position_and_rewritten_clock_are_imported() {
    let game = import(include_str!("../../../tools/tests/golden/kifu_publish/wcsc33.csa"));
    assert_eq!((game.black.as_str(), game.white.as_str()), ("あうあう将棋", "重力場計算法"));
    // 5 手指して後手が投了。
    assert_eq!((game.result_kind, game.end_reason), ("WIN_BLACK", "RESIGN"));
    assert_eq!(game.moves_count, 5);
    assert_eq!(game.clock, FISCHER_15MIN_5SEC);
    assert_eq!(game.ended_at_ms - game.started_at_ms, 9_000);

    let game = import(include_str!("../../../tools/tests/golden/kifu_publish/wcsc34.csa"));
    assert_eq!((game.result_kind, game.end_reason), ("WIN_BLACK", "RESIGN"));
    assert_eq!(game.moves_count, 33);
    assert_eq!(game.clock, FISCHER_15MIN_5SEC);
}

#[test]
fn wcsc35_csa_v3_headers_are_imported() {
    // `V3.0`、`$TIME:` / `$MAX_MOVES:` / `$JISHOGI:` / `$SITE:`、盤面 `P1..P9`、
    // `'** <評価値>` コメント。
    let game = import(include_str!("../../../tools/tests/golden/kifu_publish/wcsc35.csa"));
    assert_eq!(
        (game.black.as_str(), game.white.as_str()),
        ("習甦", "十六式いろは煌（きらめき）")
    );
    assert_eq!((game.result_kind, game.end_reason), ("WIN_BLACK", "RESIGN"));
    assert_eq!(game.moves_count, 29);
    assert_eq!(game.clock, FISCHER_15MIN_5SEC);
    assert_eq!(game.event.as_deref(), Some("test-event"));
}

#[test]
fn denryu_kif_converted_to_csa_is_imported_with_the_recorded_result() {
    let game = import(include_str!("../../../tools/tests/golden/kifu_publish/dr2.csa"));
    assert_eq!(
        (game.black.as_str(), game.white.as_str()),
        ("A.I. Ari shogi", "とあるバイトの猪突猛進")
    );
    // `まで22手で後手の勝ち`
    assert_eq!((game.result_kind, game.end_reason), ("WIN_WHITE", "RESIGN"));
    assert_eq!(game.moves_count, 22);
    assert_eq!(game.clock, FISCHER_10MIN_2SEC);

    let game = import(include_str!("../../../tools/tests/golden/kifu_publish/dr6.csa"));
    assert_eq!((game.black.as_str(), game.white.as_str()), ("Gokaku", "KKO"));
    // `まで7手で先手の勝ち`
    assert_eq!((game.result_kind, game.end_reason), ("WIN_BLACK", "RESIGN"));
    assert_eq!(game.moves_count, 7);
    assert_eq!(game.clock, FISCHER_10MIN_2SEC);
}

/// 変換時に残した `'まで…` コメント (電竜戦の KIF の結果行) から読める勝敗。
fn recorded_result(csa: &str) -> Option<&'static str> {
    let line = csa.lines().find(|line| line.starts_with("'まで"))?;
    if line.contains("先手の勝ち") {
        Some("WIN_BLACK")
    } else if line.contains("後手の勝ち") {
        Some("WIN_WHITE")
    } else if line.contains("引分け") || line.ends_with("千日手") {
        Some("DRAW")
    } else {
        None
    }
}

fn verify_dir(dir: &Path, counts: &mut BTreeMap<String, u32>, failures: &mut Vec<String>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .map(|entry| entry.unwrap().path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            verify_dir(&path, counts, failures);
            continue;
        }
        if path.extension().is_none_or(|extension| extension != "csa") {
            continue;
        }
        let csa = std::fs::read_to_string(&path).unwrap();
        let outcome = match import_upload(&csa, Some("verify")) {
            Ok(game) => match recorded_result(&csa) {
                Some(recorded) if recorded != game.result_kind => {
                    failures.push(format!("{}: result mismatch", path.display()));
                    format!("NG result {} (recorded {recorded})", game.result_kind)
                }
                _ => format!("ok {} / {}", game.result_kind, game.end_reason),
            },
            Err(e) => {
                failures.push(format!("{}: {e}", path.display()));
                let message = e.to_string();
                // 手数や指し手を除いた分類だけを数える。
                let kind = message.split(['(', ':']).next().unwrap_or(&message).trim().to_owned();
                format!("NG {kind}")
            }
        };
        *counts.entry(outcome).or_default() += 1;
    }
}

/// `kifu_publish csa|kif --dry-run --out-dir <dir>` の出力を全件、取り込みと同じ検証
/// (全手の合法性、メタの導出) に通す。棋譜アーカイブを取り込む前の確認用。
///
/// ```text
/// KIFU_VERIFY_DIR=<dir> cargo test -p rshogi-csa-server-workers \
///     --test kifu_archive_import -- --ignored --nocapture
/// ```
#[test]
#[ignore = "KIFU_VERIFY_DIR に kifu_publish --dry-run --out-dir の出力先を指定して実行する"]
fn verify_converted_archive_dir() {
    let dir = std::env::var("KIFU_VERIFY_DIR").expect("KIFU_VERIFY_DIR を指定してください");
    let mut counts = BTreeMap::new();
    let mut failures = Vec::new();
    verify_dir(Path::new(&dir), &mut counts, &mut failures);
    for (outcome, count) in &counts {
        println!("{count:5}  {outcome}");
    }
    for failure in &failures {
        println!("FAILED {failure}");
    }
    assert!(failures.is_empty(), "{} 局が取り込めません", failures.len());
}
