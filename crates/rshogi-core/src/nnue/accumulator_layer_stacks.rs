//! AccumulatorLayerStacks - LayerStacksアーキテクチャ用アキュムレータ
//!
//! LayerStacks の Feature Transformer は各視点で L1 次元を出力する。
//! L1 は const generics で 1536 / 768 等を切り替え可能。
//! 既存の Accumulator（256次元、HalfKP用）とは別に管理する。

use super::accumulator::{DirtyPiece, IndexList, MAX_PATH_LENGTH};
use super::bona_piece::BonaPiece;
#[cfg(feature = "nnue-psqt")]
use super::constants::NUM_LAYER_STACK_BUCKETS;
use super::piece_list::PieceNumber;
use crate::types::{Color, MAX_PLY, Square};

/// LayerStacks用アキュムレータ（L1次元）
#[repr(C, align(64))]
#[derive(Clone)]
pub struct AccumulatorLayerStacks<const L1: usize> {
    /// 各視点の累積値 [perspective][dimension]
    /// perspective: 0 = Black, 1 = White
    pub accumulation: [[i16; L1]; 2],

    /// Threat アキュムレータ [perspective][dimension]
    /// Threat weights (i8) の累積値。評価時に piece accumulation と加算して SCReLU に入力する。
    #[cfg(feature = "nnue-threat")]
    pub threat_accumulation: [[i16; L1]; 2],

    /// PSQT アキュムレータ [perspective][bucket]
    /// 各駒の PSQT 重みを視点ごとに累積する。
    #[cfg(feature = "nnue-psqt")]
    pub psqt_accumulation: [[i32; NUM_LAYER_STACK_BUCKETS]; 2],

    /// 計算済みフラグ
    pub computed_accumulation: bool,

    /// スコア計算済みフラグ（差分更新時にリセット）
    pub computed_score: bool,
}

impl<const L1: usize> AccumulatorLayerStacks<L1> {
    /// 新規作成
    pub fn new() -> Self {
        Self {
            accumulation: [[0; L1]; 2],
            #[cfg(feature = "nnue-threat")]
            threat_accumulation: [[0; L1]; 2],
            #[cfg(feature = "nnue-psqt")]
            psqt_accumulation: [[0; NUM_LAYER_STACK_BUCKETS]; 2],
            computed_accumulation: false,
            computed_score: false,
        }
    }

    /// 指定視点の累積値を取得
    #[inline]
    pub fn get(&self, perspective: usize) -> &[i16; L1] {
        debug_assert!(perspective < 2);
        // SAFETY: perspective は Color::Black(0) または Color::White(1) であり、
        //         accumulation は [_; 2] なので常に範囲内。
        unsafe { self.accumulation.get_unchecked(perspective) }
    }

    /// 指定視点の累積値を取得（可変）
    #[inline]
    pub fn get_mut(&mut self, perspective: usize) -> &mut [i16; L1] {
        debug_assert!(perspective < 2);
        // SAFETY: 同上。
        unsafe { self.accumulation.get_unchecked_mut(perspective) }
    }

    /// 指定視点の Threat 累積値を取得
    #[cfg(feature = "nnue-threat")]
    #[inline]
    pub fn get_threat(&self, perspective: usize) -> &[i16; L1] {
        debug_assert!(perspective < 2);
        unsafe { self.threat_accumulation.get_unchecked(perspective) }
    }

    /// 指定視点の Threat 累積値を取得（可変）
    #[cfg(feature = "nnue-threat")]
    #[inline]
    pub fn get_threat_mut(&mut self, perspective: usize) -> &mut [i16; L1] {
        debug_assert!(perspective < 2);
        unsafe { self.threat_accumulation.get_unchecked_mut(perspective) }
    }
}

