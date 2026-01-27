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

// =============================================================================
// HalfKP FT hash からの L1 検出
// =============================================================================

/// HalfKP の FT hash から L1 を検出
///
/// nnue-pytorch がハードコードしたアーキテクチャ文字列は不正確なことがある。
/// FT hash は L1 から一意に計算可能なため、これを使って実際の L1 を検出する。
///
/// # 計算式
///
/// ```text
/// FT hash = HALFKP_HASH ^ (L1 * 2)
/// HALFKP_HASH = 0x5D69D5B8 (HalfKP(Friend) のベースハッシュ)
/// ```
///
/// # 引数
/// - `ft_hash`: ファイルの offset 196-200 から読んだ FT hash
///
/// # 戻り値
/// - `Some(L1)`: 一致する L1 が見つかった
/// - `None`: 既知の L1 と一致しない
pub fn detect_halfkp_l1_from_ft_hash(ft_hash: u32) -> Option<usize> {
    // HalfKP(Friend) のベースハッシュ
    // nnue-pytorch での計算: hash ^= self.L1 * 2
    const HALFKP_HASH: u32 = 0x5D69D5B8;

    // 既知の L1 値
    const KNOWN_L1_VALUES: &[usize] = &[256, 512, 768, 1024];

    for &l1 in KNOWN_L1_VALUES {
        let expected_ft_hash = HALFKP_HASH ^ (l1 as u32 * 2);
        if ft_hash == expected_ft_hash {
            return Some(l1);
        }
    }
    None
}

/// L1 に対応するデフォルトの L2/L3 を取得
///
/// nnue-pytorch 形式のファイルで L2/L3 がハードコードされている場合、
/// L1 から最も一般的な L2/L3 の組み合わせを推測する。
///
/// # 既知の組み合わせ
///
/// | L1 | L2 | L3 | 備考 |
/// |------|----|----|------|
/// | 256 | 32 | 32 | suisho5 互換 |
/// | 512 | 8 | 96 | 一般的 |
/// | 768 | 16 | 64 | AobaNNUE 形式 |
/// | 1024 | 8 | 32 | 一般的 |
pub fn default_halfkp_l2_l3(l1: usize) -> (usize, usize) {
    match l1 {
        256 => (32, 32),
        512 => (8, 96),
        768 => (16, 64),
        1024 => (8, 32),
        _ => (32, 32), // フォールバック
    }
}

/// HalfKP ファイルの期待サイズを計算
///
/// L1/L2/L3 から期待されるファイルサイズを計算して検証に使用する。
///
/// # バイナリレイアウト (nnue-pytorch 形式、hash あり)
///
/// | オフセット | サイズ | 内容 |
/// |-----------|--------|------|
/// | 0 | 4 B | VERSION |
/// | 4 | 4 B | ネットワークハッシュ |
/// | 8 | 4 B | description長さ |
/// | 12 | N B | description文字列 |
/// | 12+N | 4 B | FT hash |
/// | 16+N | L1*2 B | FT bias |
/// | 16+N+L1*2 | 125388*L1*2 B | FT weight |
/// | ... | 4 B | Network hash |
/// | ... | L2*4 B | l1 bias |
/// | ... | ceil(L1*2/32)*32*L2 B | l1 weight |
/// | ... | L3*4 B | l2 bias |
/// | ... | ceil(L2/32)*32*L3 B | l2 weight |
/// | ... | 4 B | output bias |
/// | ... | L3 B | output weight |
pub fn expected_halfkp_file_size(l1: usize, l2: usize, l3: usize, arch_len: usize) -> u64 {
    const HALFKP_DIMENSIONS: usize = 125388;

    // 32 の倍数に切り上げ
    fn pad32(n: usize) -> usize {
        n.div_ceil(32) * 32
    }

    let header = 12 + arch_len; // version + hash + arch_len + arch_str
    let ft_hash = 4;
    let ft_bias = l1 * 2;
    let ft_weight = HALFKP_DIMENSIONS * l1 * 2;
    let network_hash = 4;
    let l1_bias = l2 * 4;
    let l1_weight = pad32(l1 * 2) * l2;
    let l2_bias = l3 * 4;
    let l2_weight = pad32(l2) * l3;
    let output_bias = 4;
    let output_weight = l3;

    (header
        + ft_hash
        + ft_bias
        + ft_weight
        + network_hash
        + l1_bias
        + l1_weight
        + l2_bias
        + l2_weight
        + output_bias
        + output_weight) as u64
}

