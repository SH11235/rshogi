//! df-pn が局面に求める操作
//!
//! 探索本体はこの trait 越しに局面を扱う。本番は `Position`、テストでは
//! 抽象的な AND/OR グラフを同じ探索本体に通して、GHI (経路依存の結果の誤再利用) を検査する。

use super::table::TableKey;
use crate::bitboard::{FILE_BB, between_bb};
use crate::movegen::{ExtMoveBuffer, GenType, MoveList, generate_legal_all, generate_with_type};
use crate::position::Position;
use crate::types::{Color, Hand, Move, PieceType, Rank, Square};

/// `MateState::delay_key` の組の番号: 成・不成の組は移動元と移動先から (`Square::NUM` の 2 乗未満)、
/// 合駒は打つ升ごと、受け方の利きの無い升への合駒は全部で 1 組
const DELAY_GROUP_DROP: u16 = (Square::NUM * Square::NUM) as u16;
const DELAY_GROUP_UNSUPPORTED_DROP: u16 = DELAY_GROUP_DROP + Square::NUM as u16;

/// 盤上と持駒を合わせた駒数 (持駒の各駒種の上限)
pub(super) const FULL_HAND: Hand = Hand::EMPTY
    .set(PieceType::Pawn, 18)
    .set(PieceType::Lance, 4)
    .set(PieceType::Knight, 4)
    .set(PieceType::Silver, 4)
    .set(PieceType::Gold, 4)
    .set(PieceType::Bishop, 2)
    .set(PieceType::Rook, 2);

/// `initial_pn_dn` の見積もりの基準値と加算量。王手の子どうしに差を付けるため 1 より大きくする。
pub(super) const PNDN_UNIT: u32 = 2;

/// df-pn が局面に求める操作
pub(super) trait MateState {
    /// 指し手
    type Mv: Copy + Eq;

    /// 1 回の展開 (同じ局面の子を並べる間) だけ使い回す計算結果。探索 (`Searcher`) ごとに 1 つ作り、
    /// 展開を始めるたびに `ExpandCache::begin` で前の展開の結果を捨てる。
    type ExpandCache: ExpandCache;

    /// 局面全体のキー (盤面・手番・持駒。経路の反復判定に使う)
    fn key(&self) -> u64;

    /// 置換表での識別子 (攻め方は `attacker`)
    fn table_key(&self, attacker: Color) -> TableKey;

    /// 指し手 `m` を指した後の `key()`。局面は変えない。
    fn key_after(&self, m: Self::Mv) -> u64;

    /// 指し手 `m` を指した後の `table_key()`。局面は変えない。
    fn table_key_after(&self, m: Self::Mv, attacker: Color) -> TableKey;

    /// 手番側の候補手を `out` に追加する。
    ///
    /// `or_node` なら攻め方の手 (合法な王手)、そうでなければ受け方の手 (合法な応手すべて)。
    fn generate(&self, or_node: bool, out: &mut Vec<Self::Mv>);

    /// 後回しにしてよい手なら (組, 組の中で読む順 (小さいほど先))。
    ///
    /// 受け方の合駒と、成れる手の成・不成の組が対象。組の先頭以外は、組の中で前の手が確定するまで
    /// 子として数えない (読まない手を作るのではなく、読む順を遅らせるだけ)。
    fn delay_key(
        &self,
        _m: Self::Mv,
        _or_node: bool,
        _cache: &mut Self::ExpandCache,
    ) -> Option<(u16, u8)> {
        None
    }

    /// 攻め方の手 `m` を指す前の攻め方の持駒。`after` は指した後の持駒。
    /// 打った駒は足し (駒数の上限で止める)、取った駒は引く (0 で止める)。
    ///
    /// 反証駒に使うときは、`after` が `m` の後の実際の持駒以上であること (反証駒の不変条件) が前提。
    /// 取った駒を `after` が持っていないと 0 で止まり、前の局面の反証駒が実際の持駒より少なくなる。
    fn hand_before(&self, m: Self::Mv, after: Hand) -> Hand;

    /// 受け方の局面 (王手されている) の証明駒。`proof` は応手すべての証明駒を合わせたもの。
    ///
    /// 攻め方の持駒が今より少ないと、その分の駒は受け方の手にあり、今は打てない合駒を打てる。
    /// 合駒の余地があれば、受け方が持っていない駒種は攻め方の今の枚数を証明駒に入れる。
    fn proof_hand_for_evasions(&self, proof: Hand, attacker: Color) -> Hand;

