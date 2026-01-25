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
    #[allow(non_camel_case_types)]
    HalfKA_hm,
    /// HalfKA (非ミラー)
    HalfKA,
    /// LayerStacks (実験的)
    LayerStacks,
}

impl FeatureSet {
    /// 文字列表現
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::HalfKP => "HalfKP",
            Self::HalfKA_hm => "HalfKA_hm",
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
    /// 例: "HalfKA_hm-512-8-96-CReLU"
    pub fn name(&self) -> String {
        format!("{}-{}-{}-{}-{}", self.feature_set, self.l1, self.l2, self.l3, self.activation)
    }
}

impl std::fmt::Display for ArchitectureSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name())
    }
}

/// アーキテクチャ解析結果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsedArchitecture {
    pub feature_set: FeatureSet,
    pub l1: usize,
    pub l2: usize,
    pub l3: usize,
}

/// FeatureSet 判定に必要な入力次元を抽出
pub fn parse_feature_input_dimensions(arch_str: &str) -> Option<usize> {
    let features_key = "Features=";
    let start = arch_str.find(features_key)?;
    let after_key = &arch_str[start + features_key.len()..];
    let bracket_start = after_key.find('[')?;
    let after_bracket = &after_key[bracket_start + 1..];
    let arrow_idx = after_bracket.find("->")?;
    let num_str = &after_bracket[..arrow_idx];
    num_str.parse::<usize>().ok()
}

/// アーキテクチャ文字列から FeatureSet を判定
pub fn parse_feature_set_from_arch(arch_str: &str) -> Result<FeatureSet, String> {
    use super::constants::{HALFKA_DIMENSIONS, HALFKA_HM_DIMENSIONS};

    if arch_str.contains("LayerStacks") || arch_str.contains("->1536x2]") {
        return Ok(FeatureSet::LayerStacks);
    }
    if arch_str.contains("HalfKP") {
        return Ok(FeatureSet::HalfKP);
    }
    if arch_str.contains("HalfKA_hm") {
        return Ok(FeatureSet::HalfKA_hm);
    }
    if arch_str.contains("HalfKA") {
        let input_dim = parse_feature_input_dimensions(arch_str).ok_or_else(|| {
            "HalfKA architecture is missing input dimensions in arch string.".to_string()
        })?;
        return match input_dim {
            HALFKA_HM_DIMENSIONS => Ok(FeatureSet::HalfKA_hm),
            HALFKA_DIMENSIONS => Ok(FeatureSet::HalfKA),
            _ => Err(format!("Unknown HalfKA input dimensions: {input_dim}")),
        };
    }

    Err("Unknown feature set in arch string.".to_string())
}

