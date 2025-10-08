use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::evaluation::evaluate::{Evaluator, MaterialEvaluator};
use crate::movegen::MoveGenerator;
use crate::search::ab::ordering::{Heuristics, MovePicker};
use crate::search::api::{InfoEvent, SearcherBackend};
use crate::search::constants::SEARCH_INF;
use crate::search::limits::SearchLimitsBuilder;
use crate::search::mate_score;
use crate::search::snapshot::SnapshotSource;
use crate::search::types::{NodeType, TerminationReason};
use crate::search::{SearchLimits, SearchStack, TranspositionTable};
use crate::shogi::{Color, Move, Piece, PieceType};
use crate::time_management::{
    self, mock_advance_time, mock_set_time, GamePhase, TimeControl, TimeLimits, TimeManager,
};
use crate::usi::{parse_usi_move, parse_usi_square};
use crate::Position;
use smallvec::SmallVec;

use super::driver::ClassicBackend;
use super::pruning::NullMovePruneParams;
use super::pvs::SearchContext;
use super::SearchProfile;

fn position_after_moves(moves: &[&str]) -> Position {
    let mut pos = Position::startpos();
    for usi in moves {
        let mv = parse_usi_move(usi).expect("valid usi move");
        assert!(pos.is_legal_move(mv), "illegal move in sequence: {}", usi);
        pos.do_move(mv);
    }
    pos
}

fn make_aspiration_stress_position() -> Position {
    const MOVES: &[&str] = &[
        "7i6h", "3c3d", "1i1h", "4a3b", "9g9f", "3a4b", "2h5h", "4b3c", "8h9g", "5a4b", "6i7h",
        "8c8d",
    ];
    position_after_moves(MOVES)
}

#[test]
fn qsearch_detects_mate_when_evasion_missing() {
    let evaluator = Arc::new(MaterialEvaluator);
    let backend = ClassicBackend::new(evaluator);
    let mut pos = Position::empty();
    pos.side_to_move = Color::Black;
    pos.board
        .put_piece(parse_usi_square("9i").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("9a").unwrap(), Piece::new(PieceType::Rook, Color::White));
    pos.board
        .put_piece(parse_usi_square("9h").unwrap(), Piece::new(PieceType::Gold, Color::White));
    pos.board
        .put_piece(parse_usi_square("8i").unwrap(), Piece::new(PieceType::Gold, Color::White));
    pos.board
        .put_piece(parse_usi_square("8h").unwrap(), Piece::new(PieceType::Silver, Color::White));
    pos.board
        .put_piece(parse_usi_square("8g").unwrap(), Piece::new(PieceType::Silver, Color::White));
    pos.board
        .put_piece(parse_usi_square("7h").unwrap(), Piece::new(PieceType::Gold, Color::White));

    let limits = SearchLimits::default();
    let start_time = Instant::now();
    let mut nodes = 0_u64;
    let mut seldepth = 0_u32;
    let mut qnodes = 0_u64;
    let qnodes_limit = crate::search::constants::DEFAULT_QNODES_LIMIT;
    let mut ctx = SearchContext {
        limits: &limits,
        start_time: &start_time,
        nodes: &mut nodes,
        seldepth: &mut seldepth,
        qnodes: &mut qnodes,
        qnodes_limit,
    };

    let score = backend.qsearch(&pos, -SEARCH_INF, SEARCH_INF, &mut ctx, 0);

    assert_eq!(score, mate_score(0, false));
}

#[test]
fn multipv_line_nodes_are_per_line() {
    let evaluator = Arc::new(MaterialEvaluator);
    let backend = ClassicBackend::new(evaluator);
    let pos = Position::startpos();
    let limits = SearchLimitsBuilder::default().depth(2).multipv(2).build();

    let result = backend.think_blocking(&pos, &limits, None);
    let lines = result.lines.as_ref().expect("expected multipv lines to be present");
    assert!(lines.len() >= 2);

    let total_nodes = result.stats.nodes;
    assert!(total_nodes > 0, "search should consume nodes");
    let total_time_ms = result.stats.elapsed.as_millis() as u64;

    let mut sum_nodes = 0_u64;
    let mut sum_time = 0_u64;
    for (idx, line) in lines.iter().enumerate() {
        if let Some(n) = line.nodes {
            assert!(n > 0, "line {idx} nodes should be positive");
            sum_nodes = sum_nodes.saturating_add(n);
        }
        if let Some(ms) = line.time_ms {
            assert!(ms <= total_time_ms, "line {idx} time exceeds total time");
            sum_time = sum_time.saturating_add(ms);
        }
        if let Some(nps) = line.nps {
            if let Some(ms) = line.time_ms {
                if ms > 0 {
                    let expected = line.nodes.unwrap_or(0).saturating_mul(1000) / ms.max(1);
                    assert_eq!(nps, expected, "line {idx} nps should match nodes/time");
                }
            }
        }
    }
    assert!(sum_nodes <= total_nodes, "per-line nodes exceed total nodes");
    assert!(sum_time <= total_time_ms, "per-line time exceeds total time");
}

#[test]
fn classify_root_bound_matches_aspiration_cases() {
    use crate::search::types::NodeType;

    type Backend = ClassicBackend<MaterialEvaluator>;

    assert_eq!(Backend::classify_root_bound(-10, 0, 30), NodeType::UpperBound);
    assert_eq!(Backend::classify_root_bound(40, 0, 30), NodeType::LowerBound);
    assert_eq!(Backend::classify_root_bound(10, 0, 30), NodeType::Exact);
}

#[test]
fn root_bound_uses_final_window_after_research() {
    use crate::search::types::NodeType;

    type Backend = ClassicBackend<MaterialEvaluator>;

    let alpha_initial = -30;
    let beta_initial = 30;
    let local_best = 100;
    let final_alpha = alpha_initial;
    let final_beta = beta_initial + 120;

    assert_eq!(
        Backend::classify_root_bound(local_best, alpha_initial, beta_initial),
        NodeType::LowerBound,
    );
    assert_eq!(
        Backend::classify_root_bound(local_best, final_alpha, final_beta),
        NodeType::Exact,
    );
}

