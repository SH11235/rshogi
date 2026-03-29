//! AccumulatorLayerStacks - LayerStacksアーキテクチャ用の1536次元アキュムレータ
//!
//! LayerStacks の Feature Transformer は各視点で 1536 次元を出力する。
//! 既存の Accumulator（256次元、HalfKP用）とは別に管理する。

use super::accumulator::{DirtyPiece, IndexList, MAX_ACTIVE_FEATURES, MAX_PATH_LENGTH};
use super::constants::NNUE_PYTORCH_L1;
use crate::types::{Color, MAX_PLY, Square};

/// LayerStacks用アキュムレータ（1536次元）
#[repr(C, align(64))]
#[derive(Clone)]
pub struct AccumulatorLayerStacks {
    /// 各視点の累積値 [perspective][dimension]
    /// perspective: 0 = Black, 1 = White
    pub accumulation: [[i16; NNUE_PYTORCH_L1]; 2],

    /// 計算済みフラグ
    pub computed_accumulation: bool,

    /// スコア計算済みフラグ（差分更新時にリセット）
    pub computed_score: bool,
}

impl AccumulatorLayerStacks {
    /// 新規作成
    pub fn new() -> Self {
        Self {
            accumulation: [[0; NNUE_PYTORCH_L1]; 2],
            computed_accumulation: false,
            computed_score: false,
        }
    }

    /// 指定視点の累積値を取得
    #[inline]
    pub fn get(&self, perspective: usize) -> &[i16; NNUE_PYTORCH_L1] {
        &self.accumulation[perspective]
    }

    /// 指定視点の累積値を取得（可変）
    #[inline]
    pub fn get_mut(&mut self, perspective: usize) -> &mut [i16; NNUE_PYTORCH_L1] {
        &mut self.accumulation[perspective]
    }
}

impl Default for AccumulatorLayerStacks {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// AccumulatorCacheLayerStacks - Finny Tables（玉位置×視点キャッシュ）
// =============================================================================

/// AccumulatorCaches のキャッシュエントリ（Finny Tables）
///
/// 各玉位置×視点ごとに、最後に計算したアキュムレータ値とその時点のアクティブ特徴量を保持。
/// refresh 時にキャッシュからの差分で更新することで、全駒加算を回避する。
#[repr(C, align(64))]
struct AccCacheEntry {
    /// キャッシュされたアキュムレータ値
    accumulation: [i16; NNUE_PYTORCH_L1],
    /// キャッシュ時点のアクティブ特徴インデックス（ソート済み）
    active_indices: [u32; MAX_ACTIVE_FEATURES],
    /// active_indices の有効数
    num_active: u16,
    /// 有効フラグ
    valid: bool,
}

impl AccCacheEntry {
    /// 無効な初期状態で作成
    fn new_invalid() -> Self {
        Self {
            accumulation: [0; NNUE_PYTORCH_L1],
            active_indices: [0; MAX_ACTIVE_FEATURES],
            num_active: 0,
            valid: false,
        }
    }
}

/// 玉位置×視点ごとのアキュムレータキャッシュ（Finny Tables）
///
/// 81マス × 2視点 = 162 エントリ。
/// 玉が移動して full refresh が必要な場合に、前回同じ玉位置で計算した
/// アキュムレータとの差分のみを適用することで高速化する。
pub struct AccumulatorCacheLayerStacks {
    /// [king_sq][perspective] のキャッシュエントリ
    entries: Box<[[AccCacheEntry; 2]; Square::NUM]>,
}

impl AccumulatorCacheLayerStacks {
    /// 新規作成（全エントリ無効）
    pub fn new() -> Self {
        // Box で確保（162 × エントリサイズはスタックに収まらないため）
        let entries: Vec<[AccCacheEntry; 2]> = (0..Square::NUM)
            .map(|_| [AccCacheEntry::new_invalid(), AccCacheEntry::new_invalid()])
            .collect();
        // SAFETY: Vec の長さが Square::NUM であることを保証
        let boxed: Box<[[AccCacheEntry; 2]]> = entries.into_boxed_slice();
        // SAFETY: Square::NUM == 81 なので配列サイズと一致する
        let ptr = Box::into_raw(boxed) as *mut [[AccCacheEntry; 2]; Square::NUM];
        let entries = unsafe { Box::from_raw(ptr) };
        Self { entries }
    }

