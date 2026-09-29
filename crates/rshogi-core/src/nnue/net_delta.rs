//! 学習済み LayerStacks NNUE 係数への整数 delta。

use std::fmt;

/// net 係数を USI option として公開するときの接頭辞。
pub const NET_DELTA_OPTION_PREFIX: &str = "SPSA_NET_";

/// delta を適用できるテンソル種別。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NetTensorKind {
    /// bucket ごとの output 層 weight (`i8`)。
    OutputWeight,
    /// bucket ごとの output 層 bias (`i32`)。
    OutputBias,
    /// Feature Transformer bias (`i16`)。
    FtBias,
    /// bucket ごとの第 2 FC 層 weight (`i8`)。
    L2Weight,
}

impl NetTensorKind {
    /// USI option 名で使う token を返す。
    pub const fn token(self) -> &'static str {
        match self {
            Self::OutputWeight => "out_w",
            Self::OutputBias => "out_b",
            Self::FtBias => "ft_b",
            Self::L2Weight => "l2_w",
        }
    }
}

/// net 内の係数を一意に表す ID。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NetCoefficientId {
    /// テンソル種別。
    pub kind: NetTensorKind,
    /// bucket を持つテンソルの bucket index。
    pub bucket: Option<usize>,
    /// `.bin` ファイル格納順の flat index。
    pub index: usize,
}

impl NetCoefficientId {
    /// `SPSA_NET_*` USI option 名を parse する。
    pub fn parse_usi_name(name: &str) -> Option<Self> {
        let body = name.strip_prefix(NET_DELTA_OPTION_PREFIX)?;
        if let Some(index) = body.strip_prefix("ft_b_").and_then(parse_usize) {
            return Some(Self {
                kind: NetTensorKind::FtBias,
                bucket: None,
                index,
            });
        }

        for (prefix, kind) in [
            ("out_w_b", NetTensorKind::OutputWeight),
            ("out_b_b", NetTensorKind::OutputBias),
            ("l2_w_b", NetTensorKind::L2Weight),
        ] {
            let Some(indices) = body.strip_prefix(prefix) else {
                continue;
            };
            let (bucket, index) = indices.split_once('_')?;
            let bucket = parse_usize(bucket)?;
            let index = parse_usize(index)?;
            if kind == NetTensorKind::OutputBias && index != 0 {
                return None;
            }
            return Some(Self {
                kind,
                bucket: Some(bucket),
                index,
            });
        }
        None
    }

    /// canonical な `SPSA_NET_*` USI option 名を返す。
    pub fn usi_name(&self) -> String {
        match self.bucket {
            Some(bucket) => {
                format!("{NET_DELTA_OPTION_PREFIX}{}_b{bucket}_{}", self.kind.token(), self.index)
            }
            None => format!("{NET_DELTA_OPTION_PREFIX}{}_{}", self.kind.token(), self.index),
        }
    }
}

fn parse_usize(value: &str) -> Option<usize> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

/// 1 係数へ加える整数 delta。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetDelta {
    /// 対象係数。
    pub id: NetCoefficientId,
    /// 現在値へ加える値。
    pub delta: i32,
}

/// 1 種類のテンソル形状。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetTensorShape {
    /// bucket を持つ場合の bucket 数。FT bias は `None`。
    pub bucket_count: Option<usize>,
    /// 1 bucket あたりの要素数。FT bias は配列全体の要素数。
    pub element_count: usize,
}

impl NetTensorShape {
    pub(crate) fn validate(self, id: &NetCoefficientId) -> Result<(), NetDeltaError> {
        let name = id.usi_name();
        match (self.bucket_count, id.bucket) {
            (Some(bucket_count), Some(bucket)) if bucket >= bucket_count => {
                return Err(NetDeltaError::BucketOutOfRange {
                    name,
                    bucket,
                    bucket_count,
                });
            }
            (Some(_), None) => return Err(NetDeltaError::MissingBucket { name }),
            (None, Some(_)) => return Err(NetDeltaError::UnexpectedBucket { name }),
            _ => {}
        }
        if id.index >= self.element_count {
            return Err(NetDeltaError::IndexOutOfRange {
                name,
                index: id.index,
                element_count: self.element_count,
            });
        }
        Ok(())
    }
}

/// net delta 適用結果。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct NetDeltaReport {
    /// 適用した delta 数。
    pub applied: usize,
    /// 要素型の範囲へ clamp された delta 数。
    pub clamped: usize,
}

