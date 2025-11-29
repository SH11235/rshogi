//! 局面（Position）

use crate::bitboard::{
    bishop_effect, dragon_effect, gold_effect, horse_effect, king_effect, knight_effect,
    lance_effect, pawn_effect, rook_effect, silver_effect, Bitboard,
};
use crate::types::{Color, Hand, Move, Piece, PieceType, Square};

use super::state::{ChangedPiece, StateInfo};
use super::zobrist::{zobrist_hand, zobrist_psq, zobrist_side};

/// 将棋の局面
pub struct Position {
    // === 盤面 ===
    /// 各マスの駒 [Square]
    pub(super) board: [Piece; Square::NUM],
    /// 駒種別Bitboard [PieceType]
    pub(super) by_type: [Bitboard; PieceType::NUM + 1],
    /// 先後別Bitboard
    pub(super) by_color: [Bitboard; Color::NUM],

    // === 手駒 ===
    /// 手駒 [Color]
    pub(super) hand: [Hand; Color::NUM],

    // === 状態 ===
    /// 現在の状態
    pub(super) state: Box<StateInfo>,
    /// 初期局面からの手数
    pub(super) game_ply: i32,
    /// 手番
    pub(super) side_to_move: Color,
    /// 玉の位置 [Color]
    pub(super) king_square: [Square; Color::NUM],
}

impl Position {
    // ========== 局面設定 ==========

    /// 空の局面を生成
    pub fn new() -> Self {
        Position {
            board: [Piece::NONE; Square::NUM],
            by_type: [Bitboard::EMPTY; PieceType::NUM + 1],
            by_color: [Bitboard::EMPTY; Color::NUM],
            hand: [Hand::EMPTY; Color::NUM],
            state: Box::new(StateInfo::new()),
            game_ply: 0,
            side_to_move: Color::Black,
            king_square: [Square::SQ_11; Color::NUM],
        }
    }

    // ========== 盤面アクセス ==========

    /// 指定マスの駒を取得
    #[inline]
    pub fn piece_on(&self, sq: Square) -> Piece {
        self.board[sq.index()]
    }

    /// 全駒のBitboard（占有）
    #[inline]
    pub fn occupied(&self) -> Bitboard {
        self.by_color[Color::Black.index()] | self.by_color[Color::White.index()]
    }

    /// 指定駒種のBitboard
    #[inline]
    pub fn pieces_pt(&self, pt: PieceType) -> Bitboard {
        self.by_type[pt as usize]
    }

    /// 指定手番の駒のBitboard
    #[inline]
    pub fn pieces_c(&self, c: Color) -> Bitboard {
        self.by_color[c.index()]
    }

    /// 指定手番・駒種のBitboard
    #[inline]
    pub fn pieces(&self, c: Color, pt: PieceType) -> Bitboard {
        self.by_color[c.index()] & self.by_type[pt as usize]
    }

    /// 手駒を取得
    #[inline]
    pub fn hand(&self, c: Color) -> Hand {
        self.hand[c.index()]
    }

    /// 玉の位置を取得
    #[inline]
    pub fn king_square(&self, c: Color) -> Square {
        self.king_square[c.index()]
    }

    /// 手番を取得
    #[inline]
    pub fn side_to_move(&self) -> Color {
        self.side_to_move
    }

    /// TT等に保存された16bit指し手を安全に取り出す
    /// - 無効な符号化や手番不一致の手はNone
    /// - 合法性までは保証しないが、明らかに不整合な手を弾く
    pub fn to_move(&self, mv: Move) -> Option<Move> {
        if mv.is_none() {
            return Some(Move::NONE);
        }

        if mv.is_drop() {
            let pt = mv.drop_piece_type();
            if self.hand(self.side_to_move).has(pt) {
                Some(mv)
            } else {
                None
            }
        } else {
            let from = mv.from();
            let pc = self.piece_on(from);
            if pc.is_some() && pc.color() == self.side_to_move {
                Some(mv)
            } else {
                None
            }
        }
    }

