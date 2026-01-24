//! NNUE アーキテクチャ仕様の型定義
//!
//! ネットワークのアーキテクチャを一意に識別するための型を提供する。

/// 特徴量セット
///
/// NNUEネットワークの入力特徴量の種類を表す。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FeatureSet {
    /// HalfKP (classic NNUE)
    HalfKP,
    /// HalfKA_hm^ (Half-Mirror + Factorization)
    HalfKA,
    /// LayerStacks (実験的)
    LayerStacks,
}

impl FeatureSet {
    /// 文字列表現
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::HalfKP => "HalfKP",
            Self::HalfKA => "HalfKA",
            Self::LayerStacks => "LayerStacks",
        }
    }
}

impl std::fmt::Display for FeatureSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// 活性化関数
///
/// FeatureTransformer 出力の活性化関数の種類を表す。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Activation {
    /// Clipped ReLU: `y = clamp(x, 0, QA)`
    CReLU,
    /// Squared Clipped ReLU: `y = clamp(x, 0, QA)²`
    SCReLU,
    /// Pairwise Clipped ReLU: `y = clamp(a, 0, QA) * clamp(b, 0, QA) >> shift`
    PairwiseCReLU,
}

impl Activation {
    /// 文字列表現
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::CReLU => "CReLU",
            Self::SCReLU => "SCReLU",
            Self::PairwiseCReLU => "PairwiseCReLU",
        }
    }

    /// 出力次元の除数
    ///
    /// L1層入力次元 = FT出力次元 * 2 / OUTPUT_DIM_DIVISOR
    ///
    /// - CReLU, SCReLU: 1（次元維持）
    /// - PairwiseCReLU: 2（次元半減）
    pub fn output_dim_divisor(&self) -> usize {
        match self {
            Self::CReLU | Self::SCReLU => 1,
            Self::PairwiseCReLU => 2,
        }
    }

    /// ヘッダー文字列のサフィックスから活性化関数を検出
    pub fn from_header_suffix(suffix: &str) -> Self {
        // NOTE: 長い識別子を先に判定しないと誤検出する
        if suffix.contains("-PairwiseCReLU") || suffix.contains("-Pairwise") {
            Self::PairwiseCReLU
        } else if suffix.contains("-SCReLU") {
            Self::SCReLU
        } else {
            Self::CReLU
        }
    }
}

impl std::fmt::Display for Activation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// アーキテクチャ仕様
///
/// ネットワークのアーキテクチャを一意に識別するための構造体。
/// `define_l1_variants!` マクロで自動生成される `SUPPORTED_SPECS` の要素として使用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ArchitectureSpec {
    /// 特徴量セット
    pub feature_set: FeatureSet,
    /// L1 サイズ (FeatureTransformer 出力次元)
    pub l1: usize,
    /// L2 サイズ (第1隠れ層出力次元)
    pub l2: usize,
    /// L3 サイズ (第2隠れ層出力次元)
    pub l3: usize,
    /// 活性化関数
    pub activation: Activation,
}

impl ArchitectureSpec {
    /// 新しい ArchitectureSpec を作成
    pub const fn new(
        feature_set: FeatureSet,
        l1: usize,
        l2: usize,
        l3: usize,
        activation: Activation,
    ) -> Self {
        Self {
            feature_set,
            l1,
            l2,
            l3,
            activation,
        }
    }

    /// アーキテクチャ名を生成
    ///
    /// 例: "HalfKA-512-8-96-CReLU"
    pub fn name(&self) -> String {
        format!("{}-{}-{}-{}-{}", self.feature_set, self.l1, self.l2, self.l3, self.activation)
    }
}

impl std::fmt::Display for ArchitectureSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_feature_set_display() {
        assert_eq!(FeatureSet::HalfKP.as_str(), "HalfKP");
        assert_eq!(FeatureSet::HalfKA.as_str(), "HalfKA");
        assert_eq!(FeatureSet::LayerStacks.as_str(), "LayerStacks");
    }

    #[test]
    fn test_activation_display() {
        assert_eq!(Activation::CReLU.as_str(), "CReLU");
        assert_eq!(Activation::SCReLU.as_str(), "SCReLU");
        assert_eq!(Activation::PairwiseCReLU.as_str(), "PairwiseCReLU");
    }

    #[test]
    fn test_activation_output_dim_divisor() {
        assert_eq!(Activation::CReLU.output_dim_divisor(), 1);
        assert_eq!(Activation::SCReLU.output_dim_divisor(), 1);
        assert_eq!(Activation::PairwiseCReLU.output_dim_divisor(), 2);
    }

    #[test]
    fn test_activation_from_header_suffix() {
        assert_eq!(
            Activation::from_header_suffix("Features=HalfKA_hm[73305->512x2]"),
            Activation::CReLU
        );
        assert_eq!(
            Activation::from_header_suffix("Features=HalfKA_hm[73305->512x2]-SCReLU"),
            Activation::SCReLU
        );
        assert_eq!(
            Activation::from_header_suffix("Features=HalfKA_hm[73305->512/2x2]-Pairwise"),
            Activation::PairwiseCReLU
        );
        assert_eq!(
            Activation::from_header_suffix("Features=HalfKA_hm[73305->512/2x2]-PairwiseCReLU"),
            Activation::PairwiseCReLU
        );
    }

    #[test]
    fn test_architecture_spec_name() {
        let spec = ArchitectureSpec::new(FeatureSet::HalfKA, 512, 8, 96, Activation::CReLU);
        assert_eq!(spec.name(), "HalfKA-512-8-96-CReLU");

        let spec2 = ArchitectureSpec::new(FeatureSet::HalfKP, 256, 32, 32, Activation::SCReLU);
        assert_eq!(spec2.name(), "HalfKP-256-32-32-SCReLU");
    }
}