impl<const L1: usize> Default for AccumulatorLayerStacks<L1> {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// AccumulatorCacheLayerStacks - Finny Tables（玉位置×視点キャッシュ）
// =============================================================================

/// AccumulatorCaches のキャッシュエントリ（Finny Tables、Stockfish 風 piece_list 差分）
///
/// 各玉位置×視点ごとに、最後に計算したアキュムレータ値と、その時点の
/// `PieceList` (perspective 固有の `piece_list_fb` / `piece_list_fw`) を保持。
/// refresh 時に現在の `PieceList` との slot-wise 比較で差分駒を抽出し、
/// 差分のみ add/sub でアキュムレータを更新することで、
/// `append_active_indices` + `sort_unstable` のオーバーヘッドを回避する。
#[repr(C, align(64))]
struct AccCacheEntry<const L1: usize> {
    /// キャッシュされたアキュムレータ値
    accumulation: [i16; L1],
    /// キャッシュ時点の `PieceList`（perspective 固有の fb または fw 配列）
    ///
    /// `PieceNumber::NB = 40` 固定長。BonaPiece::ZERO は slot 未使用を表す。
    piece_list: [BonaPiece; PieceNumber::NB],
    /// 有効フラグ
    valid: bool,
}

impl<const L1: usize> AccCacheEntry<L1> {
    /// 無効な初期状態で作成
    fn new_invalid() -> Self {
        Self {
            accumulation: [0; L1],
            piece_list: [BonaPiece::ZERO; PieceNumber::NB],
            valid: false,
        }
    }
}

/// 玉位置×視点ごとのアキュムレータキャッシュ（Finny Tables）
///
/// 81マス × 2視点 = 162 エントリ。
/// 玉が移動して full refresh が必要な場合に、前回同じ玉位置で計算した
/// アキュムレータとの差分のみを適用することで高速化する。
pub struct AccumulatorCacheLayerStacks<const L1: usize> {
    /// [king_sq][perspective] のキャッシュエントリ
    entries: Box<[[AccCacheEntry<L1>; 2]; Square::NUM]>,
}

impl<const L1: usize> AccumulatorCacheLayerStacks<L1> {
    /// 新規作成（全エントリ無効）
    pub fn new() -> Self {
        // Box で確保（162 × エントリサイズはスタックに収まらないため）
        let entries: Vec<[AccCacheEntry<L1>; 2]> = (0..Square::NUM)
            .map(|_| [AccCacheEntry::new_invalid(), AccCacheEntry::new_invalid()])
            .collect();
        // SAFETY: Vec の長さが Square::NUM であることを保証
        let boxed: Box<[[AccCacheEntry<L1>; 2]]> = entries.into_boxed_slice();
        // SAFETY: Square::NUM == 81 なので配列サイズと一致する
        let ptr = Box::into_raw(boxed) as *mut [[AccCacheEntry<L1>; 2]; Square::NUM];
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

    /// キャッシュからの差分で refresh を実行（Stockfish 風 piece_list 差分方式）
    ///
    /// キャッシュが有効な場合、現在の `PieceList` と cache の `PieceList` を
    /// slot-wise に比較し、変化した slot のみ `idx_fn` で feature index を
    /// 算出して add/sub を適用する。
    /// キャッシュが無効な場合は biases から full refresh し、cache を更新する。
    ///
    /// # 引数
    ///
    /// - `king_sq`: この視点の玉位置（cache key）
    /// - `perspective`: 視点（cache key）
    /// - `piece_list`: 現在の perspective 固有 `PieceList`（`piece_list_fb` / `piece_list_fw`）
    /// - `biases`: Feature Transformer のバイアス
    /// - `accumulation`: 更新先のアキュムレータ値
    /// - `idx_fn`: BonaPiece (非 ZERO) → feature index の変換
    /// - `add_fn`: 重み加算関数
    /// - `sub_fn`: 重み減算関数
    pub(crate) fn refresh_or_cache<FI, FA, FS>(
        &mut self,
        king_sq: Square,
        perspective: Color,
        piece_list: &[BonaPiece; PieceNumber::NB],
        biases: &[i16; L1],
        accumulation: &mut [i16; L1],
        idx_fn: FI,
        add_fn: FA,
        sub_fn: FS,
    ) where
        FI: Fn(BonaPiece) -> usize,
        FA: Fn(&mut [i16; L1], usize),
        FS: Fn(&mut [i16; L1], usize),
    {
        let entry = &mut self.entries[king_sq.raw() as usize][perspective as usize];

        if entry.valid {
            crate::nnue::stats::count_cache_hit!();
            // キャッシュのアキュムレータ値をコピー
            accumulation.copy_from_slice(&entry.accumulation);

            // 40 slot 比較して変化した slot のみ差分適用
            let mut diff_count = 0usize;
            for (cached_bp, &current_bp) in entry.piece_list.iter().copied().zip(piece_list.iter())
            {
                if cached_bp != current_bp {
                    if cached_bp != BonaPiece::ZERO {
                        sub_fn(accumulation, idx_fn(cached_bp));
                        diff_count += 1;
                    }
                    if current_bp != BonaPiece::ZERO {
                        add_fn(accumulation, idx_fn(current_bp));
                        diff_count += 1;
                    }
                }
            }
            crate::nnue::stats::count_refresh_diff!(diff_count);
        } else {
            crate::nnue::stats::count_cache_miss!();
            // キャッシュ無効 → バイアスから full refresh
            accumulation.copy_from_slice(biases);
            for &bp in piece_list.iter() {
                if bp != BonaPiece::ZERO {
                    add_fn(accumulation, idx_fn(bp));
                }
            }
        }

        // キャッシュを更新
        entry.accumulation.copy_from_slice(accumulation);
        entry.piece_list.copy_from_slice(piece_list);
        entry.valid = true;
    }
}

impl<const L1: usize> Default for AccumulatorCacheLayerStacks<L1> {
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
pub struct StackEntryLayerStacks<const L1: usize> {
    /// アキュムレータ
    pub accumulator: AccumulatorLayerStacks<L1>,
    /// 変更された駒の情報
    pub dirty_piece: DirtyPiece,
    /// 直前のエントリインデックス（差分計算用）
    pub previous: Option<usize>,
    /// progress8kpabs の重み付き和（差分更新用）
    #[cfg(feature = "nnue-progress-diff")]
    pub progress_sum: f32,
    /// progress_sum 計算済みフラグ
    #[cfg(feature = "nnue-progress-diff")]
    pub computed_progress: bool,
}

impl<const L1: usize> StackEntryLayerStacks<L1> {
    pub fn new() -> Self {
        Self {
            accumulator: AccumulatorLayerStacks::new(),
            dirty_piece: DirtyPiece::default(),
            previous: None,
            #[cfg(feature = "nnue-progress-diff")]
            progress_sum: 0.0,
            #[cfg(feature = "nnue-progress-diff")]
            computed_progress: false,
        }
    }
}

impl<const L1: usize> Default for StackEntryLayerStacks<L1> {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// AccumulatorStackLayerStacks - スタック管理
// =============================================================================

/// アキュムレータスタック（LayerStacks用）
pub struct AccumulatorStackLayerStacks<const L1: usize> {
    /// スタックエントリ
    entries: Box<[StackEntryLayerStacks<L1>]>,
    /// 現在のインデックス
    current: usize,
}

impl<const L1: usize> AccumulatorStackLayerStacks<L1> {
    const STACK_SIZE: usize = (MAX_PLY as usize) + 16;

