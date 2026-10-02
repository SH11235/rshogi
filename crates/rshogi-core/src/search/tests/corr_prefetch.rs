//! 補正履歴の prefetch アドレスと補正値読み取りの一致。

use std::sync::Arc;

use crate::eval::EvalHash;
use crate::position::Position;
use crate::tt::TranspositionTable;
use crate::types::{Color, MAX_PLY, Move, Piece, Square};

use super::super::corr_prefetch::{continuation_entries, prefetch_correction};
use super::super::eval_helpers::correction_value;
use super::super::{SearchTuneParams, SearchWorker};

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
            {
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
