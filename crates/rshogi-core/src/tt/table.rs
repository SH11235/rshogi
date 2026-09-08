//! TranspositionTable本体
//!
//! - Cluster: エントリのグループ
//! - TranspositionTable: テーブル本体
//! - probe/write操作

use super::alloc::{AllocKind, Allocation};
use super::entry::{AtomicTTEntry, TTData, TTEntry};
use super::{CLUSTER_SIZE, GENERATION_DELTA};
use crate::position::Position;
use crate::prefetch::TtPrefetch;
use crate::types::{Bound, Color, Move, Value};
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

/// 3 エントリと排他フラグを同じ32バイトに格納する。
#[repr(C, align(32))]
pub struct Cluster {
    entries: [AtomicTTEntry; CLUSTER_SIZE],
    locked: AtomicBool,
    padding: u8,
}

impl Cluster {
    const fn new() -> Self {
        Self {
            entries: [const { AtomicTTEntry::new() }; CLUSTER_SIZE],
            locked: AtomicBool::new(false),
            padding: 0,
        }
    }

    /// TT は読み捨て可能なキャッシュなので、競合時は待機せず利用を見送る。
    fn try_lock(&self) -> Option<ClusterGuard<'_>> {
        self.locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| ClusterGuard { cluster: self })
    }
}

impl Default for Cluster {
    fn default() -> Self {
        Self::new()
    }
}

struct ClusterGuard<'a> {
    cluster: &'a Cluster,
}

impl ClusterGuard<'_> {
    fn load(&self, index: usize) -> TTEntry {
        self.cluster.entries[index].load()
    }
    fn store(&self, index: usize, entry: TTEntry) {
        self.cluster.entries[index].store(entry);
    }
}

impl Drop for ClusterGuard<'_> {
    fn drop(&mut self) {
        self.cluster.locked.store(false, Ordering::Release);
    }
}

const _: () = assert!(std::mem::size_of::<Cluster>() == 32);

struct ClusterTable {
    alloc: Allocation,
    len: usize,
}

// SAFETY: 共有参照から触れる格納要素はすべてatomicで、snapshotはClusterGuardで排他する。
// 割当の解放・resize・clearは &mut self を要求し、借用中のprobe writerとは共存しない。
unsafe impl Sync for ClusterTable {}

impl ClusterTable {
    fn new(len: usize) -> Self {
        let bytes = len * std::mem::size_of::<Cluster>();
        let alloc = Allocation::allocate(bytes, std::mem::align_of::<Cluster>());
        let ptr = alloc.ptr().as_ptr() as *mut Cluster;
        // SAFETY: Clusterのアラインメントとlen個分の領域を確保済み。全フィールドはゼロが有効で、
        // AtomicBoolはfalse、AtomicU16は0となる。初期化中の割当はまだ共有されていない。
        unsafe {
            std::ptr::write_bytes(ptr, 0, len);
        }
        Self { alloc, len }
    }

    fn uses_large_pages(&self) -> bool {
        self.alloc.kind() == AllocKind::LargePages
    }
}

impl Deref for ClusterTable {
    type Target = [Cluster];

    fn deref(&self) -> &Self::Target {
        // SAFETY: allocはlen個の初期化済みClusterを所有し、この借用中は解放されない。
        unsafe { std::slice::from_raw_parts(self.alloc.ptr().as_ptr() as *const Cluster, self.len) }
    }
}

impl DerefMut for ClusterTable {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: 排他借用により既存のslice/ProbeResultがなく、全len個が初期化済み。
        unsafe {
            std::slice::from_raw_parts_mut(self.alloc.ptr().as_ptr() as *mut Cluster, self.len)
        }
    }
}

/// 置換表
pub struct TranspositionTable {
    /// クラスターの配列
    table: ClusterTable,
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

