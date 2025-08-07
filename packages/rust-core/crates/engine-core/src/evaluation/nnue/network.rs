//! Neural network implementation for NNUE
//!
//! Implements 256x2-32-32-1 architecture with ClippedReLU activation

use crate::nnue::simd::SimdDispatcher;

/// Neural network for NNUE evaluation
pub struct Network {
    /// Hidden layer 1 weights [512][32]
    pub hidden1_weights: Vec<i8>,
    /// Hidden layer 1 biases \[32\]
    pub hidden1_biases: Vec<i32>,
    /// Hidden layer 2 weights \[32\]\[32\]
    pub hidden2_weights: Vec<i8>,
    /// Hidden layer 2 biases \[32\]
    pub hidden2_biases: Vec<i32>,
    /// Output layer weights \[32\]
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

        // Transform features to 8-bit (quantization) - using stack array
        let mut input = [0i8; 512];
        self.transform_features(acc_us, acc_them, &mut input);

        // Hidden layer 1 - using stack array
        let mut hidden1 = [0i32; 32];
        self.affine_propagate::<512, 32>(
            &input,
            &self.hidden1_weights,
            &self.hidden1_biases,
            &mut hidden1,
        );

        // ClippedReLU activation - using stack array
        let mut hidden1_out = [0i8; 32];
        self.clipped_relu::<32>(&hidden1, &mut hidden1_out);

        // Hidden layer 2 - using stack array
        let mut hidden2 = [0i32; 32];
        self.affine_propagate::<32, 32>(
            &hidden1_out,
            &self.hidden2_weights,
            &self.hidden2_biases,
            &mut hidden2,
        );

        // ClippedReLU activation - using stack array
        let mut hidden2_out = [0i8; 32];
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
        SimdDispatcher::transform_features(us, them, output, 256);
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

        SimdDispatcher::affine_transform(input, weights, biases, output, IN, OUT);
    }

    /// ClippedReLU activation: max(0, min(x, 127))
    fn clipped_relu<const N: usize>(&self, input: &[i32], output: &mut [i8]) {
        debug_assert_eq!(input.len(), N);
        debug_assert_eq!(output.len(), N);

        SimdDispatcher::clipped_relu(input, output, N);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
