use std::fs::File;
use std::io::{self, BufRead, BufReader, Read};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextCompressionKind {
    Plain,
    Gzip,
    Zstd,
}

fn sniff_magic(path: &str) -> io::Result<TextCompressionKind> {
    // Read first 4 bytes
    let mut f = File::open(path)?;
    let mut magic = [0u8; 4];
    let n = f.read(&mut magic)?;
    if n >= 2 && magic[0] == 0x1F && magic[1] == 0x8B {
        return Ok(TextCompressionKind::Gzip);
    }
    if n >= 4 && magic == [0x28, 0xB5, 0x2F, 0xFD] {
        return Ok(TextCompressionKind::Zstd);
    }
    Ok(TextCompressionKind::Plain)
}

pub fn open_maybe_compressed_reader(
    path: &str,
    buf_bytes: usize,
) -> Result<Box<dyn BufRead>, Box<dyn std::error::Error>> {
    // Prefer magic; fall back to extension when inconclusive
    let magic_kind = sniff_magic(path)?;
    let kind = match magic_kind {
        TextCompressionKind::Plain => {
            if path.ends_with(".gz") {
                TextCompressionKind::Gzip
            } else if path.ends_with(".zst") {
                TextCompressionKind::Zstd
            } else {
                TextCompressionKind::Plain
            }
        }
        other => other,
    };

    let file = File::open(path)?;
    let reader: Box<dyn Read> = match kind {
        TextCompressionKind::Plain => Box::new(file),
        TextCompressionKind::Gzip => {
            use flate2::read::MultiGzDecoder;
            Box::new(MultiGzDecoder::new(file))
        }
        TextCompressionKind::Zstd => {
            #[cfg(feature = "zstd")]
            {
                Box::new(zstd::Decoder::new(file)?)
            }
            #[cfg(not(feature = "zstd"))]
            {
                return Err("zst input requires building 'tools' with feature 'zstd'".into());
            }
        }
    };
    Ok(Box::new(BufReader::with_capacity(buf_bytes, reader)))
}
