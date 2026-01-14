//! MovePicker サポートメソッド
//!
//! MovePicker が必要とする Position のメソッドを実装する。

use super::Position;
use crate::bitboard::{
    between_bb, bishop_effect, direct_of, gold_effect, king_effect, knight_effect, lance_effect,
    pawn_effect, ray_effect, rook_effect, silver_effect, Bitboard, Direct,
};
use crate::movegen::{generate_evasions, generate_with_type, ExtMoveBuffer, GenType};
use crate::types::{Color, Move, Piece, PieceType, Square, Value};

impl Position {
    // =========================================================================
    // 指し手の妥当性チェック
    // =========================================================================

    /// pseudo-legal チェック（TT手の妥当性確認用）
    ///
    /// 指し手が現在の局面で pseudo-legal かどうかを確認する。
    /// 完全な合法性（自玉への王手回避など）はチェックしない。
    ///
    /// YaneuraOuの実装を参考に、王手中の不正な手を早期リジェクトする。
    /// 成らない手の制限は行わない（特殊な詰み手順の発見を可能にするため）。
    ///
    /// ## パフォーマンスについて
    ///
    /// この最適化によるNPS改善は誤差範囲内（+0.3%）だった。
    /// これはTT手検証で王手中の不正な手が出現する頻度が低いため。
    /// パフォーマンスよりもコードの正確性向上が主な目的。
    pub fn pseudo_legal(&self, m: Move) -> bool {
        if m.is_none() {
            return false;
        }

        let us = self.side_to_move();
        let to = m.to();

        if m.is_drop() {
            // 駒打ち
            let pt = m.drop_piece_type();

            // 手駒にあるか
            if !self.hand(us).has(pt) {
                return false;
            }

            // 移動先が空きか
            if self.piece_on(to).is_some() {
                return false;
            }

            // 王手中の合駒チェック
            if self.in_check() {
                let checkers = self.checkers();
                let checker_sq = checkers.lsb().unwrap();

                // 両王手なら合駒不可
                if checkers.count() > 1 {
                    return false;
                }

                // 王と王手駒の間に打つ手でなければ不可
                let king_sq = self.king_square(us);
                if !between_bb(checker_sq, king_sq).contains(to) {
                    return false;
                }
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
            let pc = self.piece_on(from);

            // 移動元に自分の駒があるか
            if pc.is_none() || pc.color() != us {
                return false;
            }

            // 駒の動きとして正しいか
            let pt = pc.piece_type();
            let occupied = self.occupied();

            // 成りフラグの検証
            // TTからの手が現在の局面の駒種と一致しない場合のパニックを防ぐ
            if m.is_promote() {
                // 成りフラグが立っている場合、駒種が成れるかチェック
                if !pt.can_promote() {
                    return false;
                }
                // 成れる段（敵陣 = from or to が敵陣）かどうかチェック
                let in_enemy_zone = if us == Color::Black {
                    from.rank().index() <= 2 || to.rank().index() <= 2
                } else {
                    from.rank().index() >= 6 || to.rank().index() >= 6
                };
                if !in_enemy_zone {
                    return false;
                }
            }

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

            if !attacks.contains(to) {
                return false;
            }

            // 移動先に自分の駒がないか
            let to_pc = self.piece_on(to);
            if to_pc.is_some() && to_pc.color() == us {
                return false;
            }

            // 成りの場合、成れない駒は成れない
            if m.is_promotion() && !pt.can_promote() {
                return false;
            }

            // 【参考実装】成らない手の制限（YaneuraOu の generate_all_legal_moves = false 相当）
            // 特殊な詰み手順（歩不成での打ち歩詰め回避、角不成での利き調整など）の
            // 発見を可能にするため、本実装では有効化しない。
            // NPS改善も誤差範囲内だったため、制限しないことによるデメリットはない。
            //
            // if !m.is_promotion() {
            //     match pt {
            //         PieceType::Pawn => {
            //             // 歩の不成: 敵陣での不成を禁止
            //             if is_enemy_field(us, to) {
            //                 return false;
            //             }
            //         }
            //         PieceType::Lance => {
            //             // 香の不成: 1-2段目（先手）/ 8-9段目（後手）への不成を禁止
            //             if is_deep_enemy_field(us, to) {
            //                 return false;
            //             }
            //         }
            //         PieceType::Bishop | PieceType::Rook => {
            //             // 大駒の不成: 敵陣に関わる移動での不成を禁止
            //             if is_enemy_field(us, from) || is_enemy_field(us, to) {
            //                 return false;
            //             }
            //         }
            //         _ => {}
            //     }
            // }

            // 王手中の駒移動チェック
            let checkers = self.checkers();
            if !checkers.is_empty() {
                // 玉以外を動かす場合
                if pt != PieceType::King {
                    // 両王手なら玉を動かす以外は不可
                    if checkers.count() > 1 {
                        return false;
                    }

                    // 王手を遮断しているか、王手駒を取る手でなければ不可
                    let checker_sq = checkers.lsb().unwrap();
                    let king_sq = self.king_square(us);
                    let valid_targets = between_bb(checker_sq, king_sq) | checkers;
                    if !valid_targets.contains(to) {
                        return false;
                    }
                }
            }

            true
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

    /// capture_stage: 捕獲手かどうか（ProbCut等の判定用）
    #[inline]
    pub fn capture_stage(&self, m: Move) -> bool {
        !m.is_drop() && self.piece_on(m.to()).is_some()
    }

    /// pseudo-legal判定（生成モード指定版）
    ///
    /// 互換性のため `generate_all_legal_moves` パラメータを受け取るが、
    /// 成らない手の制限は行わないため、常に `pseudo_legal()` と同じ動作をする。
    #[inline]
    pub fn pseudo_legal_with_all(&self, m: Move, _generate_all_legal_moves: bool) -> bool {
        self.pseudo_legal(m)
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

    /// 指し手で動いた後の駒（成り後・打ち駒を含む）を取得
    #[inline]
    pub fn moved_piece_after_move(&self, m: Move) -> Piece {
        debug_assert!(m.has_piece_info(), "Move must carry piece info");
        m.moved_piece_after()
    }

    /// 取る手または成る手かどうか
    #[inline]
    pub fn is_capture_or_promotion(&self, m: Move) -> bool {
        self.is_capture(m) || m.is_promotion()
    }

    /// 取る手または歩成りの手かどうか（ProbCut用）
    /// YaneuraOu: capture_or_pawn_promotion
    #[inline]
    pub fn capture_or_pawn_promotion(&self, m: Move) -> bool {
        self.is_capture(m)
            || (m.is_promotion() && self.moved_piece(m).piece_type() == PieceType::Pawn)
    }

    // =========================================================================
    // 歩の陣形インデックス
    // =========================================================================

    /// PawnHistory 用のインデックスを計算
    ///
    /// 歩の配置に基づくハッシュ値からインデックスを計算する。
    pub fn pawn_history_index(&self) -> usize {
        (self.pawn_key() as usize) & (crate::search::PAWN_HISTORY_SIZE - 1)
    }

    // =========================================================================
    // 指し手生成（MovePicker用）
    // =========================================================================

    /// 捕獲手を生成（ExtMoveBufferに直接書き込み）
    ///
    /// generate関数がExtMoveBufferに直接書き込むため、中間バッファ不要。
    pub fn generate_captures(&self, moves: &mut ExtMoveBuffer) -> usize {
        if self.in_check() {
            // 王手回避手を生成してから捕獲手のみフィルタ
            generate_evasions(self, moves);
            moves.retain(|m| self.is_capture(m));
        } else {
            generate_with_type(self, GenType::CapturesProPlus, moves, None);
        }
        moves.len()
    }

    /// 静かな手を生成（ExtMoveBufferに直接書き込み、既存の要素の後に追加）
    ///
    /// generate関数がExtMoveBufferに直接書き込むため、中間バッファ不要。
    /// offset は互換性のために維持するが、moves.len() と等しい必要がある。
    pub fn generate_quiets(&self, moves: &mut ExtMoveBuffer, offset: usize) -> usize {
        if self.in_check() {
            return 0;
        }

        debug_assert_eq!(
            offset,
            moves.len(),
            "offset should equal buffer length: offset={offset}, len={}",
            moves.len()
        );

        let start_len = moves.len();
        // YaneuraOu標準のQUIETS相当: 成りも含む静かな手を生成する
        generate_with_type(self, GenType::Quiets, moves, None);
        moves.len() - start_len
    }

    /// 回避手を生成（ExtMoveBufferに直接書き込み）
    ///
    /// generate関数がExtMoveBufferに直接書き込むため、中間バッファ不要。
    pub fn generate_evasions_ext(&self, moves: &mut ExtMoveBuffer) -> usize {
        debug_assert!(self.in_check());
        generate_with_type(self, GenType::Evasions, moves, None);
        moves.len()
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

        debug_assert!(
            self.piece_on(from).is_some(),
            "see_ge_detailed called with empty from square"
        );

        // 初期攻撃者集合（occupiedに依存）
        let mut attackers = self.attackers_to_occ(to, occupied) & occupied;

        loop {
            // 次に to に利く最も価値の低い駒を探す
            let our_attackers = attackers & self.pieces_c(stm);

            if our_attackers.is_empty() {
                // 取り返す駒がない → 現在の手番の負け
                break;
            }

            // 最も価値の低い駒を選択
            let (attacker_sq, attacker_value) =
                self.least_valuable_attacker(our_attackers, stm, to, occupied);

            // 駒を取り除く
            let attacker_bb = Bitboard::from_square(attacker_sq);
            attackers ^= attacker_bb;
            occupied ^= attacker_bb;

            // attacker_sq が遮っていたラインの背後の利きを追加する（やねうら王 SEE と同様）
            if let Some(dir) = direct_of(to, attacker_sq) {
                let ray = ray_effect(dir, to, occupied);
                let extras = match dir {
                    Direct::RU | Direct::RD | Direct::LU | Direct::LD => {
                        ray & (self.pieces_pt(PieceType::Bishop) | self.pieces_pt(PieceType::Horse))
                    }
                    Direct::U => {
                        let rookers =
                            self.pieces_pt(PieceType::Rook) | self.pieces_pt(PieceType::Dragon);
                        let lance = self.pieces(Color::White, PieceType::Lance);
                        ray & (rookers | lance)
                    }
                    Direct::D => {
                        let rookers =
                            self.pieces_pt(PieceType::Rook) | self.pieces_pt(PieceType::Dragon);
                        let lance = self.pieces(Color::Black, PieceType::Lance);
                        ray & (rookers | lance)
                    }
                    Direct::L | Direct::R => {
                        ray & (self.pieces_pt(PieceType::Rook) | self.pieces_pt(PieceType::Dragon))
                    }
                };
                attackers |= extras & occupied;
            }

            // バランスを更新
            balance = -balance - 1 - victim_value;
            victim_value = attacker_value;

            if balance >= 0 {
                // pinされた駒でも、相手が玉なら勝ち確定
                if attacker_value == see_piece_value(PieceType::King) {
                    // 相手に取り返す駒があるかチェック
                    let their_attackers = attackers & self.pieces_c(!stm);
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

        unreachable!(
            "least_valuable_attacker should always find an attacker when attackers is non-empty"
        );
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

// 【参考実装】成らない手の制限用ヘルパー関数
// pseudo_legal 内のコメントアウトされた実装で使用する。
//
// /// 敵陣かどうか（1-3段目/7-9段目）
// #[inline]
// fn is_enemy_field(us: Color, sq: Square) -> bool {
//     match us {
//         Color::Black => sq.rank().index() < 3,
//         Color::White => sq.rank().index() > 5,
//     }
// }
//
// /// 深い敵陣かどうか（1-2段目/8-9段目）- 香の不成禁止用
// #[inline]
// fn is_deep_enemy_field(us: Color, sq: Square) -> bool {
//     match us {
//         Color::Black => sq.rank().index() < 2,
//         Color::White => sq.rank().index() > 6,
//     }
// }

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

    /// 成りフラグ検証テスト: 成れない駒（金）に成りフラグが立っている手は pseudo_legal で弾く
    #[test]
    fn test_pseudo_legal_rejects_invalid_promote_on_gold() {
        let mut pos = Position::new();
        let from = Square::new(File::File5, Rank::Rank9);
        let to = Square::new(File::File5, Rank::Rank8);
        let king_sq = Square::new(File::File1, Rank::Rank9);
        let enemy_king = Square::new(File::File1, Rank::Rank1);

        pos.put_piece(Piece::B_GOLD, from);
        pos.put_piece(Piece::B_KING, king_sq);
        pos.put_piece(Piece::W_KING, enemy_king);
        pos.king_square[Color::Black.index()] = king_sq;
        pos.king_square[Color::White.index()] = enemy_king;

        // 金に成りフラグを付けた不正な手
        let invalid_promote = Move::new_move(from, to, true);
        assert!(!pos.pseudo_legal(invalid_promote), "Gold with promote flag should be rejected");

        // 金の通常移動は許可
        let valid_move = Move::new_move(from, to, false);
        assert!(pos.pseudo_legal(valid_move), "Gold normal move should be allowed");
    }

    /// 成りフラグ検証テスト: 既に成駒（龍）に成りフラグが立っている手は pseudo_legal で弾く
    #[test]
    fn test_pseudo_legal_rejects_invalid_promote_on_promoted_piece() {
        let mut pos = Position::new();
        let from = Square::new(File::File5, Rank::Rank5);
        let to = Square::new(File::File5, Rank::Rank4);
        let king_sq = Square::new(File::File1, Rank::Rank9);
        let enemy_king = Square::new(File::File1, Rank::Rank1);

        pos.put_piece(Piece::B_DRAGON, from);
        pos.put_piece(Piece::B_KING, king_sq);
        pos.put_piece(Piece::W_KING, enemy_king);
        pos.king_square[Color::Black.index()] = king_sq;
        pos.king_square[Color::White.index()] = enemy_king;

        // 龍に成りフラグを付けた不正な手
        let invalid_promote = Move::new_move(from, to, true);
        assert!(
            !pos.pseudo_legal(invalid_promote),
            "Dragon with promote flag should be rejected"
        );
    }

    /// 成りフラグ検証テスト: 敵陣外での成りは pseudo_legal で弾く
    #[test]
    fn test_pseudo_legal_rejects_promote_outside_enemy_zone() {
        let mut pos = Position::new();
        // 先手の銀を5五に置く（敵陣外）
        let from = Square::new(File::File5, Rank::Rank5);
        let to = Square::new(File::File5, Rank::Rank4); // 移動先も敵陣外
        let king_sq = Square::new(File::File1, Rank::Rank9);
        let enemy_king = Square::new(File::File1, Rank::Rank1);

        pos.put_piece(Piece::B_SILVER, from);
        pos.put_piece(Piece::B_KING, king_sq);
        pos.put_piece(Piece::W_KING, enemy_king);
        pos.king_square[Color::Black.index()] = king_sq;
        pos.king_square[Color::White.index()] = enemy_king;

        // 敵陣外での成りは不正
        let invalid_promote = Move::new_move(from, to, true);
        assert!(
            !pos.pseudo_legal(invalid_promote),
            "Promote outside enemy zone should be rejected"
        );
    }

    /// 成りフラグ検証テスト: 敵陣内での成りは許可
    #[test]
    fn test_pseudo_legal_allows_promote_in_enemy_zone() {
        let mut pos = Position::new();
        // 先手の銀を3四に置く（敵陣外だが移動先が敵陣内）
        let from = Square::new(File::File3, Rank::Rank4);
        let to = Square::new(File::File3, Rank::Rank3); // 移動先が敵陣
        let king_sq = Square::new(File::File1, Rank::Rank9);
        let enemy_king = Square::new(File::File1, Rank::Rank1);

        pos.put_piece(Piece::B_SILVER, from);
        pos.put_piece(Piece::B_KING, king_sq);
        pos.put_piece(Piece::W_KING, enemy_king);
        pos.king_square[Color::Black.index()] = king_sq;
        pos.king_square[Color::White.index()] = enemy_king;

        // 敵陣内への成りは正当
        let valid_promote = Move::new_move(from, to, true);
        assert!(pos.pseudo_legal(valid_promote), "Promote into enemy zone should be allowed");
    }
}
