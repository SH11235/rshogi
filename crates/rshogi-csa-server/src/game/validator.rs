//! 合法手・千日手・打ち歩詰・連続王手千日手・入玉宣言の判定。
//!
//! `rshogi-core` の `Position` を入力として受け取り、CSA トークンを内部 `Move` に
//! 変換しつつ合法性を検証する。Validator 単体で完結し、`EnteringKingRule`
//! 切替（24 点法／27 点法／トライルール等）も同じ API から受け付けられるよう
//! 構成している。

use rshogi_core::movegen::{MoveList, generate_legal_all};
use rshogi_core::position::Position;
use rshogi_core::types::{
    Color as CoreColor, EnteringKingRule, File, Move, PieceType, Rank, RepetitionState, Square,
};

use crate::types::{Color, CsaMoveToken};

/// `validate_move` が返す違反種別。
///
/// `GameRoom` はこれを [`crate::game::result::IllegalReason`] にマップし、
/// 関係者へ `#ILLEGAL_MOVE` を通知する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Violation {
    /// CSA トークンの構文不正（駒種コード不明・桁数不足など）。
    Malformed(String),
    /// 手番側のプレフィックスが現在の手番と一致しない。
    WrongTurn {
        /// クライアントが指してきた手番。
        got: Color,
        /// サーバーが期待する手番。
        expected: Color,
    },
    /// 手番側に該当の手駒がない（駒打ちで在庫不足）。
    NoPieceInHand,
    /// 二歩（同じ筋に既に手番側の歩が存在する）。
    DoublePawn,
    /// 打ち歩詰（敵玉頭への歩打ちで詰ませる手）。
    Uchifuzume,
    /// 上記以外の非合法手（pin 違反、不可能な動き、自玉自殺手など）。
    Illegal,
}

/// `classify_repetition` の判定結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepetitionVerdict {
    /// 千日手なし。
    None,
    /// 通常の千日手（4 回出現で引き分け）。
    Sennichite,
    /// 連続王手の千日手で手番側が負け（連続王手していた側）。
    OuteSennichiteLose,
    /// 連続王手の千日手で手番側が勝ち（連続王手されていた側）。
    OuteSennichiteWin,
}

/// `evaluate_kachi` の判定結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KachiOutcome {
    /// 入玉宣言が成立した（24/27 点法の必要点を満たしている）。
    Accepted,
    /// 不成立（条件を満たしていない）。`%KACHI` 宣言は反則負けとして扱う。
    Rejected,
}

/// 合法性・千日手・入玉宣言を判定するサービス。
///
/// `entering_king_rule` で `%KACHI` 判定方式を切替可能にしている（既定は
/// CSA 24 点法 = `Point24`。27 点法やトライルールも選択可能）。
#[derive(Debug, Clone, Copy)]
pub struct Validator {
    entering_king_rule: EnteringKingRule,
}

impl Validator {
    /// 新しい Validator を生成する。`%KACHI` 判定で使う入玉ルールを指定する。
    pub fn new(entering_king_rule: EnteringKingRule) -> Self {
        Self { entering_king_rule }
    }

    /// 入玉ルールを取得する。
    pub fn entering_king_rule(&self) -> EnteringKingRule {
        self.entering_king_rule
    }

    /// CSA 手トークン（例: `+7776FU`）を `pos` の手番に対して妥当か判定し、
    /// 内部 `Move` を返す。呼び出し側は返却された `Move` を `pos.do_move` に渡す。
    pub fn validate_move(&self, pos: &Position, token: &CsaMoveToken) -> Result<Move, Violation> {
        let parsed = parse_csa_token(token.as_str())?;
        let core_color: CoreColor = parsed.side.into();
        if core_color != pos.side_to_move() {
            return Err(Violation::WrongTurn {
                got: parsed.side,
                expected: pos.side_to_move().into(),
            });
        }

        let candidate = build_move(pos, &parsed)?;

        // 駒打ちのとき、まず二歩・在庫不足を個別に判定して理由を切り分ける。
        if candidate.is_drop() {
            if pos.hand(core_color).count(candidate.drop_piece_type()) == 0 {
                return Err(Violation::NoPieceInHand);
            }
            if candidate.drop_piece_type() == PieceType::Pawn
                && file_has_own_pawn(pos, core_color, candidate.to())
            {
                return Err(Violation::DoublePawn);
            }
        }

        // 同一の {from, to, drop_pt, promote} を持つ合法手があれば採用する。
        // generate_legal_all は不成も含めて生成するため CSA 仕様（不成許容）と整合する。
        let mut list = MoveList::new();
        generate_legal_all(pos, &mut list);
        if let Some(found) = list.iter().find(|mv| moves_match(**mv, candidate)) {
            return Ok(*found);
        }

        // 候補が合法手リストに無い場合、駒打ちで歩なら打ち歩詰の可能性がある。
        if candidate.is_drop()
            && candidate.drop_piece_type() == PieceType::Pawn
            && is_pawn_drop_only_blocked_by_uchifuzume(pos, candidate.to(), core_color)
        {
            return Err(Violation::Uchifuzume);
        }
        Err(Violation::Illegal)
    }

