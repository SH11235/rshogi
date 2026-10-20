//! df-pn が局面に求める操作
//!
//! 探索本体はこの trait 越しに局面を扱う。本番は `Position`、テストでは
//! 抽象的な AND/OR グラフを同じ探索本体に通して、GHI (経路依存の結果の誤再利用) を検査する。

use crate::movegen::{ExtMoveBuffer, GenType, MoveList, generate_legal_all, generate_with_type};
use crate::position::Position;
use crate::types::Move;

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
        let gen_type = if or_node && !self.in_check() {
            GenType::ChecksAll
        } else {
            GenType::EvasionsAll
        };
        if gen_type == GenType::EvasionsAll && !self.in_check() {
            // 受け方の手番は直前の王手で必ず王手されている
            debug_assert!(false, "AND ノードで王手されていない: {}", self.to_sfen());
            return;
        }
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
