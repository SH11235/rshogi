//! Create a mock NNUE weight file for testing
//!
//! This generates a small NNUE file with random weights suitable for testing.
//! The weights are small random values to ensure different positions get different evaluations.

// Import NNUE structures
use engine_core::nnue::features::FeatureTransformer;

use engine_core::nnue::network::Network;
use rand::{Rng, SeedableRng};
use rand_xoshiro::Xoshiro256Plus;
use std::error::Error;
use std::fs;
use std::io::Write;
use std::path::Path;

// We need to implement save_weights here since it's marked as #[cfg(test)]
// This is a copy of the save_weights function from weights.rs
fn save_weights(
    path: &str,
    transformer: &FeatureTransformer,
    network: &Network,
) -> Result<(), Box<dyn Error>> {
    use std::io::{Seek, SeekFrom};
    use std::mem;

    let mut file = fs::File::create(path)?;

    // NNUE header structure
    #[repr(C)]
    struct NNUEHeader {
        magic: [u8; 4],
        version: u32,
        architecture: u32,
        size: u32,
    }

    // Write header
    let header = NNUEHeader {
        magic: *b"NNUE",
        version: 1,
        architecture: 0x7AF32F16, // HALFKP_256X2_32_32
        size: 0,                  // Will be updated later
    };

    let header_bytes: [u8; mem::size_of::<NNUEHeader>()] = unsafe { mem::transmute_copy(&header) };
    file.write_all(&header_bytes)?;

    // Write feature transformer weights
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

fn main() -> Result<(), Box<dyn Error>> {
    println!("Creating mock NNUE weight file...");

    // Use a fixed seed for reproducibility
    let mut rng = Xoshiro256Plus::seed_from_u64(42);

    // Create directory if it doesn't exist
    let test_data_dir = Path::new("../engine-core/tests/data");
    if !test_data_dir.exists() {
        fs::create_dir_all(test_data_dir)?;
    }

    // Create feature transformer with small random weights
    let mut transformer = FeatureTransformer::zero();

    // Initialize with very small random values to avoid overflow
    // Use sparse initialization - most weights are zero, only a few are non-zero
    // This dramatically improves compression
    println!("Initializing feature transformer weights (sparse)...");
    for (i, weight) in transformer.weights.iter_mut().enumerate() {
        // Only set 1% of weights to non-zero values
        if i % 100 == 0 {
            *weight = rng.random_range(-10..=10);
        }
    }

    // Feature transformer biases: range [-100, 100]
    for (i, bias) in transformer.biases.iter_mut().enumerate() {
        // Set 10% of biases to non-zero
        if i % 10 == 0 {
            *bias = rng.random_range(-100..=100);
        }
    }

    // Create network with small random weights
    let mut network = Network::zero();

    // Hidden layer 1 weights: sparse initialization
    println!("Initializing hidden layer 1 (sparse)...");
    for (i, weight) in network.hidden1_weights.iter_mut().enumerate() {
        // Set 5% of weights to non-zero
        if i % 20 == 0 {
            *weight = rng.random_range(-5..=5);
        }
    }
    // All biases get small values (they're important)
    for bias in network.hidden1_biases.iter_mut() {
        *bias = rng.random_range(-50..=50);
    }

    // Hidden layer 2 weights: sparse initialization
    println!("Initializing hidden layer 2 (sparse)...");
    for (i, weight) in network.hidden2_weights.iter_mut().enumerate() {
        // Set 10% of weights to non-zero
        if i % 10 == 0 {
            *weight = rng.random_range(-5..=5);
        }
    }
    // All biases get small values
    for bias in network.hidden2_biases.iter_mut() {
        *bias = rng.random_range(-50..=50);
    }

    // Output layer weights: all get values (small layer)
    println!("Initializing output layer...");
    for weight in network.output_weights.iter_mut() {
        *weight = rng.random_range(-10..=10);
    }
    network.output_bias = rng.random_range(-100..=100);

    // Save to file
    let output_path = "../engine-core/tests/data/mock_nn.bin";
    println!("Saving to {output_path}...");
    save_weights(output_path, &transformer, &network)?;

    // Get file size
    let metadata = fs::metadata(output_path)?;
    let file_size = metadata.len();
    println!("Created mock NNUE file: {file_size} bytes");

    // Compress the file using flate2
    println!("Compressing file...");
    let input_data = fs::read(output_path)?;
    let compressed_path = "../engine-core/tests/data/mock_nn.bin.gz";
    let compressed_file = fs::File::create(compressed_path)?;
    let mut encoder = flate2::write::GzEncoder::new(compressed_file, flate2::Compression::best());
    encoder.write_all(&input_data)?;
    encoder.finish()?;

    let compressed_metadata = fs::metadata(compressed_path)?;
    let compressed_size = compressed_metadata.len();
    println!(
        "Compressed file: {} bytes (compression ratio: {:.1}%)",
        compressed_size,
        (compressed_size as f64 / file_size as f64) * 100.0
    );

    // Remove the uncompressed file
    fs::remove_file(output_path)?;
    println!("Removed uncompressed file");

    println!("Done! Mock NNUE file created at: {compressed_path}");

    Ok(())
}
