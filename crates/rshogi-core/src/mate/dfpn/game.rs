//! 対局ルールに沿った詰み戦略の検証。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::movegen::{MoveList, generate_legal_all};
use crate::position::Position;
use crate::time::Instant;
use crate::types::{Color, Hand, MAX_PLY, Move, PieceType};

use super::state::MateState;
use super::table::TableKey;
use super::{DfpnSolver, MateLimits, MateMode, MateResult};

pub(crate) fn has_checking_move(pos: &Position) -> bool {
    if pos.is_pass_rights_enabled() {
        return false;
    }
    let mut checks = Vec::new();
    pos.generate(true, &mut checks);
    !checks.is_empty()
}

pub(crate) struct GameMate {
    pub(crate) first: Move,
    pub(crate) plies: u16,
}

impl DfpnSolver {
    pub(crate) fn solve_game(
        &mut self,
        pos: &mut Position,
        max_moves: i32,
        deadline: Instant,
        stop: &AtomicBool,
    ) -> Option<GameMate> {
        let attacker = pos.side_to_move();
        if pos.is_pass_rights_enabled()
            || (max_moves > 0 && pos.game_ply() >= max_moves)
            || pos.pieces(attacker, PieceType::King).count() != 1
            || pos.pieces(!attacker, PieceType::King).count() != 1
            || pos.state_index() + 1 != usize::try_from(pos.game_ply()).ok()?
        {
            return None;
        }
        let mut history = HashMap::new();
        for index in 0..pos.state_index() {
            let state = pos.state_at(index);
            let count = history.entry(state.board_key ^ state.hand_key).or_insert(0u8);
            *count += 1;
            if *count == 4 {
                return None;
            }
        }
        let time_ms = deadline.checked_duration_since(Instant::now())?.as_millis() as u64;
        if time_ms == 0 || stop.load(Ordering::Acquire) {
            return None;
        }
        let limits = MateLimits { nodes: 0, time_ms };
        let MateResult::Mate(pv) = self.solve(pos, &limits, stop, |_| {}) else {
            return None;
        };
        let first = *pv.first()?;
        let mut verifier = Verifier {
            solver: self,
            deadline,
            stop,
            max_moves,
            history,
            moves: Vec::new(),
        };
        let paused = super::EffectsPaused::new(pos);
        let plies = verifier.verify(paused.0, true, 0, Some(first))?;
        verifier.running().then_some(GameMate { first, plies })
    }

    fn mate_entry_length(&self, pos: &Position, mv: Move, attacker: Color) -> Option<u16> {
        match self.mode {
            MateMode::Speed => self.table.probe::<false>(TableKey {
                key: pos.key_after(mv),
                board: 0,
                hand: Hand::EMPTY,
            }),
            MateMode::Power => self.table.probe::<true>(pos.table_key_after(mv, attacker)),
        }
        .filter(|value| value.pn == 0)
        .map(|value| value.mate_len)
    }
}

struct Verifier<'a> {
    solver: &'a DfpnSolver,
    deadline: Instant,
    stop: &'a AtomicBool,
    max_moves: i32,
    history: HashMap<u64, u8>,
    moves: Vec<(u16, Move)>,
}

