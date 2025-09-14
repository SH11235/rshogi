//! Neural network implementation for NNUE
//!
//! Implements 256x2-32-32-1 architecture with ClippedReLU activation

use crate::nnue::simd::SimdDispatcher;
use std::cell::RefCell;

thread_local! {
    static WORKSPACE: RefCell<Workspace> = RefCell::new(Workspace::default());
}

#[derive(Default)]
struct Workspace {
    input: Vec<i8>,
    h1: Vec<i32>,
    h1_out: Vec<i8>,
    h2: Vec<i32>,
    h2_out: Vec<i8>,
}

impl Workspace {}

/// Neural network for NNUE evaluation
pub struct Network {
    /// Hidden layer 1 weights [input_dim][h1_dim]
    pub hidden1_weights: Vec<i8>,
    /// Hidden layer 1 biases \[h1_dim\]
    pub hidden1_biases: Vec<i32>,
    /// Hidden layer 2 weights \[h1_dim\]\[h2_dim\]
    pub hidden2_weights: Vec<i8>,
    /// Hidden layer 2 biases \[h2_dim\]
    pub hidden2_biases: Vec<i32>,
    /// Output layer weights \[h2_dim\]
    pub output_weights: Vec<i8>,
    /// Output layer bias
    pub output_bias: i32,
    /// Cached input dimension (typically acc_dim * 2)
    pub(crate) input_dim: usize,
    /// Hidden layer 1 dimension (default 32)
    pub(crate) h1_dim: usize,
    /// Hidden layer 2 dimension (default 32)
    pub(crate) h2_dim: usize,
}

impl Network {
    /// Create zero-initialized network
    pub fn zero() -> Self {
        let input_dim = 512; // 256 x 2 (default)
        let h1_dim = 32;
        let h2_dim = 32;
        Network {
            hidden1_weights: vec![0; input_dim * h1_dim],
            hidden1_biases: vec![0; h1_dim],
            hidden2_weights: vec![0; h1_dim * h2_dim],
            hidden2_biases: vec![0; h2_dim],
            output_weights: vec![0; h2_dim],
            output_bias: 0,
            input_dim,
            h1_dim,
            h2_dim,
        }
    }

    /// Forward propagation through the network
    pub fn propagate(&self, acc_us: &[i16], acc_them: &[i16]) -> i32 {
        let acc_dim = acc_us.len();
        debug_assert_eq!(acc_them.len(), acc_dim);
        let input_dim = acc_dim * 2;
        debug_assert_eq!(
            input_dim, self.input_dim,
            "input_dim mismatch: {} vs {}",
            input_dim, self.input_dim
        );

        // Try to reuse thread-local workspace; fall back to local buffers if re-entrant
        if let Some(result) = WORKSPACE
            .try_with(|ws| {
                if let Ok(mut ws) = ws.try_borrow_mut() {
                    // Move out buffers to avoid nested borrows; put them back at the end
                    let mut input = std::mem::take(&mut ws.input);
                    let mut h1 = std::mem::take(&mut ws.h1);
                    let mut h1_out = std::mem::take(&mut ws.h1_out);
                    let mut h2 = std::mem::take(&mut ws.h2);
                    let mut h2_out = std::mem::take(&mut ws.h2_out);

                    // Ensure sizes
                    if input.len() != input_dim {
                        input.resize(input_dim, 0);
                    }
                    if h1.len() != self.h1_dim {
                        h1.resize(self.h1_dim, 0);
                    }
                    if h1_out.len() != self.h1_dim {
                        h1_out.resize(self.h1_dim, 0);
                    }
                    if h2.len() != self.h2_dim {
                        h2.resize(self.h2_dim, 0);
                    }
                    if h2_out.len() != self.h2_dim {
                        h2_out.resize(self.h2_dim, 0);
                    }

                    // Transform features to 8-bit (quantization)
                    self.transform_features(acc_us, acc_them, &mut input);

                    // Hidden layer 1
                    self.affine_propagate_dyn(
                        &input,
                        &self.hidden1_weights,
                        &self.hidden1_biases,
                        &mut h1,
                    );

                    // ClippedReLU activation
                    self.clipped_relu_dyn(&h1, &mut h1_out);

                    // Hidden layer 2
                    self.affine_propagate_dyn(
                        &h1_out,
                        &self.hidden2_weights,
                        &self.hidden2_biases,
                        &mut h2,
                    );

                    // ClippedReLU activation
                    self.clipped_relu_dyn(&h2, &mut h2_out);

                    // Output layer (dot-product)
                    let mut output = self.output_bias;
                    // Iterate directly (avoid needless_range_loop)
                    for (i, &v) in h2_out.iter().enumerate() {
                        if i == self.h2_dim {
                            break;
                        }
                        output += v as i32 * self.output_weights[i] as i32;
                    }

                    // Put buffers back into workspace
                    ws.input = input;
                    ws.h1 = h1;
                    ws.h1_out = h1_out;
                    ws.h2 = h2;
                    ws.h2_out = h2_out;

                    Some(output)
                } else {
                    None
                }
            })
            .ok()
            .flatten()
        {
            return result;
        }

        // Fallback: allocate local buffers if TLS workspace is busy
        let mut input = vec![0i8; input_dim];
        self.transform_features(acc_us, acc_them, &mut input);
        let mut h1 = vec![0i32; self.h1_dim];
        self.affine_propagate_dyn(&input, &self.hidden1_weights, &self.hidden1_biases, &mut h1);
        let mut h1_out = vec![0i8; self.h1_dim];
        self.clipped_relu_dyn(&h1, &mut h1_out);
        let mut h2 = vec![0i32; self.h2_dim];
        self.affine_propagate_dyn(&h1_out, &self.hidden2_weights, &self.hidden2_biases, &mut h2);
        let mut h2_out = vec![0i8; self.h2_dim];
        self.clipped_relu_dyn(&h2, &mut h2_out);
        let mut output = self.output_bias;
        for (i, &v) in h2_out.iter().enumerate() {
            if i == self.h2_dim {
                break;
            }
            output += v as i32 * self.output_weights[i] as i32;
        }
        output
    }

