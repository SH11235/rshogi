//! LSNN ファイル I/O
//!
//! LayerStack NNUE の独自ファイルフォーマット読み込み。
//! nnue-pytorch / YaneuraOu 形式とは非互換。

use super::bucket::BucketDivision;
use super::constants::*;
use super::weights::{
    FtWeights, L1WeightsBucket, L2WeightsBucket, LayerStackWeights, OutWeightsBucket,
};
use std::io::{self, Read};

/// LSNN ファイルマジックナンバー
pub const LSNN_MAGIC: [u8; 4] = *b"LSNN";

/// LSNN ファイルバージョン
pub const LSNN_VERSION: u32 = 1;

/// LSNN ヘッダ（32 bytes）
///
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct LsnnHeader {
    /// マジックナンバー "LSNN"
    pub magic: [u8; 4],

    /// ファイルバージョン（1）
    pub version: u32,

    /// Feature Transformer 出力次元（1536）
    pub ft_out: u32,

    /// L1 出力次元（16）
    pub l1_out: u32,

    /// L2 出力次元（64）
    pub l2_out: u32,

    /// バケット数（4 or 9）
    pub num_buckets: u32,

    /// バケット分割方式（0=TwoByTwo, 1=ThreeByThree）
    pub bucket_division: u32,

    /// bypass 使用フラグ（0 or 1）
    pub use_bypass: u32,
}

impl LsnnHeader {
    /// ヘッダーサイズ（bytes）
    pub const SIZE: usize = 32;

    /// バイト列から読み込み
    pub fn from_bytes(bytes: &[u8; Self::SIZE]) -> io::Result<Self> {
        let magic = [bytes[0], bytes[1], bytes[2], bytes[3]];
        if magic != LSNN_MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Invalid LSNN magic: {:?}", magic),
            ));
        }

        let version = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        if version != LSNN_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Unsupported LSNN version: {version}"),
            ));
        }

        let ft_out = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
        let l1_out = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
        let l2_out = u32::from_le_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
        let num_buckets = u32::from_le_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
        let bucket_division = u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]);
        let use_bypass = u32::from_le_bytes([bytes[28], bytes[29], bytes[30], bytes[31]]);

        // 次元の検証
        if ft_out as usize != FT_PER_PERSPECTIVE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Invalid ft_out: {ft_out}, expected {FT_PER_PERSPECTIVE}"),
            ));
        }
        if l1_out as usize != L1_OUT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Invalid l1_out: {l1_out}, expected {L1_OUT}"),
            ));
        }
        if l2_out as usize != L2_OUT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Invalid l2_out: {l2_out}, expected {L2_OUT}"),
            ));
        }

        // バケット数の検証
        if num_buckets != 4 && num_buckets != 9 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Invalid num_buckets: {num_buckets}, expected 4 or 9"),
            ));
        }

        // バケット分割方式の検証
        if bucket_division > 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Invalid bucket_division: {bucket_division}"),
            ));
        }

        // バケット数と分割方式の整合性
        let expected_buckets = if bucket_division == 0 { 4 } else { 9 };
        if num_buckets != expected_buckets {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Bucket count mismatch: num_buckets={num_buckets}, bucket_division={bucket_division}"
                ),
            ));
        }

        Ok(Self {
            magic,
            version,
            ft_out,
            l1_out,
            l2_out,
            num_buckets,
            bucket_division,
            use_bypass,
        })
    }

    /// バケット分割方式を取得
    pub fn get_bucket_division(&self) -> BucketDivision {
        if self.bucket_division == 0 {
            BucketDivision::TwoByTwo
        } else {
            BucketDivision::ThreeByThree
        }
    }

    /// bypass 使用フラグを取得
    pub fn get_use_bypass(&self) -> bool {
        self.use_bypass != 0
    }
}

