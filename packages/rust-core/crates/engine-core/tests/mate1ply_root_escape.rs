use engine_core::search::root_escape;
use engine_core::shogi::{Move, Position};
use engine_core::usi::{create_position, move_to_usi, parse_usi_move};
use std::fs;

const COMMON_TRAP_MOVES: &str = "2g2f 3c3d 2f2e 4a3b 2e2d 2c2d 2h2d P*2c 2d3d 8c8d 3d3f 8d8e 3f5f 3a4b 5f3f 4b3c 3f6f 7a7b 6f2f 7b8c 2f5f 6a5b 5f2f 5a4b 5i5h 4b3a 6g6f 4c4d 6i5i 5b4c 2f2e 8c9d 5h6i 7c7d 2e6e 8b6b 6e5e 1c1d 5e2e 1d1e 6i6h 4c3d 2e5e 3b4b 5e5f 4b4c 7g7f 5c5d 9g9f 3d2e P*2g 6c6d 8h9g 3c3d 6f6e 7d7e 9g7e 8e8f 6e6d 8f8g+ 6d6c 6b4b 7e4b+ 3a4b R*8b 4b3c 8b8a+ B*8f N*7g 8g7g 8i7g P*6g 6h6g N*5e 6g6h 2b3a 6c6b+ 8f6d 6b6c 6d4b 8a9a 5e4g+ 6h7h P*8f L*4h 8f8g+ 7h8g 4g4h 4i4h L*8f 8g7h 1e1f 6c5b 4b6d 9a9c 1f1g+ 9c9d P*8g 9d6d 3a6d 1i1g R*1h S*2h 1a1g+ 2i1g 2e1e B*5a L*4b P*1b 8g8h+ 7h6i 8h7i 6i6h 8f8i+ L*1i 7i6i 6h6i 1h2h+ 3i2h 8i9i 5b4b 6d4b 5a4b+ 3c4b R*8b 4b3a B*2b 3a4a 1b1a+ L*6g N*6h 6g6h+ 6i6h P*6g 6h6g N*5e 6g6f B*9c";

const MATE_TRAP_SUFFIX: &str = "L*7e 9c8b";

fn load_fixture(name: &str) -> Position {
    let path = format!("{}/tests/fixtures/mate1ply/{}", env!("CARGO_MANIFEST_DIR"), name);
    let sfen = fs::read_to_string(path).expect("fixture exists");
    Position::from_sfen(sfen.trim()).expect("valid SFEN")
}

fn position_from_sections(sections: &[&str]) -> Position {
    let moves: Vec<String> = sections
        .iter()
        .flat_map(|section| section.split_whitespace().filter(|s| !s.is_empty()))
        .map(|mv| mv.to_string())
        .collect();
    create_position(true, None, &moves).expect("valid startpos + moves")
}

#[test]
fn mate_trap_marks_mate_risky_move_and_preserves_safe_alternatives() {
    let pos = position_from_sections(&[COMMON_TRAP_MOVES, MATE_TRAP_SUFFIX]);
    let summary = root_escape::root_escape_scan(&pos, None);

    assert!(!summary.safe.is_empty(), "at least one safe move should exist in trap position");
    assert!(
        !summary.risky.is_empty(),
        "7g6e should be classified as risky due to mate threat"
    );

    let (_, mate_reply) = summary
        .risky
        .iter()
        .find(|(mv, _)| move_to_usi(mv) == "7g6e")
        .copied()
        .expect("7g6e must be classified as risky");
    assert_eq!(move_to_usi(&mate_reply), "R*6g", "7g6e must be paired with the mate reply R*6g");
    assert!(
        summary.safe.iter().any(|mv| move_to_usi(mv) == "7e7b+"),
        "safe alternative should remain in the safe set"
    );
}

#[test]
fn static_risk_threshold_promotes_see_risky_moves() {
    let pos = position_from_sections(&[COMMON_TRAP_MOVES]);
    let mut summary = root_escape::root_escape_scan(&pos, None);

    let suspect = summary
        .safe
        .iter()
        .copied()
        .find(|mv| move_to_usi(mv) == "8b8d")
        .expect("SEE classification should start from the safe set");

    let see_loss = root_escape::see_loss_for_move(&pos, suspect)
        .expect("SEE loss should be computable for the suspect move");
    assert!(see_loss < 0, "SEE loss should be negative, got {}", see_loss);

    root_escape::apply_static_risks(&pos, &mut summary, 200);

    let loss = summary.see_loss(suspect).expect("SEE threshold should mark the move as risky");
    assert!(loss <= -200, "SEE loss {} should exceed the configured threshold", loss);
    assert!(
        !summary.is_safe(suspect),
        "SEE reclassification must remove the move from the safe set"
    );
}

#[test]
fn safe_moves_empty_when_all_replies_mate() {
    let pos = load_fixture("all_risky_no_escape.sfen");
    let summary = root_escape::root_escape_scan(&pos, None);
    assert!(summary.safe.is_empty(), "safe list should be empty when every move allows mate");
    assert!(!summary.risky.is_empty(), "risky list should enumerate the losing moves");
}

#[test]
fn threat_reclassification_marks_bishop_fork_drop_risky() {
    let pos = load_fixture("bishop_fork_risk.sfen");
    let mut summary = root_escape::root_escape_scan(&pos, None);
    assert!(
        summary.safe.iter().any(|mv| move_to_usi(mv) == "L*7e"),
        "7e drop should initially be safe"
    );
    let initial_safe = summary.safe.len();
    let candidates: Vec<Move> = summary.safe.clone();
    root_escape::apply_threat_risks(&pos, &mut summary, &candidates, usize::MAX, 200);

    let l7e = parse_usi_move("L*7e").expect("valid move");
    assert!(
        summary.see_loss(l7e).is_some(),
        "7e drop must become risky after threat detection"
    );
    assert!(
        summary.safe.len() < initial_safe,
        "at least one safe move should be removed after threat detection"
    );
}