#[test]
fn tt_bound_follows_used_window() {
    use crate::search::tt::{TTProbe, TranspositionTable};
    use crate::search::NodeType;

    let evaluator = Arc::new(MaterialEvaluator);
    let backend = ClassicBackend::with_tt(evaluator.clone(), Arc::new(TranspositionTable::new(16)));
    let pos = Position::startpos();
    let limits = SearchLimits::default();
    let t0 = Instant::now();
    let mut nodes = 0_u64;
    let mut seldepth = 0_u32;
    let mut qnodes = 0_u64;
    let qnodes_limit = crate::search::constants::DEFAULT_QNODES_LIMIT;
    let mut ctx = SearchContext {
        limits: &limits,
        start_time: &t0,
        nodes: &mut nodes,
        seldepth: &mut seldepth,
        qnodes: &mut qnodes,
        qnodes_limit,
    };
    let mut stack = vec![SearchStack::default(); crate::search::constants::MAX_PLY + 1];
    let mut heur = Heuristics::default();
    let mut tt_hits = 0;
    let mut beta_cuts = 0;
    let mut lmr_counter = 0;

    // Narrow window forcing a bound update – verify TT entry uses the window after MDP adjustments
    let original_alpha = -10;
    let original_beta = -5;
    let (score, _) = backend.alphabeta(
        crate::search::ab::pvs::ABArgs {
            pos: &pos,
            depth: 2,
            alpha: original_alpha,
            beta: original_beta,
            ply: 0,
            is_pv: true,
            stack: &mut stack,
            heur: &mut heur,
            tt_hits: &mut tt_hits,
            beta_cuts: &mut beta_cuts,
            lmr_counter: &mut lmr_counter,
        },
        &mut ctx,
    );
    assert!(backend.tt.as_ref().is_some(), "default backend should have TT");
    if let Some(tt) = backend.tt {
        if let Some(entry) = tt.probe(pos.zobrist_hash(), pos.side_to_move) {
            let mut used_alpha = original_alpha;
            let mut used_beta = original_beta;
            crate::search::mate_distance_pruning(&mut used_alpha, &mut used_beta, 0);
            let expected = if score <= used_alpha {
                NodeType::UpperBound
            } else if score >= used_beta {
                NodeType::LowerBound
            } else {
                NodeType::Exact
            };
            assert_eq!(entry.node_type(), expected);
        }
    }
}

#[test]
fn qsearch_skips_quiet_checks_when_disabled() {
    let pos = Position::startpos();
    let heur = Heuristics::default();
    let mut picker = MovePicker::new_qsearch(&pos, None, None, None, 0);

    while let Some(mv) = picker.next(&heur) {
        assert!(mv.is_capture_hint() || !pos.gives_check(mv));
    }
}

#[test]
fn qsearch_respects_qnodes_limit() {
    let evaluator = Arc::new(MaterialEvaluator);
    let backend = ClassicBackend::new(evaluator);
    let pos = Position::startpos();
    let limit_value = 8_u64;
    let limits = SearchLimitsBuilder::default().qnodes_limit(limit_value).build();
    let start_time = Instant::now();
    let mut nodes = 0_u64;
    let mut seldepth = 0_u32;
    let mut qnodes = 0_u64;
    let mut ctx = SearchContext {
        limits: &limits,
        start_time: &start_time,
        nodes: &mut nodes,
        seldepth: &mut seldepth,
        qnodes: &mut qnodes,
        qnodes_limit: limit_value,
    };

    let _ = backend.qsearch(&pos, -SEARCH_INF, SEARCH_INF, &mut ctx, 0);

    assert!(
        qnodes <= limit_value,
        "qsearch should respect qnodes limit ({} > {})",
        qnodes,
        limit_value
    );
}

#[test]
fn qsearch_in_check_processes_evasion_before_qnode_cutoff() {
    let evaluator = Arc::new(MaterialEvaluator);
    let backend = ClassicBackend::new(evaluator);
    let mut pos = Position::empty();
    pos.side_to_move = Color::Black;
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));
    pos.board
        .put_piece(parse_usi_square("5h").unwrap(), Piece::new(PieceType::Rook, Color::White));

    let limits = SearchLimitsBuilder::default().qnodes_limit(1).build();
    let start_time = Instant::now();
    let mut nodes = 0_u64;
    let mut seldepth = 0_u32;
    let mut qnodes = 0_u64;
    let mut ctx = SearchContext {
        limits: &limits,
        start_time: &start_time,
        nodes: &mut nodes,
        seldepth: &mut seldepth,
        qnodes: &mut qnodes,
        qnodes_limit: 1,
    };

    let alpha = -1000;
    let beta = 1000;
    let _score = backend.qsearch(&pos, alpha, beta, &mut ctx, 0);

    assert!(
        nodes > 1,
        "qsearch should recurse into at least one evasion before honoring qnode limit"
    );
    assert!(qnodes <= 1, "qnodes counter must respect configured limit");
}

#[test]
fn compute_qnodes_limit_scales_with_remaining_time() {
    mock_set_time(0);

    let time_limits = TimeLimits {
        time_control: TimeControl::FixedTime { ms_per_move: 5_000 },
        ..Default::default()
    };
    let tm = Arc::new(TimeManager::new(&time_limits, Color::Black, 0, GamePhase::Opening));

    let mut limits = SearchLimitsBuilder::default()
        .time_control(TimeControl::FixedTime { ms_per_move: 5_000 })
        .start_time(time_management::mock_now())
        .build();
    limits.time_manager = Some(tm.clone());

    let initial = ClassicBackend::<MaterialEvaluator>::compute_qnodes_limit_for_test(&limits, 4, 1);
    assert!(
        initial <= 50_000 && initial > crate::search::constants::MIN_QNODES_LIMIT,
        "initial qnodes limit should scale with soft budget (got {initial})",
        initial = initial
    );

    mock_advance_time(4_000);
    let reduced = ClassicBackend::<MaterialEvaluator>::compute_qnodes_limit_for_test(&limits, 4, 1);
    assert!(
        reduced < initial,
        "qnodes limit should shrink as remaining time decreases (initial={initial}, reduced={reduced})"
    );
    assert!(
        reduced >= crate::search::constants::MIN_QNODES_LIMIT,
        "qnodes limit should not fall below safety floor"
    );

    mock_set_time(0);
}

