//! NNUE weight file management
//!
//! Handles loading and parsing of NNUE weight files

use super::features::{FeatureTransformer, FE_END};
use super::network::Network;
use crate::shogi::SHOGI_BOARD_SIZE;
use std::fs::File;
use std::io::Read;
use std::mem;

#[cfg(debug_assertions)]
use log::debug;

/// NNUE file header
#[derive(Debug, Clone, Copy)]
pub struct NNUEHeader {
    magic: [u8; 4],    // "NNUE"
    version: u32,      // Version number
    architecture: u32, // Architecture ID
    size: u32,         // File size
}

/// Architecture ID for HalfKP 256x2-32-32
const HALFKP_256X2_32_32: u32 = 0x7AF32F16;
/// Architecture ID for HalfKP x2 dynamic dims (v2)
const HALFKP_X2_DYNAMIC: u32 = 0xD15C_A11C; // made-up stable tag for dynamic HalfKP×2

/// Supported NNUE format versions
const MIN_SUPPORTED_VERSION: u32 = 1;
const MAX_SUPPORTED_VERSION: u32 = 2;

/// Maximum reasonable file size (200MB)
const MAX_FILE_SIZE: u64 = 200 * 1024 * 1024;

/// Upper bounds for v2 dims (sanity checks)
const ACC_DIM_MAX: u32 = 4096;
const H1_DIM_MAX: u32 = 1024;
const H2_DIM_MAX: u32 = 1024;

/// Expected weight sizes for validation
const EXPECTED_FT_WEIGHTS: usize = SHOGI_BOARD_SIZE * FE_END * FeatureTransformer::DEFAULT_DIM; // Feature transformer weights
const EXPECTED_FT_BIASES: usize = FeatureTransformer::DEFAULT_DIM; // Feature transformer biases
const EXPECTED_H1_WEIGHTS: usize = 512 * 32; // Hidden layer 1 weights
const EXPECTED_H1_BIASES: usize = 32; // Hidden layer 1 biases
const EXPECTED_H2_WEIGHTS: usize = 32 * 32; // Hidden layer 2 weights
const EXPECTED_H2_BIASES: usize = 32; // Hidden layer 2 biases
const EXPECTED_OUT_WEIGHTS: usize = 32; // Output layer weights
const EXPECTED_OUT_BIASES: usize = 1; // Output layer bias

/// Weight file reader
pub struct WeightReader {
    file: File,
}

/// Errors that can occur while loading NNUE weight files
#[derive(Debug)]
pub enum WeightsError {
    InvalidMagic,
    UnsupportedVersion { found: u32, min: u32, max: u32 },
    UnsupportedArchitectureV1(u32),
    UnsupportedArchitectureV2(u32),
    SizeTooLarge { size: u64, max: u64 },
    SizeMismatchV1 { expected: u64, actual: u64 },
    SizeMismatchV2 { expected: u64, actual: u64 },
    DimsInvalid,
    DimsInconsistent(&'static str),
    SectionTruncated(&'static str),
    Overflow(&'static str),
    Io(std::io::Error),
}

impl std::fmt::Display for WeightsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WeightsError::InvalidMagic => write!(f, "Invalid NNUE file magic"),
            WeightsError::UnsupportedVersion { found, min, max } => {
                write!(f, "Unsupported NNUE version: {}, supported range: {}-{}", found, min, max)
            }
            WeightsError::UnsupportedArchitectureV1(a) => {
                write!(f, "Unsupported architecture for v1: 0x{a:08X}")
            }
            WeightsError::UnsupportedArchitectureV2(a) => {
                write!(f, "Unsupported architecture for v2: 0x{a:08X}")
            }
            WeightsError::SizeTooLarge { size, max } => {
                write!(f, "NNUE file too large: {} bytes, maximum: {} bytes", size, max)
            }
            WeightsError::SizeMismatchV1 { expected, actual } => {
                write!(f, "SizeMismatch(v1): expected {}, actual {}", expected, actual)
            }
            WeightsError::SizeMismatchV2 { expected, actual } => {
                write!(f, "SizeMismatch(v2): expected {} bytes, actual {} bytes", expected, actual)
            }
            WeightsError::DimsInvalid => write!(f, "DimsInvalid: zero or exceeds maximum"),
            WeightsError::DimsInconsistent(m) => write!(f, "DimsInconsistent: {m}"),
            WeightsError::SectionTruncated(m) => write!(f, "SectionTruncated: {m}"),
            WeightsError::Overflow(m) => write!(f, "Overflow: {m}"),
            WeightsError::Io(e) => write!(f, "IO error: {e}"),
        }
    }
}

impl std::error::Error for WeightsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            WeightsError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for WeightsError {
    fn from(e: std::io::Error) -> Self {
        WeightsError::Io(e)
    }
}

