//! alpha_beta モジュールのテスト

use std::sync::Arc;

use crate::eval::EvalHash;
use crate::search::alpha_beta::{
    DepthLivenessState, SearchWorker, build_reductions, enforce_decreasing_depth, reduction,
    should_activate_depth_liveness,
};
use crate::search::{LimitsType, SearchTuneParams};
use crate::tt::TranspositionTable;

#[test]
fn test_reduction_values() {
    // reduction(true, 10, 5) などが正の値を返すことを確認
    let tune = SearchTuneParams::default();
    let reductions = build_reductions(tune.lmr_table_coeff);
    let root_delta = 64;
    let delta = 32;
    assert!(reduction(&reductions, &tune, true, 10, 5, delta, root_delta) / 1024 >= 0);
    assert!(
        reduction(&reductions, &tune, false, 10, 5, delta, root_delta) / 1024
            >= reduction(&reductions, &tune, true, 10, 5, delta, root_delta) / 1024
    );
}

#[test]
fn test_reduction_bounds() {
    // 境界値テスト
    let tune = SearchTuneParams::default();
    let reductions = build_reductions(tune.lmr_table_coeff);
    let root_delta = 64;
    let delta = 32;
    assert_eq!(reduction(&reductions, &tune, true, 0, 0, delta, root_delta), 0); // depth=0, mc=0 は計算外
    assert!(reduction(&reductions, &tune, true, 63, 63, delta, root_delta) / 1024 < 64);
    assert!(reduction(&reductions, &tune, false, 63, 63, delta, root_delta) / 1024 < 64);
}

/// depth/move_countが大きい場合にreductionが正の値を返すことを確認
#[test]
fn test_reduction_returns_nonzero_for_large_values() {
    let tune = SearchTuneParams::default();
    let reductions = build_reductions(tune.lmr_table_coeff);
    let root_delta = 64;
    let delta = 32;
    // 深い探索で多くの手を試した場合、reductionは正の値であるべき
    let r = reduction(&reductions, &tune, false, 10, 10, delta, root_delta) / 1024;
    assert!(
        r > 0,
        "reduction should return positive value for depth=10, move_count=10, got {r}"
    );

    // improving=trueの場合は若干小さい値になる
    let r_imp = reduction(&reductions, &tune, true, 10, 10, delta, root_delta) / 1024;
    assert!(r >= r_imp, "non-improving should have >= reduction than improving");
}

/// 境界ケース: depth=1, move_count=1でもreduction関数が動作することを確認
#[test]
fn test_reduction_small_values() {
    let tune = SearchTuneParams::default();
    let reductions = build_reductions(tune.lmr_table_coeff);
    let root_delta = 64;
    let delta = 32;
    // 小さな値でもpanicしないことを確認
    let r = reduction(&reductions, &tune, true, 1, 1, delta, root_delta) / 1024;
    assert!(r >= 0, "reduction should not be negative");
}

#[test]
fn test_reduction_extremes_no_overflow() {
    let tune = SearchTuneParams::default();
    let reductions = build_reductions(tune.lmr_table_coeff);
    // 最大depth/mcでもオーバーフローせずに値が得られることを確認
    let delta = 0;
    let root_delta = 1;
    let r = reduction(&reductions, &tune, false, 63, 63, delta, root_delta);
    assert!(
        (0..i32::MAX / 2).contains(&r),
        "reduction extreme should be in safe range, got {r}"
    );
}

#[test]
fn test_reduction_zero_root_delta_clamped() {
    let tune = SearchTuneParams::default();
    let reductions = build_reductions(tune.lmr_table_coeff);
    // root_delta=0 を渡しても内部で1にクランプされることを確認
    let r = reduction(&reductions, &tune, false, 10, 10, 0, 0) / 1024;
    assert!(r >= 0, "reduction should clamp root_delta to >=1 even when 0 is passed");
}