#[test]
fn qsearch_detects_mate_with_min_qnodes_budget() {
    let evaluator = Arc::new(MaterialEvaluator);
    let backend = ClassicBackend::new(evaluator);
    let mut pos = Position::empty();
    pos.side_to_move = Color::Black;
    pos.board
        .put_piece(parse_usi_square("9i").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("9a").unwrap(), Piece::new(PieceType::Rook, Color::White));
    pos.board
        .put_piece(parse_usi_square("9h").unwrap(), Piece::new(PieceType::Gold, Color::White));
    pos.board
        .put_piece(parse_usi_square("8i").unwrap(), Piece::new(PieceType::Gold, Color::White));
    pos.board
        .put_piece(parse_usi_square("8h").unwrap(), Piece::new(PieceType::Silver, Color::White));
    pos.board
        .put_piece(parse_usi_square("8g").unwrap(), Piece::new(PieceType::Silver, Color::White));
    pos.board
        .put_piece(parse_usi_square("7h").unwrap(), Piece::new(PieceType::Gold, Color::White));

    let limits = SearchLimitsBuilder::default().qnodes_limit(1).build();
    let start_time = Instant::now();
    let mut nodes = 0_u64;
    let mut seldepth = 0_u32;
    let mut qnodes = 0_u64;
    let qnodes_limit = 1;
    let mut ctx = SearchContext {
        limits: &limits,
        start_time: &start_time,
        nodes: &mut nodes,
        seldepth: &mut seldepth,
        qnodes: &mut qnodes,
        qnodes_limit,
    };

    let score = backend.qsearch(&pos, -SEARCH_INF, SEARCH_INF, &mut ctx, 0);

    assert_eq!(score, mate_score(0, false));
}

#[test]
fn qsearch_returns_stand_pat_when_limit_exhausted() {
    let evaluator = Arc::new(MaterialEvaluator);
    let backend = ClassicBackend::new(evaluator.clone());
    let pos = Position::startpos();
    let limits = SearchLimitsBuilder::default().qnodes_limit(1).build();
    let start_time = Instant::now();
    let mut nodes = 0_u64;
    let mut seldepth = 0_u32;
    let mut qnodes = 0_u64;
    let mut ctx = SearchContext {
        limits: &limits,
        start_time: &start_time,
        nodes: &mut nodes,
        seldepth: &mut seldepth,
        qnodes: &mut qnodes,
        qnodes_limit: 1,
    };

    let material = MaterialEvaluator;
    let stand_pat = material.evaluate(&pos);
    let alpha = stand_pat - 200;
    let beta = stand_pat + 200;
    let score = backend.qsearch(&pos, alpha, beta, &mut ctx, 0);

    assert_eq!(score, stand_pat.max(alpha));
    assert_eq!(qnodes, 1);
}

#[test]
fn qsearch_prunes_negative_see_small_capture() {
    let evaluator = Arc::new(MaterialEvaluator);
    let backend = ClassicBackend::new(evaluator.clone());
    let mut pos = Position::empty();
    pos.side_to_move = Color::Black;
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));
    pos.board
        .put_piece(parse_usi_square("5g").unwrap(), Piece::new(PieceType::Silver, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5f").unwrap(), Piece::new(PieceType::Pawn, Color::White));
    pos.board
        .put_piece(parse_usi_square("5d").unwrap(), Piece::new(PieceType::Rook, Color::White));

    let capture = parse_usi_move("5g5f").unwrap();
    assert!(pos.see(capture) < 0, "expected negative SEE for the capture scenario");

    let limits = SearchLimits::default();
    let start_time = Instant::now();
    let mut nodes = 0_u64;
    let mut seldepth = 0_u32;
    let mut qnodes = 0_u64;
    let mut ctx = SearchContext {
        limits: &limits,
        start_time: &start_time,
        nodes: &mut nodes,
        seldepth: &mut seldepth,
        qnodes: &mut qnodes,
        qnodes_limit: crate::search::constants::DEFAULT_QNODES_LIMIT,
    };

    let material = MaterialEvaluator;
    let stand_pat = material.evaluate(&pos);
    let score = backend.qsearch(&pos, stand_pat - 200, stand_pat + 200, &mut ctx, 0);

    assert_eq!(score, stand_pat);
    assert_eq!(qnodes, 1, "negative SEE small capture should be pruned without expanding");
}

#[test]
fn qsearch_depth_cap_still_handles_in_check() {
    let evaluator = Arc::new(MaterialEvaluator);
    let backend = ClassicBackend::new(evaluator);
    let mut pos = Position::empty();
    pos.side_to_move = Color::Black;
    pos.board
        .put_piece(parse_usi_square("9i").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("9a").unwrap(), Piece::new(PieceType::Rook, Color::White));
    pos.board
        .put_piece(parse_usi_square("9h").unwrap(), Piece::new(PieceType::Gold, Color::White));
    pos.board
        .put_piece(parse_usi_square("8i").unwrap(), Piece::new(PieceType::Gold, Color::White));
    pos.board
        .put_piece(parse_usi_square("8h").unwrap(), Piece::new(PieceType::Silver, Color::White));

    let limits = SearchLimits::default();
    let start_time = Instant::now();
    let mut nodes = 0_u64;
    let mut seldepth = 0_u32;
    let mut qnodes = 0_u64;
    let mut ctx = SearchContext {
        limits: &limits,
        start_time: &start_time,
        nodes: &mut nodes,
        seldepth: &mut seldepth,
        qnodes: &mut qnodes,
        qnodes_limit: crate::search::constants::DEFAULT_QNODES_LIMIT,
    };

    let max_ply = crate::search::constants::MAX_QUIESCE_DEPTH as u32;
    let score = backend.qsearch(&pos, -SEARCH_INF, SEARCH_INF, &mut ctx, max_ply);

    assert_eq!(score, mate_score(max_ply as u8, false));
}

#[test]
fn root_rank_map_distinguishes_promotion_pairs() {
    let mut pos = Position::empty();
    pos.side_to_move = Color::Black;
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));
    pos.board
        .put_piece(parse_usi_square("3d").unwrap(), Piece::new(PieceType::Silver, Color::Black));

    let mg = MoveGenerator::new();
    let root_moves = mg.generate_all(&pos).unwrap();
    let mut promo_pair: Vec<_> = root_moves
        .as_slice()
        .iter()
        .copied()
        .filter(|mv| {
            mv.from().is_some_and(|sq| sq == parse_usi_square("3d").unwrap())
                && mv.to() == parse_usi_square("4c").unwrap()
        })
        .collect();
    promo_pair.sort_by_key(|mv| (mv.is_promote(), mv.to_u32()));
    assert_eq!(promo_pair.len(), 2, "expected promotion pair for Silver move");

    let mut rank_map: HashMap<u32, u32> = HashMap::new();
    for (idx, mv) in promo_pair.iter().enumerate() {
        rank_map.entry(mv.to_u32()).or_insert(idx as u32 + 1);
    }

    let non_promo = promo_pair[0];
    let promo = promo_pair[1];
    assert_ne!(
        rank_map.get(&non_promo.to_u32()),
        rank_map.get(&promo.to_u32()),
        "promotion pair must receive distinct currmove numbers",
    );
}