    /// 新規作成
    pub fn new() -> Self {
        let entries: Vec<StackEntryLayerStacks<L1>> =
            (0..Self::STACK_SIZE).map(|_| StackEntryLayerStacks::new()).collect();

        Self {
            entries: entries.into_boxed_slice(),
            current: 0,
        }
    }

    /// 現在のエントリを取得
    #[inline]
    pub fn current(&self) -> &StackEntryLayerStacks<L1> {
        debug_assert!(self.current < self.entries.len());
        // SAFETY: current は push/pop の対でインクリメント/デクリメントされ、
        //         do_move と undo_move の対称呼び出しにより 0 <= current < STACK_SIZE が保証される。
        unsafe { self.entries.get_unchecked(self.current) }
    }

    /// 現在のエントリを取得（可変）
    #[inline]
    pub fn current_mut(&mut self) -> &mut StackEntryLayerStacks<L1> {
        debug_assert!(self.current < self.entries.len());
        // SAFETY: 同上。do_move/undo_move の対称呼び出しで current は常に範囲内。
        unsafe { self.entries.get_unchecked_mut(self.current) }
    }

    /// 現在のインデックスを取得
    #[inline]
    pub fn current_index(&self) -> usize {
        self.current
    }

    /// 指定インデックスのエントリを取得
    #[inline]
    pub(crate) fn entry_at(&self, index: usize) -> &StackEntryLayerStacks<L1> {
        debug_assert!(index < self.entries.len());
        // SAFETY: index は previous チェーンまたは find_usable_accumulator 由来で常に
        //         current 以下の有効なインデックス（STACK_SIZE 未満）。
        unsafe { self.entries.get_unchecked(index) }
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
        #[cfg(feature = "nnue-progress-diff")]
        {
            entry.computed_progress = false;
        }
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
    ) -> (&AccumulatorLayerStacks<L1>, &mut AccumulatorLayerStacks<L1>) {
        let cur_idx = self.current;
        debug_assert!(prev_idx < cur_idx, "prev_idx ({prev_idx}) must be < cur_idx ({cur_idx})");
        debug_assert!(cur_idx < self.entries.len());
        let (left, right) = self.entries.split_at_mut(cur_idx);
        // SAFETY: prev_idx < cur_idx（上の debug_assert で検証）かつ left の長さは cur_idx。
        //         right は少なくとも 1 要素を持つ（cur_idx < entries.len() を保証）。
        unsafe {
            (
                &left.get_unchecked(prev_idx).accumulator,
                &mut right.get_unchecked_mut(0).accumulator,
            )
        }
    }