#[test]
fn test_depth_liveness_activation_requires_node_and_run_thresholds() {
    assert!(!should_activate_depth_liveness(99_999, 0, 8, 100_000, 8));
    assert!(!should_activate_depth_liveness(100_000, 0, 7, 100_000, 8));
    assert!(should_activate_depth_liveness(100_000, 0, 8, 100_000, 8));

    // root search が go の途中から始まっても、試行内の消費 node だけを数える。
    assert!(!should_activate_depth_liveness(149_999, 50_000, 8, 100_000, 8));
    assert!(should_activate_depth_liveness(150_000, 50_000, 8, 100_000, 8));

    // どちらかの閾値が0なら同一バイナリの無効化条件になる。
    assert!(!should_activate_depth_liveness(u64::MAX, 0, 256, 0, 8));
    assert!(!should_activate_depth_liveness(u64::MAX, 0, 256, 100_000, 0));
}

#[test]
fn test_depth_liveness_enforces_strict_progress_only_after_activation() {
    assert_eq!(enforce_decreasing_depth(5, 4, false), 5);
    assert_eq!(enforce_decreasing_depth(4, 4, false), 4);
    assert_eq!(enforce_decreasing_depth(5, 4, true), 3);
    assert_eq!(enforce_decreasing_depth(4, 4, true), 3);
    assert_eq!(enforce_decreasing_depth(2, 4, true), 2);
    assert_eq!(enforce_decreasing_depth(1, 1, true), 0);
}

#[test]
fn test_depth_liveness_update_depth_counts_runs_and_enforces_after_activation() {
    let mut liveness = DepthLivenessState::new();
    let (node_thr, run_thr) = (100, 3);

    // root (ply=0) は real child ではないので run は増えない。
    assert_eq!(liveness.update_depth(0, 5, 0, false, node_thr, run_thr), 5);
    assert_eq!(liveness.snapshot_ply(0), (5, 0));

    // 非減少 edge (child depth >= parent entry depth) が連続すると run が積み上がる。
    assert_eq!(liveness.update_depth(200, 5, 1, false, node_thr, run_thr), 5);
    assert_eq!(liveness.snapshot_ply(1), (5, 1));
    assert_eq!(liveness.update_depth(200, 5, 2, false, node_thr, run_thr), 5);
    assert_eq!(liveness.snapshot_ply(2), (5, 2));

    // run が閾値に達すると発火し、以降の実手 child は parent entry depth - 1 に切り詰められる。
    assert_eq!(liveness.update_depth(200, 5, 3, false, node_thr, run_thr), 4);
    assert_eq!(liveness.snapshot_ply(3), (4, 0));
    assert_eq!(liveness.update_depth(200, 5, 4, false, node_thr, run_thr), 3);

    // depth が減る edge では run がリセットされる (発火前の状態で確認)。
    let mut liveness = DepthLivenessState::new();
    liveness.update_depth(0, 5, 0, false, node_thr, run_thr);
    liveness.update_depth(200, 5, 1, false, node_thr, run_thr);
    assert_eq!(liveness.update_depth(200, 4, 2, false, node_thr, run_thr), 4);
    assert_eq!(liveness.snapshot_ply(2), (4, 0));

    // excluded (SE verification) は別 root 扱いで、run にも enforcement にも関与しない。
    let run_before = liveness.snapshot_ply(1);
    assert_eq!(liveness.update_depth(200, 3, 1, true, node_thr, run_thr), 3);
    assert_eq!(liveness.snapshot_ply(1), (3, 0));
    liveness.restore_ply(1, run_before);
    assert_eq!(liveness.snapshot_ply(1), run_before);
}

#[test]
fn test_depth_liveness_verification_snapshot_restores_outer_path() {
    let mut liveness = DepthLivenessState::new();
    let (node_thr, run_thr) = (100, 8);

    liveness.update_depth(0, 16, 0, false, node_thr, run_thr);
    liveness.update_depth(200, 15, 1, false, node_thr, run_thr);
    let outer = liveness.snapshot_ply(1);
    assert_eq!(outer, (15, 0));

    // NMP verification は excluded_move を立てずに同一 ply で浅い depth を再探索し、
    // 追跡フィールドを上書きする。
    liveness.update_depth(200, 4, 1, false, node_thr, run_thr);
    assert_eq!(liveness.snapshot_ply(1), (4, 0));

    // 復元しないと、外側の子 (ply=2, depth=14) が entry depth 4 と比較され
    // 偽の非減少 edge として run を進めてしまう。
    liveness.update_depth(200, 14, 2, false, node_thr, run_thr);
    assert_eq!(liveness.snapshot_ply(2), (14, 1));

    liveness.restore_ply(1, outer);
    assert_eq!(liveness.snapshot_ply(1), (15, 0));
    liveness.update_depth(200, 14, 2, false, node_thr, run_thr);
    assert_eq!(liveness.snapshot_ply(2), (14, 0));
}