    /// 手数を取得
    #[inline]
    pub fn game_ply(&self) -> i32 {
        self.game_ply
    }

    /// 現在の状態を取得
    #[inline]
    pub fn state(&self) -> &StateInfo {
        &self.state
    }

    /// 現在の状態を可変で取得（NNUE差分更新など内部状態の更新用）
    #[inline]
    pub fn state_mut(&mut self) -> &mut StateInfo {
        &mut self.state
    }

    /// 局面のハッシュキー
    #[inline]
    pub fn key(&self) -> u64 {
        self.state.key()
    }

    // ========== 利き計算 ==========

    /// 指定マスに利いている駒（全手番）
    pub fn attackers_to(&self, sq: Square) -> Bitboard {
        self.attackers_to_occ(sq, self.occupied())
    }

    /// 指定マスに利いている駒（占有指定）
    pub fn attackers_to_occ(&self, sq: Square, occupied: Bitboard) -> Bitboard {
        // 各駒種から逆方向に利きを求める
        // 例: sqに歩で利いている駒 = sqから後手の歩の利き方向にある先手の歩
        //     sqに後手歩で利いている駒 = sqから先手の歩の利き方向にある後手の歩

        let b_pawn = pawn_effect(Color::White, sq) & self.pieces(Color::Black, PieceType::Pawn);
        let w_pawn = pawn_effect(Color::Black, sq) & self.pieces(Color::White, PieceType::Pawn);

        let b_knight =
            knight_effect(Color::White, sq) & self.pieces(Color::Black, PieceType::Knight);
        let w_knight =
            knight_effect(Color::Black, sq) & self.pieces(Color::White, PieceType::Knight);

        let b_silver =
            silver_effect(Color::White, sq) & self.pieces(Color::Black, PieceType::Silver);
        let w_silver =
            silver_effect(Color::Black, sq) & self.pieces(Color::White, PieceType::Silver);

        // 金の動きをする駒（金、と、成香、成桂、成銀）
        let gold_movers_b = self.pieces(Color::Black, PieceType::Gold)
            | self.pieces(Color::Black, PieceType::ProPawn)
            | self.pieces(Color::Black, PieceType::ProLance)
            | self.pieces(Color::Black, PieceType::ProKnight)
            | self.pieces(Color::Black, PieceType::ProSilver);
        let gold_movers_w = self.pieces(Color::White, PieceType::Gold)
            | self.pieces(Color::White, PieceType::ProPawn)
            | self.pieces(Color::White, PieceType::ProLance)
            | self.pieces(Color::White, PieceType::ProKnight)
            | self.pieces(Color::White, PieceType::ProSilver);

        let b_gold = gold_effect(Color::White, sq) & gold_movers_b;
        let w_gold = gold_effect(Color::Black, sq) & gold_movers_w;

        let king = king_effect(sq)
            & (self.pieces(Color::Black, PieceType::King)
                | self.pieces(Color::White, PieceType::King));

        // 遠方駒
        let b_lance =
            lance_effect(Color::White, sq, occupied) & self.pieces(Color::Black, PieceType::Lance);
        let w_lance =
            lance_effect(Color::Black, sq, occupied) & self.pieces(Color::White, PieceType::Lance);

        let bishop_bb = self.pieces_pt(PieceType::Bishop) | self.pieces_pt(PieceType::Horse);
        let bishop = bishop_effect(sq, occupied) & bishop_bb;

        let rook_bb = self.pieces_pt(PieceType::Rook) | self.pieces_pt(PieceType::Dragon);
        let rook = rook_effect(sq, occupied) & rook_bb;

        // 馬・龍の近接利き
        let horse = king_effect(sq) & self.pieces_pt(PieceType::Horse);
        let dragon = king_effect(sq) & self.pieces_pt(PieceType::Dragon);

        b_pawn
            | w_pawn
            | b_knight
            | w_knight
            | b_silver
            | w_silver
            | b_gold
            | w_gold
            | king
            | b_lance
            | w_lance
            | bishop
            | rook
            | horse
            | dragon
    }