    /// 千日手判定。`pos` は最後の `do_move` 直後の局面を渡す。
    ///
    /// `Position::repetition_state` 内部で連続王手判定も行われるため、
    /// 千日手成立時の勝敗側もここで切り分ける。
    ///
    /// **発火タイミング:**
    /// - 通常千日手 (Draw): 同一局面 4 回目の到達で発火する。`state.repetition < 0`
    ///   (= `times >= 3` = 4 回目以降の出現) を必須条件としており、それ以前の非決定的
    ///   な再来では `None` を返す。これは競技将棋の「同一局面 4 回で引き分け」ルール
    ///   に一致する。
    /// - 連続王手千日手 (Win/Lose): 1 サイクル目の再来で発火する。連続王手は
    ///   反則行為であり、1 循環で反則確定すればそれ以降は続行する意味がないため、
    ///   非決定的な (`rep > 0` の) 再来でも Verdict を返す。
    pub fn classify_repetition(&self, pos: &Position) -> RepetitionVerdict {
        let state = pos.state();
        if state.repetition == 0 {
            return RepetitionVerdict::None;
        }
        match state.repetition_type {
            RepetitionState::None | RepetitionState::Superior | RepetitionState::Inferior => {
                RepetitionVerdict::None
            }
            // 非決定的な再来 (rep > 0) では発火せず対局続行。決定的 (rep < 0) でのみ発火。
            RepetitionState::Draw if state.repetition < 0 => RepetitionVerdict::Sennichite,
            RepetitionState::Draw => RepetitionVerdict::None,
            RepetitionState::Lose => RepetitionVerdict::OuteSennichiteLose,
            RepetitionState::Win => RepetitionVerdict::OuteSennichiteWin,
        }
    }

    /// 通常の千日手（4 回出現の引き分け）が成立しているか。
    ///
    /// 設計書 §Validator のインターフェースに合わせた薄いラッパ。
    pub fn is_sennichite(&self, pos: &Position) -> bool {
        matches!(self.classify_repetition(pos), RepetitionVerdict::Sennichite)
    }

    /// 連続王手の千日手が成立しているか（勝敗側の判別はしない）。
    ///
    /// 勝敗の切り分けが必要なら [`Self::classify_repetition`] を使う。
    pub fn is_oute_sennichite(&self, pos: &Position) -> bool {
        matches!(
            self.classify_repetition(pos),
            RepetitionVerdict::OuteSennichiteLose | RepetitionVerdict::OuteSennichiteWin
        )
    }

    /// 直前の駒打ちが打ち歩詰かどうかを `pos` のみから判定する補助 API。
    ///
    /// 通常は `validate_move` の戻り値で判定するが、観戦応答や棋譜検証など
    /// `Move` を手元に持たないユースケースのために残してある。
    pub fn is_uchifuzume(&self, pos: &Position, token: &CsaMoveToken) -> bool {
        matches!(self.validate_move(pos, token), Err(Violation::Uchifuzume))
    }

    /// `%KACHI`（入玉宣言）が `pos` の手番側で成立するか判定する。
    ///
    /// 内部は `rshogi_core::Position::declaration_win` に委譲する。CSA 24 点法
    /// （`Point24`）を既定とし、`Point27`/`Point24H`/`Point27H` も同 API で扱える。
    /// `TryRule` も API 契約上は宣言成立を示す任意の `Move` を Accepted として
    /// 受け付けるため、`entering_king_rule` を切替えても呼び出し側のコードは変更不要。
    pub fn evaluate_kachi(&self, pos: &Position) -> KachiOutcome {
        let mv = pos.declaration_win(self.entering_king_rule);
        if mv.is_none() {
            KachiOutcome::Rejected
        } else {
            KachiOutcome::Accepted
        }
    }