#[test]
fn test_depth_liveness_verification_root_mark_skips_run_and_enforcement() {
    let mut liveness = DepthLivenessState::new();
    let (node_thr, run_thr) = (100, 1);

    // 発火前 (node 閾値未達) に multi-extension 相当の非減少 edge で entry 16 > parent 15 を作る。
    liveness.update_depth(50, 15, 0, false, node_thr, run_thr);
    assert_eq!(liveness.update_depth(50, 16, 1, false, node_thr, run_thr), 16);
    assert_eq!(liveness.snapshot_ply(1), (16, 1));

    // node 閾値超過後の same-ply verification entry (depth 15 >= parent entry 15)。
    // mark が無いと非減少 edge と誤認され、ここが最初の発火点になって 14 へ clamp される。
    liveness.mark_same_ply_verification_root();
    assert_eq!(
        liveness.update_depth(200, 15, 1, false, node_thr, run_thr),
        15,
        "mark 付き verification root は run 判定・enforcement の対象外"
    );
    assert_eq!(liveness.snapshot_ply(1), (15, 0));

    // verification entry で guard が発火していないことを、node 閾値未達の非減少 edge が
    // clamp されない (発火済みなら閾値によらず enforcement される) ことで確認する。
    assert_eq!(
        liveness.update_depth(50, 15, 2, false, node_thr, run_thr),
        15,
        "verification root では guard は発火しない"
    );
    // mark は一回性: 上の ply=2 entry は通常の実手 child として run が数えられている。
    assert_eq!(liveness.snapshot_ply(2), (15, 1));
}