    /// 指定マスに利いている指定手番の駒
    pub fn attackers_to_c(&self, sq: Square, c: Color) -> Bitboard {
        self.attackers_to_occ(sq, self.occupied()) & self.pieces_c(c)
    }

    /// 自玉へのピン駒
    #[inline]
    pub fn blockers_for_king(&self, c: Color) -> Bitboard {
        self.state.blockers_for_king[c.index()]
    }

    /// 王手している駒
    #[inline]
    pub fn checkers(&self) -> Bitboard {
        self.state.checkers
    }

    /// 王手されているか
    #[inline]
    pub fn in_check(&self) -> bool {
        !self.state.checkers.is_empty()
    }

    /// 指定駒種で王手となる升
    #[inline]
    pub fn check_squares(&self, pt: PieceType) -> Bitboard {
        self.state.check_squares[pt as usize]
    }

    // ========== 内部操作 ==========

    /// 盤面に駒を置く
    pub(super) fn put_piece(&mut self, pc: Piece, sq: Square) {
        debug_assert!(self.board[sq.index()].is_none());
        self.board[sq.index()] = pc;
        self.by_type[pc.piece_type() as usize].set(sq);
        self.by_color[pc.color().index()].set(sq);
    }

    /// 盤面から駒を取り除く
    fn remove_piece(&mut self, sq: Square) {
        let pc = self.board[sq.index()];
        debug_assert!(pc.is_some());
        self.board[sq.index()] = Piece::NONE;
        self.by_type[pc.piece_type() as usize].clear(sq);
        self.by_color[pc.color().index()].clear(sq);
    }

    /// pin駒とpinしている駒を更新
    pub(super) fn update_blockers_and_pinners(&mut self) {
        for c in [Color::Black, Color::White] {
            self.state.blockers_for_king[c.index()] = Bitboard::EMPTY;
            self.state.pinners[c.index()] = Bitboard::EMPTY;

            let ksq = self.king_square[c.index()];
            let them = !c;
            let occupied = self.occupied();

            // 敵の遠方駒からの利き
            let snipers = (lance_effect(c, ksq, Bitboard::EMPTY)
                & self.pieces(them, PieceType::Lance))
                | (bishop_effect(ksq, Bitboard::EMPTY)
                    & (self.pieces(them, PieceType::Bishop) | self.pieces(them, PieceType::Horse)))
                | (rook_effect(ksq, Bitboard::EMPTY)
                    & (self.pieces(them, PieceType::Rook) | self.pieces(them, PieceType::Dragon)));

            for sniper_sq in snipers.iter() {
                // 玉とsniperの間にある駒
                let between = crate::bitboard::between_bb(ksq, sniper_sq) & occupied;
                // 間に1枚だけある場合、それがblocker
                if !between.is_empty() && !between.more_than_one() {
                    self.state.blockers_for_king[c.index()] =
                        self.state.blockers_for_king[c.index()] | between;
                    // blockerが敵の駒なら、sniperはpinner
                    if (between & self.pieces_c(them)).is_empty() {
                        // blockerは自駒なので、sniperはpinner
                        self.state.pinners[c.index()].set(sniper_sq);
                    }
                }
            }
        }
    }

