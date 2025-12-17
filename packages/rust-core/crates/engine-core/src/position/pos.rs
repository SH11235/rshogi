//! 局面（Position）

use crate::bitboard::{
    bishop_effect, dragon_effect, gold_effect, horse_effect, king_effect, knight_effect,
    lance_effect, pawn_effect, rook_effect, silver_effect, Bitboard,
};
use crate::eval::material::{hand_piece_value, signed_piece_value};
use crate::types::{
    Color, Hand, Move, Piece, PieceType, PieceTypeSet, RepetitionState, Square, Value,
};

use super::state::{ChangedPiece, StateInfo};
use super::zobrist::{zobrist_hand, zobrist_psq, zobrist_side};

/// 小駒（香・桂・銀・金とその成り駒）かどうか
#[inline]
pub(super) fn is_minor_piece(pc: Piece) -> bool {
    matches!(
        pc.piece_type(),
        PieceType::Lance
            | PieceType::Knight
            | PieceType::Silver
            | PieceType::Gold
            | PieceType::ProPawn
            | PieceType::ProLance
            | PieceType::ProKnight
            | PieceType::ProSilver
    )
}

/// 将棋の局面
#[derive(Clone)]
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
    /// 状態スタック
    pub(super) state_stack: Vec<StateInfo>,
    /// 現在の状態インデックス
    state_idx: usize,
    /// 初期局面からの手数
    pub(super) game_ply: i32,
    /// 手番
    pub(super) side_to_move: Color,
    /// 玉の位置 [Color]
    pub(super) king_square: [Square; Color::NUM],
}

impl Position {
    /// 部分ハッシュを更新（XOR）
    #[inline]
    fn xor_partial_keys(&self, st: &mut StateInfo, pc: Piece, sq: Square) {
        if pc.piece_type() == PieceType::Pawn {
            st.pawn_key ^= zobrist_psq(pc, sq);
        } else {
            if is_minor_piece(pc) {
                st.minor_piece_key ^= zobrist_psq(pc, sq);
            }
            st.non_pawn_key[pc.color().index()] ^= zobrist_psq(pc, sq);
        }
    }

    #[inline]
    fn cur_state(&self) -> &StateInfo {
        &self.state_stack[self.state_idx]
    }

    #[inline]
    fn cur_state_mut(&mut self) -> &mut StateInfo {
        &mut self.state_stack[self.state_idx]
    }

    /// 状態スタックに新しい StateInfo を積む（必要なら再利用）
    #[inline]
    fn push_state(&mut self, mut st: StateInfo) {
        st.previous = Some(self.state_idx);
        let next_idx = self.state_idx + 1;
        if self.state_stack.len() > next_idx {
            self.state_stack[next_idx] = st;
        } else {
            self.state_stack.push(st);
        }
        self.state_idx = next_idx;
    }

    // ========== 局面設定 ==========