#[test]
fn multipv_filter_retains_promotion_variant() {
    let non_prom = parse_usi_move("3d4c").unwrap();
    let prom = parse_usi_move("3d4c+").unwrap();
    let root_moves: Vec<(Move, i32)> = vec![(non_prom, 0), (prom, 0)];
    let mut excluded = SmallVec::<[Move; 4]>::new();
    excluded.push(non_prom);
    let excluded_keys: SmallVec<[u32; 4]> = excluded.iter().map(|m| m.to_u32()).collect();
    let active: SmallVec<[(Move, i32); 4]> = root_moves
        .iter()
        .copied()
        .filter(|(m, _)| !excluded_keys.iter().any(|&ex| ex == m.to_u32()))
        .collect();

    assert!(
        active.iter().any(|(m, _)| m.to_u32() == prom.to_u32()),
        "promotion variant should remain after excluding non-promotion"
    );
}

#[test]
fn move_picker_returns_promotion_and_nonpromotion() {
    let mut pos = Position::empty();
    pos.side_to_move = Color::Black;
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));
    pos.board
        .put_piece(parse_usi_square("3d").unwrap(), Piece::new(PieceType::Silver, Color::Black));

    let mut picker = MovePicker::new_normal(&pos, None, None, [None, None], None, None);
    let heur = Heuristics::default();
    let mut found_promo = false;
    let mut found_nonpromo = false;
    let source = parse_usi_square("3d").unwrap();
    let dest = parse_usi_square("4c").unwrap();

    while let Some(mv) = picker.next(&heur) {
        if mv.from() == Some(source) && mv.to() == dest {
            if mv.is_promote() {
                found_promo = true;
            } else {
                found_nonpromo = true;
            }
            if found_promo && found_nonpromo {
                break;
            }
        }
    }

    assert!(found_nonpromo, "non-promotion move should be surfaced");
    assert!(found_promo, "promotion move should be surfaced");
}

#[test]
fn tt_move_does_not_hide_promotion_variant() {
    let mut pos = Position::empty();
    pos.side_to_move = Color::Black;
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));
    pos.board
        .put_piece(parse_usi_square("3d").unwrap(), Piece::new(PieceType::Silver, Color::Black));

    // TT 手として非成を登録し、同じ from→to の成りが返るか確認する
    let tt_move = parse_usi_move("3d4c").unwrap();
    let mut picker = MovePicker::new_normal(&pos, Some(tt_move), None, [None, None], None, None);
    let heur = Heuristics::default();

    // 1 手目は TT 手（非成）が返り、その後も昇成が得られることを期待
    let first = picker.next(&heur).expect("expected TT move first");
    assert_eq!(first.to_u32(), tt_move.to_u32());

    let mut found_promo = false;
    while let Some(mv) = picker.next(&heur) {
        if mv.from() == Some(parse_usi_square("3d").unwrap())
            && mv.to() == parse_usi_square("4c").unwrap()
            && mv.is_promote()
        {
            found_promo = true;
            break;
        }
    }

    assert!(found_promo, "promotion move should still be surfaced after TT move");
}

#[test]
fn probcut_sort_is_deterministic() {
    let mut pos = Position::empty();
    pos.side_to_move = Color::Black;
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));
    pos.board
        .put_piece(parse_usi_square("2h").unwrap(), Piece::new(PieceType::Silver, Color::Black));
    pos.board
        .put_piece(parse_usi_square("8h").unwrap(), Piece::new(PieceType::Silver, Color::Black));
    pos.board
        .put_piece(parse_usi_square("1g").unwrap(), Piece::new(PieceType::Pawn, Color::White));
    pos.board
        .put_piece(parse_usi_square("9g").unwrap(), Piece::new(PieceType::Pawn, Color::White));

    let heur = Heuristics::default();
    let threshold = 0;
    let mut first = MovePicker::new_probcut(&pos, None, None, threshold);
    let mut second = MovePicker::new_probcut(&pos, None, None, threshold);

    let collect = |picker: &mut MovePicker<'_>| {
        let mut moves = Vec::new();
        while let Some(mv) = picker.next(&heur) {
            moves.push(mv.to_u32());
        }
        moves
    };

    let seq1 = collect(&mut first);
    let seq2 = collect(&mut second);

    assert!(seq1.len() >= 2, "expected at least two probcut candidates");
    assert_eq!(seq1, seq2, "probcut picker must produce deterministic order");
}

#[test]
fn excluded_move_hides_entire_promotion_family() {
    let mut pos = Position::empty();
    pos.side_to_move = Color::Black;
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));
    pos.board
        .put_piece(parse_usi_square("3d").unwrap(), Piece::new(PieceType::Silver, Color::Black));

    let excluded = Some(parse_usi_move("3d4c").unwrap());
    let mut picker = MovePicker::new_normal(&pos, None, excluded, [None, None], None, None);
    let heur = Heuristics::default();

    while let Some(mv) = picker.next(&heur) {
        let from = mv.from();
        if from == Some(parse_usi_square("3d").unwrap())
            && mv.to() == parse_usi_square("4c").unwrap()
        {
            panic!("excluded move (including promotion) must not be returned: {mv:?}");
        }
    }
}

#[test]
fn extract_pv_returns_consistent_line() {
    let evaluator = Arc::new(MaterialEvaluator);
    let backend = ClassicBackend::new(evaluator);
    let pos = Position::startpos();
    let limits = SearchLimitsBuilder::default().depth(2).build();

    let result = backend.think_blocking(&pos, &limits, None);
    let best_move = result.best_move.expect("backend should find a best move");
    let depth = result.stats.depth as i32;
    let mut nodes = 0_u64;
    let pv = backend.extract_pv(&pos, depth, best_move, &limits, &mut nodes);

    assert!(!pv.is_empty(), "extract_pv should return a non-empty PV");
    assert_eq!(pv[0], best_move, "PV head should match best move");
}