    /// 全エントリを無効化
    pub fn invalidate(&mut self) {
        for sq_entries in self.entries.iter_mut() {
            for entry in sq_entries.iter_mut() {
                entry.valid = false;
            }
        }
    }

    /// キャッシュからの差分で refresh を実行
    ///
    /// キャッシュが有効な場合、現在のアクティブ特徴量との差分を計算し、
    /// add/sub のみでアキュムレータを更新する。
    /// キャッシュが無効な場合は通常の full refresh を行い、キャッシュを更新する。
    ///
    /// # 引数
    ///
    /// - `king_sq`: この視点の玉位置
    /// - `perspective`: 視点（先手/後手）
    /// - `active`: 現在のアクティブ特徴インデックス（ソート済み）
    /// - `biases`: Feature Transformer のバイアス
    /// - `accumulation`: 更新先のアキュムレータ値
    /// - `add_fn`: 重み加算関数
    /// - `sub_fn`: 重み減算関数
    pub(crate) fn refresh_or_cache<FA, FS>(
        &mut self,
        king_sq: Square,
        perspective: Color,
        active: &[u32],
        biases: &[i16; NNUE_PYTORCH_L1],
        accumulation: &mut [i16; NNUE_PYTORCH_L1],
        add_fn: FA,
        sub_fn: FS,
    ) where
        FA: Fn(&mut [i16; NNUE_PYTORCH_L1], usize),
        FS: Fn(&mut [i16; NNUE_PYTORCH_L1], usize),
    {
        let entry = &mut self.entries[king_sq.raw() as usize][perspective as usize];

        if entry.valid {
            // キャッシュが有効 → 差分更新
            // キャッシュのアキュムレータ値をコピー
            accumulation.copy_from_slice(&entry.accumulation);

            // ソート済み配列のマージベース差分（O(n)）
            let cached = &entry.active_indices[..entry.num_active as usize];
            Self::apply_diff(cached, active, accumulation, &add_fn, &sub_fn);
        } else {
            // キャッシュ無効 → バイアスから full refresh
            accumulation.copy_from_slice(biases);
            for &idx in active {
                add_fn(accumulation, idx as usize);
            }
        }

        // キャッシュを更新
        entry.accumulation.copy_from_slice(accumulation);
        let n = active.len().min(MAX_ACTIVE_FEATURES);
        entry.active_indices[..n].copy_from_slice(&active[..n]);
        entry.num_active = n as u16;
        entry.valid = true;
    }

