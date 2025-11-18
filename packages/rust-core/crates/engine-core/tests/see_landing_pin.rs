use engine_core::shogi::Position;
use engine_core::usi::parse_usi_move;

#[test]
fn see_landing_detects_defender_pinned_after_quiet_move() {
    // Board: k4r3/9/9/8b/9/6R2/5G3/4K4/9 b - 1
    // Black to move: R3f -> 4f opens the diagonal between the white bishop (1d) and black king (5h),
    // pinning the gold on 4g. White's rook on 4a can then capture 4f, and the pinned gold may not recapture.
    let sfen = "k4r3/9/9/8b/9/6R2/5G3/4K4/9 b - 1";
    let pos = Position::from_sfen(sfen).expect("valid SFEN");
    let mv = parse_usi_move("3f4f").expect("valid move");

    let see = pos.see_landing_after_move(mv, 0);
    assert!(see <= -500, "SEE landing should flag the move as a large loss, got {}", see);
}