/// Marker trait restricting generic weight-reading helpers to plain-old-data integer types.
///
/// 目的: 重みファイル読み込み時に任意型への誤用（未定義ビットパターンを持つ型 / Drop を伴う型）を防ぎ、
/// `read_exact` で得た生バイト列をそのまま `mem::transmute_copy` 相当の解釈で安全に扱える型に限定する。
///
/// # Safety
/// このトレイトを実装できるのは「全てのビットパターンが有効で、レイアウトが安定し、`Drop`/内部参照/パディング
/// に起因する未定義動作を招かない」POD 整数型のみであることを呼び出し側に保証する必要がある。
/// 具体的には以下を満たすこと:
/// * 任意の 8bit チャンク列をその型として再解釈しても未定義動作にならない（全ビットパターン有効）。
/// * `Copy` であり、`Drop` 実装が無い。
/// * メモリ再解釈時に内部不変条件（例: enum 判別子整合など）を壊さない。
/// * 現状は標準整数型（i8）のみ実装し、外部から追加実装できないよう `pub(crate)` に制限している。
pub(crate) unsafe trait PlainBytes: Copy {}
unsafe impl PlainBytes for i8 {}
// Restrict generic byte reader to i8 only to avoid accidental misuse for LE types

impl WeightReader {
    /// Create reader from file path
    pub fn from_file(path: &str) -> Result<Self, WeightsError> {
        let file = File::open(path)?;
        Ok(WeightReader { file })
    }

    /// Read header and validate
    pub fn read_header(&mut self) -> Result<NNUEHeader, WeightsError> {
        let mut magic = [0u8; 4];
        let mut version = [0u8; 4];
        let mut architecture = [0u8; 4];
        let mut size = [0u8; 4];

        self.file.read_exact(&mut magic)?;
        self.file.read_exact(&mut version)?;
        self.file.read_exact(&mut architecture)?;
        self.file.read_exact(&mut size)?;

        // Validate magic
        if &magic != b"NNUE" {
            return Err(WeightsError::InvalidMagic);
        }

        let header = NNUEHeader {
            magic,
            version: u32::from_le_bytes(version),
            architecture: u32::from_le_bytes(architecture),
            size: u32::from_le_bytes(size),
        };

        // Debug output for header information
        #[cfg(debug_assertions)]
        debug!(
            "NNUE Header: magic={:?}, version={}, size={} bytes",
            std::str::from_utf8(&header.magic).unwrap_or("???"),
            header.version,
            header.size
        );

        // Validate version
        if header.version < MIN_SUPPORTED_VERSION || header.version > MAX_SUPPORTED_VERSION {
            return Err(WeightsError::UnsupportedVersion {
                found: header.version,
                min: MIN_SUPPORTED_VERSION,
                max: MAX_SUPPORTED_VERSION,
            });
        }

        // Validate file size (upper bound only here; exact match is checked in the version branch below)
        if (header.size as u64) > MAX_FILE_SIZE {
            return Err(WeightsError::SizeTooLarge {
                size: header.size as u64,
                max: MAX_FILE_SIZE,
            });
        }

        Ok(header)
    }

    /// Read a little-endian u32 value from the file
    fn read_u32_le(&mut self) -> Result<u32, WeightsError> {
        let mut buf = [0u8; 4];
        self.file.read_exact(&mut buf)?;
        Ok(u32::from_le_bytes(buf))
    }

    /// Read weights of type T (aligned safely). T must be a POD integer type.
    pub(crate) fn read_weights<T: PlainBytes>(
        &mut self,
        count: usize,
    ) -> Result<Vec<T>, WeightsError> {
        use std::mem::MaybeUninit;
        let size = count
            .checked_mul(mem::size_of::<T>())
            .ok_or(WeightsError::Overflow("weight size overflow"))?;

        // Allocate uninitialized buffer for T and read as bytes into it.
        let mut v: Vec<MaybeUninit<T>> = Vec::with_capacity(count);
        let dst_bytes = unsafe { std::slice::from_raw_parts_mut(v.as_mut_ptr() as *mut u8, size) };
        self.file.read_exact(dst_bytes)?;
        unsafe { v.set_len(count) };
        let v: Vec<T> = unsafe { std::mem::transmute(v) };
        Ok(v)
    }

    /// Read `count` little-endian i16 values (portable across host endianness).
    ///
    /// Safety rationale (LE fast-path):
    /// * We allocate a `Vec<MaybeUninit<i16>>` with capacity `count` and obtain a raw byte slice.
    /// * `read_exact` fully initializes all `bytes = count*2` bytes; on success every element is initialized.
    /// * `i16` accepts every 16-bit pattern, so transmuting `Vec<MaybeUninit<i16>>` -> `Vec<i16>` is sound.
    /// * We call `set_len(count)` only after the successful read, so no uninitialized elements are observed.
    #[cfg(target_endian = "little")]
    pub(crate) fn read_i16_le_vec(&mut self, count: usize) -> Result<Vec<i16>, WeightsError> {
        use std::mem::MaybeUninit;
        let bytes = count.checked_mul(2).ok_or(WeightsError::Overflow("weight size overflow"))?;
        let mut v: Vec<MaybeUninit<i16>> = Vec::with_capacity(count);
        let dst = unsafe { std::slice::from_raw_parts_mut(v.as_mut_ptr() as *mut u8, bytes) };
        self.file.read_exact(dst)?;
        unsafe { v.set_len(count) };
        // Safety: fully initialized; i16 is POD and all bit patterns are valid.
        let v: Vec<i16> = unsafe { std::mem::transmute(v) };
        Ok(v)
    }

