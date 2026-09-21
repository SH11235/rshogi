//! LayerStacks の事前展開コンテナ。FT/PSQT/Threat は64B整列した LE 配列。
//!
//! 後段の FC は各層の shape / layout ID を検証し、並べ替え済み重みを直接読む。
//! version 2 は metadata stream / i16 FT / i32 PSQT / i8 Threat の4区画。
//! metadata stream は通常ヘッダー、raw FT biases、PSQT biases、Threat profile、FC。
//! 重みの shape と extension は既存のモデルローダーが検証する。
use super::accumulator::AlignedBox;
use super::net_bin_layout::LayerStacksBinLayout;
use sha2::{Digest, Sha256};
use std::cell::Cell;
use std::fs::{File, OpenOptions};
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::ops::Range;
use std::path::Path;
#[cfg(windows)]
use std::sync::Arc;

const MAGIC: &[u8; 16] = b"RSHOGI-PACKED-02";
const HEADER_SIZE: usize = 256;
// 0..16 magic, 16..20 version, 20..24 reserved,
// 24..88 offset/length pairs, 88..120 source SHA256, 120..152 container SHA256.
const DIGEST_RANGE: Range<usize> = 120..152;

/// 変換先 affine kernel の重み配置。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FcLayout {
    /// universal edition 用の行順。
    RowMajor,
    /// この変換プログラムと同じ ISA でビルドした static edition 用。
    Native,
}

fn write_fc<R: Read + Seek, W: Write + Seek>(
    source: &mut R,
    target: &mut W,
    tensor: &super::net_bin_layout::TensorBinLayout,
    input: usize,
    output: usize,
    layout: FcLayout,
) -> io::Result<()> {
    copy_range(source, target, tensor.biases.clone())?;
    let scramble = layout == FcLayout::Native && super::layers::uses_scrambled_weights(output);
    let length = tensor.weights.len();
    for value in [
        u32::from(scramble),
        u32::try_from(input).map_err(|_| invalid("FC input overflow"))?,
        u32::try_from(output).map_err(|_| invalid("FC output overflow"))?,
        u32::try_from(length).map_err(|_| invalid("FC length overflow"))?,
    ] {
        target.write_all(&value.to_le_bytes())?;
    }
    let position =
        usize::try_from(target.stream_position()?).map_err(|_| invalid("FC offset overflow"))?;
    target.write_all(&[0; 64][..aligned(position)? - position])?;
    if !scramble {
        return copy_range(source, target, tensor.weights.clone());
    }
    // FC一層だけを保持する。FTのGiB級配列はここへ持ち込まない。
    let mut weights = vec![0; length];
    source.seek(SeekFrom::Start(tensor.weights.start as u64))?;
    source.read_exact(&mut weights)?;
    let padded = super::layers::padded_input(input);
    let mut buffer = [0; 65536];
    let mut used = 0;
    for chunk in 0..padded / 4 {
        for row in 0..output {
            buffer[used..used + 4]
                .copy_from_slice(&weights[row * padded + chunk * 4..row * padded + chunk * 4 + 4]);
            used += 4;
            if used == buffer.len() {
                target.write_all(&buffer)?;
                used = 0;
            }
        }
    }
    target.write_all(&buffer[..used])
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn aligned(value: usize) -> io::Result<usize> {
    value
        .checked_add(63)
        .map(|n| n & !63)
        .ok_or_else(|| invalid("section offset overflow"))
}
fn source_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1);
    }
    options.open(path)
}
fn copy_range<R: Read + Seek, W: Write>(
    reader: &mut R,
    output: &mut W,
    range: Range<usize>,
) -> io::Result<()> {
    reader.seek(SeekFrom::Start(range.start as u64))?;
    let len = range.len() as u64;
    if io::copy(&mut reader.take(len), output)? != len {
        return Err(invalid("truncated section"));
    }
    Ok(())
}
fn hash_file<R: Read + Seek>(reader: &mut R) -> io::Result<[u8; 32]> {
    reader.seek(SeekFrom::Start(0))?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(hash.finalize().into())
}
fn decode_i16<R: Read + Seek, W: Write>(
    reader: &mut R,
    output: &mut W,
    range: Range<usize>,
    count: usize,
) -> io::Result<()> {
    reader.seek(SeekFrom::Start(range.start as u64))?;
    let mut limited = reader.take(range.len() as u64);
    let mut block = [0u8; 65536];
    let mut used = 0;
    for _ in 0..count {
        let value = super::leb128::read_signed_leb128(&mut limited)?;
        // 通常デコーダーと同じ i16 への変換。読み出す要素数と終端は厳密に検証する。
        block[used..used + 2].copy_from_slice(&(value as i16).to_le_bytes());
        used += 2;
        if used == block.len() {
            output.write_all(&block)?;
            used = 0;
        }
    }
    if limited.limit() != 0 {
        return Err(invalid("unexpected extra FT values"));
    }
    output.write_all(&block[..used])
}