/// LSNN ファイルを読み込み
///
/// # 引数
///
/// - `reader`: バイト入力ストリーム
///
/// # 戻り値
///
/// 読み込んだ重み構造体
pub fn read_lsnn<R: Read>(reader: &mut R) -> io::Result<LayerStackWeights> {
    // ヘッダー読み込み
    let mut header_bytes = [0u8; LsnnHeader::SIZE];
    reader.read_exact(&mut header_bytes)?;
    let header = LsnnHeader::from_bytes(&header_bytes)?;

    let bucket_division = header.get_bucket_division();
    let use_bypass = header.get_use_bypass();
    let num_buckets = header.num_buckets as usize;

    let mut weights = LayerStackWeights::new(bucket_division, use_bypass);

    // Feature Transformer 重み読み込み（Bias-first, nnue-pytorch-nodchip 互換）
    read_ft_weights_bias_first(reader, &mut weights.ft)?;

    // LayerStacks 重み読み込み（バケットごと、Bias-first）
    for bucket in 0..num_buckets {
        read_l1_weights_bias_first(reader, &mut weights.l1[bucket])?;
        read_l2_weights_bias_first(reader, &mut weights.l2[bucket])?;
        read_out_weights_bias_first(reader, &mut weights.out[bucket])?;
    }

    Ok(weights)
}

/// Feature Transformer 重みを読み込み（Bias-first、nnue-pytorch-nodchip 互換）
fn read_ft_weights_bias_first<R: Read>(reader: &mut R, ft: &mut FtWeights) -> io::Result<()> {
    // ft_bias: i16[1536]（先に読み込み）
    let bias_bytes = FT_PER_PERSPECTIVE * 2;
    let mut buf = vec![0u8; bias_bytes];
    reader.read_exact(&mut buf)?;

    for (i, chunk) in buf.chunks_exact(2).enumerate() {
        ft.bias[i] = i16::from_le_bytes([chunk[0], chunk[1]]);
    }

    // ft_weight: i16[HALFKA_FEATURES][1536]（row-major）
    let weight_bytes = HALFKA_FEATURES * FT_PER_PERSPECTIVE * 2;
    let mut buf = vec![0u8; weight_bytes];
    reader.read_exact(&mut buf)?;

    for (i, chunk) in buf.chunks_exact(2).enumerate() {
        ft.weight[i] = i16::from_le_bytes([chunk[0], chunk[1]]);
    }

    Ok(())
}

/// L1 層重みを読み込み（Bias-first、nnue-pytorch-nodchip 互換）
fn read_l1_weights_bias_first<R: Read>(reader: &mut R, l1: &mut L1WeightsBucket) -> io::Result<()> {
    // l1_bias: i32[16]（先に読み込み）
    let bias_bytes = L1_OUT * 4;
    let mut buf = vec![0u8; bias_bytes];
    reader.read_exact(&mut buf)?;

    for (i, chunk) in buf.chunks_exact(4).enumerate() {
        l1.bias[i] = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
    }

    // l1_weight: i8[16][1536]（row-major）
    let weight_bytes = L1_OUT * L1_IN;
    let mut buf = vec![0u8; weight_bytes];
    reader.read_exact(&mut buf)?;

    for (i, &b) in buf.iter().enumerate() {
        l1.weight[i] = b as i8;
    }

    Ok(())
}

/// L2 層重みを読み込み（Bias-first、nnue-pytorch-nodchip 互換）
fn read_l2_weights_bias_first<R: Read>(reader: &mut R, l2: &mut L2WeightsBucket) -> io::Result<()> {
    // l2_bias: i32[64]（先に読み込み）
    let bias_bytes = L2_OUT * 4;
    let mut buf = vec![0u8; bias_bytes];
    reader.read_exact(&mut buf)?;

    for (i, chunk) in buf.chunks_exact(4).enumerate() {
        l2.bias[i] = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
    }

    // l2_weight: i8[64][30]（row-major）
    let weight_bytes = L2_OUT * DUAL_ACT_OUT;
    let mut buf = vec![0u8; weight_bytes];
    reader.read_exact(&mut buf)?;

    for (i, &b) in buf.iter().enumerate() {
        l2.weight[i] = b as i8;
    }

    Ok(())
}

