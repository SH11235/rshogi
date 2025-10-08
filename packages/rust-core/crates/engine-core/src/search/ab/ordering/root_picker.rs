use rand::{rngs::SmallRng, Rng, SeedableRng};

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
    primary: Vec<usize>,
    fallback: Vec<usize>,
    primary_cursor: usize,
    fallback_cursor: usize,
}

#[derive(Clone, Copy)]
pub struct RootJitter {
    pub seed: u64,
    pub amplitude: i32,
}

impl RootJitter {
    pub const fn new(seed: u64, amplitude: i32) -> Self {
        Self { seed, amplitude }
    }
}

pub struct RootPickerConfig<'a> {
    pub pos: &'a Position,
    pub moves: &'a [Move],
    pub tt_move: Option<Move>,
    pub prev_lines: Option<&'a [RootLine]>,
    pub jitter: Option<RootJitter>,
}

impl RootPicker {
    pub fn new(config: RootPickerConfig) -> Self {
        let RootPickerConfig {
            pos,
            moves,
            tt_move,
            prev_lines,
            jitter,
        } = config;
        let (mut rng_opt, jitter_amplitude) = match jitter {
            Some(cfg) if cfg.amplitude != 0 => {
                (Some(SmallRng::seed_from_u64(cfg.seed)), cfg.amplitude.abs())
            }
            _ => (None, 0),
        };
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

            if let Some(rng) = rng_opt.as_mut() {
                if jitter_amplitude > 0 && !mv.is_capture_hint() && is_check == 0 {
                    let jitter = rng.random_range(-jitter_amplitude..=jitter_amplitude);
                    key = key.saturating_add(jitter);
                }
            }

            let scored_move = RootScoredMove {
                mv,
                key,
                order: idx,
            };
            scored.push(scored_move);
        }

        scored.sort_unstable_by(|a, b| b.key.cmp(&a.key).then_with(|| a.order.cmp(&b.order)));

        let len = scored.len();
        let primary = Vec::new();
        let mut fallback = Vec::with_capacity(len);
        for entry in &scored {
            fallback.push(entry.order);
        }

        Self {
            scored,
            primary,
            fallback,
            primary_cursor: 0,
            fallback_cursor: 0,
        }
    }

    pub fn next(&mut self) -> Option<(Move, i32, usize)> {
        if self.primary_cursor < self.primary.len() {
            let idx = self.primary[self.primary_cursor];
            self.primary_cursor += 1;
            return self.entry_at(idx);
        }
        if self.fallback_cursor < self.fallback.len() {
            let idx = self.fallback[self.fallback_cursor];
            self.fallback_cursor += 1;
            return self.entry_at(idx);
        }
        None
    }

    fn entry_at(&self, idx: usize) -> Option<(Move, i32, usize)> {
        self.scored
            .iter()
            .find(|entry| entry.order == idx)
            .map(|entry| (entry.mv, entry.key, entry.order))
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
        let mut picker = RootPicker::new(RootPickerConfig {
            pos: &pos,
            moves: &moves,
            tt_move: Some(tt_move),
            prev_lines: None,
            jitter: None,
        });
        let (mv, _, _) = picker.next().unwrap();
        assert!(mv.equals_without_piece_type(&tt_move));
    }

    #[test]
    fn prev_score_boosts_move() {
        let pos = Position::startpos();
        let mv_a = parse_usi_move("7g7f").unwrap();
        let mv_b = parse_usi_move("2g2f").unwrap();
        let moves = [mv_a, mv_b];
        let prev_lines = [make_root_line(1, mv_b, 150), make_root_line(2, mv_a, -50)];
        let mut picker = RootPicker::new(RootPickerConfig {
            pos: &pos,
            moves: &moves,
            tt_move: None,
            prev_lines: Some(&prev_lines),
            jitter: None,
        });
        let (mv, _, _) = picker.next().unwrap();
        assert!(mv.equals_without_piece_type(&mv_b));
    }

    #[test]
    fn multipv_rank_breaks_ties() {
        let pos = Position::startpos();
        let mv_a = parse_usi_move("7g7f").unwrap();
        let mv_b = parse_usi_move("2g2f").unwrap();
        let moves = [mv_a, mv_b];
        let prev_lines = [make_root_line(2, mv_a, 0), make_root_line(1, mv_b, 0)];
        let mut picker = RootPicker::new(RootPickerConfig {
            pos: &pos,
            moves: &moves,
            tt_move: None,
            prev_lines: Some(&prev_lines),
            jitter: None,
        });
        let (mv, _, _) = picker.next().unwrap();
        assert!(mv.equals_without_piece_type(&mv_b));
    }
}