        let table = ClusterTable::new(cluster_count);

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
            self.table = ClusterTable::new(new_count);
            self.cluster_count = new_count;
        }
    }

    /// クリア
    pub fn clear(&mut self) {
        self.generation8.store(0, Ordering::Relaxed);
        let len = self.table.len();
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);

        // サイズが小さい場合やスレッド数が1の場合は逐次クリア
        if threads <= 1 || len < threads * 1024 {
            for cluster in self.table.iter_mut() {
                *cluster = Cluster::new();
            }
            return;
        }

        let chunk = len.div_ceil(threads);
        std::thread::scope(|scope| {
            for clusters in self.table.chunks_mut(chunk) {
                scope.spawn(move || clusters.fill_with(Cluster::new));
            }
        });
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

    /// 置換表を検索（クラスター内は16bitキーでマッチング）。
    /// 他スレッドが同じクラスターを操作中ならmissを返し、このprobeからの書込みも省略する。
    pub fn probe(&self, key: u64, pos: &Position) -> ProbeResult<'_> {
        let cluster = self.first_entry(key, pos.side_to_move());
        let Some(guard) = cluster.try_lock() else {
            return ProbeResult {
                found: false,
                data: TTData::EMPTY,
                writer: None,
            };
        };
        let key16 = key as u16;
        let gen8 = self.generation();
        let mut replace = 0;
        let mut min_value = i32::MAX;
        for index in 0..CLUSTER_SIZE {
            let entry = guard.load(index);
            if entry.key16() == key16 {
                let mut data = entry.read();
                if data.mv != Move::NONE {
                    if let Some(m) = pos.to_move(data.mv) {
                        data.mv = m;
                    } else {
                        // 不正な手を持つ同一短縮キーはヒットとして採用しない。
                        let value = entry.depth8() as i32 - entry.relative_age(gen8) as i32;
                        if value < min_value {
                            min_value = value;
                            replace = index;
                        }
                        continue;
                    }
                }
                return ProbeResult {
                    found: entry.is_occupied(),
                    data,
                    writer: Some((cluster, index)),
                };
            }
            let value = entry.depth8() as i32 - entry.relative_age(gen8) as i32;
            if value < min_value {
                min_value = value;
                replace = index;
            }
        }
        ProbeResult {
            found: false,
            data: TTData::EMPTY,
            writer: Some((cluster, replace)),
        }
    }

    /// 置換表の使用率を1000分率で返す
    pub fn hashfull(&self, max_age: u8) -> i32 {
        let max_age_internal = max_age << super::GENERATION_BITS;
        let gen8 = self.generation();
        let mut count = 0;
        let sample_count = 1000.min(self.cluster_count);

        for cluster in self.table.iter().take(sample_count) {
            let Some(guard) = cluster.try_lock() else {
                continue;
            };
            for index in 0..CLUSTER_SIZE {
                let entry = guard.load(index);
                if entry.is_occupied() && entry.relative_age(gen8) <= max_age_internal {
                    count += 1;
                }
            }
        }

        count / CLUSTER_SIZE as i32
    }

    /// Large Pagesを使って確保されたかを返す
    pub fn uses_large_pages(&self) -> bool {
        self.table.uses_large_pages()
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
        debug_assert!(index < self.cluster_count);
        // SAFETY: cluster_index は (key * cluster_count >> 64) & !1 | bit0 で計算され、
        //         結果は常に 0..cluster_count の範囲内。
        //         cluster_count は new/resize で `& !1` により常に偶数が保証されるため、
        //         bit0 OR 後も cluster_count を超えない。table の長さは cluster_count。
        unsafe { self.table.get_unchecked(index) }
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

/// probe結果。書込み先のテーブル借用が有効な間だけ使用できる。
///
/// 所有元の破棄後に書き込むことはできない。
/// ```compile_fail,E0505
/// use rshogi_core::{position::Position, tt::TranspositionTable, types::{Bound, Move, Value}};
/// let tt = TranspositionTable::new(1);
/// let pos = Position::new();
/// let probe = tt.probe(1, &pos);
/// drop(tt);
/// probe.write(1, Value::ZERO, false, Bound::Exact, 1, Move::NONE, Value::ZERO, 0);
/// ```
///
/// 未使用のwriterが残っている間はresizeできない。
/// ```compile_fail,E0502
/// use rshogi_core::{position::Position, tt::TranspositionTable, types::{Bound, Move, Value}};
/// let mut tt = TranspositionTable::new(1);
/// let pos = Position::new();
/// let probe = tt.probe(1, &pos);
/// tt.resize(2);
/// probe.write(1, Value::ZERO, false, Bound::Exact, 1, Move::NONE, Value::ZERO, 0);
/// ```
pub struct ProbeResult<'a> {
    /// ヒットしたか
    pub found: bool,
    /// 読み取ったデータ
    pub data: TTData,
    /// 書き込み用エントリ
    writer: Option<(&'a Cluster, usize)>,
}

impl ProbeResult<'_> {
    /// エントリに書き込む（内部で16bitに切り詰め）
    ///
    /// probe後の別writerによる更新を再読込し、排他下で置換条件を判定する。
    /// 競合時はこの書込みを見送る。
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
        let Some((cluster, index)) = self.writer else {
            return;
        };
        let Some(guard) = cluster.try_lock() else {
            return;
        };
        let mut entry = guard.load(index);
        entry.save(key, value, is_pv, bound, depth, mv, eval, generation8);
        guard.store(index, entry);
    }
}

