//! DL USI エンジンの局面読み込みと探索上限。

use anyhow::{Result, bail, ensure};
use rshogi_core::movegen::{MoveList, generate_legal_all};
use rshogi_core::position::Position;
use rshogi_core::types::{Color, Move, PieceType};
use rshogi_mcts::{History, Limits};
use std::time::Duration;

/// position コマンドを検証し、開始局面からの履歴ごと構築する。
pub fn parse_position(command: &str) -> Result<(Position, History)> {
    let tokens: Vec<_> = command.split_whitespace().collect();
    ensure!(tokens.first() == Some(&"position"), "expected position command");
    let mut pos = Position::new();
    let mut index = match tokens.get(1) {
        Some(&"startpos") => {
            pos.set_hirate();
            2
        }
        Some(&"sfen") => {
            ensure!(tokens.len() >= 6, "incomplete SFEN");
            pos.set_sfen(&tokens[2..6].join(" "))?;
            6
        }
        _ => bail!("expected startpos or sfen"),
    };
    for side in [Color::Black, Color::White] {
        ensure!(pos.pieces(side, PieceType::King).count() == 1, "one king per side is required");
    }
    ensure!(
        pos.attackers_to_c(pos.king_square(!pos.side_to_move()), pos.side_to_move())
            .is_empty(),
        "non-moving side must not be in check"
    );
    let mut history = History::new(&pos);
    if index < tokens.len() {
        ensure!(tokens[index] == "moves", "unexpected position suffix");
        index += 1;
    }
    for text in &tokens[index..] {
        let parsed = Move::from_usi(text).ok_or_else(|| anyhow::anyhow!("invalid move {text}"))?;
        let mv = pos.to_move(parsed).ok_or_else(|| anyhow::anyhow!("invalid move {text}"))?;
        let mut legal = MoveList::new();
        generate_legal_all(&pos, &mut legal);
        ensure!(legal.contains(mv), "illegal move {text}");
        pos.do_move(mv, pos.gives_check(mv));
        history.push(&pos);
    }
    Ok((pos, history))
}

/// go コマンドで指定された時間と simulation 上限。
#[derive(Debug, Default)]
pub struct Go {
    nodes: Option<u64>,
    movetime: Option<u64>,
    black_time: Option<u64>,
    white_time: Option<u64>,
    black_increment: u64,
    white_increment: u64,
    byoyomi: u64,
    /// infinite では、木が満杯でも stop まで bestmove を返さない。
    pub infinite: bool,
}

impl Go {
    /// 対応外の制限を黙って無視せず、明示的に拒否する。
    pub fn parse(command: &str) -> Result<Self> {
        let mut tokens = command.split_whitespace();
        ensure!(tokens.next() == Some("go"), "expected go command");
        let mut go = Self::default();
        while let Some(name) = tokens.next() {
            if name == "infinite" {
                go.infinite = true;
                continue;
            }
            ensure!(
                [
                    "nodes", "movetime", "btime", "wtime", "binc", "winc", "byoyomi"
                ]
                .contains(&name),
                "unsupported go parameter {name}"
            );
            let value = tokens
                .next()
                .ok_or_else(|| anyhow::anyhow!("missing {name} value"))?
                .parse::<u64>()?;
            match name {
                "nodes" => go.nodes = Some(value),
                "movetime" => go.movetime = Some(value),
                "btime" => go.black_time = Some(value),
                "wtime" => go.white_time = Some(value),
                "binc" => go.black_increment = value,
                "winc" => go.white_increment = value,
                "byoyomi" => go.byoyomi = value,
                _ => unreachable!(),
            }
        }
        ensure!(
            !go.infinite
                || (go.nodes.is_none()
                    && go.movetime.is_none()
                    && go.black_time.is_none()
                    && go.white_time.is_none()
                    && go.byoyomi == 0),
            "infinite cannot be combined with other limits"
        );
        Ok(go)
    }

    /// 残り時計の三十分の一と加算を基準に、手元の時計を超えない予算を返す。
    pub fn limits(&self, side: Color, overhead_ms: u64) -> Limits {
        let (remaining, increment) = if side == Color::Black {
            (self.black_time, self.black_increment)
        } else {
            (self.white_time, self.white_increment)
        };
        let budget = if self.infinite {
            None
        } else if let Some(time) = self.movetime {
            Some(time)
        } else if remaining.is_some() || self.byoyomi > 0 {
            let remaining = remaining.unwrap_or(0);
            Some(
                (remaining / 30)
                    .saturating_add(increment)
                    .saturating_add(self.byoyomi)
                    .min(remaining.saturating_add(self.byoyomi)),
            )
        } else {
            None
        };
        Limits {
            visits: self.nodes.unwrap_or(if self.infinite || budget.is_some() {
                u64::MAX
            } else {
                128
            }),
            time: budget.map(|ms| Duration::from_millis(ms.saturating_sub(overhead_ms))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn position_replay_keeps_repetition_history() -> Result<()> {
        let (_, history) = parse_position(
            "position sfen 4k4/9/9/9/9/9/9/9/4K4 b - 1 moves 5i6i 5a6a 6i5i 6a5a 5i6i 5a6a 6i5i 6a5a 5i6i 5a6a 6i5i 6a5a",
        )?;
        assert_eq!(history.outcome(), Some(rshogi_mcts::Outcome::Draw));
        assert!(parse_position("position startpos moves 7g7e").is_err());
        assert!(parse_position("position startpos nonsense").is_err());
        assert!(parse_position("position sfen 9/9/9/9/9/9/9/9/9 b - 1").is_err());
        Ok(())
    }
    #[test]
    fn time_budget_uses_side_clock_and_respects_hard_limit() -> Result<()> {
        let go = Go::parse("go btime 30000 wtime 3000 binc 600 winc 100")?;
        assert_eq!(go.limits(Color::Black, 50).time, Some(Duration::from_millis(1550)));
        assert_eq!(go.limits(Color::White, 50).time, Some(Duration::from_millis(150)));
        let go = Go::parse("go btime 1 wtime 0 binc 600 winc 600")?;
        assert_eq!(go.limits(Color::Black, 50).time, Some(Duration::ZERO));
        assert_eq!(
            Go::parse("go byoyomi 1000")?.limits(Color::Black, 50).time,
            Some(Duration::from_millis(950))
        );
        assert!(Go::parse("go ponder").is_err());
        assert!(Go::parse("go depth 5").is_err());
        assert!(Go::parse("go nodes").is_err());
        Ok(())
    }
}
