//! LayerStacks `.bin` の feature 非依存レイアウト走査。

use std::fmt;
use std::io::{self, Cursor, Read, Seek, SeekFrom, Write};
use std::ops::Range;

use super::constants::{
    DEFAULT_NUM_BUCKETS, HALFKA_DIMENSIONS, HALFKA_HM_DIMENSIONS, HALFKA_HM_SPLIT_DIMENSIONS,
    HALFKA_MERGED_DIMENSIONS, HALFKP_DIMENSIONS, MAX_ARCH_LEN, MAX_LAYER_STACK_BUCKETS,
    NNUE_VERSION_HALFKA, NNUE_VERSION_LAYERSTACK_NUM_BUCKETS,
};
use super::leb128::{LEB128_MAGIC, MAX_COMPRESSED_SIZE, encode_signed_leb128};
use super::net_delta::{
    NetCoefficientId, NetDelta, NetDeltaError, NetDeltaReport, NetTensorKind, NetTensorShape,
    add_i8_delta, add_i16_delta, add_i32_delta,
};
use super::spec::{
    FeatureSet, detect_layer_stacks_feature, parse_arch_dimensions, parse_effect_bucket_config,
    parse_feature_input_dimensions, validate_layer_stacks_architecture_header,
    validate_layer_stacks_dimensions,
};

/// tensor の biases / weights byte 範囲。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TensorBinLayout {
    /// biases の byte 範囲。
    pub biases: Range<usize>,
    /// weights の byte 範囲。
    pub weights: Range<usize>,
}

/// Feature Transformer の格納方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FtBinEncoding {
    /// biases と weights を連結した旧 bullet 形式の LEB128 1 ブロック。
    Leb128Combined,
    /// biases と weights を分けた YO 形式の LEB128 2 ブロック。
    Leb128Split,
}

/// Feature Transformer ブロックのレイアウト。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeatureTransformerBinLayout {
    /// FT hash の byte 範囲。
    pub hash: Range<usize>,
    /// FT の格納方式。
    pub encoding: FtBinEncoding,
    /// bias 値を格納する byte 範囲。LEB128 では block header を除く。
    pub biases: Range<usize>,
    /// weight 値を格納する byte 範囲。LEB128 では block header を除く。
    pub weights: Range<usize>,
}

/// 1 bucket 分の全結合層レイアウト。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerStackBucketBinLayout {
    /// bucket 固有 FC hash の byte 範囲。
    pub fc_hash: Range<usize>,
    /// 第 1 FC 層。
    pub l1: TensorBinLayout,
    /// 第 2 FC 層。
    pub l2: TensorBinLayout,
    /// output 層。
    pub output: TensorBinLayout,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredBinBlock {
    source: Range<usize>,
    bytes: Vec<u8>,
}

/// LayerStacks `.bin` 全体の byte レイアウト。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerStacksBinLayout {
    /// NNUE format version。
    pub version: u32,
    /// network hash。
    pub network_hash: u32,
    /// header の architecture 文字列。
    pub architecture: String,
    /// 格納 bucket 数。
    pub num_buckets: usize,
    /// Feature Transformer 出力次元。
    pub l1: usize,
    /// 第 1 FC 層出力次元。
    pub l2: usize,
    /// 第 2 FC 層出力次元。
    pub l3: usize,
    /// Threat を除く Feature Transformer 入力次元。
    pub ft_input_dimensions: usize,
    /// Feature Transformer ブロック。
    pub feature_transformer: FeatureTransformerBinLayout,
    /// PSQT ブロック。architecture が PSQT を持たなければ `None`。
    pub psqt: Option<TensorBinLayout>,
    /// Threat profile id の byte 範囲。
    pub threat_profile: Option<Range<usize>>,
    /// Threat weights の byte 範囲。
    pub threat_weights: Option<Range<usize>>,
    /// bucket ごとの FC ブロック。
    pub buckets: Vec<LayerStackBucketBinLayout>,
    // FT weights は EffectBucket で GiB 級になるため保持せず、係数列挙に必要な
    // FT bias と小さい FC block だけを保持して常駐量を net の FT サイズから切り離す。
    ft_biases: Vec<i16>,
    fc_data: StoredBinBlock,
}

/// LayerStacks `.bin` の書換えエラー。
#[derive(Debug)]
pub enum NetBinPatchError {
    /// `.bin` レイアウトの読み取りに失敗した。
    Io(io::Error),
    /// delta の ID または対象バイト列が不正だった。
    Delta(NetDeltaError),
}

impl fmt::Display for NetBinPatchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(formatter),
            Self::Delta(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for NetBinPatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Delta(error) => Some(error),
        }
    }
}

impl From<io::Error> for NetBinPatchError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<NetDeltaError> for NetBinPatchError {
    fn from(error: NetDeltaError) -> Self {
        Self::Delta(error)
    }
}

/// LayerStacks `.bin` の指定係数へ engine と同じ saturating delta をストリーミング適用する。
///
/// 編集対象外は固定サイズバッファで転送する。FT は bias prefix だけを再エンコードし、
/// GiB 級になり得る weights payload は読み込まず入力 byte をそのまま転送する。
pub fn apply_deltas<R: Read + Seek, W: Write>(
    input: &mut R,
    output: &mut W,
    deltas: &[NetDelta],
) -> Result<NetDeltaReport, NetBinPatchError> {
    let mut layout = LayerStacksBinLayout::from_reader(input)?;
    for delta in deltas {
        layout.tensor_shape(delta.id.kind).validate(&delta.id)?;
    }
    let has_nonzero_ft_delta = deltas
        .iter()
        .any(|delta| delta.id.kind == NetTensorKind::FtBias && delta.delta != 0);
    if has_nonzero_ft_delta {
        validate_canonical_ft_biases(input, &layout)?;
    }

    let mut report = NetDeltaReport {
        applied: deltas.len(),
        clamped: 0,
    };
    for delta in deltas {
        if delta.delta == 0 {
            continue;
        }
        let clamped = match delta.id.kind {
            NetTensorKind::FtBias => {
                let current = layout
                    .ft_biases
                    .get_mut(delta.id.index)
                    .ok_or_else(|| invalid_binary("validated FT bias index is missing"))?;
                let (value, clamped) = add_i16_delta(*current, delta.delta);
                *current = value;
                clamped
            }
            NetTensorKind::OutputWeight => {
                let bucket = validated_bucket(&layout, &delta.id)?;
                let offset = bucket.output.weights.start + delta.id.index;
                layout.fc_data.patch_i8(offset, delta.delta)?
            }
            NetTensorKind::OutputBias => {
                let bucket = validated_bucket(&layout, &delta.id)?;
                let offset = bucket.output.biases.start;
                layout.fc_data.patch_i32(offset, delta.delta)?
            }
            NetTensorKind::L2Weight => {
                let bucket = validated_bucket(&layout, &delta.id)?;
                let offset = bucket.l2.weights.start + delta.id.index;
                layout.fc_data.patch_i8(offset, delta.delta)?
            }
        };
        report.clamped += usize::from(clamped);
    }

    let mut source_position = 0;
    if has_nonzero_ft_delta {
        source_position = write_reencoded_ft_biases(input, output, &layout)?;
    }
    copy_range(input, output, source_position..layout.fc_data.source.start)?;
    output.write_all(&layout.fc_data.bytes)?;
    Ok(report)
}

fn validated_bucket<'a>(
    layout: &'a LayerStacksBinLayout,
    id: &NetCoefficientId,
) -> Result<&'a LayerStackBucketBinLayout, NetDeltaError> {
    let bucket = id.bucket.ok_or_else(|| NetDeltaError::MissingBucket {
        name: id.usi_name(),
    })?;
    layout
        .buckets
        .get(bucket)
        .ok_or_else(|| invalid_binary("validated bucket is missing from layout"))
}