    /// スタックをリセット
    #[inline]
    pub fn reset(&mut self) {
        self.current = 0;
        self.entries[0].accumulator.computed_accumulation = false;
        self.entries[0].accumulator.computed_score = false;
        self.entries[0].previous = None;
        #[cfg(feature = "nnue-progress-diff")]
        {
            self.entries[0].computed_progress = false;
        }
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

        debug_assert!(self.current < self.entries.len());
        // SAFETY: current は do_move/undo_move の対称呼び出しで常に範囲内。
        let current = unsafe { self.entries.get_unchecked(self.current) };

        // 現局面で玉が動いていたら差分更新不可
        if current.dirty_piece.king_moved[0] || current.dirty_piece.king_moved[1] {
            return None;
        }

        // 直前局面をチェック（depth=1から開始）
        let mut prev_idx = current.previous?;
        let mut depth = 1;

        loop {
            debug_assert!(prev_idx < self.entries.len());
            // SAFETY: prev_idx は previous チェーンを辿った有効なインデックス（STACK_SIZE 未満）。
            let prev = unsafe { self.entries.get_unchecked(prev_idx) };

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

impl<const L1: usize> Default for AccumulatorStackLayerStacks<L1> {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// LayerStacksAccStack - L1 サイズ dispatch enum
// =============================================================================

/// LayerStacks アキュムレータスタックの L1 サイズ dispatch enum
///
/// Cargo feature `layerstacks-1536` / `layerstacks-768` で有効なバリアントが制御される。
pub enum LayerStacksAccStack {
    #[cfg(feature = "layerstacks-1536")]
    L1536(AccumulatorStackLayerStacks<1536>),
    #[cfg(feature = "layerstacks-768")]
    L768(AccumulatorStackLayerStacks<768>),
    #[cfg(feature = "layerstacks-512")]
    L512(AccumulatorStackLayerStacks<512>),
}

/// LayerStacks dispatch match の網羅性を確保するマクロ
///
/// 全 feature が無効の場合（WASM ビルド等）は空 enum のためインスタンスが存在しないが、
/// コンパイルを通すために wildcard arm を生成する。
macro_rules! ls_match {
    ($val:expr, $pat:ident => $body:expr) => {
        match $val {
            #[cfg(feature = "layerstacks-1536")]
            Self::L1536($pat) => $body,
            #[cfg(feature = "layerstacks-768")]
            Self::L768($pat) => $body,
            #[cfg(feature = "layerstacks-512")]
            Self::L512($pat) => $body,
            #[cfg(not(any(
                feature = "layerstacks-1536",
                feature = "layerstacks-768",
                feature = "layerstacks-512"
            )))]
            _ => unreachable!("no LayerStacks variant enabled"),
        }
    };
}

impl LayerStacksAccStack {
    /// L1 サイズを取得
    pub fn l1_size(&self) -> usize {
        match self {
            #[cfg(feature = "layerstacks-1536")]
            Self::L1536(_) => 1536,
            #[cfg(feature = "layerstacks-768")]
            Self::L768(_) => 768,
            #[cfg(feature = "layerstacks-512")]
            Self::L512(_) => 512,
            #[cfg(not(any(
                feature = "layerstacks-1536",
                feature = "layerstacks-768",
                feature = "layerstacks-512"
            )))]
            _ => unreachable!("no LayerStacks variant enabled"),
        }
    }

    /// スタックをリセット
    #[inline]
    pub fn reset(&mut self) {
        ls_match!(self, s => s.reset());
    }

    /// do_move 時にスタックをプッシュ
    #[inline]
    pub fn push(&mut self) {
        ls_match!(self, s => s.push());
    }

