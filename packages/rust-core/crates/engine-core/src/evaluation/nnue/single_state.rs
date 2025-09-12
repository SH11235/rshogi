use super::single::SingleChannelNet;
use crate::{
    evaluation::nnue::features::{halfkp_index, BonaPiece},
    shogi::piece_type_to_hand_index,
    Color, Piece, PieceType, Position,
};
use smallvec::SmallVec;

/// SINGLE_CHANNEL 用の増分 Acc（土台）。
/// - acc_dim はネットから取得（通常は 256）
/// - 値は pre/post の両方を保持（pre=前活性、post=ReLU(pre)）
#[derive(Clone, Debug)]
pub struct SingleAcc {
    pub(crate) pre_black: Vec<f32>,
    pub(crate) pre_white: Vec<f32>,
    pub(crate) post_black: Vec<f32>,
    pub(crate) post_white: Vec<f32>,
}

impl SingleAcc {
    #[inline]
    pub fn acc_for(&self, stm: Color) -> &[f32] {
        match stm {
            Color::Black => &self.post_black,
            Color::White => &self.post_white,
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
                let row = &net.w0[base..base + d];
                for (pb, r) in pre_black.iter_mut().zip(row.iter()) {
                    *pb += *r;
                }
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
                let row = &net.w0[base..base + d];
                for (pw, r) in pre_white.iter_mut().zip(row.iter()) {
                    *pw += *r;
                }
            }
        }

        let mut post_black = pre_black.clone();
        let mut post_white = pre_white.clone();
        for v in &mut post_black {
            if *v < 0.0 {
                *v = 0.0;
            }
        }
        for v in &mut post_white {
            if *v < 0.0 {
                *v = 0.0;
            }
        }

        SingleAcc {
            pre_black,
            pre_white,
            post_black,
            post_white,
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
            if let Some(b) = BonaPiece::from_board(piece, to) {
                added_b.push(halfkp_index(bk, b));
            }
            if let Some(w) = BonaPiece::from_board(piece.flip_color(), to.flip()) {
                added_w.push(halfkp_index(wk_flip, w));
            }

            let color = pre_pos.side_to_move;
            let hand_idx = piece_type_to_hand_index(pt).expect("valid hand type");
            let count = pre_pos.hands[color as usize][hand_idx];
            if count > 0 {
                if let Ok(bh) = BonaPiece::from_hand(pt, color, count) {
                    removed_b.push(halfkp_index(bk, bh));
                }
                if let Ok(wh) = BonaPiece::from_hand(pt, color.flip(), count) {
                    removed_w.push(halfkp_index(wk_flip, wh));
                }
                if count > 1 {
                    if let Ok(bh2) = BonaPiece::from_hand(pt, color, count - 1) {
                        added_b.push(halfkp_index(bk, bh2));
                    }
                    if let Ok(wh2) = BonaPiece::from_hand(pt, color.flip(), count - 1) {
                        added_w.push(halfkp_index(wk_flip, wh2));
                    }
                }
            }
        } else {
            let from = mv.from().expect("from exists");
            let to = mv.to();
            let moving_piece = pre_pos.piece_at(from).expect("piece at from");
            if let Some(b) = BonaPiece::from_board(moving_piece, from) {
                removed_b.push(halfkp_index(bk, b));
            }
            if let Some(w) = BonaPiece::from_board(moving_piece.flip_color(), from.flip()) {
                removed_w.push(halfkp_index(wk_flip, w));
            }

            let dest_piece = if mv.is_promote() {
                moving_piece.promote()
            } else {
                moving_piece
            };
            if let Some(b) = BonaPiece::from_board(dest_piece, to) {
                added_b.push(halfkp_index(bk, b));
            }
            if let Some(w) = BonaPiece::from_board(dest_piece.flip_color(), to.flip()) {
                added_w.push(halfkp_index(wk_flip, w));
            }

            if let Some(captured) = pre_pos.piece_at(to) {
                if let Some(b) = BonaPiece::from_board(captured, to) {
                    removed_b.push(halfkp_index(bk, b));
                }
                if let Some(w) = BonaPiece::from_board(captured.flip_color(), to.flip()) {
                    removed_w.push(halfkp_index(wk_flip, w));
                }

                let hand_type = captured.piece_type;
                // NOTE: Piece は基底種(piece_type)と promoted フラグを分離保持する設計のため、
                //       手駒化は常に基底種 hand_type で正しい（成駒を取っても基底種に戻る）。
                debug_assert!(hand_type != PieceType::King);
                debug_assert!(piece_type_to_hand_index(hand_type).is_ok());
                let hand_idx = piece_type_to_hand_index(hand_type).expect("hand type");
                let color = pre_pos.side_to_move;
                let new_count = pre_pos.hands[color as usize][hand_idx] + 1;
                if let Ok(bh) = BonaPiece::from_hand(hand_type, color, new_count) {
                    added_b.push(halfkp_index(bk, bh));
                }
                if let Ok(wh) = BonaPiece::from_hand(hand_type, color.flip(), new_count) {
                    added_w.push(halfkp_index(wk_flip, wh));
                }
                if new_count > 1 {
                    if let Ok(bh_old) = BonaPiece::from_hand(hand_type, color, new_count - 1) {
                        removed_b.push(halfkp_index(bk, bh_old));
                    }
                    if let Ok(wh_old) = BonaPiece::from_hand(hand_type, color.flip(), new_count - 1)
                    {
                        removed_w.push(halfkp_index(wk_flip, wh_old));
                    }
                }
            }
        }

