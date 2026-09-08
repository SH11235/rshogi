//! 対局履歴と評価値による終局裁定。
//! core の反復判定は探索用で 2 回目を引分とし、遡りも plies_from_null.min(16) に制限される。
//! ply も探索 root 基準なので、対局の 4 回同一局面判定には再利用しない。

use std::collections::HashMap;
use std::str::FromStr;

use rshogi_core::position::Position;
use rshogi_core::types::Color;
use serde::Serialize;

use super::{EvalLog, GameOutcome};

/// 裁定による勝敗と終局理由。
pub struct Verdict {
    /// 対局の勝敗。
    pub outcome: GameOutcome,
    /// JSONL に記録する終局理由。
    pub reason: &'static str,
}

fn loss(side: Color, reason: &'static str) -> Verdict {
    Verdict {
        outcome: if side == Color::Black {
            GameOutcome::WhiteWin
        } else {
            GameOutcome::BlackWin
        },
        reason,
    }
}

fn position_key(pos: &Position) -> String {
    let sfen = pos.to_sfen();
    let board = sfen.rsplit_once(' ').map_or(sfen.as_str(), |(board, _)| board);
    let rights = if pos.is_pass_rights_enabled() {
        (pos.pass_rights(Color::Black), pos.pass_rights(Color::White))
    } else {
        (0, 0)
    };
    format!("{board} {} {}", rights.0, rights.1)
}

/// 4 回同一局面と連続王手の千日手を判定する。
pub struct RuleAdjudicator {
    history: HashMap<String, (u32, u32)>,
    check_streak: [u32; 2],
}

impl RuleAdjudicator {
    /// 開始局面を対局内 ply=0 の 1 回目として登録する。
    pub fn new(pos: &Position) -> Self {
        Self {
            history: HashMap::from([(position_key(pos), (0, 1))]),
            check_streak: [0; 2],
        }
    }

    /// 合法手適用後の局面を登録する。ply は開始局面からの手数、pass は非王手とする。
    pub fn after_move(
        &mut self,
        pos: &Position,
        mover: Color,
        gives_check: bool,
        ply: u32,
    ) -> Option<Verdict> {
        let streak = &mut self.check_streak[mover as usize];
        *streak = if gives_check {
            streak.saturating_add(1)
        } else {
            0
        };
        let (first, count) = self.history.entry(position_key(pos)).or_insert((ply, 0));
        *count += 1;
        if *count < 4 {
            return None;
        }
        let moves_per_side = (ply - *first) / 2;
        let side = pos.side_to_move();
        for checker in [side, !side] {
            if self.check_streak[checker as usize] >= moves_per_side {
                return Some(loss(checker, "sennichite_perpetual_check"));
            }
        }
        Some(Verdict {
            outcome: GameOutcome::Draw,
            reason: "sennichite",
        })
    }
}

/// 自己視点の劣勢評価が同一側で連続した場合の投了裁定。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ResignRule {
    /// 必要な同一側の連続着手数（1 以上）。
    pub movecount: u32,
    /// 劣勢評価の絶対閾値（非負の cp）。
    pub score: i32,
}

/// 両側を通じて均衡評価が連続した場合の引分裁定。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DrawRule {
    /// 裁定を許可する対局内の手数（ply）。
    pub movenumber: u32,
    /// 必要な連続着手数（1 以上、両側を通算）。
    pub movecount: u32,
    /// 均衡評価の絶対閾値（非負の cp）。
    pub score: i32,
}