/// Output 層重みを読み込み（Bias-first、nnue-pytorch-nodchip 互換）
fn read_out_weights_bias_first<R: Read>(
    reader: &mut R,
    out: &mut OutWeightsBucket,
) -> io::Result<()> {
    // out_bias: i32（先に読み込み）
    let mut buf = [0u8; 4];
    reader.read_exact(&mut buf)?;
    out.bias = i32::from_le_bytes(buf);

    // out_weight: i8[64]
    let weight_bytes = L2_OUT;
    let mut buf = vec![0u8; weight_bytes];
    reader.read_exact(&mut buf)?;

    for (i, &b) in buf.iter().enumerate() {
        out.weight[i] = b as i8;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn create_valid_header(num_buckets: u32, bucket_division: u32, use_bypass: u32) -> [u8; 32] {
        let mut header = [0u8; 32];

        // magic
        header[0..4].copy_from_slice(b"LSNN");

        // version = 1
        header[4..8].copy_from_slice(&1u32.to_le_bytes());

        // ft_out = 1536
        header[8..12].copy_from_slice(&1536u32.to_le_bytes());

        // l1_out = 16
        header[12..16].copy_from_slice(&16u32.to_le_bytes());

        // l2_out = 64
        header[16..20].copy_from_slice(&64u32.to_le_bytes());

        // num_buckets
        header[20..24].copy_from_slice(&num_buckets.to_le_bytes());

        // bucket_division
        header[24..28].copy_from_slice(&bucket_division.to_le_bytes());

        // use_bypass
        header[28..32].copy_from_slice(&use_bypass.to_le_bytes());

        header
    }

    #[test]
    fn test_header_parse_2x2() {
        let header_bytes = create_valid_header(4, 0, 0);
        let header = LsnnHeader::from_bytes(&header_bytes).unwrap();

        assert_eq!(header.magic, *b"LSNN");
        assert_eq!(header.version, 1);
        assert_eq!(header.ft_out, 1536);
        assert_eq!(header.l1_out, 16);
        assert_eq!(header.l2_out, 64);
        assert_eq!(header.num_buckets, 4);
        assert_eq!(header.bucket_division, 0);
        assert_eq!(header.use_bypass, 0);
        assert_eq!(header.get_bucket_division(), BucketDivision::TwoByTwo);
        assert!(!header.get_use_bypass());
    }

    #[test]
    fn test_header_parse_3x3() {
        let header_bytes = create_valid_header(9, 1, 1);
        let header = LsnnHeader::from_bytes(&header_bytes).unwrap();

        assert_eq!(header.num_buckets, 9);
        assert_eq!(header.bucket_division, 1);
        assert_eq!(header.use_bypass, 1);
        assert_eq!(header.get_bucket_division(), BucketDivision::ThreeByThree);
        assert!(header.get_use_bypass());
    }

    #[test]
    fn test_header_invalid_magic() {
        let mut header_bytes = create_valid_header(4, 0, 0);
        header_bytes[0] = b'X';

        let result = LsnnHeader::from_bytes(&header_bytes);
        assert!(result.is_err());
    }

    #[test]
    fn test_header_invalid_version() {
        let mut header_bytes = create_valid_header(4, 0, 0);
        header_bytes[4..8].copy_from_slice(&2u32.to_le_bytes());

        let result = LsnnHeader::from_bytes(&header_bytes);
        assert!(result.is_err());
    }

    #[test]
    fn test_header_bucket_mismatch() {
        // num_buckets=4 but bucket_division=1 (ThreeByThree, expects 9)
        let header_bytes = create_valid_header(4, 1, 0);
        let result = LsnnHeader::from_bytes(&header_bytes);
        assert!(result.is_err());
    }

    #[test]
    fn test_read_lsnn_empty_weights() {
        // 最小限の LSNN ファイルをシミュレート（ゼロ重み）
        let header_bytes = create_valid_header(4, 0, 0);

        // FT: weight + bias
        let ft_weight_size = HALFKA_FEATURES * FT_PER_PERSPECTIVE * 2;
        let ft_bias_size = FT_PER_PERSPECTIVE * 2;

        // LayerStack per bucket: l1 + l2 + out
        let l1_weight_size = L1_OUT * L1_IN;
        let l1_bias_size = L1_OUT * 4;
        let l2_weight_size = L2_OUT * DUAL_ACT_OUT;
        let l2_bias_size = L2_OUT * 4;
        let out_weight_size = L2_OUT;
        let out_bias_size = 4;

        let bucket_size = l1_weight_size
            + l1_bias_size
            + l2_weight_size
            + l2_bias_size
            + out_weight_size
            + out_bias_size;

        let total_size = 32 + ft_weight_size + ft_bias_size + bucket_size * 4;

        let mut data = vec![0u8; total_size];
        data[0..32].copy_from_slice(&header_bytes);

        let mut cursor = Cursor::new(data);
        let weights = read_lsnn(&mut cursor).unwrap();

        assert_eq!(weights.bucket_division, BucketDivision::TwoByTwo);
        assert!(!weights.use_bypass);
        assert_eq!(weights.num_buckets(), 4);
    }
}
