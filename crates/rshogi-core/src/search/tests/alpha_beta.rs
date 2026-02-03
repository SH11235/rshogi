//! alpha_beta モジュールのテスト

use std::sync::Arc;

use crate::eval::EvalHash;
use crate::search::alpha_beta::{reduction, SearchWorker};
use crate::tt::TranspositionTable;

#[test]
fn test_reduction_values() {
    // reduction(true, 10, 5) などが正の値を返すことを確認
    // LazyLockにより初回アクセス時に自動初期化される
    let root_delta = 64;
    let delta = 32;
    assert!(reduction(true, 10, 5, delta, root_delta) / 1024 >= 0);
    assert!(
        reduction(false, 10, 5, delta, root_delta) / 1024
            >= reduction(true, 10, 5, delta, root_delta) / 1024
    );
}

#[test]
fn test_reduction_bounds() {
    // 境界値テスト
    let root_delta = 64;
    let delta = 32;
    assert_eq!(reduction(true, 0, 0, delta, root_delta), 0); // depth=0, mc=0 は計算外
    assert!(reduction(true, 63, 63, delta, root_delta) / 1024 < 64);
    assert!(reduction(false, 63, 63, delta, root_delta) / 1024 < 64);
}

/// depth/move_countが大きい場合にreductionが正の値を返すことを確認
#[test]
fn test_reduction_returns_nonzero_for_large_values() {
    let root_delta = 64;
    let delta = 32;
    // 深い探索で多くの手を試した場合、reductionは正の値であるべき
    let r = reduction(false, 10, 10, delta, root_delta) / 1024;
    assert!(
        r > 0,
        "reduction should return positive value for depth=10, move_count=10, got {r}"
    );

    // improving=trueの場合は若干小さい値になる
    let r_imp = reduction(true, 10, 10, delta, root_delta) / 1024;
    assert!(r >= r_imp, "non-improving should have >= reduction than improving");
}

/// 境界ケース: depth=1, move_count=1でもreduction関数が動作することを確認
#[test]
fn test_reduction_small_values() {
    let root_delta = 64;
    let delta = 32;
    // 小さな値でもpanicしないことを確認
    let r = reduction(true, 1, 1, delta, root_delta) / 1024;
    assert!(r >= 0, "reduction should not be negative");
}

#[test]
fn test_reduction_extremes_no_overflow() {
    // 最大depth/mcでもオーバーフローせずに値が得られることを確認
    let delta = 0;
    let root_delta = 1;
    let r = reduction(false, 63, 63, delta, root_delta);
    assert!(
        (0..i32::MAX / 2).contains(&r),
        "reduction extreme should be in safe range, got {r}"
    );
}

#[test]
fn test_reduction_zero_root_delta_clamped() {
    // root_delta=0 を渡しても内部で1にクランプされることを確認
    let r = reduction(false, 10, 10, 0, 0) / 1024;
    assert!(r >= 0, "reduction should clamp root_delta to >=1 even when 0 is passed");
}

#[test]
fn test_sentinel_initialization() {
    // SearchWorker作成時にsentinelが正しく初期化されることを確認
    let tt = Arc::new(TranspositionTable::new(16));
    let eval_hash = Arc::new(EvalHash::new(1));
    let worker = SearchWorker::new(tt, eval_hash, 0, 0);

    // sentinelポインタがdanglingではなく、実際のテーブルを指していることを確認
    let sentinel = worker.cont_history_sentinel;
    // NonNullはnullにならないことが保証されているので、
    // 代わりにsafeにderefできることを確認（ポインタが有効なメモリを指していること）
    let sentinel_ref = unsafe { sentinel.as_ref() };
    // PieceToHistoryテーブルはゼロ初期化されているはず
    assert_eq!(
        sentinel_ref.get(crate::types::Piece::B_PAWN, crate::types::Square::SQ_11),
        0,
        "sentinel table should be zero-initialized"
    );

    // 全てのスタックエントリがsentinelで初期化されていることを確認
    for (i, stack) in worker.state.stack.iter().enumerate() {
        assert_eq!(
            stack.cont_history_ptr, sentinel,
            "stack[{i}].cont_history_ptr should be initialized to sentinel"
        );
    }
}

#[test]
fn test_cont_history_ptr_returns_sentinel_for_negative_offset() {
    let tt = Arc::new(TranspositionTable::new(16));
    let eval_hash = Arc::new(EvalHash::new(1));
    let worker = SearchWorker::new(tt, eval_hash, 0, 0);

    // ply < back の場合はsentinelを返すことを確認
    let ptr = worker.cont_history_ptr(0, 1);
    assert_eq!(ptr, worker.cont_history_sentinel);

    let ptr = worker.cont_history_ptr(3, 5);
    assert_eq!(ptr, worker.cont_history_sentinel);
}
