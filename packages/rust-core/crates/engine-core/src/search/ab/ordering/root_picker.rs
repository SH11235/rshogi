use crate::search::params::{
    root_multipv_bonus, root_prev_score_scale, root_tt_bonus, ROOT_BASE_KEY, ROOT_PREV_SCORE_CLAMP,
};
use crate::search::types::RootLine;
use crate::shogi::Move;
use crate::Position;

#[derive(Clone, Copy)]
struct RootScoredMove {
    mv: Move,
    key: i32,
    order: usize,
}

pub struct RootPicker {
    scored: Vec<RootScoredMove>,
    cursor: usize,
}

impl RootPicker {
    pub fn new(
        pos: &Position,
        moves: &[Move],
        tt_move: Option<Move>,
        prev_lines: Option<&[RootLine]>,
    ) -> Self {
        let mut scored = Vec::with_capacity(moves.len());
        for (idx, &mv) in moves.iter().enumerate() {
            let is_check = pos.gives_check(mv) as i32;
            let see = if mv.is_capture_hint() { pos.see(mv) } else { 0 };
            let is_promo = mv.is_promote() as i32;
            let good_capture = mv.is_capture_hint() && see >= 0;

            let mut key = ROOT_BASE_KEY;
            // チェック/成りは “基礎 + 追加” の二段加点で強調している（既存順位との互換性保持）。
            key += is_check * 2_000 + see * 10 + is_promo;
            key += 500 * is_check + 300 * is_promo + 200 * (good_capture as i32);

            if let Some(ttm) = tt_move {
                if mv.equals_without_piece_type(&ttm) {
                    key += root_tt_bonus();
                }
            }

            if let Some(prev) = prev_lines.and_then(|lines| {
                lines.iter().find(|line| line.root_move.equals_without_piece_type(&mv))
            }) {
                let clamped = prev.score_cp.clamp(-ROOT_PREV_SCORE_CLAMP, ROOT_PREV_SCORE_CLAMP);
                key += root_prev_score_scale() * clamped;
                key += root_multipv_bonus(prev.multipv_index);
            }

            let scored_move = RootScoredMove {
                mv,
                key,
                order: idx,
            };
            scored.push(scored_move);
        }

        scored.sort_unstable_by(|a, b| b.key.cmp(&a.key).then_with(|| a.order.cmp(&b.order)));

        Self { scored, cursor: 0 }
    }

    pub fn next(&mut self) -> Option<(Move, i32)> {
        if self.cursor >= self.scored.len() {
            return None;
        }
        let entry = self.scored[self.cursor];
        self.cursor += 1;
        Some((entry.mv, entry.key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::types::{Bound, RootLine};
    use crate::usi::parse_usi_move;
    use crate::Position;
    use smallvec::smallvec;

    fn make_root_line(index: u8, mv: Move, score_cp: i32) -> RootLine {
        RootLine {
            multipv_index: index,
            root_move: mv,
            score_internal: score_cp,
            score_cp,
            bound: Bound::Exact,
            depth: 1,
            seldepth: None,
            pv: smallvec![mv],
            nodes: None,
            time_ms: None,
            nps: None,
            exact_exhausted: false,
            exhaust_reason: None,
            mate_distance: None,
        }
    }

    #[test]
    fn tt_move_priority() {
        let pos = Position::startpos();
        let moves = [
            parse_usi_move("7g7f").unwrap(),
            parse_usi_move("2g2f").unwrap(),
        ];
        let tt_move = parse_usi_move("2g2f").expect("valid tt move");
        let mut picker = RootPicker::new(&pos, &moves, Some(tt_move), None);
        let first = picker.next().unwrap();
        assert!(first.0.equals_without_piece_type(&tt_move));
    }

    #[test]
    fn prev_score_boosts_move() {
        let pos = Position::startpos();
        let mv_a = parse_usi_move("7g7f").unwrap();
        let mv_b = parse_usi_move("2g2f").unwrap();
        let moves = [mv_a, mv_b];
        let prev_lines = [make_root_line(1, mv_b, 150), make_root_line(2, mv_a, -50)];
        let mut picker = RootPicker::new(&pos, &moves, None, Some(&prev_lines));
        let first = picker.next().unwrap();
        assert!(first.0.equals_without_piece_type(&mv_b));
    }

    #[test]
    fn multipv_rank_breaks_ties() {
        let pos = Position::startpos();
        let mv_a = parse_usi_move("7g7f").unwrap();
        let mv_b = parse_usi_move("2g2f").unwrap();
        let moves = [mv_a, mv_b];
        let prev_lines = [make_root_line(2, mv_a, 0), make_root_line(1, mv_b, 0)];
        let mut picker = RootPicker::new(&pos, &moves, None, Some(&prev_lines));
        let first = picker.next().unwrap();
        assert!(first.0.equals_without_piece_type(&mv_b));
    }
}
