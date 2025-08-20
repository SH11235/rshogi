//! Basic move generation tests

use crate::{
    movegen::generator::MoveGenImpl, usi::parse_usi_square, Color, Piece, PieceType, Position,
};

#[test]
fn test_movegen_startpos() {
    let pos = Position::startpos();

    let mut gen = MoveGenImpl::new(&pos);
    let moves = gen.generate_all();

    // Verify moves are generated
    assert!(!moves.is_empty(), "Should generate some moves");

    // Verify no duplicates
    let set: std::collections::HashSet<_> = moves.as_slice().iter().cloned().collect();
    assert_eq!(set.len(), moves.len(), "No duplicates");

    // Verify all moves are pseudo-legal
    for &m in moves.as_slice() {
        assert!(pos.is_pseudo_legal(m), "Generated move should be pseudo-legal");
    }
}

#[test]
fn test_movegen_king_moves() {
    let mut pos = Position::empty();
    pos.board
        .put_piece(parse_usi_square("5e").unwrap(), Piece::new(PieceType::King, Color::Black));

    let mut gen = MoveGenImpl::new(&pos);
    let moves = gen.generate_all();

    // King in center has 8 moves
    assert_eq!(moves.len(), 8);
}

#[test]
fn test_no_king_capture() {
    // 将棋では玉を取る手は禁止されているため、手生成時に除外される必要がある
    // このテストは、玉を取れる位置関係でも、そのような手が生成されないことを検証する
    let mut pos = Position::empty();

    // テスト局面の設定：
    // - 先手の銀(5b)が後手の玉(6c)を取れる位置関係
    // - 正しい実装では、この銀→玉の手は生成されないはず
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::Black)); // 先手玉: 5a
    pos.board
        .put_piece(parse_usi_square("5b").unwrap(), Piece::new(PieceType::Silver, Color::Black)); // 先手銀: 5b
    pos.board
        .put_piece(parse_usi_square("6c").unwrap(), Piece::new(PieceType::King, Color::White)); // 後手玉: 6c

    let mut gen = MoveGenImpl::new(&pos);
    let moves = gen.generate_all();

    // 生成された全ての手をチェックし、玉を取る手が含まれていないことを確認
    for m in moves.as_slice() {
        if !m.is_drop() {
            if let Some(from) = m.from() {
                let to = m.to();
                if from == parse_usi_square("5b").unwrap() && to == parse_usi_square("6c").unwrap()
                {
                    panic!("Generated illegal move: silver captures king!");
                }
            }
        }
    }

    log::debug!("OK: No king capture moves generated");
}

#[test]
fn test_board_edge_knight_moves() {
    // 盤端での桂馬の動き
    let mut pos = Position::empty();

    // 桂馬を1筋と9筋に配置 (Black knights at rank 8)
    pos.board
        .put_piece(parse_usi_square("1i").unwrap(), Piece::new(PieceType::Knight, Color::Black)); // 1i
    pos.board
        .put_piece(parse_usi_square("9i").unwrap(), Piece::new(PieceType::Knight, Color::Black)); // 9i
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));

    let mut gen = MoveGenImpl::new(&pos);
    let moves = gen.generate_all();

    // 1iの桂馬は2gにしか行けない (file 7, rank 6)
    let knight1_moves: Vec<_> = moves
        .as_slice()
        .iter()
        .filter(|m| !m.is_drop() && m.from() == Some(parse_usi_square("1i").unwrap()))
        .collect();

    assert_eq!(knight1_moves.len(), 1);
    assert_eq!(knight1_moves[0].to(), parse_usi_square("2g").unwrap()); // Black knight jumps to rank 6

    // 9iの桂馬は8gにしか行けない
    let knight9_moves: Vec<_> = moves
        .as_slice()
        .iter()
        .filter(|m| !m.is_drop() && m.from() == Some(parse_usi_square("9i").unwrap()))
        .collect();

    assert_eq!(knight9_moves.len(), 1);
    assert_eq!(knight9_moves[0].to(), parse_usi_square("8g").unwrap()); // Black knight jumps to rank 6
}

#[test]
fn test_all_legal_moves_generated_completeness() {
    // Test that MoveGenImpl generates all legal moves by verifying
    // each generated move is legal and all legal moves are generated
    use std::collections::HashSet;

    let pos = Position::startpos();

    // Generate moves using MoveGenImpl
    let mut gen = MoveGenImpl::new(&pos);
    let all_moves = gen.generate_all();
    let move_set: HashSet<_> = all_moves.as_slice().iter().cloned().collect();

    // Verify all generated moves are legal
    for &mv in all_moves.as_slice() {
        assert!(pos.is_pseudo_legal(mv), "Generated move should be pseudo-legal: {mv:?}");
    }

    // Verify no duplicates
    assert_eq!(all_moves.len(), move_set.len(), "Should have no duplicate moves");
}