    /// 王手マスを更新
    pub(super) fn update_check_squares(&mut self) {
        let them = !self.side_to_move;
        let ksq = self.king_square[them.index()];
        let occupied = self.occupied();

        // 各駒種で王手となるマス
        self.state.check_squares[PieceType::Pawn as usize] = pawn_effect(them, ksq);
        self.state.check_squares[PieceType::Knight as usize] = knight_effect(them, ksq);
        self.state.check_squares[PieceType::Silver as usize] = silver_effect(them, ksq);
        self.state.check_squares[PieceType::Gold as usize] = gold_effect(them, ksq);
        self.state.check_squares[PieceType::King as usize] = Bitboard::EMPTY; // 玉で王手はない
        self.state.check_squares[PieceType::Lance as usize] = lance_effect(them, ksq, occupied);
        self.state.check_squares[PieceType::Bishop as usize] = bishop_effect(ksq, occupied);
        self.state.check_squares[PieceType::Rook as usize] = rook_effect(ksq, occupied);

        // 成駒
        self.state.check_squares[PieceType::ProPawn as usize] = gold_effect(them, ksq);
        self.state.check_squares[PieceType::ProLance as usize] = gold_effect(them, ksq);
        self.state.check_squares[PieceType::ProKnight as usize] = gold_effect(them, ksq);
        self.state.check_squares[PieceType::ProSilver as usize] = gold_effect(them, ksq);
        self.state.check_squares[PieceType::Horse as usize] = horse_effect(ksq, occupied);
        self.state.check_squares[PieceType::Dragon as usize] = dragon_effect(ksq, occupied);
    }

    // ========== 指し手実行 ==========

    /// 指し手を実行
    pub fn do_move(&mut self, m: Move, gives_check: bool) {
        let us = self.side_to_move;
        let them = !us;

        // 1. 新しいStateInfoを作成
        let mut new_state = Box::new(self.state.partial_clone());
        // NNUE 関連は毎手リセットし、DirtyPiece はここで構築する。
        new_state.accumulator.reset();
        new_state.dirty_piece.clear();

        // 2. 局面情報の更新
        self.game_ply += 1;
        new_state.plies_from_null += 1;

        // 3. 手番の変更とハッシュ更新
        new_state.board_key ^= zobrist_side();

        // 4. 駒の移動
        if m.is_drop() {
            let pt = m.drop_piece_type();
            let to = m.to();
            let pc = Piece::new(us, pt);

            // DirtyPiece: 手駒の変化（us の pt が 1 減る）
            let old_hand = self.hand[us.index()];
            let old_count = old_hand.count(pt) as u8;
            let new_count = old_count.saturating_sub(1);
            new_state.dirty_piece.hand_changes.push(super::state::HandChange {
                owner: us,
                piece_type: pt,
                old_count,
                new_count,
            });

            // 手駒から減らす
            self.hand[us.index()] = self.hand[us.index()].sub(pt);
            new_state.hand_key ^= zobrist_hand(us, pt);

            // 盤上に配置
            self.put_piece(pc, to);
            new_state.board_key ^= zobrist_psq(pc, to);

            new_state.captured_piece = Piece::NONE;

            // DirtyPiece: 打ち駒（盤上に新しく現れる）
            new_state.dirty_piece.pieces.push(ChangedPiece {
                color: us,
                old_piece: Piece::NONE,
                old_sq: None,
                new_piece: pc,
                new_sq: Some(to),
            });
        } else {
            let from = m.from();
            let to = m.to();
            let pc = self.piece_on(from);
            let captured = self.piece_on(to);

            // 駒を取る場合
            if captured.is_some() {
                let captured_pt = captured.piece_type().unpromote();
                // DirtyPiece: 手駒の変化（us の captured_pt が 1 増える）
                let old_hand = self.hand[us.index()];
                let old_count = old_hand.count(captured_pt) as u8;
                let new_count = old_count.saturating_add(1);
                new_state.dirty_piece.hand_changes.push(super::state::HandChange {
                    owner: us,
                    piece_type: captured_pt,
                    old_count,
                    new_count,
                });

                self.remove_piece(to);
                new_state.board_key ^= zobrist_psq(captured, to);

                // 手駒に追加（成駒は生駒に戻す）
                self.hand[us.index()] = self.hand[us.index()].add(captured_pt);
                new_state.hand_key ^= zobrist_hand(us, captured_pt);

                // 駒割評価値の更新
                // TODO: material_value の更新
            }
            new_state.captured_piece = captured;

            // 駒を移動
            self.remove_piece(from);
            new_state.board_key ^= zobrist_psq(pc, from);

            let moved_pc = if m.is_promote() {
                pc.promote().unwrap()
            } else {
                pc
            };
            self.put_piece(moved_pc, to);
            new_state.board_key ^= zobrist_psq(moved_pc, to);

            // 玉の移動
            if pc.piece_type() == PieceType::King {
                self.king_square[us.index()] = to;
                new_state.dirty_piece.king_moved[us.index()] = true;
            }

            // DirtyPiece: 移動した駒
            new_state.dirty_piece.pieces.push(ChangedPiece {
                color: us,
                old_piece: pc,
                old_sq: Some(from),
                new_piece: moved_pc,
                new_sq: Some(to),
            });

            // DirtyPiece: 取った駒（盤上から消える）
            if captured.is_some() {
                new_state.dirty_piece.pieces.push(ChangedPiece {
                    color: them,
                    old_piece: captured,
                    old_sq: Some(to),
                    new_piece: Piece::NONE,
                    new_sq: None,
                });
            }
        }

        // 5. 手番交代
        self.side_to_move = them;

        // 6. 王手情報の更新
        new_state.checkers = if gives_check {
            self.attackers_to_c(self.king_square[them.index()], us)
        } else {
            Bitboard::EMPTY
        };

        // 7. StateInfoの付け替え
        new_state.last_move = m;
        let old_state = std::mem::replace(&mut self.state, new_state);
        self.state.previous = Some(old_state);

        // 8. pin情報と王手マスの更新
        self.update_blockers_and_pinners();
        self.update_check_squares();
    }