    /// ソート済み配列のマージベース差分を適用
    ///
    /// cached と current を同時に走査し、差分（add/sub）のみを適用する。
    /// 両配列はソート済みであるため O(n+m) で完了する。
    #[inline]
    fn apply_diff<FA, FS>(
        cached: &[u32],
        current: &[u32],
        accumulation: &mut [i16; NNUE_PYTORCH_L1],
        add_fn: &FA,
        sub_fn: &FS,
    ) where
        FA: Fn(&mut [i16; NNUE_PYTORCH_L1], usize),
        FS: Fn(&mut [i16; NNUE_PYTORCH_L1], usize),
    {
        let mut ci = 0;
        let mut ni = 0;

        while ci < cached.len() && ni < current.len() {
            let c = cached[ci];
            let n = current[ni];
            if c < n {
                // cached にあって current にない → 削除
                sub_fn(accumulation, c as usize);
                ci += 1;
            } else if c > n {
                // current にあって cached にない → 追加
                add_fn(accumulation, n as usize);
                ni += 1;
            } else {
                // 両方にある → 変化なし
                ci += 1;
                ni += 1;
            }
        }

        // 残りの cached（削除）
        while ci < cached.len() {
            sub_fn(accumulation, cached[ci] as usize);
            ci += 1;
        }

        // 残りの current（追加）
        while ni < current.len() {
            add_fn(accumulation, current[ni] as usize);
            ni += 1;
        }
    }
}

impl Default for AccumulatorCacheLayerStacks {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// DirtyPiece - 駒の変更情報（LayerStacks用、accumulator.rsから再エクスポート）
// =============================================================================

// DirtyPiece は accumulator.rs で定義済み

// =============================================================================
// StackEntryLayerStacks - スタックエントリ
// =============================================================================

/// スタックエントリ（LayerStacks用）
pub struct StackEntryLayerStacks {
    /// アキュムレータ
    pub accumulator: AccumulatorLayerStacks,
    /// 変更された駒の情報
    pub dirty_piece: DirtyPiece,
    /// 直前のエントリインデックス（差分計算用）
    pub previous: Option<usize>,
    /// progress8kpabs の重み付き和（差分更新用）
    pub progress_sum: f32,
    /// progress_sum 計算済みフラグ
    pub computed_progress: bool,
}

impl StackEntryLayerStacks {
    pub fn new() -> Self {
        Self {
            accumulator: AccumulatorLayerStacks::new(),
            dirty_piece: DirtyPiece::default(),
            previous: None,
            progress_sum: 0.0,
            computed_progress: false,
        }
    }
}

impl Default for StackEntryLayerStacks {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// AccumulatorStackLayerStacks - スタック管理
// =============================================================================

/// アキュムレータスタック（LayerStacks用）
pub struct AccumulatorStackLayerStacks {
    /// スタックエントリ
    entries: Box<[StackEntryLayerStacks]>,
    /// 現在のインデックス
    current: usize,
}

impl AccumulatorStackLayerStacks {
    const STACK_SIZE: usize = (MAX_PLY as usize) + 16;

    /// 新規作成
    pub fn new() -> Self {
        let entries: Vec<StackEntryLayerStacks> =
            (0..Self::STACK_SIZE).map(|_| StackEntryLayerStacks::new()).collect();

        Self {
            entries: entries.into_boxed_slice(),
            current: 0,
        }
    }

    /// 現在のエントリを取得
    #[inline]
    pub fn current(&self) -> &StackEntryLayerStacks {
        debug_assert!(self.current < self.entries.len());
        // SAFETY: current は push/pop で管理され、常に entries.len() 未満。
        unsafe { self.entries.get_unchecked(self.current) }
    }

    /// 現在のエントリを取得（可変）
    #[inline]
    pub fn current_mut(&mut self) -> &mut StackEntryLayerStacks {
        debug_assert!(self.current < self.entries.len());
        // SAFETY: 同上。
        unsafe { self.entries.get_unchecked_mut(self.current) }
    }

    /// 現在のインデックスを取得
    #[inline]
    pub fn current_index(&self) -> usize {
        self.current
    }

    /// 指定インデックスのエントリを取得
    #[inline]
    pub fn entry_at(&self, index: usize) -> &StackEntryLayerStacks {
        &self.entries[index]
    }

    /// 指定インデックスのエントリを取得（可変）
    #[inline]
    pub fn entry_at_mut(&mut self, index: usize) -> &mut StackEntryLayerStacks {
        &mut self.entries[index]
    }

    /// スタックをプッシュ
    #[inline]
    pub fn push(&mut self) {
        let prev = self.current;
        self.current += 1;
        debug_assert!(self.current < Self::STACK_SIZE);
        // SAFETY: current < STACK_SIZE は上の debug_assert で検証。
        //         push は do_move ごとに 1 回呼ばれ、pop と対になるため
        //         current は常に STACK_SIZE 未満。
        let entry = unsafe { self.entries.get_unchecked_mut(self.current) };
        entry.previous = Some(prev);
        entry.accumulator.computed_accumulation = false;
        entry.accumulator.computed_score = false;
        entry.dirty_piece = DirtyPiece::default();
        entry.computed_progress = false;
    }

    /// スタックをポップ
    #[inline]
    pub fn pop(&mut self) {
        debug_assert!(self.current > 0);
        self.current -= 1;
    }