/// LayerStacks `.bin` を事前展開する。出力が存在する場合は上書きしない。
///
/// FT は固定長バッファで変換し、展開後の全配列をヒープに保持しない。
/// 完成時にヘッダーとチェックサムを確定する。失敗時には未完成ファイルが残る場合がある。
/// 入力ファイルは変換中に変更しないこと（Windows では書込共有も拒否する）。
pub fn pack_layer_stacks(input: impl AsRef<Path>, output: impl AsRef<Path>) -> io::Result<()> {
    pack_layer_stacks_with_layout(input, output, FcLayout::RowMajor)
}

/// 指定した affine kernel 配置で変換する。配置が異なるエンジンではロードを拒否する。
pub fn pack_layer_stacks_with_layout(
    input: impl AsRef<Path>,
    output: impl AsRef<Path>,
    fc_layout: FcLayout,
) -> io::Result<()> {
    let mut source = BufReader::new(source_file(input.as_ref())?);
    let source_hash = hash_file(&mut source)?;
    source.seek(SeekFrom::Start(0))?;
    let layout = LayerStacksBinLayout::scan_for_conversion(&mut source)?;
    let mut target = OpenOptions::new().read(true).write(true).create_new(true).open(output)?;
    target.write_all(&[0; HEADER_SIZE])?;
    let mut ranges: [Range<usize>; 4] = std::array::from_fn(|_| 0..0);
    ranges[0].start = HEADER_SIZE;
    copy_range(&mut source, &mut target, 0..layout.feature_transformer.hash.end)?;
    decode_i16(&mut source, &mut target, layout.feature_transformer.biases.clone(), layout.l1)?;
    if let Some(psqt) = &layout.psqt {
        copy_range(&mut source, &mut target, psqt.biases.clone())?;
    }
    if let Some(profile) = &layout.threat_profile {
        copy_range(&mut source, &mut target, profile.clone())?;
    }
    for bucket in &layout.buckets {
        copy_range(&mut source, &mut target, bucket.fc_hash.clone())?;
        write_fc(&mut source, &mut target, &bucket.l1, layout.l1, layout.l2, fc_layout)?;
        write_fc(&mut source, &mut target, &bucket.l2, 2 * (layout.l2 - 1), layout.l3, fc_layout)?;
        write_fc(&mut source, &mut target, &bucket.output, layout.l3, 1, fc_layout)?;
    }
    ranges[0].end =
        usize::try_from(target.stream_position()?).map_err(|_| invalid("file too large"))?;
    for (section, range) in ranges.iter_mut().enumerate().skip(1) {
        let start = aligned(
            usize::try_from(target.stream_position()?).map_err(|_| invalid("file too large"))?,
        )?;
        let position = target.stream_position()? as usize;
        target.write_all(&[0; 64][..start - position])?;
        match section {
            1 => decode_i16(
                &mut source,
                &mut target,
                layout.feature_transformer.weights.clone(),
                layout
                    .ft_input_dimensions
                    .checked_mul(layout.l1)
                    .ok_or_else(|| invalid("FT dimensions overflow"))?,
            )?,
            2 => {
                if let Some(psqt) = &layout.psqt {
                    copy_range(&mut source, &mut target, psqt.weights.clone())?;
                }
            }
            3 => {
                if let Some(threat) = &layout.threat_weights {
                    copy_range(&mut source, &mut target, threat.clone())?;
                }
            }
            _ => unreachable!(),
        }
        *range = start
            ..usize::try_from(target.stream_position()?).map_err(|_| invalid("file too large"))?;
    }
    if hash_file(&mut source)? != source_hash {
        return Err(invalid("source changed during conversion"));
    }
    let mut header = [0u8; HEADER_SIZE];
    header[..16].copy_from_slice(MAGIC);
    header[16..20].copy_from_slice(&2u32.to_le_bytes());
    for (i, range) in ranges.iter().enumerate() {
        header[24 + i * 16..32 + i * 16].copy_from_slice(&(range.start as u64).to_le_bytes());
        header[32 + i * 16..40 + i * 16].copy_from_slice(&(range.len() as u64).to_le_bytes());
    }
    header[88..120].copy_from_slice(&source_hash);
    target.seek(SeekFrom::Start(0))?;
    target.write_all(&header)?;
    let digest = hash_file(&mut target)?;
    target.seek(SeekFrom::Start(DIGEST_RANGE.start as u64))?;
    target.write_all(&digest)?;
    target.sync_all()
}

