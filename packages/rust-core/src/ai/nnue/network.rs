//! Neural network implementation for NNUE
//!
//! Implements 256x2-32-32-1 architecture with ClippedReLU activation

use std::cmp::{max, min};

/// Clamp value to range
#[inline]
fn clamp(x: i32, low: i32, high: i32) -> i32 {
    min(max(x, low), high)
}

/// Neural network for NNUE evaluation
pub struct Network {
    /// Hidden layer 1 weights [512][32]
    pub hidden1_weights: Vec<i8>,
    /// Hidden layer 1 biases [32]
    pub hidden1_biases: Vec<i32>,
    /// Hidden layer 2 weights [32][32]
    pub hidden2_weights: Vec<i8>,
    /// Hidden layer 2 biases [32]
    pub hidden2_biases: Vec<i32>,
    /// Output layer weights [32]
    pub output_weights: Vec<i8>,
    /// Output layer bias
    pub output_bias: i32,
}

impl Network {
    /// Create zero-initialized network
    pub fn zero() -> Self {
        Network {
            hidden1_weights: vec![0; 512 * 32],
            hidden1_biases: vec![0; 32],
            hidden2_weights: vec![0; 32 * 32],
            hidden2_biases: vec![0; 32],
            output_weights: vec![0; 32],
            output_bias: 0,
        }
    }

    /// Forward propagation through the network
    pub fn propagate(&self, acc_us: &[i16], acc_them: &[i16]) -> i32 {
        debug_assert_eq!(acc_us.len(), 256);
        debug_assert_eq!(acc_them.len(), 256);

        // Transform features to 8-bit (quantization)
        let mut input = vec![0i8; 512];
        self.transform_features(acc_us, acc_them, &mut input);

        // Hidden layer 1
        let mut hidden1 = vec![0i32; 32];
        self.affine_propagate::<512, 32>(
            &input,
            &self.hidden1_weights,
            &self.hidden1_biases,
            &mut hidden1,
        );

        // ClippedReLU activation
        let mut hidden1_out = vec![0i8; 32];
        self.clipped_relu::<32>(&hidden1, &mut hidden1_out);

        // Hidden layer 2
        let mut hidden2 = vec![0i32; 32];
        self.affine_propagate::<32, 32>(
            &hidden1_out,
            &self.hidden2_weights,
            &self.hidden2_biases,
            &mut hidden2,
        );

        // ClippedReLU activation
        let mut hidden2_out = vec![0i8; 32];
        self.clipped_relu::<32>(&hidden2, &mut hidden2_out);

        // Output layer
        let mut output = self.output_bias;
        for (i, &h2_out) in hidden2_out.iter().enumerate().take(32) {
            output += h2_out as i32 * self.output_weights[i] as i32;
        }

        output
    }

    /// Transform 16-bit features to 8-bit with clamping
    fn transform_features(&self, us: &[i16], them: &[i16], output: &mut [i8]) {
        const SHIFT: i32 = 6; // Shift for quantization

        for i in 0..256 {
            // Our perspective
            output[i] = clamp((us[i] as i32) >> SHIFT, -127, 127) as i8;
            // Opponent perspective
            output[i + 256] = clamp((them[i] as i32) >> SHIFT, -127, 127) as i8;
        }
    }

    /// Affine transformation (matrix multiply + bias)
    fn affine_propagate<const IN: usize, const OUT: usize>(
        &self,
        input: &[i8],
        weights: &[i8],
        biases: &[i32],
        output: &mut [i32],
    ) {
        debug_assert_eq!(input.len(), IN);
        debug_assert_eq!(weights.len(), IN * OUT);
        debug_assert_eq!(biases.len(), OUT);
        debug_assert_eq!(output.len(), OUT);

        // Copy biases
        output.copy_from_slice(biases);

        // Matrix multiplication
        for i in 0..OUT {
            let mut sum = output[i];
            for j in 0..IN {
                sum += input[j] as i32 * weights[i * IN + j] as i32;
            }
            output[i] = sum;
        }
    }

    /// ClippedReLU activation: max(0, min(x, 127))
    fn clipped_relu<const N: usize>(&self, input: &[i32], output: &mut [i8]) {
        debug_assert_eq!(input.len(), N);
        debug_assert_eq!(output.len(), N);

        for i in 0..N {
            output[i] = clamp(input[i], 0, 127) as i8;
        }
    }
}

/// SIMD-optimized versions (placeholder for now)
#[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
mod simd {
    use super::*;
    use std::arch::x86_64::*;

    impl Network {
        /// AVX2-optimized affine transformation
        pub unsafe fn affine_propagate_avx2<const IN: usize, const OUT: usize>(
            &self,
            input: &[i8],
            weights: &[i8],
            biases: &[i32],
            output: &mut [i32],
        ) {
            // AVX2 implementation would go here
            // For now, fall back to scalar version
            self.affine_propagate::<IN, OUT>(input, weights, biases, output);
        }

        /// AVX2-optimized ClippedReLU
        pub unsafe fn clipped_relu_avx2<const N: usize>(&self, input: &[i32], output: &mut [i8]) {
            // AVX2 implementation would go here
            // For now, fall back to scalar version
            self.clipped_relu::<N>(input, output);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clamp() {
        assert_eq!(clamp(50, 0, 127), 50);
        assert_eq!(clamp(-10, 0, 127), 0);
        assert_eq!(clamp(200, 0, 127), 127);
    }

    #[test]
    fn test_network_zero() {
        let network = Network::zero();
        let acc_us = vec![0i16; 256];
        let acc_them = vec![0i16; 256];

        let output = network.propagate(&acc_us, &acc_them);
        assert_eq!(output, 0);
    }

    #[test]
    fn test_transform_features() {
        let network = Network::zero();
        let us = vec![64i16; 256]; // Will become 1 after shift by 6
        let them = vec![-64i16; 256]; // Will become -1 after shift by 6
        let mut output = vec![0i8; 512];

        network.transform_features(&us, &them, &mut output);

        assert_eq!(output[0], 1);
        assert_eq!(output[256], -1);
    }

    #[test]
    fn test_affine_propagate() {
        let network = Network::zero();
        let input = vec![10i8; 4];
        let weights = vec![1i8, 2, 3, 4, 5, 6, 7, 8]; // 2x4 matrix
        let biases = vec![100i32, 200];
        let mut output = vec![0i32; 2];

        network.affine_propagate::<4, 2>(&input, &weights, &biases, &mut output);

        // output[0] = 100 + 10*(1+2+3+4) = 100 + 100 = 200
        // output[1] = 200 + 10*(5+6+7+8) = 200 + 260 = 460
        assert_eq!(output[0], 200);
        assert_eq!(output[1], 460);
    }

    #[test]
    fn test_clipped_relu() {
        let network = Network::zero();
        let input = vec![-50, 0, 50, 100, 150];
        let mut output = vec![0i8; 5];

        network.clipped_relu::<5>(&input, &mut output);

        assert_eq!(output[0], 0); // -50 -> 0
        assert_eq!(output[1], 0); // 0 -> 0
        assert_eq!(output[2], 50); // 50 -> 50
        assert_eq!(output[3], 100); // 100 -> 100
        assert_eq!(output[4], 127); // 150 -> 127 (clipped)
    }
}