    /// 攻め方の局面の反証駒。`disproof` は王手すべての反証駒の共通部分。
    ///
    /// 今は持っていない駒種でも、持っていれば打って王手できる駒種は反証駒から除く
    /// (その王手は読んでいないので、詰むかもしれない)。
    fn disproof_hand_for_checks(&self, disproof: Hand, attacker: Color) -> Hand;

    /// 受け方の局面で、応手 `m` で詰まないときの反証駒。`disproof` は `m` の後の局面の反証駒。
    ///
    /// `m` が駒打ちなら、受け方の手に打つ駒が 1 枚は残る枚数までに抑える。
    fn disproof_hand_for_escape(&self, m: Self::Mv, disproof: Hand, attacker: Color) -> Hand;

    /// 攻め方 (手番側) の 1 手詰めの手。返す手は合法な王手で、指した後に受け方の合法手が無いことまで
    /// 確かめ済み。
    fn mate_1ply(&mut self) -> Option<Self::Mv>;

    /// 置換表に無い子 (指し手 `m` の後の局面) の pn/dn の初期値。確定の判定には使わないので、
    /// 正の有限値なら正しさには影響しない。
    fn initial_pn_dn(
        &self,
        _m: Self::Mv,
        _or_node: bool,
        _cache: &mut Self::ExpandCache,
    ) -> (u32, u32) {
        (1, 1)
    }

    /// `mate_1ply` の手と、その詰みの証明駒
    fn mate_1ply_with_proof(&mut self) -> Option<(Self::Mv, Hand)>;

    /// 指し手を進める
    fn do_move(&mut self, m: Self::Mv);

    /// 指し手を戻す
    fn undo_move(&mut self, m: Self::Mv);
}

/// `MateState::ExpandCache` の操作
pub(super) trait ExpandCache: Default {
    /// 新しい展開を始める (前の展開で覚えた結果を使わない)
    fn begin(&mut self);
}

impl ExpandCache for () {
    fn begin(&mut self) {}
}

/// 升ごとの (手番側の駒の利きの数, 相手の駒の利きの数)。同じ升への手 (打つ駒違い・成不成) で
/// 利きを数え直さないため、1 回の展開の間だけ覚えておく。展開ごとに表を消さず、世代で区別する。
pub(super) struct AttackCounts {
    generation: u32,
    filled: [u32; Square::NUM],
    counts: [(u8, u8); Square::NUM],
}

impl Default for AttackCounts {
    fn default() -> Self {
        Self {
            generation: 1,
            filled: [0; Square::NUM],
            counts: [(0, 0); Square::NUM],
        }
    }
}

impl ExpandCache for AttackCounts {
    fn begin(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            self.filled = [0; Square::NUM];
            self.generation = 1;
        }
    }
}

impl AttackCounts {
    fn get(&mut self, pos: &Position, sq: Square) -> (u32, u32) {
        let i = sq.index();
        if self.filled[i] != self.generation {
            let us = pos.side_to_move();
            let attackers = pos.attackers_to(sq);
            self.counts[i] = (
                (attackers & pos.pieces_c(us)).count() as u8,
                (attackers & pos.pieces_c(!us)).count() as u8,
            );
            self.filled[i] = self.generation;
        }
        let (ours, theirs) = self.counts[i];
        (u32::from(ours), u32::from(theirs))
    }
}

impl MateState for Position {
    type Mv = Move;
    type ExpandCache = AttackCounts;

    fn key(&self) -> u64 {
        Position::key(self)
    }

    fn table_key(&self, attacker: Color) -> TableKey {
        TableKey {
            key: Position::key(self),
            board: self.state().board_key,
            hand: self.hand(attacker),
        }
    }

    fn key_after(&self, m: Move) -> u64 {
        Position::key_after(self, m)
    }

    fn table_key_after(&self, m: Move, attacker: Color) -> TableKey {
        let (board, hand_key) = Position::keys_after(self, m);
        let mut hand = self.hand(attacker);
        if self.side_to_move() == attacker {
            if m.is_drop() {
                hand = hand.sub(m.drop_piece_type());
            } else {
                let captured = self.piece_on(m.to());
                // 玉は持駒にならない (`keys_after` と同じ扱い)
                if captured.is_some() && captured.piece_type() != PieceType::King {
                    hand = hand.add(captured.piece_type().unpromote());
                }
            }
        }
        TableKey {
            key: board ^ hand_key,
            board,
            hand,
        }
    }

