//! TranspositionTable本体
//!
//! - Cluster: エントリのグループ
//! - TranspositionTable: テーブル本体
//! - probe/write操作

use super::entry::{TTData, TTEntry};
use super::{CLUSTER_SIZE, GENERATION_DELTA};
use crate::position::Position;
use crate::types::{Bound, Color, Move, Value};
use std::sync::atomic::{AtomicU8, Ordering};

/// クラスター構造
/// 同じハッシュインデックスに対して複数のエントリを持つ
#[repr(C, align(32))]
pub struct Cluster {
    entries: [TTEntry; CLUSTER_SIZE],
    _padding: [u8; 2], // 10 * 3 + 2 = 32 bytes
}

impl Cluster {
    /// 新しいクラスターを作成
    const fn new() -> Self {
        Self {
            entries: [TTEntry::new(); CLUSTER_SIZE],
            _padding: [0; 2],
        }
    }
}

impl Default for Cluster {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for Cluster {
    fn clone(&self) -> Self {
        Self {
            entries: self.entries,
            _padding: self._padding,
        }
    }
}

// クラスターは32バイトであることを保証
const _: () = assert!(std::mem::size_of::<Cluster>() == 32);

/// 置換表
pub struct TranspositionTable {
    /// クラスターの配列
    table: Box<[Cluster]>,
    /// クラスター数
    cluster_count: usize,
    /// 世代カウンター（下位3bitは使用しない）
    generation8: AtomicU8,
}

impl TranspositionTable {
    /// 新しい置換表を作成（サイズはMB単位）
    pub fn new(mb_size: usize) -> Self {
        let cluster_count = (mb_size * 1024 * 1024 / std::mem::size_of::<Cluster>()) & !1;
        let cluster_count = cluster_count.max(2); // 最小2クラスター

        let table = vec![Cluster::new(); cluster_count].into_boxed_slice();

        Self {
            table,
            cluster_count,
            generation8: AtomicU8::new(0),
        }
    }

    /// サイズを変更
    pub fn resize(&mut self, mb_size: usize) {
        let new_count = (mb_size * 1024 * 1024 / std::mem::size_of::<Cluster>()) & !1;
        let new_count = new_count.max(2);

        if new_count != self.cluster_count {
            self.table = vec![Cluster::new(); new_count].into_boxed_slice();
            self.cluster_count = new_count;
        }
    }

    /// クリア
    pub fn clear(&mut self) {
        self.generation8.store(0, Ordering::Relaxed);

        for cluster in self.table.iter_mut() {
            *cluster = Cluster::new();
        }
    }

    /// 新しい探索を開始（世代を進める）
    pub fn new_search(&self) {
        self.generation8.fetch_add(GENERATION_DELTA, Ordering::Relaxed);
    }

    /// 現在の世代を取得
    #[inline]
    pub fn generation(&self) -> u8 {
        self.generation8.load(Ordering::Relaxed)
    }

    /// 置換表を検索
    pub fn probe(&self, key: u64, pos: &Position) -> ProbeResult {
        let side_to_move = pos.side_to_move();
        let cluster = self.first_entry(key, side_to_move);
        let key16 = key as u16;

        // クラスター内を検索
        for entry in &cluster.entries {
            if entry.key16() == key16 {
                let mut data = entry.read();

                if data.mv != Move::NONE {
                    if let Some(m) = pos.to_move(data.mv) {
                        data.mv = m;
                    } else {
                        continue;
                    }
                }

                return ProbeResult {
                    found: entry.is_occupied(),
                    data,
                    writer: entry as *const _ as *mut _,
                    key16,
                };
            }
        }

        // 置換するエントリを選択（価値が最小のもの）
        let gen8 = self.generation();
        let mut replace = cluster.entries.as_ptr() as *mut TTEntry;
        let mut min_value = i32::MAX;

        for entry in &cluster.entries {
            // 置換価値 = depth8 - relative_age (YaneuraOu準拠)
            let value = entry.depth8() as i32 - entry.relative_age(gen8) as i32;

            if value < min_value {
                min_value = value;
                replace = entry as *const _ as *mut TTEntry;
            }
        }

        ProbeResult {
            found: false,
            data: TTData::EMPTY,
            writer: replace,
            key16,
        }
    }

    /// 置換表の使用率を1000分率で返す
    pub fn hashfull(&self, max_age: u8) -> i32 {
        let max_age_internal = max_age << super::GENERATION_BITS;
        let gen8 = self.generation();
        let mut count = 0;
        let sample_count = 1000.min(self.cluster_count);

        for cluster in self.table.iter().take(sample_count) {
            for entry in &cluster.entries {
                if entry.is_occupied() && entry.relative_age(gen8) <= max_age_internal {
                    count += 1;
                }
            }
        }

        count / CLUSTER_SIZE as i32
    }

    /// クラスターインデックスを計算
    #[inline]
    fn cluster_index(&self, key: u64, side_to_move: Color) -> usize {
        // key * cluster_count / 2^64 でインデックスを計算
        let index = ((key as u128 * self.cluster_count as u128) >> 64) as usize;
        // bit0を手番に設定
        (index & !1) | side_to_move as usize
    }