#[cfg(windows)]
type MetadataReader<'a> = io::Cursor<&'a [u8]>;
#[cfg(not(windows))]
type MetadataReader<'a> = BufReader<SectionReader<'a>>;

#[cfg(any(test, not(windows)))]
pub(super) struct SectionReader<'a> {
    file: &'a std::cell::RefCell<File>,
    range: Range<usize>,
    position: usize,
}
#[cfg(any(test, not(windows)))]
impl Read for SectionReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let count = buffer.len().min(self.range.len().saturating_sub(self.position));
        if count == 0 {
            return Ok(0);
        }
        // tensor の読み込みも同じ File を使うため、各 read 前に論理位置へ戻す。
        let mut file = self.file.borrow_mut();
        file.seek(SeekFrom::Start((self.range.start + self.position) as u64))?;
        let n = file.read(&mut buffer[..count])?;
        self.position += n;
        Ok(n)
    }
}
#[cfg(any(test, not(windows)))]
impl Seek for SectionReader<'_> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let next = match from {
            SeekFrom::Start(n) => i128::from(n),
            SeekFrom::End(n) => self.range.len() as i128 + i128::from(n),
            SeekFrom::Current(n) => self.position as i128 + i128::from(n),
        };
        if next < 0 || next > self.range.len() as i128 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek outside metadata section",
            ));
        }
        self.position = next as usize;
        Ok(self.position as u64)
    }
}

