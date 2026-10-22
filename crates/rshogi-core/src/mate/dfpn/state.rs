//! df-pn が局面に求める操作
//!
//! 探索本体はこの trait 越しに局面を扱う。本番は `Position`、テストでは
//! 抽象的な AND/OR グラフを同じ探索本体に通して、GHI (経路依存の結果の誤再利用) を検査する。

use crate::movegen::{ExtMoveBuffer, GenType, MoveList, generate_legal_all, generate_with_type};
use crate::position::Position;
use crate::types::{Move, PieceType, Rank};

/// df-pn が局面に求める操作
pub(super) trait MateState {
    /// 指し手
    type Mv: Copy + Eq;

    /// 局面の同一性キー (盤面・手番・持駒。置換表と経路の反復判定に使う)
    fn key(&self) -> u64;

    /// 指し手 `m` を指した後の `key()`。局面は変えない。
    fn key_after(&self, m: Self::Mv) -> u64;

    /// 手番側の候補手を `out` に追加する。
    ///
    /// `or_node` なら攻め方の手 (合法な王手)、そうでなければ受け方の手 (合法な応手すべて)。
    fn generate(&self, or_node: bool, out: &mut Vec<Self::Mv>);

    /// 後回しにしてよい手なら、同じ組の中で読む順 (小さいほど先)。
    ///
    /// 受け方の合駒と、成れる手の成・不成の組が対象。組の先頭以外は、直前の手が確定するまで
    /// 子として数えない (読まない手を作るのではなく、読む順を遅らせるだけ)。
    fn delay_rank(&self, _m: Self::Mv, _or_node: bool) -> Option<u8> {
        None
    }

    /// 後回しにしてよい 2 手 (`delay_rank` が `Some`) が同じ組か
    fn same_delay_group(&self, _a: Self::Mv, _b: Self::Mv) -> bool {
        false
    }

    /// 攻め方の 1 手詰めの手。返す手は合法な王手で、指した後に受け方の合法手が無いことまで確かめ済み。
    fn mate_1ply(&mut self) -> Option<Self::Mv>;

    /// 指し手を進める
    fn do_move(&mut self, m: Self::Mv);

    /// 指し手を戻す
    fn undo_move(&mut self, m: Self::Mv);
}

impl MateState for Position {
    type Mv = Move;

    fn key(&self) -> u64 {
        Position::key(self)
    }

    fn key_after(&self, m: Move) -> u64 {
        Position::key_after(self, m)
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

    fn delay_rank(&self, m: Move, or_node: bool) -> Option<u8> {
        if m.is_drop() {
            // 合駒は安い駒から読む
            return (!or_node).then(|| match m.drop_piece_type() {
                PieceType::Pawn => 0,
                PieceType::Lance => 1,
                PieceType::Knight => 2,
                PieceType::Silver => 3,
                PieceType::Gold => 4,
                PieceType::Bishop => 5,
                _ => 6,
            });
        }
        let us = self.side_to_move();
        if !crate::mate::can_promote(us, m.from(), m.to()) {
            return None;
        }
        // 歩・角・飛の不成と、2 段目への香の不成は、打ち歩詰めの回避などでしか成より良くならない
        let delayable = match self.piece_on(m.from()).piece_type() {
            PieceType::Pawn | PieceType::Bishop | PieceType::Rook => true,
            PieceType::Lance => m.to().rank().relative(us) == Rank::Rank2,
            _ => false,
        };
        delayable.then_some(u8::from(!m.is_promote()))
    }

    fn same_delay_group(&self, a: Move, b: Move) -> bool {
        match (a.is_drop(), b.is_drop()) {
            (true, true) => {
                if a.to() == b.to() {
                    return true;
                }
                // 受け方の駒の利きが無く王手にもならない中合いは、どこに打っても大差ないので 1 組にまとめる
                let us = self.side_to_move();
                let unsupported =
                    |m: Move| self.attackers_to_c(m.to(), us).is_empty() && !self.gives_check(m);
                unsupported(a) && unsupported(b)
            }
            (false, false) => a.from() == b.from() && a.to() == b.to(),
            _ => false,
        }
    }

    fn mate_1ply(&mut self) -> Option<Move> {
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
        self.do_move(mv, true);
        let mut replies = MoveList::new();
        generate_legal_all(self, &mut replies);
        self.undo_move(mv);
        if replies.is_empty() { Some(mv) } else { None }
    }

    fn do_move(&mut self, m: Move) {
        let gives_check = self.gives_check(m);
        Position::do_move(self, m, gives_check);
    }

    fn undo_move(&mut self, m: Move) {
        Position::undo_move(self, m);
    }
}
