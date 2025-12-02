//! Skill Level (強さ制限・手加減) 機能
//!
//! Stockfish/YaneuraOu の Skill を移植したもの。

use rand::Rng;

use crate::types::{Depth, Move, Value};

use super::RootMoves;

/// Skill 関連のオプション（USI setoption から受け取る値を格納）
#[derive(Clone, Copy, Debug)]
pub struct SkillOptions {
    /// 0..20。20 以上は手加減なし。
    pub skill_level: i32,
    /// UCI_LimitStrength を有効にするか
    pub uci_limit_strength: bool,
    /// UCI_Elo の値（0 のときは未指定）
    pub uci_elo: i32,
}

impl Default for SkillOptions {
    fn default() -> Self {
        Self {
            skill_level: 20,
            uci_limit_strength: false,
            uci_elo: 0,
        }
    }
}

/// Skill 計算用の内部状態
#[derive(Clone, Debug)]
pub struct Skill {
    level: f64,
    pub best: Move,
}

impl Skill {
    /// オプションから Skill を生成
    pub fn from_options(opts: &SkillOptions) -> Self {
        // Stockfish の近似多項式をそのまま移植
        const LOWEST_ELO: i32 = 1320;
        const HIGHEST_ELO: i32 = 3190;

        let level = if opts.uci_limit_strength && opts.uci_elo != 0 {
            let e = (opts.uci_elo - LOWEST_ELO) as f64 / (HIGHEST_ELO - LOWEST_ELO) as f64;
            (((37.2473 * e - 40.8525) * e + 22.2943) * e - 0.311438).clamp(0.0, 19.0)
        } else {
            opts.skill_level as f64
        };

        Self {
            level,
            best: Move::NONE,
        }
    }

    /// Skill が有効か（20 未満で手加減が入る）
    pub fn enabled(&self) -> bool {
        self.level < 20.0
    }

    /// depth が SkillLevel 相当の深さに到達したか（Stockfish 準拠）
    pub fn time_to_pick(&self, depth: Depth) -> bool {
        depth == 1 + self.level as Depth
    }

    /// 上位 MultiPV から「弱さ」に応じた手を選ぶ
    pub fn pick_best<R: Rng + ?Sized>(
        &mut self,
        root_moves: &RootMoves,
        multi_pv: usize,
        rng: &mut R,
    ) -> Move {
        // RootMoves は降順ソート済み前提
        if root_moves.is_empty() || multi_pv == 0 {
            return Move::NONE;
        }

        let capped_multi_pv = multi_pv.min(root_moves.len());
        let top_score = root_moves[0].score.raw();
        let last_score = root_moves[capped_multi_pv - 1].score.raw();

        // Stockfish 相当: delta は 100cp（PawnValue）まで
        let delta = (top_score - last_score).min(100);
        let weakness = 120.0 - 2.0 * self.level;
        let weakness_int = weakness.max(1.0) as u32;

        let mut max_score = Value::new(-32001).raw();
        let mut best_move = root_moves[0].mv();

        for rm in root_moves.iter().take(capped_multi_pv) {
            let rand_term = rng.random::<u32>() % weakness_int;
            let push = ((weakness * (top_score - rm.score.raw()) as f64)
                + delta as f64 * rand_term as f64)
                / 128.0;
            let candidate = rm.score.raw() + push as i32;

            if candidate >= max_score {
                max_score = candidate;
                best_move = rm.mv();
            }
        }

        self.best = best_move;
        best_move
    }
}

#[cfg(test)]
mod tests {
    use rand::RngCore;

    use crate::search::RootMove;
    use crate::types::Move;

    use super::*;

    #[derive(Clone)]
    struct FixedSeqRng {
        data: Vec<u32>,
        idx: usize,
    }

    impl FixedSeqRng {
        fn new(seq: &[u32]) -> Self {
            Self {
                data: seq.to_vec(),
                idx: 0,
            }
        }

        fn next_val(&mut self) -> u32 {
            let v = self.data.get(self.idx).copied().unwrap_or(0);
            self.idx = (self.idx + 1) % self.data.len().max(1);
            v
        }
    }

    impl RngCore for FixedSeqRng {
        fn next_u32(&mut self) -> u32 {
            self.next_val()
        }

        fn next_u64(&mut self) -> u64 {
            self.next_val() as u64
        }

        fn fill_bytes(&mut self, dest: &mut [u8]) {
            for chunk in dest.chunks_mut(8) {
                let bytes = self.next_u64().to_le_bytes();
                let len = chunk.len().min(8);
                chunk[..len].copy_from_slice(&bytes[..len]);
            }
        }
    }

    #[test]
    fn skill_enabled_flag() {
        let s = Skill::from_options(&SkillOptions {
            skill_level: 10,
            ..Default::default()
        });
        assert!(s.enabled());

        let s = Skill::from_options(&SkillOptions {
            skill_level: 20,
            ..Default::default()
        });
        assert!(!s.enabled());
    }

    #[test]
    fn pick_best_prefers_weaker_move_with_high_weakness() {
        // 固定乱数（常に 119）で、上位以外を選ぶケース
        let mut rng = FixedSeqRng::new(&[0, 119, 119, 119]); // 先頭手だけ乱数0、以降は119
        let mut skill = Skill::from_options(&SkillOptions {
            skill_level: 0,
            ..Default::default()
        });

        let root_moves = RootMoves::from_vec(
            vec![
                (300, "7g7f"),
                (50, "2g2f"), // これを選んでほしい
                (0, "3g3f"),
                (-50, "8h7g"),
            ]
            .into_iter()
            .map(|(score, mv)| {
                let mut rm = RootMove::new(Move::from_usi(mv).unwrap());
                rm.score = Value::new(score);
                rm
            })
            .collect(),
        );

        let best = skill.pick_best(&root_moves, 4, &mut rng);
        assert_eq!(best, Move::from_usi("2g2f").unwrap());
    }
}
