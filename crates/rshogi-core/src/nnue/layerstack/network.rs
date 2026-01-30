//! LayerStack ネットワーク実装
//!
//! LSNN ファイルから読み込んだ重みを使用して評価を行う。
//!
//! # 注意
//!
//! 現在の実装では差分更新を行わず、常にフル再計算を行う。
//! これは正確性を優先した設計であり、最適化は後の課題とする。

use super::bucket::{bucket_index, BucketDivision};
use super::constants::*;
use super::forward::{internal_to_cp, layer_stack_forward, product_pooling};
use super::io::read_lsnn;
use super::weights::LayerStackWeights;
use crate::nnue::accumulator::{AlignedBox, DirtyPiece, MAX_PATH_LENGTH};
use crate::position::Position;
use crate::types::{Color, PieceType, Value};
use std::io::{Read, Seek};

// =============================================================================
// AccumulatorLayerStack
// =============================================================================

/// LayerStack 用アキュムレータ
///
/// Feature Transformer の出力 [i16; 1536] を視点ごとに保持。
pub struct AccumulatorLayerStack {
    /// アキュムレータバッファ [perspective][1536]
    pub accumulation: [AlignedBox<i16>; 2],
    /// 計算済みフラグ
    pub computed_accumulation: bool,
}

impl AccumulatorLayerStack {
    /// 新規作成
    pub fn new() -> Self {
        Self {
            accumulation: [
                AlignedBox::new_zeroed(FT_PER_PERSPECTIVE),
                AlignedBox::new_zeroed(FT_PER_PERSPECTIVE),
            ],
            computed_accumulation: false,
        }
    }

    /// クリア
    pub fn clear(&mut self) {
        self.accumulation[0].fill(0);
        self.accumulation[1].fill(0);
        self.computed_accumulation = false;
    }
}

impl Default for AccumulatorLayerStack {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for AccumulatorLayerStack {
    fn clone(&self) -> Self {
        Self {
            accumulation: [self.accumulation[0].clone(), self.accumulation[1].clone()],
            computed_accumulation: self.computed_accumulation,
        }
    }
}

// =============================================================================
// AccumulatorStackLayerStack
// =============================================================================

/// LayerStack 用スタックエントリ
pub struct AccumulatorEntryLayerStack {
    pub accumulator: AccumulatorLayerStack,
    pub dirty_piece: DirtyPiece,
    pub previous: Option<usize>,
}

impl AccumulatorEntryLayerStack {
    fn new() -> Self {
        Self {
            accumulator: AccumulatorLayerStack::new(),
            dirty_piece: DirtyPiece::default(),
            previous: None,
        }
    }
}

impl Clone for AccumulatorEntryLayerStack {
    fn clone(&self) -> Self {
        Self {
            accumulator: self.accumulator.clone(),
            dirty_piece: self.dirty_piece,
            previous: self.previous,
        }
    }
}

/// LayerStack 用アキュムレータスタック
pub struct AccumulatorStackLayerStack {
    stack: Vec<AccumulatorEntryLayerStack>,
    current_idx: usize,
}

impl AccumulatorStackLayerStack {
    /// 新規作成
    pub fn new() -> Self {
        let stack = (0..MAX_PATH_LENGTH).map(|_| AccumulatorEntryLayerStack::new()).collect();
        Self {
            stack,
            current_idx: 0,
        }
    }

    /// リセット
    pub fn reset(&mut self) {
        for entry in &mut self.stack {
            entry.accumulator.computed_accumulation = false;
            entry.previous = None;
        }
        self.current_idx = 0;
    }

    /// プッシュ
    pub fn push(&mut self, dirty_piece: DirtyPiece) {
        let prev = self.current_idx;
        self.current_idx += 1;
        if self.current_idx >= self.stack.len() {
            self.stack.push(AccumulatorEntryLayerStack::new());
        }
        self.stack[self.current_idx].dirty_piece = dirty_piece;
        self.stack[self.current_idx].previous = Some(prev);
        self.stack[self.current_idx].accumulator.computed_accumulation = false;
    }

    /// ポップ
    pub fn pop(&mut self) {
        if self.current_idx > 0 {
            self.current_idx -= 1;
        }
    }