/// アーキテクチャ文字列から L1, L2, L3 を抽出
///
/// 戻り値: (L1, L2, L3)
/// パース失敗時はデフォルト値 (0, 0, 0) を返す
pub fn parse_arch_dimensions(arch_str: &str) -> (usize, usize, usize) {
    // L1: "->NNNx2]" または "->NNN/2x2]" (Pairwise) パターンを探す
    let l1 = if let Some(idx) = arch_str.find("x2]") {
        let before = &arch_str[..idx];
        if let Some(arrow_idx) = before.rfind("->") {
            let after_arrow = &before[arrow_idx + 2..];
            // Pairwise形式 "512/2" の場合は "/" で終端、通常形式なら全体が数値
            let num_str = if let Some(slash_idx) = after_arrow.find('/') {
                &after_arrow[..slash_idx]
            } else {
                after_arrow
            };
            num_str.parse::<usize>().unwrap_or(0)
        } else {
            0
        }
    } else {
        0
    };

    // L2, L3: AffineTransform[OUT<-IN] パターンを探す
    // 例: AffineTransform[8<-1024] → L2=8
    //     AffineTransform[96<-8] → L3=96
    let mut layers: Vec<(usize, usize)> = Vec::new();
    let pattern = "AffineTransform[";

    let mut search_start = 0;
    while let Some(start) = arch_str[search_start..].find(pattern) {
        let abs_start = search_start + start + pattern.len();
        if let Some(end) = arch_str[abs_start..].find(']') {
            let content = &arch_str[abs_start..abs_start + end];
            if let Some(arrow_idx) = content.find("<-") {
                let out_str = &content[..arrow_idx];
                let in_str = &content[arrow_idx + 2..];
                if let (Ok(out), Ok(inp)) = (out_str.parse::<usize>(), in_str.parse::<usize>()) {
                    layers.push((out, inp));
                }
            }
            search_start = abs_start + end;
        } else {
            break;
        }
    }

    // 1. まず bullet-shogi 形式 "l2=8,l3=96" を優先的にパース
    //    明示的に指定された値を尊重する
    let mut l2 = 0usize;
    let mut l3 = 0usize;
    for part in arch_str.split(',') {
        if let Some(val_str) = part.strip_prefix("l2=") {
            if let Ok(val) = val_str.parse::<usize>() {
                l2 = val;
            }
        } else if let Some(val_str) = part.strip_prefix("l3=") {
            if let Ok(val) = val_str.parse::<usize>() {
                l3 = val;
            }
        }
    }

    // 2. l2/l3 が取得できなかった場合、AffineTransform パターンでフォールバック
    //    nnue-pytorch のネストされた構造では、出力に近い順に並ぶ
    //    例: AffineTransform[1<-96](ClippedReLU[96](AffineTransform[96<-8](...)))
    //    パース結果: [1<-96], [96<-8], [8<-1024]
    //    逆順にして入力側から: [8<-1024] (L2), [96<-8] (L3), [1<-96] (output)
    if l2 == 0 || l3 == 0 {
        layers.reverse();
        if layers.len() >= 3 {
            if l2 == 0 {
                l2 = layers[0].0;
            }
            if l3 == 0 {
                l3 = layers[1].0;
            }
        }
    }

    (l1, l2, l3)
}

/// HalfKP アーキテクチャ文字列から L1 を抽出
///
/// パース失敗時は 0 を返す
pub fn parse_halfkp_l1(arch_str: &str) -> usize {
    // "->NNN" または "->NNN/2" (Pairwise) パターンを探す
    if let Some(idx) = arch_str.find("->") {
        let after = &arch_str[idx + 2..];
        let end = after.find(|c: char| !c.is_ascii_digit()).unwrap_or(after.len());
        let num_str = &after[..end];
        return num_str.parse().unwrap_or(0);
    }
    // "[NNNx2]" または "[NNN/2x2]" パターンを探す
    if let Some(idx) = arch_str.find("x2]") {
        let before = &arch_str[..idx];
        // Pairwise形式 "512/2" の場合
        if let Some(slash_idx) = before.rfind('/') {
            let num_part = &before[..slash_idx];
            if let Some(start) = num_part.rfind(|c: char| !c.is_ascii_digit()) {
                let num_str = &num_part[start + 1..];
                return num_str.parse().unwrap_or(0);
            }
        } else if let Some(start) = before.rfind(|c: char| !c.is_ascii_digit()) {
            let num_str = &before[start + 1..];
            return num_str.parse().unwrap_or(0);
        }
    }
    0
}

