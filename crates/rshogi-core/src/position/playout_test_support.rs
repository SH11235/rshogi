//! テスト専用: 固定局面と seed 固定のランダムプレイアウト
//!
//! 平手から全合法手（不成を含む）を一様に選んで進める。プレイアウトごとに
//! `seed + index` で乱数列を作るので、失敗したプレイアウトだけを単独で再現できる。

use rand::{Rng, SeedableRng};
use rand_xoshiro::Xoshiro256PlusPlus;

use super::Position;
use crate::movegen::{MoveList, generate_legal_all};
use crate::types::Move;

/// 合法手の多い「指し手生成祭り」の局面（打つ手・成る手が多い）。
/// YaneuraOu の unit test の `matsuri_sfen` と同じ局面で、SFEN の持ち駒の並び順だけが違う。
pub(crate) const PERFT_MATSURI: &str =
    "l6nl/5+P1gk/2np1S3/p1p4Pp/3P2Sp1/1PPb2P1P/P5GS1/R8/LN4bKL w RGgsn5p 1";
/// 双方に持ち駒と成駒がある中盤の実戦形
pub(crate) const PERFT_MIDGAME: &str =
    "l2+R3nl/3s1kg2/3pppsp1/p1p3p1p/2lS3P1/P4PP1P/1PNPP1N2/2K1g1SR1/+b4G2L w BGN2p 46";

pub(crate) struct RandomPlayout {
    pub(crate) pos: Position,
    seed: u64,
    index: u64,
    rng: Xoshiro256PlusPlus,
    moves: Vec<Move>,
}

impl RandomPlayout {
    pub(crate) fn new(seed: u64, index: u64) -> Self {
        let mut pos = Position::new();
        pos.set_hirate();
        Self {
            pos,
            seed,
            index,
            rng: Xoshiro256PlusPlus::seed_from_u64(seed.wrapping_add(index)),
            moves: Vec::new(),
        }
    }

    /// 合法手から一様に 1 手選んで進める。合法手が無ければ `None`。
    pub(crate) fn step(&mut self) -> Option<Move> {
        let mut list = MoveList::new();
        generate_legal_all(&self.pos, &mut list);
        if list.is_empty() {
            return None;
        }
        let mv = list.at(self.rng.random_range(0..list.len()));
        let gives_check = self.pos.gives_check(mv);
        self.pos.do_move(mv, gives_check);
        self.moves.push(mv);
        Some(mv)
    }

    /// ここまでに進めた指し手（先頭が初手）
    pub(crate) fn moves(&self) -> &[Move] {
        &self.moves
    }

    /// 失敗時に手順を再現するための情報（seed・プレイアウト番号・USI の指し手列）
    pub(crate) fn describe(&self) -> String {
        let moves: Vec<String> = self.moves.iter().map(|mv| mv.to_usi()).collect();
        format!(
            "seed={:#x} playout={} position startpos moves {}",
            self.seed,
            self.index,
            moves.join(" ")
        )
    }
}