    /// 前回と現在のアキュムレータを同時に取得（clone不要）
    ///
    /// `split_at_mut`を使用して、prev_idx の accumulator への不変参照と
    /// 現在の accumulator への可変参照を同時に返す。
    #[inline]
    pub fn get_prev_and_current_accumulators(
        &mut self,
        prev_idx: usize,
    ) -> (&AccumulatorLayerStacks, &mut AccumulatorLayerStacks) {
        let cur_idx = self.current;
        debug_assert!(prev_idx < cur_idx, "prev_idx ({prev_idx}) must be < cur_idx ({cur_idx})");
        let (left, right) = self.entries.split_at_mut(cur_idx);
        (&left[prev_idx].accumulator, &mut right[0].accumulator)
    }

    /// スタックをリセット
    #[inline]
    pub fn reset(&mut self) {
        self.current = 0;
        self.entries[0].accumulator.computed_accumulation = false;
        self.entries[0].accumulator.computed_score = false;
        self.entries[0].previous = None;
        self.entries[0].computed_progress = false;
    }

    /// 祖先を辿って使用可能なアキュムレータを探す
    ///
    /// ## 実装方針
    ///
    /// アキュムレータの差分更新における祖先探索には複数のアプローチがある:
    ///
    /// - **YaneuraOu方式**: 1手前のみをチェック（シンプルだが差分更新の機会を逃す）
    /// - **Stockfish方式**: スタック全体を探索し、各ステップで玉移動をチェック
    ///
    /// このプロジェクトでは、HalfKP側（accumulator.rs）と同じロジックを採用している。
    /// 最大8手前まで探索し、各ステップで玉移動があれば即座に打ち切る方式である。
    /// この方式により、1手前限定より多くの差分更新機会を得つつ、玉移動時の
    /// 無駄な探索を早期に打ち切ることでNPS向上が観測されている。
    ///
    /// ## 戻り値
    ///
    /// `Some((計算済みエントリのインデックス, 経由する局面数))` - 玉移動がない範囲で
    /// 計算済み祖先が見つかった場合。`None` - 使用可能な祖先が見つからない場合。
    pub fn find_usable_accumulator(&self) -> Option<(usize, usize)> {
        // representative 4局面 x 2 rounds の search-only A/B では
        // MAX_DEPTH=4 が MAX_DEPTH=1 比で +2.15% だったため維持する。
        const MAX_DEPTH: usize = 4;

        let current = &self.entries[self.current];

        // 現局面で玉が動いていたら差分更新不可
        if current.dirty_piece.king_moved[0] || current.dirty_piece.king_moved[1] {
            return None;
        }

        // 直前局面をチェック（depth=1から開始）
        let mut prev_idx = current.previous?;
        let mut depth = 1;

        loop {
            let prev = &self.entries[prev_idx];

            // 計算済みなら成功
            if prev.accumulator.computed_accumulation {
                return Some((prev_idx, depth));
            }

            // 探索上限に達した
            if depth >= MAX_DEPTH {
                return None;
            }

            // さらに前の局面へ（ルートに達したらNone）
            let next_prev_idx = prev.previous?;

            // 玉が動いていたら打ち切り（早期終了による最適化）
            if prev.dirty_piece.king_moved[0] || prev.dirty_piece.king_moved[1] {
                return None;
            }

            prev_idx = next_prev_idx;
            depth += 1;
        }
    }

    /// 指定インデックスから現在位置までのパスを収集
    ///
    /// 戻り値:
    /// - Some(path): source_idx に到達できた場合、source側から適用する順のインデックス列
    /// - None: パスが途切れた場合、または MAX_PATH_LENGTH を超えた場合
    pub fn collect_path(&self, source_idx: usize) -> Option<IndexList<MAX_PATH_LENGTH>> {
        self.collect_path_internal(source_idx)
    }