#[test]
fn search_profile_basic_disables_advanced_pruning() {
    let profile = SearchProfile::basic();
    // Basic profile (Material/Nnue) disables all advanced pruning techniques
    assert!(!profile.prune.enable_nmp);
    assert!(!profile.prune.enable_iid);
    assert!(!profile.prune.enable_razor);
    assert!(!profile.prune.enable_probcut);
    assert!(!profile.prune.enable_static_beta_pruning);
}

struct RecordingEvaluator {
    inner: MaterialEvaluator,
    set_position: AtomicUsize,
    do_move: AtomicUsize,
    undo_move: AtomicUsize,
    do_null_move: AtomicUsize,
    undo_null_move: AtomicUsize,
}

impl Default for RecordingEvaluator {
    fn default() -> Self {
        Self {
            inner: MaterialEvaluator,
            set_position: AtomicUsize::new(0),
            do_move: AtomicUsize::new(0),
            undo_move: AtomicUsize::new(0),
            do_null_move: AtomicUsize::new(0),
            undo_null_move: AtomicUsize::new(0),
        }
    }
}

impl RecordingEvaluator {
    fn counts(&self) -> HookCallCounts {
        HookCallCounts {
            set_position: self.set_position.load(Ordering::Relaxed),
            do_move: self.do_move.load(Ordering::Relaxed),
            undo_move: self.undo_move.load(Ordering::Relaxed),
            do_null_move: self.do_null_move.load(Ordering::Relaxed),
            undo_null_move: self.undo_null_move.load(Ordering::Relaxed),
        }
    }
}

struct HookCallCounts {
    set_position: usize,
    do_move: usize,
    undo_move: usize,
    do_null_move: usize,
    undo_null_move: usize,
}

impl Evaluator for RecordingEvaluator {
    fn evaluate(&self, pos: &Position) -> i32 {
        self.inner.evaluate(pos)
    }

    fn on_set_position(&self, pos: &Position) {
        self.set_position.fetch_add(1, Ordering::Relaxed);
        self.inner.on_set_position(pos);
    }

    fn on_do_move(&self, pre_pos: &Position, mv: crate::shogi::Move) {
        self.do_move.fetch_add(1, Ordering::Relaxed);
        self.inner.on_do_move(pre_pos, mv);
    }

    fn on_undo_move(&self) {
        self.undo_move.fetch_add(1, Ordering::Relaxed);
        self.inner.on_undo_move();
    }

    fn on_do_null_move(&self, pre_pos: &Position) {
        self.do_null_move.fetch_add(1, Ordering::Relaxed);
        self.inner.on_do_null_move(pre_pos);
    }

    fn on_undo_null_move(&self) {
        self.undo_null_move.fetch_add(1, Ordering::Relaxed);
        self.inner.on_undo_null_move();
    }
}

#[test]
fn evaluator_hooks_balance_for_classic_backend() {
    let evaluator = Arc::new(RecordingEvaluator::default());
    let backend = ClassicBackend::with_profile(Arc::clone(&evaluator), SearchProfile::enhanced());

    // 初期局面を使用（探索枝が豊富で、NMPが確実に発動する）
    let pos = Position::startpos();

    let limits = SearchLimitsBuilder::default().depth(5).build();

    let _ = backend.think_blocking(&pos, &limits, None);

    let counts = evaluator.counts();
    assert!(counts.set_position >= 1, "expected on_set_position to be called");
    assert!(counts.do_move > 0, "expected on_do_move to be used during search");
    assert_eq!(counts.do_move, counts.undo_move, "move hooks must balance");
    assert!(counts.do_null_move > 0, "null move pruning should be exercised");
    assert_eq!(counts.do_null_move, counts.undo_null_move, "null-move hooks must balance");
}

struct PanicEvaluator;

impl Evaluator for PanicEvaluator {
    fn evaluate(&self, _pos: &Position) -> i32 {
        panic!("panic-evaluator invoked");
    }
}

#[test]
fn panic_in_search_thread_returns_error_result_with_stop_info() {
    time_management::mock_set_time(0);

    let evaluator = Arc::new(PanicEvaluator);
    let backend =
        Arc::new(ClassicBackend::with_profile(Arc::clone(&evaluator), SearchProfile::enhanced()));

    let time_limits = TimeLimits {
        time_control: TimeControl::FixedTime { ms_per_move: 1000 },
        ..Default::default()
    };
    let tm = Arc::new(TimeManager::new(&time_limits, Color::Black, 0, GamePhase::Opening));
    tm.override_limits_for_test(600, 800);

    // 経過時間を 250ms に設定してから探索を開始する。
    time_management::mock_advance_time(250);
    let expected_elapsed = tm.elapsed_ms();

    let mut limits = SearchLimits::builder().depth(1).build();
    limits.time_manager = Some(Arc::clone(&tm));
    limits.start_time = Instant::now() - Duration::from_millis(expected_elapsed);

    let active_counter = Arc::new(AtomicUsize::new(0));
    let task = backend.start_async(Position::startpos(), limits, None, Arc::clone(&active_counter));
    let (_stop, rx, handle) = task.into_parts();

    let result = rx.recv_timeout(Duration::from_millis(200)).expect("panic fallback result");

    if let Some(handle) = handle {
        handle.join().expect("search thread join");
    }

    assert_eq!(result.end_reason, TerminationReason::Error);
    let info = result.stop_info.expect("stop info should be present");
    assert_eq!(info.reason, TerminationReason::Error);
    assert_eq!(info.soft_limit_ms, 600);
    assert_eq!(info.hard_limit_ms, 800);
    assert!(!info.hard_timeout, "panic fallback should not mark hard timeout");
    let delta = info.elapsed_ms.abs_diff(expected_elapsed);
    assert!(
        delta <= 150,
        "elapsed_ms should stay close to expected (expected {expected_elapsed}, actual {} , delta {delta})",
        info.elapsed_ms
    );
    let stats_elapsed = result.stats.elapsed.as_millis() as u64;
    assert!(
        stats_elapsed.abs_diff(info.elapsed_ms) <= 5,
        "stats_elapsed={} info_elapsed={} diff={}",
        stats_elapsed,
        info.elapsed_ms,
        stats_elapsed.abs_diff(info.elapsed_ms)
    );
    assert_eq!(active_counter.load(Ordering::SeqCst), 0);
}