    /// Read `count` little-endian i16 values (portable across host endianness)
    #[cfg(not(target_endian = "little"))]
    pub(crate) fn read_i16_le_vec(&mut self, count: usize) -> Result<Vec<i16>, WeightsError> {
        let bytes = count.checked_mul(2).ok_or(WeightsError::Overflow("weight size overflow"))?;
        let mut buf = vec![0u8; bytes];
        self.file.read_exact(&mut buf)?;
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            let j = i * 2;
            out.push(i16::from_le_bytes([buf[j], buf[j + 1]]));
        }
        Ok(out)
    }

    /// Read `count` little-endian i32 values (portable across host endianness).
    ///
    /// Safety rationale parallels `read_i16_le_vec` (see above): fully filled buffer + POD i32.
    #[cfg(target_endian = "little")]
    pub(crate) fn read_i32_le_vec(&mut self, count: usize) -> Result<Vec<i32>, WeightsError> {
        use std::mem::MaybeUninit;
        let bytes = count.checked_mul(4).ok_or(WeightsError::Overflow("weight size overflow"))?;
        let mut v: Vec<MaybeUninit<i32>> = Vec::with_capacity(count);
        let dst = unsafe { std::slice::from_raw_parts_mut(v.as_mut_ptr() as *mut u8, bytes) };
        self.file.read_exact(dst)?;
        unsafe { v.set_len(count) };
        // Safety: fully initialized; i32 is POD and all bit patterns valid.
        let v: Vec<i32> = unsafe { std::mem::transmute(v) };
        Ok(v)
    }

    /// Read `count` little-endian i32 values (portable across host endianness)
    #[cfg(not(target_endian = "little"))]
    pub(crate) fn read_i32_le_vec(&mut self, count: usize) -> Result<Vec<i32>, WeightsError> {
        let bytes = count.checked_mul(4).ok_or(WeightsError::Overflow("weight size overflow"))?;
        let mut buf = vec![0u8; bytes];
        self.file.read_exact(&mut buf)?;
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            let j = i * 4;
            out.push(i32::from_le_bytes([buf[j], buf[j + 1], buf[j + 2], buf[j + 3]]));
        }
        Ok(out)
    }
}