    fn collect_path_internal(&self, source_idx: usize) -> Option<IndexList<MAX_PATH_LENGTH>> {
        let mut path = IndexList::new();
        let mut idx = self.current;

        while idx != source_idx {
            // パス長が上限を超えたら失敗
            if !path.push(idx) {
                return None;
            }
            match self.entries[idx].previous {
                Some(prev) => idx = prev,
                None => return None,
            }
        }

        path.reverse();
        Some(path)
    }
}

impl Default for AccumulatorStackLayerStacks {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_accumulator_new() {
        let acc = AccumulatorLayerStacks::new();
        assert!(!acc.computed_accumulation);
        assert_eq!(acc.accumulation[0].len(), NNUE_PYTORCH_L1);
    }

    #[test]
    fn test_stack_push_pop() {
        let mut stack = AccumulatorStackLayerStacks::new();
        assert_eq!(stack.current_index(), 0);

        stack.push();
        assert_eq!(stack.current_index(), 1);
        assert_eq!(stack.current().previous, Some(0));

        stack.pop();
        assert_eq!(stack.current_index(), 0);
    }

    #[test]
    fn test_cache_new_is_invalid() {
        let cache = AccumulatorCacheLayerStacks::new();
        // 全エントリが無効であることを確認
        for sq in 0..Square::NUM {
            // SAFETY: sq は 0..81 の範囲内であることが Square::NUM により保証
            let king_sq = unsafe { Square::from_u8_unchecked(sq as u8) };
            for perspective in [Color::Black, Color::White] {
                let entry = &cache.entries[king_sq.raw() as usize][perspective as usize];
                assert!(!entry.valid);
            }
        }
    }

    #[test]
    fn test_cache_invalidate() {
        let mut cache = AccumulatorCacheLayerStacks::new();
        // エントリを有効にする
        cache.entries[0][0].valid = true;
        cache.entries[40][1].valid = true;

        cache.invalidate();

        // 全エントリが無効になっていることを確認
        assert!(!cache.entries[0][0].valid);
        assert!(!cache.entries[40][1].valid);
    }

    /// AccumulatorCaches の apply_diff がソート済み配列の差分を正しく適用することを確認
    ///
    /// add_fn: idx を acc[0] に加算、sub_fn: idx を acc[0] から減算。
    /// これにより acc[0] の最終値で差分の正しさを検証する。
    #[test]
    fn test_apply_diff_basic() {
        let mut acc = [0i16; NNUE_PYTORCH_L1];
        // 初期値を設定（cached の重み合計 = 1+3+5 = 9）
        acc[0] = 9;

        // cached = [1, 3, 5], current = [2, 3, 4]
        // → remove 1,5 (-6)、add 2,4 (+6)（3 は共通で変化なし）
        let cached = [1u32, 3, 5];
        let current = [2u32, 3, 4];

        AccumulatorCacheLayerStacks::apply_diff(
            &cached,
            &current,
            &mut acc,
            &|a, idx| a[0] = a[0].wrapping_add(idx as i16),
            &|a, idx| a[0] = a[0].wrapping_sub(idx as i16),
        );

        // 9 - 1 - 5 + 2 + 4 = 9 (差分は対称のためたまたま同じ)
        // current の合計 = 2+3+4 = 9
        assert_eq!(acc[0], 9);
    }

    /// apply_diff: cached が空の場合（全て追加）
    #[test]
    fn test_apply_diff_all_added() {
        let mut acc = [0i16; NNUE_PYTORCH_L1];

        let cached: [u32; 0] = [];
        let current = [10u32, 20, 30];

        AccumulatorCacheLayerStacks::apply_diff(
            &cached,
            &current,
            &mut acc,
            &|a, idx| a[0] = a[0].wrapping_add(idx as i16),
            &|a, idx| a[0] = a[0].wrapping_sub(idx as i16),
        );

        // 0 + 10 + 20 + 30 = 60
        assert_eq!(acc[0], 60);
    }

