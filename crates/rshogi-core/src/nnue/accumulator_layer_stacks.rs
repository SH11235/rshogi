//! AccumulatorLayerStacks - LayerStacksアーキテクチャ用アキュムレータ
//!
//! LayerStacks の Feature Transformer は各視点で L1 次元を出力する。
//! L1 は const generics で 1536 / 768 等を切り替え可能。
//! 既存の Accumulator（256次元、HalfKP用）とは別に管理する。

use super::accumulator::{DirtyPiece, IndexList, MAX_PATH_LENGTH};
use super::bona_piece::BonaPiece;
#[cfg(feature = "nnue-psqt")]
use super::constants::MAX_LAYER_STACK_BUCKETS;
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
    pub psqt_accumulation: [[i32; MAX_LAYER_STACK_BUCKETS]; 2],

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
            psqt_accumulation: [[0; MAX_LAYER_STACK_BUCKETS]; 2],
            computed_accumulation: false,
            computed_score: false,
        }
    }

    /// 指定視点の累積値を取得。
    ///
    /// # Panics
    /// `perspective` が0（先手）・1（後手）以外の場合。
    #[inline]
    pub fn get(&self, perspective: usize) -> &[i16; L1] {
        &self.accumulation[perspective]
    }

    /// 指定視点の累積値を取得（可変）。
    ///
    /// # Panics
    /// `perspective` が0（先手）・1（後手）以外の場合。
    #[inline]
    pub fn get_mut(&mut self, perspective: usize) -> &mut [i16; L1] {
        &mut self.accumulation[perspective]
    }

    /// 指定視点の Threat 累積値を取得。
    ///
    /// # Panics
    /// `perspective` が0（先手）・1（後手）以外の場合。
    #[cfg(feature = "nnue-threat")]
    #[inline]
    pub fn get_threat(&self, perspective: usize) -> &[i16; L1] {
        &self.threat_accumulation[perspective]
    }

    /// 指定視点の Threat 累積値を取得（可変）。
    ///
    /// # Panics
    /// `perspective` が0（先手）・1（後手）以外の場合。
    #[cfg(feature = "nnue-threat")]
    #[inline]
    pub fn get_threat_mut(&mut self, perspective: usize) -> &mut [i16; L1] {
        &mut self.threat_accumulation[perspective]
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
///
/// `nnue-psqt` feature 有効時は PSQT アキュムレータも同時にキャッシュし、
/// 玉移動時の PSQT フル再計算（40 駒 × num_buckets bucket = 数百 i32 加算）を
/// slot 差分（通常 0〜数 slot）に置き換える。
#[repr(C, align(64))]
struct AccCacheEntry<const L1: usize> {
    /// キャッシュされたアキュムレータ値
    ///
    /// `refresh_or_cache` がこの field を aligned SIMD load/store を使う
    /// add/sub weight 関数へ直接渡すため、struct の `align(64)` と field 先頭
    /// 配置 (offset 0) が 64-byte アライメントの前提として load-bearing。
    accumulation: [i16; L1],
    /// キャッシュされた PSQT アキュムレータ値
    ///
    /// `refresh_or_cache_with_psqt` の `add_psqt_fn` / `sub_psqt_fn` で
    /// main acc と同一の差分タイミングで更新される。
    ///
    /// メモリフットプリント: `nnue-psqt` 有効時、各エントリは
    /// `accumulation (2 × L1 bytes)` + `psqt_accumulation (36 bytes)` +
    /// `piece_list (40 × 2 bytes)` + `valid (1 byte)` で構成され、64-byte
    /// 境界にアライメントされる。L1=1536 で約 3,188 bytes + パディング。
    /// `Square::NUM = 81` × 2 perspective = 162 エントリ ≈ 520 KB。
    #[cfg(feature = "nnue-psqt")]
    psqt_accumulation: [i32; MAX_LAYER_STACK_BUCKETS],
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
            #[cfg(feature = "nnue-psqt")]
            psqt_accumulation: [0; MAX_LAYER_STACK_BUCKETS],
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

    /// キャッシュ差分のfeature indexをまとめて適用するrefresh。
    ///
    /// add/subを1件ずつ呼ぶ代わりに、全indexを固定長listへ集めて`apply_fn`へ渡す。
    /// Feature Transformer側はこのlistを使い、accumulatorをtileごとに1回だけ
    /// load/storeできる。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn refresh_or_cache<FI, FApply>(
        &mut self,
        king_sq: Square,
        perspective: Color,
        piece_list: &[BonaPiece; PieceNumber::NB],
        biases: &[i16; L1],
        accumulation: &mut [i16; L1],
        idx_fn: FI,
        apply_fn: FApply,
    ) where
        FI: Fn(BonaPiece) -> usize,
        FApply:
            Fn(&mut [i16; L1], &IndexList<{ PieceNumber::NB }>, &IndexList<{ PieceNumber::NB }>),
    {
        let entry = &mut self.entries[king_sq.raw() as usize][perspective as usize];
        // apply_fn (tiled SIMD) は aligned load/store を使うため、entry.accumulation を
        // 直接渡すには AccCacheEntry の align(64) + field 先頭配置が必須。
        debug_assert_eq!(entry.accumulation.as_ptr() as usize % 64, 0);
        // 各駒slotはremoved/addedへ最大1件ずつ入るため、容量40を超えない。
        let mut removed = IndexList::<{ PieceNumber::NB }>::new();
        let mut added = IndexList::<{ PieceNumber::NB }>::new();

        let was_valid = entry.valid;
        // entry.accumulation を作業領域として直接更新するため、差分適用の途中で
        // unwind すると entry が不整合になる。valid を先に落とし、全 field の
        // 書き戻し完了後に立て直すことで半更新 entry の再利用を防ぐ。
        entry.valid = false;

        if was_valid {
            crate::nnue::stats::count_cache_hit!();
            // entry を作業領域にすることで、cache hit 時の L1 要素全量コピーを
            // 最後の entry→accumulation 1回だけにする。差分indexはlistへ集めて
            // 後段の apply_fn でtile一括適用する。
            for (cached_bp, &current_bp) in entry.piece_list.iter().copied().zip(piece_list.iter())
            {
                if cached_bp != current_bp {
                    if cached_bp != BonaPiece::ZERO {
                        let pushed = removed.push(idx_fn(cached_bp));
                        debug_assert!(pushed);
                    }
                    if current_bp != BonaPiece::ZERO {
                        let pushed = added.push(idx_fn(current_bp));
                        debug_assert!(pushed);
                    }
                }
            }
            crate::nnue::stats::count_refresh_diff!(removed.len() + added.len());
        } else {
            crate::nnue::stats::count_cache_miss!();
            // キャッシュ無効 → バイアスから full refresh
            entry.accumulation.copy_from_slice(biases);
            for &bp in piece_list.iter() {
                if bp != BonaPiece::ZERO {
                    let pushed = added.push(idx_fn(bp));
                    debug_assert!(pushed);
                }
            }
        }

        apply_fn(&mut entry.accumulation, &removed, &added);

        // 更新済みcache entryを探索stack側へ公開する。
        accumulation.copy_from_slice(&entry.accumulation);
        entry.piece_list.copy_from_slice(piece_list);
        entry.valid = true;
    }

    /// PSQT 付きキャッシュ refresh（Stockfish 風 piece_list 差分方式）
    ///
    /// main acc と PSQT acc を同一の slot 差分で更新する。各 slot 差分につき
    /// main FT の add/sub に加えて PSQT の add/sub（9-i32 SIMD）を呼ぶ。
    /// cache miss 時は両方を biases から full refresh する。
    ///
    /// 既存の `refresh_or_cache` と比較して PSQT を Finny Tables に載せる経路:
    /// 玉移動時の PSQT フル再計算（40 駒 × num_buckets bucket = 数百 i32 加算）を
    /// slot 差分（通常 0〜数 slot）に置き換える。
    ///
    /// # 引数
    ///
    /// 上記 `refresh_or_cache` に加えて:
    /// - `psqt_biases`: PSQT バイアス [MAX_LAYER_STACK_BUCKETS]（実体は対称差設計で常にゼロ、
    ///   ただし cache hit 時は参照せず `entry.psqt_accumulation` を直接ロードする）
    /// - `psqt_acc`: 更新先の PSQT アキュムレータ
    /// - `add_psqt_fn`: PSQT 加算関数（9-i32 SIMD: `add_psqt_weights`）
    /// - `sub_psqt_fn`: PSQT 減算関数（9-i32 SIMD: `sub_psqt_weights`）
    ///
    /// # 引数数について
    ///
    /// 引数 11 個（うちクロージャ 5 つ）+ `#[allow(clippy::too_many_arguments)]`
    /// 指定。構造体で束ねる選択肢もあるが、ホットパス（per-do_move 呼び出し）で
    /// クロージャを呼び出すため **モノモルフィズム + インライン展開が必須**。
    /// generic Fn パラメータの直接渡しが現状最も高速で、構造体経由（特に
    /// `Box<dyn Fn>` での動的 dispatch）は NPS 退行のリスクがある。
    /// 可読性は犠牲になるが性能優先の設計。
    #[cfg(feature = "nnue-psqt")]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn refresh_or_cache_with_psqt<FI, FA, FS, FAP, FSP>(
        &mut self,
        king_sq: Square,
        perspective: Color,
        piece_list: &[BonaPiece; PieceNumber::NB],
        biases: &[i16; L1],
        psqt_biases: &[i32; MAX_LAYER_STACK_BUCKETS],
        accumulation: &mut [i16; L1],
        psqt_acc: &mut [i32; MAX_LAYER_STACK_BUCKETS],
        idx_fn: FI,
        add_fn: FA,
        sub_fn: FS,
        add_psqt_fn: FAP,
        sub_psqt_fn: FSP,
    ) where
        FI: Fn(BonaPiece) -> usize,
        FA: Fn(&mut [i16; L1], usize),
        FS: Fn(&mut [i16; L1], usize),
        FAP: Fn(&mut [i32; MAX_LAYER_STACK_BUCKETS], usize),
        FSP: Fn(&mut [i32; MAX_LAYER_STACK_BUCKETS], usize),
    {
        let entry = &mut self.entries[king_sq.raw() as usize][perspective as usize];

        if entry.valid {
            crate::nnue::stats::count_cache_hit!();
            accumulation.copy_from_slice(&entry.accumulation);
            *psqt_acc = entry.psqt_accumulation;

            let mut diff_count = 0usize;
            for (cached_bp, &current_bp) in entry.piece_list.iter().copied().zip(piece_list.iter())
            {
                if cached_bp != current_bp {
                    if cached_bp != BonaPiece::ZERO {
                        let idx = idx_fn(cached_bp);
                        sub_fn(accumulation, idx);
                        sub_psqt_fn(psqt_acc, idx);
                        diff_count += 1;
                    }
                    if current_bp != BonaPiece::ZERO {
                        let idx = idx_fn(current_bp);
                        add_fn(accumulation, idx);
                        add_psqt_fn(psqt_acc, idx);
                        diff_count += 1;
                    }
                }
            }
            crate::nnue::stats::count_refresh_diff!(diff_count);
        } else {
            crate::nnue::stats::count_cache_miss!();
            accumulation.copy_from_slice(biases);
            *psqt_acc = *psqt_biases;
            for &bp in piece_list.iter() {
                if bp != BonaPiece::ZERO {
                    let idx = idx_fn(bp);
                    add_fn(accumulation, idx);
                    add_psqt_fn(psqt_acc, idx);
                }
            }
        }

        entry.accumulation.copy_from_slice(accumulation);
        entry.psqt_accumulation = *psqt_acc;
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
    /// progresskpabs の重み付き和（差分更新用）
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

    /// 現在のエントリを取得。
    #[inline]
    pub fn current(&self) -> &StackEntryLayerStacks<L1> {
        debug_assert!(self.current < self.entries.len());
        // SAFETY: entries と current は非公開で、entries の長さは構築後に変わらない。
        // new/reset は有効な 0 を設定し、push/pop は境界検査の成功後だけ current を更新する。
        unsafe { self.entries.get_unchecked(self.current) }
    }

    /// 現在のエントリを取得（可変）。
    #[inline]
    pub fn current_mut(&mut self) -> &mut StackEntryLayerStacks<L1> {
        debug_assert!(self.current < self.entries.len());
        // SAFETY: entries と current は非公開で、entries の長さは構築後に変わらない。
        // new/reset と検査済みの push/pop が current < entries.len() を維持する。
        // &mut self により、このエントリへの可変参照は排他的である。
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
        &self.entries[index]
    }

    /// スタックをプッシュ。
    ///
    /// # Panics
    /// 容量上限の場合。current と既存エントリは変更しない。
    #[inline]
    pub fn push(&mut self) {
        let prev = self.current;
        let next = prev + 1;
        let entry = self.entries.get_mut(next).expect("accumulator stack capacity exceeded");
        entry.previous = Some(prev);
        entry.accumulator.computed_accumulation = false;
        entry.accumulator.computed_score = false;
        entry.dirty_piece = DirtyPiece::default();
        #[cfg(feature = "nnue-progress-diff")]
        {
            entry.computed_progress = false;
        }
        self.current = next;
    }

    /// スタックをポップ。
    ///
    /// # Panics
    /// ルートの場合。current は変更しない。
    #[inline]
    pub fn pop(&mut self) {
        self.current = self.current.checked_sub(1).expect("cannot pop accumulator root");
    }

    /// 前回と現在のアキュムレータを同時に取得（clone不要）
    ///
    /// `split_at_mut`を使用して、prev_idx の accumulator への不変参照と
    /// 現在の accumulator への可変参照を同時に返す。
    ///
    /// # Panics
    /// `prev_idx` が現在位置より前でない場合（ルートを含む）。
    #[inline]
    pub fn get_prev_and_current_accumulators(
        &mut self,
        prev_idx: usize,
    ) -> (&AccumulatorLayerStacks<L1>, &mut AccumulatorLayerStacks<L1>) {
        let cur_idx = self.current;
        assert!(prev_idx < cur_idx, "previous index must precede current index");
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
    /// - **YaneuraOu方式**: 1手前のみをチェック（`max_depth=1`。シンプルだが差分更新機会を逃す）
    /// - **Stockfish方式**: スタックを遡って計算済み祖先を探し、各ステップで玉移動をチェック
    ///
    /// 本実装は Stockfish 方式。遡及上限 `max_depth` は呼び出し側が **runtime のモデル種別**
    /// (`has_threat`) で渡す（threat モデル=1 / それ以外=4。理由は呼び出し側コメント参照）。
    /// edition-universal など threat 非対応モデルも同一バイナリで動く構成があるため、
    /// compile-time feature でなく runtime で分ける。各ステップで玉移動があれば即座に打ち切る。
    ///
    /// ## 戻り値
    ///
    /// `Some((計算済みエントリのインデックス, 経由する局面数))` - 玉移動がない範囲で
    /// 計算済み祖先が見つかった場合。`None` - 使用可能な祖先が見つからない場合、
    /// または public previous が範囲外・自己参照・前方参照の場合。
    pub fn find_usable_accumulator(&self, max_depth: usize) -> Option<(usize, usize)> {
        let current = &self.entries[self.current];

        // 現局面で玉が動いていたら差分更新不可
        if current.dirty_piece.king_moved[0] || current.dirty_piece.king_moved[1] {
            return None;
        }

        // 直前局面をチェック（depth=1から開始）
        let mut prev_idx = current.previous?;
        let mut depth = 1;
        let mut child_idx = self.current;

        loop {
            // previous は公開 field。範囲外・自己参照・前方参照を辿らない。
            if prev_idx >= child_idx {
                return None;
            }
            let prev = self.entries.get(prev_idx)?;

            // 計算済みなら成功
            if prev.accumulator.computed_accumulation {
                return Some((prev_idx, depth));
            }

            // 探索上限に達した
            if depth >= max_depth {
                return None;
            }

            // さらに前の局面へ（ルートに達したらNone）
            let next_prev_idx = prev.previous?;

            // 玉が動いていたら打ち切り（早期終了による最適化）
            if prev.dirty_piece.king_moved[0] || prev.dirty_piece.king_moved[1] {
                return None;
            }

            child_idx = prev_idx;
            prev_idx = next_prev_idx;
            depth += 1;
        }
    }

    /// 指定インデックスから現在位置までのパスを収集
    ///
    /// 戻り値:
    /// - Some(path): source_idx に到達できた場合、source側から適用する順のインデックス列
    /// - None: パスが途切れた場合、MAX_PATH_LENGTH 超過、または previous が祖先を指さない場合
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
                Some(prev) if prev < idx => idx = prev,
                _ => return None,
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
/// Cargo feature `layerstacks-1536x16x32` / `layerstacks-1536x32x32`
/// / `layerstacks-768x16x32` / `layerstacks-768x8x32` / `layerstacks-512x16x32` / `layerstacks-1024x16x32`
/// / `layerstacks-3072x16x32` で
/// 有効なバリアントが制御される。
pub enum LayerStacksAccStack {
    #[cfg(feature = "layerstacks-1536x16x32")]
    L1536x16x32(AccumulatorStackLayerStacks<1536>),
    #[cfg(feature = "layerstacks-1536x32x32")]
    L1536x32x32(AccumulatorStackLayerStacks<1536>),
    #[cfg(feature = "layerstacks-1024x16x32")]
    L1024x16x32(AccumulatorStackLayerStacks<1024>),
    #[cfg(feature = "layerstacks-3072x16x32")]
    L3072x16x32(AccumulatorStackLayerStacks<3072>),
    #[cfg(feature = "layerstacks-768x16x32")]
    L768x16x32(AccumulatorStackLayerStacks<768>),
    #[cfg(feature = "layerstacks-768x8x32")]
    L768x8x32(AccumulatorStackLayerStacks<768>),
    #[cfg(feature = "layerstacks-512x16x32")]
    L512x16x32(AccumulatorStackLayerStacks<512>),
}

/// LayerStacks dispatch match の網羅性を確保するマクロ
///
/// 全 feature が無効の場合（WASM ビルド等）は空 enum のためインスタンスが存在しないが、
/// コンパイルを通すために wildcard arm を生成する。
macro_rules! ls_match {
    ($val:expr, $pat:ident => $body:expr) => {
        match $val {
            #[cfg(feature = "layerstacks-1536x16x32")]
            Self::L1536x16x32($pat) => $body,
            #[cfg(feature = "layerstacks-1536x32x32")]
            Self::L1536x32x32($pat) => $body,
            #[cfg(feature = "layerstacks-768x16x32")]
            Self::L768x16x32($pat) => $body,
            #[cfg(feature = "layerstacks-768x8x32")]
            Self::L768x8x32($pat) => $body,
            #[cfg(feature = "layerstacks-512x16x32")]
            Self::L512x16x32($pat) => $body,
            #[cfg(feature = "layerstacks-1024x16x32")]
            Self::L1024x16x32($pat) => $body,
            #[cfg(feature = "layerstacks-3072x16x32")]
            Self::L3072x16x32($pat) => $body,
            #[cfg(not(any(
                feature = "layerstacks-1536x16x32",
                feature = "layerstacks-1536x32x32",
                feature = "layerstacks-768x16x32",
                feature = "layerstacks-768x8x32",
                feature = "layerstacks-512x16x32",
                feature = "layerstacks-1024x16x32",
                feature = "layerstacks-3072x16x32"
            )))]
            _ => unreachable!("no LayerStacks variant enabled"),
        }
    };
}