    /// 現在のインデックス
    #[inline]
    pub fn current_index(&self) -> usize {
        self.current_idx
    }

    /// 現在のエントリ
    #[inline]
    pub fn current(&self) -> &AccumulatorEntryLayerStack {
        &self.stack[self.current_idx]
    }

    /// 現在のエントリ（可変）
    #[inline]
    pub fn current_mut(&mut self) -> &mut AccumulatorEntryLayerStack {
        &mut self.stack[self.current_idx]
    }

    /// 指定インデックスのエントリ
    #[inline]
    pub fn entry_at(&self, idx: usize) -> &AccumulatorEntryLayerStack {
        &self.stack[idx]
    }

    /// 指定インデックスのエントリ（可変）
    #[inline]
    pub fn entry_at_mut(&mut self, idx: usize) -> &mut AccumulatorEntryLayerStack {
        &mut self.stack[idx]
    }

    /// 使用可能なアキュムレータを探す
    pub fn find_usable_accumulator(&self) -> Option<(usize, usize)> {
        let mut idx = self.current_idx;
        let mut depth = 0;

        while let Some(prev) = self.stack[idx].previous {
            idx = prev;
            depth += 1;
            if self.stack[idx].accumulator.computed_accumulation {
                return Some((idx, depth));
            }
        }

        // ルートが計算済みかチェック
        if self.stack[0].accumulator.computed_accumulation {
            return Some((0, depth));
        }

        None
    }
}

impl Default for AccumulatorStackLayerStack {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// LayerStackStack
// =============================================================================

/// LayerStack Accumulator スタック
///
/// 他のアーキテクチャとの一貫性のために enum でラップ。
/// LayerStack は単一構成（L1=1536 固定）のためバリアントは1つ。
pub enum LayerStackStack {
    L1536(AccumulatorStackLayerStack),
}

impl LayerStackStack {
    /// ネットワークに対応するスタックを生成
    pub fn from_network(_net: &LayerStackNetwork) -> Self {
        Self::L1536(AccumulatorStackLayerStack::new())
    }

    /// L1 サイズを取得
    pub fn l1_size(&self) -> usize {
        FT_PER_PERSPECTIVE
    }

    /// リセット
    pub fn reset(&mut self) {
        match self {
            Self::L1536(s) => s.reset(),
        }
    }

    /// プッシュ
    pub fn push(&mut self, dirty: DirtyPiece) {
        match self {
            Self::L1536(s) => s.push(dirty),
        }
    }

    /// ポップ
    pub fn pop(&mut self) {
        match self {
            Self::L1536(s) => s.pop(),
        }
    }

    /// 現在のインデックス
    pub fn current_index(&self) -> usize {
        match self {
            Self::L1536(s) => s.current_index(),
        }
    }

    /// 祖先を辿って使用可能なアキュムレータを探す
    pub fn find_usable_accumulator(&self) -> Option<(usize, usize)> {
        match self {
            Self::L1536(s) => s.find_usable_accumulator(),
        }
    }

    /// 現在のアキュムレータが計算済みかどうか
    #[inline]
    pub fn is_current_computed(&self) -> bool {
        match self {
            Self::L1536(s) => s.current().accumulator.computed_accumulation,
        }
    }

    /// 現在のエントリの previous インデックス
    #[inline]
    pub fn current_previous(&self) -> Option<usize> {
        match self {
            Self::L1536(s) => s.current().previous,
        }
    }

    /// 指定インデックスのエントリが計算済みかどうか
    #[inline]
    pub fn is_entry_computed(&self, idx: usize) -> bool {
        match self {
            Self::L1536(s) => s.entry_at(idx).accumulator.computed_accumulation,
        }
    }

    /// 現在のエントリの dirty piece を取得
    #[inline]
    pub fn current_dirty_piece(&self) -> DirtyPiece {
        match self {
            Self::L1536(s) => s.current().dirty_piece,
        }
    }
}

impl Default for LayerStackStack {
    fn default() -> Self {
        Self::L1536(AccumulatorStackLayerStack::new())
    }
}

// =============================================================================
// LayerStackNetwork
// =============================================================================

/// LayerStack ネットワーク
pub struct LayerStackNetwork {
    /// 重み
    weights: LayerStackWeights,
}

impl LayerStackNetwork {
    /// リーダーから読み込み
    pub fn read<R: Read + Seek>(reader: &mut R) -> std::io::Result<Self> {
        let weights = read_lsnn(reader)?;
        Ok(Self { weights })
    }

