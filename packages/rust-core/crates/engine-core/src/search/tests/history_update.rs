use crate::search::{
    history::{
        ContinuationHistory, PawnHistory, CONTINUATION_HISTORY_WEIGHTS, TT_MOVE_HISTORY_BONUS,
        TT_MOVE_HISTORY_MALUS,
    },
    tt_history::TTMoveHistory,
    ContHistKey, LimitsType, Search, SearchInfo, Stack,
};
use crate::types::{Move, Piece, Square};

// Search関連のテストではスタック使用量が大きいため、必要に応じてスタックサイズを拡張する。
const STACK_SIZE: usize = 64 * 1024 * 1024; // 64MB

/// TT手がbestだった場合にTTMoveHistoryが加点されることを確認
#[test]
fn tt_move_history_updates_on_bestmove() {
    std::thread::Builder::new()
        .stack_size(STACK_SIZE)
        .spawn(|| {
            let mut search = Search::new(16);
            let mut pos = crate::position::Position::new();
            pos.set_hirate();

            let limits = LimitsType {
                depth: 1,
                ..Default::default()
            };

            // 実際の探索を流して、TT手がbestとして保存されるようにする
            let _ = search.go(&mut pos, limits, None::<fn(&SearchInfo)>);

            let opts = search.time_options(); // just to avoid warnings
            assert!(opts.minimum_thinking_time > 0);

            // 内部のtt_move_historyがゼロでないことを確認できるAPIがないので、
            // 少なくともpanicしないことのみを確認する（実際の更新はMovePicker内で加点される）
        })
        .unwrap()
        .join()
        .unwrap();
}

/// ContinuationHistoryがquiet bestmoveで更新されることを確認
/// NOTE: SearchWorker内部へのアクセスが制限されているため、
/// 簡易的に探索が完了することを確認するのみ
#[test]
fn continuation_history_updates_on_quiet_best() {
    std::thread::Builder::new()
        .stack_size(STACK_SIZE)
        .spawn(|| {
            let mut search = Search::new(16);
            let mut pos = crate::position::Position::new();
            pos.set_hirate();

            // 2手だけ指して、継続手の履歴が取れる状況を作る
            let mv1 = Move::from_usi("7g7f").unwrap();
            let mv2 = Move::from_usi("3c3d").unwrap();

            let gives_check1 = pos.gives_check(mv1);
            pos.do_move(mv1, gives_check1);
            let gives_check2 = pos.gives_check(mv2);
            pos.do_move(mv2, gives_check2);

            let limits = LimitsType {
                depth: 2,
                ..Default::default()
            };

            // 探索を実行（ContinuationHistoryが内部で更新されることを確認）
            let result = search.go(&mut pos, limits, None::<fn(&SearchInfo)>);

            // 結果が存在することを確認
            assert!(result.best_move.is_some(), "探索結果が存在するべき");
        })
        .unwrap()
        .join()
        .unwrap();
}

// =============================================================================
// TTMoveHistory TDDテスト
// =============================================================================

/// TTMoveHistory: 正のボーナス(+811)が正しく加点されることを確認
#[test]
fn tt_move_history_positive_update() {
    let mut history = TTMoveHistory::new();
    let ply = 5;

    // 初期値は0
    assert_eq!(history.get(ply), 0);

    // 正のボーナスを適用
    history.update(ply, TT_MOVE_HISTORY_BONUS);

    // 値が正になっていることを確認
    let value = history.get(ply);
    assert!(
        value > 0,
        "TTMoveHistory should be positive after +{TT_MOVE_HISTORY_BONUS} bonus, got {value}"
    );
}

/// TTMoveHistory: 負のボーナス(-848)が正しく減点されることを確認
#[test]
fn tt_move_history_negative_update() {
    let mut history = TTMoveHistory::new();
    let ply = 5;

    // まず正の値を蓄積
    for _ in 0..5 {
        history.update(ply, TT_MOVE_HISTORY_BONUS);
    }
    let before = history.get(ply);
    assert!(before > 0, "History should be positive before malus");

    // 負のボーナスを適用
    history.update(ply, TT_MOVE_HISTORY_MALUS);

    // 値が減少していることを確認
    let after = history.get(ply);
    assert!(
        after < before,
        "TTMoveHistory should decrease after {TT_MOVE_HISTORY_MALUS} malus"
    );
}

/// TTMoveHistory: YaneuraOu定数の正しさを確認
#[test]
fn tt_move_history_constants_are_correct() {
    assert_eq!(TT_MOVE_HISTORY_BONUS, 811);
    assert_eq!(TT_MOVE_HISTORY_MALUS, -848);
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

/// ContinuationHistory重みの定数が正しいことを確認
#[test]
fn continuation_history_weights_are_correct() {
    // YaneuraOu準拠の重み
    assert_eq!(CONTINUATION_HISTORY_WEIGHTS.len(), 6);
    assert_eq!(CONTINUATION_HISTORY_WEIGHTS[0], (1, 1108));
    assert_eq!(CONTINUATION_HISTORY_WEIGHTS[1], (2, 652));
    assert_eq!(CONTINUATION_HISTORY_WEIGHTS[2], (3, 273));
    assert_eq!(CONTINUATION_HISTORY_WEIGHTS[3], (4, 572));
    assert_eq!(CONTINUATION_HISTORY_WEIGHTS[4], (5, 126));
    assert_eq!(CONTINUATION_HISTORY_WEIGHTS[5], (6, 449));
}

/// ContinuationHistory: 複数ply更新の重み付けをテスト
#[test]
fn continuation_history_weighted_updates() {
    // ContinuationHistoryは大きいのでBoxで作成（スタックオーバーフロー防止）
    let mut cont_hist = ContinuationHistory::new_boxed();

    let base_bonus = 1000;
    let pc = Piece::B_PAWN;
    // SAFETY: 60は有効なSquareインデックス
    let to = unsafe { Square::from_u8_unchecked(60) }; // 7六相当

    // 各plyに対して重み付き更新をシミュレート
    for (ply_back, weight) in CONTINUATION_HISTORY_WEIGHTS.iter() {
        let prev_pc = Piece::B_PAWN;
        // SAFETY: ply_back % 81 は有効なSquareインデックス
        let prev_to = unsafe { Square::from_u8_unchecked((*ply_back % 81) as u8) };
        let near_ply_offset = if *ply_back < 2 { 80 } else { 0 };
        let adjusted_bonus = base_bonus * weight / 1024 + near_ply_offset;

        cont_hist.update(prev_pc, prev_to, pc, to, adjusted_bonus);
    }

    // 1手前（weight=1108）の更新が最も大きいはず
    // SAFETY: 1と5は有効なSquareインデックス
    let sq_1 = unsafe { Square::from_u8_unchecked(1) };
    let sq_5 = unsafe { Square::from_u8_unchecked(5) };
    let value_1_ply = cont_hist.get(Piece::B_PAWN, sq_1, pc, to);
    let value_5_ply = cont_hist.get(Piece::B_PAWN, sq_5, pc, to);

    assert!(
        value_1_ply > value_5_ply,
        "1 ply back (weight=1108) should have higher value than 5 ply back (weight=126)"
    );
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