/// Load NNUE weights from file
pub fn load_weights(path: &str) -> Result<(FeatureTransformer, Network), WeightsError> {
    let meta_len = std::fs::metadata(path)?.len();
    let mut reader = WeightReader::from_file(path)?;
    let header = reader.read_header()?;

    // Branch by version/architecture
    match header.version {
        1 => {
            // Validate declared size matches actual file size (strict check for v1)
            if meta_len != header.size as u64 {
                return Err(WeightsError::SizeMismatchV1 {
                    expected: header.size as u64,
                    actual: meta_len,
                });
            }
            // Validate architecture for v1
            if header.architecture != HALFKP_256X2_32_32 {
                return Err(WeightsError::UnsupportedArchitectureV1(header.architecture));
            }

            // Strict total-size validation (prevent large allocations on malformed files)
            let mut expect_total: u64 = 16; // header
            expect_total = expect_total
                .checked_add(
                    (EXPECTED_FT_WEIGHTS as u64)
                        .checked_mul(2)
                        .ok_or(WeightsError::Overflow("mul"))?,
                )
                .ok_or(WeightsError::Overflow("add"))?; // i16
            expect_total = expect_total
                .checked_add(
                    (EXPECTED_FT_BIASES as u64)
                        .checked_mul(4)
                        .ok_or(WeightsError::Overflow("mul"))?,
                )
                .ok_or(WeightsError::Overflow("add"))?; // i32
            expect_total = expect_total
                .checked_add(EXPECTED_H1_WEIGHTS as u64)
                .ok_or(WeightsError::Overflow("add"))?; // i8
            expect_total = expect_total
                .checked_add(
                    (EXPECTED_H1_BIASES as u64)
                        .checked_mul(4)
                        .ok_or(WeightsError::Overflow("mul"))?,
                )
                .ok_or(WeightsError::Overflow("add"))?; // i32
            expect_total = expect_total
                .checked_add(EXPECTED_H2_WEIGHTS as u64)
                .ok_or(WeightsError::Overflow("add"))?; // i8
            expect_total = expect_total
                .checked_add(
                    (EXPECTED_H2_BIASES as u64)
                        .checked_mul(4)
                        .ok_or(WeightsError::Overflow("mul"))?,
                )
                .ok_or(WeightsError::Overflow("add"))?; // i32
            expect_total = expect_total
                .checked_add(EXPECTED_OUT_WEIGHTS as u64)
                .ok_or(WeightsError::Overflow("add"))?; // i8
            expect_total = expect_total.checked_add(4).ok_or(WeightsError::Overflow("add"))?; // i32 (OUT bias)

            if expect_total > MAX_FILE_SIZE {
                return Err(WeightsError::SizeTooLarge {
                    size: expect_total,
                    max: MAX_FILE_SIZE,
                });
            }
            if expect_total != meta_len {
                return Err(WeightsError::SizeMismatchV1 {
                    expected: expect_total,
                    actual: meta_len,
                });
            }

            // Read feature transformer weights
            let ft_weights = reader.read_i16_le_vec(EXPECTED_FT_WEIGHTS)?;
            let ft_biases = reader.read_i32_le_vec(EXPECTED_FT_BIASES)?;
            // Read hidden layer 1
            let hidden1_weights = reader.read_weights::<i8>(EXPECTED_H1_WEIGHTS)?;
            let hidden1_biases = reader.read_i32_le_vec(EXPECTED_H1_BIASES)?;
            // Read hidden layer 2
            let hidden2_weights = reader.read_weights::<i8>(EXPECTED_H2_WEIGHTS)?;
            let hidden2_biases = reader.read_i32_le_vec(EXPECTED_H2_BIASES)?;
            // Read output layer
            let output_weights = reader.read_weights::<i8>(EXPECTED_OUT_WEIGHTS)?;
            let output_bias_vec = reader.read_i32_le_vec(EXPECTED_OUT_BIASES)?;
            let output_bias = output_bias_vec
                .first()
                .copied()
                .ok_or(WeightsError::SectionTruncated("output bias"))?;

            // Create structures
            let feature_transformer = FeatureTransformer {
                weights: ft_weights,
                biases: ft_biases,
                acc_dim: FeatureTransformer::DEFAULT_DIM,
            };
            let network = Network {
                hidden1_weights,
                hidden1_biases,
                hidden2_weights,
                hidden2_biases,
                output_weights,
                output_bias,
                input_dim: 512, // 256 x 2 (current classic)
                h1_dim: 32,
                h2_dim: 32,
            };
            Ok((feature_transformer, network))
        }
        2 => {
            // Validate declared size matches actual file size (use v2 variant)
            if meta_len != header.size as u64 {
                return Err(WeightsError::SizeMismatchV2 {
                    expected: header.size as u64,
                    actual: meta_len,
                });
            }
            // Validate architecture for v2
            if header.architecture != HALFKP_X2_DYNAMIC {
                return Err(WeightsError::UnsupportedArchitectureV2(header.architecture));
            }

            // Read dims block: acc_dim, h1_dim, h2_dim (LE u32)
            let acc_dim_u32 = reader.read_u32_le()?;
            let h1_dim_u32 = reader.read_u32_le()?;
            let h2_dim_u32 = reader.read_u32_le()?;

            // Basic range checks
            if acc_dim_u32 == 0
                || h1_dim_u32 == 0
                || h2_dim_u32 == 0
                || acc_dim_u32 > ACC_DIM_MAX
                || h1_dim_u32 > H1_DIM_MAX
                || h2_dim_u32 > H2_DIM_MAX
            {
                return Err(WeightsError::DimsInvalid);
            }

            let acc_dim = acc_dim_u32 as usize;
            let h1_dim = h1_dim_u32 as usize;
            let h2_dim = h2_dim_u32 as usize;
            let input_dim = acc_dim
                .checked_mul(2)
                .ok_or(WeightsError::DimsInconsistent("acc_dim*2 overflow"))?;

            // Compute expected byte size with u64 checked math
            let mut expect_total: u64 = 16 + 12; // header + dims

            // FT weights: 81 * FE_END * acc_dim (i16)
            let ft_w_count = (SHOGI_BOARD_SIZE as u64)
                .checked_mul(FE_END as u64)
                .and_then(|v| v.checked_mul(acc_dim as u64))
                .ok_or(WeightsError::DimsInconsistent("FT weights count overflow"))?;
            expect_total = expect_total
                .checked_add(ft_w_count.checked_mul(2).ok_or(WeightsError::Overflow("mul"))?)
                .ok_or(WeightsError::Overflow("add"))?;
            // FT biases: acc_dim (i32)
            expect_total = expect_total
                .checked_add((acc_dim as u64).checked_mul(4).ok_or(WeightsError::Overflow("mul"))?)
                .ok_or(WeightsError::Overflow("add"))?;
            // H1 weights: input_dim * h1_dim (i8)
            let h1_w_count = (input_dim as u64)
                .checked_mul(h1_dim as u64)
                .ok_or(WeightsError::DimsInconsistent("H1 weights overflow"))?;
            expect_total =
                expect_total.checked_add(h1_w_count).ok_or(WeightsError::Overflow("add"))?;
            // H1 biases: h1_dim (i32)
            expect_total = expect_total
                .checked_add((h1_dim as u64).checked_mul(4).ok_or(WeightsError::Overflow("mul"))?)
                .ok_or(WeightsError::Overflow("add"))?;
            // H2 weights: h1_dim * h2_dim (i8)
            let h2_w_count = (h1_dim as u64)
                .checked_mul(h2_dim as u64)
                .ok_or(WeightsError::DimsInconsistent("H2 weights overflow"))?;
            expect_total =
                expect_total.checked_add(h2_w_count).ok_or(WeightsError::Overflow("add"))?;
            // H2 biases: h2_dim (i32)
            expect_total = expect_total
                .checked_add((h2_dim as u64).checked_mul(4).ok_or(WeightsError::Overflow("mul"))?)
                .ok_or(WeightsError::Overflow("add"))?;
            // OUT weights: h2_dim (i8)
            expect_total =
                expect_total.checked_add(h2_dim as u64).ok_or(WeightsError::Overflow("add"))?;
            // OUT bias: 1 (i32)
            expect_total = expect_total.checked_add(4).ok_or(WeightsError::Overflow("add"))?;

            if expect_total > MAX_FILE_SIZE {
                return Err(WeightsError::SizeTooLarge {
                    size: expect_total,
                    max: MAX_FILE_SIZE,
                });
            }
            if expect_total != meta_len {
                return Err(WeightsError::SizeMismatchV2 {
                    expected: expect_total,
                    actual: meta_len,
                });
            }

            // Now read sections according to dims
            let ft_weights = reader.read_i16_le_vec(
                (SHOGI_BOARD_SIZE * FE_END)
                    .checked_mul(acc_dim)
                    .ok_or(WeightsError::Overflow("mul"))?,
            )?;
            let ft_biases = reader.read_i32_le_vec(acc_dim)?;
            let hidden1_weights = reader.read_weights::<i8>(
                input_dim.checked_mul(h1_dim).ok_or(WeightsError::Overflow("mul"))?,
            )?;
            let hidden1_biases = reader.read_i32_le_vec(h1_dim)?;
            let hidden2_weights = reader.read_weights::<i8>(
                h1_dim.checked_mul(h2_dim).ok_or(WeightsError::Overflow("mul"))?,
            )?;
            let hidden2_biases = reader.read_i32_le_vec(h2_dim)?;
            let output_weights = reader.read_weights::<i8>(h2_dim)?;
            let output_bias_vec = reader.read_i32_le_vec(1)?;
            let output_bias = output_bias_vec
                .first()
                .copied()
                .ok_or(WeightsError::SectionTruncated("output bias"))?;

            let feature_transformer = FeatureTransformer {
                weights: ft_weights,
                biases: ft_biases,
                acc_dim,
            };
            let network = Network {
                hidden1_weights,
                hidden1_biases,
                hidden2_weights,
                hidden2_biases,
                output_weights,
                output_bias,
                input_dim,
                h1_dim,
                h2_dim,
            };
            Ok((feature_transformer, network))
        }
        _ => Err(WeightsError::UnsupportedVersion {
            found: header.version,
            min: MIN_SUPPORTED_VERSION,
            max: MAX_SUPPORTED_VERSION,
        }),
    }
}

