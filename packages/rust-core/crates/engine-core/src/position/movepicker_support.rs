//! MovePicker サポートメソッド
//!
//! MovePicker が必要とする Position のメソッドを実装する。

use super::Position;
use crate::bitboard::{
    bishop_effect, gold_effect, king_effect, knight_effect, lance_effect, pawn_effect, rook_effect,
    silver_effect, Bitboard,
};
use crate::movegen::{generate_evasions, generate_non_evasions, ExtMove, MAX_MOVES};
use crate::types::{Color, Move, Piece, PieceType, Square, Value};

impl Position {
    // =========================================================================
    // 指し手の妥当性チェック
    // =========================================================================

    /// pseudo-legal チェック（TT手の妥当性確認用）
    ///
    /// 指し手が現在の局面で pseudo-legal かどうかを確認する。
    /// 完全な合法性（自玉への王手回避など）はチェックしない。
    pub fn pseudo_legal(&self, m: Move) -> bool {
        if m.is_none() {
            return false;
        }

        let us = self.side_to_move();

        if m.is_drop() {
            // 駒打ち
            let pt = m.drop_piece_type();
            let to = m.to();

            // 手駒にあるか
            if !self.hand(us).has(pt) {
                return false;
            }

            // 移動先が空きか
            if self.piece_on(to).is_some() {
                return false;
            }

            // 二歩チェック（歩の場合）
            if pt == PieceType::Pawn {
                let file_mask = crate::bitboard::FILE_BB[to.file().index()];
                if !(self.pieces(us, PieceType::Pawn) & file_mask).is_empty() {
                    return false;
                }
            }

            true
        } else {
            // 駒移動
            let from = m.from();
            let to = m.to();
            let pc = self.piece_on(from);

            // 移動元に自分の駒があるか
            if pc.is_none() || pc.color() != us {
                return false;
            }

            // 移動先に自分の駒がないか
            let to_pc = self.piece_on(to);
            if to_pc.is_some() && to_pc.color() == us {
                return false;
            }

            // 駒の動きとして正しいか（簡易チェック）
            // 完全なチェックは重いので、基本的な駒の動きのみ確認
            let pt = pc.piece_type();
            let occupied = self.occupied();

            let attacks = match pt {
                PieceType::Pawn => pawn_effect(us, from),
                PieceType::Lance => lance_effect(us, from, occupied),
                PieceType::Knight => knight_effect(us, from),
                PieceType::Silver => silver_effect(us, from),
                PieceType::Gold
                | PieceType::ProPawn
                | PieceType::ProLance
                | PieceType::ProKnight
                | PieceType::ProSilver => gold_effect(us, from),
                PieceType::Bishop => bishop_effect(from, occupied),
                PieceType::Rook => rook_effect(from, occupied),
                PieceType::Horse => bishop_effect(from, occupied) | king_effect(from),
                PieceType::Dragon => rook_effect(from, occupied) | king_effect(from),
                PieceType::King => king_effect(from),
            };

            attacks.contains(to)
        }
    }

    // =========================================================================
    // 指し手に関する情報取得
    // =========================================================================

    /// 指し手で動く駒を取得
    #[inline]
    pub fn moved_piece(&self, m: Move) -> Piece {
        if m.is_drop() {
            // 駒打ちの場合は、手番と駒種から駒を構築
            Piece::make(self.side_to_move(), m.drop_piece_type())
        } else {
            self.piece_on(m.from())
        }
    }

    /// 取る手かどうか
    #[inline]
    pub fn is_capture(&self, m: Move) -> bool {
        if m.is_drop() {
            false
        } else {
            self.piece_on(m.to()).is_some()
        }
    }

    /// 取る手または成る手かどうか
    #[inline]
    pub fn is_capture_or_promotion(&self, m: Move) -> bool {
        self.is_capture(m) || m.is_promotion()
    }

    // =========================================================================
    // 歩の陣形インデックス
    // =========================================================================

    /// PawnHistory 用のインデックスを計算
    ///
    /// 歩の配置に基づくハッシュ値からインデックスを計算する。
    pub fn pawn_history_index(&self) -> usize {
        // 歩のBitboardからシンプルなハッシュを計算
        let b_pawns = self.pieces(Color::Black, PieceType::Pawn);
        let w_pawns = self.pieces(Color::White, PieceType::Pawn);

        let hash = b_pawns.as_u128() ^ (w_pawns.as_u128().rotate_left(7));
        (hash as usize) & (crate::search::PAWN_HISTORY_SIZE - 1)
    }

    // =========================================================================
    // 指し手生成（MovePicker用）
    // =========================================================================