    /// 空の局面を生成
    pub fn new() -> Self {
        Position {
            board: [Piece::NONE; Square::NUM],
            by_type: [Bitboard::EMPTY; PieceType::NUM + 1],
            by_color: [Bitboard::EMPTY; Color::NUM],
            hand: [Hand::EMPTY; Color::NUM],
            state_stack: vec![StateInfo::new()],
            state_idx: 0,
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

    /// 直前の手で取られた駒を返す
    ///
    /// YaneuraOu: pos.captured_piece()
    #[inline]
    pub fn captured_piece(&self) -> Piece {
        self.cur_state().captured_piece
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

    /// 駒種集合のBitboard（先後無視）
    #[inline]
    pub fn pieces_by_types(&self, set: PieceTypeSet) -> Bitboard {
        if set.is_empty() {
            return Bitboard::EMPTY;
        }
        if set.is_all() {
            return self.occupied();
        }

        let mut bb = Bitboard::EMPTY;
        for pt in set.iter() {
            bb |= self.by_type[pt as usize];
        }
        bb
    }

    /// 駒種集合のBitboard（手番指定）
    #[inline]
    pub fn pieces_c_by_types(&self, c: Color, set: PieceTypeSet) -> Bitboard {
        if set.is_empty() {
            return Bitboard::EMPTY;
        }
        if set.is_all() {
            return self.by_color[c.index()];
        }

        let mut bb = Bitboard::EMPTY;
        for pt in set.iter() {
            bb |= self.by_type[pt as usize] & self.by_color[c.index()];
        }
        bb
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

    /// 千日手/優劣局面判定（do_move 時に計算した情報を使用）
    pub fn repetition_state(&self, ply: i32) -> RepetitionState {
        let rep = self.cur_state().repetition;
        if rep != 0 && rep.abs() < ply {
            return self.cur_state().repetition_type;
        }

        RepetitionState::None
    }

    /// 現在の状態を取得
    #[inline]
    pub fn state(&self) -> &StateInfo {
        self.cur_state()
    }

    /// 直前の局面の状態（StateInfo）を取得
    pub fn previous_state(&self) -> Option<&StateInfo> {
        self.cur_state().previous.map(|idx| &self.state_stack[idx])
    }

    /// 現在の状態を可変で取得（NNUE差分更新など内部状態の更新用）
    #[inline]
    pub fn state_mut(&mut self) -> &mut StateInfo {
        self.cur_state_mut()
    }

    /// 局面のハッシュキー
    #[inline]
    pub fn key(&self) -> u64 {
        self.cur_state().key()
    }

    /// 歩ハッシュ
    #[inline]
    pub fn pawn_key(&self) -> u64 {
        self.cur_state().pawn_key
    }

    /// 小駒ハッシュ
    #[inline]
    pub fn minor_piece_key(&self) -> u64 {
        self.cur_state().minor_piece_key
    }

    /// 歩以外のハッシュ（手番別）
    #[inline]
    pub fn non_pawn_key(&self, c: Color) -> u64 {
        self.cur_state().non_pawn_key[c.index()]
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
        self.cur_state().blockers_for_king[c.index()]
    }

    /// 王手している駒
    #[inline]
    pub fn checkers(&self) -> Bitboard {
        self.cur_state().checkers
    }

    /// 王手されているか
    #[inline]
    pub fn in_check(&self) -> bool {
        !self.cur_state().checkers.is_empty()
    }

    /// 指定駒種で王手となる升
    #[inline]
    pub fn check_squares(&self, pt: PieceType) -> Bitboard {
        self.cur_state().check_squares[pt as usize]
    }

    /// 現在のpin状態（指定升を除外）
    pub fn pinned_pieces(&self, them: Color, avoid: Square) -> Bitboard {
        self.pinned_pieces_excluding(them, avoid)
    }

    /// fromを取り除いた占有でのpin駒（やねうら王のpinned_pieces<Them>(from)相当）
    pub fn pinned_pieces_excluding(&self, them: Color, avoid: Square) -> Bitboard {
        let occ = self.occupied() & !Bitboard::from_square(avoid);
        self.pinned_pieces_with_occupancy(them, occ, Bitboard::EMPTY)
    }

    /// from->toに動かした後の占有でのpin駒（やねうら王のpinned_pieces(Them, from, to)相当）
    pub fn pinned_pieces_after_move(&self, them: Color, from: Square, to: Square) -> Bitboard {
        let mut occ = self.occupied();
        occ ^= Bitboard::from_square(from);
        occ |= Bitboard::from_square(to);

        let enemy = !them;
        let enemy_removed = if self.piece_on(to).is_some() && self.piece_on(to).color() == enemy {
            Bitboard::from_square(to)
        } else {
            Bitboard::EMPTY
        };

        self.pinned_pieces_with_occupancy(them, occ, enemy_removed)
    }

    /// fromの駒を動かしたときに開き王手になるか（簡易判定）
    pub fn discovered(&self, from: Square, to: Square, ksq: Square, pinned: Bitboard) -> bool {
        pinned.contains(from) && !crate::mate::aligned(from, to, ksq)
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
            let (blockers, pinners) =
                self.compute_blockers_and_pinners(c, self.occupied(), Bitboard::EMPTY);
            let st = self.cur_state_mut();
            st.blockers_for_king[c.index()] = blockers;
            st.pinners[c.index()] = pinners;
        }
    }

    /// 王手マスを更新
    pub(super) fn update_check_squares(&mut self) {
        let them = !self.side_to_move;
        let ksq = self.king_square[them.index()];
        let occupied = self.occupied();
        let st = self.cur_state_mut();

        // 各駒種で王手となるマス
        st.check_squares[PieceType::Pawn as usize] = pawn_effect(them, ksq);
        st.check_squares[PieceType::Knight as usize] = knight_effect(them, ksq);
        st.check_squares[PieceType::Silver as usize] = silver_effect(them, ksq);
        st.check_squares[PieceType::Gold as usize] = gold_effect(them, ksq);
        st.check_squares[PieceType::King as usize] = Bitboard::EMPTY; // 玉で王手はない
        st.check_squares[PieceType::Lance as usize] = lance_effect(them, ksq, occupied);
        st.check_squares[PieceType::Bishop as usize] = bishop_effect(ksq, occupied);
        st.check_squares[PieceType::Rook as usize] = rook_effect(ksq, occupied);

        // 成駒
        st.check_squares[PieceType::ProPawn as usize] = gold_effect(them, ksq);
        st.check_squares[PieceType::ProLance as usize] = gold_effect(them, ksq);
        st.check_squares[PieceType::ProKnight as usize] = gold_effect(them, ksq);
        st.check_squares[PieceType::ProSilver as usize] = gold_effect(them, ksq);
        st.check_squares[PieceType::Horse as usize] = horse_effect(ksq, occupied);
        st.check_squares[PieceType::Dragon as usize] = dragon_effect(ksq, occupied);
    }

    // ========== 指し手実行 ==========

    /// 指し手を実行
    pub fn do_move(&mut self, m: Move, gives_check: bool) {
        let us = self.side_to_move;
        let them = !us;
        let prev_continuous = self.cur_state().continuous_check;

        // 現在の占有とblockers/pinners、玉位置を退避（差分更新で利用）
        let prev_blockers = self.cur_state().blockers_for_king;
        let prev_pinners = self.cur_state().pinners;
        let prev_king_sq = self.king_square;

        // 1. 新しいStateInfoを作成
        let mut new_state = self.cur_state().partial_clone();
        // NNUE 関連は毎手リセットし、DirtyPiece はここで構築する。
        new_state.accumulator.reset();
        new_state.dirty_piece.clear();
        let mut material_value = new_state.material_value.raw();

        // 2. 局面情報の更新
        self.game_ply += 1;
        new_state.plies_from_null += 1;

        // 3. 手番の変更とハッシュ更新
        new_state.board_key ^= zobrist_side();

        // 4. 駒の移動
        let mut moved_from: Option<Square> = None;
        let moved_to: Square;
        let moved_pt: PieceType;

        if m.is_drop() {
            let pt = m.drop_piece_type();
            let to = m.to();
            let pc = Piece::new(us, pt);
            moved_to = to;
            moved_pt = pt;

            // DirtyPiece: 手駒の変化（us の pt が 1 減る）
            let old_hand = self.hand[us.index()];
            let old_count = old_hand.count(pt) as u8;
            let new_count = old_count.saturating_sub(1);
            new_state.dirty_piece.push_hand_change(super::state::HandChange {
                owner: us,
                piece_type: pt,
                old_count,
                new_count,
            });

            // 手駒から減らす
            self.hand[us.index()] = self.hand[us.index()].sub(pt);
            new_state.hand_key = new_state.hand_key.wrapping_sub(zobrist_hand(us, pt));
            // material_value: 打ち駒では手駒→盤上で価値は変化しない

            // 盤上に配置
            self.put_piece(pc, to);
            new_state.board_key ^= zobrist_psq(pc, to);
            self.xor_partial_keys(&mut new_state, pc, to);

            new_state.captured_piece = Piece::NONE;

            // DirtyPiece: 打ち駒（盤上に新しく現れる）
            new_state.dirty_piece.push_piece(ChangedPiece {
                color: us,
                old_piece: Piece::NONE,
                old_sq: None,
                new_piece: pc,
                new_sq: Some(to),
            });
        } else {
            let from = m.from();
            let to = m.to();
            moved_from = Some(from);
            moved_to = to;
            let pc = self.piece_on(from);
            let captured = self.piece_on(to);
            moved_pt = if m.is_promote() {
                pc.piece_type().promote().unwrap()
            } else {
                pc.piece_type()
            };

            // 駒を取る場合
            if captured.is_some() {
                let captured_pt = captured.piece_type().unpromote();
                debug_assert!(
                    captured_pt != PieceType::King,
                    "illegal capture of king at {} by move {} in position {}",
                    to.to_usi(),
                    m.to_usi(),
                    self.to_sfen()
                );
                self.remove_piece(to);
                new_state.board_key ^= zobrist_psq(captured, to);
                self.xor_partial_keys(&mut new_state, captured, to);

                // material_value: 盤上から駒が消える
                material_value -= signed_piece_value(captured);

                // 手駒に追加（成駒は生駒に戻す）※手駒にならない駒種は無視
                if matches!(
                    captured_pt,
                    PieceType::Pawn
                        | PieceType::Lance
                        | PieceType::Knight
                        | PieceType::Silver
                        | PieceType::Gold
                        | PieceType::Bishop
                        | PieceType::Rook
                ) {
                    // DirtyPiece: 手駒の変化（us の captured_pt が 1 増える）
                    let old_hand = self.hand[us.index()];
                    let old_count = old_hand.count(captured_pt) as u8;
                    let new_count = old_count.saturating_add(1);
                    new_state.dirty_piece.push_hand_change(super::state::HandChange {
                        owner: us,
                        piece_type: captured_pt,
                        old_count,
                        new_count,
                    });

                    self.hand[us.index()] = self.hand[us.index()].add(captured_pt);
                    new_state.hand_key =
                        new_state.hand_key.wrapping_add(zobrist_hand(us, captured_pt));

                    material_value += hand_piece_value(us, captured_pt);
                }
            }
            new_state.captured_piece = captured;

            // 駒を移動
            self.remove_piece(from);
            new_state.board_key ^= zobrist_psq(pc, from);
            self.xor_partial_keys(&mut new_state, pc, from);

            let moved_pc = if m.is_promote() {
                pc.promote().unwrap()
            } else {
                pc
            };
            self.put_piece(moved_pc, to);
            new_state.board_key ^= zobrist_psq(moved_pc, to);
            self.xor_partial_keys(&mut new_state, moved_pc, to);

            // 成りによるmaterial差分
            if moved_pc != pc {
                material_value += signed_piece_value(moved_pc) - signed_piece_value(pc);
            }

            // 玉の移動
            if pc.piece_type() == PieceType::King {
                self.king_square[us.index()] = to;
                new_state.dirty_piece.king_moved[us.index()] = true;
            }

            // DirtyPiece: 移動した駒
            new_state.dirty_piece.push_piece(ChangedPiece {
                color: us,
                old_piece: pc,
                old_sq: Some(from),
                new_piece: moved_pc,
                new_sq: Some(to),
            });

            // DirtyPiece: 取った駒（盤上から消える）
            if captured.is_some() {
                new_state.dirty_piece.push_piece(ChangedPiece {
                    color: them,
                    old_piece: captured,
                    old_sq: Some(to),
                    new_piece: Piece::NONE,
                    new_sq: None,
                });
            }
        }
        // 6. 王手情報の更新（diffベース）
        let mut checkers = Bitboard::EMPTY;
        if gives_check {
            let ksq = self.king_square[them.index()];
            // 直接王手
            checkers |=
                self.cur_state().check_squares[moved_pt as usize] & Bitboard::from_square(moved_to);

            // 開き王手（動かした駒が遮断駒だった場合）
            if let Some(from_sq) = moved_from {
                let prev_blockers = self.cur_state().blockers_for_king[them.index()];
                if prev_blockers.contains(from_sq) {
                    if let Some(dir) = crate::bitboard::direct_of(ksq, from_sq) {
                        let ray = crate::bitboard::direct_effect(from_sq, dir, self.occupied());
                        checkers |= ray & self.pieces_c(us);
                    }
                }
            }

            // 捕獲で遮断駒を取り除いた場合の開き王手
            let prev_blockers = self.cur_state().blockers_for_king[them.index()];
            if prev_blockers.contains(moved_to) {
                if let Some(dir) = crate::bitboard::direct_of(ksq, moved_to) {
                    let ray = crate::bitboard::direct_effect(moved_to, dir, self.occupied());
                    checkers |= ray & self.pieces_c(us);
                }
            }
        } else {
            // 安全のため盤面から再計算（gives_checkの取り違えがあっても整合性を保つ）
            checkers = self.attackers_to_c(self.king_square[them.index()], us);
        }
        let is_check = !checkers.is_empty();
        // 4. 連続王手カウンタの更新（YaneuraOu準拠）
        if is_check {
            new_state.continuous_check[us.index()] = prev_continuous[us.index()] + 2;
        } else {
            new_state.continuous_check[us.index()] = 0;
        }
        // 受け手側はリセット
        new_state.continuous_check[them.index()] = 0;

        // 5. 手番交代
        self.side_to_move = them;

        // 6. 王手情報の更新
        new_state.checkers = checkers;

        // 7. 千日手判定に使う手駒スナップショットを保存
        new_state.hand_snapshot = self.hand;
        new_state.material_value = Value::new(material_value);

        // 8. StateInfoの付け替え（previous をぶら下げる）
        new_state.last_move = m;
        self.push_state(new_state);

        // 9. 繰り返し情報の更新
        self.update_repetition_info();

        // 10. pin情報を差分更新（王との直線/斜め上の駒が動いた場合のみ再計算）
        {
            let occ_after = self.occupied();
            let changed_sqs: [Option<Square>; 2] = [moved_from, Some(moved_to)];

            for c in [Color::Black, Color::White] {
                let king_sq_prev = prev_king_sq[c.index()];
                let king_sq_now = self.king_square[c.index()];
                let king_moved = king_sq_prev != king_sq_now;

                let mut needs_recompute = king_moved;
                if !needs_recompute {
                    for sq in changed_sqs.iter().flatten().copied() {
                        if prev_blockers[c.index()].contains(sq)
                            || prev_pinners[c.index()].contains(sq)
                            || crate::bitboard::direct_of(king_sq_now, sq).is_some()
                        {
                            needs_recompute = true;
                            break;
                        }
                    }
                }

                if !needs_recompute {
                    let st = self.cur_state_mut();
                    st.blockers_for_king[c.index()] = prev_blockers[c.index()];
                    st.pinners[c.index()] = prev_pinners[c.index()];
                    continue;
                }

                let (blockers, pinners) =
                    self.compute_blockers_and_pinners(c, occ_after, Bitboard::EMPTY);
                let st = self.cur_state_mut();
                st.blockers_for_king[c.index()] = blockers;
                st.pinners[c.index()] = pinners;
            }
        }

        // 11. 王手マスの更新
        self.update_check_squares();
    }

    /// 指し手を戻す
    pub fn undo_move(&mut self, m: Move) {
        // 1. 手番を戻す
        self.side_to_move = !self.side_to_move;
        self.game_ply -= 1;
        let us = self.side_to_move;
        let captured = self.cur_state().captured_piece;
        let prev_idx = self.cur_state().previous.expect("No previous state for undo");

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
            if captured.is_some() {
                self.put_piece(captured, to);
                // 手駒から除去
                let cap_pt = captured.piece_type().unpromote();
                self.hand[us.index()] = self.hand[us.index()].sub(cap_pt);
            }
        }

        // 3. StateInfoを戻す
        self.state_idx = prev_idx;
    }

    /// null moveを実行
    pub fn do_null_move(&mut self) {
        let mut new_state = self.cur_state().partial_clone();
        new_state.accumulator.reset();
        new_state.dirty_piece.clear();

        new_state.board_key ^= zobrist_side();
        new_state.plies_from_null = 0;
        new_state.captured_piece = Piece::NONE;
        new_state.last_move = Move::NULL;
        new_state.hand_snapshot = self.hand;

        self.side_to_move = !self.side_to_move;

        self.push_state(new_state);

        // null move後は王手されていないはず
        self.cur_state_mut().checkers = Bitboard::EMPTY;

        self.update_blockers_and_pinners();
        self.update_check_squares();
    }

    /// null moveを戻す
    pub fn undo_null_move(&mut self) {
        self.side_to_move = !self.side_to_move;
        let prev_idx = self.cur_state().previous.expect("No previous state for undo_null_move");
        self.state_idx = prev_idx;
    }

    /// 繰り返し情報を更新（最大16手遡り）
    fn update_repetition_info(&mut self) {
        // 初期化
        let side = self.side_to_move;
        let (plies_from_null, board_key, hand_snapshot, prev_idx_opt, cc_side, cc_opp) = {
            let st = self.cur_state();
            (
                st.plies_from_null,
                st.board_key,
                st.hand_snapshot,
                st.previous,
                st.continuous_check[side.index()],
                st.continuous_check[(!side).index()],
            )
        };

        let max_back = plies_from_null.min(16);
        let mut repetition = 0;
        let mut repetition_times = 0;
        let mut repetition_type = RepetitionState::None;

        if max_back >= 4 {
            let mut dist = 2;
            let mut st_idx_opt = prev_idx_opt.and_then(|idx| self.state_stack[idx].previous);

            while dist <= max_back {
                if let Some(st_idx) = st_idx_opt {
                    let stp = &self.state_stack[st_idx];
                    if stp.board_key == board_key {
                        let prev_hand = stp.hand_snapshot[side.index()];
                        let cur_hand = hand_snapshot[side.index()];

                        if cur_hand == prev_hand {
                            let times = stp.repetition_times + 1;
                            repetition_times = times;
                            repetition = if times >= 3 { -dist } else { dist };

                            let mut rep_type = if dist <= cc_side {
                                RepetitionState::Lose
                            } else if dist <= cc_opp {
                                RepetitionState::Win
                            } else {
                                RepetitionState::Draw
                            };

                            if stp.repetition_times > 0 && stp.repetition_type != rep_type {
                                rep_type = RepetitionState::Draw;
                            }

                            repetition_type = rep_type;
                            break;
                        }

                        if cur_hand.is_superior_or_equal(prev_hand) {
                            repetition_type = RepetitionState::Superior;
                            repetition = dist;
                            break;
                        }

                        if prev_hand.is_superior_or_equal(cur_hand) {
                            repetition_type = RepetitionState::Inferior;
                            repetition = dist;
                            break;
                        }
                    }
                    st_idx_opt = stp.previous.and_then(|idx| self.state_stack[idx].previous);
                    dist += 2;
                } else {
                    break;
                }
            }
        }

        let st = self.cur_state_mut();
        st.repetition = repetition;
        st.repetition_times = repetition_times;
        st.repetition_type = repetition_type;
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
            pt.promote().unwrap_or(pt)
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

impl Position {
    /// 1手詰めを検出（該当手があれば返す。なければ Move::NONE）
    pub fn mate_1ply(&mut self) -> Move {
        crate::mate::mate_1ply(self).unwrap_or(Move::NONE)
    }
}

impl Default for Position {
    fn default() -> Self {
        Self::new()
    }
}

impl Position {
    /// 占有を指定してpin駒を再計算（king_color側の玉に対するpin）
    fn pinned_pieces_with_occupancy(
        &self,
        king_color: Color,
        occupied: Bitboard,
        enemy_removed: Bitboard,
    ) -> Bitboard {
        let (blockers, _) = self.compute_blockers_and_pinners(king_color, occupied, enemy_removed);
        blockers & self.pieces_c(king_color)
    }

    /// 占有を指定してpin候補とpinnerを再計算
    fn compute_blockers_and_pinners(
        &self,
        king_color: Color,
        occupied: Bitboard,
        enemy_removed: Bitboard,
    ) -> (Bitboard, Bitboard) {
        let ksq = self.king_square[king_color.index()];
        let enemy = !king_color;

        let lance_bb = self.pieces(enemy, PieceType::Lance) & !enemy_removed;
        let bishop_bb = (self.pieces(enemy, PieceType::Bishop)
            | self.pieces(enemy, PieceType::Horse))
            & !enemy_removed;
        let rook_bb = (self.pieces(enemy, PieceType::Rook) | self.pieces(enemy, PieceType::Dragon))
            & !enemy_removed;

        let snipers = (lance_effect(king_color, ksq, Bitboard::EMPTY) & lance_bb)
            | (bishop_effect(ksq, Bitboard::EMPTY) & bishop_bb)
            | (rook_effect(ksq, Bitboard::EMPTY) & rook_bb);

        let mut blockers = Bitboard::EMPTY;
        let mut pinners = Bitboard::EMPTY;
        for sniper_sq in snipers.iter() {
            let between = crate::bitboard::between_bb(ksq, sniper_sq) & occupied;
            if between.is_empty() || between.more_than_one() {
                continue;
            }

            // blockerが自駒のときのみpin対象
            if (between & self.pieces_c(enemy)).is_empty() {
                blockers |= between;
                pinners.set(sniper_sq);
            } else {
                blockers |= between;
            }
        }

        (blockers, pinners)
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
    fn test_blockers_pinners_incremental_matches_full() {
        // 配置: 先手玉5九, 後手玉1一, 後手飛5六, 先手金5八（玉を遮る）, 先手桂1三（玉筋外）
        let mut pos = Position::new();
        let bk = Square::new(File::File5, Rank::Rank9);
        let wk = Square::new(File::File1, Rank::Rank1);
        let rook = Square::new(File::File5, Rank::Rank6);
        let blocker = Square::new(File::File5, Rank::Rank8);
        let knight = Square::new(File::File1, Rank::Rank3);

        pos.put_piece(Piece::B_KING, bk);
        pos.king_square[Color::Black.index()] = bk;
        pos.put_piece(Piece::W_KING, wk);
        pos.king_square[Color::White.index()] = wk;
        pos.put_piece(Piece::W_ROOK, rook);
        pos.put_piece(Piece::B_GOLD, blocker);
        pos.put_piece(Piece::B_KNIGHT, knight);

        pos.update_blockers_and_pinners();

        let prev_blockers = pos.blockers_for_king(Color::Black);
        let prev_pinners = pos.cur_state().pinners[Color::White.index()];

        // 玉筋とは無関係の桂を動かしてもblockers/pinnersは変わらない
        let mv_offline = Move::new_move(knight, Square::new(File::File1, Rank::Rank2), false);
        pos.do_move(mv_offline, false);
        assert_eq!(pos.blockers_for_king(Color::Black), prev_blockers);
        assert_eq!(pos.cur_state().pinners[Color::White.index()], prev_pinners);

        // 金を筋から外すとblockers/pinnersが更新される（再計算と一致）
        let mv_unblock = Move::new_move(blocker, Square::new(File::File6, Rank::Rank8), false);
        pos.do_move(mv_unblock, false);
        let (blockers_full, pinners_full) =
            pos.compute_blockers_and_pinners(Color::Black, pos.occupied(), Bitboard::EMPTY);
        assert_eq!(pos.blockers_for_king(Color::Black), blockers_full);
        assert_eq!(pos.cur_state().pinners[Color::White.index()], pinners_full);

        // 捕獲で遮断駒を除去した場合の開き王手も検出される
        // 先手の飛車 1一, 後手玉 1九, 先手金 1七（遮断駒）, 後手歩 2七 を1七の金で取って開き王手になるケース
        let mut pos = Position::new();
        let wk = Square::new(File::File1, Rank::Rank9);
        let br = Square::new(File::File1, Rank::Rank1);
        let b_blocker = Square::new(File::File1, Rank::Rank7);
        let w_target = Square::new(File::File2, Rank::Rank7);
        let bk = Square::new(File::File5, Rank::Rank9); // 先手玉はどこでもよい
        pos.put_piece(Piece::W_KING, wk);
        pos.king_square[Color::White.index()] = wk;
        pos.put_piece(Piece::B_KING, bk);
        pos.king_square[Color::Black.index()] = bk;
        pos.put_piece(Piece::B_ROOK, br);
        pos.put_piece(Piece::B_GOLD, b_blocker);
        pos.put_piece(Piece::W_PAWN, w_target);
        pos.side_to_move = Color::Black;
        pos.update_blockers_and_pinners();

        let mv_capture = Move::new_move(b_blocker, w_target, false);
        pos.do_move(mv_capture, false);
        // checkersに飛車が含まれていれば開き王手が検出されている
        assert!(pos.cur_state().checkers.contains(br));

        // 玉を動かした場合も再計算される
        let king_from = pos.king_square(Color::Black);
        let king_to = Square::new(File::File6, Rank::Rank9);
        let king_move = Move::new_move(king_from, king_to, false);
        pos.do_move(king_move, false);
        let (blockers_full, pinners_full) =
            pos.compute_blockers_and_pinners(Color::Black, pos.occupied(), Bitboard::EMPTY);
        assert_eq!(pos.blockers_for_king(Color::Black), blockers_full);
        assert_eq!(pos.cur_state().pinners[Color::White.index()], pinners_full);
    }

    #[test]
    fn test_pieces_by_type_set() {
        let mut pos = Position::new();
        let gold_sq = Square::new(File::File5, Rank::Rank5);
        let pro_sq = Square::new(File::File4, Rank::Rank4);
        let dragon_sq = Square::new(File::File9, Rank::Rank9);

        pos.put_piece(Piece::B_GOLD, gold_sq);
        pos.put_piece(Piece::B_PRO_PAWN, pro_sq);
        pos.put_piece(Piece::W_DRAGON, dragon_sq);

        let gold_like = pos.pieces_c_by_types(Color::Black, PieceTypeSet::golds());
        assert!(gold_like.contains(gold_sq));
        assert!(gold_like.contains(pro_sq));
        assert!(!gold_like.contains(dragon_sq));

        let sliders = pos.pieces_by_types(PieceTypeSet::rook_dragon());
        assert!(sliders.contains(dragon_sq));
        assert!(!sliders.contains(gold_sq));

        let all_black = pos.pieces_c_by_types(Color::Black, PieceTypeSet::ALL);
        assert_eq!(all_black.count(), 2);
    }

    #[test]
    fn test_pinned_pieces_variants() {
        let mut pos = Position::new();
        // 玉と駒配置
        let ksq = Square::new(File::File5, Rank::Rank9);
        let rook_sq = Square::new(File::File5, Rank::Rank1);
        let blocker_sq = Square::new(File::File5, Rank::Rank5);
        pos.put_piece(Piece::B_KING, ksq);
        pos.put_piece(Piece::W_ROOK, rook_sq);
        pos.put_piece(Piece::B_GOLD, blocker_sq);
        pos.king_square[Color::Black.index()] = ksq;
        pos.king_square[Color::White.index()] = Square::new(File::File1, Rank::Rank1);

        // 通常: blockerはpinされている
        let pinned = pos.pinned_pieces_excluding(Color::Black, Square::SQ_11);
        assert!(pinned.contains(blocker_sq));

        // blockerを除去した占有でpinは消える
        let pinned_removed = pos.pinned_pieces_excluding(Color::Black, blocker_sq);
        assert!(pinned_removed.is_empty());

        // rookを取った場合、pinは消える
        let capture_sq = Square::new(File::File5, Rank::Rank2);
        pos.put_piece(Piece::B_PAWN, capture_sq);
        let pinned_after_capture = pos.pinned_pieces_after_move(Color::Black, capture_sq, rook_sq);
        assert!(pinned_after_capture.is_empty());
        // 以降の検証に影響しないよう除去
        pos.remove_piece(capture_sq);

        // pinners配列も更新される
        pos.update_blockers_and_pinners();
        assert!(pos.state().pinners[Color::Black.index()].contains(rook_sq));

        // 敵駒が間にいる場合はpinnerにならない
        let mut pos2 = Position::new();
        pos2.put_piece(Piece::B_KING, ksq);
        pos2.put_piece(Piece::W_ROOK, rook_sq);
        let enemy_blocker = Square::new(File::File5, Rank::Rank4);
        pos2.put_piece(Piece::W_GOLD, enemy_blocker);
        pos2.king_square[Color::Black.index()] = ksq;
        pos2.king_square[Color::White.index()] = Square::new(File::File1, Rank::Rank1);
        pos2.update_blockers_and_pinners();
        assert!(pos2.state().blockers_for_king[Color::Black.index()].contains(enemy_blocker));
        assert!(!pos2.state().pinners[Color::Black.index()].contains(rook_sq));
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

    /// 自己対局中に発生した王手見落としによるpanicを再現し、checkersが正しく更新されることを確認する。
    #[test]
    fn test_checkers_matches_attackers_after_moves() {
        let mut pos = Position::new();
        pos.set_hirate();

        // byoyomi 3000ms の自己対局ログより抽出（最後の timeout は除外）。
        let moves = [
            "7g7f", "4a3b", "1g1f", "5a5b", "4g4f", "3c3d", "6g6f", "1c1d", "5i4h", "9c9d", "4h4g",
            "4c4d", "2h3h", "9a9c", "1i1g", "3a4b", "3h7h", "5c5d", "5g5f", "6c6d", "7h1h", "8b6b",
            "1h5h", "6d6e", "6f6e", "6b6e", "5h6h", "P*6g", "6h4h", "4d4e", "8h2b+", "3b2b",
            "B*7g", "4e4f",
        ];

        for (idx, mv_str) in moves.iter().enumerate() {
            let mv = Move::from_usi(mv_str).unwrap_or_else(|| panic!("invalid move: {mv_str}"));
            let gives_check = pos.gives_check(mv);
            pos.do_move(mv, gives_check);

            let king_sq = pos.king_square(pos.side_to_move());
            let expected_checkers = pos.attackers_to_c(king_sq, !pos.side_to_move());

            assert_eq!(
                pos.checkers(),
                expected_checkers,
                "checkers mismatch at ply {} after move {} in sfen {}",
                idx + 1,
                mv_str,
                pos.to_sfen()
            );
        }
    }

    #[test]
    fn test_do_move_sets_checkers_from_board_state() {
        let mut pos = Position::new();
        // 玉と持ち駒だけの簡単な局面を作り、わざとgives_check=falseで王手の手を指す。
        let b_king = Square::new(File::File5, Rank::Rank9);
        let w_king = Square::new(File::File5, Rank::Rank1);
        pos.put_piece(Piece::B_KING, b_king);
        pos.put_piece(Piece::W_KING, w_king);
        pos.king_square[Color::Black.index()] = b_king;
        pos.king_square[Color::White.index()] = w_king;
        pos.hand[Color::Black.index()] = pos.hand[Color::Black.index()].add(PieceType::Gold);

        let drop_sq = Square::from_usi("4a").unwrap();
        let mv = Move::new_drop(PieceType::Gold, drop_sq);

        // 王手になるにもかかわらず false を渡しても、盤面から正しくチェック状態が計算されることを確認。
        pos.do_move(mv, false);
        let expected_checkers = pos.attackers_to_c(pos.king_square(Color::White), Color::Black);
        assert!(!expected_checkers.is_empty(), "drop should give check");
        assert_eq!(pos.checkers(), expected_checkers);
        assert_eq!(pos.state().continuous_check[Color::Black.index()], 2);
        assert_eq!(pos.state().continuous_check[Color::White.index()], 0);
        assert_eq!(pos.side_to_move(), Color::White);
    }

    /// パニック再現SFENで敵玉取りや自殺手が非合法になることを確認
    #[test]
    fn panic_position_disallows_king_capture() {
        let sfen = "ln2k1+L1+R/2s2s3/p1pl1p3/1+r2p1p1p/9/4B4/5PPPP/4Gg3/2+b2GKNL w S2NPgs7p 107";
        let mut pos = Position::new();
        pos.set_sfen(sfen).unwrap();

        // 白手番で「4h3i」（敵玉取り）は非合法
        let capture_king = Move::from_usi("4h3i").unwrap();
        assert!(!pos.is_legal(capture_king));

        // 黒手番で玉を3h→3iに動かす手（敵の利きに飛び込む）は非合法
        let mut pos_black = Position::new();
        pos_black.set_sfen(sfen).unwrap();
        pos_black.side_to_move = Color::Black;
        let b_king = pos_black.king_square(Color::Black);
        pos_black.remove_piece(b_king);
        let king_from = Square::from_usi("3h").unwrap();
        let king_to = Square::from_usi("3i").unwrap();
        pos_black.put_piece(Piece::B_KING, king_from);
        pos_black.king_square[Color::Black.index()] = king_from;
        pos_black.update_blockers_and_pinners();
        pos_black.update_check_squares();
        let king_move = Move::new_move(king_from, king_to, false);
        assert!(!pos_black.is_legal(king_move));
    }
}