/// net delta の検証・適用エラー。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetDeltaError {
    /// 探索などがネットワークを共有中のため変更できない。
    SharedNetwork,
    /// LayerStacks 以外の architecture が指定された。
    UnsupportedArchitecture {
        /// 読み込まれている architecture 名。
        architecture: String,
    },
    /// `.bin` の内容が、解析済み layout と整合しない。
    InvalidBinary {
        /// 不整合の説明。
        message: String,
    },
    /// bucket 必須の kind に bucket が無い。
    MissingBucket {
        /// 問題の係数名。
        name: String,
    },
    /// bucket を持たない kind に bucket が指定された。
    UnexpectedBucket {
        /// 問題の係数名。
        name: String,
    },
    /// bucket index が範囲外。
    BucketOutOfRange {
        /// 問題の係数名。
        name: String,
        /// 指定された bucket index。
        bucket: usize,
        /// 読み込まれた net の bucket 数。
        bucket_count: usize,
    },
    /// flat index が範囲外。
    IndexOutOfRange {
        /// 問題の係数名。
        name: String,
        /// 指定された flat index。
        index: usize,
        /// 対象テンソルの要素数。
        element_count: usize,
    },
}

impl fmt::Display for NetDeltaError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedArchitecture { architecture } => {
                write!(formatter, "unsupported architecture \"{architecture}\"")
            }
            Self::SharedNetwork => {
                write!(formatter, "NNUE network is shared; reload before applying deltas")
            }
            Self::InvalidBinary { message } => write!(formatter, "invalid NNUE binary: {message}"),
            Self::MissingBucket { name } => write!(formatter, "{name}: bucket is required"),
            Self::UnexpectedBucket { name } => {
                write!(formatter, "{name}: bucket is not allowed")
            }
            Self::BucketOutOfRange {
                name,
                bucket,
                bucket_count,
            } => write!(
                formatter,
                "{name}: bucket {bucket} is out of range (bucket count: {bucket_count})"
            ),
            Self::IndexOutOfRange {
                name,
                index,
                element_count,
            } => write!(
                formatter,
                "{name}: index {index} is out of range (element count: {element_count})"
            ),
        }
    }
}

impl std::error::Error for NetDeltaError {}

/// `i8` 係数へ整数 delta を saturating 加算する。
pub fn add_i8_delta(value: i8, delta: i32) -> (i8, bool) {
    let sum = i64::from(value) + i64::from(delta);
    let clamped = sum.clamp(i64::from(i8::MIN), i64::from(i8::MAX));
    (clamped as i8, sum != clamped)
}

/// `i16` 係数へ整数 delta を saturating 加算する。
pub fn add_i16_delta(value: i16, delta: i32) -> (i16, bool) {
    let sum = i64::from(value) + i64::from(delta);
    let clamped = sum.clamp(i64::from(i16::MIN), i64::from(i16::MAX));
    (clamped as i16, sum != clamped)
}

/// `i32` 係数へ整数 delta を saturating 加算する。
pub fn add_i32_delta(value: i32, delta: i32) -> (i32, bool) {
    let sum = i64::from(value) + i64::from(delta);
    let clamped = sum.clamp(i64::from(i32::MIN), i64::from(i32::MAX));
    (clamped as i32, sum != clamped)
}

#[doc(hidden)]
/// crate 間の `.bin` 整合性テストで共有する合成 LayerStacks builder。
pub mod test_utils {
    use crate::nnue::constants::NNUE_VERSION_LAYERSTACK_NUM_BUCKETS;
    use crate::nnue::layers::padded_input;
    use crate::nnue::leb128::LEB128_MAGIC;

    /// 合成 `.bin` 内の 1 bucket の編集対象 offset。
    pub struct SyntheticBucketOffsets {
        /// L2 weights の先頭 offset。
        pub l2_weights: usize,
        /// output bias の先頭 offset。
        pub output_bias: usize,
        /// output weights の先頭 offset。
        pub output_weights: usize,
    }

    /// 決定的に生成した合成 LayerStacks `.bin` と主要 offset。
    pub struct SyntheticLayerStacksBin {
        /// `.bin` 全体。
        pub bytes: Vec<u8>,
        /// FT biases の先頭 offset。
        pub ft_biases: usize,
        /// bucket ごとの主要 offset。
        pub buckets: Vec<SyntheticBucketOffsets>,
    }

    #[derive(Debug, Clone, Copy)]
    /// 合成 FT の符号化方式。
    pub enum SyntheticFtEncoding {
        /// biases と weights を連結した LEB128。
        Leb128Combined,
        /// biases と weights を分けた LEB128。
        Leb128Split,
    }

    #[derive(Debug, Clone, Copy)]
    /// 合成 FT 係数の値域。
    pub enum SyntheticFtValues {
        /// 1 byte signed LEB128 に収まる正値。
        SingleBytePositive,
        /// 負数、多 byte 値、`i16` 境界値を含む。
        SignedBoundaries,
    }