/// Save NNUE weights to file (for testing)
#[cfg(test)]
pub fn save_weights(
    path: &str,
    transformer: &FeatureTransformer,
    network: &Network,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::{Seek, SeekFrom, Write};

    let mut file = File::create(path)?;

    // Write header fields explicitly (LE), avoid transmute of struct layout
    file.write_all(b"NNUE")?;
    file.write_all(&1u32.to_le_bytes())?;
    file.write_all(&HALFKP_256X2_32_32.to_le_bytes())?;
    file.write_all(&0u32.to_le_bytes())?; // placeholder, update below

    // Write feature transformer
    let ft_weight_bytes: Vec<u8> =
        transformer.weights.iter().flat_map(|&w| w.to_le_bytes()).collect();
    file.write_all(&ft_weight_bytes)?;

    let ft_bias_bytes: Vec<u8> = transformer.biases.iter().flat_map(|&b| b.to_le_bytes()).collect();
    file.write_all(&ft_bias_bytes)?;

    // Write hidden layer 1
    let h1_weight_bytes: Vec<u8> = network.hidden1_weights.iter().map(|&w| w as u8).collect();
    file.write_all(&h1_weight_bytes)?;
    let h1_bias_bytes: Vec<u8> =
        network.hidden1_biases.iter().flat_map(|&b| b.to_le_bytes()).collect();
    file.write_all(&h1_bias_bytes)?;

    // Write hidden layer 2
    let h2_weight_bytes: Vec<u8> = network.hidden2_weights.iter().map(|&w| w as u8).collect();
    file.write_all(&h2_weight_bytes)?;
    let h2_bias_bytes: Vec<u8> =
        network.hidden2_biases.iter().flat_map(|&b| b.to_le_bytes()).collect();
    file.write_all(&h2_bias_bytes)?;

    // Write output layer
    let out_weight_bytes: Vec<u8> = network.output_weights.iter().map(|&w| w as u8).collect();
    file.write_all(&out_weight_bytes)?;
    file.write_all(&network.output_bias.to_le_bytes())?;

    // Update file size in header
    let file_size = file.seek(SeekFrom::End(0))? as u32;
    file.seek(SeekFrom::Start(12))?; // Offset to size field
    file.write_all(&file_size.to_le_bytes())?;

    Ok(())
}

