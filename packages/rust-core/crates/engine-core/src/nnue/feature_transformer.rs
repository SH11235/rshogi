//! FeatureTransformer - 入力特徴量を変換する最初の層
//!
//! HalfKP特徴量を256次元の中間表現に変換

use super::accumulator::{Accumulator, Aligned};
use super::bona_piece::{halfkp_index, BonaPiece};
use super::constants::{HALFKP_DIMENSIONS, TRANSFORMED_FEATURE_DIMENSIONS};
use crate::position::Position;
use crate::types::Color;
use std::io::{self, Read};

/// FeatureTransformerのパラメータ
#[repr(C, align(64))]
pub struct FeatureTransformer {
    /// バイアス [half_dimensions]
    pub biases: Aligned<[i16; TRANSFORMED_FEATURE_DIMENSIONS]>,

    /// 重み [input_dimensions][half_dimensions]
    /// 大きいのでBox化
    pub weights: Box<[i16]>,
}

impl FeatureTransformer {
    /// ファイルから読み込み
    pub fn read<R: Read>(reader: &mut R) -> io::Result<Self> {
        // バイアスを読み込み
        let mut biases = [0i16; TRANSFORMED_FEATURE_DIMENSIONS];
        let mut buf = [0u8; 2];
        for bias in biases.iter_mut() {
            reader.read_exact(&mut buf)?;
            *bias = i16::from_le_bytes(buf);
        }

        // 重みを読み込み
        let weight_size = HALFKP_DIMENSIONS * TRANSFORMED_FEATURE_DIMENSIONS;
        let mut weights = vec![0i16; weight_size].into_boxed_slice();
        for weight in weights.iter_mut() {
            reader.read_exact(&mut buf)?;
            *weight = i16::from_le_bytes(buf);
        }

        Ok(Self {
            biases: Aligned(biases),
            weights,
        })
    }

    /// 差分計算を使わずにAccumulatorを計算
    pub fn refresh_accumulator(&self, pos: &Position, acc: &mut Accumulator) {
        for perspective in [Color::Black, Color::White] {
            let p = perspective as usize;
            let accumulation = acc.get_mut(p);

            // バイアスで初期化
            accumulation.copy_from_slice(&self.biases.0);

            // アクティブな特徴量の重みを加算
            let active_indices = self.get_active_features(pos, perspective);
            for index in active_indices {
                self.add_weights(accumulation, index);
            }
        }

        acc.computed_accumulation = true;
        acc.computed_score = false;
    }

    /// 差分計算でAccumulatorを更新
    ///
    /// 戻り値: 差分更新が成功したらtrue、全計算が必要ならfalse
    pub fn update_accumulator(
        &self,
        _pos: &Position,
        acc: &mut Accumulator,
        prev_acc: &Accumulator,
    ) -> bool {
        for perspective in [Color::Black, Color::White] {
            let p = perspective as usize;

            // 前の値をコピー
            let prev = prev_acc.get(p);
            let curr = acc.get_mut(p);
            curr.copy_from_slice(prev);

            // TODO: 差分更新のロジック
            // 現時点では全計算にフォールバック
        }

        // 差分更新が実装されるまでは全計算にフォールバック
        false
    }

    /// アクティブな特徴量のインデックスリストを取得
    fn get_active_features(&self, pos: &Position, perspective: Color) -> Vec<usize> {
        let king_sq = pos.king_square(perspective);
        let mut features = Vec::with_capacity(38); // 最大38駒（玉2つ以外）

        // 盤上の駒
        for sq in pos.occupied().iter() {
            let pc = pos.piece_on(sq);
            if pc.is_none() {
                continue;
            }
            // 玉は特徴量に含めない
            if pc.piece_type() == crate::types::PieceType::King {
                continue;
            }

            let bp = BonaPiece::from_piece_square(pc, sq, perspective);
            if bp != BonaPiece::ZERO {
                features.push(halfkp_index(king_sq, bp));
            }
        }

        // TODO: 手駒の特徴量

        features
    }

    /// 重みを累積値に加算
    #[inline]
    fn add_weights(&self, accumulation: &mut [i16; TRANSFORMED_FEATURE_DIMENSIONS], index: usize) {
        let offset = index * TRANSFORMED_FEATURE_DIMENSIONS;
        if offset + TRANSFORMED_FEATURE_DIMENSIONS > self.weights.len() {
            return; // 範囲外チェック
        }

        for (acc, &weight) in accumulation
            .iter_mut()
            .zip(&self.weights[offset..offset + TRANSFORMED_FEATURE_DIMENSIONS])
        {
            *acc = acc.saturating_add(weight);
        }
    }

    /// 重みを累積値から減算
    #[inline]
    #[allow(dead_code)]
    fn sub_weights(&self, accumulation: &mut [i16; TRANSFORMED_FEATURE_DIMENSIONS], index: usize) {
        let offset = index * TRANSFORMED_FEATURE_DIMENSIONS;
        if offset + TRANSFORMED_FEATURE_DIMENSIONS > self.weights.len() {
            return;
        }

        for (acc, &weight) in accumulation
            .iter_mut()
            .zip(&self.weights[offset..offset + TRANSFORMED_FEATURE_DIMENSIONS])
        {
            *acc = acc.saturating_sub(weight);
        }
    }

    /// Accumulatorの値を変換して出力
    /// ClippedReLU(clamp(0, 127))を適用し、両視点を結合
    pub fn transform(
        &self,
        acc: &Accumulator,
        side_to_move: Color,
        output: &mut [u8; TRANSFORMED_FEATURE_DIMENSIONS * 2],
    ) {
        let perspectives = [side_to_move, !side_to_move];

        for (p, &perspective) in perspectives.iter().enumerate() {
            let offset = TRANSFORMED_FEATURE_DIMENSIONS * p;
            let accumulation = acc.get(perspective as usize);

            for i in 0..TRANSFORMED_FEATURE_DIMENSIONS {
                output[offset + i] = accumulation[i].clamp(0, 127) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add_weights() {
        // ダミーのFeatureTransformerを作成
        let ft = FeatureTransformer {
            biases: Aligned([0i16; TRANSFORMED_FEATURE_DIMENSIONS]),
            weights: vec![1i16; HALFKP_DIMENSIONS * TRANSFORMED_FEATURE_DIMENSIONS]
                .into_boxed_slice(),
        };

        let mut acc = [0i16; TRANSFORMED_FEATURE_DIMENSIONS];
        ft.add_weights(&mut acc, 0);

        // 全て1が加算されているはず
        for &val in acc.iter() {
            assert_eq!(val, 1);
        }
    }
}