    /// apply_diff: current が空の場合（全て削除）
    #[test]
    fn test_apply_diff_all_removed() {
        let mut acc = [0i16; NNUE_PYTORCH_L1];
        acc[0] = 60; // 初期値 = cached の合計

        let cached = [10u32, 20, 30];
        let current: [u32; 0] = [];

        AccumulatorCacheLayerStacks::apply_diff(
            &cached,
            &current,
            &mut acc,
            &|a, idx| a[0] = a[0].wrapping_add(idx as i16),
            &|a, idx| a[0] = a[0].wrapping_sub(idx as i16),
        );

        // 60 - 10 - 20 - 30 = 0
        assert_eq!(acc[0], 0);
    }

    /// apply_diff: 完全一致の場合（変化なし）
    #[test]
    fn test_apply_diff_identical() {
        let mut acc = [0i16; NNUE_PYTORCH_L1];
        acc[0] = 42;

        let cached = [1u32, 2, 3, 4, 5];
        let current = [1u32, 2, 3, 4, 5];

        AccumulatorCacheLayerStacks::apply_diff(
            &cached,
            &current,
            &mut acc,
            &|a, idx| a[0] = a[0].wrapping_add(idx as i16),
            &|a, idx| a[0] = a[0].wrapping_sub(idx as i16),
        );

        // 変化なしなので 42 のまま
        assert_eq!(acc[0], 42);
    }

    /// refresh_or_cache: 最初のキャッシュミス → full refresh → キャッシュ保存
    #[test]
    fn test_refresh_or_cache_cold_start() {
        let mut cache = AccumulatorCacheLayerStacks::new();
        let king_sq = Square::SQ_55; // 5五
        let perspective = Color::Black;

        let mut biases = [0i16; NNUE_PYTORCH_L1];
        biases[0] = 100;
        biases[1] = 200;

        let active = [5u32, 10, 15]; // ダミーの特徴量
        let mut accumulation = [0i16; NNUE_PYTORCH_L1];

        // 加算関数: index を accumulation[0] に加算（テスト用簡略化）
        cache.refresh_or_cache(
            king_sq,
            perspective,
            &active,
            &biases,
            &mut accumulation,
            |acc, idx| acc[0] = acc[0].wrapping_add(idx as i16),
            |acc, idx| acc[0] = acc[0].wrapping_sub(idx as i16),
        );

        // biases[0] + 5 + 10 + 15 = 130
        assert_eq!(accumulation[0], 130);
        // biases[1] はそのまま
        assert_eq!(accumulation[1], 200);

        // キャッシュが有効になっていることを確認
        let entry = &cache.entries[king_sq.raw() as usize][perspective as usize];
        assert!(entry.valid);
        assert_eq!(entry.num_active, 3);
    }

    /// refresh_or_cache: 2回目のキャッシュヒット → 差分更新
    #[test]
    fn test_refresh_or_cache_hit() {
        let mut cache = AccumulatorCacheLayerStacks::new();
        let king_sq = Square::SQ_55;
        let perspective = Color::Black;

        let biases = [0i16; NNUE_PYTORCH_L1];

        // 1回目: active = [5, 10, 15]
        let active1 = [5u32, 10, 15];
        let mut acc1 = [0i16; NNUE_PYTORCH_L1];
        cache.refresh_or_cache(
            king_sq,
            perspective,
            &active1,
            &biases,
            &mut acc1,
            |acc, idx| acc[0] = acc[0].wrapping_add(idx as i16),
            |acc, idx| acc[0] = acc[0].wrapping_sub(idx as i16),
        );
        // acc1[0] = 0 + 5 + 10 + 15 = 30
        assert_eq!(acc1[0], 30);

        // 2回目: active = [5, 10, 20] （15→20 に変化）
        let active2 = [5u32, 10, 20];
        let mut acc2 = [0i16; NNUE_PYTORCH_L1];
        cache.refresh_or_cache(
            king_sq,
            perspective,
            &active2,
            &biases,
            &mut acc2,
            |acc, idx| acc[0] = acc[0].wrapping_add(idx as i16),
            |acc, idx| acc[0] = acc[0].wrapping_sub(idx as i16),
        );
        // キャッシュヒット: acc1[0] - 15 + 20 = 30 - 15 + 20 = 35
        assert_eq!(acc2[0], 35);
    }
}