    /// L1 サイズを取得（常に 1536）
    pub fn l1_size(&self) -> usize {
        FT_PER_PERSPECTIVE
    }

    /// アーキテクチャ名を取得
    pub fn architecture_name(&self) -> &'static str {
        match self.weights.bucket_division {
            BucketDivision::TwoByTwo => "LayerStack-1536-15-64-2x2",
            BucketDivision::ThreeByThree => "LayerStack-1536-15-64-3x3",
        }
    }

    /// バケット数を取得
    pub fn num_buckets(&self) -> usize {
        self.weights.num_buckets()
    }

    /// バケット分割方式を取得
    pub fn bucket_division(&self) -> BucketDivision {
        self.weights.bucket_division
    }

    /// bypass 使用フラグを取得
    pub fn use_bypass(&self) -> bool {
        self.weights.use_bypass
    }

    /// アキュムレータをフル再計算
    pub fn refresh_accumulator(&self, pos: &Position, stack: &mut LayerStackStack) {
        let LayerStackStack::L1536(s) = stack;
        self.refresh_accumulator_impl(pos, s);
    }

    /// アキュムレータをフル再計算（内部実装）
    fn refresh_accumulator_impl(&self, pos: &Position, stack: &mut AccumulatorStackLayerStack) {
        let entry = stack.current_mut();

        // 各視点のアキュムレータを計算
        for perspective in [Color::Black, Color::White] {
            let p_idx = perspective.index();

            // バイアスで初期化
            for (i, &bias) in self.weights.ft.bias.iter().enumerate() {
                entry.accumulator.accumulation[p_idx][i] = bias;
            }

            // アクティブな特徴量を追加
            self.add_active_features(pos, perspective, &mut entry.accumulator.accumulation[p_idx]);
        }

        entry.accumulator.computed_accumulation = true;
    }

    /// 差分更新
    ///
    /// 現在の実装ではフル再計算を行う（最適化は後の課題）
    pub fn update_accumulator(
        &self,
        pos: &Position,
        _dirty: &DirtyPiece,
        stack: &mut LayerStackStack,
        _source_idx: usize,
    ) {
        // 差分更新は未実装。フル再計算にフォールバック
        self.refresh_accumulator(pos, stack);
    }

    /// 前方差分更新
    ///
    /// 現在の実装では常に false を返し、フル再計算にフォールバック
    pub fn forward_update_incremental(
        &self,
        _pos: &Position,
        _stack: &mut LayerStackStack,
        _source_idx: usize,
    ) -> bool {
        // 差分更新は未実装
        false
    }

    /// 評価値を計算
    pub fn evaluate(&self, pos: &Position, stack: &LayerStackStack) -> Value {
        let LayerStackStack::L1536(s) = stack;
        self.evaluate_impl(pos, s)
    }

    /// 評価値を計算（内部実装）
    fn evaluate_impl(&self, pos: &Position, stack: &AccumulatorStackLayerStack) -> Value {
        let entry = stack.current();
        let stm = pos.side_to_move();

        // アキュムレータを取得
        let stm_acc = &entry.accumulator.accumulation[stm.index()];
        let nstm_acc = &entry.accumulator.accumulation[(!stm).index()];

        // Perspective 結合 + ClippedReLU
        let mut l0 = [0u8; PERSPECTIVE_CAT];
        for (i, &val) in stm_acc.iter().enumerate() {
            l0[i] = val.clamp(0, QUANTIZED_ONE) as u8;
        }
        for (i, &val) in nstm_acc.iter().enumerate() {
            l0[i + FT_PER_PERSPECTIVE] = val.clamp(0, QUANTIZED_ONE) as u8;
        }

        // Product Pooling
        let mut x = [0u8; PP_OUT];
        product_pooling(&l0, &mut x);

        // バケット計算
        let bucket = bucket_index(pos, self.weights.bucket_division);

        // LayerStack forward
        let internal = layer_stack_forward(&x, bucket, &self.weights);

        // cp に変換
        let cp = internal_to_cp(internal);

        Value::from(cp)
    }

