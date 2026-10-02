//! 手ごとの参照実装と、バッチ採点のスコア・順序の一致検証。

use super::*;
use crate::movegen::{GenType, generate_with_type};
use crate::position::SFEN_HIRATE;
use crate::position::playout_test_support::{PERFT_MATSURI, PERFT_MIDGAME, RandomPlayout};
use rand::{Rng, SeedableRng};
use rand_xoshiro::Xoshiro256PlusPlus;

// ExtMove の PartialEq は value だけを比較するので、駒情報も含む指し手を取り出す。
fn entries(moves: &[ExtMove]) -> Vec<(u32, i32)> {
    moves.iter().map(|ext| (ext.mv.raw32(), ext.value)).collect()
}

fn copy_picker(template: &MovePicker, pos: &Position) -> MovePicker {
    let mut picker = MovePicker::new(
        pos,
        template.tt_move,
        template.depth,
        template.ply,
        template.continuation_history,
        template.generate_all_legal_moves,
    );
    for ext in template.moves.as_slice() {
        picker.moves.push(*ext);
    }
    picker.cur = template.cur;
    picker.end_cur = template.end_cur;
    picker
}

fn assert_scores_and_order(template: &MovePicker, pos: &Position, history: &HistoryTables) {
    let mut expected = copy_picker(template, pos);
    expected.score_quiets_legacy(pos, history);
    let range = template.cur..template.end_cur;
    let before = template.moves.as_slice();
    assert_eq!(
        entries(&before[..range.start]),
        entries(&expected.moves.as_slice()[..range.start])
    );
    assert_eq!(entries(&before[range.end..]), entries(&expected.moves.as_slice()[range.end..]));

    for score in [
        MovePicker::score_quiets_batched::<4>,
        MovePicker::score_quiets_batched::<8>,
        MovePicker::score_quiets,
    ] {
        let mut actual = copy_picker(template, pos);
        score(&mut actual, pos, history);
        assert_eq!(
            entries(actual.moves.as_slice()),
            entries(expected.moves.as_slice()),
            "ply={} all={} sfen={}",
            template.ply,
            template.generate_all_legal_moves,
            pos.to_sfen()
        );
        for limit in [i32::MIN, -3560, -3560 * 4, -3560 * 18] {
            let mut old = expected.moves.as_slice()[range.clone()].to_vec();
            let mut new = actual.moves.as_slice()[range.clone()].to_vec();
            let len = old.len();
            assert_eq!(
                partial_insertion_sort(&mut old, len, limit),
                partial_insertion_sort(&mut new, len, limit)
            );
            assert_eq!(entries(&old), entries(&new), "limit={limit}");
        }
    }
}

fn quiet_picker(pos: &Position, all: bool, keys: [ContHistKey; 6]) -> MovePicker {
    let mut picker = MovePicker::new(pos, Move::NONE, 4, 0, keys, all);
    // 捕獲手の後ろから始まる範囲と、その外側を変更しないことも検証する。
    for value in [123, -456, 789] {
        picker.moves.push(ExtMove::new(Move::NULL, value));
    }
    picker.cur = picker.moves.len();
    if all {
        generate_with_type(pos, GenType::QuietsAll, &mut picker.moves, None);
    } else {
        pos.generate_quiets(&mut picker.moves, picker.cur);
    }
    picker.end_cur = picker.moves.len();
    picker.moves.push(ExtMove::new(Move::NULL, -987));
    picker
}