impl LayerStacksBinLayout {
    /// byte slice を走査し、LayerStacks の各 byte 範囲を返す。
    ///
    /// 実装は [`Self::from_reader`] に委譲する。
    pub fn from_bytes(bytes: &[u8]) -> io::Result<Self> {
        Self::from_reader(&mut Cursor::new(bytes))
    }

    /// seek 可能な `.bin` を走査し、LayerStacks の各 byte 範囲を返す。
    ///
    /// FT の圧縮 weights payload は読み込まず seek で飛ばす。常駐メモリは decode 済み
    /// FT bias と全 bucket の FC block に限られ、FT weights のサイズには依存しない。
    pub fn from_reader<R: Read + Seek>(reader: &mut R) -> io::Result<Self> {
        Self::read_layout(reader, true)
    }

    /// 変換用に byte 範囲だけを走査し、FC 係数を保持しない。
    /// 返り値の coefficient API は FC データを利用できないため使わないこと。
    #[cfg(feature = "prepacked-nnue")]
    pub(crate) fn scan_for_conversion<R: Read + Seek>(reader: &mut R) -> io::Result<Self> {
        Self::read_layout(reader, false)
    }

    fn read_layout<R: Read + Seek>(reader: &mut R, retain_fc: bool) -> io::Result<Self> {
        let mut cursor = StreamCursor::new(reader)?;
        let version = cursor.read_u32("version")?;
        if version != NNUE_VERSION_HALFKA && version != NNUE_VERSION_LAYERSTACK_NUM_BUCKETS {
            return Err(invalid(format!("unsupported LayerStacks version {version:#x}")));
        }
        let network_hash = cursor.read_u32("network hash")?;
        let arch_len = cursor.read_u32("architecture length")? as usize;
        if arch_len == 0 || arch_len > MAX_ARCH_LEN {
            return Err(invalid(format!("invalid architecture string length: {arch_len}")));
        }
        let architecture_bytes = cursor.read_bytes(arch_len, "architecture string")?;
        let architecture = std::str::from_utf8(&architecture_bytes)
            .map_err(|_| invalid("architecture string is not UTF-8"))?
            .to_owned();
        if !is_layer_stacks_architecture(&architecture) {
            return Err(invalid(format!("architecture is not LayerStacks: {architecture}")));
        }

        let num_buckets = if version == NNUE_VERSION_LAYERSTACK_NUM_BUCKETS {
            cursor.read_u32("num_buckets")? as usize
        } else {
            DEFAULT_NUM_BUCKETS
        };
        if !(1..=MAX_LAYER_STACK_BUCKETS).contains(&num_buckets) {
            return Err(invalid(format!(
                "invalid LayerStacks num_buckets={num_buckets}; expected 1..={MAX_LAYER_STACK_BUCKETS}"
            )));
        }

        let (l1, l2, l3) = parse_arch_dimensions(&architecture);
        validate_layer_stacks_dimensions(l1, l2, l3).map_err(invalid)?;
        let threat_dimensions = validate_layer_stacks_architecture_header(&architecture)
            .map_err(invalid)?
            .unwrap_or(0);
        let ft_input_dimensions = parse_ft_input_dimensions(&architecture, threat_dimensions)?;
        let ft_hash = cursor.take(4, "FT hash")?;
        let (feature_transformer, ft_biases) = parse_leb128_ft(&mut cursor, ft_hash, l1)?;

        let psqt = if architecture.contains("PSQT=") {
            Some(TensorBinLayout {
                biases: cursor.take_count(num_buckets, 4, "PSQT biases")?,
                weights: cursor.take_count(
                    ft_input_dimensions
                        .checked_mul(num_buckets)
                        .ok_or_else(|| invalid("PSQT dimensions overflow"))?,
                    4,
                    "PSQT weights",
                )?,
            })
        } else {
            None
        };
        let threat_profile = if architecture.contains("ThreatProfile=") {
            Some(cursor.take(4, "Threat profile")?)
        } else {
            None
        };
        let threat_weights = if threat_dimensions == 0 {
            None
        } else {
            Some(
                cursor.take_count(
                    threat_dimensions
                        .checked_mul(l1)
                        .ok_or_else(|| invalid("Threat dimensions overflow"))?,
                    1,
                    "Threat weights",
                )?,
            )
        };

        let mut buckets = Vec::with_capacity(num_buckets);
        for bucket in 0..num_buckets {
            let fc_hash = cursor.take(4, &format!("bucket {bucket} FC hash"))?;
            let first = parse_affine(&mut cursor, l1, l2, "l1")?;
            let second_input = 2usize
                .checked_mul(l2 - 1)
                .ok_or_else(|| invalid("L2 input dimensions overflow"))?;
            let second = parse_affine(&mut cursor, second_input, l3, "l2")?;
            let output = parse_affine(&mut cursor, l3, 1, "output")?;
            buckets.push(LayerStackBucketBinLayout {
                fc_hash,
                l1: first,
                l2: second,
                output,
            });
        }
        let consumed = cursor.position;
        if consumed != cursor.file_size {
            return Err(invalid(format!(
                "unexpected trailing LayerStacks data: consumed={}, file_size={}",
                consumed, cursor.file_size
            )));
        }
        let fc_range = buckets
            .first()
            .zip(buckets.last())
            .map(|(first, last)| first.fc_hash.start..last.output.weights.end)
            .ok_or_else(|| invalid("LayerStacks has no FC buckets"))?;
        let fc_data = StoredBinBlock {
            bytes: if retain_fc {
                cursor.read_range(fc_range.clone(), "FC blocks")?
            } else {
                Vec::new()
            },
            source: fc_range,
        };
        Ok(Self {
            version,
            network_hash,
            architecture,
            num_buckets,
            l1,
            l2,
            l3,
            ft_input_dimensions,
            feature_transformer,
            psqt,
            threat_profile,
            threat_weights,
            buckets,
            ft_biases,
            fc_data,
        })
    }

    /// 対象 tensor のファイル格納上の形状を返す。
    pub fn tensor_shape(&self, kind: NetTensorKind) -> NetTensorShape {
        match kind {
            NetTensorKind::OutputWeight => NetTensorShape {
                bucket_count: Some(self.num_buckets),
                element_count: self.buckets[0].output.weights.len(),
            },
            NetTensorKind::OutputBias => NetTensorShape {
                bucket_count: Some(self.num_buckets),
                element_count: 1,
            },
            NetTensorKind::FtBias => NetTensorShape {
                bucket_count: None,
                element_count: self.l1,
            },
            NetTensorKind::L2Weight => NetTensorShape {
                bucket_count: Some(self.num_buckets),
                element_count: self.buckets[0].l2.weights.len(),
            },
        }
    }

    /// ファイル格納順 ID で指定した係数の現在値を返す。
    pub fn coefficient(&self, id: &NetCoefficientId) -> Result<i32, NetDeltaError> {
        self.tensor_shape(id.kind).validate(id)?;
        match id.kind {
            NetTensorKind::FtBias => Ok(i32::from(self.ft_biases[id.index])),
            NetTensorKind::OutputWeight => {
                let bucket = id.bucket.expect("validated bucket");
                self.fc_data.read_i8(self.buckets[bucket].output.weights.start + id.index)
            }
            NetTensorKind::OutputBias => {
                let bucket = id.bucket.expect("validated bucket");
                self.fc_data.read_i32(self.buckets[bucket].output.biases.start)
            }
            NetTensorKind::L2Weight => {
                let bucket = id.bucket.expect("validated bucket");
                self.fc_data.read_i8(self.buckets[bucket].l2.weights.start + id.index)
            }
        }
    }
}

