//! FeatureTransformerNnuePytorch - nnue-pytorch用の1536次元Feature Transformer
//!
//! HalfKA_hm^ 特徴量（キングバケット×BonaPiece）から、
//! 片側 1536 次元×両視点の中間表現を生成する。

use super::accumulator::{Aligned, AlignedBox};
use super::accumulator::{DirtyPiece, IndexList, MAX_ACTIVE_FEATURES};
use super::accumulator_nnue_pytorch::{AccumulatorNnuePytorch, AccumulatorStackNnuePytorch};
use super::constants::{HALFKA_HM_DIMENSIONS, NNUE_PYTORCH_L1};
use super::features::{FeatureSet, HalfKA_hmFeatureSet};
use super::leb128::read_compressed_tensor_i16;
use crate::position::Position;
use crate::types::Color;
use std::io::{self, Read};

/// 特徴インデックスの範囲外アクセス時のパニック
#[cold]
#[inline(never)]
fn feature_index_oob(index: usize, max: usize) -> ! {
    panic!("Feature index out of range: {index} (max: {max})")
}

/// nnue-pytorch用のFeatureTransformer（1536次元出力）
#[repr(C, align(64))]
pub struct FeatureTransformerNnuePytorch {
    /// バイアス [L1]
    pub biases: Aligned<[i16; NNUE_PYTORCH_L1]>,

    /// 重み [input_dimensions][L1]
    /// 64バイトアラインメントで確保
    pub weights: AlignedBox<i16>,
}

impl FeatureTransformerNnuePytorch {
    /// ファイルから読み込み（非圧縮形式）
    pub fn read<R: Read>(reader: &mut R) -> io::Result<Self> {
        // バイアスを読み込み
        let mut biases = [0i16; NNUE_PYTORCH_L1];
        let mut buf = [0u8; 2];
        for bias in biases.iter_mut() {
            reader.read_exact(&mut buf)?;
            *bias = i16::from_le_bytes(buf);
        }

        // 重みを読み込み
        let weight_size = HALFKA_HM_DIMENSIONS * NNUE_PYTORCH_L1;
        let mut weights = AlignedBox::new_zeroed(weight_size);
        for weight in weights.iter_mut() {
            reader.read_exact(&mut buf)?;
            *weight = i16::from_le_bytes(buf);
        }

        Ok(Self {
            biases: Aligned(biases),
            weights,
        })
    }

    /// LEB128圧縮形式から読み込み（自動検出）
    ///
    /// 圧縮/非圧縮を自動判定して読み込む。
    /// "COMPRESSED_LEB128"マジックがあれば圧縮形式として読み込む。
    pub fn read_leb128<R: Read>(reader: &mut R) -> io::Result<Self> {
        // バイアスを読み込み（圧縮形式を自動検出）
        let bias_vec = read_compressed_tensor_i16(reader, NNUE_PYTORCH_L1)?;
        let mut biases = [0i16; NNUE_PYTORCH_L1];
        biases.copy_from_slice(&bias_vec);

        // 重みを読み込み（圧縮形式を自動検出）
        let weight_size = HALFKA_HM_DIMENSIONS * NNUE_PYTORCH_L1;
        let weight_vec = read_compressed_tensor_i16(reader, weight_size)?;
        let mut weights = AlignedBox::new_zeroed(weight_size);
        weights.copy_from_slice(&weight_vec);

        Ok(Self {
            biases: Aligned(biases),
            weights,
        })
    }

    /// 差分計算を使わずにAccumulatorを計算
    pub fn refresh_accumulator(&self, pos: &Position, acc: &mut AccumulatorNnuePytorch) {
        for perspective in [Color::Black, Color::White] {
            let p = perspective as usize;
            let accumulation = acc.get_mut(p);

            // バイアスで初期化
            accumulation.copy_from_slice(&self.biases.0);

            // アクティブな特徴量の重みを加算
            let active_indices = self.get_active_features(pos, perspective);
            for &index in active_indices.iter() {
                self.add_weights(accumulation, index);
            }
        }

        acc.computed_accumulation = true;
        acc.computed_score = false;
    }