#[test]
fn stop_returns_latest_committed_root_line() {
    let evaluator = Arc::new(MaterialEvaluator);
    let backend =
        Arc::new(ClassicBackend::with_profile(Arc::clone(&evaluator), SearchProfile::enhanced()));

    let mut limits = SearchLimitsBuilder::default().fixed_time_ms(1500).build();
    let stop_flag = Arc::new(AtomicBool::new(false));
    limits.stop_flag = Some(Arc::clone(&stop_flag));

    let captured: Arc<Mutex<Vec<crate::search::types::RootLine>>> =
        Arc::new(Mutex::new(Vec::new()));
    let captured_cb = Arc::clone(&captured);
    limits.info_callback = Some(Arc::new(move |event: InfoEvent| {
        if let InfoEvent::PV { line } = event {
            captured_cb.lock().unwrap().push((*line).clone());
        }
    }));

    let active_counter = Arc::new(AtomicUsize::new(0));
    let task = backend.start_async(Position::startpos(), limits, None, Arc::clone(&active_counter));
    let (stop_handle, rx, handle) = task.into_parts();

    thread::sleep(Duration::from_millis(150));
    stop_handle.request_stop();

    let result = rx.recv_timeout(Duration::from_secs(2)).expect("search result after stop");

    if let Some(handle) = handle {
        handle.join().expect("search thread join");
    }
    assert_eq!(active_counter.load(Ordering::SeqCst), 0);

    assert!(result.stats.nodes > 0, "expected the search to explore nodes");
    let stable_line = result
        .lines
        .as_ref()
        .and_then(|lines| lines.first())
        .cloned()
        .expect("expected stable root line in result");
    assert!(matches!(stable_line.bound, NodeType::Exact));
    assert_eq!(result.best_move, Some(stable_line.root_move));
    assert_eq!(result.stats.root_report_source, Some(SnapshotSource::Stable));
}

#[test]
fn abdada_no_reduction_for_owner_side() {
    // Owner path: do not preset busy; the first entrant sets busy and must NOT reduce here.
    let evaluator = Arc::new(MaterialEvaluator);
    let tt = Arc::new(TranspositionTable::new(16));
    let backend = super::driver::ClassicBackend::with_tt(Arc::clone(&evaluator), Arc::clone(&tt));

    let pos = Position::startpos();

    let logs: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let logs_cb = Arc::clone(&logs);
    let limits = SearchLimitsBuilder::default()
        .depth(6)
        .info_string_callback(Arc::new(move |s: &str| {
            logs_cb.lock().unwrap().push(s.to_string());
        }))
        .build();

    let _ = backend.think_blocking(&pos, &limits, None);

    let collected = logs.lock().unwrap();
    let saw_reduce = collected.iter().any(|l| l.contains("abdada_cut_reduction=1 next_depth="));
    assert!(
        !saw_reduce,
        "owner side should not reduce on first busy set (no busy detection)"
    );
}

#[test]
fn abdada_no_reduction_when_depth_below_threshold() {
    // With preset busy but depth<6, ABDADA must not trigger
    let evaluator = Arc::new(MaterialEvaluator);
    let tt = Arc::new(TranspositionTable::new(16));
    let backend = super::driver::ClassicBackend::with_tt(Arc::clone(&evaluator), Arc::clone(&tt));

    let pos = Position::startpos();
    let hash = pos.zobrist_hash();
    tt.store(crate::search::tt::TTStoreArgs::new(
        hash,
        None::<crate::shogi::Move>,
        0,
        0,
        10,
        crate::search::NodeType::Exact,
        pos.side_to_move,
    ));
    let _ = tt.set_exact_cut(hash, pos.side_to_move);

    let logs: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let logs_cb = Arc::clone(&logs);
    let limits = SearchLimitsBuilder::default()
        .depth(5) // below ABDADA_MIN_DEPTH(6)
        .info_string_callback(Arc::new(move |s: &str| {
            logs_cb.lock().unwrap().push(s.to_string());
        }))
        .build();

    let _ = backend.think_blocking(&pos, &limits, None);

    let collected = logs.lock().unwrap();
    let saw_reduce = collected.iter().any(|l| l.contains("abdada_cut_reduction=1 next_depth="));
    assert!(
        !saw_reduce,
        "depth<6 should not trigger abdada reduction even when busy is preset"
    );
}

#[test]
fn stop_during_aspiration_returns_stable_snapshot() {
    let evaluator = Arc::new(MaterialEvaluator);
    let backend =
        Arc::new(ClassicBackend::with_profile(Arc::clone(&evaluator), SearchProfile::enhanced()));
    let base_position = make_aspiration_stress_position();

    let active_counter = Arc::new(AtomicUsize::new(0));
    let mut result = None;
    let mut last_failures = 0u32;

    for attempt in 0..6 {
        let stop_flag = Arc::new(AtomicBool::new(false));
        let flag_for_callback = stop_flag.clone();
        let limits = SearchLimitsBuilder::default()
            .fixed_time_ms(1500)
            .info_string_callback(Arc::new(move |msg: &str| {
                if msg.contains("aspiration fail") {
                    flag_for_callback.store(true, Ordering::Release);
                }
            }))
            .build();
        let mut limits = limits;
        limits.stop_flag = Some(stop_flag.clone());

        let task = backend.clone().start_async(
            base_position.clone(),
            limits,
            None,
            Arc::clone(&active_counter),
        );
        let (stop_handle, rx, handle) = task.into_parts();

        // Fallback stop to avoid hangs if aspiration never fails in this attempt.
        let fallback_flag = stop_handle.flag();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(600));
            fallback_flag.store(true, Ordering::Release);
        });

        let attempt_result = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("search result after aspiration stop");

        if let Some(handle) = handle {
            handle.join().expect("search thread join");
        }

        last_failures = attempt_result.stats.aspiration_failures.unwrap_or_default();
        result = Some(attempt_result);

        if last_failures > 0 || attempt == 5 {
            break;
        }
    }

    let result = result.expect("search attempts should produce a result");
    assert!(last_failures > 0, "aspiration failures expected within attempts");
    assert_eq!(active_counter.load(Ordering::SeqCst), 0);
    assert_eq!(result.stats.root_report_source, Some(SnapshotSource::Stable));
    assert_eq!(result.stats.stable_depth, Some(result.stats.depth));
    assert!(result.stats.incomplete_depth.is_some());
    assert!(result.stats.aspiration_failures.expect("aspiration failure counter present") > 0);
}