    /// 指し手を戻す
    pub fn undo_move(&mut self, m: Move) {
        // 1. 手番を戻す
        self.side_to_move = !self.side_to_move;
        self.game_ply -= 1;
        let us = self.side_to_move;

        // 2. 駒の移動を戻す
        if m.is_drop() {
            let pt = m.drop_piece_type();
            let to = m.to();

            // 盤上から除去
            self.remove_piece(to);
            // 手駒に戻す
            self.hand[us.index()] = self.hand[us.index()].add(pt);
        } else {
            let from = m.from();
            let to = m.to();
            let moved_pc = self.piece_on(to);
            let original_pc = if m.is_promote() {
                moved_pc.unpromote()
            } else {
                moved_pc
            };

            // 駒を元の位置に戻す
            self.remove_piece(to);
            self.put_piece(original_pc, from);

            // 玉の移動を戻す
            if original_pc.piece_type() == PieceType::King {
                self.king_square[us.index()] = from;
            }

            // 取った駒を復元
            let captured = self.state.captured_piece;
            if captured.is_some() {
                self.put_piece(captured, to);
                // 手駒から除去
                let cap_pt = captured.piece_type().unpromote();
                self.hand[us.index()] = self.hand[us.index()].sub(cap_pt);
            }
        }

        // 3. StateInfoを戻す
        self.state = self.state.previous.take().unwrap();
    }

    /// null moveを実行
    pub fn do_null_move(&mut self) {
        let mut new_state = Box::new(self.state.partial_clone());
        new_state.accumulator.reset();
        new_state.dirty_piece.clear();

        new_state.board_key ^= zobrist_side();
        new_state.plies_from_null = 0;
        new_state.captured_piece = Piece::NONE;
        new_state.last_move = Move::NULL;

        self.side_to_move = !self.side_to_move;

        let old_state = std::mem::replace(&mut self.state, new_state);
        self.state.previous = Some(old_state);

        // null move後は王手されていないはず
        self.state.checkers = Bitboard::EMPTY;

        self.update_blockers_and_pinners();
        self.update_check_squares();
    }