#[test]
fn quiet_score_batches_match_random_positions_and_histories() {
    let mut history = HistoryTables::new_boxed();
    let mut saw_drop = false;
    let mut saw_promotion = false;
    let mut saw_check_bonus = false;
    let mut saw_rejected_check = false;
    let mut saw_non_check = false;
    for sfen in [SFEN_HIRATE, PERFT_MATSURI, PERFT_MIDGAME] {
        for seed in [0x5EED_u64, 0xCAFE] {
            let mut rng = Xoshiro256PlusPlus::seed_from_u64(seed);
            let mut playout = RandomPlayout::new(seed, 0);
            playout.pos.set_sfen(sfen).unwrap();
            for step in 0..64 {
                let pos = &playout.pos;
                if !pos.in_check() {
                    let base = quiet_picker(pos, true, [ContHistKey::null_sentinel(); 6]);
                    let quiets = &base.moves.as_slice()[base.cur..base.end_cur];
                    let mut keys = [ContHistKey::null_sentinel(); 6];
                    if !quiets.is_empty() {
                        for key in &mut keys[1..] {
                            let mv = quiets[rng.random_range(0..quiets.len())].mv;
                            *key = ContHistKey::new(
                                rng.random(),
                                rng.random(),
                                mv.moved_piece_after(),
                                mv.to(),
                            );
                        }
                        // 同じ table を複数の continuation 項が読む場合も含める。
                        if step % 2 == 0 {
                            keys[1] = keys[0];
                        }
                    }
                    for ext in quiets {
                        let mv = ext.mv;
                        let pc = mv.moved_piece_after();
                        let to = mv.to();
                        saw_drop |= mv.is_drop();
                        saw_promotion |= mv.is_promotion();
                        if pos.check_squares(pc.piece_type()).contains(to) {
                            if pos.see_ge(mv, Value::new(-75)) {
                                saw_check_bonus = true;
                            } else {
                                saw_rejected_check = true;
                            }
                        } else {
                            saw_non_check = true;
                        }
                        // 範囲の内外の bonus で、正負・飽和値・過去の更新がある状態を作る。
                        history.main_history.update(
                            pos.side_to_move(),
                            mv,
                            rng.random_range(-32767..=32767),
                        );
                        history.pawn_history.update(
                            base.pawn_history_index,
                            pc,
                            to,
                            rng.random_range(-32767..=32767),
                        );
                        for key in keys {
                            history.continuation_history[key.in_check as usize]
                                [key.capture as usize]
                                .get_table_mut(key.piece, key.to)
                                .update(pc, to, rng.random_range(-32767..=32767));
                        }
                        for ply in 0..LOW_PLY_HISTORY_SIZE {
                            history.low_ply_history.update(
                                ply,
                                mv,
                                rng.random_range(-32767..=32767),
                            );
                        }
                    }
                    for all in [false, true] {
                        let mut picker = quiet_picker(pos, all, keys);
                        for ply in 0..=LOW_PLY_HISTORY_SIZE + 1 {
                            picker.ply = ply as i32;
                            assert_scores_and_order(&picker, pos, &history);
                        }
                    }
                }
                if playout.step().is_none() {
                    break;
                }
            }
        }
    }
    assert!(saw_drop && saw_promotion && saw_check_bonus && saw_rejected_check && saw_non_check);
}

#[test]
fn quiet_score_batches_match_empty_short_and_tied_ranges() {
    let mut pos = Position::new();
    pos.set_hirate();
    let history = HistoryTables::new_boxed();
    let mut picker = quiet_picker(&pos, false, [ContHistKey::null_sentinel(); 6]);
    let end = picker.end_cur;
    // 全方式で 0 手・1 手・バッチ境界の前後と端数、同点の順序を比較する。
    // 他の並行テストも同じスコアを期待するため、この設定変更で結果は変わらない。
    for batch in [0, 4, 8] {
        set_quiet_score_batch(batch).unwrap();
        for len in 0..=17 {
            picker.end_cur = picker.cur + len;
            assert!(picker.end_cur <= end);
            for ply in [
                0,
                LOW_PLY_HISTORY_SIZE as i32 - 1,
                LOW_PLY_HISTORY_SIZE as i32,
                127,
            ] {
                picker.ply = ply;
                assert_scores_and_order(&picker, &pos, &history);
            }
        }
    }
}
