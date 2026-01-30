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
}

impl FeatureSet {
    /// 文字列表現
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::HalfKP => "HalfKP",
            Self::HalfKA_hm => "HalfKA_hm",
            Self::HalfKA => "HalfKA",
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
// ファイルサイズベースのアーキテクチャ検出
// =============================================================================

/// 32 の倍数に切り上げ（const fn版）
const fn pad32(n: usize) -> usize {
    n.div_ceil(32) * 32
}

/// HalfKP の network_payload を計算
///
/// network_payload はヘッダーと hash を除いた純粋なネットワークデータサイズ。
/// これはアーキテクチャ（L1, L2, L3）から一意に決まる。
pub const fn network_payload_halfkp(l1: usize, l2: usize, l3: usize) -> u64 {
    const HALFKP_DIMENSIONS: usize = 125388;

    let ft_bias = l1 * 2;
    let ft_weight = HALFKP_DIMENSIONS * l1 * 2;
    let l1_bias = l2 * 4;
    let l1_weight = pad32(l1 * 2) * l2;
    let l2_bias = l3 * 4;
    let l2_weight = pad32(l2) * l3;
    let output_bias = 4;
    let output_weight = l3;

    (ft_bias + ft_weight + l1_bias + l1_weight + l2_bias + l2_weight + output_bias + output_weight)
        as u64
}

/// HalfKA_hm の network_payload を計算
pub const fn network_payload_halfka_hm(l1: usize, l2: usize, l3: usize) -> u64 {
    const HALFKA_HM_DIMENSIONS: usize = 73305;

    let ft_bias = l1 * 2;
    let ft_weight = HALFKA_HM_DIMENSIONS * l1 * 2;
    let l1_bias = l2 * 4;
    let l1_weight = pad32(l1 * 2) * l2;
    let l2_bias = l3 * 4;
    let l2_weight = pad32(l2) * l3;
    let output_bias = 4;
    let output_weight = l3;

    (ft_bias + ft_weight + l1_bias + l1_weight + l2_bias + l2_weight + output_bias + output_weight)
        as u64
}

/// HalfKA の network_payload を計算
pub const fn network_payload_halfka(l1: usize, l2: usize, l3: usize) -> u64 {
    const HALFKA_DIMENSIONS: usize = 138510;

    let ft_bias = l1 * 2;
    let ft_weight = HALFKA_DIMENSIONS * l1 * 2;
    let l1_bias = l2 * 4;
    let l1_weight = pad32(l1 * 2) * l2;
    let l2_bias = l3 * 4;
    let l2_weight = pad32(l2) * l3;
    let output_bias = 4;
    let output_weight = l3;

    (ft_bias + ft_weight + l1_bias + l1_weight + l2_bias + l2_weight + output_bias + output_weight)
        as u64
}

/// アーキテクチャ検出結果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchDetectionResult {
    /// 検出されたアーキテクチャ仕様
    pub spec: ArchitectureSpec,
    /// hash が含まれているか (true = +8B)
    pub has_hash: bool,
}

