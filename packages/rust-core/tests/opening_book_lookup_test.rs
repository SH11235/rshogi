//! 初期盤面付近での定跡検索テスト
//! $ cargo test --test opening_book_initial_position_test -- --nocapture

use shogi_core::opening_book::PositionHasher;
use shogi_core::opening_book_reader::OpeningBookReader;
use std::fs::File;
use std::io::Read;

// 初期盤面のSFEN（手数0）
const INITIAL_SFEN: &str = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 0";
// 2六歩の後の盤面
// const AFTER_26_SFEN: &str = "lnsgkgsnl/1r5b1/ppppppppp/9/9/7P1/PPPPPPP1P/1B5R1/LNSGKGSNL b - 1";
// 7六歩の後の盤面
const AFTER_76_SFEN: &str = "lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL b - 2";

#[test]
fn test_initial_position_opening_moves() {
    // OpeningBookReaderを初期化
    let mut reader = OpeningBookReader::new();

    // テスト用の定跡ファイルを読み込む
    let test_file = "converted_openings/opening_book_web.binz";
    if let Ok(mut file) = File::open(test_file) {
        let mut data = Vec::new();
        if file.read_to_end(&mut data).is_ok() {
            match reader.load_data(&data) {
                Ok(result) => {
                    println!("Opening book loaded: {}", result);

                    // 初期盤面の手を検索
                    let moves = reader.find_moves(INITIAL_SFEN);

                    println!("Number of moves found: {}", moves.len());
                    for (i, book_move) in moves.iter().enumerate() {
                        println!(
                            "Move {}: {} (eval: {}, depth: {})",
                            i + 1,
                            book_move.notation,
                            book_move.evaluation,
                            book_move.depth
                        );
                    }

                    // 定跡に手があることを確認（将棋の初期盤面なら通常は手がある）
                    assert!(!moves.is_empty(), "Initial position should have opening moves");

                    // よくある手があるか確認
                    let common_moves = ["7g7f", "2g2f", "5g5f", "2h2g"];
                    let found_moves: Vec<&str> =
                        moves.iter().map(|m| m.notation.as_str()).collect();

                    let has_common_move = common_moves.iter().any(|&m| found_moves.contains(&m));

                    assert!(
                        has_common_move,
                        "Should contain at least one common opening move. Found: {:?}",
                        found_moves
                    );
                }
                Err(e) => {
                    panic!("Failed to load opening book: {}", e);
                }
            }
        }
    } else {
        println!("Warning: Opening book file not found at {}. Skipping test.", test_file);
    }
}

// 7六歩の後の盤面での定跡が正しく取得できるかテスト
#[test]
fn test_after_76_position_opening_moves() {
    // OpeningBookReaderを初期化
    let mut reader = OpeningBookReader::new();
    // テスト用の定跡ファイルを読み込む
    let test_file = "converted_openings/opening_book_full.binz";
    if let Ok(mut file) = File::open(test_file) {
        let mut data = Vec::new();
        if file.read_to_end(&mut data).is_ok() {
            match reader.load_data(&data) {
                Ok(result) => {
                    println!("Opening book loaded: {}", result);

                    // 7六歩の後の盤面の手を検索
                    let moves = reader.find_moves(AFTER_76_SFEN);

                    println!("Number of moves found: {}", moves.len());
                    for (i, book_move) in moves.iter().enumerate() {
                        println!(
                            "Move {}: {} (eval: {}, depth: {})",
                            i + 1,
                            book_move.notation,
                            book_move.evaluation,
                            book_move.depth
                        );
                    }

                    // 定跡に手があることを確認
                    assert!(!moves.is_empty(), "After 76 position should have opening moves");
                    let expected_moves = ["2g2f", "1g1f", "6i7h", "7i7h", "9g9f"];
                    let found_moves: Vec<&str> =
                        moves.iter().map(|m| m.notation.as_str()).collect();

                    let has_common_move = expected_moves.iter().any(|&m| found_moves.contains(&m));

                    assert!(
                        has_common_move,
                        "Should contain at least one common opening move. Found: {:?}",
                        found_moves
                    );
                }
                Err(e) => {
                    panic!("Failed to load opening book: {}", e);
                }
            }
        }
    } else {
        println!("Warning: Opening book file not found at {}. Skipping test.", test_file);
    }
}

/// ハッシュ計算の詳細をデバッグ出力するヘルパー関数
#[test]
fn debug_initial_position_hash_parts() {
    // SFENを分解
    let parts: Vec<&str> = INITIAL_SFEN.split_whitespace().collect();

    println!("=== Initial Position SFEN Parts ===");
    println!("Full SFEN: {}", INITIAL_SFEN);
    println!("Board:     {}", parts[0]);
    println!("Turn:      {}", parts[1]);
    println!("Hands:     {}", parts[2]);
    println!("Move:      {}", parts[3]);

    // ハッシュ計算に使用される部分（最初の3要素）
    let hash_input = format!("{} {} {}", parts[0], parts[1], parts[2]);
    println!("\nHash input (first 3 parts): {}", hash_input);

    // ハッシュを計算
    let hash = PositionHasher::hash_position(&INITIAL_SFEN).unwrap();
    println!("Calculated hash: {:#016x}", hash);
}