    /// アクティブな特徴量を収集し、アキュムレータに重みを加算
    fn add_active_features(&self, pos: &Position, perspective: Color, acc: &mut [i16]) {
        use crate::nnue::bona_piece::{BonaPiece, PIECE_BASE};
        use crate::nnue::bona_piece_halfka_hm::{
            halfka_index, is_hm_mirror, king_bonapiece, king_bucket, pack_bonapiece,
        };

        let king_sq = pos.king_square(perspective);
        let kb = king_bucket(king_sq, perspective);
        let mirror = is_hm_mirror(king_sq, perspective);

        // 自玉と敵玉を特徴量に追加（HalfKA_hm の特性）
        let own_king_sq = pos.king_square(perspective);
        let opp_king_sq = pos.king_square(!perspective);

        // 自玉
        let own_king_bp = king_bonapiece(own_king_sq.index(), true);
        let own_king_packed = pack_bonapiece(own_king_bp, mirror);
        let own_king_idx = halfka_index(kb, own_king_packed);
        self.add_weight(acc, own_king_idx);

        // 敵玉
        let opp_king_bp = king_bonapiece(opp_king_sq.index(), false);
        let opp_king_packed = pack_bonapiece(opp_king_bp, mirror);
        let opp_king_idx = halfka_index(kb, opp_king_packed);
        self.add_weight(acc, opp_king_idx);

        // 盤上の駒（玉を除く）
        for color in [Color::Black, Color::White] {
            let bb = pos.pieces_c(color);
            for sq in bb.iter() {
                let pc = pos.piece_on(sq);
                let pt = pc.piece_type();
                if pt == PieceType::King {
                    continue;
                }

                // BonaPiece を計算
                let is_friend = pc.color() == perspective;
                let base = PIECE_BASE[pt as usize][is_friend as usize];
                let sq_idx = sq.index();
                let bp = BonaPiece::new(base + sq_idx as u16);

                let packed = pack_bonapiece(bp, mirror);
                let idx = halfka_index(kb, packed);
                self.add_weight(acc, idx);
            }
        }

        // 持ち駒
        let hand_piece_types = [
            PieceType::Pawn,
            PieceType::Lance,
            PieceType::Knight,
            PieceType::Silver,
            PieceType::Gold,
            PieceType::Bishop,
            PieceType::Rook,
        ];

        for color in [Color::Black, Color::White] {
            for pt in hand_piece_types {
                let count = pos.hand(color).count(pt);
                for i in 0..count {
                    let bp = BonaPiece::from_hand_piece(perspective, color, pt, (i + 1) as u8);
                    if bp != BonaPiece::ZERO {
                        let packed = pack_bonapiece(bp, mirror);
                        let idx = halfka_index(kb, packed);
                        self.add_weight(acc, idx);
                    }
                }
            }
        }
    }

    /// 単一の特徴量の重みを加算
    #[inline]
    fn add_weight(&self, acc: &mut [i16], idx: usize) {
        let weight_offset = idx * FT_PER_PERSPECTIVE;
        for (j, acc_val) in acc.iter_mut().enumerate() {
            *acc_val += self.weights.ft.weight[weight_offset + j];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_accumulator_stack_push_pop() {
        let mut stack = AccumulatorStackLayerStack::new();
        let dirty = DirtyPiece::default();

        stack.reset();
        assert_eq!(stack.current_index(), 0);

        stack.push(dirty);
        assert_eq!(stack.current_index(), 1);

        stack.push(dirty);
        assert_eq!(stack.current_index(), 2);

        stack.pop();
        assert_eq!(stack.current_index(), 1);

        stack.pop();
        assert_eq!(stack.current_index(), 0);
    }

    #[test]
    fn test_layer_stack_stack_default() {
        let stack = LayerStackStack::default();
        assert_eq!(stack.l1_size(), FT_PER_PERSPECTIVE);
    }

    #[test]
    fn test_layer_stack_stack_reset() {
        let mut stack = LayerStackStack::default();
        let dirty = DirtyPiece::default();

        stack.push(dirty);
        stack.push(dirty);
        assert_eq!(stack.current_index(), 2);

        stack.reset();
        assert_eq!(stack.current_index(), 0);
    }
}
