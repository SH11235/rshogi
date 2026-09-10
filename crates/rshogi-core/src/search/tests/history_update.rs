//! alpha_beta の完了処理が実際の履歴 entry を更新することを観測する。
use crate::eval::EvalHash;
use crate::nnue::{
    AccumulatorStackVariant, halfka_split::HalfKaSplitStack,
    network_halfka_split::AccumulatorStackHalfKaSplit,
};
use crate::position::Position;
use crate::search::{
    ContHistKey, LimitsType, NodeType, RootMoves, SearchTuneParams, SearchWorker, Stack,
    TimeManagement,
    history::{ContinuationHistory, PawnHistory},
};
use crate::tt::TranspositionTable;
use crate::types::{Bound, DEPTH_QS, Move, Piece, Square, Value};
use std::sync::{Arc, atomic::AtomicBool};

fn observe_completed_node(
    sfen: &str,
    best_usi: &str,
    tt_is_best: bool,
    sentinel_move: Option<Move>,
    tune: SearchTuneParams,
) {
    let mut pos = Position::new();
    pos.set_sfen(sfen).unwrap();
    let alpha = crate::eval::material::evaluate_material(&pos);
    let beta = alpha + Value::new(1);
    let before_pos = (pos.to_sfen(), pos.key());
    let best = pos.to_move(Move::from_usi(best_usi).unwrap()).unwrap();
    assert!(pos.is_legal(best));
    let legal = RootMoves::from_legal_moves(&pos, &[]);
    let other = legal.iter().map(|rm| rm.pv[0]).find(|mv| *mv != best && !mv.is_pass()).unwrap();
    let tt_move = if tt_is_best { best } else { other };
    let mut worker = SearchWorker::new(
        Arc::new(TranspositionTable::new(1)),
        Arc::new(EvalHash::new(1)),
        0,
        0,
        tune,
    );
    let limits = LimitsType {
        depth: 1,
        ..Default::default()
    };
    worker.prepare_search(&limits);
    worker.state.root_depth = 8;
    worker.state.nnue_stack = AccumulatorStackVariant::HalfKaSplit(HalfKaSplitStack::L256(
        AccumulatorStackHalfKaSplit::new(),
    ));
    let ply = 6;
    let keys: [ContHistKey; 6] = std::array::from_fn(|i| {
        ContHistKey::new(i % 2 == 0, i % 3 == 0, Piece::W_PAWN, Square::from_u8(i as u8).unwrap())
    });
    for (i, key) in keys.iter().enumerate() {
        let prev = ply - i as i32 - 1;
        worker.set_cont_history_for_move(prev, key.in_check, key.capture, key.piece, key.to);
        worker.state.stack[prev as usize].current_move = other;
    }
    if let Some(mv) = sentinel_move {
        worker.clear_cont_history_for_null(ply - 1);
        worker.state.stack[(ply - 1) as usize].current_move = mv;
    }
    // 子 qsearch は確定 TT 値で返り、NNUE や追加の履歴更新を実行しない。
    for rm in legal.iter() {
        let mv = rm.pv[0];
        let mut child = pos.clone();
        let check = child.gives_check(mv);
        child.do_move(mv, check);
        let score = -(alpha.raw() + if mv == best { 100 } else { -100 });
        let _ = worker.tt.probe(child.key(), &child).write(
            child.key(),
            Value::new(score),
            false,
            Bound::Exact,
            DEPTH_QS,
            Move::NONE,
            Value::ZERO,
            worker.tt.generation(),
        );
    }
    let _ = worker.tt.probe(pos.key(), &pos).write(
        pos.key(),
        Value::NONE,
        false,
        Bound::None,
        0,
        tt_move,
        Value::ZERO,
        worker.tt.generation(),
    );
    let pc = best.moved_piece_after();
    let to = best.to();
    let us = pos.side_to_move();
    let pawn_idx = pos.pawn_history_index();
    let capture = pos.capture_stage(best);
    let captured = capture.then(|| pos.piece_on(to).piece_type());
    // SAFETY: 探索していない間だけ owner の読み取り参照を保持する。
    let before = {
        let h = unsafe { worker.history.as_ref_unchecked() };
        (
            h.tt_move_history.get(),
            h.main_history.get(us, best),
            h.pawn_history.get(pawn_idx, pc, to),
            keys.map(|key| {
                h.continuation_history[key.in_check as usize][key.capture as usize]
                    .get(key.piece, key.to, pc, to)
            }),
            h.continuation_history[0][0].get(Piece::NONE, Square::SQ_11, pc, to),
            captured.map(|pt| h.capture_history.get(pc, to, pt)),
            h.main_history.get(us, other),
        )
    };
    let original_pc = pos.moved_piece(best);
    // SAFETY: 同じく探索前の短い読み取りのみ。
    let original_before = {
        let h = unsafe { worker.history.as_ref_unchecked() };
        (
            h.pawn_history.get(pawn_idx, original_pc, to),
            keys.map(|key| {
                h.continuation_history[key.in_check as usize][key.capture as usize].get(
                    key.piece,
                    key.to,
                    original_pc,
                    to,
                )
            }),
            captured.map(|pt| h.capture_history.get(original_pc, to, pt)),
        )
    };
    assert_eq!(before.0, 0);
    let mut tm =
        TimeManagement::new(Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
    let value = worker.search_node_wrapper::<{ NodeType::NonPV as u8 }>(
        &mut pos, 1, alpha, beta, ply, true, &limits, &mut tm,
    );
    assert!(value >= beta);
    assert!(!worker.state.abort);
    assert_eq!((pos.to_sfen(), pos.key()), before_pos);
    let stored = worker.tt.probe(pos.key(), &pos);
    assert_eq!(stored.data.mv, best);
    // SAFETY: 探索完了後で可変参照と同時保持しない。
    let h = unsafe { worker.history.as_ref_unchecked() };
    assert_eq!(
        h.tt_move_history.get() as i32,
        if tt_is_best {
            tune.tt_move_history_bonus
        } else {
            tune.tt_move_history_malus
        }
    );
    if let Some(pt) = captured {
        assert!(h.capture_history.get(pc, to, pt) > before.5.unwrap());
        assert_eq!(h.main_history.get(us, best), before.1);
        assert_eq!(h.pawn_history.get(pawn_idx, pc, to), before.2);
    } else {
        assert!(h.main_history.get(us, best) > before.1);
        assert!(h.pawn_history.get(pawn_idx, pc, to) > before.2);
    }
    for (i, key) in keys.iter().enumerate() {
        let after = h.continuation_history[key.in_check as usize][key.capture as usize]
            .get(key.piece, key.to, pc, to);
        let updated = crate::search::history::continuation_history_weight(&tune, i + 1) > 0
            && !capture
            && (!pos.in_check() || i < 2)
            && !(sentinel_move.is_some() && i == 0);
        if updated {
            assert!(after > before.3[i], "continuation back {}", i + 1);
        } else {
            assert_eq!(after, before.3[i], "excluded continuation back {}", i + 1);
        }
    }
    if best.is_promotion() {
        let original_pc = pos.moved_piece(best);
        assert_ne!(original_pc, pc);
        assert_eq!(h.pawn_history.get(pawn_idx, original_pc, to), original_before.0);
        for (i, key) in keys.iter().enumerate() {
            assert_eq!(
                h.continuation_history[key.in_check as usize][key.capture as usize].get(
                    key.piece,
                    key.to,
                    original_pc,
                    to
                ),
                original_before.1[i]
            );
        }
        if let Some(pt) = captured {
            assert_eq!(h.capture_history.get(original_pc, to, pt), original_before.2.unwrap());
        }
    }
    assert_eq!(h.continuation_history[0][0].get(Piece::NONE, Square::SQ_11, pc, to), before.4);
    if !tt_is_best && !pos.capture_stage(other) {
        assert!(h.main_history.get(us, other) < before.6);
    }
}

#[test]
fn completed_search_observes_history_entries() {
    let guard = crate::eval::material::test_support::lock_material();
    crate::eval::set_material_level(crate::eval::MaterialLevel::Lv1);
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            let quiet = "8k/9/9/9/4P4/9/9/9/K8 b - 1";
            for tune in [
                SearchTuneParams::default(),
                SearchTuneParams {
                    tt_move_history_bonus: 129,
                    tt_move_history_malus: -97,
                    continuation_history_weight_3: 0,
                    ..SearchTuneParams::default()
                },
            ] {
                for tt_best in [true, false] {
                    observe_completed_node(quiet, "5e5d", tt_best, None, tune);
                }
            }
            observe_completed_node(
                "8k/9/4S4/9/9/9/9/9/K8 b - 1",
                "5c5b+",
                true,
                None,
                SearchTuneParams::default(),
            );
            observe_completed_node(
                "8k/4p4/4R4/9/9/9/9/9/K8 b - 1",
                "5c5b+",
                true,
                None,
                SearchTuneParams::default(),
            );
            observe_completed_node(
                "k8/9/9/9/9/9/9/4r4/4K4 b - 1",
                "5i4i",
                true,
                None,
                SearchTuneParams::default(),
            );
            for mv in [Move::NULL, Move::PASS] {
                observe_completed_node(quiet, "5e5d", true, Some(mv), SearchTuneParams::default());
            }
        })
        .unwrap()
        .join()
        .unwrap();
    drop(guard);
}