    /// Transform 16-bit features to 8-bit with clamping
    fn transform_features(&self, us: &[i16], them: &[i16], output: &mut [i8]) {
        debug_assert_eq!(output.len(), us.len() * 2);
        SimdDispatcher::transform_features(us, them, output, us.len());
    }

    /// Affine transformation (matrix multiply + bias)
    fn affine_propagate_dyn(
        &self,
        input: &[i8],
        weights: &[i8],
        biases: &[i32],
        output: &mut [i32],
    ) {
        let in_dim = input.len();
        let out_dim = output.len();
        debug_assert_eq!(weights.len(), in_dim * out_dim);
        debug_assert_eq!(biases.len(), out_dim);
        SimdDispatcher::affine_transform(input, weights, biases, output, in_dim, out_dim);
    }

    /// ClippedReLU activation: max(0, min(x, 127))
    fn clipped_relu_dyn(&self, input: &[i32], output: &mut [i8]) {
        debug_assert_eq!(input.len(), output.len());
        SimdDispatcher::clipped_relu(input, output, input.len());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

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

        network.affine_propagate_dyn(&input, &weights, &biases, &mut output);

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

        network.clipped_relu_dyn(&input, &mut output);

        assert_eq!(output[0], 0); // -50 -> 0
        assert_eq!(output[1], 0); // 0 -> 0
        assert_eq!(output[2], 50); // 50 -> 50
        assert_eq!(output[3], 100); // 100 -> 100
        assert_eq!(output[4], 127); // 150 -> 127 (clipped)
    }

    // フォールバック経路（TLSワークスペースが借用中でも動作すること）の機能テスト
    #[test]
    fn propagate_falls_back_when_workspace_borrowed() {
        let net = Network::zero();
        let acc_us = vec![0i16; 256];
        let acc_them = vec![0i16; 256];

        // 通常経路の結果
        let out_normal = net.propagate(&acc_us, &acc_them);

        // ワークスペースを敢えて借用中にし、フォールバックを踏ませる
        let out_fallback = WORKSPACE.with(|cell| {
            let _guard = cell.borrow_mut();
            net.propagate(&acc_us, &acc_them)
        });

        assert_eq!(out_normal, out_fallback);
    }
}