        // 同一 fid の重複を軽減（安全のための微最適化）
        removed_b.sort_unstable();
        removed_b.dedup();
        removed_w.sort_unstable();
        removed_w.dedup();
        added_b.sort_unstable();
        added_b.dedup();
        added_w.sort_unstable();
        added_w.dedup();

        // 交差相殺（removed と added の共通要素を打ち消す）
        fn cancel_cross(a: &mut SmallVec<[usize; 16]>, b: &mut SmallVec<[usize; 16]>) {
            let mut i = 0usize;
            let mut j = 0usize;
            let mut a_only: SmallVec<[usize; 16]> = SmallVec::new();
            let mut b_only: SmallVec<[usize; 16]> = SmallVec::new();
            while i < a.len() && j < b.len() {
                match a[i].cmp(&b[j]) {
                    std::cmp::Ordering::Less => {
                        a_only.push(a[i]);
                        i += 1;
                    }
                    std::cmp::Ordering::Greater => {
                        b_only.push(b[j]);
                        j += 1;
                    }
                    std::cmp::Ordering::Equal => {
                        // 相殺: 両方スキップ
                        i += 1;
                        j += 1;
                    }
                }
            }
            // 余りを追加
            a_only.extend_from_slice(&a[i..]);
            b_only.extend_from_slice(&b[j..]);
            a.clear();
            a.extend_from_slice(&a_only);
            b.clear();
            b.extend_from_slice(&b_only);
        }
        cancel_cross(&mut removed_b, &mut added_b);
        cancel_cross(&mut removed_w, &mut added_w);

        // pre に適用
        for &fid in &removed_b {
            if fid >= net.n_feat {
                continue;
            }
            let base = fid * d;
            let row = &net.w0[base..base + d];
            for (pb, r) in next.pre_black.iter_mut().zip(row.iter()) {
                *pb -= *r;
            }
        }
        for &fid in &removed_w {
            if fid >= net.n_feat {
                continue;
            }
            let base = fid * d;
            let row = &net.w0[base..base + d];
            for (pw, r) in next.pre_white.iter_mut().zip(row.iter()) {
                *pw -= *r;
            }
        }
        for &fid in &added_b {
            if fid >= net.n_feat {
                continue;
            }
            let base = fid * d;
            let row = &net.w0[base..base + d];
            for (pb, r) in next.pre_black.iter_mut().zip(row.iter()) {
                *pb += *r;
            }
        }
        for &fid in &added_w {
            if fid >= net.n_feat {
                continue;
            }
            let base = fid * d;
            let row = &net.w0[base..base + d];
            for (pw, r) in next.pre_white.iter_mut().zip(row.iter()) {
                *pw += *r;
            }
        }

        // ReLU で post を更新
        next.post_black.clone_from(&next.pre_black);
        next.post_white.clone_from(&next.pre_white);
        for v in &mut next.post_black {
            if *v < 0.0 {
                *v = 0.0;
            }
        }
        for v in &mut next.post_white {
            if *v < 0.0 {
                *v = 0.0;
            }
        }

        next
    }
}