#[test]
fn test_nmp_verification_restores_depth_liveness_in_production_path() {
    use std::cell::Cell;
    use std::sync::atomic::AtomicBool;

    use crate::position::Position;
    use crate::search::alpha_beta::SearchContext;
    use crate::search::pruning::try_null_move_pruning;
    use crate::search::{NodeType, TimeManagement};
    use crate::types::{Move, Value};

    let tt = Arc::new(TranspositionTable::new(16));
    let eval_hash = Arc::new(EvalHash::new(1));
    let mut worker = SearchWorker::new(tt, eval_hash, 0, 0, SearchTuneParams::default());
    // r = 1 + 16/32 = 1 にして verification depth 15 >= parent entry 15 の
    // 非減少 same-ply entry を作る (verification root 除外が無いと clamp される設定)。
    worker
        .search_tune_params
        .set_from_usi_name("SPSA_NMP_REDUCTION_BASE", 1)
        .unwrap();
    worker
        .search_tune_params
        .set_from_usi_name("SPSA_NMP_REDUCTION_DEPTH_DIV", 32)
        .unwrap();
    let mut fixed_depth = LimitsType::new();
    fixed_depth.depth = 20;
    worker.prepare_search(&fixed_depth);

    let mut pos = Position::new();
    pos.set_hirate();

    // 外側 move path: root (ply=0) entry 15 と、multi-extension 相当で entry 16 になった
    // NMP ノード自身 (ply=1) を、node 閾値未達 (guard 未発火) の時点で記録する。
    let (node_thr, run_thr) = (100, 1);
    worker.state.nodes = 50;
    worker.state.depth_liveness_update_for_test(15, 0, node_thr, run_thr);
    worker.state.depth_liveness_update_for_test(16, 1, node_thr, run_thr);
    let outer = worker.state.depth_liveness_snapshot(1).unwrap();
    assert_eq!(outer, (16, 1));
    // null search 中に node 閾値を跨いだ状況を再現する。
    worker.state.nodes = 200;

    let ctx = SearchContext {
        tt: &worker.tt,
        eval_hash: &worker.eval_hash,
        history: &worker.history,
        cont_history_sentinel: worker.cont_history_sentinel,
        generate_all_legal_moves: worker.generate_all_legal_moves,
        max_moves_to_draw: worker.max_moves_to_draw,
        thread_id: worker.thread_id,
        allow_tt_write: worker.allow_tt_write,
        tune_params: &worker.search_tune_params,
        reductions: &worker.reductions,
        draw_value_table: worker.draw_value_table,
    };
    let mut time_manager =
        TimeManagement::new(Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));

    // depth >= nmp_verification_depth_threshold かつ null search が beta を上回る状況を作り、
    // verification search (同一 ply の再帰) まで到達させる。margin は depth 16 の既定
    // パラメータで負になるため、static_eval は beta より十分高くしておく。
    let beta = Value::new(100);
    let static_eval = Value::new(1000);
    let depth = 16;
    let calls = Cell::new(0);
    let verification_snapshot = Cell::new(None);
    let (value, _improving) = try_null_move_pruning::<{ NodeType::NonPV as u8 }, _>(
        &mut worker.state,
        &ctx,
        &mut pos,
        depth,
        beta,
        1,
        true,
        false,
        static_eval,
        false,
        Move::NONE,
        &fixed_depth,
        &mut time_manager,
        |st, _ctx, _pos, child_depth, _alpha, _beta, ply, _cut_node, _limits, _tm| {
            calls.set(calls.get() + 1);
            if ply == 1 {
                // verification search: 実際の search_node と同様に同一 ply の追跡を更新する。
                assert_eq!(child_depth, 15, "r=1 で verification depth は 15 のはず");
                let entered =
                    st.depth_liveness_update_for_test(child_depth, ply, node_thr, run_thr);
                assert_eq!(
                    entered, child_depth,
                    "verification root は非減少 same-ply entry でも clamp されないこと"
                );
                verification_snapshot.set(st.depth_liveness_snapshot(1));
                beta
            } else {
                -beta
            }
        },
    );

    assert_eq!(calls.get(), 2, "null search と verification search の両方を通ること");
    assert!(value.is_some(), "verification が beta を上回れば NMP cutoff が成立すること");
    let corrupted = verification_snapshot.get().unwrap();
    assert_ne!(corrupted, outer, "verification search は同一 ply の追跡を実際に上書きすること");
    assert_eq!(
        worker.state.depth_liveness_snapshot(1).unwrap(),
        outer,
        "try_null_move_pruning は verification 後に外側 ply の追跡を復元すること"
    );
    assert!(
        !worker.state.depth_liveness_is_active_for_test(),
        "verification entry を発火点にしないこと"
    );
}

