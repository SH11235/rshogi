use rshogi_core::position::Position;
use rshogi_core::types::Color;

use crate::Outcome;

#[derive(Clone)]
struct Entry {
    key: u64,
    side: Color,
    in_check: bool,
}

/// 開始局面以後の手順を保持する、終局判定用の履歴。
#[derive(Clone)]
pub struct History {
    entries: Vec<Entry>,
}

impl History {
    /// 開始局面を記録する。開始局面以前の手順は推測しない。
    pub fn new(position: &Position) -> Self {
        let mut history = Self {
            entries: Vec::new(),
        };
        history.push(position);
        history
    }

    /// 合法手を実行した後の局面を記録する。
    pub fn push(&mut self, position: &Position) {
        self.entries.push(Entry {
            key: position.key(),
            side: position.side_to_move(),
            in_check: position.in_check(),
        });
    }

    pub(crate) fn reserve(&mut self, plies: usize) -> anyhow::Result<()> {
        self.entries.try_reserve_exact(plies)?;
        Ok(())
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn truncate(&mut self, len: usize) {
        self.entries.truncate(len);
    }

    pub(crate) fn matches(&self, position: &Position) -> bool {
        self.entries.last().is_some_and(|e| e.key == position.key())
    }

    /// 四回同一局面と連続王手の反則を、現在の手番の視点で返す。
    pub fn outcome(&self) -> Option<Outcome> {
        let current = self.entries.last()?;
        let mut count = 0;
        let mut first = 0;
        for (index, entry) in self.entries.iter().enumerate().rev() {
            if entry.key == current.key {
                count += 1;
                if count == 4 {
                    first = index;
                    break;
                }
            }
        }
        if count < 4 {
            return None;
        }
        let mut continuous = [true; 2];
        for entry in &self.entries[first + 1..] {
            continuous[(!entry.side).index()] &= entry.in_check;
        }
        if continuous[current.side.index()] {
            Some(Outcome::Loss)
        } else if continuous[(!current.side).index()] {
            Some(Outcome::Win)
        } else {
            Some(Outcome::Draw)
        }
    }
}