    /// null moveを戻す
    pub fn undo_null_move(&mut self) {
        self.side_to_move = !self.side_to_move;
        self.state = self.state.previous.take().unwrap();
    }

    /// 王手になるかどうか
    pub fn gives_check(&self, m: Move) -> bool {
        let us = self.side_to_move;
        let to = m.to();

        if m.is_drop() {
            // 打ち駒の場合：打った駒が王手マスにあるか
            let pt = m.drop_piece_type();
            return self.check_squares(pt).contains(to);
        }

        let from = m.from();
        let pc = self.piece_on(from);
        let pt = pc.piece_type();

        // 直接王手：移動先が王手マスにあるか
        let moved_pt = if m.is_promote() {
            pt.promote().unwrap()
        } else {
            pt
        };
        if self.check_squares(moved_pt).contains(to) {
            return true;
        }

        // 開き王手：fromがblockerで、fromが王との直線上から外れるか
        let them = !us;
        let ksq = self.king_square[them.index()];
        let blockers = self.blockers_for_king(them);

        if blockers.contains(from) {
            // fromが王との直線上にある場合、toも同じ直線上にないと開き王手
            let line = crate::bitboard::line_bb(ksq, from);
            if !line.contains(to) {
                return true;
            }
        }

        false
    }
}

impl Default for Position {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{File, Rank};

    #[test]
    fn test_position_new() {
        let pos = Position::new();
        assert_eq!(pos.side_to_move(), Color::Black);
        assert_eq!(pos.game_ply(), 0);
        assert!(pos.occupied().is_empty());
    }

    #[test]
    fn test_put_and_remove_piece() {
        let mut pos = Position::new();
        let sq = Square::new(File::File5, Rank::Rank5);

        pos.put_piece(Piece::B_PAWN, sq);
        assert_eq!(pos.piece_on(sq), Piece::B_PAWN);
        assert!(pos.pieces(Color::Black, PieceType::Pawn).contains(sq));

        pos.remove_piece(sq);
        assert_eq!(pos.piece_on(sq), Piece::NONE);
        assert!(!pos.pieces(Color::Black, PieceType::Pawn).contains(sq));
    }

    #[test]
    fn test_attackers_to_pawn() {
        let mut pos = Position::new();
        // 5五に先手歩
        let sq55 = Square::new(File::File5, Rank::Rank5);
        let sq54 = Square::new(File::File5, Rank::Rank4);
        pos.put_piece(Piece::B_PAWN, sq55);

        // 5四への利き
        let attackers = pos.attackers_to(sq54);
        assert!(attackers.contains(sq55));
    }

    #[test]
    fn test_do_move_drop() {
        let mut pos = Position::new();
        // 玉を配置
        let sq59 = Square::new(File::File5, Rank::Rank9);
        let sq51 = Square::new(File::File5, Rank::Rank1);
        pos.put_piece(Piece::B_KING, sq59);
        pos.put_piece(Piece::W_KING, sq51);
        pos.king_square[Color::Black.index()] = sq59;
        pos.king_square[Color::White.index()] = sq51;

        // 先手に歩を持たせる
        pos.hand[Color::Black.index()] = pos.hand[Color::Black.index()].add(PieceType::Pawn);

        // 5五歩打ち
        let to = Square::new(File::File5, Rank::Rank5);
        let m = Move::new_drop(PieceType::Pawn, to);

        pos.do_move(m, false);

        assert_eq!(pos.piece_on(to), Piece::B_PAWN);
        assert_eq!(pos.side_to_move(), Color::White);
        assert!(!pos.hand(Color::Black).has(PieceType::Pawn));

        pos.undo_move(m);

        assert_eq!(pos.piece_on(to), Piece::NONE);
        assert_eq!(pos.side_to_move(), Color::Black);
        assert!(pos.hand(Color::Black).has(PieceType::Pawn));
    }

