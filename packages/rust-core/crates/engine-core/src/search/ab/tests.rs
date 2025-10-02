use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use crate::evaluation::evaluate::{Evaluator, MaterialEvaluator};
use crate::search::api::SearcherBackend;
use crate::search::constants::SEARCH_INF;
use crate::search::limits::SearchLimitsBuilder;
use crate::search::mate_score;
use crate::search::SearchLimits;
use crate::shogi::{Color, Piece, PieceType};
use crate::usi::parse_usi_square;
use crate::Position;

use super::driver::ClassicBackend;
use super::pvs::SearchContext;
use super::SearchProfile;

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
    let mut ctx = SearchContext {
        limits: &limits,
        start_time: &start_time,
        nodes: &mut nodes,
        seldepth: &mut seldepth,
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

    let total_line_nodes: u64 = lines.iter().filter_map(|l| l.nodes).sum();
    assert!(total_line_nodes > 0, "line nodes should accumulate positive work");
    assert!(
        total_line_nodes <= result.stats.nodes,
        "line nodes should not exceed total nodes"
    );

    for line in lines.iter() {
        if let Some(n) = line.nodes {
            assert!(n > 0, "each line should report positive nodes");
            assert!(n <= result.stats.nodes);
        }
        if let Some(ms) = line.time_ms {
            assert!(ms <= result.stats.elapsed.as_millis() as u64);
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
    assert!(profile.prune.enable_nmp);
    assert!(!profile.prune.enable_iid);
    assert!(!profile.prune.enable_razor);
    assert!(!profile.prune.enable_probcut);
    assert!(profile.prune.enable_static_beta_pruning);
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
    // 分岐数を絞った局面（両玉と金のみ）でフック呼び出しの整合性を検証する。
    // 深さ4の探索でもノード爆発を抑え、テスト実行時間を短縮することが目的。
    let mut pos = Position::empty();
    pos.side_to_move = Color::Black;
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("9i").unwrap(), Piece::new(PieceType::Gold, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));
    pos.board
        .put_piece(parse_usi_square("1a").unwrap(), Piece::new(PieceType::Gold, Color::White));
    pos.hash = pos.compute_hash();
    pos.zobrist_hash = pos.hash;
    let limits = SearchLimitsBuilder::default().depth(4).build();

    let _ = backend.think_blocking(&pos, &limits, None);

    let counts = evaluator.counts();
    assert!(counts.set_position >= 1, "expected on_set_position to be called");
    assert!(counts.do_move > 0, "expected on_do_move to be used during search");
    assert_eq!(counts.do_move, counts.undo_move, "move hooks must balance");
    assert!(counts.do_null_move > 0, "null move pruning should be exercised");
    assert_eq!(counts.do_null_move, counts.undo_null_move, "null-move hooks must balance");
}
