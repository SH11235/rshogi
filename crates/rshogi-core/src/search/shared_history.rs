//! スレッド間で共有する履歴 (pawn history と、局面キーで引く correction history)。
//!
//! Lazy SMP の各スレッドが同じ表を読み書きし、ほかのスレッドが調べた歩の形や駒配置の
//! 補正値・履歴を使えるようにする。表の大きさは 1 スレッド分 × スレッド数 (2 のべき乗に
//! 切り上げ) なので、1 スレッドでは 1 スレッド専用の表と同じ大きさ・同じ引き方になる。
//!
//! 読み書きは relaxed の load / store で、同じ entry を同時に更新すると片方の更新が
//! 失われうる。履歴は近似値なのでそれを許し、値が i16 として壊れないことだけを保証する。

use std::sync::atomic::{AtomicI16, Ordering};

use crate::position::Position;
use crate::types::{Color, Piece, Square};

use super::history::{CORRECTION_HISTORY_LIMIT, CORRECTION_HISTORY_SIZE, PAWN_HISTORY_SIZE};

/// pawnHistory初期値 (YO: pawnHistory.clear_range(-1238, ...))
pub(super) const PAWN_HISTORY_INIT: i16 = -1238;

/// 複数スレッドから読み書きする履歴の 1 entry。更新式は `StatsEntry` と同じ。
#[repr(transparent)]
pub struct SharedStatsEntry<const D: i32>(AtomicI16);

impl<const D: i32> SharedStatsEntry<D> {
    /// 値を取得
    #[inline]
    pub fn get(&self) -> i16 {
        self.0.load(Ordering::Relaxed)
    }

    /// 値を設定
    #[inline]
    pub fn set(&self, v: i16) {
        self.0.store(v, Ordering::Relaxed);
    }

    /// ボーナス値を加算（範囲制限付き）。`StatsEntry::update` と同じ式。
    #[inline]
    pub fn update(&self, bonus: i32) {
        let value = self.get() as i32;
        let clamped = bonus.clamp(-D, D);
        let delta = clamped - value * clamped.abs() / D;
        let updated = value + delta;
        debug_assert!(updated.abs() <= D, "SharedStatsEntry out of range: {updated} (D={D})");
        self.set(updated as i16);
    }
}

type CorrectionEntry = SharedStatsEntry<CORRECTION_HISTORY_LIMIT>;

/// 1 つの index に並べた pawn / minor / non-pawn の correction。
///
/// 3 種類は別々の局面キーで引くので、同じ bundle の中身は別々の局面に属する。
#[repr(C)]
pub struct CorrectionBundle {
    pawn: [CorrectionEntry; Color::NUM],
    minor: [CorrectionEntry; Color::NUM],
    /// [board_color][side_to_move]
    non_pawn: [[CorrectionEntry; Color::NUM]; Color::NUM],
}

/// pawn history の 1 index 分: [piece][to] -> score
pub type SharedPawnHistoryRow = [[SharedStatsEntry<8192>; Square::NUM]; Piece::NUM];

/// スレッド間で共有する履歴の表。
pub struct SharedHistories {
    correction: Box<[CorrectionBundle]>,
    pawn_history: Box<[SharedPawnHistoryRow]>,
    correction_mask: usize,
    pawn_history_mask: usize,
    scale: usize,
}

impl SharedHistories {
    /// `num_threads` 本のスレッドで共有する表を作る (初期値込み)。
    pub fn new(num_threads: usize) -> Self {
        let scale = Self::scale_for(num_threads);
        let correction_len = CORRECTION_HISTORY_SIZE * scale;
        let pawn_history_len = PAWN_HISTORY_SIZE * scale;
        // SAFETY: CorrectionBundle と SharedPawnHistoryRow は AtomicI16 だけで構成され、
        // AtomicI16 はゼロのビット列が有効な値 (0) である。
        let (correction, pawn_history) = unsafe {
            (
                Box::<[CorrectionBundle]>::new_zeroed_slice(correction_len).assume_init(),
                Box::<[SharedPawnHistoryRow]>::new_zeroed_slice(pawn_history_len).assume_init(),
            )
        };
        let shared = Self {
            correction,
            pawn_history,
            correction_mask: correction_len - 1,
            pawn_history_mask: pawn_history_len - 1,
            scale,
        };
        shared.clear(PAWN_HISTORY_INIT);
        shared
    }