#[test]
fn test_nmp_qsearch_verification_leaves_no_stale_mark() {
    use std::cell::Cell;
    use std::sync::atomic::AtomicBool;

    use crate::position::Position;
    use crate::search::alpha_beta::SearchContext;
    use crate::search::pruning::try_null_move_pruning;
    use crate::search::{NodeType, TimeManagement};
    use crate::types::{Move, Value};

    let tt = Arc::new(TranspositionTable::new(16));
    let eval_hash = Arc::new(EvalHash::new(1));
    let mut worker = SearchWorker::new(tt, eval_hash, 0, 0, SearchTuneParams::default());
    // r = 32 で verification depth を負にし、search_node が liveness 更新前に
    // qsearch へ早期 return する経路を作る (マークが消費されない状況)。
    worker
        .search_tune_params
        .set_from_usi_name("SPSA_NMP_REDUCTION_BASE", 32)
        .unwrap();
    worker
        .search_tune_params
        .set_from_usi_name("SPSA_NMP_REDUCTION_DEPTH_DIV", 32)
        .unwrap();
    let mut fixed_depth = LimitsType::new();
    fixed_depth.depth = 20;
    worker.prepare_search(&fixed_depth);

    let mut pos = Position::new();
    pos.set_hirate();

    let (node_thr, run_thr) = (100, 1);
    worker.state.nodes = 50;
    worker.state.depth_liveness_update_for_test(15, 0, node_thr, run_thr);
    worker.state.depth_liveness_update_for_test(16, 1, node_thr, run_thr);
    worker.state.nodes = 200;

    let ctx = SearchContext {
        tt: &worker.tt,
        eval_hash: &worker.eval_hash,
        history: &worker.history,
        cont_history_sentinel: worker.cont_history_sentinel,
        generate_all_legal_moves: worker.generate_all_legal_moves,
        max_moves_to_draw: worker.max_moves_to_draw,
        thread_id: worker.thread_id,
        allow_tt_write: worker.allow_tt_write,
        tune_params: &worker.search_tune_params,
        reductions: &worker.reductions,
        draw_value_table: worker.draw_value_table,
    };
    let mut time_manager =
        TimeManagement::new(Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));

    let beta = Value::new(100);
    let static_eval = Value::new(1000);
    let calls = Cell::new(0);
    let (value, _improving) = try_null_move_pruning::<{ NodeType::NonPV as u8 }, _>(
        &mut worker.state,
        &ctx,
        &mut pos,
        16,
        beta,
        1,
        true,
        false,
        static_eval,
        false,
        Move::NONE,
        &fixed_depth,
        &mut time_manager,
        |_st, _ctx, _pos, child_depth, _alpha, _beta, ply, _cut_node, _limits, _tm| {
            calls.set(calls.get() + 1);
            // depth <= 0 の呼び出しは search_node なら liveness 更新前に qsearch へ
            // 早期 return するため、ここでは追跡を触らない。
            assert!(child_depth <= 0, "r=32 で null/verification とも depth は負のはず");
            if ply == 1 { beta } else { -beta }
        },
    );

    assert_eq!(calls.get(), 2);
    assert!(value.is_some());

    // マークが残留していると、次の実手 child が verification root と誤認され
    // run 判定・enforcement から外れてしまう (発火せず 16 が素通しになる)。
    assert_eq!(
        worker.state.depth_liveness_update_for_test(16, 2, node_thr, run_thr),
        15,
        "verification 後の実手 child は通常どおり追跡され、発火・enforcement されること"
    );
}

#[test]
fn test_depth_liveness_state_is_reset_for_each_go() {
    let tt = Arc::new(TranspositionTable::new(16));
    let eval_hash = Arc::new(EvalHash::new(1));
    let mut worker = SearchWorker::new(tt, eval_hash, 0, 0, SearchTuneParams::default());
    let mut fixed_depth = LimitsType::new();
    fixed_depth.depth = 15;

    assert!(!worker.state.depth_liveness_is_enabled());
    worker.prepare_search(&fixed_depth);

    assert!(worker.state.depth_liveness_is_enabled());
    worker.state.set_depth_liveness_active_for_test(true);
    assert!(worker.state.depth_liveness_is_active_for_test());
    worker.prepare_search(&fixed_depth);

    assert!(worker.state.depth_liveness_is_enabled());
    assert!(!worker.state.depth_liveness_is_active_for_test());

    let mut node_limited = fixed_depth;
    node_limited.nodes = 100_000;
    worker.prepare_search(&node_limited);
    assert!(!worker.state.depth_liveness_is_enabled());
}

