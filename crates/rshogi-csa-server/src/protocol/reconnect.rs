//! 再接続時の現手番予算の共有 wire 契約。

/// 送信時点で現手番に残された予算。主時間台帳とは独立した上限。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconnectClock {
    /// 経過時間を控除し、再接続補償を反映した残りミリ秒。
    pub remaining_ms: u64,
}

impl ReconnectClock {
    const PREFIX: &str = "Current_Turn_Remaining_Ms:";

    /// TCP / Workers 共通の拡張行へ変換する。
    pub fn to_line(self) -> String {
        format!("{}{t}", Self::PREFIX, t = self.remaining_ms)
    }

    /// 対象外の行は `None`、対象行の不正な値はエラーにする。
    pub fn parse_line(line: &str) -> Result<Option<Self>, std::num::ParseIntError> {
        line.strip_prefix(Self::PREFIX)
            .map(|value| value.trim().parse().map(|remaining_ms| Self { remaining_ms }))
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remaining_clock_wire_contract() {
        for value in [0, 1234, u64::MAX] {
            let clock = ReconnectClock {
                remaining_ms: value,
            };
            assert_eq!(ReconnectClock::parse_line(&clock.to_line()).unwrap(), Some(clock));
        }
        assert_eq!(ReconnectClock::parse_line("Last_Move:+7776FU").unwrap(), None);
        assert!(ReconnectClock::parse_line("Current_Turn_Remaining_Ms:-1").is_err());
        assert!(
            ReconnectClock::parse_line("Current_Turn_Remaining_Ms:18446744073709551616").is_err()
        );
    }
}
