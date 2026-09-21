//! TTごとの定数サイズ診断。通常ビルドではモジュールごと除外する。
//! 本診断のpayload採用/保持は、moveを除くkey/depth/gen/bound/value/evalの保存判断を指す。
use super::TTEntry;
use std::sync::atomic::{AtomicU64, Ordering};

/// TT書込の累積観測。更新中のsnapshotは複数項目を同時には読まない。
/// full key衝突や並行格納の最終勝者は判別できない。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TTWriteStats {
    /// 格納操作の数。並行writerに上書きされない保証ではない。
    pub attempts: u64,
    /// 再load時に空のslot。
    pub empty: u64,
    /// 使用中slotの短縮キーが一致。full key一致とは限らない。
    pub same_key16: u64,
    /// 使用中slotの短縮キーが不一致。
    pub different_key16: u64,
    /// 置換条件を満たした数。保存値が同一でも数える。
    pub payload_accepted: u64,
    /// 置換条件を満たさず旧payloadを保持した数。
    pub payload_retained: u64,
    /// 使用中の同key16・同depthでpayloadを採用した数。
    pub same_key16_same_depth_accepted: u64,
    /// payload保持時に指し手だけ変わった数。
    pub move_only_changed: u64,
    /// payload保持時に指し手も変わらなかった数。
    pub unchanged: u64,
}

impl TTWriteStats {
    /// 同じTTの以前のsnapshotからの差分。カウンタのu64周回を扱う。
    pub fn since(self, previous: Self) -> Self {
        Self {
            attempts: self.attempts.wrapping_sub(previous.attempts),
            empty: self.empty.wrapping_sub(previous.empty),
            same_key16: self.same_key16.wrapping_sub(previous.same_key16),
            different_key16: self.different_key16.wrapping_sub(previous.different_key16),
            payload_accepted: self.payload_accepted.wrapping_sub(previous.payload_accepted),
            payload_retained: self.payload_retained.wrapping_sub(previous.payload_retained),
            same_key16_same_depth_accepted: self
                .same_key16_same_depth_accepted
                .wrapping_sub(previous.same_key16_same_depth_accepted),
            move_only_changed: self.move_only_changed.wrapping_sub(previous.move_only_changed),
            unchanged: self.unchanged.wrapping_sub(previous.unchanged),
        }
    }
}

impl std::fmt::Display for TTWriteStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "attempts={} empty={} same_key16={} different_key16={} payload_accepted={} payload_retained={} same_key16_same_depth_accepted={} move_only_changed={} unchanged={}",
            self.attempts,
            self.empty,
            self.same_key16,
            self.different_key16,
            self.payload_accepted,
            self.payload_retained,
            self.same_key16_same_depth_accepted,
            self.move_only_changed,
            self.unchanged
        )
    }
}

pub(super) struct WriteCounters([AtomicU64; 9]);
impl WriteCounters {
    pub(super) const fn new() -> Self {
        Self([const { AtomicU64::new(0) }; 9])
    }
    pub(super) fn record(
        &self,
        before: TTEntry,
        after: TTEntry,
        key: u64,
        depth: i32,
        accepted: bool,
    ) {
        self.0[0].fetch_add(1, Ordering::Relaxed);
        let relation = if !before.is_occupied() {
            1
        } else if before.key16() == key as u16 {
            2
        } else {
            3
        };
        self.0[relation].fetch_add(1, Ordering::Relaxed);
        self.0[if accepted { 4 } else { 5 }].fetch_add(1, Ordering::Relaxed);
        if accepted {
            if relation == 2 && before.depth() == depth {
                self.0[6].fetch_add(1, Ordering::Relaxed);
            }
        } else {
            self.0[if before.read().mv != after.read().mv {
                7
            } else {
                8
            }]
            .fetch_add(1, Ordering::Relaxed);
        }
    }
    pub(super) fn snapshot(&self) -> TTWriteStats {
        TTWriteStats {
            attempts: self.0[0].load(Ordering::Relaxed),
            empty: self.0[1].load(Ordering::Relaxed),
            same_key16: self.0[2].load(Ordering::Relaxed),
            different_key16: self.0[3].load(Ordering::Relaxed),
            payload_accepted: self.0[4].load(Ordering::Relaxed),
            payload_retained: self.0[5].load(Ordering::Relaxed),
            same_key16_same_depth_accepted: self.0[6].load(Ordering::Relaxed),
            move_only_changed: self.0[7].load(Ordering::Relaxed),
            unchanged: self.0[8].load(Ordering::Relaxed),
        }
    }
}