    /// undo_move 時にスタックをポップ
    #[inline]
    pub fn pop(&mut self) {
        ls_match!(self, s => s.pop());
    }

    /// 現在のエントリの dirty_piece を設定
    #[inline]
    pub fn set_current_dirty_piece(&mut self, dirty: DirtyPiece) {
        ls_match!(self, s => s.current_mut().dirty_piece = dirty);
    }
}

// =============================================================================
// LayerStacksAccCache - L1 サイズ dispatch enum
// =============================================================================

/// LayerStacks アキュムレータキャッシュの L1 サイズ dispatch enum
pub enum LayerStacksAccCache {
    #[cfg(feature = "layerstacks-1536")]
    L1536(AccumulatorCacheLayerStacks<1536>),
    #[cfg(feature = "layerstacks-768")]
    L768(AccumulatorCacheLayerStacks<768>),
    #[cfg(feature = "layerstacks-512")]
    L512(AccumulatorCacheLayerStacks<512>),
}

impl LayerStacksAccCache {
    /// この cache の L1 サイズを返す
    ///
    /// ネットワーク reload で L1 バリアントが変わったときに、旧 cache を
    /// 使い回してしまう事故を防ぐため、`prepare_search` 側で
    /// `network.l1_size()` と比較するために使う。
    pub fn l1_size(&self) -> usize {
        match self {
            #[cfg(feature = "layerstacks-1536")]
            Self::L1536(_) => 1536,
            #[cfg(feature = "layerstacks-768")]
            Self::L768(_) => 768,
            #[cfg(feature = "layerstacks-512")]
            Self::L512(_) => 512,
            #[cfg(not(any(
                feature = "layerstacks-1536",
                feature = "layerstacks-768",
                feature = "layerstacks-512"
            )))]
            _ => unreachable!("no LayerStacks variant enabled"),
        }
    }

    /// 全エントリを無効化
    pub fn invalidate(&mut self) {
        match self {
            #[cfg(feature = "layerstacks-1536")]
            Self::L1536(c) => c.invalidate(),
            #[cfg(feature = "layerstacks-768")]
            Self::L768(c) => c.invalidate(),
            #[cfg(feature = "layerstacks-512")]
            Self::L512(c) => c.invalidate(),
            #[cfg(not(any(
                feature = "layerstacks-1536",
                feature = "layerstacks-768",
                feature = "layerstacks-512"
            )))]
            _ => unreachable!("no LayerStacks variant enabled"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nnue::constants::NNUE_PYTORCH_L1;

    /// テスト用の具体的な L1 サイズ
    const TEST_L1: usize = NNUE_PYTORCH_L1; // 1536

    #[test]
    fn test_accumulator_new() {
        let acc = AccumulatorLayerStacks::<TEST_L1>::new();
        assert!(!acc.computed_accumulation);
        assert_eq!(acc.accumulation[0].len(), TEST_L1);
    }

    #[test]
    fn test_stack_push_pop() {
        let mut stack = AccumulatorStackLayerStacks::<TEST_L1>::new();
        assert_eq!(stack.current_index(), 0);

        stack.push();
        assert_eq!(stack.current_index(), 1);
        assert_eq!(stack.current().previous, Some(0));

        stack.pop();
        assert_eq!(stack.current_index(), 0);
    }

    #[test]
    fn test_cache_new_is_invalid() {
        let cache = AccumulatorCacheLayerStacks::<TEST_L1>::new();
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
        let mut cache = AccumulatorCacheLayerStacks::<TEST_L1>::new();
        // エントリを有効にする
        cache.entries[0][0].valid = true;
        cache.entries[40][1].valid = true;

        cache.invalidate();

        // 全エントリが無効になっていることを確認
        assert!(!cache.entries[0][0].valid);
        assert!(!cache.entries[40][1].valid);
    }

    /// refresh_or_cache: cold start → full refresh → cache に保存
    #[test]
    fn test_refresh_or_cache_cold_start() {
        let mut cache = AccumulatorCacheLayerStacks::<TEST_L1>::new();
        let king_sq = Square::SQ_55;
        let perspective = Color::Black;

        let mut biases = [0i16; TEST_L1];
        biases[0] = 100;
        biases[1] = 200;

        // ダミーの piece_list: slot 0-2 に非 ZERO BonaPiece
        let mut piece_list = [BonaPiece::ZERO; PieceNumber::NB];
        piece_list[0] = BonaPiece(5);
        piece_list[1] = BonaPiece(10);
        piece_list[2] = BonaPiece(15);

        let mut accumulation = [0i16; TEST_L1];
        cache.refresh_or_cache(
            king_sq,
            perspective,
            &piece_list,
            &biases,
            &mut accumulation,
            |bp| bp.0 as usize,
            |acc, idx| acc[0] = acc[0].wrapping_add(idx as i16),
            |acc, idx| acc[0] = acc[0].wrapping_sub(idx as i16),
        );

        // biases[0] + 5 + 10 + 15 = 130
        assert_eq!(accumulation[0], 130);
        assert_eq!(accumulation[1], 200);

        let entry = &cache.entries[king_sq.raw() as usize][perspective as usize];
        assert!(entry.valid);
        assert_eq!(entry.piece_list[0], BonaPiece(5));
        assert_eq!(entry.piece_list[1], BonaPiece(10));
        assert_eq!(entry.piece_list[2], BonaPiece(15));
    }

    /// refresh_or_cache: 2 回目のキャッシュヒット → piece_list 差分更新
    #[test]
    fn test_refresh_or_cache_hit() {
        let mut cache = AccumulatorCacheLayerStacks::<TEST_L1>::new();
        let king_sq = Square::SQ_55;
        let perspective = Color::Black;
        let biases = [0i16; TEST_L1];

        // 1 回目: slot 0-2 = [5, 10, 15]
        let mut pl1 = [BonaPiece::ZERO; PieceNumber::NB];
        pl1[0] = BonaPiece(5);
        pl1[1] = BonaPiece(10);
        pl1[2] = BonaPiece(15);
        let mut acc1 = [0i16; TEST_L1];
        cache.refresh_or_cache(
            king_sq,
            perspective,
            &pl1,
            &biases,
            &mut acc1,
            |bp| bp.0 as usize,
            |acc, idx| acc[0] = acc[0].wrapping_add(idx as i16),
            |acc, idx| acc[0] = acc[0].wrapping_sub(idx as i16),
        );
        assert_eq!(acc1[0], 30);

        // 2 回目: slot 2 の 15 → 20 に変化
        let mut pl2 = pl1;
        pl2[2] = BonaPiece(20);
        let mut acc2 = [0i16; TEST_L1];
        cache.refresh_or_cache(
            king_sq,
            perspective,
            &pl2,
            &biases,
            &mut acc2,
            |bp| bp.0 as usize,
            |acc, idx| acc[0] = acc[0].wrapping_add(idx as i16),
            |acc, idx| acc[0] = acc[0].wrapping_sub(idx as i16),
        );
        // hit: 30 - 15 + 20 = 35
        assert_eq!(acc2[0], 35);
    }

    /// refresh_or_cache: slot 消滅 (capture)
    #[test]
    fn test_refresh_or_cache_slot_disappears() {
        let mut cache = AccumulatorCacheLayerStacks::<TEST_L1>::new();
        let king_sq = Square::SQ_55;
        let perspective = Color::Black;
        let biases = [0i16; TEST_L1];

        let mut pl1 = [BonaPiece::ZERO; PieceNumber::NB];
        pl1[0] = BonaPiece(5);
        pl1[1] = BonaPiece(10);
        let mut acc1 = [0i16; TEST_L1];
        cache.refresh_or_cache(
            king_sq,
            perspective,
            &pl1,
            &biases,
            &mut acc1,
            |bp| bp.0 as usize,
            |acc, idx| acc[0] = acc[0].wrapping_add(idx as i16),
            |acc, idx| acc[0] = acc[0].wrapping_sub(idx as i16),
        );
        assert_eq!(acc1[0], 15);

        // slot 1 が ZERO になった
        let mut pl2 = pl1;
        pl2[1] = BonaPiece::ZERO;
        let mut acc2 = [0i16; TEST_L1];
        cache.refresh_or_cache(
            king_sq,
            perspective,
            &pl2,
            &biases,
            &mut acc2,
            |bp| bp.0 as usize,
            |acc, idx| acc[0] = acc[0].wrapping_add(idx as i16),
            |acc, idx| acc[0] = acc[0].wrapping_sub(idx as i16),
        );
        // hit: 15 - 10 = 5
        assert_eq!(acc2[0], 5);
    }
}
