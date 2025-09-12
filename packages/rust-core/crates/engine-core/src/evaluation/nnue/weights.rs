//! NNUE weight file management
//!
//! Handles loading and parsing of NNUE weight files

use super::features::{FeatureTransformer, FE_END};
use super::network::Network;
use crate::shogi::SHOGI_BOARD_SIZE;
use std::error::Error;
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

/// Supported NNUE format versions
const MIN_SUPPORTED_VERSION: u32 = 1;
const MAX_SUPPORTED_VERSION: u32 = 1;

/// Maximum reasonable file size (200MB)
const MAX_FILE_SIZE: u32 = 200 * 1024 * 1024;

/// Expected weight sizes for validation
const EXPECTED_FT_WEIGHTS: usize = SHOGI_BOARD_SIZE * FE_END * 256; // Feature transformer weights
const EXPECTED_FT_BIASES: usize = 256; // Feature transformer biases
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

impl WeightReader {
    /// Create reader from file path
    pub fn from_file(path: &str) -> Result<Self, Box<dyn Error>> {
        let file = File::open(path)?;
        Ok(WeightReader { file })
    }

    /// Read header and validate
    pub fn read_header(&mut self) -> Result<NNUEHeader, Box<dyn Error>> {
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
            return Err("Invalid NNUE file magic".into());
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
            return Err(format!(
                "Unsupported NNUE version: {}, supported range: {}-{}",
                header.version, MIN_SUPPORTED_VERSION, MAX_SUPPORTED_VERSION
            )
            .into());
        }

        // Validate file size
        if header.size > MAX_FILE_SIZE {
            return Err(format!(
                "NNUE file too large: {} bytes, maximum: {} bytes",
                header.size, MAX_FILE_SIZE
            )
            .into());
        }

        // Validate architecture
        let arch = header.architecture; // Copy to avoid unaligned access
        if arch != HALFKP_256X2_32_32 {
            return Err(format!(
                "Unsupported architecture: 0x{arch:08X}, expected 0x{HALFKP_256X2_32_32:08X}"
            )
            .into());
        }

        Ok(header)
    }

    /// Read weights of type T
    pub fn read_weights<T: Copy + Default>(
        &mut self,
        count: usize,
    ) -> Result<Vec<T>, Box<dyn Error>> {
        let size = count * mem::size_of::<T>();
        let mut buffer = vec![0u8; size];
        self.file.read_exact(&mut buffer)?;

        let mut result = vec![T::default(); count];
        unsafe {
            std::ptr::copy_nonoverlapping(buffer.as_ptr() as *const T, result.as_mut_ptr(), count);
        }

        Ok(result)
    }
}