    /// 24 点法相当の駒点合計を計算する純粋関数。
    ///
    /// 玉を除く敵陣 1〜3 段（後手から見れば 7〜9 段）の自駒と全持ち駒を合算し、
    /// 大駒（角・馬・飛・龍）を 5 点、小駒を 1 点として加算する。
    /// 入玉判定の閾値（24 点法 31 点 / 27 点法 28 点）には触れない。
    pub fn jishogi_score(&self, pos: &Position, side: Color) -> u32 {
        let core_color: CoreColor = side.into();
        let enemy_field = enemy_field_bb(core_color);
        let our_in_enemy = pos.pieces_c(core_color) & enemy_field;

        // 大駒（角・馬・飛・龍）。
        let big_in_enemy = (pos.pieces(core_color, PieceType::Bishop)
            | pos.pieces(core_color, PieceType::Horse)
            | pos.pieces(core_color, PieceType::Rook)
            | pos.pieces(core_color, PieceType::Dragon))
            & enemy_field;

        let in_enemy = our_in_enemy.count();
        let big = big_in_enemy.count();

        // 玉が敵陣にいる場合だけ「玉 1 枚分」を控除する（敵陣に玉が無い局面では
        // `our_in_enemy` に玉が含まれないので減算しない）。
        let king_in_enemy = enemy_field.contains(pos.king_square(core_color));
        let king_offset = if king_in_enemy { 1 } else { 0 };

        let hand = pos.hand(core_color);
        let small_hand = hand.count(PieceType::Pawn)
            + hand.count(PieceType::Lance)
            + hand.count(PieceType::Knight)
            + hand.count(PieceType::Silver)
            + hand.count(PieceType::Gold);
        let big_hand = hand.count(PieceType::Bishop) + hand.count(PieceType::Rook);

        // big は in_enemy 内で既に 1 点として数えられているので、追加で 4 点ずつ。
        in_enemy + big * 4 - king_offset + small_hand + big_hand * 5
    }
}

/// CSA トークンを構造化したもの（`validate_move` 内部用）。
struct ParsedToken {
    side: Color,
    from_file: u8,
    from_rank: u8,
    to_file: u8,
    to_rank: u8,
    /// CSA の駒種コードが指す `PieceType`（成り駒なら成り後の値）。
    dst_pt: PieceType,
}

fn parse_csa_token(token: &str) -> Result<ParsedToken, Violation> {
    if token.len() != 7 {
        return Err(Violation::Malformed(format!("expected 7 bytes, got {token:?}")));
    }
    let bytes = token.as_bytes();
    let side = match bytes[0] {
        b'+' => Color::Black,
        b'-' => Color::White,
        _ => return Err(Violation::Malformed(format!("invalid side prefix in {token:?}"))),
    };
    let from_file = digit(bytes[1], token)?;
    let from_rank = digit(bytes[2], token)?;
    let to_file = digit(bytes[3], token)?;
    let to_rank = digit(bytes[4], token)?;
    let code = &token[5..7];
    let dst_pt = piece_type_from_csa(code)
        .ok_or_else(|| Violation::Malformed(format!("unknown piece code: {code} in {token:?}")))?;

    if !(1..=9).contains(&to_file) || !(1..=9).contains(&to_rank) {
        return Err(Violation::Malformed(format!("invalid destination in {token:?}")));
    }
    if from_file == 0 && from_rank == 0 {
        // 駒打ち。dst_pt が手駒可能・成り駒でないことは build_move 側で再確認する。
    } else if !(1..=9).contains(&from_file) || !(1..=9).contains(&from_rank) {
        return Err(Violation::Malformed(format!("invalid source in {token:?}")));
    }

    Ok(ParsedToken {
        side,
        from_file,
        from_rank,
        to_file,
        to_rank,
        dst_pt,
    })
}

fn digit(byte: u8, token: &str) -> Result<u8, Violation> {
    if byte.is_ascii_digit() {
        Ok(byte - b'0')
    } else {
        Err(Violation::Malformed(format!("non-digit coordinate in {token:?}")))
    }
}

/// CSA 駒種コード（例: `FU`/`UM`）を `PieceType` に変換する。
fn piece_type_from_csa(code: &str) -> Option<PieceType> {
    Some(match code {
        "FU" => PieceType::Pawn,
        "KY" => PieceType::Lance,
        "KE" => PieceType::Knight,
        "GI" => PieceType::Silver,
        "KI" => PieceType::Gold,
        "KA" => PieceType::Bishop,
        "HI" => PieceType::Rook,
        "OU" => PieceType::King,
        "TO" => PieceType::ProPawn,
        "NY" => PieceType::ProLance,
        "NK" => PieceType::ProKnight,
        "NG" => PieceType::ProSilver,
        "UM" => PieceType::Horse,
        "RY" => PieceType::Dragon,
        _ => return None,
    })
}

/// CSA の筋番号（1-9）から `File` を取り出す。
fn file_from_csa(n: u8) -> Option<File> {
    if (1..=9).contains(&n) {
        File::from_u8(n - 1)
    } else {
        None
    }
}

/// CSA の段番号（1-9）から `Rank` を取り出す。
fn rank_from_csa(n: u8) -> Option<Rank> {
    if (1..=9).contains(&n) {
        Rank::from_u8(n - 1)
    } else {
        None
    }
}

/// 駒種が手駒可能（玉・成り駒以外）かを判定する。
fn is_droppable(pt: PieceType) -> bool {
    matches!(
        pt,
        PieceType::Pawn
            | PieceType::Lance
            | PieceType::Knight
            | PieceType::Silver
            | PieceType::Gold
            | PieceType::Bishop
            | PieceType::Rook
    )
}