    /// `num_threads` 本で共有するときの、1 スレッド分の表に対する倍率。
    ///
    /// wasm32 はアドレス空間が 4 GB なので、pawn history (1 倍で約 41 MB) が収まるよう抑える。
    pub fn scale_for(num_threads: usize) -> usize {
        let scale = num_threads.max(1).next_power_of_two();
        if cfg!(target_arch = "wasm32") {
            scale.min(16)
        } else {
            scale
        }
    }

    /// この表の倍率 (`scale_for` の値)
    pub fn scale(&self) -> usize {
        self.scale
    }

    /// すべての entry を初期値に戻す。探索中のスレッドが無いときに呼ぶ。
    ///
    /// スレッド数に比例して表が大きくなるので、複数スレッドで分けて埋める。
    pub fn clear(&self, pawn_history_init: i16) {
        let parts = clear_parallelism(self.scale);
        let fill = |part: usize| {
            let range = |len: usize| len * part / parts..len * (part + 1) / parts;
            for bundle in &self.correction[range(self.correction.len())] {
                for entry in
                    bundle.pawn.iter().chain(&bundle.minor).chain(bundle.non_pawn.iter().flatten())
                {
                    entry.set(0);
                }
            }
            for entry in
                self.pawn_history[range(self.pawn_history.len())].iter().flatten().flatten()
            {
                entry.set(pawn_history_init);
            }
        };
        run_parts(parts, &fill);
    }

    #[inline]
    fn correction_bundle(&self, key: u64) -> &CorrectionBundle {
        // SAFETY: correction_mask = len - 1 (len は 2 のべき乗) なので index < len。
        unsafe { self.correction.get_unchecked(key as usize & self.correction_mask) }
    }

    /// pawn correction を取得
    #[inline]
    pub fn pawn_correction(&self, pawn_key: u64, stm: Color) -> i16 {
        self.correction_bundle(pawn_key).pawn[stm.index()].get()
    }

    /// pawn correction を更新
    #[inline]
    pub fn update_pawn_correction(&self, pawn_key: u64, stm: Color, bonus: i32) {
        self.correction_bundle(pawn_key).pawn[stm.index()].update(bonus);
    }

    /// minor piece correction を取得
    #[inline]
    pub fn minor_correction(&self, minor_key: u64, stm: Color) -> i16 {
        self.correction_bundle(minor_key).minor[stm.index()].get()
    }

    /// minor piece correction を更新
    #[inline]
    pub fn update_minor_correction(&self, minor_key: u64, stm: Color, bonus: i32) {
        self.correction_bundle(minor_key).minor[stm.index()].update(bonus);
    }

    /// `board_color` 側の non-pawn correction を取得
    #[inline]
    pub fn non_pawn_correction(&self, non_pawn_key: u64, board_color: Color, stm: Color) -> i16 {
        self.correction_bundle(non_pawn_key).non_pawn[board_color.index()][stm.index()].get()
    }

    /// `board_color` 側の non-pawn correction を更新
    #[inline]
    pub fn update_non_pawn_correction(
        &self,
        non_pawn_key: u64,
        board_color: Color,
        stm: Color,
        bonus: i32,
    ) {
        self.correction_bundle(non_pawn_key).non_pawn[board_color.index()][stm.index()]
            .update(bonus);
    }

    /// pawn history の index を計算する
    #[inline]
    pub fn pawn_history_index(&self, pawn_key: u64) -> usize {
        pawn_key as usize & self.pawn_history_mask
    }

    #[inline]
    fn pawn_history_entry(&self, index: usize, pc: Piece, to: Square) -> &SharedStatsEntry<8192> {
        debug_assert!(index < self.pawn_history.len());
        // SAFETY: index & mask < len (len は 2 のべき乗)。別の倍率の表で計算した index でも範囲内に収まる。
        //         Piece::index() < Piece::NUM、Square::index() < Square::NUM。
        unsafe {
            self.pawn_history
                .get_unchecked(index & self.pawn_history_mask)
                .get_unchecked(pc.index())
                .get_unchecked(to.index())
        }
    }

    /// pawn history を取得
    #[inline]
    pub fn pawn_history(&self, index: usize, pc: Piece, to: Square) -> i16 {
        self.pawn_history_entry(index, pc, to).get()
    }

    /// pawn history を更新
    #[inline]
    pub fn update_pawn_history(&self, index: usize, pc: Piece, to: Square, bonus: i32) {
        self.pawn_history_entry(index, pc, to).update(bonus);
    }
}