impl StoredBinBlock {
    fn get(&self, offset: usize, width: usize) -> Result<&[u8], NetDeltaError> {
        let relative = offset
            .checked_sub(self.source.start)
            .ok_or_else(|| invalid_binary(format!("byte offset {offset}")))?;
        let end = relative
            .checked_add(width)
            .ok_or_else(|| invalid_binary("byte range overflow"))?;
        self.bytes
            .get(relative..end)
            .ok_or_else(|| invalid_binary(format!("byte offset {offset}")))
    }

    fn read_i8(&self, offset: usize) -> Result<i32, NetDeltaError> {
        Ok(i32::from(self.get(offset, 1)?[0] as i8))
    }

    fn read_i32(&self, offset: usize) -> Result<i32, NetDeltaError> {
        Ok(i32::from_le_bytes(
            self.get(offset, 4)?.try_into().map_err(|_| invalid_binary("truncated i32"))?,
        ))
    }

    fn patch_i8(&mut self, offset: usize, delta: i32) -> Result<bool, NetDeltaError> {
        let relative = offset
            .checked_sub(self.source.start)
            .ok_or_else(|| invalid_binary(format!("byte offset {offset}")))?;
        let current = self
            .bytes
            .get_mut(relative)
            .ok_or_else(|| invalid_binary(format!("byte offset {offset}")))?;
        let (value, clamped) = add_i8_delta(*current as i8, delta);
        *current = value as u8;
        Ok(clamped)
    }

    fn patch_i32(&mut self, offset: usize, delta: i32) -> Result<bool, NetDeltaError> {
        let relative = offset
            .checked_sub(self.source.start)
            .ok_or_else(|| invalid_binary(format!("i32 offset {offset}")))?;
        let end = relative.checked_add(4).ok_or_else(|| invalid_binary("i32 range overflow"))?;
        let bytes = self
            .bytes
            .get_mut(relative..end)
            .ok_or_else(|| invalid_binary(format!("i32 offset {offset}")))?;
        let current =
            i32::from_le_bytes(bytes.try_into().map_err(|_| invalid_binary("truncated i32"))?);
        let (value, clamped) = add_i32_delta(current, delta);
        bytes.copy_from_slice(&value.to_le_bytes());
        Ok(clamped)
    }
}

fn is_layer_stacks_architecture(architecture: &str) -> bool {
    matches!(
        super::spec::parse_feature_set_from_arch(architecture),
        Ok(FeatureSet::LayerStacks | FeatureSet::HalfKaHmMergedEffectBucket)
    ) || matches!(
        detect_layer_stacks_feature(architecture),
        Ok(FeatureSet::HalfKaHmMergedEffectBucket)
    )
}

fn parse_ft_input_dimensions(architecture: &str, threat: usize) -> io::Result<usize> {
    let reported = parse_feature_input_dimensions(architecture)
        .ok_or_else(|| invalid("missing FT input dimensions"))?;
    let feature = detect_layer_stacks_feature(architecture).map_err(invalid)?;
    let dimensions = if feature == FeatureSet::HalfKaHmMergedEffectBucket {
        parse_effect_bucket_config(architecture)
            .ok_or_else(|| invalid("malformed EffectBucket token"))?
            .dimensions()
    } else {
        match feature {
            FeatureSet::HalfKP => HALFKP_DIMENSIONS,
            FeatureSet::HalfKaHmMerged => HALFKA_HM_DIMENSIONS,
            FeatureSet::HalfKaSplit => HALFKA_DIMENSIONS,
            FeatureSet::HalfKaMerged => HALFKA_MERGED_DIMENSIONS,
            FeatureSet::HalfKaHmSplit => HALFKA_HM_SPLIT_DIMENSIONS,
            FeatureSet::HalfKaHmMergedEffectBucket | FeatureSet::LayerStacks => {
                return Err(invalid("unsupported LayerStacks FT feature set"));
            }
        }
    };
    if reported == dimensions || (threat != 0 && dimensions.checked_add(threat) == Some(reported)) {
        Ok(dimensions)
    } else {
        Err(invalid(format!(
            "FT input dimension mismatch: header={reported}, runtime={dimensions}, threat={threat}"
        )))
    }
}

fn parse_leb128_ft<R: Read + Seek>(
    cursor: &mut StreamCursor<'_, R>,
    hash: Range<usize>,
    bias_count: usize,
) -> io::Result<(FeatureTransformerBinLayout, Vec<i16>)> {
    let first = cursor.take_leb128_prefix("FT first LEB128 block", bias_count)?;
    if first.prefix_end == first.data.end {
        let weights = cursor.take_leb128_payload("FT weights LEB128 block")?;
        Ok((
            FeatureTransformerBinLayout {
                hash,
                encoding: FtBinEncoding::Leb128Split,
                biases: first.data,
                weights,
            },
            first.values,
        ))
    } else {
        Ok((
            FeatureTransformerBinLayout {
                hash,
                encoding: FtBinEncoding::Leb128Combined,
                biases: first.data.start..first.prefix_end,
                weights: first.prefix_end..first.data.end,
            },
            first.values,
        ))
    }
}

fn parse_affine<R: Read + Seek>(
    cursor: &mut StreamCursor<'_, R>,
    input: usize,
    output: usize,
    name: &str,
) -> io::Result<TensorBinLayout> {
    Ok(TensorBinLayout {
        biases: cursor.take_count(output, 4, &format!("{name} biases"))?,
        weights: cursor.take_count(
            output
                .checked_mul(checked_padded_input(input)?)
                .ok_or_else(|| invalid(format!("{name} dimensions overflow")))?,
            1,
            &format!("{name} weights"),
        )?,
    })
}

fn checked_padded_input(input: usize) -> io::Result<usize> {
    input
        .checked_add(31)
        .map(|value| value / 32 * 32)
        .ok_or_else(|| invalid("padded input dimensions overflow"))
}

struct Leb128Block {
    data: Range<usize>,
    prefix_end: usize,
    values: Vec<i16>,
}

struct StreamCursor<'a, R> {
    reader: &'a mut R,
    position: usize,
    file_size: usize,
}

impl<'a, R: Read + Seek> StreamCursor<'a, R> {
    fn new(reader: &'a mut R) -> io::Result<Self> {
        let position = usize_from_u64(reader.stream_position()?, "stream position")?;
        if position != 0 {
            return Err(invalid(format!(
                "LayerStacks reader must start at byte 0, got {position}"
            )));
        }
        let file_size = usize_from_u64(reader.seek(SeekFrom::End(0))?, "file size")?;
        reader.seek(SeekFrom::Start(0))?;
        Ok(Self {
            reader,
            position: 0,
            file_size,
        })
    }

    fn take(&mut self, size: usize, name: &str) -> io::Result<Range<usize>> {
        let end = self
            .position
            .checked_add(size)
            .ok_or_else(|| invalid(format!("{name} range overflow")))?;
        if end > self.file_size {
            return Err(invalid(format!("truncated {name}")));
        }
        let range = self.position..end;
        self.reader.seek(SeekFrom::Start(u64_from_usize(end, name)?))?;
        self.position = end;
        Ok(range)
    }

    fn read_bytes(&mut self, size: usize, name: &str) -> io::Result<Vec<u8>> {
        let end = self
            .position
            .checked_add(size)
            .ok_or_else(|| invalid(format!("{name} range overflow")))?;
        if end > self.file_size {
            return Err(invalid(format!("truncated {name}")));
        }
        let mut bytes = vec![0u8; size];
        self.reader.read_exact(&mut bytes)?;
        self.position = end;
        Ok(bytes)
    }