fn build_move(pos: &Position, parsed: &ParsedToken) -> Result<Move, Violation> {
    let to_file = file_from_csa(parsed.to_file)
        .ok_or_else(|| Violation::Malformed(format!("invalid to file: {}", parsed.to_file)))?;
    let to_rank = rank_from_csa(parsed.to_rank)
        .ok_or_else(|| Violation::Malformed(format!("invalid to rank: {}", parsed.to_rank)))?;
    let to_sq = Square::new(to_file, to_rank);

    if parsed.from_file == 0 && parsed.from_rank == 0 {
        if !is_droppable(parsed.dst_pt) {
            return Err(Violation::Malformed(format!("cannot drop {:?}", parsed.dst_pt)));
        }
        return Ok(Move::new_drop(parsed.dst_pt, to_sq));
    }

    let from_file = file_from_csa(parsed.from_file)
        .ok_or_else(|| Violation::Malformed(format!("invalid from file: {}", parsed.from_file)))?;
    let from_rank = rank_from_csa(parsed.from_rank)
        .ok_or_else(|| Violation::Malformed(format!("invalid from rank: {}", parsed.from_rank)))?;
    let from_sq = Square::new(from_file, from_rank);

    let src_piece = pos.piece_on(from_sq);
    if src_piece.is_none() {
        return Err(Violation::Illegal);
    }
    let core_side: CoreColor = parsed.side.into();
    if src_piece.color() != core_side {
        return Err(Violation::Illegal);
    }

    let src_pt = src_piece.piece_type();
    let promote = match (src_pt.is_promoted(), parsed.dst_pt.is_promoted()) {
        // 成り駒のまま移動：CSA 駒種は成り駒側を指定する。
        (true, true) => {
            if src_pt != parsed.dst_pt {
                return Err(Violation::Illegal);
            }
            false
        }
        // 生駒のまま移動：CSA 駒種は生駒を指定する。
        (false, false) => {
            if src_pt != parsed.dst_pt {
                return Err(Violation::Illegal);
            }
            false
        }
        // 生駒 → 成り駒：成り。dst の生駒部分が一致する必要がある。
        (false, true) => {
            if src_pt.promote() != Some(parsed.dst_pt) {
                return Err(Violation::Illegal);
            }
            true
        }
        // 成り駒 → 生駒：禁止（成った駒は元に戻らない）。
        (true, false) => return Err(Violation::Illegal),
    };

    Ok(Move::new_move(from_sq, to_sq, promote))
}

/// 候補手と合法手リスト中の 1 手が等しいかを判定する。
fn moves_match(a: Move, b: Move) -> bool {
    if a.is_drop() != b.is_drop() {
        return false;
    }
    if a.to() != b.to() {
        return false;
    }
    if a.is_drop() {
        a.drop_piece_type() == b.drop_piece_type()
    } else {
        a.from() == b.from() && a.is_promote() == b.is_promote()
    }
}

/// 自分の歩が `to.file()` に既に存在するか（二歩判定）。
fn file_has_own_pawn(pos: &Position, color: CoreColor, to: Square) -> bool {
    let our_pawns = pos.pieces(color, PieceType::Pawn);
    let to_file = to.file();
    for r in Rank::ALL {
        if our_pawns.contains(Square::new(to_file, r)) {
            return true;
        }
    }
    false
}

/// 駒打ち先 `to` が手番側の歩を打って敵玉の頭になる位置か。
fn attacks_enemy_king_head(pos: &Position, to: Square) -> bool {
    let us = pos.side_to_move();
    let them = !us;
    let enemy_king = pos.king_square(them);

    // 歩の利き方向: 先手は段が 1 つ小さい方、後手は 1 つ大きい方。
    let to_file = to.file();
    let to_rank = to.rank();
    let target_rank = match us {
        CoreColor::Black => {
            if to_rank == Rank::Rank1 {
                return false;
            }
            // to_rank.index() は 1..=8 の範囲なので、`as u8 - 1` は 0..=7 で必ず有効。
            Rank::from_u8(to_rank as u8 - 1).expect("rank index in 0..=7")
        }
        CoreColor::White => {
            if to_rank == Rank::Rank9 {
                return false;
            }
            Rank::from_u8(to_rank as u8 + 1).expect("rank index in 1..=8")
        }
    };
    enemy_king == Square::new(to_file, target_rank)
}