    fn generate(&self, or_node: bool, out: &mut Vec<Move>) {
        let mut buffer = ExtMoveBuffer::new();
        // 不成でしか詰まない・不成でしか逃れられない局面があるので、どちらも *All で生成する。
        let gen_type = match (or_node, self.in_check()) {
            (true, false) => GenType::ChecksAll,
            (_, true) => GenType::EvasionsAll,
            (false, false) => {
                // 受け方の手番は直前の王手で必ず王手されている。崩れていても応手を空にして
                // 「詰み」と誤らないよう、合法手をすべて返す。
                debug_assert!(false, "AND ノードで王手されていない: {}", self.to_sfen());
                GenType::LegalAll
            }
        };
        generate_with_type(self, gen_type, &mut buffer, None);
        for ext in buffer.iter() {
            let mv = ext.mv;
            if !self.is_legal(mv) {
                continue;
            }
            // 攻め方が王手されているときは、王手を外しつつ王手をかける手だけが攻め方の手
            if or_node && gen_type == GenType::EvasionsAll && !self.gives_check(mv) {
                continue;
            }
            out.push(mv);
        }
    }

    fn delay_key(&self, m: Move, or_node: bool, cache: &mut AttackCounts) -> Option<(u16, u8)> {
        let us = self.side_to_move();
        if m.is_drop() {
            if or_node {
                return None;
            }
            let piece = match m.drop_piece_type() {
                PieceType::Pawn => 0,
                PieceType::Lance => 1,
                PieceType::Knight => 2,
                PieceType::Silver => 3,
                PieceType::Gold => 4,
                PieceType::Bishop => 5,
                _ => 6,
            };
            // 受け方の駒の利きが無い升への合駒は、どこに打っても大差ないので 1 組にまとめる。
            // 同じ升への合駒は駒によらず同じ組に入れ、安い駒から読む
            let group = if cache.get(self, m.to()).0 == 0 {
                DELAY_GROUP_UNSUPPORTED_DROP
            } else {
                DELAY_GROUP_DROP + m.to().index() as u16
            };
            return Some((group, piece));
        }
        if !crate::mate::can_promote(us, m.from(), m.to()) {
            return None;
        }
        // 歩・角・飛の不成と、2 段目への香の不成は、打ち歩詰めの回避などでしか成より良くならない
        let delayable = match self.piece_on(m.from()).piece_type() {
            PieceType::Pawn | PieceType::Bishop | PieceType::Rook => true,
            PieceType::Lance => m.to().rank().relative(us) == Rank::Rank2,
            _ => false,
        };
        let group = (m.from().index() * Square::NUM + m.to().index()) as u16;
        delayable.then_some((group, u8::from(!m.is_promote())))
    }

    fn hand_before(&self, m: Move, after: Hand) -> Hand {
        if m.is_drop() {
            let pt = m.drop_piece_type();
            return if after.count(pt) < FULL_HAND.count(pt) {
                after.add(pt)
            } else {
                after
            };
        }
        let captured = self.piece_on(m.to());
        if captured.is_some() && captured.piece_type() != PieceType::King {
            let pt = captured.piece_type().unpromote();
            if after.has(pt) {
                return after.sub(pt);
            }
        }
        after
    }

    fn proof_hand_for_evasions(&self, mut proof: Hand, attacker: Color) -> Hand {
        let defender = self.side_to_move();
        let checkers = self.checkers();
        if checkers.count() != 1 {
            return proof;
        }
        let checker = checkers.lsb().expect("王手している駒は 1 枚");
        let between = between_bb(self.king_square(defender), checker);
        if between.is_empty() {
            return proof;
        }
        let attacker_hand = self.hand(attacker);
        let defender_hand = self.hand(defender);
        for pt in PieceType::HAND_PIECES {
            if defender_hand.has(pt) {
                continue;
            }
            if pt == PieceType::Pawn && !pawn_can_interpose(self, defender, between) {
                continue;
            }
            proof = proof.set(pt, attacker_hand.count(pt));
        }
        proof
    }