    fn read_array<const N: usize>(&mut self, name: &str) -> io::Result<[u8; N]> {
        let end = self
            .position
            .checked_add(N)
            .ok_or_else(|| invalid(format!("{name} range overflow")))?;
        if end > self.file_size {
            return Err(invalid(format!("truncated {name}")));
        }
        let mut bytes = [0u8; N];
        self.reader.read_exact(&mut bytes)?;
        self.position = end;
        Ok(bytes)
    }

    fn read_range(&mut self, range: Range<usize>, name: &str) -> io::Result<Vec<u8>> {
        if range.start > range.end || range.end > self.file_size {
            return Err(invalid(format!("invalid {name} range")));
        }
        self.reader.seek(SeekFrom::Start(u64_from_usize(range.start, name)?))?;
        self.position = range.start;
        self.read_bytes(range.len(), name)
    }

    fn take_count(&mut self, count: usize, width: usize, name: &str) -> io::Result<Range<usize>> {
        let size = count
            .checked_mul(width)
            .ok_or_else(|| invalid(format!("{name} size overflow")))?;
        self.take(size, name)
    }

    fn read_u32(&mut self, name: &str) -> io::Result<u32> {
        Ok(u32::from_le_bytes(self.read_array(name)?))
    }

    fn take_leb128_payload(&mut self, name: &str) -> io::Result<Range<usize>> {
        let magic: [u8; 17] = self.read_array(name)?;
        if magic != LEB128_MAGIC {
            return Err(invalid(format!("{name} has invalid magic")));
        }
        let size = self.read_u32(name)? as usize;
        if size == 0 || size > MAX_COMPRESSED_SIZE {
            return Err(invalid(format!(
                "invalid {name} size: {size} (max: {MAX_COMPRESSED_SIZE})"
            )));
        }
        self.take(size, name)
    }

    fn take_leb128_prefix(&mut self, name: &str, prefix_count: usize) -> io::Result<Leb128Block> {
        let magic: [u8; 17] = self.read_array(name)?;
        if magic != LEB128_MAGIC {
            return Err(invalid("unsupported FT encoding: expected COMPRESSED_LEB128 magic"));
        }
        let size = self.read_u32(name)? as usize;
        if size == 0 || size > MAX_COMPRESSED_SIZE {
            return Err(invalid(format!(
                "invalid {name} size: {size} (max: {MAX_COMPRESSED_SIZE})"
            )));
        }
        let data_start = self.position;
        let data_end = data_start
            .checked_add(size)
            .ok_or_else(|| invalid(format!("{name} range overflow")))?;
        if data_end > self.file_size {
            return Err(invalid(format!("truncated {name}")));
        }
        if prefix_count > size {
            return Err(invalid(format!(
                "{name} cannot contain {prefix_count} values in {size} bytes"
            )));
        }
        let mut values = Vec::with_capacity(prefix_count);
        for _ in 0..prefix_count {
            values.push(self.read_leb128_i16(data_end)?);
        }
        let prefix_end = self.position;
        self.reader.seek(SeekFrom::Start(u64_from_usize(data_end, name)?))?;
        self.position = data_end;
        Ok(Leb128Block {
            data: data_start..data_end,
            prefix_end,
            values,
        })
    }

    fn read_leb128_i16(&mut self, block_end: usize) -> io::Result<i16> {
        let mut result = 0i64;
        let mut shift = 0u32;
        loop {
            if self.position >= block_end {
                return Err(invalid("truncated FT bias LEB128 value"));
            }
            let byte = self.read_array::<1>("FT bias LEB128 value")?[0];
            result |= i64::from(byte & 0x7f) << shift;
            shift += 7;
            if byte & 0x80 == 0 {
                if shift < 64 && byte & 0x40 != 0 {
                    result |= !0i64 << shift;
                }
                return i16::try_from(result)
                    .map_err(|_| invalid(format!("FT bias is outside i16 range: {result}")));
            }
            if shift >= 64 {
                return Err(invalid("FT bias LEB128 value overflow"));
            }
        }
    }
}

fn validate_canonical_ft_biases<R: Read + Seek>(
    input: &mut R,
    layout: &LayerStacksBinLayout,
) -> Result<(), NetBinPatchError> {
    let mut original = vec![0; layout.feature_transformer.biases.len()];
    input.seek(SeekFrom::Start(u64_from_usize(
        layout.feature_transformer.biases.start,
        "FT biases",
    )?))?;
    input.read_exact(&mut original)?;
    if original != encode_ft_biases(&layout.ft_biases) {
        return Err(invalid_binary("FT bias LEB128 payload is not canonical").into());
    }
    Ok(())
}

fn write_reencoded_ft_biases<R: Read + Seek, W: Write>(
    input: &mut R,
    output: &mut W,
    layout: &LayerStacksBinLayout,
) -> Result<usize, NetBinPatchError> {
    let encoded = encode_ft_biases(&layout.ft_biases);
    let size = match layout.feature_transformer.encoding {
        FtBinEncoding::Leb128Combined => encoded
            .len()
            .checked_add(layout.feature_transformer.weights.len())
            .ok_or_else(|| invalid("encoded FT block size overflow"))?,
        FtBinEncoding::Leb128Split => encoded.len(),
    };
    if size > MAX_COMPRESSED_SIZE {
        return Err(invalid(format!("encoded FT block exceeds maximum size: {size}")).into());
    }
    let size = u32::try_from(size).map_err(|_| invalid("encoded FT block exceeds u32 size"))?;
    let size_offset = layout
        .feature_transformer
        .biases
        .start
        .checked_sub(4)
        .ok_or_else(|| invalid_binary("FT block size field range"))?;
    copy_range(input, output, 0..size_offset)?;
    output.write_all(&size.to_le_bytes())?;
    output.write_all(&encoded)?;
    Ok(match layout.feature_transformer.encoding {
        FtBinEncoding::Leb128Combined => layout.feature_transformer.weights.start,
        FtBinEncoding::Leb128Split => layout.feature_transformer.biases.end,
    })
}

fn encode_ft_biases(values: &[i16]) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(values.len().saturating_mul(3));
    for &value in values {
        encode_signed_leb128(i64::from(value), &mut encoded);
    }
    encoded
}

fn copy_range<R: Read + Seek, W: Write>(
    input: &mut R,
    output: &mut W,
    range: Range<usize>,
) -> io::Result<()> {
    input.seek(SeekFrom::Start(u64_from_usize(range.start, "copy range")?))?;
    let mut remaining = range.len();
    // FT weights は GiB 級になり得るため、net サイズに依存しない固定長バッファで転送する
    // (heap 確保: 小さいスタックのスレッドから呼ばれても溢れない)。
    let mut buffer = vec![0u8; 1024 * 1024];
    while remaining != 0 {
        let chunk = remaining.min(buffer.len());
        input.read_exact(&mut buffer[..chunk])?;
        output.write_all(&buffer[..chunk])?;
        remaining -= chunk;
    }
    Ok(())
}

fn usize_from_u64(value: u64, name: &str) -> io::Result<usize> {
    usize::try_from(value).map_err(|_| invalid(format!("{name} exceeds usize")))
}

