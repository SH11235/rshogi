//! NNUE weight file management
//!
//! Handles loading and parsing of NNUE weight files

use super::features::{FeatureTransformer, FE_END};
use super::network::Network;
use std::error::Error;
use std::fs::File;
use std::io::Read;
use std::mem;

/// NNUE file header
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct NNUEHeader {
    magic: [u8; 4],    // "NNUE"
    version: u32,      // Version number
    architecture: u32, // Architecture ID
    size: u32,         // File size
}

/// Architecture ID for HalfKP 256x2-32-32
const HALFKP_256X2_32_32: u32 = 0x7AF32F16;

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
        let mut header_bytes = [0u8; mem::size_of::<NNUEHeader>()];
        self.file.read_exact(&mut header_bytes)?;

        let header: NNUEHeader = unsafe { mem::transmute_copy(&header_bytes) };

        // Validate magic
        if &header.magic != b"NNUE" {
            return Err("Invalid NNUE file magic".into());
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
    let ft_weights = reader.read_weights::<i16>(81 * FE_END * 256)?;
    let ft_biases = reader.read_weights::<i32>(256)?;

    // Read hidden layer 1
    let hidden1_weights = reader.read_weights::<i8>(512 * 32)?;
    let hidden1_biases = reader.read_weights::<i32>(32)?;

    // Read hidden layer 2
    let hidden2_weights = reader.read_weights::<i8>(32 * 32)?;
    let hidden2_biases = reader.read_weights::<i32>(32)?;

    // Read output layer
    let output_weights = reader.read_weights::<i8>(32)?;
    let output_bias_vec = reader.read_weights::<i32>(1)?;
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