#[test]
fn multipv_incomplete_iteration_falls_back_to_stable_snapshot() {
    let evaluator = Arc::new(MaterialEvaluator);
    let backend =
        Arc::new(ClassicBackend::with_profile(Arc::clone(&evaluator), SearchProfile::enhanced()));
    let base_position = make_aspiration_stress_position();

    let stop_flag = Arc::new(AtomicBool::new(false));
    let flag_for_cb = stop_flag.clone();
    let limits = SearchLimitsBuilder::default()
        .fixed_time_ms(1500)
        .multipv(3)
        .info_string_callback(Arc::new(move |msg: &str| {
            if msg.contains("iter_start depth=4") {
                flag_for_cb.store(true, Ordering::Release);
            }
        }))
        .build();
    let mut limits = limits;
    limits.stop_flag = Some(stop_flag);

    let active_counter = Arc::new(AtomicUsize::new(0));
    let task = backend.start_async(base_position, limits, None, Arc::clone(&active_counter));
    let (stop_handle, rx, handle) = task.into_parts();

    // Backup timeout to ensure completion even if callback didn't fire.
    let fallback_flag = stop_handle.flag();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(600));
        fallback_flag.store(true, Ordering::Release);
    });

    let result = rx
        .recv_timeout(Duration::from_secs(2))
        .expect("search result after multipv stop");

    if let Some(handle) = handle {
        handle.join().expect("search thread join");
    }
    assert_eq!(active_counter.load(Ordering::SeqCst), 0);

    let lines = result.lines.expect("stable lines present");
    assert_eq!(lines.len(), 1, "incomplete depth should fall back to previous stable PV");
    assert_eq!(result.stats.root_report_source, Some(SnapshotSource::Stable));
    assert!(result.stats.incomplete_depth.is_some());
}

#[test]
fn fixed_time_limit_populates_stop_info() {
    let evaluator = Arc::new(MaterialEvaluator);
    let backend = ClassicBackend::with_profile(Arc::clone(&evaluator), SearchProfile::enhanced());
    let pos = Position::startpos();

    let limits = SearchLimitsBuilder::default().fixed_time_ms(50).build();

    let result = backend.think_blocking(&pos, &limits, None);
    let info = result
        .stop_info
        .expect("stop info should be present when only time_limit is provided");

    assert_eq!(info.soft_limit_ms, 50);
    assert_eq!(info.hard_limit_ms, 50);
    assert_eq!(info.reason, TerminationReason::TimeLimit);
    assert_eq!(result.end_reason, TerminationReason::TimeLimit);
    assert_eq!(result.end_reason, info.reason);
    let stats_elapsed = result.stats.elapsed.as_millis() as u64;
    assert!(
        stats_elapsed.abs_diff(info.elapsed_ms) <= 5,
        "stats_elapsed={} info_elapsed={} diff={}",
        stats_elapsed,
        info.elapsed_ms,
        stats_elapsed.abs_diff(info.elapsed_ms)
    );
    assert!(!info.hard_timeout, "lead windowでの停止はhard_timeout=falseのままにする");
}

struct SleepyEvaluator {
    delay: Duration,
}

impl Evaluator for SleepyEvaluator {
    fn evaluate(&self, _pos: &Position) -> i32 {
        thread::sleep(self.delay);
        0
    }
}

#[test]
fn fixed_time_limit_lead_window_marks_soft_reason() {
    let evaluator = Arc::new(SleepyEvaluator {
        delay: Duration::from_millis(15),
    });
    let backend = ClassicBackend::with_profile(Arc::clone(&evaluator), SearchProfile::enhanced());
    let pos = Position::startpos();

    // lead window が作動するよう適度な固定時間（40ms）と遅延 evaluator を併用する。
    let lead_triggered = Arc::new(AtomicBool::new(false));
    let callback_flag = Arc::clone(&lead_triggered);
    let limits = SearchLimitsBuilder::default()
        .fixed_time_ms(40)
        .info_string_callback(Arc::new(move |msg: &str| {
            if msg.contains("stop_lead_break") {
                callback_flag.store(true, Ordering::Relaxed);
            }
        }))
        .build();
    let result = backend.think_blocking(&pos, &limits, None);
    let info = result.stop_info.expect("stop info present");

    assert!(lead_triggered.load(Ordering::Relaxed), "lead window callback should have fired");
    assert_eq!(info.reason, TerminationReason::TimeLimit);
    assert!(!info.hard_timeout, "lead window経由の停止ではhard_timeout=false");
    assert_eq!(info.soft_limit_ms, 40);
    assert_eq!(info.hard_limit_ms, 40);
    assert_eq!(result.end_reason, TerminationReason::TimeLimit);
}

#[test]
fn fixed_time_limit_lead_window_notifies_finalize_once() {
    use crate::search::parallel::{FinalizeReason, FinalizerMsg, StopController};

    let evaluator = Arc::new(SleepyEvaluator {
        delay: Duration::from_millis(15),
    });
    let backend = ClassicBackend::with_profile(Arc::clone(&evaluator), SearchProfile::enhanced());
    let pos = Position::startpos();
    let controller = Arc::new(StopController::new());
    let (finalizer_tx, finalizer_rx) = mpsc::channel();
    controller.register_finalizer(finalizer_tx);

    let session_id = 123;
    controller.publish_session(None, session_id);

    let limits = SearchLimitsBuilder::default().session_id(session_id).fixed_time_ms(30).build();
    let mut limits = limits;
    limits.stop_controller = Some(controller.clone());

    // publish_session() で最初に SessionStart が流れるので消費しておく。
    let start_msg = finalizer_rx
        .recv_timeout(Duration::from_millis(100))
        .expect("SessionStart should arrive");
    match start_msg {
        FinalizerMsg::SessionStart { session_id: sid } => {
            assert_eq!(sid, session_id);
        }
        other => panic!("unexpected first message: {other:?}"),
    }

    let result = backend.think_blocking(&pos, &limits, None);
    let info = result.stop_info.expect("stop info present");
    assert_eq!(info.reason, TerminationReason::TimeLimit);
    assert!(!info.hard_timeout, "lead windowでの停止はhard_timeout=falseのままにする");

    let finalize_msg = finalizer_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("StopController should receive Planned finalize");
    match finalize_msg {
        FinalizerMsg::Finalize {
            session_id: sid,
            reason,
        } => {
            assert_eq!(sid, session_id);
            assert_eq!(reason, FinalizeReason::Planned);
        }
        other => panic!("unexpected finalize message: {other:?}"),
    }
    assert!(finalizer_rx.try_recv().is_err(), "finalize should fire exactly once");
}