fn u64_from_usize(value: usize, name: &str) -> io::Result<u64> {
    u64::try_from(value).map_err(|_| invalid(format!("{name} offset exceeds u64")))
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn invalid_binary(message: impl Into<String>) -> NetDeltaError {
    NetDeltaError::InvalidBinary {
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "prepacked-nnue")]
    #[test]
    fn conversion_scan_never_reads_fc_payload() {
        struct RejectFcRead {
            inner: Cursor<Vec<u8>>,
            fc_start: u64,
        }
        impl Read for RejectFcRead {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                if !buffer.is_empty() && self.inner.position() >= self.fc_start {
                    return Err(io::Error::other("FC payload must not be read by conversion scan"));
                }
                self.inner.read(buffer)
            }
        }
        impl Seek for RejectFcRead {
            fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
                self.inner.seek(from)
            }
        }
        let model = build_synthetic_layer_stacks_with_ft_encoding(
            "HalfKP",
            <HalfKPFeatureSet as FeatureSetTrait>::DIMENSIONS,
            32,
            8,
            32,
            2,
            SyntheticFtEncoding::Leb128Split,
        );
        let reference = LayerStacksBinLayout::from_bytes(&model.bytes).unwrap();
        let mut reader = RejectFcRead {
            inner: Cursor::new(model.bytes),
            fc_start: reference.buckets[0].fc_hash.start as u64,
        };
        let scanned = LayerStacksBinLayout::scan_for_conversion(&mut reader).unwrap();
        assert_eq!(scanned.buckets, reference.buckets);
        assert_eq!(scanned.feature_transformer, reference.feature_transformer);
        assert!(scanned.fc_data.bytes.is_empty());
        reader.seek(SeekFrom::Start(0)).unwrap();
        assert!(LayerStacksBinLayout::from_reader(&mut reader).is_err());
    }
    #[cfg(feature = "nnue-runtime-dimensions")]
    use std::sync::Arc;

    use super::*;
    #[cfg(feature = "nnue-runtime-dimensions")]
    use crate::nnue::evaluator::NNUEEvaluator;
    use crate::nnue::features::{FeatureSet as FeatureSetTrait, HalfKPFeatureSet};
    use crate::nnue::net_delta::test_utils::{
        SyntheticFtConfig, SyntheticFtEncoding, SyntheticFtValues,
        build_synthetic_layer_stacks_with_ft_encoding, build_synthetic_layer_stacks_with_ft_values,
    };
    #[cfg(feature = "nnue-runtime-dimensions")]
    use crate::nnue::network::{
        LayerStackBucketMode, NNUENetwork, configure_layer_stack_routing,
        reset_layer_stack_progress_buckets, reset_layer_stack_progress_kpabs_weights,
    };
    #[cfg(feature = "nnue-runtime-dimensions")]
    use crate::position::{Position, SFEN_HIRATE};

    fn id(kind: NetTensorKind, bucket: Option<usize>, index: usize) -> NetCoefficientId {
        NetCoefficientId {
            kind,
            bucket,
            index,
        }
    }

    fn replace_combined_ft_payload(bytes: &[u8], payload: &[u8]) -> Vec<u8> {
        let layout = LayerStacksBinLayout::from_bytes(bytes).expect("layout");
        assert_eq!(layout.feature_transformer.encoding, FtBinEncoding::Leb128Combined);
        let original =
            layout.feature_transformer.biases.start..layout.feature_transformer.weights.end;
        let size_offset = original.start - 4;
        let size = u32::try_from(payload.len()).expect("payload size");
        let mut replaced = Vec::with_capacity(bytes.len() - original.len() + payload.len());
        replaced.extend_from_slice(&bytes[..size_offset]);
        replaced.extend_from_slice(&size.to_le_bytes());
        replaced.extend_from_slice(payload);
        replaced.extend_from_slice(&bytes[original.end..]);
        replaced
    }

    fn replace_architecture(bytes: &[u8], architecture: &str) -> Vec<u8> {
        let original_len = u32::from_le_bytes(bytes[8..12].try_into().expect("architecture size"));
        let original_end = 12 + usize::try_from(original_len).expect("architecture length");
        let replacement_len = u32::try_from(architecture.len()).expect("replacement length");
        let mut replaced = Vec::with_capacity(bytes.len() - original_end + 12 + architecture.len());
        replaced.extend_from_slice(&bytes[..8]);
        replaced.extend_from_slice(&replacement_len.to_le_bytes());
        replaced.extend_from_slice(architecture.as_bytes());
        replaced.extend_from_slice(&bytes[original_end..]);
        replaced
    }

    fn apply_deltas_to_vec(
        bytes: &[u8],
        deltas: &[NetDelta],
    ) -> Result<(Vec<u8>, NetDeltaReport), NetBinPatchError> {
        let mut input = Cursor::new(bytes);
        let mut output = Vec::new();
        let report = apply_deltas(&mut input, &mut output, deltas)?;
        Ok((output, report))
    }

    fn minimal_layer_stacks_with_ft_prefix(
        l1: usize,
        l2: usize,
        l3: usize,
        payload: &[u8],
    ) -> Vec<u8> {
        let architecture = format!(
            "Features=HalfKP[{}->{l1}x2],LayerStacks,l2={l2},l3={l3}",
            <HalfKPFeatureSet as FeatureSetTrait>::DIMENSIONS
        );
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&NNUE_VERSION_LAYERSTACK_NUM_BUCKETS.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&(architecture.len() as u32).to_le_bytes());
        bytes.extend_from_slice(architecture.as_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(LEB128_MAGIC);
        bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        bytes.extend_from_slice(payload);
        bytes
    }

    fn with_psqt_and_threat(bytes: &[u8]) -> Vec<u8> {
        let original = LayerStacksBinLayout::from_bytes(bytes).expect("original");
        let architecture = format!(
            "Features=HalfKP[{}->32x2],LayerStacks,l2=4,l3=3,PSQT=2,Threat=5,ThreatProfile=0",
            <HalfKPFeatureSet as FeatureSetTrait>::DIMENSIONS
        );
        let mut extended = Vec::new();
        extended.extend_from_slice(&original.version.to_le_bytes());
        extended.extend_from_slice(&original.network_hash.to_le_bytes());
        extended.extend_from_slice(&(architecture.len() as u32).to_le_bytes());
        extended.extend_from_slice(architecture.as_bytes());
        extended.extend_from_slice(&(original.num_buckets as u32).to_le_bytes());
        extended.extend_from_slice(
            &bytes[original.feature_transformer.hash.start..original.buckets[0].fc_hash.start],
        );
        extended.resize(extended.len() + original.num_buckets * 4, 0);
        extended
            .resize(extended.len() + original.ft_input_dimensions * original.num_buckets * 4, 0);
        extended.extend_from_slice(&0u32.to_le_bytes());
        extended.resize(extended.len() + 5 * original.l1, 0);
        extended.extend_from_slice(&bytes[original.buckets[0].fc_hash.start..]);
        extended
    }

    struct ReadAudit {
        cursor: Cursor<Vec<u8>>,
        reads: Vec<Range<usize>>,
    }

    impl Read for ReadAudit {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let start = usize_from_u64(self.cursor.position(), "audit position")?;
            let read = self.cursor.read(buffer)?;
            if read != 0 {
                self.reads.push(start..start + read);
            }
            Ok(read)
        }
    }

    impl Seek for ReadAudit {
        fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
            self.cursor.seek(position)
        }
    }

    #[test]
    #[cfg(feature = "nnue-runtime-dimensions")]
    fn layout_coefficients_match_dynamic_network_for_leb128() {
        for (num_buckets, encoding) in [
            (2, SyntheticFtEncoding::Leb128Combined),
            (3, SyntheticFtEncoding::Leb128Split),
        ] {
            let synthetic = build_synthetic_layer_stacks_with_ft_encoding(
                "HalfKP",
                <HalfKPFeatureSet as FeatureSetTrait>::DIMENSIONS,
                32,
                4,
                3,
                num_buckets,
                encoding,
            );
            let layout = LayerStacksBinLayout::from_bytes(&synthetic.bytes).expect("layout");
            let network = NNUENetwork::from_bytes(&synthetic.bytes).expect("dynamic network");
            assert_eq!(layout.num_buckets, num_buckets);
            assert_eq!(layout.l1, 32);
            assert_eq!(layout.l2, 4);
            assert_eq!(layout.l3, 3);
            assert_eq!(
                layout.ft_input_dimensions,
                <HalfKPFeatureSet as FeatureSetTrait>::DIMENSIONS
            );

            let cases = [
                id(NetTensorKind::OutputWeight, Some(0), 0),
                id(NetTensorKind::OutputWeight, Some(num_buckets - 1), 31),
                id(NetTensorKind::OutputBias, Some(0), 0),
                id(NetTensorKind::OutputBias, Some(num_buckets - 1), 0),
                id(NetTensorKind::FtBias, None, 0),
                id(NetTensorKind::FtBias, None, 17),
                id(NetTensorKind::L2Weight, Some(0), 2),
                id(NetTensorKind::L2Weight, Some(num_buckets - 1), 95),
            ];
            for coefficient in cases {
                assert_eq!(
                    layout.coefficient(&coefficient).expect("layout coefficient"),
                    network.net_coefficient(&coefficient).expect("network coefficient"),
                    "{coefficient:?}, encoding={encoding:?}"
                );
            }
        }
    }

    #[test]
    fn rejects_non_leb128_ft_encoding() {
        let mut synthetic = build_synthetic_layer_stacks_with_ft_encoding(
            "HalfKP",
            <HalfKPFeatureSet as FeatureSetTrait>::DIMENSIONS,
            32,
            4,
            3,
            2,
            SyntheticFtEncoding::Leb128Split,
        );
        let original = LayerStacksBinLayout::from_bytes(&synthetic.bytes).expect("original");
        synthetic.bytes[original.feature_transformer.hash.end] = 0;

        let error = LayerStacksBinLayout::from_bytes(&synthetic.bytes).expect_err("unsupported FT");
        assert!(error.to_string().contains("unsupported FT encoding"));

        #[cfg(feature = "nnue-runtime-dimensions")]
        {
            let error = match NNUENetwork::from_bytes(&synthetic.bytes) {
                Ok(_) => panic!("unsupported FT was accepted"),
                Err(error) => error,
            };
            assert_eq!(error.to_string(), "Expected COMPRESSED_LEB128 magic");
        }
    }

    #[test]
    fn layout_skips_ft_weight_payload() {
        for encoding in [
            SyntheticFtEncoding::Leb128Combined,
            SyntheticFtEncoding::Leb128Split,
        ] {
            let mut synthetic = build_synthetic_layer_stacks_with_ft_encoding(
                "HalfKP",
                <HalfKPFeatureSet as FeatureSetTrait>::DIMENSIONS,
                32,
                4,
                3,
                2,
                encoding,
            );
            let original = LayerStacksBinLayout::from_bytes(&synthetic.bytes).expect("original");
            let original_bias = original
                .coefficient(&id(NetTensorKind::FtBias, None, 0))
                .expect("original FT bias");
            synthetic.bytes[original.feature_transformer.weights.clone()].fill(0x80);

            let mut reader = ReadAudit {
                cursor: Cursor::new(synthetic.bytes),
                reads: Vec::new(),
            };
            let layout = LayerStacksBinLayout::from_reader(&mut reader).expect("layout");
            assert_eq!(layout.feature_transformer.weights, original.feature_transformer.weights);
            assert!(reader.reads.iter().all(|read| {
                read.end <= layout.feature_transformer.weights.start
                    || read.start >= layout.feature_transformer.weights.end
            }));
            assert_eq!(
                layout.coefficient(&id(NetTensorKind::FtBias, None, 0)).expect("FT bias"),
                original_bias
            );
        }
    }

    #[test]
    fn ft_delta_preserves_invalid_weight_payload_verbatim() {
        for encoding in [
            SyntheticFtEncoding::Leb128Combined,
            SyntheticFtEncoding::Leb128Split,
        ] {
            let mut synthetic = build_synthetic_layer_stacks_with_ft_encoding(
                "HalfKP",
                <HalfKPFeatureSet as FeatureSetTrait>::DIMENSIONS,
                32,
                4,
                3,
                2,
                encoding,
            );
            let input_layout =
                LayerStacksBinLayout::from_bytes(&synthetic.bytes).expect("input layout");
            synthetic.bytes[input_layout.feature_transformer.weights.clone()].fill(0x80);
            let (patched, report) = apply_deltas_to_vec(
                &synthetic.bytes,
                &[NetDelta {
                    id: id(NetTensorKind::FtBias, None, 0),
                    delta: 120,
                }],
            )
            .expect("patch without decoding FT weights");
            let output_layout = LayerStacksBinLayout::from_bytes(&patched).expect("output layout");
            assert_ne!(
                output_layout.feature_transformer.weights.start,
                input_layout.feature_transformer.weights.start,
                "{encoding:?}"
            );
            assert_eq!(
                &patched[output_layout.feature_transformer.weights],
                &synthetic.bytes[input_layout.feature_transformer.weights],
                "{encoding:?}"
            );
            assert_eq!(report.applied, 1);
        }
    }

    #[test]
    fn e4_alias_layout_coefficient_and_apply_match_dynamic_reader() {
        let input_dimensions =
            crate::nnue::bona_piece_effect_bucket::EffectBucketConfig::KINGFIXED_2X2.dimensions();
        let synthetic = build_synthetic_layer_stacks_with_ft_encoding(
            "HalfKaHmMerged",
            input_dimensions,
            2,
            2,
            1,
            2,
            SyntheticFtEncoding::Leb128Split,
        );
        let architecture =
            format!("Features=HalfKaHmMerged[{input_dimensions}->2x2],E4=2x2fixed,l2=2,l3=1");
        let bytes = replace_architecture(&synthetic.bytes, &architecture);
        let id = id(NetTensorKind::FtBias, None, 0);
        let input_layout = LayerStacksBinLayout::from_bytes(&bytes).expect("E4 input layout");
        let before = input_layout.coefficient(&id).expect("input coefficient");
        let (patched, report) = apply_deltas_to_vec(
            &bytes,
            &[NetDelta {
                id: id.clone(),
                delta: 1,
            }],
        )
        .expect("apply E4 delta");
        let output_layout = LayerStacksBinLayout::from_bytes(&patched).expect("E4 output layout");
        assert_eq!(output_layout.coefficient(&id).expect("output coefficient"), before + 1);
        assert_eq!(report.applied, 1);

        #[cfg(feature = "nnue-runtime-dimensions")]
        {
            let network = NNUENetwork::from_bytes(&patched).expect("dynamic E4 network");
            assert_eq!(network.net_coefficient(&id).expect("dynamic coefficient"), before + 1);
        }
    }

    #[test]
    fn retained_data_is_limited_to_ft_biases_and_fc_blocks() {
        let synthetic = build_synthetic_layer_stacks_with_ft_encoding(
            "HalfKP",
            <HalfKPFeatureSet as FeatureSetTrait>::DIMENSIONS,
            32,
            4,
            3,
            3,
            SyntheticFtEncoding::Leb128Combined,
        );
        let mut reader = Cursor::new(&synthetic.bytes);
        let from_reader = LayerStacksBinLayout::from_reader(&mut reader).expect("reader layout");
        let from_bytes = LayerStacksBinLayout::from_bytes(&synthetic.bytes).expect("bytes layout");

        assert_eq!(from_reader, from_bytes);
        assert_eq!(from_reader.ft_biases.len(), from_reader.l1);
        assert_eq!(
            from_reader.fc_data.bytes.len(),
            from_reader.buckets.last().expect("last bucket").output.weights.end
                - from_reader.buckets[0].fc_hash.start
        );
        assert!(
            from_reader.fc_data.bytes.len()
                + from_reader.ft_biases.len() * std::mem::size_of::<i16>()
                < from_reader.feature_transformer.weights.len()
        );
    }

    #[test]
    fn rejects_non_layer_stacks_and_trailing_data() {
        let mut non_layer_stacks = Vec::new();
        let arch = "Features=HalfKP[125388->32x2],l2=4,l3=3";
        non_layer_stacks.extend_from_slice(&NNUE_VERSION_HALFKA.to_le_bytes());
        non_layer_stacks.extend_from_slice(&0u32.to_le_bytes());
        non_layer_stacks.extend_from_slice(&(arch.len() as u32).to_le_bytes());
        non_layer_stacks.extend_from_slice(arch.as_bytes());
        assert!(LayerStacksBinLayout::from_bytes(&non_layer_stacks).is_err());

        let mut synthetic = build_synthetic_layer_stacks_with_ft_encoding(
            "HalfKP",
            <HalfKPFeatureSet as FeatureSetTrait>::DIMENSIONS,
            32,
            4,
            3,
            2,
            SyntheticFtEncoding::Leb128Split,
        )
        .bytes;
        synthetic.push(0);
        let error = LayerStacksBinLayout::from_bytes(&synthetic).expect_err("trailing byte");
        assert!(error.to_string().contains("trailing"));
    }

    #[test]
    fn rejects_loader_invalid_architecture_headers() {
        let synthetic = build_synthetic_layer_stacks_with_ft_encoding(
            "HalfKP",
            <HalfKPFeatureSet as FeatureSetTrait>::DIMENSIONS,
            32,
            4,
            3,
            2,
            SyntheticFtEncoding::Leb128Split,
        );
        for suffix in [",Threat=0", ",Factorizer"] {
            let original = LayerStacksBinLayout::from_bytes(&synthetic.bytes).expect("original");
            let architecture = format!("{}{suffix}", original.architecture);
            let bytes = replace_architecture(&synthetic.bytes, &architecture);
            assert!(LayerStacksBinLayout::from_bytes(&bytes).is_err(), "{suffix}");
        }
    }

    #[test]
    fn rejects_gigantic_l1_without_panicking() {
        let bytes = minimal_layer_stacks_with_ft_prefix(usize::MAX - 1, 2, 1, &[0]);
        let result = std::panic::catch_unwind(move || {
            let mut reader = Cursor::new(bytes);
            LayerStacksBinLayout::from_reader(&mut reader)
        })
        .expect("invalid header must not panic");
        let error = result.expect_err("gigantic l1");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn rejects_dimensions_above_runtime_loader_limit() {
        for (l1, l2, l3) in [(16_386, 2, 1), (32, 16_385, 1), (32, 2, 16_385)] {
            let bytes = minimal_layer_stacks_with_ft_prefix(l1, l2, l3, &[0]);
            let error = LayerStacksBinLayout::from_bytes(&bytes).expect_err("oversized dimension");
            assert_eq!(error.kind(), io::ErrorKind::InvalidData, "{l1}x{l2}x{l3}");
        }
    }

    #[test]
    fn rejects_ft_bias_prefix_larger_than_compressed_payload() {
        let bytes = minimal_layer_stacks_with_ft_prefix(32, 4, 3, &[0]);
        let error = LayerStacksBinLayout::from_bytes(&bytes).expect_err("short FT bias payload");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("cannot contain 32 values in 1 bytes"));
    }

    #[test]
    fn layout_skips_psqt_and_threat_payloads() {
        let synthetic = build_synthetic_layer_stacks_with_ft_encoding(
            "HalfKP",
            <HalfKPFeatureSet as FeatureSetTrait>::DIMENSIONS,
            32,
            4,
            3,
            2,
            SyntheticFtEncoding::Leb128Split,
        );
        let bytes = with_psqt_and_threat(&synthetic.bytes);
        let expected = LayerStacksBinLayout::from_bytes(&bytes).expect("expected layout");
        let mut reader = ReadAudit {
            cursor: Cursor::new(bytes),
            reads: Vec::new(),
        };
        let actual = LayerStacksBinLayout::from_reader(&mut reader).expect("audited layout");
        assert_eq!(actual, expected);

        let psqt = actual.psqt.expect("PSQT");
        let skipped = [
            psqt.biases,
            psqt.weights,
            actual.threat_weights.expect("threat weights"),
        ];
        for payload in skipped {
            assert!(
                reader
                    .reads
                    .iter()
                    .all(|read| read.end <= payload.start || read.start >= payload.end),
                "payload was read: {payload:?}"
            );
        }
    }

    #[test]
    fn locates_optional_psqt_and_threat_blocks() {
        let synthetic = build_synthetic_layer_stacks_with_ft_encoding(
            "HalfKP",
            <HalfKPFeatureSet as FeatureSetTrait>::DIMENSIONS,
            32,
            4,
            3,
            2,
            SyntheticFtEncoding::Leb128Split,
        );
        let bytes = with_psqt_and_threat(&synthetic.bytes);

        let layout = LayerStacksBinLayout::from_bytes(&bytes).expect("extended layout");
        let psqt = layout.psqt.expect("PSQT");
        assert_eq!(psqt.biases.len(), layout.num_buckets * 4);
        assert_eq!(psqt.weights.len(), layout.ft_input_dimensions * layout.num_buckets * 4);
        assert_eq!(layout.threat_profile.expect("profile").len(), 4);
        assert_eq!(layout.threat_weights.expect("threat").len(), 5 * layout.l1);
    }

    #[test]
    fn combined_leb128_accepts_canonical_signed_i16_boundaries() {
        let synthetic = build_synthetic_layer_stacks_with_ft_values(
            "HalfKP",
            <HalfKPFeatureSet as FeatureSetTrait>::DIMENSIONS,
            14,
            2,
            1,
            2,
            SyntheticFtConfig {
                encoding: SyntheticFtEncoding::Leb128Combined,
                values: SyntheticFtValues::SignedBoundaries,
            },
        );
        let layout = LayerStacksBinLayout::from_bytes(&synthetic.bytes).expect("layout");
        for value in [-1, 64, -65, -8192, 8191, i16::MIN, i16::MAX] {
            assert!(layout.ft_biases.contains(&value));
        }
        let (patched, report) = apply_deltas_to_vec(
            &synthetic.bytes,
            &[NetDelta {
                id: id(NetTensorKind::FtBias, None, 0),
                delta: 1,
            }],
        )
        .expect("apply canonical FT delta");
        assert_ne!(patched, synthetic.bytes);
        assert_eq!(report.applied, 1);
    }

    #[test]
    fn combined_leb128_rejects_non_canonical_and_out_of_i16_values() {
        let synthetic = build_synthetic_layer_stacks_with_ft_encoding(
            "HalfKP",
            <HalfKPFeatureSet as FeatureSetTrait>::DIMENSIONS,
            2,
            2,
            1,
            2,
            SyntheticFtEncoding::Leb128Combined,
        );
        let layout = LayerStacksBinLayout::from_bytes(&synthetic.bytes).expect("layout");
        let original = &synthetic.bytes
            [layout.feature_transformer.biases.start..layout.feature_transformer.weights.end];

        let mut non_canonical_payload = vec![0x88, 0x00];
        non_canonical_payload.extend_from_slice(&original[1..]);
        let non_canonical = replace_combined_ft_payload(&synthetic.bytes, &non_canonical_payload);
        let error = apply_deltas_to_vec(
            &non_canonical,
            &[NetDelta {
                id: id(NetTensorKind::FtBias, None, 0),
                delta: 1,
            }],
        )
        .expect_err("non-canonical payload");
        assert!(error.to_string().contains("not canonical"));

        for invalid_value in [i64::from(i16::MIN) - 1, i64::from(i16::MAX) + 1] {
            let mut invalid_prefix = Vec::new();
            encode_signed_leb128(invalid_value, &mut invalid_prefix);
            invalid_prefix.extend_from_slice(&original[1..]);
            let invalid = replace_combined_ft_payload(&synthetic.bytes, &invalid_prefix);
            let error = apply_deltas_to_vec(
                &invalid,
                &[NetDelta {
                    id: id(NetTensorKind::FtBias, None, 0),
                    delta: 1,
                }],
            )
            .expect_err("out-of-range payload");
            assert!(error.to_string().contains("outside i16 range"));
        }
    }

    #[test]
    fn zero_deltas_preserve_every_byte_and_non_ft_regions_stay_verbatim() {
        let synthetic = build_synthetic_layer_stacks_with_ft_encoding(
            "HalfKP",
            <HalfKPFeatureSet as FeatureSetTrait>::DIMENSIONS,
            32,
            4,
            3,
            2,
            SyntheticFtEncoding::Leb128Split,
        );
        let zero = NetDelta {
            id: id(NetTensorKind::FtBias, None, 0),
            delta: 0,
        };
        let (empty, empty_report) = apply_deltas_to_vec(&synthetic.bytes, &[]).expect("empty");
        let (unchanged, zero_report) =
            apply_deltas_to_vec(&synthetic.bytes, &[zero]).expect("zero");
        assert_eq!(empty, synthetic.bytes);
        assert_eq!(unchanged, synthetic.bytes);
        assert_eq!(empty_report.applied, 0);
        assert_eq!(zero_report.applied, 1);

        let layout = LayerStacksBinLayout::from_bytes(&synthetic.bytes).expect("layout");
        let delta = NetDelta {
            id: id(NetTensorKind::FtBias, None, 0),
            delta: 64,
        };
        let (patched, _) = apply_deltas_to_vec(&synthetic.bytes, &[delta]).expect("patch");
        let patched_layout = LayerStacksBinLayout::from_bytes(&patched).expect("patched layout");
        assert_eq!(
            &patched[..layout.feature_transformer.hash.end],
            &synthetic.bytes[..layout.feature_transformer.hash.end]
        );
        assert_eq!(patched_layout.version, layout.version);
        assert_eq!(patched_layout.architecture, layout.architecture);
        assert_eq!(
            patched_layout.feature_transformer.encoding,
            layout.feature_transformer.encoding
        );
        assert_eq!(
            &patched[patched_layout.feature_transformer.weights.start..],
            &synthetic.bytes[layout.feature_transformer.weights.start..]
        );
    }

    #[test]
    fn zero_ft_delta_mixed_with_output_delta_preserves_combined_ft_block() {
        let synthetic = build_synthetic_layer_stacks_with_ft_values(
            "HalfKP",
            <HalfKPFeatureSet as FeatureSetTrait>::DIMENSIONS,
            2,
            2,
            1,
            2,
            SyntheticFtConfig {
                encoding: SyntheticFtEncoding::Leb128Combined,
                values: SyntheticFtValues::SignedBoundaries,
            },
        );
        let input_layout =
            LayerStacksBinLayout::from_bytes(&synthetic.bytes).expect("input layout");
        let deltas = [
            NetDelta {
                id: id(NetTensorKind::FtBias, None, 0),
                delta: 0,
            },
            NetDelta {
                id: id(NetTensorKind::OutputBias, Some(0), 0),
                delta: 1,
            },
        ];
        let (patched, report) = apply_deltas_to_vec(&synthetic.bytes, &deltas).expect("patch");
        let output_layout = LayerStacksBinLayout::from_bytes(&patched).expect("output layout");
        let input_ft =
            input_layout.feature_transformer.hash.end..input_layout.feature_transformer.weights.end;
        let output_ft = output_layout.feature_transformer.hash.end
            ..output_layout.feature_transformer.weights.end;
        assert_eq!(&patched[output_ft], &synthetic.bytes[input_ft]);
        assert_eq!(report.applied, 2);
        assert_eq!(report.clamped, 0);
    }

    #[test]
    fn byte_patching_uses_storage_type_saturation() {
        let synthetic = build_synthetic_layer_stacks_with_ft_encoding(
            "HalfKP",
            <HalfKPFeatureSet as FeatureSetTrait>::DIMENSIONS,
            32,
            4,
            3,
            2,
            SyntheticFtEncoding::Leb128Combined,
        );
        let deltas = [
            NetDelta {
                id: id(NetTensorKind::OutputWeight, Some(0), 0),
                delta: i32::MAX,
            },
            NetDelta {
                id: id(NetTensorKind::OutputBias, Some(0), 0),
                delta: i32::MAX,
            },
            NetDelta {
                id: id(NetTensorKind::FtBias, None, 0),
                delta: i32::MIN,
            },
        ];
        let (patched, report) = apply_deltas_to_vec(&synthetic.bytes, &deltas).expect("patch");
        let layout = LayerStacksBinLayout::from_bytes(&patched).expect("layout");
        assert_eq!(report.applied, 3);
        assert_eq!(report.clamped, 3);
        assert_eq!(layout.coefficient(&deltas[0].id).expect("i8"), i8::MAX.into());
        assert_eq!(layout.coefficient(&deltas[1].id).expect("i32"), i32::MAX);
        assert_eq!(layout.coefficient(&deltas[2].id).expect("i16"), i16::MIN.into());
    }

    #[cfg(feature = "nnue-runtime-dimensions")]
    fn evaluate(bytes: &[u8], deltas: &[NetDelta], buckets: usize) -> i32 {
        let mut network = NNUENetwork::from_bytes(bytes).expect("network");
        network.apply_net_deltas(deltas).expect("deltas");
        configure_layer_stack_routing(LayerStackBucketMode::ProgressKPAbs, buckets, Some(buckets))
            .expect("routing");
        let mut position = Position::new();
        position.set_sfen(SFEN_HIRATE).expect("hirate");
        let mut evaluator = NNUEEvaluator::new_with_position(Arc::new(network), &position);
        evaluator.evaluate(&position).raw()
    }

    #[cfg(feature = "nnue-runtime-dimensions")]
    #[test]
    fn patched_net_evaluation_matches_runtime_deltas_for_both_leb128_forms() {
        let routing_guard = crate::nnue::network::layer_stack_routing_test_guard();
        reset_layer_stack_progress_kpabs_weights();
        for (buckets, encoding) in [
            (4, SyntheticFtEncoding::Leb128Combined),
            (9, SyntheticFtEncoding::Leb128Split),
        ] {
            let synthetic = build_synthetic_layer_stacks_with_ft_encoding(
                "HalfKP",
                <HalfKPFeatureSet as FeatureSetTrait>::DIMENSIONS,
                32,
                4,
                2,
                buckets,
                encoding,
            );
            let selected_bucket = buckets / 2;
            let deltas = [
                NetDelta {
                    id: id(NetTensorKind::OutputWeight, Some(selected_bucket), 0),
                    delta: 64,
                },
                NetDelta {
                    id: id(NetTensorKind::OutputBias, Some(selected_bucket), 0),
                    delta: 256,
                },
                NetDelta {
                    id: id(NetTensorKind::FtBias, None, 0),
                    delta: 48,
                },
                NetDelta {
                    id: id(NetTensorKind::L2Weight, Some(selected_bucket), 3),
                    delta: 64,
                },
            ];
            let baseline = evaluate(&synthetic.bytes, &[], buckets);
            let runtime = evaluate(&synthetic.bytes, &deltas, buckets);
            let (patched, report) = apply_deltas_to_vec(&synthetic.bytes, &deltas).expect("patch");
            let from_file = evaluate(&patched, &[], buckets);
            assert_ne!(runtime, baseline, "{encoding:?}");
            assert_eq!(from_file, runtime, "{encoding:?}");
            assert_eq!(report.applied, deltas.len());
            assert_eq!(report.clamped, 0);
        }
        reset_layer_stack_progress_buckets();
        reset_layer_stack_progress_kpabs_weights();

        drop(routing_guard);
    }
}