fn parse_fields<const N: usize>(input: &str, keys: [&str; N]) -> Result<[u32; N], String> {
    let mut values = [None; N];
    for field in input.split(|c: char| c == ',' || c.is_whitespace()).filter(|s| !s.is_empty()) {
        let (key, value) =
            field.split_once('=').ok_or_else(|| format!("key=value が必要です: {field}"))?;
        let index = keys
            .iter()
            .position(|k| *k == key)
            .ok_or_else(|| format!("未知の key: {key}"))?;
        if values[index].is_some() {
            return Err(format!("重複した key: {key}"));
        }
        let value = value.parse::<u32>().map_err(|_| format!("非負の整数が必要です: {field}"))?;
        if (key == "movecount" && value == 0) || (key == "score" && value > i32::MAX as u32) {
            return Err(format!("値が範囲外です: {field}"));
        }
        values[index] = Some(value);
    }
    let mut result = [0; N];
    for (index, value) in values.into_iter().enumerate() {
        result[index] = value.ok_or_else(|| format!("必須 key がありません: {}", keys[index]))?;
    }
    Ok(result)
}

impl FromStr for ResignRule {
    type Err = String;
    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let [movecount, score] = parse_fields(input, ["movecount", "score"])?;
        Ok(Self {
            movecount,
            score: score as i32,
        })
    }
}

impl FromStr for DrawRule {
    type Err = String;
    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let [movenumber, movecount, score] =
            parse_fields(input, ["movenumber", "movecount", "score"])?;
        Ok(Self {
            movenumber,
            movecount,
            score: score as i32,
        })
    }
}

/// オプトインの評価値裁定。投了、引分の順に判定する。
pub struct ScoreAdjudicator {
    resign: Option<ResignRule>,
    draw: Option<DrawRule>,
    resign_streak: [u32; 2],
    draw_streak: u32,
}

impl ScoreAdjudicator {
    /// None のルールは無効にする。
    pub fn new(resign: Option<ResignRule>, draw: Option<DrawRule>) -> Self {
        Self {
            resign,
            draw,
            resign_streak: [0; 2],
            draw_streak: 0,
        }
    }

