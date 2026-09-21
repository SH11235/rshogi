//! テスト専用: seed 固定のランダムプレイアウト
//!
//! 平手から全合法手（不成を含む）を一様に選んで進める。プレイアウトごとに
//! `seed + index` で乱数列を作るので、失敗したプレイアウトだけを単独で再現できる。

use rand::{Rng, SeedableRng};
use rand_xoshiro::Xoshiro256PlusPlus;

use super::Position;
use crate::movegen::{MoveList, generate_legal_all};
use crate::types::Move;

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