    #[test]
    fn test_do_move_normal() {
        let mut pos = Position::new();
        // 7七に先手歩、玉を配置
        let sq77 = Square::new(File::File7, Rank::Rank7);
        let sq76 = Square::new(File::File7, Rank::Rank6);
        let sq59 = Square::new(File::File5, Rank::Rank9);
        let sq51 = Square::new(File::File5, Rank::Rank1);

        pos.put_piece(Piece::B_PAWN, sq77);
        pos.put_piece(Piece::B_KING, sq59);
        pos.put_piece(Piece::W_KING, sq51);
        pos.king_square[Color::Black.index()] = sq59;
        pos.king_square[Color::White.index()] = sq51;

        // 7六歩
        let m = Move::new_move(sq77, sq76, false);

        pos.do_move(m, false);

        assert_eq!(pos.piece_on(sq77), Piece::NONE);
        assert_eq!(pos.piece_on(sq76), Piece::B_PAWN);
        assert_eq!(pos.side_to_move(), Color::White);

        pos.undo_move(m);

        assert_eq!(pos.piece_on(sq77), Piece::B_PAWN);
        assert_eq!(pos.piece_on(sq76), Piece::NONE);
        assert_eq!(pos.side_to_move(), Color::Black);
    }

    #[test]
    fn test_do_move_capture() {
        let mut pos = Position::new();
        // 7六に先手歩、7五に後手歩、玉を配置
        let sq76 = Square::new(File::File7, Rank::Rank6);
        let sq75 = Square::new(File::File7, Rank::Rank5);
        let sq59 = Square::new(File::File5, Rank::Rank9);
        let sq51 = Square::new(File::File5, Rank::Rank1);

        pos.put_piece(Piece::B_PAWN, sq76);
        pos.put_piece(Piece::W_PAWN, sq75);
        pos.put_piece(Piece::B_KING, sq59);
        pos.put_piece(Piece::W_KING, sq51);
        pos.king_square[Color::Black.index()] = sq59;
        pos.king_square[Color::White.index()] = sq51;

        // 7五歩（取る）
        let m = Move::new_move(sq76, sq75, false);

        pos.do_move(m, false);

        assert_eq!(pos.piece_on(sq76), Piece::NONE);
        assert_eq!(pos.piece_on(sq75), Piece::B_PAWN);
        assert!(pos.hand(Color::Black).has(PieceType::Pawn));
        assert_eq!(pos.side_to_move(), Color::White);

        pos.undo_move(m);

        assert_eq!(pos.piece_on(sq76), Piece::B_PAWN);
        assert_eq!(pos.piece_on(sq75), Piece::W_PAWN);
        assert!(!pos.hand(Color::Black).has(PieceType::Pawn));
        assert_eq!(pos.side_to_move(), Color::Black);
    }

    #[test]
    fn test_do_move_promote() {
        let mut pos = Position::new();
        // 2三に先手歩、玉を配置
        let sq23 = Square::new(File::File2, Rank::Rank3);
        let sq22 = Square::new(File::File2, Rank::Rank2);
        let sq59 = Square::new(File::File5, Rank::Rank9);
        let sq51 = Square::new(File::File5, Rank::Rank1);

        pos.put_piece(Piece::B_PAWN, sq23);
        pos.put_piece(Piece::B_KING, sq59);
        pos.put_piece(Piece::W_KING, sq51);
        pos.king_square[Color::Black.index()] = sq59;
        pos.king_square[Color::White.index()] = sq51;

        // 2二歩成
        let m = Move::new_move(sq23, sq22, true);

        pos.do_move(m, false);

        assert_eq!(pos.piece_on(sq23), Piece::NONE);
        assert_eq!(pos.piece_on(sq22), Piece::B_PRO_PAWN);

        pos.undo_move(m);

        assert_eq!(pos.piece_on(sq23), Piece::B_PAWN);
        assert_eq!(pos.piece_on(sq22), Piece::NONE);
    }
}
