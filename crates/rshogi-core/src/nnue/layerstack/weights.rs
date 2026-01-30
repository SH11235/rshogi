//! LayerStack 重み構造体
//!
//! LSNN ファイルから読み込んだ重みを保持する構造体。
//! Factorizer は export 時に L1 に統合済みのため、推論側では持たない。

use super::bucket::BucketDivision;
use super::constants::*;

/// Feature Transformer の重み
pub struct FtWeights {
    /// バイアス: [i16; 1536]
    pub bias: Box<[i16; FT_PER_PERSPECTIVE]>,

    /// 重み: i16[HALFKA_FEATURES][1536]（row-major）
    ///
    /// weight[feature_idx * FT_PER_PERSPECTIVE + output_idx]
    pub weight: Box<[i16]>,
}

impl FtWeights {
    /// 新規作成（ゼロ初期化）
    pub fn new() -> Self {
        Self {
            bias: Box::new([0i16; FT_PER_PERSPECTIVE]),
            weight: vec![0i16; HALFKA_FEATURES * FT_PER_PERSPECTIVE].into_boxed_slice(),
        }
    }
}

impl Default for FtWeights {
    fn default() -> Self {
        Self::new()
    }
}

/// L1 層の重み（1バケット分）
pub struct L1WeightsBucket {
    /// 重み: i8[16][1536]（row-major）
    pub weight: Box<[i8; L1_OUT * L1_IN]>,

    /// バイアス: i32[16]
    pub bias: Box<[i32; L1_OUT]>,
}

impl L1WeightsBucket {
    /// 新規作成（ゼロ初期化）
    pub fn new() -> Self {
        Self {
            weight: Box::new([0i8; L1_OUT * L1_IN]),
            bias: Box::new([0i32; L1_OUT]),
        }
    }
}

impl Default for L1WeightsBucket {
    fn default() -> Self {
        Self::new()
    }
}

/// L2 層の重み（1バケット分）
pub struct L2WeightsBucket {
    /// 重み: i8[64][30]（row-major）
    pub weight: Box<[i8; L2_OUT * DUAL_ACT_OUT]>,

    /// バイアス: i32[64]
    pub bias: Box<[i32; L2_OUT]>,
}

impl L2WeightsBucket {
    /// 新規作成（ゼロ初期化）
    pub fn new() -> Self {
        Self {
            weight: Box::new([0i8; L2_OUT * DUAL_ACT_OUT]),
            bias: Box::new([0i32; L2_OUT]),
        }
    }
}

impl Default for L2WeightsBucket {
    fn default() -> Self {
        Self::new()
    }
}

/// Output 層の重み（1バケット分）
pub struct OutWeightsBucket {
    /// 重み: i8[64]
    pub weight: Box<[i8; L2_OUT]>,

    /// バイアス: i32
    pub bias: i32,
}

impl OutWeightsBucket {
    /// 新規作成（ゼロ初期化）
    pub fn new() -> Self {
        Self {
            weight: Box::new([0i8; L2_OUT]),
            bias: 0,
        }
    }
}

impl Default for OutWeightsBucket {
    fn default() -> Self {
        Self::new()
    }
}

/// LayerStack 全体の重み
pub struct LayerStackWeights {
    /// バケット分割方式
    pub bucket_division: BucketDivision,

    /// bypass 使用フラグ
    pub use_bypass: bool,

    /// Feature Transformer の重み
    pub ft: FtWeights,

    /// L1 層の重み（バケット数分）
    pub l1: Vec<L1WeightsBucket>,

    /// L2 層の重み（バケット数分）
    pub l2: Vec<L2WeightsBucket>,

    /// Output 層の重み（バケット数分）
    pub out: Vec<OutWeightsBucket>,
}

impl LayerStackWeights {
    /// 新規作成
    ///
    /// # 引数
    ///
    /// - `bucket_division`: バケット分割方式
    /// - `use_bypass`: bypass 使用フラグ
    pub fn new(bucket_division: BucketDivision, use_bypass: bool) -> Self {
        let num_buckets = bucket_division.num_buckets();

        Self {
            bucket_division,
            use_bypass,
            ft: FtWeights::new(),
            l1: (0..num_buckets).map(|_| L1WeightsBucket::new()).collect(),
            l2: (0..num_buckets).map(|_| L2WeightsBucket::new()).collect(),
            out: (0..num_buckets).map(|_| OutWeightsBucket::new()).collect(),
        }
    }

    /// バケット数を取得
    #[inline]
    pub fn num_buckets(&self) -> usize {
        self.bucket_division.num_buckets()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ft_weights_size() {
        let ft = FtWeights::new();
        assert_eq!(ft.bias.len(), FT_PER_PERSPECTIVE);
        assert_eq!(ft.weight.len(), HALFKA_FEATURES * FT_PER_PERSPECTIVE);
    }

    #[test]
    fn test_l1_weights_size() {
        let l1 = L1WeightsBucket::new();
        assert_eq!(l1.weight.len(), L1_OUT * L1_IN);
        assert_eq!(l1.bias.len(), L1_OUT);
    }

    #[test]
    fn test_l2_weights_size() {
        let l2 = L2WeightsBucket::new();
        assert_eq!(l2.weight.len(), L2_OUT * DUAL_ACT_OUT);
        assert_eq!(l2.bias.len(), L2_OUT);
    }

    #[test]
    fn test_out_weights_size() {
        let out = OutWeightsBucket::new();
        assert_eq!(out.weight.len(), L2_OUT);
    }

    #[test]
    fn test_layerstack_weights_2x2() {
        let weights = LayerStackWeights::new(BucketDivision::TwoByTwo, true);
        assert_eq!(weights.num_buckets(), 4);
        assert_eq!(weights.l1.len(), 4);
        assert_eq!(weights.l2.len(), 4);
        assert_eq!(weights.out.len(), 4);
        assert!(weights.use_bypass);
    }

    #[test]
    fn test_layerstack_weights_3x3() {
        let weights = LayerStackWeights::new(BucketDivision::ThreeByThree, false);
        assert_eq!(weights.num_buckets(), 9);
        assert_eq!(weights.l1.len(), 9);
        assert_eq!(weights.l2.len(), 9);
        assert_eq!(weights.out.len(), 9);
        assert!(!weights.use_bypass);
    }
}