    #[derive(Debug, Clone, Copy)]
    /// 合成 FT の符号化方式と係数値域。
    pub struct SyntheticFtConfig {
        /// LEB128 ブロック構成。
        pub encoding: SyntheticFtEncoding,
        /// 係数の値域。
        pub values: SyntheticFtValues,
    }

    struct Lcg(u32);

    impl Lcg {
        fn next(&mut self) -> u32 {
            self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            self.0
        }

        fn range(&mut self, min: i32, max: i32) -> i32 {
            min + (self.next() % ((max - min + 1) as u32)) as i32
        }
    }

    fn append_affine(
        bytes: &mut Vec<u8>,
        input_dim: usize,
        output_dim: usize,
        rng: &mut Lcg,
        bias_range: std::ops::RangeInclusive<i32>,
        weight_max: i32,
    ) -> usize {
        for _ in 0..output_dim {
            let bias = rng.range(*bias_range.start(), *bias_range.end());
            bytes.extend_from_slice(&bias.to_le_bytes());
        }
        let weights = bytes.len();
        for _ in 0..output_dim * padded_input(input_dim) {
            bytes.push(rng.range(1, weight_max) as u8);
        }
        weights
    }

    /// LEB128 1 ブロック形式の合成 LayerStacks `.bin` を生成する。
    pub fn build_synthetic_layer_stacks(
        feature_name: &str,
        input_dimensions: usize,
        l1: usize,
        l2: usize,
        l3: usize,
        num_buckets: usize,
    ) -> SyntheticLayerStacksBin {
        build_synthetic_layer_stacks_with_ft_encoding(
            feature_name,
            input_dimensions,
            l1,
            l2,
            l3,
            num_buckets,
            SyntheticFtEncoding::Leb128Combined,
        )
    }

    /// 指定 FT 符号化の合成 LayerStacks `.bin` を生成する。
    pub fn build_synthetic_layer_stacks_with_ft_encoding(
        feature_name: &str,
        input_dimensions: usize,
        l1: usize,
        l2: usize,
        l3: usize,
        num_buckets: usize,
        ft_encoding: SyntheticFtEncoding,
    ) -> SyntheticLayerStacksBin {
        build_synthetic_layer_stacks_with_ft_values(
            feature_name,
            input_dimensions,
            l1,
            l2,
            l3,
            num_buckets,
            SyntheticFtConfig {
                encoding: ft_encoding,
                values: SyntheticFtValues::SingleBytePositive,
            },
        )
    }

    /// 指定 FT 符号化と値域の合成 LayerStacks `.bin` を生成する。
    pub fn build_synthetic_layer_stacks_with_ft_values(
        feature_name: &str,
        input_dimensions: usize,
        l1: usize,
        l2: usize,
        l3: usize,
        num_buckets: usize,
        ft: SyntheticFtConfig,
    ) -> SyntheticLayerStacksBin {
        let arch = format!(
            "Features={feature_name}[{input_dimensions}->{l1}x2],LayerStacks,l2={l2},l3={l3}"
        );
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&NNUE_VERSION_LAYERSTACK_NUM_BUCKETS.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&(arch.len() as u32).to_le_bytes());
        bytes.extend_from_slice(arch.as_bytes());
        bytes.extend_from_slice(&(num_buckets as u32).to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());

        let mut rng = Lcg(0x5eed_1234);
        let ft_element_count = l1 + input_dimensions * l1;
        let signed_boundaries = [-1, 64, -65, -8192, 8191, i16::MIN, i16::MAX];
        let mut ft_value = |index: usize, bias: bool| match ft.values {
            SyntheticFtValues::SingleBytePositive if bias => rng.range(8, 15) as i16,
            SyntheticFtValues::SingleBytePositive => rng.range(1, 2) as i16,
            SyntheticFtValues::SignedBoundaries => {
                signed_boundaries[index % signed_boundaries.len()]
            }
        };
        let ft_bias_values: Vec<_> = (0..l1).map(|index| ft_value(index, true)).collect();
        let ft_weight_values: Vec<_> =
            (l1..ft_element_count).map(|index| ft_value(index, false)).collect();
        let ft_biases = match ft.encoding {
            SyntheticFtEncoding::Leb128Combined => {
                bytes.extend_from_slice(LEB128_MAGIC);
                let size_offset = bytes.len();
                bytes.extend_from_slice(&0u32.to_le_bytes());
                let offset = bytes.len();
                for &value in ft_bias_values.iter().chain(&ft_weight_values) {
                    super::super::leb128::encode_signed_leb128(i64::from(value), &mut bytes);
                }
                let size = u32::try_from(bytes.len() - offset).expect("synthetic FT size");
                bytes[size_offset..size_offset + 4].copy_from_slice(&size.to_le_bytes());
                offset
            }
            SyntheticFtEncoding::Leb128Split => {
                bytes.extend_from_slice(LEB128_MAGIC);
                let bias_size_offset = bytes.len();
                bytes.extend_from_slice(&0u32.to_le_bytes());
                let offset = bytes.len();
                for &value in &ft_bias_values {
                    super::super::leb128::encode_signed_leb128(i64::from(value), &mut bytes);
                }
                let bias_size =
                    u32::try_from(bytes.len() - offset).expect("synthetic FT bias size");
                bytes[bias_size_offset..bias_size_offset + 4]
                    .copy_from_slice(&bias_size.to_le_bytes());
                bytes.extend_from_slice(LEB128_MAGIC);
                let weight_size_offset = bytes.len();
                bytes.extend_from_slice(&0u32.to_le_bytes());
                let weight_offset = bytes.len();
                for &value in &ft_weight_values {
                    super::super::leb128::encode_signed_leb128(i64::from(value), &mut bytes);
                }
                let weight_size =
                    u32::try_from(bytes.len() - weight_offset).expect("synthetic FT weight size");
                bytes[weight_size_offset..weight_size_offset + 4]
                    .copy_from_slice(&weight_size.to_le_bytes());
                offset
            }
        };

