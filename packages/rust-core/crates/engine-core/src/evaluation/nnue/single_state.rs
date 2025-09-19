use super::single::SingleChannelNet;
use crate::{
    evaluation::nnue::features::{oriented_board_feature_index, oriented_hand_feature_index},
    shogi::piece_type_to_hand_index,
    simd::add_row_scaled_f32,
    Color, Piece, PieceType, Position,
};
use smallvec::SmallVec;

#[inline]
fn aggregate_counts(removed: &[usize], added: &[usize]) -> SmallVec<[(usize, i16); 32]> {
    #[cfg(feature = "diff_agg_hash")]
    {
        use std::collections::HashMap;
        let mut map: HashMap<usize, i16> = HashMap::with_capacity(removed.len() + added.len());
        for &fid in removed {
            *map.entry(fid).or_insert(0) -= 1;
        }
        for &fid in added {
            *map.entry(fid).or_insert(0) += 1;
        }
        let mut out: SmallVec<[(usize, i16); 32]> = SmallVec::new();
        out.extend(map.into_iter().filter(|&(_, c)| c != 0));
        return out;
    }
    #[cfg(not(feature = "diff_agg_hash"))]
    {
        let mut agg: SmallVec<[(usize, i16); 32]> = SmallVec::new();
        agg.reserve_exact(removed.len() + added.len());
        // linear map update
        let mut update = |fid: usize, delta: i16| {
            if let Some((_, c)) = agg.iter_mut().find(|(f, _)| *f == fid) {
                *c += delta;
            } else {
                agg.push((fid, delta));
            }
        };
        for &fid in removed {
            update(fid, -1);
        }
        for &fid in added {
            update(fid, 1);
        }
        agg.retain(|entry| entry.1 != 0);
        agg
    }
}

/// SINGLE_CHANNEL 用の増分 Acc（土台）。
/// - acc_dim はネットから取得（通常は 256）
/// - pre のみ保持（前活性）。ReLU は評価直前に一度だけ適用する（ReLU遅延）。
#[derive(Clone, Debug)]
pub struct SingleAcc {
    pub(crate) pre_black: Vec<f32>,
    pub(crate) pre_white: Vec<f32>,
    pub(crate) weights_uid: u64,
}

impl SingleAcc {
    #[inline]
    pub fn acc_for(&self, stm: Color) -> &[f32] {
        match stm {
            Color::Black => &self.pre_black,
            Color::White => &self.pre_white,
        }
    }

    /// 現局面からフル再構築（差分なし）。白番視点では王座標を flip。
    pub fn refresh(pos: &Position, net: &SingleChannelNet) -> Self {
        let d = net.acc_dim;
        let mut pre_black = vec![0.0f32; d];
        let mut pre_white = vec![0.0f32; d];

        if let Some(ref b0) = net.b0 {
            debug_assert_eq!(b0.len(), d);
            for (pb, b) in pre_black.iter_mut().zip(b0.iter()) {
                *pb += *b;
            }
            for (pw, b) in pre_white.iter_mut().zip(b0.iter()) {
                *pw += *b;
            }
        }

        // 黒視点
        if let Some(bk) = pos.board.king_square(Color::Black) {
            let feats = super::features::extract_features(pos, bk, Color::Black);
            for &fid in feats.as_slice() {
                if fid >= net.n_feat {
                    continue;
                }
                let base = fid * d;
                debug_assert!(
                    base.checked_add(d).is_some_and(|end| end <= net.w0.len()),
                    "w0 out of bounds: fid={fid}, d={d}, w0_len={}",
                    net.w0.len()
                );
                let row = &net.w0[base..base + d];
                add_row_scaled_f32(&mut pre_black, row, 1.0);
            }
        }

        // 白視点（kingをflip）
        if let Some(wk) = pos.board.king_square(Color::White) {
            let feats = super::features::extract_features(pos, wk.flip(), Color::White);
            for &fid in feats.as_slice() {
                if fid >= net.n_feat {
                    continue;
                }
                let base = fid * d;
                debug_assert!(
                    base.checked_add(d).is_some_and(|end| end <= net.w0.len()),
                    "w0 out of bounds: fid={fid}, d={d}, w0_len={}",
                    net.w0.len()
                );
                let row = &net.w0[base..base + d];
                add_row_scaled_f32(&mut pre_white, row, 1.0);
            }
        }

        SingleAcc {
            pre_black,
            pre_white,
            weights_uid: net.uid,
        }
    }