/// ファイルサイズから L2/L3 を検出
///
/// L1 が判明している場合、ファイルサイズを使って L2/L3 を特定する。
pub fn detect_halfkp_l2_l3_from_size(
    l1: usize,
    file_size: u64,
    arch_len: usize,
) -> Option<(usize, usize)> {
    // 既知の L2/L3 の組み合わせ（L1 ごと）
    let candidates: &[(usize, usize)] = match l1 {
        256 => &[(32, 32)],
        512 => &[(8, 96), (32, 32)],
        768 => &[(16, 64)],
        1024 => &[(8, 32), (8, 96)],
        _ => return None,
    };

    for &(l2, l3) in candidates {
        if expected_halfkp_file_size(l1, l2, l3, arch_len) == file_size {
            return Some((l2, l3));
        }
    }
    None
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

    // =============================================================================
    // FT hash 検出テスト
    // =============================================================================

    #[test]
    fn test_detect_halfkp_l1_from_ft_hash() {
        // 既知の L1 値に対する期待される FT hash
        // HALFKP_HASH = 0x5D69D5B8
        // FT hash = HALFKP_HASH ^ (L1 * 2)

        // L1=256: 0x5D69D5B8 ^ 512 = 0x5D69D5B8 ^ 0x200 = 0x5D69D7B8
        assert_eq!(detect_halfkp_l1_from_ft_hash(0x5D69D7B8), Some(256));

        // L1=512: 0x5D69D5B8 ^ 1024 = 0x5D69D5B8 ^ 0x400 = 0x5D69D1B8
        assert_eq!(detect_halfkp_l1_from_ft_hash(0x5D69D1B8), Some(512));

        // L1=768: 0x5D69D5B8 ^ 1536 = 0x5D69D5B8 ^ 0x600 = 0x5D69D3B8
        // nn_bin_info.md で確認済みの値
        assert_eq!(detect_halfkp_l1_from_ft_hash(0x5D69D3B8), Some(768));

        // L1=1024: 0x5D69D5B8 ^ 2048 = 0x5D69D5B8 ^ 0x800 = 0x5D69DDB8
        assert_eq!(detect_halfkp_l1_from_ft_hash(0x5D69DDB8), Some(1024));

        // 不明な FT hash
        assert_eq!(detect_halfkp_l1_from_ft_hash(0x12345678), None);
        assert_eq!(detect_halfkp_l1_from_ft_hash(0), None);
    }

    #[test]
    fn test_default_halfkp_l2_l3() {
        assert_eq!(default_halfkp_l2_l3(256), (32, 32));
        assert_eq!(default_halfkp_l2_l3(512), (8, 96));
        assert_eq!(default_halfkp_l2_l3(768), (16, 64));
        assert_eq!(default_halfkp_l2_l3(1024), (8, 32));
        // 不明な L1 はフォールバック
        assert_eq!(default_halfkp_l2_l3(999), (32, 32));
    }

    #[test]
    fn test_expected_halfkp_file_size() {
        // L1=768, L2=16, L3=64, arch_len=184 の場合
        // nn_bin_info.md で確認済み: 192,624,720 bytes
        let size = expected_halfkp_file_size(768, 16, 64, 184);
        assert_eq!(size, 192_624_720);
    }

    #[test]
    fn test_detect_halfkp_l2_l3_from_size() {
        // L1=768, file_size=192,624,720, arch_len=184
        let result = detect_halfkp_l2_l3_from_size(768, 192_624_720, 184);
        assert_eq!(result, Some((16, 64)));

        // ファイルサイズが一致しない場合
        let result = detect_halfkp_l2_l3_from_size(768, 12345, 184);
        assert_eq!(result, None);

        // 不明な L1
        let result = detect_halfkp_l2_l3_from_size(999, 12345, 184);
        assert_eq!(result, None);
    }
}