/// Load NNUE weights from file
pub fn load_weights(path: &str) -> Result<(FeatureTransformer, Network), Box<dyn Error>> {
    let mut reader = WeightReader::from_file(path)?;
    let _header = reader.read_header()?;

    // Read feature transformer weights
    let ft_weights = reader.read_weights::<i16>(EXPECTED_FT_WEIGHTS)?;
    if ft_weights.len() != EXPECTED_FT_WEIGHTS {
        return Err(format!(
            "Feature transformer weights dimension mismatch: expected {}, got {}",
            EXPECTED_FT_WEIGHTS,
            ft_weights.len()
        )
        .into());
    }

    let ft_biases = reader.read_weights::<i32>(EXPECTED_FT_BIASES)?;
    if ft_biases.len() != EXPECTED_FT_BIASES {
        return Err(format!(
            "Feature transformer biases dimension mismatch: expected {}, got {}",
            EXPECTED_FT_BIASES,
            ft_biases.len()
        )
        .into());
    }

    // Read hidden layer 1
    let hidden1_weights = reader.read_weights::<i8>(EXPECTED_H1_WEIGHTS)?;
    if hidden1_weights.len() != EXPECTED_H1_WEIGHTS {
        return Err(format!(
            "Hidden layer 1 weights dimension mismatch: expected {}, got {}",
            EXPECTED_H1_WEIGHTS,
            hidden1_weights.len()
        )
        .into());
    }

    let hidden1_biases = reader.read_weights::<i32>(EXPECTED_H1_BIASES)?;
    if hidden1_biases.len() != EXPECTED_H1_BIASES {
        return Err(format!(
            "Hidden layer 1 biases dimension mismatch: expected {}, got {}",
            EXPECTED_H1_BIASES,
            hidden1_biases.len()
        )
        .into());
    }

    // Read hidden layer 2
    let hidden2_weights = reader.read_weights::<i8>(EXPECTED_H2_WEIGHTS)?;
    if hidden2_weights.len() != EXPECTED_H2_WEIGHTS {
        return Err(format!(
            "Hidden layer 2 weights dimension mismatch: expected {}, got {}",
            EXPECTED_H2_WEIGHTS,
            hidden2_weights.len()
        )
        .into());
    }

    let hidden2_biases = reader.read_weights::<i32>(EXPECTED_H2_BIASES)?;
    if hidden2_biases.len() != EXPECTED_H2_BIASES {
        return Err(format!(
            "Hidden layer 2 biases dimension mismatch: expected {}, got {}",
            EXPECTED_H2_BIASES,
            hidden2_biases.len()
        )
        .into());
    }

    // Read output layer
    let output_weights = reader.read_weights::<i8>(EXPECTED_OUT_WEIGHTS)?;
    if output_weights.len() != EXPECTED_OUT_WEIGHTS {
        return Err(format!(
            "Output layer weights dimension mismatch: expected {}, got {}",
            EXPECTED_OUT_WEIGHTS,
            output_weights.len()
        )
        .into());
    }

    let output_bias_vec = reader.read_weights::<i32>(EXPECTED_OUT_BIASES)?;
    if output_bias_vec.len() != EXPECTED_OUT_BIASES {
        return Err(format!(
            "Output layer bias dimension mismatch: expected {}, got {}",
            EXPECTED_OUT_BIASES,
            output_bias_vec.len()
        )
        .into());
    }
    let output_bias = output_bias_vec[0];

    // Create structures
    let feature_transformer = FeatureTransformer {
        weights: ft_weights,
        biases: ft_biases,
    };

    let network = Network {
        hidden1_weights,
        hidden1_biases,
        hidden2_weights,
        hidden2_biases,
        output_weights,
        output_bias,
    };

    Ok((feature_transformer, network))
}

/// Save NNUE weights to file (for testing)
#[cfg(test)]
pub fn save_weights(
    path: &str,
    transformer: &FeatureTransformer,
    network: &Network,
) -> Result<(), Box<dyn Error>> {
    use std::io::{Seek, SeekFrom, Write};

    let mut file = File::create(path)?;

    // Write header
    let header = NNUEHeader {
        magic: *b"NNUE",
        version: 1,
        architecture: HALFKP_256X2_32_32,
        size: 0, // Will be updated later
    };

    let header_bytes: [u8; mem::size_of::<NNUEHeader>()] = unsafe { mem::transmute_copy(&header) };
    file.write_all(&header_bytes)?;

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
        std::fs::write(path, b"BADMAGIC").unwrap();

        let result = load_weights(path);
        assert!(result.is_err());

        std::fs::remove_file(path).ok();
    }
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

    fn read_f32_vec(
        r: &mut std::io::Cursor<&[u8]>,
        n: usize,
    ) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
        let mut out = vec![0f32; n];
        // Safe: transmute bytes for exact size
        let bytes = unsafe { std::slice::from_raw_parts_mut(out.as_mut_ptr() as *mut u8, n * 4) };
        r.read_exact(bytes)?;
        Ok(out)
    }

    let w0 = read_f32_vec(&mut rdr, input_dim * acc_dim)?;
    let b0 = if has_b0 {
        Some(read_f32_vec(&mut rdr, acc_dim)?)
    } else {
        None
    };
    let w2 = read_f32_vec(&mut rdr, acc_dim)?;
    rdr.read_exact(&mut u4)?;
    let b2 = f32::from_le_bytes(u4);

    Ok(super::single::SingleChannelNet {
        n_feat: input_dim,
        acc_dim,
        scale: 600.0,
        w0,
        b0,
        w2,
        b2,
    })
}