pub(super) struct PackedModel {
    #[cfg(windows)]
    owner: Arc<super::mapped_weights::ReadOnlyMapping>,
    #[cfg(not(windows))]
    file: std::cell::RefCell<File>,
    ranges: [Range<usize>; 4],
    used: Cell<u8>,
}
impl PackedModel {
    pub(super) fn fc<R: Read + Seek>(
        &self,
        reader: &mut R,
        input: usize,
        output: usize,
        scramble: bool,
    ) -> io::Result<AlignedBox<i8>> {
        let expected = output
            .checked_mul(super::layers::padded_input(input))
            .ok_or_else(|| invalid("FC dimensions overflow"))?;
        let mut descriptor = [0u8; 16];
        reader.read_exact(&mut descriptor)?;
        let fields: [u32; 4] = std::array::from_fn(|i| {
            u32::from_le_bytes(descriptor[i * 4..i * 4 + 4].try_into().unwrap())
        });
        if fields[0] != u32::from(scramble)
            || fields[1] as usize != input
            || fields[2] as usize != output
            || fields[3] as usize != expected
        {
            return Err(invalid(
                "prepacked FC layout or shape mismatch; reconvert for the target edition/ISA",
            ));
        }
        let relative = usize::try_from(reader.stream_position()?)
            .map_err(|_| invalid("FC offset overflow"))?;
        let position = self.ranges[0]
            .start
            .checked_add(relative)
            .ok_or_else(|| invalid("FC offset overflow"))?;
        let start = aligned(position)?;
        let end = start.checked_add(expected).ok_or_else(|| invalid("FC length overflow"))?;
        if expected == 0 || end > self.ranges[0].end {
            return Err(invalid("FC outside metadata section"));
        }
        let mut padding = [0u8; 64];
        reader.read_exact(&mut padding[..start - position])?;
        if padding.iter().any(|b| *b != 0) {
            return Err(invalid("nonzero FC padding"));
        }
        #[cfg(windows)]
        let weights = AlignedBox::from_mapped(self.owner.clone(), start, expected)?;
        #[cfg(not(windows))]
        let weights = {
            let mut weights = AlignedBox::new_zeroed(expected);
            for value in weights.iter_mut() {
                let mut b = [0];
                reader.read_exact(&mut b)?;
                *value = b[0] as i8;
            }
            weights
        };
        reader.seek(SeekFrom::Start((end - self.ranges[0].start) as u64))?;
        Ok(weights)
    }
    pub(super) fn is_packed<R: Read + Seek>(reader: &mut R) -> io::Result<bool> {
        let position = reader.stream_position()?;
        let mut magic = [0u8; 16];
        let result = match reader.read_exact(&mut magic) {
            Ok(()) => Ok(&magic == MAGIC),
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(false),
            Err(e) => Err(e),
        };
        reader.seek(SeekFrom::Start(position))?;
        result
    }
    pub(super) fn open(path: &Path) -> io::Result<Self> {
        #[cfg(windows)]
        let owner = Arc::new(super::mapped_weights::ReadOnlyMapping::open(path)?);
        #[cfg(windows)]
        let mut reader = io::Cursor::new(owner.bytes());
        #[cfg(not(windows))]
        let mut reader = source_file(path)?;
        let len = usize::try_from(reader.seek(SeekFrom::End(0))?)
            .map_err(|_| invalid("file too large"))?;
        reader.seek(SeekFrom::Start(0))?;
        let mut header = [0u8; HEADER_SIZE];
        reader.read_exact(&mut header)?;
        let ranges = parse_header(&header, len)?;
        let expected = header[DIGEST_RANGE].to_vec();
        header[DIGEST_RANGE].fill(0);
        let mut hash = Sha256::new();
        hash.update(header);
        let mut buffer = [0u8; 65536];
        loop {
            let n = reader.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            hash.update(&buffer[..n]);
        }
        if hash.finalize().as_slice() != expected {
            return Err(invalid("prepacked checksum mismatch"));
        }
        Ok(Self {
            #[cfg(windows)]
            owner,
            #[cfg(not(windows))]
            file: std::cell::RefCell::new(reader),
            ranges,
            used: Cell::new(0),
        })
    }
    pub(super) fn metadata(&self) -> io::Result<MetadataReader<'_>> {
        let range = self.ranges[0].clone();
        #[cfg(windows)]
        {
            Ok(io::Cursor::new(&self.owner.bytes()[range]))
        }
        #[cfg(not(windows))]
        {
            Ok(BufReader::new(SectionReader {
                file: &self.file,
                range,
                position: 0,
            }))
        }
    }
    fn span(&self, section: usize, width: usize, count: usize) -> io::Result<Range<usize>> {
        let range = self.ranges[section].clone();
        if count == 0 || count.checked_mul(width) != Some(range.len()) {
            return Err(invalid("prepacked tensor shape mismatch"));
        }
        self.used.set(self.used.get() | (1 << section));
        Ok(range)
    }
    pub(super) fn finish(&self) -> io::Result<()> {
        for section in 1..4 {
            if !self.ranges[section].is_empty() && self.used.get() & (1 << section) == 0 {
                return Err(invalid("unused prepacked tensor"));
            }
        }
        Ok(())
    }
}
macro_rules! read_tensor {
    ($name:ident, $ty:ty, $section:expr) => {
        impl PackedModel {
            pub(super) fn $name(&self, count: usize) -> io::Result<AlignedBox<$ty>> {
                let range = self.span($section, std::mem::size_of::<$ty>(), count)?;
                #[cfg(windows)]
                {
                    AlignedBox::from_mapped(self.owner.clone(), range.start, count)
                }
                #[cfg(not(windows))]
                {
                    let mut result = AlignedBox::new_zeroed(count);
                    let mut file = self.file.borrow_mut();
                    file.seek(SeekFrom::Start(range.start as u64))?;
                    let mut reader = BufReader::new(&mut *file);
                    for value in result.iter_mut() {
                        let mut bytes = [0; std::mem::size_of::<$ty>()];
                        reader.read_exact(&mut bytes)?;
                        *value = <$ty>::from_le_bytes(bytes);
                    }
                    Ok(result)
                }
            }
        }
    };
}
read_tensor!(ft, i16, 1);
#[cfg(any(test, feature = "nnue-runtime-dimensions", feature = "nnue-psqt"))]
read_tensor!(psqt, i32, 2);
#[cfg(any(test, feature = "nnue-runtime-dimensions", feature = "nnue-threat"))]
read_tensor!(threat, i8, 3);
fn parse_header(header: &[u8; HEADER_SIZE], file_len: usize) -> io::Result<[Range<usize>; 4]> {
    if &header[..16] != MAGIC
        || header[16..20] != 2u32.to_le_bytes()
        || header[20..24].iter().chain(header[152..].iter()).any(|b| *b != 0)
    {
        return Err(invalid("invalid prepacked header or version"));
    }
    let mut ranges = std::array::from_fn(|_| 0..0);
    let mut end = HEADER_SIZE;
    for (i, range) in ranges.iter_mut().enumerate() {
        let offset = u64::from_le_bytes(header[24 + i * 16..32 + i * 16].try_into().unwrap());
        let length = u64::from_le_bytes(header[32 + i * 16..40 + i * 16].try_into().unwrap());
        let offset = usize::try_from(offset).map_err(|_| invalid("section offset overflow"))?;
        let length = usize::try_from(length).map_err(|_| invalid("section length overflow"))?;
        if offset != aligned(end)? {
            return Err(invalid("noncanonical section offset"));
        }
        end = offset.checked_add(length).ok_or_else(|| invalid("section length overflow"))?;
        if end > file_len {
            return Err(invalid("truncated prepacked file"));
        }
        *range = offset..end;
    }
    if end != file_len || ranges[0].is_empty() || ranges[1].is_empty() {
        return Err(invalid("invalid prepacked file length"));
    }
    Ok(ranges)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nnue::constants::HALFKP_DIMENSIONS;
    use crate::nnue::net_delta::test_utils::{
        SyntheticFtConfig, SyntheticFtEncoding, SyntheticFtValues,
        build_synthetic_layer_stacks_with_ft_values,
    };
    pub(super) struct Fixture {
        pub(super) source: std::path::PathBuf,
        pub(super) packed: std::path::PathBuf,
    }
    impl Fixture {
        pub(super) fn new(encoding: SyntheticFtEncoding) -> Self {
            Self::with_layout(encoding, FcLayout::RowMajor)
        }
        pub(super) fn with_layout(encoding: SyntheticFtEncoding, layout: FcLayout) -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let base = std::env::temp_dir().join(format!(
                "rshogi-packed-{}-{nonce}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            let source = base.with_extension("bin");
            let packed = base.with_extension("packed");
            let synthetic = build_synthetic_layer_stacks_with_ft_values(
                "HalfKP",
                HALFKP_DIMENSIONS,
                32,
                8,
                32,
                2,
                SyntheticFtConfig {
                    encoding,
                    values: SyntheticFtValues::SignedBoundaries,
                },
            );
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&source)
                .unwrap()
                .write_all(&synthetic.bytes)
                .unwrap();
            pack_layer_stacks_with_layout(&source, &packed, layout).unwrap();
            Self { source, packed }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_file(&self.source).unwrap();
            std::fs::remove_file(&self.packed).unwrap();
        }
    }
    #[test]
    fn both_encodings_preserve_all_static_tensors() {
        use crate::nnue::ls_feature_spec::HalfKpSpec;
        use crate::nnue::network_layer_stacks::NetworkLayerStacks;
        type Net = NetworkLayerStacks<32, 8, 14, 32, HalfKpSpec>;
        for encoding in [
            SyntheticFtEncoding::Leb128Combined,
            SyntheticFtEncoding::Leb128Split,
        ] {
            let fixture = Fixture::with_layout(encoding, FcLayout::Native);
            let original = Net::load(&fixture.source).unwrap();
            let mut packed = Net::load(&fixture.packed).unwrap();
            assert_eq!(original.fv_scale, packed.fv_scale);
            assert_eq!(original.num_buckets, packed.num_buckets);
            assert_eq!(original.feature_transformer.biases.0, packed.feature_transformer.biases.0);
            assert_eq!(
                &*original.feature_transformer.weights,
                &*packed.feature_transformer.weights
            );
            for (a, b) in original.layer_stacks.buckets.iter().zip(&packed.layer_stacks.buckets) {
                assert_eq!(a.l1.biases, b.l1.biases);
                assert_eq!(&*a.l1.weights, &*b.l1.weights);
                assert_eq!(a.l2.biases, b.l2.biases);
                assert_eq!(&*a.l2.weights, &*b.l2.weights);
                assert_eq!(a.output.biases, b.output.biases);
                assert_eq!(&*a.output.weights, &*b.output.weights);
            }
            let peer = Net::load(&fixture.packed).unwrap();
            let before = peer.layer_stacks.buckets[0].l2.file_weight(0);
            packed.layer_stacks.buckets[0].l2.apply_file_weight_delta(0, 1);
            assert_eq!(packed.layer_stacks.buckets[0].l2.file_weight(0), before.saturating_add(1));
            assert_eq!(peer.layer_stacks.buckets[0].l2.file_weight(0), before);
            drop(peer);
            #[cfg(windows)]
            assert!(OpenOptions::new().write(true).open(&fixture.packed).is_err());
            drop(packed);
            assert!(OpenOptions::new().write(true).open(&fixture.packed).is_ok());
        }
    }
    #[cfg(feature = "nnue-runtime-dimensions")]
    #[test]
    fn full_loader_preserves_dynamic_evaluation_and_deltas() {
        use crate::nnue::evaluator::NNUEEvaluator;
        use crate::nnue::net_delta::{NetCoefficientId, NetDelta, NetTensorKind};
        use crate::nnue::network::{
            LayerStackBucketMode, NNUENetwork, configure_layer_stack_routing,
            layer_stack_routing_test_guard,
        };
        use crate::position::{Position, SFEN_HIRATE};
        let _guard = layer_stack_routing_test_guard();
        configure_layer_stack_routing(LayerStackBucketMode::ProgressKPAbs, 2, Some(2)).unwrap();
        let fixture = Fixture::new(SyntheticFtEncoding::Leb128Split);
        let mut original = NNUENetwork::load(&fixture.source).unwrap();
        let mut packed = NNUENetwork::load(&fixture.packed).unwrap();
        let deltas = [
            NetDelta {
                id: NetCoefficientId {
                    kind: NetTensorKind::FtBias,
                    bucket: None,
                    index: 0,
                },
                delta: 1,
            },
            NetDelta {
                id: NetCoefficientId {
                    kind: NetTensorKind::L2Weight,
                    bucket: Some(0),
                    index: 0,
                },
                delta: 1,
            },
            NetDelta {
                id: NetCoefficientId {
                    kind: NetTensorKind::OutputWeight,
                    bucket: Some(0),
                    index: 0,
                },
                delta: -1,
            },
        ];
        assert_eq!(original.apply_net_deltas(&deltas).unwrap().applied, 3);
        assert_eq!(packed.apply_net_deltas(&deltas).unwrap().applied, 3);
        let mut position = Position::new();
        position.set_sfen(SFEN_HIRATE).unwrap();
        let mut a = NNUEEvaluator::new_with_position(std::sync::Arc::new(original), &position);
        let mut b = NNUEEvaluator::new_with_position(std::sync::Arc::new(packed), &position);
        assert_eq!(a.evaluate(&position), b.evaluate(&position));
    }
    #[test]
    fn container_rejects_corruption_shapes_and_unconsumed_sections() {
        let fixture = Fixture::new(SyntheticFtEncoding::Leb128Combined);
        let repeated = fixture.packed.with_extension("repeated");
        pack_layer_stacks(&fixture.source, &repeated).unwrap();
        let first_digest = hash_file(&mut File::open(&fixture.packed).unwrap()).unwrap();
        let second_digest = hash_file(&mut File::open(&repeated).unwrap()).unwrap();
        std::fs::remove_file(repeated).unwrap();
        assert_eq!(first_digest, second_digest);
        {
            let packed = PackedModel::open(&fixture.packed).unwrap();
            assert!(packed.finish().is_err());
            assert!(packed.ft(1).is_err());
            assert!(packed.psqt(1).is_err());
            assert!(packed.threat(1).is_err());
            let _weights = packed.ft(HALFKP_DIMENSIONS * 32).unwrap();
            packed.finish().unwrap();
        }
        assert_eq!(
            pack_layer_stacks(&fixture.source, &fixture.packed).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        let mut file = OpenOptions::new().write(true).open(&fixture.packed).unwrap();
        file.seek(SeekFrom::End(-1)).unwrap();
        file.write_all(&[127]).unwrap();
        drop(file);
        assert!(PackedModel::open(&fixture.packed).is_err());
    }
    #[test]
    fn header_rejects_overlap_overflow_truncation_and_unknown_version() {
        let mut header = [0u8; HEADER_SIZE];
        header[..16].copy_from_slice(MAGIC);
        header[16..20].copy_from_slice(&2u32.to_le_bytes());
        for (i, (offset, len)) in
            [(256u64, 64u64), (320, 64), (384, 0), (384, 0)].into_iter().enumerate()
        {
            header[24 + i * 16..32 + i * 16].copy_from_slice(&offset.to_le_bytes());
            header[32 + i * 16..40 + i * 16].copy_from_slice(&len.to_le_bytes());
        }
        assert!(parse_header(&header, 384).is_ok());
        assert!(parse_header(&header, 383).is_err());
        assert!(parse_header(&header, 385).is_err());
        for (offset, value) in [(40, 256u64), (40, 321), (48, u64::MAX)] {
            let mut changed = header;
            changed[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
            assert!(parse_header(&changed, 384).is_err());
        }
        header[16] = 3;
        assert!(parse_header(&header, 384).is_err());
    }

    #[test]
    fn fc_descriptor_rejects_wrong_layout_shape_and_length() {
        let fixture = Fixture::new(SyntheticFtEncoding::Leb128Combined);
        let model = PackedModel::open(&fixture.packed).unwrap();
        let layout = LayerStacksBinLayout::from_reader(&mut BufReader::new(
            File::open(&fixture.source).unwrap(),
        ))
        .unwrap();
        let descriptor_position = layout.feature_transformer.hash.end + 32 * 2 + 4 + 8 * 4;
        let mut metadata = Vec::new();
        model.metadata().unwrap().read_to_end(&mut metadata).unwrap();
        let mut reader = io::Cursor::new(metadata.clone());
        reader.set_position(descriptor_position as u64);
        assert!(model.fc(&mut reader, 32, 8, false).is_ok());
        for (field, value) in [(0, 2u32), (1, 64), (2, 16), (3, 0), (3, u32::MAX)] {
            let mut bytes = metadata.clone();
            let start = descriptor_position + field * 4;
            bytes[start..start + 4].copy_from_slice(&value.to_le_bytes());
            let mut reader = io::Cursor::new(bytes);
            reader.set_position(descriptor_position as u64);
            assert!(model.fc(&mut reader, 32, 8, false).is_err());
        }
        let mut reader = io::Cursor::new(metadata);
        reader.set_position(descriptor_position as u64);
        assert!(model.fc(&mut reader, 32, 8, true).is_err());
    }

    #[test]
    fn bounded_metadata_reader_handles_interleaved_tensor_reads() {
        let fixture = Fixture::new(SyntheticFtEncoding::Leb128Split);
        let mut file = File::open(&fixture.packed).unwrap();
        let mut expected = [0u8; 32];
        file.seek(SeekFrom::Start(88)).unwrap();
        file.read_exact(&mut expected).unwrap();
        let file = std::cell::RefCell::new(file);
        let section = SectionReader {
            file: &file,
            range: 88..120,
            position: 0,
        };
        let mut reader = BufReader::with_capacity(3, section);
        let mut result = [0; 32];
        reader.read_exact(&mut result[..7]).unwrap();
        file.borrow_mut().seek(SeekFrom::Start(0)).unwrap();
        reader.read_exact(&mut result[7..]).unwrap();
        assert_eq!(result, expected);
        assert_eq!(reader.read(&mut result[..1]).unwrap(), 0);
        reader.seek(SeekFrom::End(-3)).unwrap();
        reader.read_exact(&mut result[..3]).unwrap();
        assert_eq!(&result[..3], &expected[29..]);
        assert!(reader.seek(SeekFrom::Start(33)).is_err());
        assert!(reader.seek(SeekFrom::Start(0)).is_ok());
        assert!(reader.seek(SeekFrom::Current(-1)).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn metadata_borrows_mapping_without_private_copy() {
        let fixture = Fixture::new(SyntheticFtEncoding::Leb128Split);
        let model = PackedModel::open(&fixture.packed).unwrap();
        let reader = model.metadata().unwrap();
        assert_eq!(
            reader.get_ref().as_ptr(),
            model.owner.bytes()[model.ranges[0].clone()].as_ptr()
        );
        assert_eq!(reader.get_ref().len(), model.ranges[0].len());
    }
}

#[cfg(all(test, feature = "nnue-psqt", feature = "nnue-threat"))]
mod extension_tests {
    use super::*;
    use crate::nnue::constants::HALFKP_DIMENSIONS;
    use crate::nnue::ls_feature_spec::HalfKpSpec;
    use crate::nnue::net_delta::test_utils::{SyntheticFtEncoding, build_synthetic_layer_stacks};
    use crate::nnue::network_layer_stacks::NetworkLayerStacks;
    #[test]
    fn packed_psqt_and_threat_match_standard_loader() {
        type Net = NetworkLayerStacks<32, 8, 14, 32, HalfKpSpec>;
        let fixture = super::tests::Fixture::new(SyntheticFtEncoding::Leb128Combined);
        let base = build_synthetic_layer_stacks("HalfKP", HALFKP_DIMENSIONS, 32, 8, 32, 2).bytes;
        let layout = LayerStacksBinLayout::from_bytes(&base).unwrap();
        let threat_dims = crate::nnue::threat_features::THREAT_DIMENSIONS;
        let profile = crate::nnue::threat_exclusion::THREAT_PROFILE_ID;
        let arch =
            format!("{},PSQT=2,Threat={threat_dims},ThreatProfile={profile}", layout.architecture);
        let old_header_end = 12 + layout.architecture.len();
        let mut bytes = base[..8].to_vec();
        bytes.extend_from_slice(&(arch.len() as u32).to_le_bytes());
        bytes.extend_from_slice(arch.as_bytes());
        bytes.extend_from_slice(&base[old_header_end..layout.feature_transformer.weights.end]);
        for value in [-1234i32, 5678] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        for index in 0..HALFKP_DIMENSIONS * 2 {
            bytes.extend_from_slice(&((index as i32 % 257) - 128).to_le_bytes());
        }
        bytes.extend_from_slice(&profile.to_le_bytes());
        bytes.extend((0..threat_dims * 32).map(|index| ((index % 255) as i16 - 127) as u8));
        bytes.extend_from_slice(&base[layout.feature_transformer.weights.end..]);
        std::fs::write(&fixture.source, &bytes).unwrap();
        std::fs::remove_file(&fixture.packed).unwrap();
        pack_layer_stacks_with_layout(&fixture.source, &fixture.packed, FcLayout::Native).unwrap();
        let original = Net::load(&fixture.source).unwrap();
        let packed = Net::load(&fixture.packed).unwrap();
        assert_eq!(
            original.feature_transformer.psqt_biases(),
            packed.feature_transformer.psqt_biases()
        );
        assert_eq!(
            original.feature_transformer.psqt_weights(),
            packed.feature_transformer.psqt_weights()
        );
        assert_eq!(
            &*original.feature_transformer.threat_weights,
            &*packed.feature_transformer.threat_weights
        );
    }
}
