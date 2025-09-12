use super::single::SingleChannelNet;
use crate::{
    evaluation::nnue::features::{halfkp_index, BonaPiece},
    shogi::piece_type_to_hand_index,
    Color, Piece, PieceType, Position,
};

/// SINGLE_CHANNEL 用の増分 Acc（土台）。
/// - acc_dim はネットから取得（通常は 256）
/// - 値は ReLU 後を保持する（evaluate_from_accumulator にそのまま渡せる）
#[derive(Clone, Debug)]
pub struct SingleAcc {
    black: Vec<f32>,
    white: Vec<f32>,
}

impl SingleAcc {
    #[inline]
    pub fn acc_for(&self, stm: Color) -> &[f32] {
        match stm {
            Color::Black => &self.black,
            Color::White => &self.white,
        }
    }

    /// 現局面からフル再構築（差分なし）。白番視点では王座標を flip。
    pub fn refresh(pos: &Position, net: &SingleChannelNet) -> Self {
        let d = net.acc_dim;
        let mut black = vec![0.0f32; d];
        let mut white = vec![0.0f32; d];

        if let Some(ref b0) = net.b0 {
            debug_assert_eq!(b0.len(), d);
            for i in 0..d {
                black[i] += b0[i];
                white[i] += b0[i];
            }
        }

        // 黒視点
        if let Some(bk) = pos.board.king_square(Color::Black) {
            let feats = super::features::extract_features(pos, bk, Color::Black);
            for &fid in feats.as_slice() {
                if fid >= net.n_feat { continue; }
                let base = fid * d;
                let row = &net.w0[base..base + d];
                for i in 0..d { black[i] += row[i]; }
            }
        }

        // 白視点（kingをflip）
        if let Some(wk) = pos.board.king_square(Color::White) {
            let feats = super::features::extract_features(pos, wk.flip(), Color::White);
            for &fid in feats.as_slice() {
                if fid >= net.n_feat { continue; }
                let base = fid * d;
                let row = &net.w0[base..base + d];
                for i in 0..d { white[i] += row[i]; }
            }
        }

        for v in &mut black { if *v < 0.0 { *v = 0.0; } }
        for v in &mut white { if *v < 0.0 { *v = 0.0; } }

        SingleAcc { black, white }
    }

    /// 差分更新：pre状態とmvから、両視点のaccを更新した新しい状態を返す。
    /// 王移動を含む場合は安全側でフル再構築。
    pub fn apply_update(pre: &SingleAcc, pre_pos: &Position, mv: crate::shogi::Move, net: &SingleChannelNet) -> SingleAcc {
        let d = net.acc_dim;
        if mv.piece_type() == Some(PieceType::King) {
            let mut post = pre_pos.clone();
            let _u = post.do_move(mv);
            return SingleAcc::refresh(&post, net);
        }

        let mut next = pre.clone();
        let mut removed: Vec<usize> = Vec::with_capacity(16);
        let mut added: Vec<usize> = Vec::with_capacity(16);

        let bk = match pre_pos.board.king_square(Color::Black) { Some(s) => s, None => { let mut post = pre_pos.clone(); let _u=post.do_move(mv); return SingleAcc::refresh(&post, net) } };
        let wk_flip = match pre_pos.board.king_square(Color::White) { Some(s) => s.flip(), None => { let mut post = pre_pos.clone(); let _u=post.do_move(mv); return SingleAcc::refresh(&post, net) } };

        if mv.is_drop() {
            let to = mv.to();
            let pt = mv.drop_piece_type();
            let piece = Piece::new(pt, pre_pos.side_to_move);
            if let Some(b) = BonaPiece::from_board(piece, to) { added.push(halfkp_index(bk, b)); }
            if let Some(w) = BonaPiece::from_board(piece.flip_color(), to.flip()) { added.push(halfkp_index(wk_flip, w)); }

            let color = pre_pos.side_to_move;
            let hand_idx = piece_type_to_hand_index(pt).expect("valid hand type");
            let count = pre_pos.hands[color as usize][hand_idx];
            if count > 0 {
                if let Ok(bh) = BonaPiece::from_hand(pt, color, count) { removed.push(halfkp_index(bk, bh)); }
                if let Ok(wh) = BonaPiece::from_hand(pt, color.flip(), count) { removed.push(halfkp_index(wk_flip, wh)); }
                if count > 1 {
                    if let Ok(bh2) = BonaPiece::from_hand(pt, color, count - 1) { added.push(halfkp_index(bk, bh2)); }
                    if let Ok(wh2) = BonaPiece::from_hand(pt, color.flip(), count - 1) { added.push(halfkp_index(wk_flip, wh2)); }
                }
            }
        } else {
            let from = mv.from().expect("from exists");
            let to = mv.to();
            let moving_piece = pre_pos.piece_at(from).expect("piece at from");
            if let Some(b) = BonaPiece::from_board(moving_piece, from) { removed.push(halfkp_index(bk, b)); }
            if let Some(w) = BonaPiece::from_board(moving_piece.flip_color(), from.flip()) { removed.push(halfkp_index(wk_flip, w)); }

            let dest_piece = if mv.is_promote() { moving_piece.promote() } else { moving_piece };
            if let Some(b) = BonaPiece::from_board(dest_piece, to) { added.push(halfkp_index(bk, b)); }
            if let Some(w) = BonaPiece::from_board(dest_piece.flip_color(), to.flip()) { added.push(halfkp_index(wk_flip, w)); }

            if let Some(captured) = pre_pos.piece_at(to) {
                if let Some(b) = BonaPiece::from_board(captured, to) { removed.push(halfkp_index(bk, b)); }
                if let Some(w) = BonaPiece::from_board(captured.flip_color(), to.flip()) { removed.push(halfkp_index(wk_flip, w)); }

                let hand_type = captured.piece_type;
                let hand_idx = piece_type_to_hand_index(hand_type).expect("hand type");
                let color = pre_pos.side_to_move;
                let new_count = pre_pos.hands[color as usize][hand_idx] + 1;
                if let Ok(bh) = BonaPiece::from_hand(hand_type, color, new_count) { added.push(halfkp_index(bk, bh)); }
                if let Ok(wh) = BonaPiece::from_hand(hand_type, color.flip(), new_count) { added.push(halfkp_index(wk_flip, wh)); }
                if new_count > 1 {
                    if let Ok(bh_old) = BonaPiece::from_hand(hand_type, color, new_count - 1) { removed.push(halfkp_index(bk, bh_old)); }
                    if let Ok(wh_old) = BonaPiece::from_hand(hand_type, color.flip(), new_count - 1) { removed.push(halfkp_index(wk_flip, wh_old)); }
                }
            }
        }

        for &fid in &removed {
            if fid >= net.n_feat { continue; }
            let base = fid * d;
            let row = &net.w0[base..base + d];
            for i in 0..d { next.black[i] -= row[i]; next.white[i] -= row[i]; }
        }
        for &fid in &added {
            if fid >= net.n_feat { continue; }
            let base = fid * d;
            let row = &net.w0[base..base + d];
            for i in 0..d { next.black[i] += row[i]; next.white[i] += row[i]; }
        }

        for v in &mut next.black { if *v < 0.0 { *v = 0.0; } }
        for v in &mut next.white { if *v < 0.0 { *v = 0.0; } }

        next
    }
}