impl SharedHistories {
    /// 指し手を実行した直後の局面で引く entry をキャッシュへ先読みする。
    ///
    /// 子局面の静的評価 (NNUE) の間にメモリ待ちを隠す。`pc` / `to` は実行した手の
    /// 動かした後の駒と移動先で、pawn history はその組を読む。
    #[inline]
    pub fn prefetch(&self, pos: &Position, pc: Piece, to: Square) {
        prefetch_ptr(self.correction_bundle(pos.pawn_key()));
        prefetch_ptr(self.correction_bundle(pos.minor_piece_key()));
        prefetch_ptr(self.correction_bundle(pos.non_pawn_key(Color::White)));
        prefetch_ptr(self.correction_bundle(pos.non_pawn_key(Color::Black)));
        prefetch_ptr(self.pawn_history_entry(self.pawn_history_index(pos.pawn_key()), pc, to));
    }
}

#[inline]
fn prefetch_ptr<T>(entry: &T) {
    #[cfg(target_arch = "x86_64")]
    // SAFETY: entry は生存中の値を指し、prefetch は内容を変更しない。
    unsafe {
        use std::arch::x86_64::_mm_prefetch;
        _mm_prefetch(entry as *const T as *const i8, 3);
    }
    #[cfg(not(target_arch = "x86_64"))]
    let _ = entry;
}

/// 表を埋めるのに使うスレッド数。1 スレッド分の表なら分けない。
fn clear_parallelism(scale: usize) -> usize {
    #[cfg(not(target_arch = "wasm32"))]
    let available = std::thread::available_parallelism().map_or(1, |n| n.get());
    #[cfg(all(target_arch = "wasm32", feature = "wasm-threads"))]
    let available = rayon::current_num_threads();
    #[cfg(all(target_arch = "wasm32", not(feature = "wasm-threads")))]
    let available = 1;
    scale.min(available).max(1)
}

/// `fill(0..parts)` を並列に実行する。
fn run_parts(parts: usize, fill: &(dyn Fn(usize) + Sync)) {
    if parts == 1 {
        fill(0);
        return;
    }
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::scope(|scope| {
        for part in 0..parts {
            scope.spawn(move || fill(part));
        }
    });
    #[cfg(all(target_arch = "wasm32", feature = "wasm-threads"))]
    rayon::scope(|scope| {
        for part in 0..parts {
            scope.spawn(move |_| fill(part));
        }
    });
    #[cfg(all(target_arch = "wasm32", not(feature = "wasm-threads")))]
    for part in 0..parts {
        fill(part);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::StatsEntry;

    #[test]
    fn table_size_scales_with_thread_count() {
        let single = SharedHistories::new(1);
        assert_eq!(single.correction.len(), CORRECTION_HISTORY_SIZE);
        assert_eq!(single.pawn_history.len(), PAWN_HISTORY_SIZE);
        assert_eq!(single.pawn_history_index(PAWN_HISTORY_SIZE as u64 + 3), 3);

        let three = SharedHistories::new(3);
        assert_eq!(three.correction.len(), CORRECTION_HISTORY_SIZE * 4);
        assert_eq!(three.pawn_history.len(), PAWN_HISTORY_SIZE * 4);
        let key = PAWN_HISTORY_SIZE as u64 + 3;
        assert_eq!(three.pawn_history_index(key), key as usize);
    }

    #[test]
    fn update_matches_stats_entry() {
        let shared = SharedStatsEntry::<CORRECTION_HISTORY_LIMIT>(AtomicI16::new(0));
        let mut local = StatsEntry::<CORRECTION_HISTORY_LIMIT>::default();
        for bonus in [300, -1500, 77, 2000, -3, -900, 1024, -1024, 5] {
            shared.update(bonus);
            local.update(bonus);
            assert_eq!(shared.get(), local.get(), "bonus {bonus}");
        }
    }

    #[test]
    fn clear_restores_initial_values() {
        let shared = SharedHistories::new(4);
        let last_key = (shared.correction.len() - 1) as u64;
        shared.update_pawn_correction(last_key, Color::White, 500);
        shared.update_non_pawn_correction(7, Color::Black, Color::White, -300);
        let last_index = shared.pawn_history.len() - 1;
        shared.update_pawn_history(last_index, Piece::B_PAWN, Square::SQ_55, 4000);

        shared.clear(-7);

        assert!(shared.correction.iter().all(|bundle| {
            bundle
                .pawn
                .iter()
                .chain(&bundle.minor)
                .chain(bundle.non_pawn.iter().flatten())
                .all(|entry| entry.get() == 0)
        }));
        assert!(shared.pawn_history.iter().flatten().flatten().all(|entry| entry.get() == -7));
    }
}