/// アーキテクチャ文字列を解析して主要パラメータを返す
pub fn parse_architecture(arch_str: &str) -> Result<ParsedArchitecture, String> {
    let feature_set = parse_feature_set_from_arch(arch_str)?;
    let (mut l1, l2, l3) = parse_arch_dimensions(arch_str);

    if feature_set == FeatureSet::HalfKP {
        let halfkp_l1 = parse_halfkp_l1(arch_str);
        if halfkp_l1 != 0 {
            l1 = halfkp_l1;
        }
    }

    Ok(ParsedArchitecture {
        feature_set,
        l1,
        l2,
        l3,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_feature_set_display() {
        assert_eq!(FeatureSet::HalfKP.as_str(), "HalfKP");
        assert_eq!(FeatureSet::HalfKA_hm.as_str(), "HalfKA_hm");
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
        let spec = ArchitectureSpec::new(FeatureSet::HalfKA_hm, 512, 8, 96, Activation::CReLU);
        assert_eq!(spec.name(), "HalfKA_hm-512-8-96-CReLU");

        let spec2 = ArchitectureSpec::new(FeatureSet::HalfKP, 256, 32, 32, Activation::SCReLU);
        assert_eq!(spec2.name(), "HalfKP-256-32-32-SCReLU");
    }

    #[test]
    fn test_parse_feature_set_from_arch() {
        assert_eq!(
            parse_feature_set_from_arch(
                "Features=HalfKA_hm[73305->512x2],Network=AffineTransform[1<-96]"
            )
            .unwrap(),
            FeatureSet::HalfKA_hm
        );
        assert_eq!(
            parse_feature_set_from_arch(
                "Features=HalfKA[138510->512x2],Network=AffineTransform[1<-96]"
            )
            .unwrap(),
            FeatureSet::HalfKA
        );
        assert_eq!(
            parse_feature_set_from_arch(
                "Features=HalfKA[73305->512x2],Network=AffineTransform[1<-96]"
            )
            .unwrap(),
            FeatureSet::HalfKA_hm
        );
        assert_eq!(
            parse_feature_set_from_arch("Features=HalfKP[125388->256x2]").unwrap(),
            FeatureSet::HalfKP
        );
    }

    #[test]
    fn test_parse_feature_set_from_arch_missing_dimensions() {
        let err = parse_feature_set_from_arch("Features=HalfKA,Network=AffineTransform[1<-96]")
            .unwrap_err();
        assert!(err.contains("missing input dimensions"));
    }

    #[test]
    fn test_parse_arch_dimensions() {
        // nnue-pytorch 形式 (ネスト構造、出力→入力の順)
        // 実際のファイル例: "Network=AffineTransform[1<-96](ClippedReLU[96](AffineTransform[96<-8](...)))"
        let arch = "Features=HalfKA_hm[73305->512x2],Network=AffineTransform[1<-96](ClippedReLU[96](AffineTransform[96<-8](ClippedReLU[8](AffineTransform[8<-1024](InputSlice[1024(0:1024)])))))";
        assert_eq!(parse_arch_dimensions(arch), (512, 8, 96));

        // nnue-pytorch 形式 (1024次元)
        let arch = "Features=HalfKA_hm[73305->1024x2],Network=AffineTransform[1<-96](ClippedReLU[96](AffineTransform[96<-8](ClippedReLU[8](AffineTransform[8<-2048](InputSlice[2048(0:2048)])))))";
        assert_eq!(parse_arch_dimensions(arch), (1024, 8, 96));

        // bullet-shogi 形式 (l2=, l3= パターン)
        let arch = "Features=HalfKA_hm^[73305->512x2]-SCReLU,fv_scale=13,l2=8,l3=96,qa=127,qb=64";
        assert_eq!(parse_arch_dimensions(arch), (512, 8, 96));

        // bullet-shogi 形式 (1024次元)
        let arch = "Features=HalfKA_hm^[73305->1024x2]-SCReLU,fv_scale=16,l2=8,l3=96,qa=127,qb=64";
        assert_eq!(parse_arch_dimensions(arch), (1024, 8, 96));

        // bullet-shogi 形式 (256次元, 32-32)
        let arch = "Features=HalfKA_hm^[73305->256x2]-SCReLU,fv_scale=13,l2=32,l3=32,qa=127,qb=64";
        assert_eq!(parse_arch_dimensions(arch), (256, 32, 32));

        // L1のみ取得できる場合 (L2/L3 は 0)
        let arch = "Features=HalfKP[125388->256x2]";
        assert_eq!(parse_arch_dimensions(arch), (256, 0, 0));

        // Pairwise 形式 (512/2x2 = 出力512、Pairwise乗算で256に縮小)
        let arch = "Features=HalfKA_hm[73305->512/2x2]-Pairwise,fv_scale=10,l1_input=512,l2=8,l3=96,qa=255,qb=64,scale=1600,pairwise=true";
        assert_eq!(parse_arch_dimensions(arch), (512, 8, 96));

        // Pairwise 形式 (256/2x2)
        let arch = "Features=HalfKA_hm[73305->256/2x2]-Pairwise,fv_scale=10,l1_input=256,l2=32,l3=32,qa=255,qb=64";
        assert_eq!(parse_arch_dimensions(arch), (256, 32, 32));

        // 何も取得できない場合
        assert_eq!(parse_arch_dimensions("unknown"), (0, 0, 0));
        assert_eq!(parse_arch_dimensions(""), (0, 0, 0));
    }
}
