use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;

#[cfg(feature = "zstd")]
use zstd::stream::read::Decoder as ZstdDecoder;

pub fn open_reader<P: AsRef<Path>>(path: P) -> io::Result<Box<dyn BufRead>> {
    let p = path.as_ref();
    if p.to_string_lossy() == "-" {
        return Ok(Box::new(BufReader::new(io::stdin())));
    }
    let f = File::open(p)?;
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();

    if ext == "gz" {
        let dec = flate2::read::GzDecoder::new(f);
        return Ok(Box::new(BufReader::new(dec)));
    }
    #[cfg(feature = "zstd")]
    if ext == "zst" {
        let dec = ZstdDecoder::new(f)?;
        return Ok(Box::new(BufReader::new(dec)));
    }
    Ok(Box::new(BufReader::new(f)))
}

pub fn open_writer<P: AsRef<Path>>(path: P) -> io::Result<Box<dyn Write>> {
    let p = path.as_ref();
    if p.to_string_lossy() == "-" {
        return Ok(Box::new(std::io::stdout()));
    }
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
    if ext == "gz" {
        let f = File::create(p)?;
        let enc = flate2::write::GzEncoder::new(f, flate2::Compression::default());
        return Ok(Box::new(enc));
    }
    #[cfg(feature = "zstd")]
    if ext == "zst" {
        let f = File::create(p)?;
        let enc = zstd::stream::write::Encoder::new(f, 0)?; // default level
        return Ok(Box::new(enc.auto_finish()));
    }
    let f = File::create(p)?;
    Ok(Box::new(f))
}
