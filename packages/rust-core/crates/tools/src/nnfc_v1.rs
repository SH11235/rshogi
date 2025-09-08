use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};

pub const MAGIC: &[u8; 4] = b"NNFC";
pub const CACHE_VERSION_V1: u32 = 1;
pub const HEADER_SIZE_V1: u32 = 48;
pub const FEATURE_SET_ID_HALF: u32 = 0x4841_4C46; // "HALF"

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadEncoding {
    None = 0,
    Gzip = 1,
    Zstd = 2,
}

impl PayloadEncoding {
    pub fn code(self) -> u8 {
        self as u8
    }
    pub fn from_code(b: u8) -> Option<Self> {
        match b {
            0 => Some(PayloadEncoding::None),
            1 => Some(PayloadEncoding::Gzip),
            2 => Some(PayloadEncoding::Zstd),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct HeaderV1 {
    pub version: u32,
    pub feature_set_id: u32,
    pub num_samples: u64,
    pub chunk_size: u32,
    pub header_size: u32,
    pub endianness: u8,
    pub payload_encoding: PayloadEncoding,
    pub payload_offset: u64,
    pub flags_mask: u32,
}

pub fn write_header_v1_at(f: &mut File, header_pos: u64, h: &HeaderV1) -> io::Result<()> {
    f.seek(SeekFrom::Start(header_pos))?;
    f.write_all(&h.version.to_le_bytes())?;
    f.write_all(&h.feature_set_id.to_le_bytes())?;
    f.write_all(&h.num_samples.to_le_bytes())?;
    f.write_all(&h.chunk_size.to_le_bytes())?;
    f.write_all(&h.header_size.to_le_bytes())?;
    f.write_all(&[h.endianness])?;
    f.write_all(&[h.payload_encoding.code()])?;
    f.write_all(&[0u8; 2])?; // reserved16
    f.write_all(&h.payload_offset.to_le_bytes())?;
    f.write_all(&h.flags_mask.to_le_bytes())?;
    // pad to HEADER_SIZE_V1
    let written = 40usize; // bytes after magic
    let tail = (h.header_size as usize).saturating_sub(written);
    if tail > 0 {
        f.write_all(&vec![0u8; tail])?;
    }
    Ok(())
}

pub fn read_header_v1(f: &mut File) -> io::Result<HeaderV1> {
    // read magic
    let mut magic = [0u8; 4];
    f.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "Invalid cache file: bad magic"));
    }
    let mut u32b = [0u8; 4];
    let mut u64b = [0u8; 8];

    // version
    f.read_exact(&mut u32b)?;
    let version = u32::from_le_bytes(u32b);
    if version != CACHE_VERSION_V1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Unsupported cache version: {} (v1 required)", version),
        ));
    }

    // feature_set_id
    f.read_exact(&mut u32b)?;
    let feature_set_id = u32::from_le_bytes(u32b);

    // num_samples, chunk_size, header_size
    f.read_exact(&mut u64b)?;
    let num_samples = u64::from_le_bytes(u64b);
    f.read_exact(&mut u32b)?;
    let chunk_size = u32::from_le_bytes(u32b);
    f.read_exact(&mut u32b)?;
    let header_size = u32::from_le_bytes(u32b);
    if !(40..=4096).contains(&header_size) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Unreasonable header_size: {}", header_size),
        ));
    }
    // endianness
    let mut b = [0u8; 1];
    f.read_exact(&mut b)?;
    if b[0] != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Unsupported endianness (expected LE)",
        ));
    }
    let endianness = b[0];

    // payload_encoding
    f.read_exact(&mut b)?;
    let payload_encoding = PayloadEncoding::from_code(b[0])
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Unknown payload encoding"))?;
    // reserved16
    let mut _r16 = [0u8; 2];
    f.read_exact(&mut _r16)?;

    // payload_offset
    f.read_exact(&mut u64b)?;
    let payload_offset = u64::from_le_bytes(u64b);
    let header_end = 4u64 + header_size as u64;
    if payload_offset < header_end {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "payload_offset ({}) is smaller than header end ({})",
                payload_offset, header_end
            ),
        ));
    }

    // flags mask
    f.read_exact(&mut u32b)?;
    let flags_mask = u32::from_le_bytes(u32b);

    Ok(HeaderV1 {
        version,
        feature_set_id,
        num_samples,
        chunk_size,
        header_size,
        endianness,
        payload_encoding,
        payload_offset,
        flags_mask,
    })
}

pub type PayloadReader = (BufReader<Box<dyn Read>>, HeaderV1);

#[allow(clippy::type_complexity)]
pub fn open_payload_reader(path: &str) -> Result<PayloadReader, Box<dyn std::error::Error>> {
    let mut f = File::open(path)?;
    let header = read_header_v1(&mut f)?;
    if header.feature_set_id != FEATURE_SET_ID_HALF {
        return Err(format!(
            "Unsupported feature_set_id: 0x{:08x} for file {}",
            header.feature_set_id, path
        )
        .into());
    }
    // seek to payload
    let current = f.stream_position()?;
    if current < header.payload_offset {
        f.seek(SeekFrom::Start(header.payload_offset))?;
    }
    // wrap reader by encoding
    let inner: Box<dyn Read> = match header.payload_encoding {
        PayloadEncoding::None => Box::new(f),
        PayloadEncoding::Gzip => {
            use flate2::read::MultiGzDecoder;
            Box::new(MultiGzDecoder::new(f))
        }
        PayloadEncoding::Zstd => {
            #[cfg(feature = "zstd")]
            {
                Box::new(zstd::Decoder::new(f)?)
            }
            #[cfg(not(feature = "zstd"))]
            {
                return Err("zstd payload requires building with 'zstd' feature".into());
            }
        }
    };
    Ok((BufReader::new(inner), header))
}