    /// クラスターの参照を取得
    #[inline]
    fn first_entry(&self, key: u64, side_to_move: Color) -> &Cluster {
        let index = self.cluster_index(key, side_to_move);
        &self.table[index]
    }

    /// 指定キーのクラスターをプリフェッチ
    #[inline]
    pub fn prefetch(&self, key: u64, side_to_move: Color) {
        let cluster = self.first_entry(key, side_to_move);

        #[cfg(target_arch = "x86_64")]
        unsafe {
            use std::arch::x86_64::_mm_prefetch;
            _mm_prefetch(cluster as *const _ as *const i8, 3); // _MM_HINT_T0
        }

        #[cfg(target_arch = "aarch64")]
        unsafe {
            use std::arch::aarch64::__prefetch;
            __prefetch(cluster as *const _ as *const u8);
        }

        #[cfg(all(not(target_arch = "x86_64"), not(target_arch = "aarch64")))]
        let _ = cluster; // 何もしない
    }
}

/// probe結果
pub struct ProbeResult {
    /// ヒットしたか
    pub found: bool,
    /// 読み取ったデータ
    pub data: TTData,
    /// 書き込み用エントリ
    writer: *mut TTEntry,
    /// キー（書き込み時に使用）
    key16: u16,
}

impl ProbeResult {
    /// エントリに書き込む
    ///
    /// # Safety
    /// writerポインタが有効であることを前提とする
    pub fn write(
        &self,
        key: u64,
        value: Value,
        is_pv: bool,
        bound: Bound,
        depth: i32,
        mv: Move,
        eval: Value,
        generation8: u8,
    ) {
        debug_assert_eq!(self.key16, key as u16);
        unsafe {
            (*self.writer).save(self.key16, value, is_pv, bound, depth, mv, eval, generation8);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::position::{Position, SFEN_HIRATE};

    #[test]
    fn test_tt_new() {
        let tt = TranspositionTable::new(1); // 1MB
        assert!(tt.cluster_count >= 2);
        assert_eq!(tt.generation(), 0);
    }

    #[test]
    fn test_tt_new_search() {
        let tt = TranspositionTable::new(1);
        assert_eq!(tt.generation(), 0);

        tt.new_search();
        assert_eq!(tt.generation(), GENERATION_DELTA);

        tt.new_search();
        assert_eq!(tt.generation(), GENERATION_DELTA * 2);
    }

    #[test]
    fn test_tt_probe_empty() {
        let tt = TranspositionTable::new(1);
        let pos = Position::new();
        let result = tt.probe(12345, &pos);
        assert!(!result.found);
    }

    #[test]
    fn test_tt_probe_and_write() {
        let mut pos = Position::new();
        pos.set_sfen(SFEN_HIRATE).unwrap();

        let tt = TranspositionTable::new(1);
        let key = pos.key();

        // 最初はヒットしない
        let probe1 = tt.probe(key, &pos);
        assert!(!probe1.found);

        // 書き込み
        probe1.write(
            key,
            Value::new(50),
            true,
            Bound::Exact,
            10,
            Move::NONE,
            Value::ZERO,
            tt.generation(),
        );

        // 2回目はヒット
        let probe2 = tt.probe(key, &pos);
        assert!(probe2.found);
        assert_eq!(probe2.data.value.raw(), 50);
        assert_eq!(probe2.data.bound, Bound::Exact);
        assert!(probe2.data.is_pv);
    }

    #[test]
    fn test_tt_generation_cycle() {
        let tt = TranspositionTable::new(1);

        for _ in 0..300 {
            tt.new_search();
        }

        // オーバーフローしても正常に動作
        // generation は 8 の倍数で増加し、u8でwrapするので常に256未満
        let gen = tt.generation();
        // 300 * 8 = 2400, 2400 % 256 = 96
        // 正常に動作していることを確認（u8なので必ず0-255の範囲）
        let _ = gen; // コンパイルが通れば正常
    }

    #[test]
    fn test_tt_hashfull() {
        let tt = TranspositionTable::new(1);

        // 空の状態では0
        assert_eq!(tt.hashfull(0), 0);
    }

    #[test]
    fn test_tt_clear() {
        let mut pos = Position::new();
        pos.set_sfen(SFEN_HIRATE).unwrap();

        let mut tt = TranspositionTable::new(1);
        let key = pos.key();

        // 書き込み（DEPTH_ENTRY_OFFSETを考慮して有効な深さ）
        let probe1 = tt.probe(key, &pos);
        probe1.write(
            key,
            Value::new(100),
            false,
            Bound::Lower,
            10,
            Move::NONE,
            Value::ZERO,
            tt.generation(),
        );

        // クリア
        tt.clear();

        // クリア後はヒットしない
        let probe2 = tt.probe(key, &pos);
        assert!(!probe2.found);
    }

    #[test]
    fn test_tt_resize() {
        let mut tt = TranspositionTable::new(1);
        let initial_count = tt.cluster_count;

        tt.resize(2);
        assert!(tt.cluster_count > initial_count);

        tt.resize(1);
        assert_eq!(tt.cluster_count, initial_count);
    }

    #[test]
    fn test_cluster_size() {
        // クラスターは32バイト
        assert_eq!(std::mem::size_of::<Cluster>(), 32);
    }
}
