use engine_core::search::root_escape;
use engine_core::shogi::Position;
use engine_core::usi::{move_to_usi, parse_usi_move};

const MATE_TRAP_MOVES: &str = "2g2f 3c3d 2f2e 4a3b 2e2d 2c2d 2h2d P*2c 2d3d 8c8d 3d3f 8d8e 3f5f 3a4b 5f3f 4b3c 3f6f 7a7b 6f2f 7b8c 2f5f 6a5b 5f2f 5a4b 5i5h 4b3a 6g6f 4c4d 6i5i 5b4c 2f2e 8c9d 5h6i 7c7d 2e6e 8b6b 6e5e 1c1d 5e2e 1d1e 6i6h 4c3d 2e5e 3b4b 5e5f 4b4c 7g7f 5c5d 9g9f 3d2e P*2g 6c6d 8h9g 3c3d 6f6e 7d7e 9g7e 8e8f 6e6d 8f8g+ 6d6c 6b4b 7e4b+ 3a4b R*8b 4b3c 8b8a+ B*8f N*7g 8g7g 8i7g P*6g 6h6g N*5e 6g6h 2b3a 6c6b+ 8f6d 6b6c 6d4b 8a9a 5e4g+ 6h7h P*8f L*4h 8f8g+ 7h8g 4g4h 4i4h L*8f 8g7h 1e1f 6c5b 4b6d 9a9c 1f1g+ 9c9d P*8g 9d6d 3a6d 1i1g R*1h S*2h 1a1g+ 2i1g 2e1e B*5a L*4b P*1b 8g8h+ 7h6i 8h7i 6i6h 8f8i+ L*1i 7i6i 6h6i 1h2h+ 3i2h 8i9i 5b4b 6d4b 5a4b+ 3c4b R*8b 4b3a B*2b 3a4a 1b1a+ L*6g N*6h 6g6h+ 6i6h P*6g 6h6g N*5e 6g6f B*9c L*7e 9c8b";

fn position_from_moves(moves: &str) -> Position {
    let mut pos = Position::startpos();
    for token in moves.split_whitespace() {
        let mv = parse_usi_move(token).expect("valid move");
        pos.do_move(mv);
    }
    pos
}

#[test]
fn root_escape_flags_risky_moves_in_trap_position() {
    let pos = position_from_moves(MATE_TRAP_MOVES);
    let summary = root_escape::root_escape_scan(&pos, Some(512));
    assert!(!summary.safe.is_empty(), "trap position should contain safe moves");
    assert!(
        summary
            .risky
            .iter()
            .any(|(mv, mate)| move_to_usi(mv) == "7g6e" && move_to_usi(mate) == "R*6g"),
        "expected root_escape to flag 7g6e as leading to R*6g"
    );
    let safe_example = summary.safe.first().copied().expect("safe set should be non-empty");
    assert!(summary.is_safe(safe_example), "safe helper must report true for retained moves");
}

#[test]
fn root_escape_respects_move_scan_limit() {
    let pos = position_from_moves(MATE_TRAP_MOVES);
    let summary = root_escape::root_escape_scan(&pos, Some(0));
    assert!(
        summary.safe.is_empty() && summary.risky.is_empty(),
        "scan limit=0 should skip classification entirely"
    );
}