#[test]
fn test_sentinel_initialization() {
    // SearchWorker作成時にsentinelが正しく初期化されることを確認
    let tt = Arc::new(TranspositionTable::new(16));
    let eval_hash = Arc::new(EvalHash::new(1));
    let worker = SearchWorker::new(tt, eval_hash, 0, 0, SearchTuneParams::default());

    // sentinelポインタがdanglingではなく、実際のテーブルを指していることを確認
    let sentinel = worker.cont_history_sentinel;
    let correction_sentinel = worker.cont_correction_sentinel;
    // NonNullはnullにならないことが保証されているので、
    // 代わりにsafeにderefできることを確認（ポインタが有効なメモリを指していること）
    let sentinel_ref = unsafe { sentinel.as_ref() };
    // PieceToHistoryテーブルはYO準拠の初期値(-529)で初期化されているはず
    assert_eq!(
        sentinel_ref.get(crate::types::Piece::B_PAWN, crate::types::Square::SQ_11),
        crate::search::history::CONTINUATION_HISTORY_INIT,
        "sentinel table should be initialized with YO-standard value"
    );
    // correction historyのsentinelはYOと同じ初期値8を持つ。
    assert_eq!(
        unsafe {
            crate::search::history::CorrectionHistory::continuation_value_from_table(
                correction_sentinel,
                crate::types::Piece::B_PAWN,
                crate::types::Square::SQ_11,
            )
        },
        8
    );

    // 全てのスタックエントリがsentinelで初期化されていることを確認
    for (i, stack) in worker.state.stack.iter().enumerate() {
        assert_eq!(
            stack.cont_history_ptr, sentinel,
            "stack[{i}].cont_history_ptr should be initialized to sentinel"
        );
        assert_eq!(
            stack.cont_correction_ptr, correction_sentinel,
            "stack[{i}].cont_correction_ptr should be initialized to sentinel"
        );
    }

    // 未使用だったusize slotをpointerへ置き換えるため、hot Stackのfootprintは増やさない。
    assert_eq!(std::mem::size_of::<crate::search::Stack>(), 56);
}

#[test]
fn test_cont_correction_pointer_tracks_move_and_null() {
    let tt = Arc::new(TranspositionTable::new(16));
    let eval_hash = Arc::new(EvalHash::new(1));
    let mut worker = SearchWorker::new(tt, eval_hash, 0, 0, SearchTuneParams::default());
    let piece = crate::types::Piece::B_SILVER;
    let to = crate::types::Square::SQ_55;

    worker.set_cont_history_for_move(3, false, false, piece, to);
    let expected = {
        let history = unsafe { worker.history.as_ref_unchecked() };
        std::ptr::NonNull::from(history.correction_history.continuation_table(piece, to))
    };
    assert_eq!(worker.state.stack[3].cont_correction_ptr, expected);

    worker.clear_cont_history_for_null(3);
    assert_eq!(worker.state.stack[3].cont_correction_ptr, worker.cont_correction_sentinel);
}

#[test]
fn test_both_node_abort_paths_wait_for_ponderhit_or_stop() {
    use crate::search::TimeManagement;
    use crate::search::alpha_beta::SearchState;
    use crate::types::Color;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    for helper_path in [false, true] {
        for mode in 0..3 {
            let stop = Arc::new(AtomicBool::new(false));
            let hit = Arc::new(AtomicBool::new(false));
            let mut tm = TimeManagement::new(Arc::clone(&stop), Arc::clone(&hit));
            let mut limits = LimitsType::new();
            match mode {
                0 => limits.movetime = 1000,
                1 => limits.rtime = 1000,
                _ => limits.time = [60_000; 2],
            }
            limits.ponder = true;
            limits.start_time = Some(Instant::now() - Duration::from_secs(20));
            tm.init(&limits, Color::Black, 0, 256);
            let mut worker = SearchWorker::new(
                Arc::new(TranspositionTable::new(1)),
                Arc::new(EvalHash::new(1)),
                0,
                0,
                SearchTuneParams::default(),
            );
            let mut state = SearchState::new();
            let mut check = |tm: &mut TimeManagement| {
                if helper_path {
                    state.calls_cnt = 1;
                    super::super::search_helpers::check_abort(
                        &mut state,
                        &worker.create_context(),
                        &limits,
                        tm,
                    )
                } else {
                    worker.state.calls_cnt = 1;
                    worker.check_abort(&limits, tm)
                }
            };
            assert!(!check(&mut tm), "ponder中は期限切れでもabortしない");
            // init後に通知することで初期化時の通知競合を混ぜない。
            hit.store(true, Ordering::Relaxed);
            assert!(!check(&mut tm), "hit直後は新しい予算で継続する");
            assert!(!tm.is_pondering());
            stop.store(true, Ordering::Relaxed);
            assert!(check(&mut tm), "stopは固定期限を待たずに反映する");
        }
    }
}