impl TtPrefetch for TranspositionTable {
    #[inline]
    fn prefetch(&self, key: u64, side_to_move: Color) {
        TranspositionTable::prefetch(self, key, side_to_move);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::position::{Position, SFEN_HIRATE};

    #[test]
    fn test_stale_writer_rechecks_latest_entry() {
        let tt = TranspositionTable::new(0);
        let mut pos = Position::new();
        pos.set_hirate();
        let old_probe = tt.probe(1, &pos);
        let mv = Move::from_usi("7g7f").unwrap();
        tt.probe(2, &pos)
            .write(2, Value::new(20), false, Bound::Exact, 20, mv, Value::ZERO, 0);
        old_probe.write(1, Value::new(10), false, Bound::Lower, 10, Move::NONE, Value::ZERO, 0);
        let replaced = tt.probe(1, &pos);
        assert!(replaced.found);
        assert_eq!(replaced.data.mv, Move::NONE, "different key must not inherit previous move");
        tt.probe(1, &pos)
            .write(1, Value::new(30), false, Bound::Exact, 30, mv, Value::ZERO, 0);
        old_probe.write(1, Value::new(1), false, Bound::Lower, 1, Move::NONE, Value::ZERO, 0);
        let preserved = tt.probe(1, &pos);
        assert_eq!(preserved.data.value.raw(), 30, "save policy must use latest depth");
        assert_eq!(preserved.data.mv.to_usi(), "7g7f");
    }

    #[test]
    fn test_cluster_contention_skips_probe_and_write() {
        let tt = TranspositionTable::new(0);
        let pos = Position::new();
        let writer = tt.probe(1, &pos);
        let cluster = tt.first_entry(1, pos.side_to_move());
        let guard = cluster.try_lock().unwrap();
        assert!(!tt.probe(1, &pos).found);
        writer.write(1, Value::new(99), false, Bound::Exact, 10, Move::NONE, Value::ZERO, 0);
        assert!(!guard.load(0).is_occupied());
        drop(guard);
        writer.write(1, Value::new(99), false, Bound::Exact, 10, Move::NONE, Value::ZERO, 0);
        assert_eq!(tt.probe(1, &pos).data.value.raw(), 99);
    }

    #[test]
    fn test_competing_writers_publish_consistent_entries() {
        use std::sync::{Arc, Barrier};
        let tt = TranspositionTable::new(0);
        let mut pos = Position::new();
        pos.set_hirate();
        let barrier = Arc::new(Barrier::new(4));
        std::thread::scope(|scope| {
            for key in [1, 2] {
                let barrier = Arc::clone(&barrier);
                let tt = &tt;
                let pos = &pos;
                scope.spawn(move || {
                    let mv = Move::from_usi(if key == 1 { "7g7f" } else { "2g2f" }).unwrap();
                    barrier.wait();
                    for _ in 0..10_000 {
                        tt.probe(key, pos).write(
                            key,
                            Value::new(key as i32 * 100),
                            key == 1,
                            Bound::Exact,
                            key as i32 * 10,
                            mv,
                            Value::new(-(key as i32)),
                            0,
                        );
                    }
                });
            }
            for _ in 0..2 {
                let barrier = Arc::clone(&barrier);
                let tt = &tt;
                let pos = &pos;
                scope.spawn(move || {
                    barrier.wait();
                    for _ in 0..10_000 {
                        for key in [1, 2] {
                            let probe = tt.probe(key, pos);
                            if probe.found {
                                assert_eq!(probe.data.value.raw(), key as i32 * 100);
                                assert_eq!(probe.data.eval.raw(), -(key as i32));
                                assert_eq!(probe.data.depth, key as i32 * 10);
                                assert_eq!(probe.data.is_pv, key == 1);
                                assert_eq!(probe.data.bound, Bound::Exact);
                                assert_eq!(
                                    probe.data.mv.to_usi(),
                                    if key == 1 { "7g7f" } else { "2g2f" }
                                );
                            }
                        }
                        assert!(tt.hashfull(0) >= 0);
                    }
                });
            }
        });
        for key in [1, 2] {
            tt.probe(key, &pos).write(
                key,
                Value::new(key as i32 * 100),
                false,
                Bound::Exact,
                10,
                Move::NONE,
                Value::ZERO,
                0,
            );
            assert!(tt.probe(key, &pos).found);
        }
    }

    #[test]
    fn test_snapshot_replacement_capacity_and_clear() {
        let mut tt = TranspositionTable::new(1);
        let mut pos = Position::new();
        pos.set_hirate();
        assert_eq!(tt.cluster_count * CLUSTER_SIZE, 98_304);
        for key in 1..=3 {
            tt.probe(key, &pos).write(
                key,
                Value::new(key as i32),
                false,
                Bound::Exact,
                key as i32 * 10,
                Move::NONE,
                Value::ZERO,
                0,
            );
        }
        for key in 1..=3 {
            assert!(tt.probe(key, &pos).found);
        }
        tt.probe(4, &pos).write(
            4,
            Value::ZERO,
            false,
            Bound::Exact,
            40,
            Move::NONE,
            Value::ZERO,
            0,
        );
        assert!(!tt.probe(1, &pos).found, "shallowest entry must be replaced");
        tt.new_search();
        tt.probe(2, &pos).write(
            2,
            Value::ZERO,
            false,
            Bound::Exact,
            1,
            Move::NONE,
            Value::ZERO,
            tt.generation(),
        );
        tt.probe(5, &pos).write(
            5,
            Value::ZERO,
            false,
            Bound::Exact,
            1,
            Move::NONE,
            Value::ZERO,
            tt.generation(),
        );
        // depth-age: key2=2、key3=31-8、key4=41-8。
        assert!(!tt.probe(2, &pos).found);
        tt.clear();
        for key in 1..=5 {
            assert!(!tt.probe(key, &pos).found);
        }
    }

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
        let generation = tt.generation();
        // 300 * 8 = 2400, 2400 % 256 = 96
        // 正常に動作していることを確認（u8なので必ず0-255の範囲）
        let _ = generation; // コンパイルが通れば正常
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
        // クラスターは32バイト（YaneuraOu CLUSTER_SIZE=3 準拠）
        assert_eq!(std::mem::size_of::<Cluster>(), 32);
    }
}