    /// 差分計算でAccumulatorを更新
    pub fn update_accumulator(
        &self,
        pos: &Position,
        dirty_piece: &DirtyPiece,
        acc: &mut AccumulatorNnuePytorch,
        prev_acc: &AccumulatorNnuePytorch,
    ) {
        for perspective in [Color::Black, Color::White] {
            let p = perspective as usize;
            let reset = HalfKA_hmFeatureSet::needs_refresh(dirty_piece, perspective);

            if reset {
                // 玉が移動した場合は全計算
                let accumulation = acc.get_mut(p);
                accumulation.copy_from_slice(&self.biases.0);

                let active_indices = self.get_active_features(pos, perspective);
                for &index in active_indices.iter() {
                    self.add_weights(accumulation, index);
                }
            } else {
                // 差分更新
                let (removed, added) = HalfKA_hmFeatureSet::collect_changed_indices(
                    dirty_piece,
                    perspective,
                    pos.king_square(perspective),
                );

                let prev = prev_acc.get(p);
                let curr = acc.get_mut(p);
                curr.copy_from_slice(prev);

                for &index in removed.iter() {
                    self.sub_weights(curr, index);
                }

                for &index in added.iter() {
                    self.add_weights(curr, index);
                }
            }
        }

        acc.computed_accumulation = true;
        acc.computed_score = false;
    }

    /// 複数手分の差分を適用してアキュムレータを更新
    pub fn forward_update_incremental(
        &self,
        pos: &Position,
        stack: &mut AccumulatorStackNnuePytorch,
        source_idx: usize,
    ) -> bool {
        let Some(path) = stack.collect_path(source_idx) else {
            // パスが途切れた場合、または MAX_PATH_LENGTH を超えた場合
            return false;
        };

        let source_acc = stack.entry_at(source_idx).accumulator.clone();
        {
            let current_acc = &mut stack.current_mut().accumulator;
            for perspective in [Color::Black, Color::White] {
                let p = perspective as usize;
                current_acc.get_mut(p).copy_from_slice(source_acc.get(p));
            }
        }

        for &entry_idx in path.iter() {
            let dirty_piece = stack.entry_at(entry_idx).dirty_piece;

            for perspective in [Color::Black, Color::White] {
                debug_assert!(
                    !dirty_piece.king_moved[perspective.index()],
                    "King moved between source and current"
                );

                let king_sq = pos.king_square(perspective);
                let (removed, added) = HalfKA_hmFeatureSet::collect_changed_indices(
                    &dirty_piece,
                    perspective,
                    king_sq,
                );

                let p = perspective as usize;
                let accumulation = stack.current_mut().accumulator.get_mut(p);

                for &index in removed.iter() {
                    self.sub_weights(accumulation, index);
                }
                for &index in added.iter() {
                    self.add_weights(accumulation, index);
                }
            }
        }

        stack.current_mut().accumulator.computed_accumulation = true;
        stack.current_mut().accumulator.computed_score = false;
        true
    }

    /// アクティブな特徴量のインデックスリストを取得
    #[inline]
    fn get_active_features(
        &self,
        pos: &Position,
        perspective: Color,
    ) -> IndexList<MAX_ACTIVE_FEATURES> {
        HalfKA_hmFeatureSet::collect_active_indices(pos, perspective)
    }

    /// 重みを累積値に加算
    #[inline]
    fn add_weights(&self, accumulation: &mut [i16; NNUE_PYTORCH_L1], index: usize) {
        let offset = index * NNUE_PYTORCH_L1;
        if offset + NNUE_PYTORCH_L1 > self.weights.len() {
            feature_index_oob(index, self.weights.len() / NNUE_PYTORCH_L1);
        }

        let weights = &self.weights[offset..offset + NNUE_PYTORCH_L1];

        // スカラー実装（SIMD最適化は後で追加）
        for (acc, &weight) in accumulation.iter_mut().zip(weights) {
            *acc = acc.wrapping_add(weight);
        }
    }

    /// 重みを累積値から減算
    #[inline]
    fn sub_weights(&self, accumulation: &mut [i16; NNUE_PYTORCH_L1], index: usize) {
        let offset = index * NNUE_PYTORCH_L1;
        if offset + NNUE_PYTORCH_L1 > self.weights.len() {
            feature_index_oob(index, self.weights.len() / NNUE_PYTORCH_L1);
        }

        let weights = &self.weights[offset..offset + NNUE_PYTORCH_L1];

        // スカラー実装（SIMD最適化は後で追加）
        for (acc, &weight) in accumulation.iter_mut().zip(weights) {
            *acc = acc.wrapping_sub(weight);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_feature_transformer_dimensions() {
        // 次元数の確認
        assert_eq!(NNUE_PYTORCH_L1, 1536);
        assert_eq!(HALFKA_HM_DIMENSIONS, 73305);
    }
}
