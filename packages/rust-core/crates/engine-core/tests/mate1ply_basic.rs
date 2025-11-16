use engine_core::movegen::MoveGenerator;
use engine_core::search::mate1ply;
use engine_core::shogi::{Color, Position};
use engine_core::usi::parse_usi_move;
use std::fs;

fn load_fixture(name: &str) -> Position {
    let path = format!("{}/tests/fixtures/mate1ply/{}", env!("CARGO_MANIFEST_DIR"), name);
    let sfen = fs::read_to_string(path).expect("fixture exists");
    Position::from_sfen(sfen.trim()).expect("valid sfen")
}

#[test]
fn mate_in_one_detects_gold_drop_mate() {
    let mut pos = load_fixture("basic_black_mate.sfen");
    let mv = mate1ply::mate_in_one_for_side(&mut pos, Color::Black).expect("mate expected");
    assert!(pos.is_legal_move(mv), "returned move must be legal");
}

#[test]
fn mate_in_one_requires_in_check() {
    let mut pos = load_fixture("stalemate_not_check.sfen");
    assert!(
        mate1ply::mate_in_one_for_side(&mut pos, Color::Black).is_none(),
        "should not treat stalemate as mate"
    );

    // Verify that moving the knight removes the last legal king move without delivering check.
    let mv = parse_usi_move("4e3c").expect("valid move");
    let undo = pos.do_move(mv);
    assert!(!pos.is_in_check(), "position after blocking move should not be check");
    let reply_gen = MoveGenerator::new();
    assert!(
        !reply_gen.has_legal_moves(&pos).expect("movegen ok"),
        "king should be stalemated"
    );
    pos.undo_move(mv, undo);
}