/// 歩打ち候補手が「打ち歩詰でなければ合法だった」かを判定する。
///
/// `validate_move` の最終分岐で呼ばれる。`generate_legal_all` に含まれない歩打ちが
/// 打ち歩詰なのか、それとも別理由（最終段への打ち、占有マスへの打ち、王手放置で
/// 自玉が残るなど）の単なる非合法手なのかを切り分ける。
fn is_pawn_drop_only_blocked_by_uchifuzume(pos: &Position, to: Square, us: CoreColor) -> bool {
    // (a) 自玉が既に王手されていれば、歩打ちが合法手リストから漏れる主因は王手放置で、
    //     打ち歩詰の判定対象にしない。
    if pos.in_check() {
        return false;
    }

    // (b) 打つマスが空でなければそもそも歩打ちにならない。
    if pos.piece_on(to).is_some() {
        return false;
    }

    // (c) 最終段に歩を打つことは禁止（打ち歩詰以前の問題）。
    let last_rank = match us {
        CoreColor::Black => Rank::Rank1,
        CoreColor::White => Rank::Rank9,
    };
    if to.rank() == last_rank {
        return false;
    }

    // (d) 二歩は呼び出し側で除外済みだが、`is_pawn_drop_only_blocked_by_uchifuzume` 単体でも
    //     呼ばれる可能性があるためここでも再チェックしておく。
    if file_has_own_pawn(pos, us, to) {
        return false;
    }

    // (e) 歩打ちが王手にならないなら、合法手リストから漏れている理由は打ち歩詰では
    //     なく別の非合法要因（例えば自玉が pinned で動けない等）の組み合わせ。
    if !attacks_enemy_king_head(pos, to) {
        return false;
    }

    true
}

/// 敵陣（先手なら 1〜3 段、後手なら 7〜9 段）の Bitboard。
fn enemy_field_bb(color: CoreColor) -> rshogi_core::bitboard::Bitboard {
    use rshogi_core::bitboard::RANK_BB;
    match color {
        CoreColor::Black => RANK_BB[0] | RANK_BB[1] | RANK_BB[2],
        CoreColor::White => RANK_BB[6] | RANK_BB[7] | RANK_BB[8],
    }
}

impl From<Color> for CoreColor {
    fn from(c: Color) -> Self {
        match c {
            Color::Black => CoreColor::Black,
            Color::White => CoreColor::White,
        }
    }
}