#[test]
fn null_move_respects_runtime_toggle() {
    let evaluator = Arc::new(MaterialEvaluator);
    let backend =
        ClassicBackend::with_profile(Arc::clone(&evaluator), SearchProfile::enhanced_material());
    let pos = Position::startpos();
    let mut stack = vec![SearchStack::default(); crate::search::constants::MAX_PLY + 1];
    let mut heur = Heuristics::default();
    let mut tt_hits = 0;
    let mut beta_cuts = 0;
    let mut lmr_counter = 0;

    crate::search::params::set_nmp_enabled(true);
    let limits = SearchLimitsBuilder::default().depth(5).build();
    let start_time = Instant::now();
    let mut nodes = 0_u64;
    let mut seldepth = 0_u32;
    let mut qnodes = 0_u64;
    let qnodes_limit = crate::search::constants::DEFAULT_QNODES_LIMIT;
    let mut ctx = SearchContext {
        limits: &limits,
        start_time: &start_time,
        nodes: &mut nodes,
        seldepth: &mut seldepth,
        qnodes: &mut qnodes,
        qnodes_limit,
    };
    let static_eval = evaluator.evaluate(&pos);
    let allowed = backend.null_move_prune(NullMovePruneParams {
        toggles: &backend.profile.prune,
        depth: 4,
        pos: &pos,
        beta: 0,
        static_eval,
        ply: 0,
        stack: &mut stack,
        heur: &mut heur,
        tt_hits: &mut tt_hits,
        beta_cuts: &mut beta_cuts,
        lmr_counter: &mut lmr_counter,
        ctx: &mut ctx,
    });
    assert!(allowed.is_some(), "NMP should run when runtime toggle is enabled");

    crate::search::params::set_nmp_enabled(false);
    let mut stack_off = vec![SearchStack::default(); crate::search::constants::MAX_PLY + 1];
    let mut heur_off = Heuristics::default();
    let mut tt_hits_off = 0;
    let mut beta_cuts_off = 0;
    let mut lmr_counter_off = 0;
    let start_time_off = Instant::now();
    let mut nodes_off = 0_u64;
    let mut seldepth_off = 0_u32;
    let mut qnodes_off = 0_u64;
    let mut ctx_off = SearchContext {
        limits: &limits,
        start_time: &start_time_off,
        nodes: &mut nodes_off,
        seldepth: &mut seldepth_off,
        qnodes: &mut qnodes_off,
        qnodes_limit,
    };
    let denied = backend.null_move_prune(NullMovePruneParams {
        toggles: &backend.profile.prune,
        depth: 4,
        pos: &pos,
        beta: 0,
        static_eval,
        ply: 0,
        stack: &mut stack_off,
        heur: &mut heur_off,
        tt_hits: &mut tt_hits_off,
        beta_cuts: &mut beta_cuts_off,
        lmr_counter: &mut lmr_counter_off,
        ctx: &mut ctx_off,
    });
    assert!(denied.is_none(), "NMP must be disabled when runtime toggle is off");

    crate::search::params::set_nmp_enabled(true);
}
#[test]
fn excluded_drop_only_blocks_same_piece_type() {
    let mut pos = Position::empty();
    pos.side_to_move = Color::Black;
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));
    pos.hands[Color::Black as usize][PieceType::Pawn.hand_index().unwrap()] = 1;
    pos.hands[Color::Black as usize][PieceType::Gold.hand_index().unwrap()] = 1;

    // Exclude a pawn drop; gold drop to same square should remain available
    let excluded = Some(parse_usi_move("P*5f").unwrap());
    let target = parse_usi_square("5f").unwrap();
    let mut picker = MovePicker::new_normal(&pos, None, excluded, [None, None], None, None);
    let heur = Heuristics::default();
    let mut found_gold = false;

    while let Some(mv) = picker.next(&heur) {
        if mv.is_drop() && mv.drop_piece_type() == PieceType::Gold && mv.to() == target {
            found_gold = true;
            break;
        }
    }

    assert!(found_gold, "gold drop should not be excluded when pawn drop is excluded");
}

#[test]
fn generate_evasions_matches_all_single_check() {
    let mut pos = Position::empty();
    pos.side_to_move = Color::Black;
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::Rook, Color::White));
    pos.board
        .put_piece(parse_usi_square("4h").unwrap(), Piece::new(PieceType::Silver, Color::Black));

    let mg = MoveGenerator::new();
    let all_moves = mg.generate_all(&pos).unwrap();
    let evasion_moves = mg.generate_evasions(&pos).unwrap();

    assert!(!evasion_moves.is_empty(), "expected evasions when side is in check");

    let all_keys: SmallVec<[u32; 64]> =
        all_moves.as_slice().iter().map(|&mv| mv.to_u32()).collect();
    let evasion_keys: SmallVec<[u32; 64]> =
        evasion_moves.as_slice().iter().map(|&mv| mv.to_u32()).collect();

    let mut all_sorted = all_keys.clone();
    all_sorted.sort_unstable();
    let mut evasion_sorted = evasion_keys.clone();
    evasion_sorted.sort_unstable();

    assert_eq!(all_sorted, evasion_sorted);
}

#[test]
fn generate_evasions_double_check_only_king_moves() {
    let mut pos = Position::empty();
    pos.side_to_move = Color::Black;
    let king_sq = parse_usi_square("5i").unwrap();
    pos.board.put_piece(king_sq, Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::Rook, Color::White));
    pos.board
        .put_piece(parse_usi_square("1e").unwrap(), Piece::new(PieceType::Bishop, Color::White));

    let mg = MoveGenerator::new();
    let all_moves = mg.generate_all(&pos).unwrap();
    let evasion_moves = mg.generate_evasions(&pos).unwrap();

    assert!(!evasion_moves.is_empty(), "expected evasions when in double check");
    for mv in evasion_moves.as_slice() {
        assert_eq!(mv.from(), Some(king_sq), "double check must yield only king moves");
    }

    let all_keys: SmallVec<[u32; 32]> =
        all_moves.as_slice().iter().map(|&mv| mv.to_u32()).collect();
    let evasion_keys: SmallVec<[u32; 32]> =
        evasion_moves.as_slice().iter().map(|&mv| mv.to_u32()).collect();

    let mut all_sorted = all_keys.clone();
    all_sorted.sort_unstable();
    let mut evasion_sorted = evasion_keys.clone();
    evasion_sorted.sort_unstable();

    assert_eq!(all_sorted, evasion_sorted);
}