    /// 合法手の自己視点評価を記録する。ply は開始局面からの将棋の手数。
    pub fn after_move(
        &mut self,
        mover: Color,
        eval: Option<&EvalLog>,
        ply: u32,
    ) -> Option<Verdict> {
        if let Some(rule) = self.resign {
            let losing = eval.is_some_and(|e| match e.score_mate {
                Some(mate) => mate < 0,
                None => e.score_cp.is_some_and(|cp| i64::from(cp) <= -i64::from(rule.score)),
            });
            let streak = &mut self.resign_streak[mover as usize];
            *streak = if losing { streak.saturating_add(1) } else { 0 };
            if *streak >= rule.movecount {
                return Some(loss(mover, "adjudication_resign"));
            }
        }
        if let Some(rule) = self.draw {
            let balanced = eval.is_some_and(|e| {
                e.score_mate.is_none()
                    && e.score_cp.is_some_and(|cp| i64::from(cp).abs() <= i64::from(rule.score))
            });
            self.draw_streak = if balanced {
                self.draw_streak.saturating_add(1)
            } else {
                0
            };
            if ply >= rule.movenumber && self.draw_streak >= rule.movecount {
                return Some(Verdict {
                    outcome: GameOutcome::Draw,
                    reason: "adjudication_draw",
                });
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rshogi_core::movegen::is_legal_with_pass;
    use rshogi_core::types::Move;

    fn position(sfen: &str) -> Position {
        let mut pos = Position::new();
        pos.set_sfen(sfen).unwrap();
        pos
    }

    fn play(
        pos: &mut Position,
        rules: &mut RuleAdjudicator,
        usi: &str,
        ply: u32,
    ) -> Option<Verdict> {
        let mv = Move::from_usi(usi).unwrap();
        assert!(is_legal_with_pass(pos, mv), "{usi}");
        let mover = pos.side_to_move();
        let check = !mv.is_pass() && pos.gives_check(mv);
        pos.do_move(mv, check);
        rules.after_move(pos, mover, check, ply)
    }

    fn assert_verdict(verdict: Option<Verdict>, outcome: GameOutcome, reason: &str) {
        let verdict = verdict.expect("裁定が必要");
        assert_eq!(verdict.outcome.label(), outcome.label());
        assert_eq!(verdict.reason, reason);
    }

    const HIRATE: &str = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";

    #[test]
    fn fourth_occurrence_includes_start_position() {
        let mut pos = position(HIRATE);
        let mut rules = RuleAdjudicator::new(&pos);
        for ply in 1..=12 {
            let verdict = play(
                &mut pos,
                &mut rules,
                ["3i4h", "7a6b", "4h3i", "6b7a"][(ply as usize - 1) % 4],
                ply,
            );
            if ply < 12 {
                assert!(verdict.is_none(), "ply={ply}");
            } else {
                assert_verdict(verdict, GameOutcome::Draw, "sennichite");
            }
        }
    }

    #[test]
    fn perpetual_checker_loses_on_fourth_occurrence() {
        let mut pos = position("4k4/9/9/9/9/9/9/9/4K4 b R 1");
        let mut rules = RuleAdjudicator::new(&pos);
        assert!(play(&mut pos, &mut rules, "R*5e", 1).is_none());
        for ply in 2..=13 {
            let verdict = play(
                &mut pos,
                &mut rules,
                ["5a4a", "5e4e", "4a5a", "4e5e"][(ply as usize - 2) % 4],
                ply,
            );
            if ply < 13 {
                assert!(verdict.is_none(), "ply={ply}");
            } else {
                assert_verdict(verdict, GameOutcome::WhiteWin, "sennichite_perpetual_check");
            }
        }
    }

    #[test]
    fn non_repeating_moves_do_not_end_game() {
        let mut pos = position(HIRATE);
        let mut rules = RuleAdjudicator::new(&pos);
        for (index, usi) in ["7g7f", "3c3d", "2g2f", "8c8d", "2f2e", "8d8e"].into_iter().enumerate()
        {
            assert!(play(&mut pos, &mut rules, usi, index as u32 + 1).is_none());
        }
    }

    #[test]
    fn key_ignores_move_number_but_includes_board_turn_hands_and_pass_rights() {
        let pos = position(HIRATE);
        assert_eq!(position_key(&pos), position_key(&position(&HIRATE.replace(" 1", " 123"))));
        for sfen in [
            HIRATE.replace(" b ", " w "),
            HIRATE.replace("PPPPPPPPP", "1PPPPPPPP"),
        ] {
            assert_ne!(position_key(&pos), position_key(&position(&sfen)));
        }
        let sparse = HIRATE.replace("PPPPPPPPP", "1PPPPPPPP");
        assert_ne!(
            position_key(&position(&sparse)),
            position_key(&position(&sparse.replace(" - ", " P ")))
        );
        let mut pass_pos = position(HIRATE);
        for (black, white) in [(1, 0), (0, 1)] {
            pass_pos.enable_pass_rights(black, white);
            assert_ne!(position_key(&pos), position_key(&pass_pos));
        }
        pass_pos.set_pass_rights_enabled(false);
        assert_eq!(position_key(&pos), position_key(&pass_pos));
    }

    #[test]
    fn pass_resets_check_streak_and_changes_position_identity() {
        let mut pos = position(HIRATE);
        pos.enable_pass_rights(4, 4);
        let mut rules = RuleAdjudicator::new(&pos);
        rules.check_streak = [10, 10];
        for ply in 1..=8 {
            assert!(play(&mut pos, &mut rules, "pass", ply).is_none());
        }
        assert_eq!(rules.check_streak, [0, 0]);
    }

    #[test]
    fn both_checkers_tie_break_loses_side_to_move() {
        let pos = position(HIRATE);
        let mut rules = RuleAdjudicator::new(&pos);
        rules.history.insert(position_key(&pos), (0, 3));
        rules.check_streak = [6, 6];
        assert_verdict(
            rules.after_move(&pos, Color::White, true, 12),
            GameOutcome::WhiteWin,
            "sennichite_perpetual_check",
        );
    }

    fn cp(value: i32) -> EvalLog {
        EvalLog {
            score_cp: Some(value),
            ..EvalLog::default()
        }
    }
    fn mate(value: i32) -> EvalLog {
        EvalLog {
            score_mate: Some(value),
            ..EvalLog::default()
        }
    }
    fn resign() -> ScoreAdjudicator {
        ScoreAdjudicator::new(
            Some(ResignRule {
                movecount: 3,
                score: 600,
            }),
            None,
        )
    }

    #[test]
    fn resign_counts_each_side_independently() {
        for loser in [Color::Black, Color::White] {
            let mut scores = resign();
            for ply in 1..=5 {
                let mover = if ply % 2 == 1 { loser } else { !loser };
                let verdict = scores.after_move(mover, Some(&cp(-600)), ply);
                if ply < 5 {
                    assert!(verdict.is_none());
                } else {
                    assert_verdict(verdict, loss(loser, "").outcome, "adjudication_resign");
                }
            }
        }
    }

    #[test]
    fn resign_resets_on_recovery_missing_score_and_positive_mate() {
        for reset in [
            Some(cp(-599)),
            None,
            Some(EvalLog::default()),
            Some(mate(1)),
        ] {
            let mut scores = resign();
            for ply in 1..=2 {
                assert!(scores.after_move(Color::Black, Some(&cp(-700)), ply).is_none());
            }
            assert!(scores.after_move(Color::Black, reset.as_ref(), 3).is_none());
            for ply in 4..=5 {
                assert!(scores.after_move(Color::Black, Some(&mate(-1)), ply).is_none());
            }
            assert_verdict(
                scores.after_move(Color::Black, Some(&mate(-1)), 6),
                GameOutcome::WhiteWin,
                "adjudication_resign",
            );
        }
    }

    #[test]
    fn draw_counts_plies_and_resets_on_mate_missing_or_unbalanced_score() {
        let rule = DrawRule {
            movenumber: 6,
            movecount: 3,
            score: 20,
        };
        let mut scores = ScoreAdjudicator::new(None, Some(rule));
        for ply in 1..6 {
            assert!(scores.after_move(Color::Black, Some(&cp(0)), ply).is_none());
        }
        assert_verdict(
            scores.after_move(Color::White, Some(&cp(0)), 6),
            GameOutcome::Draw,
            "adjudication_draw",
        );
        for reset in [
            Some(mate(-1)),
            Some(mate(1)),
            Some(cp(21)),
            Some(cp(i32::MIN)),
            None,
            Some(EvalLog::default()),
        ] {
            let mut scores = ScoreAdjudicator::new(None, Some(rule));
            for ply in 6..8 {
                assert!(scores.after_move(Color::Black, Some(&cp(20)), ply).is_none());
            }
            assert!(scores.after_move(Color::White, reset.as_ref(), 8).is_none());
            assert!(scores.after_move(Color::Black, Some(&cp(-20)), 9).is_none());
            assert!(scores.after_move(Color::White, Some(&cp(20)), 10).is_none());
            assert_verdict(
                scores.after_move(Color::Black, Some(&cp(0)), 11),
                GameOutcome::Draw,
                "adjudication_draw",
            );
        }
    }

    #[test]
    fn score_rules_default_off_and_resign_precedes_draw() {
        let mut scores = ScoreAdjudicator::new(None, None);
        for ply in 1..=20 {
            assert!(scores.after_move(Color::Black, Some(&mate(-1)), ply).is_none());
        }
        let mut scores = ScoreAdjudicator::new(
            Some(ResignRule {
                movecount: 1,
                score: 0,
            }),
            Some(DrawRule {
                movenumber: 0,
                movecount: 1,
                score: 20,
            }),
        );
        assert_verdict(
            scores.after_move(Color::Black, Some(&cp(0)), 1),
            GameOutcome::WhiteWin,
            "adjudication_resign",
        );
    }
}