impl Verifier<'_> {
    fn running(&self) -> bool {
        !self.stop.load(Ordering::Acquire) && Instant::now() < self.deadline
    }

    fn verify(
        &mut self,
        pos: &mut Position,
        attack: bool,
        depth: usize,
        first: Option<Move>,
    ) -> Option<u16> {
        if !self.running()
            || depth > MAX_PLY as usize
            || (self.max_moves > 0 && pos.game_ply() > self.max_moves)
        {
            return None;
        }
        let key = pos.key();
        let count = self.history.entry(key).or_insert(0);
        // 衝突時も疑わしい戦略を棄却するだけで、勝ちの根拠には使わない。
        if *count >= 3 {
            return None;
        }
        *count += 1;
        let start = self.moves.len();
        self.collect_moves(pos, attack, first);
        let verified = self.verify_moves(pos, attack, depth, start);
        self.moves.truncate(start);
        let count = self.history.get_mut(&key).unwrap();
        *count -= 1;
        if *count == 0 {
            self.history.remove(&key);
        }
        verified
    }

    #[inline(never)]
    fn collect_moves(&mut self, pos: &mut Position, attack: bool, first: Option<Move>) {
        let mut moves = MoveList::new();
        generate_legal_all(pos, &mut moves);
        if !attack {
            self.moves.extend(moves.iter().map(|&mv| (0, mv)));
            return;
        }
        let start = self.moves.len();
        let side = pos.side_to_move();
        let immediate = first.is_none().then(|| crate::mate::mate_1ply(pos)).flatten();
        for &mv in moves.iter() {
            if !pos.gives_check(mv) {
                continue;
            }
            let length = if let Some(forced) = first {
                (mv.raw() == forced.raw()).then_some(0)
            } else if immediate.is_some_and(|m| mv.raw() == m.raw()) {
                Some(0)
            } else {
                self.solver.mate_entry_length(pos, mv, side)
            };
            if let Some(length) = length {
                self.moves.push((length, mv));
            }
        }
        self.moves[start..].sort_unstable_by_key(|&(length, mv)| (length, mv.raw()));
    }

    fn verify_moves(
        &mut self,
        pos: &mut Position,
        attack: bool,
        depth: usize,
        start: usize,
    ) -> Option<u16> {
        if !attack && !pos.in_check() {
            return None;
        }
        let mut longest = 0;
        for index in start..self.moves.len() {
            let (_, mv) = self.moves[index];
            pos.do_move(mv, pos.gives_check(mv));
            let result = self.verify(pos, !attack, depth + 1, None);
            pos.undo_move(mv);
            match (attack, result) {
                (true, Some(plies)) => return Some(plies + 1),
                (false, Some(plies)) => longest = longest.max(plies + 1),
                (false, None) => return None,
                (true, None) if !self.running() => return None,
                (true, None) => {}
            }
        }
        (!attack).then_some(longest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const MATE_IN_ONE: &str = "4k4/9/4P4/9/9/9/9/9/4K4 b G 1";

    fn position(sfen: &str) -> Position {
        let mut pos = Position::new();
        pos.set_sfen(sfen).unwrap();
        pos
    }

    #[test]
    fn verifies_game_mate_without_crossing_the_move_limit() {
        let mut pos = position(MATE_IN_ONE);
        let mut solver = DfpnSolver::new(1);
        let deadline = Instant::now() + Duration::from_secs(10);
        let stop = AtomicBool::new(false);
        let result = solver.solve_game(&mut pos, 512, deadline, &stop).unwrap();
        assert_eq!(result.first.to_usi(), "G*5b");
        assert_eq!(result.plies, 1);
        assert_eq!(pos.to_sfen(), MATE_IN_ONE);
        assert!(solver.solve_game(&mut pos, 1, deadline, &stop).is_none());
        stop.store(true, Ordering::Release);
        assert!(solver.solve_game(&mut pos, 512, deadline, &stop).is_none());
    }

    #[test]
    fn verifies_defender_drops_in_game() {
        let mut pos = position("8k/6S2/7G1/9/9/9/9/9/R3K4 b 2p 1");
        let mut solver = DfpnSolver::with_mode(4, MateMode::Speed);
        let deadline = Instant::now() + Duration::from_secs(10);
        let stop = AtomicBool::new(false);
        let result = solver.solve_game(&mut pos, 512, deadline, &stop).unwrap();
        assert!(matches!(result.first.to_usi().as_str(), "9i9a" | "9i9a+"));
        assert!(result.plies >= 5 && result.plies % 2 == 1);
    }

    #[test]
    fn fourth_occurrence_in_the_proof_is_rejected() {
        let mut pos = position(MATE_IN_ONE);
        let solver = DfpnSolver::new(1);
        let mut verifier = Verifier {
            solver: &solver,
            deadline: Instant::now() + Duration::from_secs(10),
            stop: &AtomicBool::new(false),
            max_moves: 512,
            history: HashMap::new(),
            moves: Vec::new(),
        };
        let mv = pos.to_move(Move::from_usi_strict("G*5b").unwrap()).unwrap();
        let child_key = pos.key_after(mv);
        verifier.history.insert(child_key, 3);
        assert!(verifier.verify(&mut pos, true, 0, Some(mv)).is_none());
        assert_eq!(verifier.history, HashMap::from([(child_key, 3)]));
    }
}