        let mut buckets = Vec::with_capacity(num_buckets);
        for _ in 0..num_buckets {
            bytes.extend_from_slice(&0u32.to_le_bytes());
            append_affine(&mut bytes, l1, l2, &mut rng, 64..=127, 2);
            let l2_weights = append_affine(&mut bytes, 2 * (l2 - 1), l3, &mut rng, 128..=255, 2);
            let output_bias = bytes.len();
            let output_weights = append_affine(&mut bytes, l3, 1, &mut rng, 128..=255, 8);
            buckets.push(SyntheticBucketOffsets {
                l2_weights,
                output_bias,
                output_weights,
            });
        }
        SyntheticLayerStacksBin {
            bytes,
            ft_biases,
            buckets,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_deltas_clamp_at_storage_bounds() {
        assert_eq!(add_i8_delta(126, 1), (127, false));
        assert_eq!(add_i8_delta(127, 1), (127, true));
        assert_eq!(add_i16_delta(i16::MIN + 1, -1), (i16::MIN, false));
        assert_eq!(add_i16_delta(i16::MIN, -1), (i16::MIN, true));
        assert_eq!(add_i32_delta(i32::MAX - 1, 1), (i32::MAX, false));
        assert_eq!(add_i32_delta(i32::MAX, 1), (i32::MAX, true));
    }

    #[test]
    fn tensor_shape_validates_bucket_and_index() {
        let shape = NetTensorShape {
            bucket_count: Some(2),
            element_count: 3,
        };
        let valid = NetCoefficientId {
            kind: NetTensorKind::OutputWeight,
            bucket: Some(1),
            index: 2,
        };
        assert_eq!(shape.validate(&valid), Ok(()));

        let mut invalid = valid.clone();
        invalid.bucket = None;
        assert!(matches!(shape.validate(&invalid), Err(NetDeltaError::MissingBucket { .. })));
        invalid.bucket = Some(2);
        assert!(matches!(shape.validate(&invalid), Err(NetDeltaError::BucketOutOfRange { .. })));
        invalid.bucket = Some(1);
        invalid.index = 3;
        assert!(matches!(shape.validate(&invalid), Err(NetDeltaError::IndexOutOfRange { .. })));
    }

    #[test]
    fn usi_name_round_trip() {
        for name in [
            "SPSA_NET_out_w_b3_17",
            "SPSA_NET_out_b_b0_0",
            "SPSA_NET_ft_b_1023",
            "SPSA_NET_l2_w_b2_45",
        ] {
            let id = NetCoefficientId::parse_usi_name(name).expect("valid name");
            assert_eq!(id.usi_name(), name);
        }
    }

    #[test]
    fn usi_name_rejects_invalid_forms() {
        for name in [
            "SPSA_out_w_b3_17",
            "SPSA_NET_out_w_17",
            "SPSA_NET_ft_b_b0_17",
            "SPSA_NET_ft_b_",
            "SPSA_NET_ft_b_-1",
            "SPSA_NET_out_w_bx_17",
            "SPSA_NET_out_w_b3_x",
            "SPSA_NET_out_w_b3_17_extra",
            "SPSA_NET_out_b_b0_1",
            "SPSA_NET_unknown_b0_0",
        ] {
            assert!(NetCoefficientId::parse_usi_name(name).is_none(), "{name}");
        }
    }
}