    /// 捕獲手を生成（ExtMove配列に直接書き込み）
    pub fn generate_captures(&self, moves: &mut [ExtMove; MAX_MOVES]) -> usize {
        let mut buffer = [Move::NONE; MAX_MOVES];
        let count = if self.in_check() {
            generate_evasions(self, &mut buffer)
        } else {
            generate_non_evasions(self, &mut buffer)
        };

        // 捕獲手のみをフィルタ
        let mut capture_count = 0;
        for m in buffer.iter().take(count) {
            if self.is_capture(*m) {
                moves[capture_count] = ExtMove::new(*m, 0);
                capture_count += 1;
            }
        }
        capture_count
    }

    /// 静かな手を生成（ExtMove配列に直接書き込み）
    pub fn generate_quiets(&self, moves: &mut [ExtMove]) -> usize {
        let mut buffer = [Move::NONE; MAX_MOVES];
        let count = if self.in_check() {
            // 王手回避中は静かな手を別途生成しない
            return 0;
        } else {
            generate_non_evasions(self, &mut buffer)
        };

        // 静かな手（非捕獲手）のみをフィルタ
        let mut quiet_count = 0;
        for m in buffer.iter().take(count) {
            if !self.is_capture(*m) && quiet_count < moves.len() {
                moves[quiet_count] = ExtMove::new(*m, 0);
                quiet_count += 1;
            }
        }
        quiet_count
    }

    /// 回避手を生成（ExtMove配列に直接書き込み）
    pub fn generate_evasions_ext(&self, moves: &mut [ExtMove; MAX_MOVES]) -> usize {
        debug_assert!(self.in_check());

        let mut buffer = [Move::NONE; MAX_MOVES];
        let count = generate_evasions(self, &mut buffer);

        for i in 0..count {
            moves[i] = ExtMove::new(buffer[i], 0);
        }
        count
    }

    // =========================================================================
    // SEE (Static Exchange Evaluation)
    // =========================================================================

    /// SEE >= threshold かどうかを判定
    ///
    /// 指し手の静的駒交換評価が閾値以上かどうかを高速に判定する。
    pub fn see_ge(&self, m: Move, threshold: Value) -> bool {
        if m.is_drop() {
            // 駒打ちは常に >= 0
            return threshold.raw() <= 0;
        }

        let from = m.from();
        let to = m.to();

        // 取られる駒の価値
        let captured_value = if self.piece_on(to).is_some() {
            see_piece_value(self.piece_on(to).piece_type())
        } else {
            0
        };

        // 成りのボーナス
        let promotion_bonus = if m.is_promotion() {
            let pt = self.piece_on(from).piece_type();
            see_piece_value(pt.promote().unwrap_or(pt)) - see_piece_value(pt)
        } else {
            0
        };

        // 最初の交換後のバランス
        let mut balance = captured_value + promotion_bonus - threshold.raw();

        // 既にマイナスなら失敗
        if balance < 0 {
            return false;
        }

        // 次に取られる駒の価値
        let next_victim = if m.is_promotion() {
            let pt = self.piece_on(from).piece_type();
            see_piece_value(pt.promote().unwrap_or(pt))
        } else {
            see_piece_value(self.piece_on(from).piece_type())
        };

        // 駒を取られても閾値を超えるか
        balance -= next_victim;

        if balance >= 0 {
            return true;
        }

        // 詳細なSEE計算
        self.see_ge_detailed(to, from, balance, next_victim)
    }

    /// 詳細なSEE計算（再帰的な駒交換をシミュレート）
    fn see_ge_detailed(
        &self,
        to: Square,
        from: Square,
        mut balance: i32,
        mut victim_value: i32,
    ) -> bool {
        // 移動元と移動先の両方を占有から外す（x-ray攻撃を正しく検出するため）
        let mut occupied =
            self.occupied() ^ Bitboard::from_square(from) ^ Bitboard::from_square(to);
        let mut stm = !self.side_to_move(); // 相手の手番から開始

        loop {
            // 次に to に利く最も価値の低い駒を探す
            let attackers = self.attackers_to_occ(to, occupied) & occupied;
            let our_attackers = attackers & self.pieces_c(stm);

            if our_attackers.is_empty() {
                // 取り返す駒がない → 現在の手番の負け
                break;
            }

            // 最も価値の低い駒を選択
            let (attacker_sq, attacker_value) =
                self.least_valuable_attacker(our_attackers, stm, to, occupied);

            // 駒を取り除く
            occupied ^= Bitboard::from_square(attacker_sq);

            // バランスを更新
            balance = -balance - 1 - victim_value;
            victim_value = attacker_value;

            if balance >= 0 {
                // pinされた駒でも、相手が玉なら勝ち確定
                if attacker_value == see_piece_value(PieceType::King) {
                    // 相手に取り返す駒があるかチェック
                    let their_attackers =
                        self.attackers_to_occ(to, occupied) & occupied & self.pieces_c(!stm);
                    if !their_attackers.is_empty() {
                        // 相手に取り返す駒がある場合は、バランスを反転
                        stm = !stm;
                        continue;
                    }
                }
                break;
            }

            stm = !stm;
        }

        // 最後に手番を持っていた側が勝ち
        stm != self.side_to_move()
    }

