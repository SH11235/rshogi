//! 補正履歴の prefetch アドレスと、モード切替時の探索不変性。

use std::sync::Arc;

use crate::eval::EvalHash;
use crate::position::Position;
use crate::tt::TranspositionTable;
use crate::types::{Color, MAX_PLY, Move, Piece, Square};

use super::super::corr_prefetch::{continuation_entries, corr_prefetch_mode, prefetch_correction};
use super::super::eval_helpers::correction_value;
use super::super::{
    LimitsType, Search, SearchInfo, SearchTuneParams, SearchWorker, set_corr_prefetch,
};

fn worker() -> Box<SearchWorker> {
    SearchWorker::new(
        Arc::new(TranspositionTable::new(1)),
        Arc::new(EvalHash::new(1)),
        0,
        0,
        SearchTuneParams::default(),
    )
}

#[test]
fn corr_prefetch_continuation_addresses_and_values_match() {
    let mut worker = worker();
    let mut pos = Position::new();
    pos.set_hirate();
    // 成り・捕獲・打ちを含め、移動後の駒と子局面の手番を検証する。
    for usi in ["7g7f", "3c3d", "8h2b+", "3a2b", "B*5e"] {
        let mv = pos.to_move(Move::from_usi(usi).unwrap()).unwrap();
        let check = pos.gives_check(mv);
        pos.do_move(mv, check);
        let pc = pos.piece_on(mv.to());
        {
            // SAFETY: このスレッドだけで排他的に更新し、共有参照はまだ作っていない。
            let h = unsafe { worker.history.as_mut_unchecked() };
            for (back, bonus) in [(2, 123), (4, -345)] {
                h.correction_history.update_continuation(
                    Piece::W_SILVER,
                    Square::from_u8(back).unwrap(),
                    pc,
                    mv.to(),
                    bonus,
                );
            }
        }
        for ply in 1..=MAX_PLY {
            for back in [2, 4] {
                if ply >= back {
                    worker.set_cont_history_for_move(
                        ply - back,
                        false,
                        false,
                        Piece::W_SILVER,
                        Square::from_u8(back as u8).unwrap(),
                    );
                }
            }
            // current_move がまだ未設定でも prefetch は今指した手を使う。
            worker.state.stack[(ply - 1) as usize].current_move = Move::NONE;
            let entries = continuation_entries(&worker.state, &pos, ply, mv);
            let expected_cnt = {
                // SAFETY: このスレッドだけで履歴を読み取り、このスコープで可変参照を作らない。
                let h = unsafe { worker.history.as_ref_unchecked() };
                let mut sum = 0;
                for (entry, back) in entries.into_iter().zip([2, 4]) {
                    let table = if ply >= back {
                        h.correction_history.continuation_table(
                            Piece::W_SILVER,
                            Square::from_u8(back as u8).unwrap(),
                        )
                    } else {
                        h.correction_history.continuation_table(Piece::NONE, Square::SQ_11)
                    };
                    let expected = &table[pc.index()][mv.to().index()];
                    if ply >= back {
                        assert_eq!(entry, Some(std::ptr::from_ref(expected)));
                    } else {
                        assert!(entry.is_none());
                    }
                    sum += i32::from(expected.get());
                }
                sum
            };
            worker.state.stack[(ply - 1) as usize].current_move = mv;
            // 異なる値を入れ、手番と non-pawn の board_color の取り違えも検出する。
            {
                // SAFETY: 上の共有参照は破棄済みで、単一スレッドの排他的な更新。
                let h = unsafe { worker.history.as_mut_unchecked() };
                let us = pos.side_to_move();
                h.correction_history.update_pawn(pos.pawn_key() as usize, us, 37);
                h.correction_history.update_minor(pos.minor_piece_key() as usize, us, -51);
                for (color, bonus) in [(Color::White, 73), (Color::Black, -91)] {
                    h.correction_history.update_non_pawn(
                        pos.non_pawn_key(color) as usize,
                        color,
                        us,
                        bonus,
                    );
                }
            }
            for mode in 0..=2 {
                worker.state.corr_prefetch = mode;
                let ctx = worker.create_context();
                let before = correction_value(&worker.state, &ctx, &pos, ply);
                prefetch_correction(&worker.state, &worker.history, &pos, ply, mv);
                // SAFETY: prefetch は履歴を変更せず、可変参照も保持していない。
                let h = unsafe { worker.history.as_ref_unchecked() };
                let c = &h.correction_history;
                let us = pos.side_to_move();
                let tp = ctx.tune_params;
                let expected = tp.correction_value_pcv_weight
                    * i32::from(c.pawn_value(pos.pawn_key() as usize, us))
                    + tp.correction_value_micv_weight
                        * i32::from(c.minor_value(pos.minor_piece_key() as usize, us))
                    + tp.correction_value_nonpawn_weight
                        * (i32::from(c.non_pawn_value(
                            pos.non_pawn_key(Color::White) as usize,
                            Color::White,
                            us,
                        )) + i32::from(c.non_pawn_value(
                            pos.non_pawn_key(Color::Black) as usize,
                            Color::Black,
                            us,
                        )))
                    + tp.correction_value_cnt_weight * expected_cnt;
                assert_eq!(before, expected);
                assert_eq!(correction_value(&worker.state, &ctx, &pos, ply), expected);
            }
        }
        for ply in [i32::MIN, -1, 0, 1, i32::MAX] {
            assert_eq!(continuation_entries(&worker.state, &pos, ply, mv), [None; 2]);
        }
        for mv in [Move::NONE, Move::NULL, Move::PASS] {
            assert_eq!(continuation_entries(&worker.state, &pos, 4, mv), [None; 2]);
        }
        worker.clear_cont_history_for_null(2);
        let entry = continuation_entries(&worker.state, &pos, 4, mv)[0].unwrap();
        // SAFETY: 単一スレッドで読み取り、履歴の可変参照は保持していない。
        let h = unsafe { worker.history.as_ref_unchecked() };
        let sentinel = h.correction_history.continuation_table(Piece::NONE, Square::SQ_11);
        assert_eq!(entry, std::ptr::from_ref(&sentinel[pc.index()][mv.to().index()]));
    }
}