/// Save v2 dynamic-dims weights with zeroed parameters (tests only)
#[cfg(test)]
pub fn save_weights_v2(
    path: &str,
    acc_dim: usize,
    h1_dim: usize,
    h2_dim: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::{Seek, SeekFrom, Write};
    let input_dim = acc_dim * 2;
    let mut f = File::create(path)?;
    f.write_all(b"NNUE")?;
    f.write_all(&2u32.to_le_bytes())?;
    f.write_all(&HALFKP_X2_DYNAMIC.to_le_bytes())?;
    f.write_all(&0u32.to_le_bytes())?;
    f.write_all(&(acc_dim as u32).to_le_bytes())?;
    f.write_all(&(h1_dim as u32).to_le_bytes())?;
    f.write_all(&(h2_dim as u32).to_le_bytes())?;

    let ft_w = SHOGI_BOARD_SIZE * FE_END * acc_dim;
    f.write_all(&vec![0u8; ft_w * 2])?;
    f.write_all(&vec![0u8; acc_dim * 4])?;
    f.write_all(&vec![0u8; input_dim * h1_dim])?;
    f.write_all(&vec![0u8; h1_dim * 4])?;
    f.write_all(&vec![0u8; h1_dim * h2_dim])?;
    f.write_all(&vec![0u8; h2_dim * 4])?;
    f.write_all(&vec![0u8; h2_dim])?;
    f.write_all(&0i32.to_le_bytes())?;
    let size = f.seek(SeekFrom::End(0))? as u32;
    f.seek(SeekFrom::Start(12))?;
    f.write_all(&size.to_le_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_header_size() {
        assert_eq!(mem::size_of::<NNUEHeader>(), 16);
    }

    #[test]
    fn test_save_load_weights() {
        let transformer = FeatureTransformer::zero();
        let network = Network::zero();

        let path = "/tmp/test_nnue.bin";
        save_weights(path, &transformer, &network).unwrap();

        let (loaded_transformer, loaded_network) = load_weights(path).unwrap();

        // Verify loaded matches saved
        assert_eq!(loaded_transformer.weights.len(), transformer.weights.len());
        assert_eq!(loaded_transformer.biases.len(), transformer.biases.len());
        assert_eq!(loaded_network.hidden1_weights.len(), network.hidden1_weights.len());

        // Clean up
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn test_invalid_magic() {
        let path = "/tmp/invalid_nnue.bin";
        // Write at least 16 bytes so header read succeeds and magic check triggers
        let mut data = Vec::new();
        data.extend_from_slice(b"BAD!"); // invalid magic
        data.extend_from_slice(&[0u8; 12]); // pad for version/arch/size
        std::fs::write(path, data).unwrap();

        let err = match load_weights(path) {
            Err(e) => e,
            Ok(_) => panic!("expected error"),
        };
        assert!(matches!(err, WeightsError::InvalidMagic));

        std::fs::remove_file(path).ok();
    }

    #[test]
    fn test_v1_version_mismatch() {
        use std::io::Write;
        let path = "/tmp/test_nnue_v1_version.bin";
        let mut f = File::create(path).unwrap();
        f.write_all(b"NNUE").unwrap();
        f.write_all(&9999u32.to_le_bytes()).unwrap(); // unsupported version
        f.write_all(&HALFKP_256X2_32_32.to_le_bytes()).unwrap();
        f.write_all(&16u32.to_le_bytes()).unwrap(); // header size only
        drop(f);
        let err = match load_weights(path) {
            Err(e) => e,
            Ok(_) => panic!("expected error"),
        };
        match err {
            WeightsError::UnsupportedVersion { .. } => {}
            other => panic!("unexpected error: {other}"),
        }
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn test_v1_architecture_mismatch() {
        use std::io::Write;
        let path = "/tmp/test_nnue_v1_arch.bin";
        let mut f = File::create(path).unwrap();
        f.write_all(b"NNUE").unwrap();
        f.write_all(&1u32.to_le_bytes()).unwrap();
        f.write_all(&0xDEAD_BEEFu32.to_le_bytes()).unwrap(); // wrong arch
        f.write_all(&16u32.to_le_bytes()).unwrap();
        drop(f);
        let err = match load_weights(path) {
            Err(e) => e,
            Ok(_) => panic!("expected error"),
        };
        match err {
            WeightsError::UnsupportedArchitectureV1(_) => {}
            other => panic!("unexpected error: {other}"),
        }
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn test_v1_size_mismatch() {
        use std::io::Write;
        let path = "/tmp/test_nnue_v1_size.bin";
        let mut f = File::create(path).unwrap();
        f.write_all(b"NNUE").unwrap();
        f.write_all(&1u32.to_le_bytes()).unwrap();
        f.write_all(&HALFKP_256X2_32_32.to_le_bytes()).unwrap();
        f.write_all(&999_999u32.to_le_bytes()).unwrap(); // wrong size
        drop(f);
        let err = match load_weights(path) {
            Err(e) => e,
            Ok(_) => panic!("expected error"),
        };
        match err {
            WeightsError::SizeMismatchV1 { .. } => {}
            other => panic!("unexpected error: {other}"),
        }
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn test_v2_architecture_mismatch() {
        use std::io::Write;
        // Header + dims only to pass size==len check
        let path = "/tmp/test_nnue_v2_arch.bin";
        let mut f = File::create(path).unwrap();
        f.write_all(b"NNUE").unwrap();
        f.write_all(&2u32.to_le_bytes()).unwrap();
        f.write_all(&0xCAFEBABEu32.to_le_bytes()).unwrap(); // wrong arch for v2
        f.write_all(&28u32.to_le_bytes()).unwrap(); // header + dims
        f.write_all(&1u32.to_le_bytes()).unwrap(); // acc
        f.write_all(&1u32.to_le_bytes()).unwrap(); // h1
        f.write_all(&1u32.to_le_bytes()).unwrap(); // h2
        drop(f);
        let err = match load_weights(path) {
            Err(e) => e,
            Ok(_) => panic!("expected error"),
        };
        match err {
            WeightsError::UnsupportedArchitectureV2(_) => {}
            other => panic!("unexpected error: {other}"),
        }
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn test_v2_dims_zero_invalid() {
        use std::io::Write;
        let path = "/tmp/test_nnue_v2_dims_zero.bin";
        let mut f = File::create(path).unwrap();
        f.write_all(b"NNUE").unwrap();
        f.write_all(&2u32.to_le_bytes()).unwrap();
        f.write_all(&HALFKP_X2_DYNAMIC.to_le_bytes()).unwrap();
        f.write_all(&28u32.to_le_bytes()).unwrap(); // header + dims only
        f.write_all(&0u32.to_le_bytes()).unwrap(); // acc_dim = 0 -> invalid
        f.write_all(&1u32.to_le_bytes()).unwrap();
        f.write_all(&1u32.to_le_bytes()).unwrap();
        drop(f);
        let err = match load_weights(path) {
            Err(e) => e,
            Ok(_) => panic!("expected error"),
        };
        match err {
            WeightsError::DimsInvalid => {}
            other => panic!("unexpected error: {other}"),
        }
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn test_v2_dims_exceed_max() {
        use std::io::Write;
        let path = "/tmp/test_nnue_v2_dims_exceed.bin";
        let mut f = File::create(path).unwrap();
        f.write_all(b"NNUE").unwrap();
        f.write_all(&2u32.to_le_bytes()).unwrap();
        f.write_all(&HALFKP_X2_DYNAMIC.to_le_bytes()).unwrap();
        f.write_all(&28u32.to_le_bytes()).unwrap(); // header + dims only
        let too_big = super::ACC_DIM_MAX + 1;
        f.write_all(&too_big.to_le_bytes()).unwrap();
        f.write_all(&1u32.to_le_bytes()).unwrap();
        f.write_all(&1u32.to_le_bytes()).unwrap();
        drop(f);
        let err = match load_weights(path) {
            Err(e) => e,
            Ok(_) => panic!("expected error"),
        };
        match err {
            WeightsError::DimsInvalid => {}
            other => panic!("unexpected error: {other}"),
        }
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn test_v2_size_mismatch_due_to_truncated_sections() {
        use std::io::Write;
        // Use tiny dims to keep file small
        let acc_dim = 1u32;
        let h1_dim = 1u32;
        let h2_dim = 1u32;
        let path = "/tmp/test_nnue_v2_size_mismatch.bin";
        let mut f = File::create(path).unwrap();
        f.write_all(b"NNUE").unwrap();
        f.write_all(&2u32.to_le_bytes()).unwrap();
        f.write_all(&HALFKP_X2_DYNAMIC.to_le_bytes()).unwrap();
        // We'll write header size equal to actual len (small), but dims imply much larger total
        let body_stub_len = 28u32 + 16; // header + dims + 16 bytes only
        f.write_all(&body_stub_len.to_le_bytes()).unwrap();
        // dims
        f.write_all(&acc_dim.to_le_bytes()).unwrap();
        f.write_all(&h1_dim.to_le_bytes()).unwrap();
        f.write_all(&h2_dim.to_le_bytes()).unwrap();
        // write just a few bytes, far fewer than expected
        f.write_all(&[0u8; 16]).unwrap();
        drop(f);
        let err = match load_weights(path) {
            Err(e) => e,
            Ok(_) => panic!("expected error"),
        };
        match err {
            WeightsError::SizeMismatchV2 { .. } => {}
            other => panic!("unexpected error: {other}"),
        }
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn test_load_v2_zero_weights() {
        // no-op

        let acc_dim: usize = 256;
        let h1_dim: usize = 32;
        let h2_dim: usize = 32;
        let input_dim = acc_dim * 2;

        let path = "/tmp/test_nnue_v2.bin";
        save_weights_v2(path, acc_dim, h1_dim, h2_dim).unwrap();

        // Load and verify dims reflected
        let (ft, net) = load_weights(path).unwrap();
        assert_eq!(ft.acc_dim(), acc_dim);
        assert_eq!(net.input_dim, input_dim);
        assert_eq!(net.h1_dim, h1_dim);
        assert_eq!(net.h2_dim, h2_dim);

        std::fs::remove_file(path).ok();
    }

    #[test]
    fn test_v2_round_trip_small_dims() {
        // no-op
        // small dims
        let acc_dim = 2usize;
        let h1_dim = 3usize;
        let h2_dim = 4usize;
        let input_dim = acc_dim * 2;
        let path = "/tmp/test_nnue_v2_roundtrip.bin";

        save_weights_v2(path, acc_dim, h1_dim, h2_dim).unwrap();

        // read back
        let (ft, net) = load_weights(path).unwrap();
        assert_eq!(ft.acc_dim(), acc_dim);
        assert_eq!(net.input_dim, input_dim);
        assert_eq!(net.h1_dim, h1_dim);
        assert_eq!(net.h2_dim, h2_dim);

        std::fs::remove_file(path).ok();
    }

    // Property-based: small dims must succeed
    use proptest::prelude::*;
    proptest! {
        #[test]
        fn v2_small_dims_roundtrip(ad in 1usize..=3, h1 in 1usize..=3, h2 in 1usize..=3) {
            let acc_dim = ad; let h1_dim = h1; let h2_dim = h2; let input_dim = acc_dim*2;
            let path = format!("/tmp/nnue_v2_prop_{}_{}_{}.bin", acc_dim, h1_dim, h2_dim);
            save_weights_v2(&path, acc_dim, h1_dim, h2_dim).unwrap();
            let (ft, net) = load_weights(&path).unwrap();
            prop_assert_eq!(ft.acc_dim(), acc_dim);
            prop_assert_eq!(net.input_dim, input_dim);
            prop_assert_eq!(net.h1_dim, h1_dim);
            prop_assert_eq!(net.h2_dim, h2_dim);
            std::fs::remove_file(&path).ok();
        }
    }
}

// Endianness-aware float reader for SINGLE weights
#[cfg(target_endian = "little")]
fn read_f32_vec(
    r: &mut std::io::Cursor<&[u8]>,
    n: usize,
) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
    let mut out = vec![0f32; n];
    // Safe: direct byte copy on little-endian targets
    let bytes = unsafe { std::slice::from_raw_parts_mut(out.as_mut_ptr() as *mut u8, n * 4) };
    r.read_exact(bytes)?;
    Ok(out)
}

#[cfg(not(target_endian = "little"))]
fn read_f32_vec(
    r: &mut std::io::Cursor<&[u8]>,
    n: usize,
) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let mut b = [0u8; 4];
        r.read_exact(&mut b)?;
        out.push(f32::from_le_bytes(b));
    }
    Ok(out)
}

/// Try to load SINGLE_CHANNEL (Version 2) weights with text header (trainer format)
pub fn load_single_weights(
    path: &str,
) -> Result<super::single::SingleChannelNet, Box<dyn std::error::Error>> {
    use std::fs;
    let data = fs::read(path)?;
    // Find END_HEADER
    let hdr_tag = b"END_HEADER";
    let hdr_pos = data
        .windows(hdr_tag.len())
        .position(|w| w == hdr_tag)
        .ok_or_else(|| "SINGLE_CHANNEL header not found".to_string())?;
    // Find newline after END_HEADER
    let mut i = hdr_pos + hdr_tag.len();
    while i < data.len() && data[i] != b'\n' {
        i += 1;
    }
    if i >= data.len() {
        return Err("Malformed SINGLE_CHANNEL header (no newline)".into());
    }
    let bin_off = i + 1;

    use std::io::Read;
    let mut rdr = std::io::Cursor::new(&data[bin_off..]);
    let mut u4 = [0u8; 4];
    rdr.read_exact(&mut u4)?;
    let input_dim = u32::from_le_bytes(u4) as usize;
    rdr.read_exact(&mut u4)?;
    let acc_dim = u32::from_le_bytes(u4) as usize;
    if input_dim == 0 || acc_dim == 0 {
        return Err("Invalid SINGLE_CHANNEL dims".into());
    }

    #[cfg(debug_assertions)]
    {
        use super::features::FE_END;
        use crate::shogi::SHOGI_BOARD_SIZE;
        let expected = SHOGI_BOARD_SIZE * FE_END;
        if input_dim != expected {
            log::warn!(
                "[NNUE] SINGLE_CHANNEL: input_dim({}) != expected({}) = SHOGI_BOARD_SIZE * FE_END — 語彙ずれの可能性",
                input_dim, expected
            );
        }
    }

    // Determine presence of b0 by remaining length (w0/b0/w2/b2). Fail fast on mismatch.
    let bytes_after_dims = data[bin_off + 8..].len();
    let bytes_w0 = input_dim
        .checked_mul(acc_dim)
        .and_then(|v| v.checked_mul(4))
        .ok_or("SINGLE_CHANNEL size overflow")?;
    let bytes_b0 = acc_dim.checked_mul(4).ok_or("SINGLE_CHANNEL size overflow")?;
    let bytes_w2 = acc_dim.checked_mul(4).ok_or("SINGLE_CHANNEL size overflow")?;
    let bytes_b2 = 4usize;

    let need_with_b0 = bytes_w0
        .checked_add(bytes_b0)
        .and_then(|v| v.checked_add(bytes_w2))
        .and_then(|v| v.checked_add(bytes_b2))
        .ok_or("SINGLE_CHANNEL size overflow")?;
    let need_without_b0 = bytes_w0
        .checked_add(bytes_w2)
        .and_then(|v| v.checked_add(bytes_b2))
        .ok_or("SINGLE_CHANNEL size overflow")?;

    let has_b0 = if bytes_after_dims == need_with_b0 {
        true
    } else if bytes_after_dims == need_without_b0 {
        false
    } else {
        return Err(format!(
            "SINGLE_CHANNEL size mismatch: rem={} (expect {} with b0 or {} without b0)",
            bytes_after_dims, need_with_b0, need_without_b0
        )
        .into());
    };

    let w0 = read_f32_vec(&mut rdr, input_dim * acc_dim)?;
    let b0 = if has_b0 {
        Some(read_f32_vec(&mut rdr, acc_dim)?)
    } else {
        None
    };
    let w2 = read_f32_vec(&mut rdr, acc_dim)?;
    rdr.read_exact(&mut u4)?;
    let b2 = f32::from_le_bytes(u4);

    // Cheap deterministic 64-bit hash as weights UID
    fn hash_f32s(mut h: u64, xs: &[f32]) -> u64 {
        for &v in xs {
            let b = v.to_le_bytes();
            let x = u32::from_le_bytes(b) as u64;
            // group hex digits in equal-sized groups for clippy friendliness
            h ^= x.wrapping_mul(0x0100_0000_01b3);
            h = h.rotate_left(13).wrapping_mul(0xc2b2_ae3d_27d4_eb4f);
        }
        h
    }
    let mut uid = 0x9E37_79B9_7F4A_7C15u64 ^ (input_dim as u64) ^ ((acc_dim as u64) << 32);
    uid = hash_f32s(uid, &w0);
    if let Some(ref bias0) = b0 {
        uid = hash_f32s(uid, bias0);
    }
    uid = hash_f32s(uid, &w2);
    uid ^= (b2.to_bits() as u64).wrapping_mul(0x9ddf_ea08eb382d69);

    Ok(super::single::SingleChannelNet {
        n_feat: input_dim,
        acc_dim,
        scale: 600.0,
        w0,
        b0,
        w2,
        b2,
        uid,
    })
}
