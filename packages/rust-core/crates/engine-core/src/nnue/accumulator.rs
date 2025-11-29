//! Accumulator - 入力特徴量の累積値を保持
//!
//! HalfKP 特徴量を FeatureTransformer で変換した結果を視点ごとに保持し、
//! 差分更新対応の評価値計算を行うための中間バッファ。
//! 現状の実装では全計算パスのみを提供し、差分更新は今後拡張予定。

use super::constants::TRANSFORMED_FEATURE_DIMENSIONS;
use crate::types::Value;

/// アライメントを保証するラッパー（64バイト = キャッシュライン）
#[repr(C, align(64))]
#[derive(Clone)]
pub struct Aligned<T>(pub T);

impl<T: Default> Default for Aligned<T> {
    fn default() -> Self {
        Self(T::default())
    }
}

/// Accumulatorの構造
/// 入力特徴量をアフィン変換した結果を保持
#[repr(C, align(64))]
#[derive(Clone)]
pub struct Accumulator {
    /// 累積値 [perspective][dimension]
    /// - perspective: BLACK=0, WHITE=1
    pub accumulation: [Aligned<[i16; TRANSFORMED_FEATURE_DIMENSIONS]>; 2],

    /// 計算済みの評価値（キャッシュ）
    pub score: Value,

    /// accumulationが計算済みかどうか
    pub computed_accumulation: bool,

    /// scoreが計算済みかどうか
    pub computed_score: bool,
}

impl Default for Accumulator {
    fn default() -> Self {
        Self {
            accumulation: [
                Aligned([0i16; TRANSFORMED_FEATURE_DIMENSIONS]),
                Aligned([0i16; TRANSFORMED_FEATURE_DIMENSIONS]),
            ],
            score: Value::ZERO,
            computed_accumulation: false,
            computed_score: false,
        }
    }
}

impl Accumulator {
    /// 新しいAccumulatorを作成
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// リセット（計算済みフラグをクリア）
    #[inline]
    pub fn reset(&mut self) {
        self.computed_accumulation = false;
        self.computed_score = false;
    }

    /// 視点ごとの累積値への参照を取得
    #[inline]
    pub fn get(&self, perspective: usize) -> &[i16; TRANSFORMED_FEATURE_DIMENSIONS] {
        &self.accumulation[perspective].0
    }

    /// 視点ごとの累積値への可変参照を取得
    #[inline]
    pub fn get_mut(&mut self, perspective: usize) -> &mut [i16; TRANSFORMED_FEATURE_DIMENSIONS] {
        &mut self.accumulation[perspective].0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_accumulator_new() {
        let acc = Accumulator::new();
        assert!(!acc.computed_accumulation);
        assert!(!acc.computed_score);
        assert_eq!(acc.score, Value::ZERO);
    }

    #[test]
    fn test_accumulator_reset() {
        let mut acc = Accumulator::new();
        acc.computed_accumulation = true;
        acc.computed_score = true;

        acc.reset();

        assert!(!acc.computed_accumulation);
        assert!(!acc.computed_score);
    }

    #[test]
    fn test_accumulator_get() {
        let mut acc = Accumulator::new();
        acc.accumulation[0].0[0] = 100;
        acc.accumulation[1].0[0] = 200;

        assert_eq!(acc.get(0)[0], 100);
        assert_eq!(acc.get(1)[0], 200);
    }

    #[test]
    fn test_accumulator_alignment() {
        let acc = Accumulator::new();
        let addr = &acc as *const _ as usize;
        // 64バイトアライメントを確認
        assert_eq!(addr % 64, 0);
    }
}
