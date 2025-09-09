//! Log-style comparison for qsearch behavior on short-check positions

use crate::evaluation::evaluate::MaterialEvaluator;
use crate::movegen::MoveGenerator;
use crate::search::unified::core::quiescence;
use crate::search::unified::UnifiedSearcher;
use crate::search::SearchLimits;
use crate::shogi::{Color, Piece, PieceType, Position};
use crate::usi::parse_usi_square;

#[test]
fn qsearch_compare_non_capture_check_logs() {
    // Build a simple position where a non-capture checking move exists
    // White king at 5e; Black rook at 5i can move to 5h to give check.
    let mut pos = Position::empty();
    pos.board
        .put_piece(parse_usi_square("5e").unwrap(), Piece::new(PieceType::King, Color::White));
    pos.board
        .put_piece(parse_usi_square("9i").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::Rook, Color::Black));
    pos.board.rebuild_occupancy_bitboards();
    pos.side_to_move = Color::Black;
    pos.hash = pos.compute_hash();
    pos.zobrist_hash = pos.hash;

    // Sanity: ensure there exists a non-capture checking move from this position
    let mg = MoveGenerator::new();
    let all = mg.generate_all(&pos).expect("move gen failed");
    let has_nocap_check = all
        .iter()
        .copied()
        .any(|mv| !mv.is_drop() && pos.piece_at(mv.to()).is_none() && pos.gives_check(mv));
    assert!(has_nocap_check, "Expected a non-capture checking move to exist");

    // Baseline: pruning disabled (qs checks disabled by our gating)
    let mut s_base = UnifiedSearcher::<MaterialEvaluator, false, false>::new(MaterialEvaluator);
    s_base.context.set_limits(SearchLimits::builder().qnodes_limit(10_000).build());

    // With checks: pruning enabled (qs checks/promotions enabled by our gating)
    let mut s_chk = UnifiedSearcher::<MaterialEvaluator, false, true>::new(MaterialEvaluator);
    s_chk.context.set_limits(SearchLimits::builder().qnodes_limit(10_000).build());

    let mut p1 = pos.clone();
    let mut p2 = pos.clone();
    let stand_pat = s_chk.evaluate(&pos);
    let base = quiescence::quiescence_search(&mut s_base, &mut p1, -30_000, 30_000, 0, 0);
    let withc = quiescence::quiescence_search(&mut s_chk, &mut p2, -30_000, 30_000, 0, 0);

    println!("[QS-LOG] Position: {}", crate::usi::position_to_sfen(&pos));
    println!("[QS-LOG] StandPat: {stand_pat}");
    println!("[QS-LOG] Baseline: score={base}, qnodes={}", s_base.stats.qnodes);
    println!("[QS-LOG] WithChk : score={withc}, qnodes={}", s_chk.stats.qnodes);
    println!(
        "[QS-LOG] Swing |SP-QS|: baseline={} vs with_checks={}",
        (stand_pat - base).abs(),
        (stand_pat - withc).abs()
    );

    // Acceptance: with checks enabled, the deviation from stand-pat should not be worse
    assert!(
        (stand_pat - withc).abs() <= (stand_pat - base).abs(),
        "With checks should reduce or maintain swing from stand-pat"
    );

    // --- Case 2: Non-capture promotion that gives check (should change material) ---
    let mut pos2 = Position::empty();
    // White king at 4b
    pos2.board
        .put_piece(parse_usi_square("4b").unwrap(), Piece::new(PieceType::King, Color::White));
    // Black king far away
    pos2.board
        .put_piece(parse_usi_square("9i").unwrap(), Piece::new(PieceType::King, Color::Black));
    // Black rook at 5d can move to 5c and promote to Dragon, which attacks 4b (diagonal)
    pos2.board
        .put_piece(parse_usi_square("5d").unwrap(), Piece::new(PieceType::Rook, Color::Black));
    pos2.board.rebuild_occupancy_bitboards();
    pos2.side_to_move = Color::Black;
    pos2.hash = pos2.compute_hash();
    pos2.zobrist_hash = pos2.hash;

    // Sanity: check there is a non-capture promotion that gives check
    let all2 = mg.generate_all(&pos2).expect("move gen failed");
    let promo_check = all2.iter().copied().any(|mv| {
        !mv.is_drop() && mv.is_promote() && pos2.piece_at(mv.to()).is_none() && pos2.gives_check(mv)
    });
    assert!(promo_check, "Expected a non-capture promotion check to exist");

    let mut s_base2 = UnifiedSearcher::<MaterialEvaluator, false, false>::new(MaterialEvaluator);
    s_base2.context.set_limits(SearchLimits::builder().qnodes_limit(10_000).build());
    let mut s_chk2 = UnifiedSearcher::<MaterialEvaluator, false, true>::new(MaterialEvaluator);
    s_chk2.context.set_limits(SearchLimits::builder().qnodes_limit(10_000).build());

    let mut pp1 = pos2.clone();
    let mut pp2 = pos2.clone();
    let sp2 = s_chk2.evaluate(&pos2);
    let base2 = quiescence::quiescence_search(&mut s_base2, &mut pp1, -30_000, 30_000, 0, 0);
    let with2 = quiescence::quiescence_search(&mut s_chk2, &mut pp2, -30_000, 30_000, 0, 0);

    println!("[QS-LOG] Case2 Position: {}", crate::usi::position_to_sfen(&pos2));
    println!("[QS-LOG] Case2 StandPat: {sp2}");
    println!("[QS-LOG] Case2 Baseline: score={base2}, qnodes={}", s_base2.stats.qnodes);
    println!("[QS-LOG] Case2 WithChk : score={with2}, qnodes={}", s_chk2.stats.qnodes);
    println!(
        "[QS-LOG] Case2 Swing |SP-QS|: baseline={} vs with_checks={}",
        (sp2 - base2).abs(),
        (sp2 - with2).abs()
    );

    assert!(
        (sp2 - with2).abs() <= (sp2 - base2).abs(),
        "Promotion-check should not increase swing versus baseline"
    );
}