    /// 最も価値の低い攻撃駒を探す
    fn least_valuable_attacker(
        &self,
        attackers: Bitboard,
        stm: Color,
        to: Square,
        _occupied: Bitboard,
    ) -> (Square, i32) {
        // 価値の低い順にチェック
        for pt in [
            PieceType::Pawn,
            PieceType::Lance,
            PieceType::Knight,
            PieceType::ProPawn,
            PieceType::ProLance,
            PieceType::ProKnight,
            PieceType::Silver,
            PieceType::ProSilver,
            PieceType::Gold,
            PieceType::Bishop,
            PieceType::Rook,
            PieceType::Horse,
            PieceType::Dragon,
            PieceType::King,
        ] {
            let bb = attackers & self.pieces(stm, pt);
            if !bb.is_empty() {
                let sq = bb.lsb().unwrap();

                // 成りの可能性を考慮した価値
                let value = if can_promote_on(stm, sq, to) && pt.can_promote() {
                    see_piece_value(pt.promote().unwrap())
                } else {
                    see_piece_value(pt)
                };

                return (sq, value);
            }
        }

        // ここには来ないはず
        (Square::SQ_11, 0)
    }
}

// =============================================================================
// ヘルパー関数
// =============================================================================

/// SEE用の駒価値
fn see_piece_value(pt: PieceType) -> i32 {
    use PieceType::*;
    match pt {
        Pawn => 90,
        Lance => 315,
        Knight => 405,
        Silver => 495,
        Gold | ProPawn | ProLance | ProKnight | ProSilver => 540,
        Bishop => 855,
        Rook => 990,
        Horse => 1089,
        Dragon => 1224,
        King => 15000,
    }
}

/// 成れるマスかどうか
fn can_promote_on(us: Color, from: Square, to: Square) -> bool {
    match us {
        Color::Black => to.rank().index() < 3 || from.rank().index() < 3,
        Color::White => to.rank().index() > 5 || from.rank().index() > 5,
    }
}