    fn disproof_hand_for_checks(&self, mut disproof: Hand, attacker: Color) -> Hand {
        let hand = self.hand(attacker);
        let king = self.king_square(!attacker);
        let empty = !self.occupied();
        for pt in PieceType::HAND_PIECES {
            if hand.has(pt) || !disproof.has(pt) {
                continue;
            }
            // 玉の筋に歩があれば二歩で打てない
            if pt == PieceType::Pawn
                && !(self.pieces(attacker, PieceType::Pawn) & FILE_BB[king.file().index()])
                    .is_empty()
            {
                continue;
            }
            if !(self.check_squares(pt) & empty).is_empty() {
                disproof = disproof.set(pt, 0);
            }
        }
        disproof
    }

    fn disproof_hand_for_escape(&self, m: Move, disproof: Hand, attacker: Color) -> Hand {
        if !m.is_drop() {
            return disproof;
        }
        let pt = m.drop_piece_type();
        let total = self.hand(attacker).count(pt) + self.hand(!attacker).count(pt);
        if disproof.count(pt) >= total {
            disproof.set(pt, total - 1)
        } else {
            disproof
        }
    }

    fn initial_pn_dn(&self, m: Move, or_node: bool, cache: &mut AttackCounts) -> (u32, u32) {
        if !or_node {
            return (1, 1);
        }
        // 王手の後の局面の見積もり (GPS 将棋 / KomoringHeights の df-pn+ と同じ考え方)。
        // 王手した升に受け方の利きが多いほど証明しにくく、攻め方の利きが多いほど反証しにくい。
        // 受け方の応手の側は見積もらない (見積もると詰む問題でもノードが増える)。
        let to = m.to();
        let (ours, theirs) = cache.get(self, to);
        let ours = ours + u32::from(m.is_drop());
        let mut pn = PNDN_UNIT;
        let mut dn = PNDN_UNIT;
        if theirs >= 2 {
            pn += PNDN_UNIT;
        }
        let captured = self.piece_on(to);
        if ours > theirs
            || captured.is_some()
                && matches!(captured.piece_type().unpromote(), PieceType::Gold | PieceType::Silver)
        {
            dn += PNDN_UNIT;
        } else {
            pn += PNDN_UNIT;
        }
        (pn, dn)
    }

    fn mate_1ply(&mut self) -> Option<Move> {
        self.checked_mate_1ply(false).map(|(mv, _)| mv)
    }

    fn mate_1ply_with_proof(&mut self) -> Option<(Move, Hand)> {
        self.checked_mate_1ply(true)
    }

    fn do_move(&mut self, m: Move) {
        let gives_check = self.gives_check(m);
        Position::do_move(self, m, gives_check);
    }

    fn undo_move(&mut self, m: Move) {
        Position::undo_move(self, m);
    }
}

impl Position {
    /// 1 手詰めの手と、`with_proof` ならその証明駒 (そうでなければ空の持駒)
    fn checked_mate_1ply(&mut self, with_proof: bool) -> Option<(Move, Hand)> {
        if self.in_check() {
            return None;
        }
        // 生成した手と同じ形 (上位 16bit に駒情報) にそろえる
        let found = Position::mate_1ply(self);
        let mv = self.to_move(found).filter(|m| !m.is_none())?;
        // 1 手詰めルーチンの返す手を信用せず、合法な王手であることと、
        // 指した後に応手 (不成を含む) が無いことを確かめる。打ち歩詰めの歩打ちは is_legal が拒否する。
        if !(self.pseudo_legal(mv) && self.is_legal(mv) && self.gives_check(mv)) {
            debug_assert!(false, "mate_1ply が合法な王手でない手を返した: {}", mv.to_usi());
            return None;
        }
        let attacker = self.side_to_move();
        self.do_move(mv, true);
        let mut replies = MoveList::new();
        generate_legal_all(self, &mut replies);
        let proof = replies.is_empty().then(|| {
            if with_proof {
                self.proof_hand_for_evasions(Hand::EMPTY, attacker)
            } else {
                Hand::EMPTY
            }
        });
        Position::undo_move(self, mv);
        proof.map(|proof| {
            (
                mv,
                if with_proof {
                    self.hand_before(mv, proof)
                } else {
                    proof
                },
            )
        })
    }
}

/// 王手の間の升 `between` のどこかに、二歩にならずに歩を打てるか (行き所の無い段は気にしない)
fn pawn_can_interpose(pos: &Position, defender: Color, between: crate::bitboard::Bitboard) -> bool {
    let pawns = pos.pieces(defender, PieceType::Pawn);
    let mut squares = between;
    while !squares.is_empty() {
        let sq = squares.pop();
        if (pawns & FILE_BB[sq.file().index()]).is_empty() {
            return true;
        }
    }
    false
}