impl LayerStacksAccStack {
    /// L1 サイズを取得
    pub fn l1_size(&self) -> usize {
        match self {
            #[cfg(feature = "layerstacks-1536x16x32")]
            Self::L1536x16x32(_) => 1536,
            #[cfg(feature = "layerstacks-1536x32x32")]
            Self::L1536x32x32(_) => 1536,
            #[cfg(feature = "layerstacks-768x16x32")]
            Self::L768x16x32(_) => 768,
            #[cfg(feature = "layerstacks-768x8x32")]
            Self::L768x8x32(_) => 768,
            #[cfg(feature = "layerstacks-512x16x32")]
            Self::L512x16x32(_) => 512,
            #[cfg(feature = "layerstacks-1024x16x32")]
            Self::L1024x16x32(_) => 1024,
            #[cfg(feature = "layerstacks-3072x16x32")]
            Self::L3072x16x32(_) => 3072,
            #[cfg(not(any(
                feature = "layerstacks-1536x16x32",
                feature = "layerstacks-1536x32x32",
                feature = "layerstacks-768x16x32",
                feature = "layerstacks-768x8x32",
                feature = "layerstacks-512x16x32",
                feature = "layerstacks-1024x16x32",
                feature = "layerstacks-3072x16x32"
            )))]
            _ => unreachable!("no LayerStacks variant enabled"),
        }
    }

    /// アーキテクチャ寸法 (L1, L2, L3) を返す
    pub fn architecture_dims(&self) -> (usize, usize, usize) {
        match self {
            #[cfg(feature = "layerstacks-1536x16x32")]
            Self::L1536x16x32(_) => (1536, 16, 32),
            #[cfg(feature = "layerstacks-1536x32x32")]
            Self::L1536x32x32(_) => (1536, 32, 32),
            #[cfg(feature = "layerstacks-768x16x32")]
            Self::L768x16x32(_) => (768, 16, 32),
            #[cfg(feature = "layerstacks-768x8x32")]
            Self::L768x8x32(_) => (768, 8, 32),
            #[cfg(feature = "layerstacks-512x16x32")]
            Self::L512x16x32(_) => (512, 16, 32),
            #[cfg(feature = "layerstacks-1024x16x32")]
            Self::L1024x16x32(_) => (1024, 16, 32),
            #[cfg(feature = "layerstacks-3072x16x32")]
            Self::L3072x16x32(_) => (3072, 16, 32),
            #[cfg(not(any(
                feature = "layerstacks-1536x16x32",
                feature = "layerstacks-1536x32x32",
                feature = "layerstacks-768x16x32",
                feature = "layerstacks-768x8x32",
                feature = "layerstacks-512x16x32",
                feature = "layerstacks-1024x16x32",
                feature = "layerstacks-3072x16x32"
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
    #[cfg(feature = "layerstack-arch")]
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
    #[cfg(feature = "layerstacks-1536x16x32")]
    L1536x16x32(AccumulatorCacheLayerStacks<1536>),
    #[cfg(feature = "layerstacks-1536x32x32")]
    L1536x32x32(AccumulatorCacheLayerStacks<1536>),
    #[cfg(feature = "layerstacks-1024x16x32")]
    L1024x16x32(AccumulatorCacheLayerStacks<1024>),
    #[cfg(feature = "layerstacks-3072x16x32")]
    L3072x16x32(AccumulatorCacheLayerStacks<3072>),
    #[cfg(feature = "layerstacks-768x16x32")]
    L768x16x32(AccumulatorCacheLayerStacks<768>),
    #[cfg(feature = "layerstacks-768x8x32")]
    L768x8x32(AccumulatorCacheLayerStacks<768>),
    #[cfg(feature = "layerstacks-512x16x32")]
    L512x16x32(AccumulatorCacheLayerStacks<512>),
}

impl LayerStacksAccCache {
    /// この cache の L1 サイズを返す
    ///
    /// ネットワーク reload で L1 バリアントが変わったときに、旧 cache を
    /// 使い回してしまう事故を防ぐため、`prepare_search` 側で
    /// `network.l1_size()` と比較するために使う。
    pub fn l1_size(&self) -> usize {
        match self {
            #[cfg(feature = "layerstacks-1536x16x32")]
            Self::L1536x16x32(_) => 1536,
            #[cfg(feature = "layerstacks-1536x32x32")]
            Self::L1536x32x32(_) => 1536,
            #[cfg(feature = "layerstacks-768x16x32")]
            Self::L768x16x32(_) => 768,
            #[cfg(feature = "layerstacks-768x8x32")]
            Self::L768x8x32(_) => 768,
            #[cfg(feature = "layerstacks-512x16x32")]
            Self::L512x16x32(_) => 512,
            #[cfg(feature = "layerstacks-1024x16x32")]
            Self::L1024x16x32(_) => 1024,
            #[cfg(feature = "layerstacks-3072x16x32")]
            Self::L3072x16x32(_) => 3072,
            #[cfg(not(any(
                feature = "layerstacks-1536x16x32",
                feature = "layerstacks-1536x32x32",
                feature = "layerstacks-768x16x32",
                feature = "layerstacks-768x8x32",
                feature = "layerstacks-512x16x32",
                feature = "layerstacks-1024x16x32",
                feature = "layerstacks-3072x16x32"
            )))]
            _ => unreachable!("no LayerStacks variant enabled"),
        }
    }

    /// アーキテクチャ寸法 (L1, L2, L3) を返す
    pub fn architecture_dims(&self) -> (usize, usize, usize) {
        match self {
            #[cfg(feature = "layerstacks-1536x16x32")]
            Self::L1536x16x32(_) => (1536, 16, 32),
            #[cfg(feature = "layerstacks-1536x32x32")]
            Self::L1536x32x32(_) => (1536, 32, 32),
            #[cfg(feature = "layerstacks-768x16x32")]
            Self::L768x16x32(_) => (768, 16, 32),
            #[cfg(feature = "layerstacks-768x8x32")]
            Self::L768x8x32(_) => (768, 8, 32),
            #[cfg(feature = "layerstacks-512x16x32")]
            Self::L512x16x32(_) => (512, 16, 32),
            #[cfg(feature = "layerstacks-1024x16x32")]
            Self::L1024x16x32(_) => (1024, 16, 32),
            #[cfg(feature = "layerstacks-3072x16x32")]
            Self::L3072x16x32(_) => (3072, 16, 32),
            #[cfg(not(any(
                feature = "layerstacks-1536x16x32",
                feature = "layerstacks-1536x32x32",
                feature = "layerstacks-768x16x32",
                feature = "layerstacks-768x8x32",
                feature = "layerstacks-512x16x32",
                feature = "layerstacks-1024x16x32",
                feature = "layerstacks-3072x16x32"
            )))]
            _ => unreachable!("no LayerStacks variant enabled"),
        }
    }

    /// 全エントリを無効化
    pub fn invalidate(&mut self) {
        match self {
            #[cfg(feature = "layerstacks-1536x16x32")]
            Self::L1536x16x32(c) => c.invalidate(),
            #[cfg(feature = "layerstacks-1536x32x32")]
            Self::L1536x32x32(c) => c.invalidate(),
            #[cfg(feature = "layerstacks-768x16x32")]
            Self::L768x16x32(c) => c.invalidate(),
            #[cfg(feature = "layerstacks-768x8x32")]
            Self::L768x8x32(c) => c.invalidate(),
            #[cfg(feature = "layerstacks-512x16x32")]
            Self::L512x16x32(c) => c.invalidate(),
            #[cfg(feature = "layerstacks-1024x16x32")]
            Self::L1024x16x32(c) => c.invalidate(),
            #[cfg(feature = "layerstacks-3072x16x32")]
            Self::L3072x16x32(c) => c.invalidate(),
            #[cfg(not(any(
                feature = "layerstacks-1536x16x32",
                feature = "layerstacks-1536x32x32",
                feature = "layerstacks-768x16x32",
                feature = "layerstacks-768x8x32",
                feature = "layerstacks-512x16x32",
                feature = "layerstacks-1024x16x32",
                feature = "layerstacks-3072x16x32"
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

    fn apply_test_changes(
        acc: &mut [i16; TEST_L1],
        removed: &IndexList<{ PieceNumber::NB }>,
        added: &IndexList<{ PieceNumber::NB }>,
    ) {
        for idx in removed.iter() {
            acc[0] = acc[0].wrapping_sub(idx as i16);
        }
        for idx in added.iter() {
            acc[0] = acc[0].wrapping_add(idx as i16);
        }
    }

    #[test]
    fn safe_boundary_perspectives() {
        use std::panic::{AssertUnwindSafe, catch_unwind};
        let mut acc = AccumulatorLayerStacks::<4>::new();
        acc.get_mut(0)[0] = 12;
        acc.get_mut(1)[0] = 34;
        for invalid in [2, usize::MAX] {
            assert!(catch_unwind(|| acc.get(invalid)).is_err());
            assert!(
                catch_unwind(AssertUnwindSafe(|| {
                    acc.get_mut(invalid);
                }))
                .is_err()
            );
            #[cfg(feature = "nnue-threat")]
            {
                assert!(catch_unwind(|| acc.get_threat(invalid)).is_err());
                assert!(
                    catch_unwind(AssertUnwindSafe(|| {
                        acc.get_threat_mut(invalid);
                    }))
                    .is_err()
                );
            }
        }
        assert_eq!(acc.get(0)[0], 12);
        assert_eq!(acc.get(1)[0], 34);
        #[cfg(feature = "nnue-threat")]
        {
            acc.get_threat_mut(1)[0] = 56;
            assert_eq!(acc.get_threat(1)[0], 56);
        }
    }

    #[test]
    fn safe_boundary_stack_preserves_state_after_failure() {
        use std::panic::{AssertUnwindSafe, catch_unwind};
        type Stack = AccumulatorStackLayerStacks<4>;
        let mut stack = Stack::new();
        stack.current_mut().accumulator.get_mut(0)[0] = 17;
        assert!(catch_unwind(AssertUnwindSafe(|| stack.pop())).is_err());
        assert_eq!(stack.current_index(), 0);
        assert_eq!(stack.current().accumulator.get(0)[0], 17);
        for _ in 1..Stack::STACK_SIZE {
            stack.push();
        }
        let last = stack.current_index();
        stack.current_mut().accumulator.get_mut(0)[0] = 29;
        assert!(catch_unwind(AssertUnwindSafe(|| stack.push())).is_err());
        assert_eq!(stack.current_index(), last);
        assert_eq!(stack.current().accumulator.get(0)[0], 29);
        for invalid in [last, usize::MAX] {
            assert!(
                catch_unwind(AssertUnwindSafe(|| {
                    stack.get_prev_and_current_accumulators(invalid);
                }))
                .is_err()
            );
            assert_eq!(stack.current_index(), last);
        }
        for _ in 0..last {
            stack.pop();
        }
        assert_eq!(stack.current().accumulator.get(0)[0], 17);
        assert!(
            catch_unwind(AssertUnwindSafe(|| {
                stack.get_prev_and_current_accumulators(0);
            }))
            .is_err()
        );
    }

    #[test]
    fn safe_boundary_previous_links_and_valid_path() {
        let mut stack = AccumulatorStackLayerStacks::<4>::new();
        stack.current_mut().accumulator.computed_accumulation = true;
        stack.push();
        stack.push();
        assert_eq!(stack.find_usable_accumulator(4), Some((0, 2)));
        assert_eq!(stack.collect_path(0).unwrap().iter().collect::<Vec<_>>(), vec![1, 2]);
        let (previous, current) = stack.get_prev_and_current_accumulators(0);
        assert!(previous.computed_accumulation);
        current.get_mut(1)[0] = 71;
        assert_eq!(stack.current().accumulator.get(1)[0], 71);
        for invalid in [2, 3, usize::MAX] {
            stack.current_mut().previous = Some(invalid);
            assert_eq!(stack.find_usable_accumulator(usize::MAX), None);
            assert!(stack.collect_path(0).is_none());
        }
        stack.current_mut().previous = Some(1);
        stack.pop();
        stack.current_mut().previous = Some(1);
        stack.push();
        assert_eq!(stack.find_usable_accumulator(usize::MAX), None);
        assert!(stack.collect_path(0).is_none());
        stack.reset();
        assert_eq!(stack.current_index(), 0);
        assert!(stack.current().previous.is_none());
    }

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
            apply_test_changes,
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
            apply_test_changes,
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
            apply_test_changes,
        );
        // hit: 30 - 15 + 20 = 35
        assert_eq!(acc2[0], 35);

        // 3回目: 同じpiece listなら、2回目に更新したcache entryをそのまま返す。
        let mut acc3 = [0i16; TEST_L1];
        cache.refresh_or_cache(
            king_sq,
            perspective,
            &pl2,
            &biases,
            &mut acc3,
            |bp| bp.0 as usize,
            apply_test_changes,
        );
        assert_eq!(acc3[0], 35);

        // 4回目: in-place 更新済み entry へさらに非ゼロ差分を適用 (hit→mutate→hit chain)
        let mut pl3 = pl2;
        pl3[0] = BonaPiece(7);
        let mut acc4 = [0i16; TEST_L1];
        cache.refresh_or_cache(
            king_sq,
            perspective,
            &pl3,
            &biases,
            &mut acc4,
            |bp| bp.0 as usize,
            apply_test_changes,
        );
        // hit: 35 - 5 + 7 = 37
        assert_eq!(acc4[0], 37);

        // 5回目: pl3 のまま再読。piece_list の書き戻しが stale なら差分が重複適用される。
        let mut acc5 = [0i16; TEST_L1];
        cache.refresh_or_cache(
            king_sq,
            perspective,
            &pl3,
            &biases,
            &mut acc5,
            |bp| bp.0 as usize,
            apply_test_changes,
        );
        assert_eq!(acc5[0], 37);
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
            apply_test_changes,
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
            apply_test_changes,
        );
        // hit: 15 - 10 = 5
        assert_eq!(acc2[0], 5);
    }

    /// PSQT 拡張: cold start → main acc / PSQT acc 双方を full refresh で計算
    #[cfg(feature = "nnue-psqt")]
    #[test]
    fn test_psqt_refresh_or_cache_cold_start() {
        let mut cache = AccumulatorCacheLayerStacks::<TEST_L1>::new();
        let king_sq = Square::SQ_55;
        let perspective = Color::Black;

        let biases = [0i16; TEST_L1];
        let psqt_biases = [0i32; MAX_LAYER_STACK_BUCKETS];

        let mut piece_list = [BonaPiece::ZERO; PieceNumber::NB];
        piece_list[0] = BonaPiece(5);
        piece_list[1] = BonaPiece(10);

        let mut acc = [0i16; TEST_L1];
        let mut psqt_acc = [0i32; MAX_LAYER_STACK_BUCKETS];
        cache.refresh_or_cache_with_psqt(
            king_sq,
            perspective,
            &piece_list,
            &biases,
            &psqt_biases,
            &mut acc,
            &mut psqt_acc,
            |bp| bp.0 as usize,
            |a, idx| a[0] = a[0].wrapping_add(idx as i16),
            |a, idx| a[0] = a[0].wrapping_sub(idx as i16),
            |p, idx| {
                // bucket b に対し idx + b を加える（bucket ごとに区別できる値）
                for (b, v) in p.iter_mut().enumerate() {
                    *v = v.wrapping_add(idx as i32 + b as i32);
                }
            },
            |p, idx| {
                for (b, v) in p.iter_mut().enumerate() {
                    *v = v.wrapping_sub(idx as i32 + b as i32);
                }
            },
        );

        assert_eq!(acc[0], 15);
        // bucket b: (5+b) + (10+b) = 15 + 2*b
        for (b, v) in psqt_acc.iter().enumerate() {
            assert_eq!(*v, 15 + 2 * b as i32, "bucket {b}");
        }

        let entry = &cache.entries[king_sq.raw() as usize][perspective as usize];
        assert!(entry.valid);
        assert_eq!(entry.psqt_accumulation, psqt_acc);
    }

    /// PSQT 拡張: 2 回目のキャッシュヒット時に PSQT も差分で更新される
    #[cfg(feature = "nnue-psqt")]
    #[test]
    fn test_psqt_refresh_or_cache_hit_updates_psqt() {
        let mut cache = AccumulatorCacheLayerStacks::<TEST_L1>::new();
        let king_sq = Square::SQ_55;
        let perspective = Color::Black;
        let biases = [0i16; TEST_L1];
        let psqt_biases = [0i32; MAX_LAYER_STACK_BUCKETS];

        let add_main = |a: &mut [i16; TEST_L1], idx: usize| {
            a[0] = a[0].wrapping_add(idx as i16);
        };
        let sub_main = |a: &mut [i16; TEST_L1], idx: usize| {
            a[0] = a[0].wrapping_sub(idx as i16);
        };
        let add_psqt = |p: &mut [i32; MAX_LAYER_STACK_BUCKETS], idx: usize| {
            for (b, v) in p.iter_mut().enumerate() {
                *v = v.wrapping_add(idx as i32 + b as i32);
            }
        };
        let sub_psqt = |p: &mut [i32; MAX_LAYER_STACK_BUCKETS], idx: usize| {
            for (b, v) in p.iter_mut().enumerate() {
                *v = v.wrapping_sub(idx as i32 + b as i32);
            }
        };

        // 1 回目: slot 0-2 = [5, 10, 15]
        let mut pl1 = [BonaPiece::ZERO; PieceNumber::NB];
        pl1[0] = BonaPiece(5);
        pl1[1] = BonaPiece(10);
        pl1[2] = BonaPiece(15);
        let mut acc1 = [0i16; TEST_L1];
        let mut psqt1 = [0i32; MAX_LAYER_STACK_BUCKETS];
        cache.refresh_or_cache_with_psqt(
            king_sq,
            perspective,
            &pl1,
            &biases,
            &psqt_biases,
            &mut acc1,
            &mut psqt1,
            |bp| bp.0 as usize,
            add_main,
            sub_main,
            add_psqt,
            sub_psqt,
        );

        // 2 回目: slot 2 の 15 → 20 に変化
        let mut pl2 = pl1;
        pl2[2] = BonaPiece(20);
        let mut acc2 = [0i16; TEST_L1];
        let mut psqt2 = [0i32; MAX_LAYER_STACK_BUCKETS];
        cache.refresh_or_cache_with_psqt(
            king_sq,
            perspective,
            &pl2,
            &biases,
            &psqt_biases,
            &mut acc2,
            &mut psqt2,
            |bp| bp.0 as usize,
            add_main,
            sub_main,
            add_psqt,
            sub_psqt,
        );

        // cache hit 経由の結果が full refresh と完全一致することを確認
        let mut cache2 = AccumulatorCacheLayerStacks::<TEST_L1>::new();
        let mut acc_full = [0i16; TEST_L1];
        let mut psqt_full = [0i32; MAX_LAYER_STACK_BUCKETS];
        cache2.refresh_or_cache_with_psqt(
            king_sq,
            perspective,
            &pl2,
            &biases,
            &psqt_biases,
            &mut acc_full,
            &mut psqt_full,
            |bp| bp.0 as usize,
            add_main,
            sub_main,
            add_psqt,
            sub_psqt,
        );
        assert_eq!(acc2, acc_full, "main acc: cache hit vs full refresh");
        assert_eq!(psqt2, psqt_full, "psqt acc: cache hit vs full refresh");
    }
}
