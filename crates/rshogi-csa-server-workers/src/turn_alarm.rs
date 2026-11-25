use std::time::Duration;

use crate::reconnect::PendingAlarmKind;

/// Cloudflare Alarm API のジッタを吸収するための発火猶予（ミリ秒）。
pub(super) const ALARM_SAFETY_MS: u64 = 200;

pub(super) fn turn_alarm_delay(deadline_ms: Option<u64>, now_ms: u64) -> Option<Duration> {
    let deadline_ms = deadline_ms?;
    Some(Duration::from_millis(
        deadline_ms.saturating_add(ALARM_SAFETY_MS).saturating_sub(now_ms),
    ))
}

pub(super) fn earliest_turn_or_grace_alarm(
    turn_deadline_ms: u64,
    grace_deadline_ms: u64,
) -> (PendingAlarmKind, u64) {
    if turn_deadline_ms <= grace_deadline_ms {
        (PendingAlarmKind::TimeUp, turn_deadline_ms)
    } else {
        (PendingAlarmKind::GraceExpired, grace_deadline_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unarmed_turn_and_grace_choose_the_earlier_deadline() {
        for (turn, grace, kind, deadline) in [
            (300_000, 30_000, PendingAlarmKind::GraceExpired, 30_000),
            (2_000, 30_000, PendingAlarmKind::TimeUp, 2_000),
            (30_000, 30_000, PendingAlarmKind::TimeUp, 30_000),
        ] {
            let (actual_kind, actual_deadline) = earliest_turn_or_grace_alarm(turn, grace);
            assert_eq!(actual_kind, kind);
            assert_eq!(actual_deadline, deadline);
            assert_eq!(
                turn_alarm_delay(Some(actual_deadline), 1_000),
                Some(Duration::from_millis(deadline - 1_000 + 200))
            );
        }
    }

    #[test]
    fn alarm_delay_excludes_time_already_spent_before_scheduling() {
        let deadline_ms = Some(10_000);
        assert_eq!(turn_alarm_delay(deadline_ms, 8_000), Some(Duration::from_millis(2_200)));
        assert_eq!(turn_alarm_delay(deadline_ms, 8_500), Some(Duration::from_millis(1_700)));
        assert_eq!(turn_alarm_delay(deadline_ms, 10_100), Some(Duration::from_millis(100)));
        assert_eq!(turn_alarm_delay(deadline_ms, 10_201), Some(Duration::ZERO));
        assert_eq!(turn_alarm_delay(None, 8_000), None);
    }
}