#[test]
fn corr_prefetch_modes_preserve_search_and_snapshot_at_prepare() {
    let _guard = crate::eval::material::test_support::lock_material();
    crate::eval::set_material_level(crate::eval::MaterialLevel::Lv1);
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            struct RestoreMode(u8);
            impl Drop for RestoreMode {
                fn drop(&mut self) {
                    set_corr_prefetch(self.0);
                }
            }
            let _restore = RestoreMode(corr_prefetch_mode());
            let mut baseline = None;
            for mode in 0..=2 {
                assert!(set_corr_prefetch(mode));
                assert_eq!(corr_prefetch_mode(), mode);
                let limits = LimitsType {
                    depth: 3,
                    ..LimitsType::new()
                };
                let mut worker = worker();
                worker.prepare_search(&limits);
                assert_eq!(worker.state.corr_prefetch, mode);
                assert!(set_corr_prefetch((mode + 1) % 3));
                assert_eq!(worker.state.corr_prefetch, mode);
                worker.prepare_search(&limits);
                assert_eq!(worker.state.corr_prefetch, (mode + 1) % 3);
                assert!(set_corr_prefetch(mode));
                for invalid in [3, u8::MAX] {
                    assert!(!set_corr_prefetch(invalid));
                    assert_eq!(corr_prefetch_mode(), mode);
                }
                let mut pos = Position::new();
                pos.set_hirate();
                let mut search = Search::new(1);
                let result = search.go(&mut pos, limits, None::<fn(&SearchInfo)>);
                let signature =
                    (result.best_move, result.score, result.depth, result.nodes, result.pv);
                if let Some(expected) = &baseline {
                    assert_eq!(&signature, expected);
                } else {
                    baseline = Some(signature);
                }
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