impl From<CoreColor> for Color {
    fn from(c: CoreColor) -> Self {
        match c {
            CoreColor::Black => Color::Black,
            CoreColor::White => Color::White,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rshogi_core::position::Position;

    fn pos_from_sfen(sfen: &str) -> Position {
        let mut p = Position::new();
        p.set_sfen(sfen).expect("valid sfen");
        p
    }

    fn token(s: &str) -> CsaMoveToken {
        CsaMoveToken::new(s)
    }

    #[test]
    fn validate_move_accepts_initial_pawn_push() {
        let v = Validator::new(EnteringKingRule::Point24);
        let pos = pos_from_sfen(rshogi_core::position::SFEN_HIRATE);
        let mv = v.validate_move(&pos, &token("+7776FU")).unwrap();
        assert!(!mv.is_drop());
        assert!(!mv.is_promote());
    }

    #[test]
    fn validate_move_rejects_wrong_turn() {
        let v = Validator::new(EnteringKingRule::Point24);
        let pos = pos_from_sfen(rshogi_core::position::SFEN_HIRATE);
        let err = v.validate_move(&pos, &token("-3334FU")).unwrap_err();
        assert!(matches!(err, Violation::WrongTurn { .. }));
    }

    #[test]
    fn validate_move_rejects_malformed_token() {
        let v = Validator::new(EnteringKingRule::Point24);
        let pos = pos_from_sfen(rshogi_core::position::SFEN_HIRATE);
        for bad in ["", "+776FU", "*7776FU", "+77a6FU", "+7776XX"] {
            let err = v.validate_move(&pos, &token(bad)).unwrap_err();
            assert!(matches!(err, Violation::Malformed(_)), "expected Malformed for {bad}");
        }
    }

    #[test]
    fn validate_move_rejects_illegal_pin_break() {
        // 後手の飛が 5 五、先手の玉が 5 九、先手の金が 5 七にいる pin 局面。
        // 5 七の金を 4 七に移動する手は pin（縦筋）違反で非合法。
        let v = Validator::new(EnteringKingRule::Point24);
        let pos = pos_from_sfen("9/9/9/9/4r4/9/4G4/9/4K4 b - 1");
        let err = v.validate_move(&pos, &token("+5747KI")).unwrap_err();
        assert_eq!(err, Violation::Illegal);
    }

    #[test]
    fn validate_move_detects_double_pawn_drop() {
        // 先手の歩が 5 七にいる状態で、5 五へ歩を打とうとする → 二歩。
        let v = Validator::new(EnteringKingRule::Point24);
        let pos = pos_from_sfen("4k4/9/9/9/9/9/4P4/9/4K4 b P 1");
        let err = v.validate_move(&pos, &token("+0055FU")).unwrap_err();
        assert_eq!(err, Violation::DoublePawn);
    }

    #[test]
    fn validate_move_detects_no_piece_in_hand() {
        // 持ち駒に銀が無い状態で銀打ちを試みる。
        let v = Validator::new(EnteringKingRule::Point24);
        let pos = pos_from_sfen("4k4/9/9/9/9/9/9/9/4K4 b P 1");
        let err = v.validate_move(&pos, &token("+0055GI")).unwrap_err();
        assert_eq!(err, Violation::NoPieceInHand);
    }

    #[test]
    fn validate_move_detects_uchifuzume() {
        // 打ち歩詰の典型例（局面開始時点で 後手玉 は王手されていないことに注意）:
        //   後手玉 1 一、先手と 1 三、先手金 3 二、先手玉 5 九、手駒に歩。
        //   先手 +0012FU で 1 二に歩打:
        //     - 1 二歩が 1 一玉に王手。
        //     - 1 二歩は 1 三の と が守っているため 玉 で取れない。
        //     - 2 一は 3 二の金が利いて退避不可。
        //     - 1 一は隅で他の退避先なし。
        //   → 打ち歩詰。
        let v = Validator::new(EnteringKingRule::Point24);
        let pos = pos_from_sfen("8k/6G2/8+P/9/9/9/9/9/4K4 b P 1");
        let err = v.validate_move(&pos, &token("+0012FU")).unwrap_err();
        assert_eq!(err, Violation::Uchifuzume);
    }

    #[test]
    fn classify_repetition_returns_none_at_start() {
        let v = Validator::new(EnteringKingRule::Point24);
        let pos = pos_from_sfen(rshogi_core::position::SFEN_HIRATE);
        assert_eq!(v.classify_repetition(&pos), RepetitionVerdict::None);
    }

    #[test]
    fn evaluate_kachi_rejects_initial_position() {
        let v = Validator::new(EnteringKingRule::Point24);
        let pos = pos_from_sfen(rshogi_core::position::SFEN_HIRATE);
        assert_eq!(v.evaluate_kachi(&pos), KachiOutcome::Rejected);
    }

    #[test]
    fn jishogi_score_initial_position_is_zero_for_both() {
        // 平手初期局面では誰も敵陣に駒を置いておらず、持ち駒もない → 0 点。
        let v = Validator::new(EnteringKingRule::Point24);
        let pos = pos_from_sfen(rshogi_core::position::SFEN_HIRATE);
        assert_eq!(v.jishogi_score(&pos, Color::Black), 0);
        assert_eq!(v.jishogi_score(&pos, Color::White), 0);
    }

    #[test]
    fn jishogi_score_counts_hand_pieces() {
        // 先手が持ち駒：飛 2 (= 10 点)、歩 3 (= 3 点)、計 13 点。
        let v = Validator::new(EnteringKingRule::Point24);
        let pos = pos_from_sfen("4k4/9/9/9/9/9/9/9/4K4 b 2R3P 1");
        assert_eq!(v.jishogi_score(&pos, Color::Black), 13);
        assert_eq!(v.jishogi_score(&pos, Color::White), 0);
    }

    #[test]
    fn color_round_trip_with_core() {
        let svr = Color::Black;
        let core: CoreColor = svr.into();
        assert_eq!(core, CoreColor::Black);
        let back: Color = core.into();
        assert_eq!(back, Color::Black);
    }

    #[test]
    fn validate_move_rejects_promote_to_unpromoted_token() {
        // 既に成り駒（と）を「成る前駒種」で動かそうとする → 非合法。
        let v = Validator::new(EnteringKingRule::Point24);
        let pos = pos_from_sfen("4k4/9/9/9/9/9/9/4+P4/4K4 b - 1");
        let err = v.validate_move(&pos, &token("+5857FU")).unwrap_err();
        assert_eq!(err, Violation::Illegal);
    }

    #[test]
    fn validate_move_accepts_promotion_into_enemy_field() {
        // 先手歩 5 三 → 5 二 で成り（FU→TO）。
        let v = Validator::new(EnteringKingRule::Point24);
        let pos = pos_from_sfen("4k4/9/4P4/9/9/9/9/9/4K4 b - 1");
        let mv = v.validate_move(&pos, &token("+5352TO")).unwrap();
        assert!(mv.is_promote());
    }

    #[test]
    fn validate_move_rejects_drop_of_promoted_piece_code() {
        // 持ち駒に歩はあるが、CSA 駒種 TO（成り駒）で打とうとする → Malformed。
        let v = Validator::new(EnteringKingRule::Point24);
        let pos = pos_from_sfen("4k4/9/9/9/9/9/9/9/4K4 b P 1");
        let err = v.validate_move(&pos, &token("+0055TO")).unwrap_err();
        assert!(matches!(err, Violation::Malformed(_)), "got: {err:?}");
    }

    #[test]
    fn validate_move_pawn_drop_with_double_pawn_takes_priority_over_uchifuzume() {
        // 5 二に既に先手歩が居る局面で、敵玉頭でもある同筋（file 5）に歩打を試みる。
        // 検証順は「在庫 → 二歩 → 合法手探索 → 打ち歩詰判定」なので、
        // 二歩が先に検出され Violation::DoublePawn が返るはず。
        // 本テストの主目的は Uchifuzume に誤分類しないことの確認。
        let v = Validator::new(EnteringKingRule::Point24);
        let pos = pos_from_sfen("4k4/4P4/9/9/9/9/9/9/4K4 b P 1");
        let err = v.validate_move(&pos, &token("+0052FU")).unwrap_err();
        assert_eq!(err, Violation::DoublePawn);
    }

    #[test]
    fn validate_move_pawn_drop_on_king_head_but_legal_is_not_uchifuzume() {
        // 後手玉 1 一、5 八に後手飛が先手玉 5 九を王手しているので、
        // 先手は王手を防がねばならない。
        // 1 二への歩打ち（敵玉頭）は合法ではない（自玉の王手放置）が、
        // 1 二の歩は 後手玉 1 一の頭に当たるため `attacks_enemy_king_head` は true。
        // それでも `is_pawn_drop_only_blocked_by_uchifuzume` は王手放置を別経路で
        // 弾くため、Uchifuzume には分類されない。
        let v = Validator::new(EnteringKingRule::Point24);
        let pos = pos_from_sfen("8k/9/9/9/9/9/9/4r4/4K4 b P 1");
        let err = v.validate_move(&pos, &token("+0012FU")).unwrap_err();
        assert_eq!(err, Violation::Illegal);
    }

    #[test]
    fn jishogi_score_counts_pieces_in_enemy_field_without_king() {
        // 先手の歩 1 枚だけ敵陣 1 段目（5 一）に置く。玉は 5 九（敵陣外）。
        // 期待: 1 点（玉控除なし）。
        let v = Validator::new(EnteringKingRule::Point24);
        let pos = pos_from_sfen("4P4/9/9/9/9/9/9/9/4K4 b - 1");
        assert_eq!(v.jishogi_score(&pos, Color::Black), 1);
    }

    #[test]
    fn jishogi_score_subtracts_one_for_king_inside_enemy_field() {
        // 先手玉が 5 一 (敵陣)、他に駒なし → 0 点。
        let v = Validator::new(EnteringKingRule::Point24);
        let pos = pos_from_sfen("4K4/9/9/9/9/9/9/9/4k4 b - 1");
        assert_eq!(v.jishogi_score(&pos, Color::Black), 0);
    }

    #[test]
    fn jishogi_score_counts_big_pieces_in_enemy_field() {
        // 先手の飛 1 枚を 5 三 に置く。5 一 に先手玉。期待: 飛 5 + 玉除外で 5 点。
        let v = Validator::new(EnteringKingRule::Point24);
        let pos = pos_from_sfen("4K4/9/4R4/9/9/9/9/9/4k4 b - 1");
        assert_eq!(v.jishogi_score(&pos, Color::Black), 5);
    }

    #[test]
    fn evaluate_kachi_accepted_for_27pt_position() {
        // 27 点法の先手必要点 = 28 点。
        // 局面:
        //   先手 = 5 一に玉、5 二に飛、6 二に角、6 一に金、4 一に金、3 一に銀、7 一に銀、
        //          2 一に桂、8 一に桂、1 一に香、9 一に香（合計 11 枚 = 玉 + 10 枚）。
        //   先手の手駒 = 飛 1, 角 1（5 + 5 = 10 点）。
        //   駒点合計 = 11(small/big in field) + 2*4(big in field) - 1(玉控除) + 10(hand)
        //            = 11 + 8 - 1 + 10 = 28 点。
        //   敵陣の自駒は 11 枚 ≥ 10 + 1（玉込み）。
        //   先手玉に王手はない。
        //   → Point27 で Accepted。
        let v = Validator::new(EnteringKingRule::Point27);
        let pos = pos_from_sfen("LNSGKGSNL/4BR3/9/9/9/9/9/9/4k4 b RB 1");
        assert_eq!(v.evaluate_kachi(&pos), KachiOutcome::Accepted);
    }

    #[test]
    fn is_sennichite_and_is_oute_sennichite_at_start_are_false() {
        let v = Validator::new(EnteringKingRule::Point24);
        let pos = pos_from_sfen(rshogi_core::position::SFEN_HIRATE);
        assert!(!v.is_sennichite(&pos));
        assert!(!v.is_oute_sennichite(&pos));
    }

    /// CSA トークン列を順次 `do_move` で適用する検証用ヘルパ。
    ///
    /// 千日手系テストは CSA トークンで棋譜を記述した方が読みやすい。`validate_move`
    /// で Move を取り出し `Position::gives_check` を渡して `do_move` する、という
    /// 3 ステップを 1 つにまとめる。
    fn apply_moves(pos: &mut Position, rule: EnteringKingRule, tokens: &[&str]) {
        let v = Validator::new(rule);
        for t in tokens {
            let tok = token(t);
            let mv = v
                .validate_move(pos, &tok)
                .unwrap_or_else(|e| panic!("validate_move failed for {t}: {e:?}"));
            let gc = pos.gives_check(mv);
            pos.do_move(mv, gc);
        }
    }

    #[test]
    fn classify_repetition_returns_sennichite_after_12_ply_gold_dance() {
        // 平手初期局面から両者の左金を 4 九 ↔ 4 八 / 4 一 ↔ 4 二 と循環させて
        // 3 サイクル (= 12 手) で初期局面 4 回目の到達 → 通常千日手。
        //
        // どの手も相手玉には利かないため連続王手は発生せず、RepetitionState::Draw
        // として分類される想定。
        let mut pos = pos_from_sfen(rshogi_core::position::SFEN_HIRATE);
        let v = Validator::new(EnteringKingRule::Point24);
        let cycle = ["+4948KI", "-4142KI", "+4849KI", "-4241KI"];
        apply_moves(&mut pos, EnteringKingRule::Point24, &cycle);
        apply_moves(&mut pos, EnteringKingRule::Point24, &cycle);
        // 3 周目の最終手でちょうど初期局面 4 回目の出現。
        apply_moves(&mut pos, EnteringKingRule::Point24, &cycle);
        assert_eq!(v.classify_repetition(&pos), RepetitionVerdict::Sennichite);
    }

    #[test]
    fn classify_repetition_returns_oute_sennichite_win_for_perpetual_checker() {
        // 黒玉 9 九、黒飛 3 八、白玉 3 二、side = White（=白が王手されている局面から開始）。
        // 4 手 1 サイクルで「白王が 3 二 ↔ 4 二 を往復し、黒飛が 3 八 ↔ 4 八 を往復して
        // そのたびに王手をかけ直す」連続王手の千日手を作る。3 サイクル (= 12 手) 経過で
        // 初期 SFEN 局面が 4 回目の出現となり、RepetitionState::Win が分類される。
        //
        // `RepetitionState::Win` は「手番側 = 連続王手されていた側が勝つ」を意味するので
        // Verdict は `OuteSennichiteWin`。側面: 黒が perpetual checker。
        let mut pos = pos_from_sfen("9/6k2/9/9/9/9/9/6R2/K8 w - 1");
        let v = Validator::new(EnteringKingRule::Point24);
        let cycle = ["-3242OU", "+3848HI", "-4232OU", "+4838HI"];
        apply_moves(&mut pos, EnteringKingRule::Point24, &cycle);
        apply_moves(&mut pos, EnteringKingRule::Point24, &cycle);
        apply_moves(&mut pos, EnteringKingRule::Point24, &cycle);
        assert_eq!(v.classify_repetition(&pos), RepetitionVerdict::OuteSennichiteWin);
        // `is_oute_sennichite` は勝敗を区別しない薄いラッパなので両 variant で true。
        assert!(v.is_oute_sennichite(&pos));
    }

    #[test]
    fn classify_repetition_returns_oute_sennichite_lose_when_cycle_ends_on_non_checking_move() {
        // Win variant と同じ駒配置・同じ連続王手循環だが、開始 SFEN を「サイクル中間
        // (白玉 4 二 退避済み、黒番で黒飛が次に王手を掛け直す直前)」に寄せると、
        // 検出発火 M12 は **白の退避手** で閉じる。
        //
        // 結果として side-to-move after M12 = Black（＝連続王手していた側）で、
        // `cc_side = cc[Black]` が高い値を持つため `RepetitionState::Lose` が選ばれる。
        // Verdict は `OuteSennichiteLose`。Lose は「手番側が負け = 連続王手していた側が負け」
        // であり、このケースでは Black（= from.opposite() = 次手番）が敗者となる。
        let mut pos = pos_from_sfen("9/5k3/9/9/9/9/9/6R2/K8 b - 1");
        let v = Validator::new(EnteringKingRule::Point24);
        let cycle = ["+3848HI", "-4232OU", "+4838HI", "-3242OU"];
        apply_moves(&mut pos, EnteringKingRule::Point24, &cycle);
        apply_moves(&mut pos, EnteringKingRule::Point24, &cycle);
        apply_moves(&mut pos, EnteringKingRule::Point24, &cycle);
        assert_eq!(v.classify_repetition(&pos), RepetitionVerdict::OuteSennichiteLose);
    }
}