/// サポートされているアーキテクチャの network_payload テーブル
///
/// (FeatureSet, L1, L2, L3, network_payload)
/// 活性化関数はファイルサイズに影響しないため、ここでは CReLU を仮定。
/// 実際の活性化関数はヘッダーから判定する。
const KNOWN_PAYLOADS: &[(FeatureSet, usize, usize, usize, u64)] = &[
    // HalfKP
    (FeatureSet::HalfKP, 256, 32, 32, network_payload_halfkp(256, 32, 32)),
    (FeatureSet::HalfKP, 512, 8, 96, network_payload_halfkp(512, 8, 96)),
    (FeatureSet::HalfKP, 512, 32, 32, network_payload_halfkp(512, 32, 32)),
    (FeatureSet::HalfKP, 768, 16, 64, network_payload_halfkp(768, 16, 64)),
    (FeatureSet::HalfKP, 1024, 8, 32, network_payload_halfkp(1024, 8, 32)),
    (FeatureSet::HalfKP, 1024, 8, 96, network_payload_halfkp(1024, 8, 96)),
    // HalfKA_hm
    (FeatureSet::HalfKA_hm, 256, 32, 32, network_payload_halfka_hm(256, 32, 32)),
    (FeatureSet::HalfKA_hm, 256, 8, 96, network_payload_halfka_hm(256, 8, 96)),
    (FeatureSet::HalfKA_hm, 512, 8, 96, network_payload_halfka_hm(512, 8, 96)),
    (FeatureSet::HalfKA_hm, 512, 32, 32, network_payload_halfka_hm(512, 32, 32)),
    (FeatureSet::HalfKA_hm, 1024, 8, 96, network_payload_halfka_hm(1024, 8, 96)),
    (FeatureSet::HalfKA_hm, 1024, 8, 32, network_payload_halfka_hm(1024, 8, 32)),
    // HalfKA
    (FeatureSet::HalfKA, 256, 32, 32, network_payload_halfka(256, 32, 32)),
    (FeatureSet::HalfKA, 256, 8, 96, network_payload_halfka(256, 8, 96)),
    (FeatureSet::HalfKA, 512, 8, 96, network_payload_halfka(512, 8, 96)),
    (FeatureSet::HalfKA, 512, 32, 32, network_payload_halfka(512, 32, 32)),
    (FeatureSet::HalfKA, 1024, 8, 96, network_payload_halfka(1024, 8, 96)),
    (FeatureSet::HalfKA, 1024, 8, 32, network_payload_halfka(1024, 8, 32)),
];

/// ファイルサイズと arch_len からアーキテクチャを検出
///
/// # 引数
/// - `file_size`: ファイル全体のサイズ
/// - `arch_len`: ヘッダーの description 文字列長
/// - `feature_set_hint`: ヘッダーから判明している FeatureSet（絞り込みに使用）
///
/// # 戻り値
/// - `Some(ArchDetectionResult)`: 一致するアーキテクチャが見つかった
/// - `None`: 一致なし（未知のアーキテクチャ）
///
/// # 判定ロジック
/// ```text
/// base = file_size - 12 - arch_len
/// base == expected_payload     → hash無し
/// base == expected_payload + 8 → hash有り
/// ```
pub fn detect_architecture_from_size(
    file_size: u64,
    arch_len: usize,
    feature_set_hint: Option<FeatureSet>,
) -> Option<ArchDetectionResult> {
    // base = file_size - header (12 + arch_len)
    let header_size = 12 + arch_len as u64;
    if file_size < header_size {
        return None;
    }
    let base = file_size - header_size;

    for &(feature_set, l1, l2, l3, expected_payload) in KNOWN_PAYLOADS {
        // FeatureSet でフィルタリング（ヒントがある場合）
        if let Some(hint) = feature_set_hint {
            if feature_set != hint {
                continue;
            }
        }

        // hash無しでチェック
        if base == expected_payload {
            return Some(ArchDetectionResult {
                spec: ArchitectureSpec::new(feature_set, l1, l2, l3, Activation::CReLU),
                has_hash: false,
            });
        }

        // hash有り (+8B) でチェック
        if base == expected_payload + 8 {
            return Some(ArchDetectionResult {
                spec: ArchitectureSpec::new(feature_set, l1, l2, l3, Activation::CReLU),
                has_hash: true,
            });
        }
    }

    None
}