// =============================================================================
// ContinuationHistory TDDテスト
// =============================================================================

/// ContinuationHistory: 基本的な更新が機能することを確認
#[test]
fn continuation_history_basic_update() {
    // ContinuationHistoryは大きいのでBoxで作成（スタックオーバーフロー防止）
    let mut cont_hist = ContinuationHistory::new_boxed();
    let prev_pc = Piece::B_PAWN;
    // SAFETY: 60は有効なSquareインデックス
    let prev_to = unsafe { Square::from_u8_unchecked(60) }; // 7六相当
    let pc = Piece::B_PAWN;
    // SAFETY: 51は有効なSquareインデックス
    let to = unsafe { Square::from_u8_unchecked(51) }; // 7五相当

    // 初期値は0
    assert_eq!(cont_hist.get(prev_pc, prev_to, pc, to), 0);

    // 更新
    cont_hist.update(prev_pc, prev_to, pc, to, 100);

    // 値が増加
    let value = cont_hist.get(prev_pc, prev_to, pc, to);
    assert!(value > 0, "ContinuationHistory should increase after update");
}

// =============================================================================
// ContHistKey TDDテスト
// =============================================================================

/// ContHistKeyが正しく構築されることを確認
#[test]
fn cont_hist_key_construction() {
    let key = ContHistKey::new(true, false, Piece::B_GOLD, Square::SQ_55);

    assert!(key.in_check);
    assert!(!key.capture);
    assert_eq!(key.piece, Piece::B_GOLD);
    assert_eq!(key.to, Square::SQ_55);
}