    /// 差分更新：pre状態とmvから、両視点のaccを更新した新しい状態を返す。
    /// 王移動を含む場合は安全側でフル再構築。
    pub fn apply_update(
        pre: &SingleAcc,
        pre_pos: &Position,
        mv: crate::shogi::Move,
        net: &SingleChannelNet,
    ) -> SingleAcc {
        let d = net.acc_dim;
        if mv.piece_type() == Some(PieceType::King) && !mv.is_drop() {
            let mut post = pre_pos.clone();
            let _u = post.do_move(mv);
            #[cfg(feature = "nnue_telemetry")]
            crate::evaluation::nnue::telemetry::record_apply_refresh_king();
            return SingleAcc::refresh(&post, net);
        }

        let mut next = pre.clone();
        let mut removed_b: SmallVec<[usize; 16]> = SmallVec::new();
        let mut removed_w: SmallVec<[usize; 16]> = SmallVec::new();
        let mut added_b: SmallVec<[usize; 16]> = SmallVec::new();
        let mut added_w: SmallVec<[usize; 16]> = SmallVec::new();

        let bk = match pre_pos.board.king_square(Color::Black) {
            Some(s) => s,
            None => {
                let mut post = pre_pos.clone();
                let _u = post.do_move(mv);
                #[cfg(feature = "nnue_telemetry")]
                crate::evaluation::nnue::telemetry::record_apply_refresh_other();
                return SingleAcc::refresh(&post, net);
            }
        };
        let wk_flip = match pre_pos.board.king_square(Color::White) {
            Some(s) => s.flip(),
            None => {
                let mut post = pre_pos.clone();
                let _u = post.do_move(mv);
                return SingleAcc::refresh(&post, net);
            }
        };

        if mv.is_drop() {
            let to = mv.to();
            let pt = mv.drop_piece_type();
            let piece = Piece::new(pt, pre_pos.side_to_move);
            if let Some(idx) = oriented_board_feature_index(Color::Black, bk, piece, to) {
                added_b.push(idx);
            }
            if let Some(idx) = oriented_board_feature_index(Color::White, wk_flip, piece, to) {
                added_w.push(idx);
            }

            let color = pre_pos.side_to_move;
            let hand_idx = piece_type_to_hand_index(pt).expect("valid hand type");
            let count = pre_pos.hands[color as usize][hand_idx];
            if count > 0 {
                match oriented_hand_feature_index(Color::Black, bk, pt, color, count) {
                    Ok(idx) => removed_b.push(idx),
                    Err(_e) => {
                        #[cfg(debug_assertions)]
                        log::error!("[NNUE] Error creating BonaPiece from hand: {_e}");
                    }
                }
                match oriented_hand_feature_index(Color::White, wk_flip, pt, color, count) {
                    Ok(idx) => removed_w.push(idx),
                    Err(_e) => {
                        #[cfg(debug_assertions)]
                        log::error!("[NNUE] Error creating BonaPiece from hand: {_e}");
                    }
                }
                if count > 1 {
                    match oriented_hand_feature_index(Color::Black, bk, pt, color, count - 1) {
                        Ok(idx) => added_b.push(idx),
                        Err(_e) => {
                            #[cfg(debug_assertions)]
                            log::warn!("[NNUE] Error creating BonaPiece from hand: {}", _e);
                        }
                    }
                    match oriented_hand_feature_index(Color::White, wk_flip, pt, color, count - 1) {
                        Ok(idx) => added_w.push(idx),
                        Err(_e) => {
                            #[cfg(debug_assertions)]
                            log::warn!("[NNUE] Error creating BonaPiece from hand: {}", _e);
                        }
                    }
                }
            }
        } else {
            let from = mv.from().expect("from exists");
            let to = mv.to();
            let moving_piece = pre_pos.piece_at(from).expect("piece at from");
            if let Some(idx) = oriented_board_feature_index(Color::Black, bk, moving_piece, from) {
                removed_b.push(idx);
            }
            if let Some(idx) =
                oriented_board_feature_index(Color::White, wk_flip, moving_piece, from)
            {
                removed_w.push(idx);
            }

            let dest_piece = if mv.is_promote() {
                moving_piece.promote()
            } else {
                moving_piece
            };
            if let Some(idx) = oriented_board_feature_index(Color::Black, bk, dest_piece, to) {
                added_b.push(idx);
            }
            if let Some(idx) = oriented_board_feature_index(Color::White, wk_flip, dest_piece, to) {
                added_w.push(idx);
            }

            if let Some(captured) = pre_pos.piece_at(to) {
                if let Some(idx) = oriented_board_feature_index(Color::Black, bk, captured, to) {
                    removed_b.push(idx);
                }
                if let Some(idx) = oriented_board_feature_index(Color::White, wk_flip, captured, to)
                {
                    removed_w.push(idx);
                }

                let hand_type = captured.piece_type;
                // NOTE: Piece は基底種(piece_type)と promoted フラグを分離保持する設計のため、
                //       手駒化は常に基底種 hand_type で正しい（成駒を取っても基底種に戻る）。
                debug_assert!(hand_type != PieceType::King);
                debug_assert!(piece_type_to_hand_index(hand_type).is_ok());
                let hand_idx = piece_type_to_hand_index(hand_type).expect("hand type");
                let color = pre_pos.side_to_move;
                let new_count = pre_pos.hands[color as usize][hand_idx] + 1;
                match oriented_hand_feature_index(Color::Black, bk, hand_type, color, new_count) {
                    Ok(idx) => added_b.push(idx),
                    Err(_e) => {
                        #[cfg(debug_assertions)]
                        log::warn!("[NNUE] Error creating BonaPiece from hand: {}", _e);
                    }
                }
                match oriented_hand_feature_index(
                    Color::White,
                    wk_flip,
                    hand_type,
                    color,
                    new_count,
                ) {
                    Ok(idx) => added_w.push(idx),
                    Err(_e) => {
                        #[cfg(debug_assertions)]
                        log::warn!("[NNUE] Error creating BonaPiece from hand: {}", _e);
                    }
                }
                if new_count > 1 {
                    match oriented_hand_feature_index(
                        Color::Black,
                        bk,
                        hand_type,
                        color,
                        new_count - 1,
                    ) {
                        Ok(idx) => removed_b.push(idx),
                        Err(_e) => {
                            #[cfg(debug_assertions)]
                            log::warn!("[NNUE] Error creating BonaPiece from hand: {}", _e);
                        }
                    }
                    match oriented_hand_feature_index(
                        Color::White,
                        wk_flip,
                        hand_type,
                        color,
                        new_count - 1,
                    ) {
                        Ok(idx) => removed_w.push(idx),
                        Err(_e) => {
                            #[cfg(debug_assertions)]
                            log::warn!("[NNUE] Error creating BonaPiece from hand: {}", _e);
                        }
                    }
                }
            }
        }

        // 交差相殺の一般化（集計した増減を一括適用）
        let diff_b = aggregate_counts(&removed_b, &added_b);
        let diff_w = aggregate_counts(&removed_w, &added_w);

        for &(fid, delta) in diff_b.iter() {
            if fid >= net.n_feat {
                continue;
            }
            let base = fid * d;
            debug_assert!(
                base.checked_add(d).is_some_and(|end| end <= net.w0.len()),
                "w0 out of bounds: fid={fid}, d={d}, w0_len={}",
                net.w0.len()
            );
            let row = &net.w0[base..base + d];
            add_row_scaled_f32(&mut next.pre_black, row, delta as f32);
        }
        for &(fid, delta) in diff_w.iter() {
            if fid >= net.n_feat {
                continue;
            }
            let base = fid * d;
            debug_assert!(
                base.checked_add(d).is_some_and(|end| end <= net.w0.len()),
                "w0 out of bounds: fid={fid}, d={d}, w0_len={}",
                net.w0.len()
            );
            let row = &net.w0[base..base + d];
            add_row_scaled_f32(&mut next.pre_white, row, delta as f32);
        }

        // Keep same net identity
        next.weights_uid = pre.weights_uid;
        next
    }
}