/// 検出されたアーキテクチャの一覧を取得（デバッグ用）
///
/// ファイルサイズが近いアーキテクチャの候補を返す。
pub fn list_candidate_architectures(
    file_size: u64,
    arch_len: usize,
) -> Vec<(ArchitectureSpec, i64)> {
    let header_size = 12 + arch_len as u64;
    let base = if file_size >= header_size {
        file_size - header_size
    } else {
        return vec![];
    };

    let mut candidates: Vec<(ArchitectureSpec, i64)> = KNOWN_PAYLOADS
        .iter()
        .flat_map(|&(feature_set, l1, l2, l3, expected_payload)| {
            let spec = ArchitectureSpec::new(feature_set, l1, l2, l3, Activation::CReLU);
            vec![
                (spec, base as i64 - expected_payload as i64), // hash無し
                (spec, base as i64 - (expected_payload + 8) as i64), // hash有り
            ]
        })
        .collect();

    // 差分の絶対値でソート（安定性不要のため unstable 使用）
    candidates.sort_unstable_by_key(|(_, diff)| diff.abs());

    // 上位10件を返す
    candidates.truncate(10);
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_feature_set_display() {
        assert_eq!(FeatureSet::HalfKP.as_str(), "HalfKP");
        assert_eq!(FeatureSet::HalfKA_hm.as_str(), "HalfKA_hm");
        assert_eq!(FeatureSet::HalfKA.as_str(), "HalfKA");
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
    // ファイルサイズベースのアーキテクチャ検出テスト
    // =============================================================================

    #[test]
    fn test_network_payload_halfkp() {
        // nn.bin (HalfKP 768-16-64) の検証
        // file_size = 192,624,720
        // arch_len = 184
        // header = 12 + 184 = 196
        // hash = 8 (FT hash + Network hash)
        // network_payload = 192,624,720 - 196 - 8 = 192,624,516
        let payload = network_payload_halfkp(768, 16, 64);
        assert_eq!(payload, 192_624_516);

        // suisho5.bin (HalfKP 256-32-32) の検証
        // file_size = 64,217,066
        // arch_len = 178
        // header = 12 + 178 = 190
        // hash = 8
        // network_payload = 64,217,066 - 190 - 8 = 64,216,868
        let payload = network_payload_halfkp(256, 32, 32);
        assert_eq!(payload, 64_216_868);
    }

    #[test]
    fn test_network_payload_halfka_hm() {
        // HalfKA_hm 256-32-32 の検証
        let payload = network_payload_halfka_hm(256, 32, 32);
        // ft_bias = 512, ft_weight = 37,532,160, l1_bias = 128, l1_weight = 16,384
        // l2_bias = 128, l2_weight = 1,024, output_bias = 4, output_weight = 32
        // total = 37,550,372
        assert_eq!(payload, 37_550_372);

        // HalfKA_hm 512-8-96 の検証
        let payload = network_payload_halfka_hm(512, 8, 96);
        // ft_bias = 1024, ft_weight = 75,064,320
        // l1_bias = 32, l1_weight = 8,192, l2_bias = 384, l2_weight = 3,072
        // output_bias = 4, output_weight = 96
        // total = 75,077,124
        assert_eq!(payload, 75_077_124);
    }

    #[test]
    fn test_detect_architecture_from_size_nn_bin() {
        // nn.bin (HalfKP 768-16-64, hash有り)
        // file_size = 192,624,720, arch_len = 184
        let result = detect_architecture_from_size(192_624_720, 184, Some(FeatureSet::HalfKP));
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.spec.feature_set, FeatureSet::HalfKP);
        assert_eq!(result.spec.l1, 768);
        assert_eq!(result.spec.l2, 16);
        assert_eq!(result.spec.l3, 64);
        assert!(result.has_hash);
    }

    #[test]
    fn test_detect_architecture_from_size_suisho5() {
        // suisho5.bin (HalfKP 256-32-32, hash有り)
        // file_size = 64,217,066, arch_len = 178
        let result = detect_architecture_from_size(64_217_066, 178, Some(FeatureSet::HalfKP));
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.spec.feature_set, FeatureSet::HalfKP);
        assert_eq!(result.spec.l1, 256);
        assert_eq!(result.spec.l2, 32);
        assert_eq!(result.spec.l3, 32);
        assert!(result.has_hash);
    }

    #[test]
    fn test_detect_architecture_from_size_no_hint() {
        // ヒントなしでも検出可能
        let result = detect_architecture_from_size(192_624_720, 184, None);
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.spec.l1, 768);
        assert_eq!(result.spec.l2, 16);
        assert_eq!(result.spec.l3, 64);
    }

    #[test]
    fn test_detect_architecture_from_size_unknown() {
        // 不明なファイルサイズ
        let result = detect_architecture_from_size(12345, 100, None);
        assert!(result.is_none());
    }

    #[test]
    fn test_detect_architecture_hash_without() {
        // hash無しファイルのシミュレーション
        // nn.bin から hash (8B) を引いたサイズ
        // 192,624,720 - 8 = 192,624,712
        let result = detect_architecture_from_size(192_624_712, 184, Some(FeatureSet::HalfKP));
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.spec.l1, 768);
        assert_eq!(result.spec.l2, 16);
        assert_eq!(result.spec.l3, 64);
        assert!(!result.has_hash); // hash無し
    }
}