/// Stack.cont_hist_keyがOption<ContHistKey>として正しく動作することを確認
#[test]
fn stack_cont_hist_key_option() {
    let mut stack = Stack::default();

    // 初期値はNone
    assert!(stack.cont_hist_key.is_none());

    // 設定
    // SAFETY: 22は有効なSquareインデックス
    let sq = unsafe { Square::from_u8_unchecked(22) };
    stack.cont_hist_key = Some(ContHistKey::new(false, true, Piece::W_SILVER, sq));

    // 取得
    let key = stack.cont_hist_key.unwrap();
    assert!(!key.in_check);
    assert!(key.capture);
    assert_eq!(key.piece, Piece::W_SILVER);
    assert_eq!(key.to, sq);
}

// =============================================================================
// PawnHistory TDDテスト
// =============================================================================

/// PawnHistory: 基本的な更新が機能することを確認
#[test]
fn pawn_history_basic_update() {
    let mut history = PawnHistory::new_boxed();
    let pawn_idx = 42; // 任意のインデックス
    let pc = Piece::B_PAWN;
    // SAFETY: 60は有効なSquareインデックス
    let to = unsafe { Square::from_u8_unchecked(60) }; // 7六相当

    // 初期値は0
    assert_eq!(history.get(pawn_idx, pc, to), 0);

    // 更新
    history.update(pawn_idx, pc, to, 200);

    // 値が増加
    let value = history.get(pawn_idx, pc, to);
    assert!(value > 0, "PawnHistory should increase after update");
}