// =============================================================================
// テスト
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{File, Rank};

    #[test]
    fn test_moved_piece() {
        let mut pos = Position::new();
        pos.set_hirate();

        // 7六歩
        let m = Move::from_usi("7g7f").unwrap();
        let pc = pos.moved_piece(m);
        assert_eq!(pc, Piece::B_PAWN);

        // 駒打ち
        let drop = Move::new_drop(PieceType::Pawn, Square::new(File::File5, Rank::Rank5));
        // 手駒に歩を追加してテスト
        pos.hand[Color::Black.index()] = pos.hand[Color::Black.index()].add(PieceType::Pawn);
        let pc_drop = pos.moved_piece(drop);
        assert_eq!(pc_drop, Piece::B_PAWN);
    }

    #[test]
    fn test_is_capture() {
        let mut pos = Position::new();
        let sq76 = Square::new(File::File7, Rank::Rank6);
        let sq75 = Square::new(File::File7, Rank::Rank5);
        let sq77 = Square::new(File::File7, Rank::Rank7);
        let sq59 = Square::new(File::File5, Rank::Rank9);
        let sq51 = Square::new(File::File5, Rank::Rank1);

        pos.put_piece(Piece::B_PAWN, sq77);
        pos.put_piece(Piece::W_PAWN, sq75);
        pos.put_piece(Piece::B_KING, sq59);
        pos.put_piece(Piece::W_KING, sq51);
        pos.king_square[Color::Black.index()] = sq59;
        pos.king_square[Color::White.index()] = sq51;

        // 7六歩（取らない）
        let m1 = Move::new_move(sq77, sq76, false);
        assert!(!pos.is_capture(m1));

        // 配置を変更
        pos.board[sq77.index()] = Piece::NONE;
        pos.put_piece(Piece::B_PAWN, sq76);

        // 7五歩（取る）
        let m2 = Move::new_move(sq76, sq75, false);
        assert!(pos.is_capture(m2));

        // 駒打ち
        pos.hand[Color::Black.index()] = pos.hand[Color::Black.index()].add(PieceType::Gold);
        let drop = Move::new_drop(PieceType::Gold, Square::new(File::File5, Rank::Rank5));
        assert!(!pos.is_capture(drop));
    }

    #[test]
    fn test_pseudo_legal_basic() {
        let mut pos = Position::new();
        pos.set_hirate();

        // 7六歩 - 合法
        let m1 = Move::from_usi("7g7f").unwrap();
        assert!(pos.pseudo_legal(m1));

        // 7五歩 - 2マス進む（違法）
        let m2 = Move::from_usi("7g7e").unwrap();
        assert!(!pos.pseudo_legal(m2));

        // 空マスから動く
        let sq55 = Square::new(File::File5, Rank::Rank5);
        let sq54 = Square::new(File::File5, Rank::Rank4);
        let m3 = Move::new_move(sq55, sq54, false);
        assert!(!pos.pseudo_legal(m3));
    }

    #[test]
    fn test_see_ge_simple_capture() {
        let mut pos = Position::new();
        let sq55 = Square::new(File::File5, Rank::Rank5);
        let sq54 = Square::new(File::File5, Rank::Rank4);
        let sq59 = Square::new(File::File5, Rank::Rank9);
        let sq51 = Square::new(File::File5, Rank::Rank1);

        // 5五に先手歩、5四に後手金
        pos.put_piece(Piece::B_PAWN, sq55);
        pos.put_piece(Piece::W_GOLD, sq54);
        pos.put_piece(Piece::B_KING, sq59);
        pos.put_piece(Piece::W_KING, sq51);
        pos.king_square[Color::Black.index()] = sq59;
        pos.king_square[Color::White.index()] = sq51;

        // 5四歩（金を取る）→ 金の価値を得る
        let m = Move::new_move(sq55, sq54, false);
        assert!(pos.see_ge(m, Value::new(0)));
        assert!(pos.see_ge(m, Value::new(400))); // 金(540) - 歩(90) = 450 > 400
    }

    #[test]
    fn test_pawn_history_index() {
        let mut pos = Position::new();
        pos.set_hirate();

        let idx = pos.pawn_history_index();
        assert!(idx < crate::search::PAWN_HISTORY_SIZE);
    }

    /// X-ray攻撃のテスト: to を占有から外さないと誤って得と判定されるケース
    ///
    /// 配置（先手番）:
    /// - 5四: 先手歩（from）
    /// - 5五: 後手歩（to）
    /// - 5八: 後手飛
    ///
    /// 5四歩で5五の歩を取ると、後手飛のX-rayが通り交換は損。
    /// to を占有から外さない旧実装では飛車の利きが見えず、SEE が誤って true になる。
    #[test]
    fn test_see_xray_attack() {
        let mut pos = Position::new();
        let from = Square::new(File::File5, Rank::Rank4);
        let to = Square::new(File::File5, Rank::Rank5);
        let rook_sq = Square::new(File::File5, Rank::Rank8);
        let b_king = Square::new(File::File1, Rank::Rank9);
        let w_king = Square::new(File::File9, Rank::Rank1);

        pos.put_piece(Piece::B_PAWN, from);
        pos.put_piece(Piece::W_PAWN, to);
        pos.put_piece(Piece::W_ROOK, rook_sq);
        pos.put_piece(Piece::B_KING, b_king);
        pos.put_piece(Piece::W_KING, w_king);
        pos.king_square[Color::Black.index()] = b_king;
        pos.king_square[Color::White.index()] = w_king;
        pos.side_to_move = Color::Black;

        let m = Move::new_move(from, to, false);

        assert!(!pos.see_ge(m, Value::new(80)), "X-ray rook should make the capture unfavorable");
    }

    /// 斜めラインのX-ray攻撃テスト（角）
    #[test]
    fn test_see_xray_attack_diagonal() {
        let mut pos = Position::new();
        let from = Square::new(File::File3, Rank::Rank3);
        let to = Square::new(File::File4, Rank::Rank4);
        let bishop_sq = Square::new(File::File7, Rank::Rank7);
        let b_king = Square::new(File::File1, Rank::Rank9);
        let w_king = Square::new(File::File9, Rank::Rank1);

        pos.put_piece(Piece::B_PAWN, from);
        pos.put_piece(Piece::W_PAWN, to);
        pos.put_piece(Piece::W_BISHOP, bishop_sq);
        pos.put_piece(Piece::B_KING, b_king);
        pos.put_piece(Piece::W_KING, w_king);
        pos.king_square[Color::Black.index()] = b_king;
        pos.king_square[Color::White.index()] = w_king;
        pos.side_to_move = Color::Black;

        let m = Move::new_move(from, to, false);

        assert!(
            !pos.see_ge(m, Value::new(80)),
            "Diagonal x-ray should make the capture unfavorable"
        );
    }
}
