//! SIMD optimizations for NNUE
//!
//! Platform-specific SIMD implementations with runtime CPU feature detection

// Platform-specific modules
#[cfg(target_arch = "x86_64")]
pub mod x86_64;

#[cfg(target_arch = "wasm32")]
pub mod wasm32;

// Re-export the appropriate implementation based on the target
#[cfg(target_arch = "x86_64")]
pub use x86_64::*;

#[cfg(all(not(target_arch = "x86_64"), not(target_arch = "wasm32")))]
pub use scalar::*;

// Scalar fallback implementation
pub mod scalar {
    /// Scalar implementation of affine transformation
    #[inline]
    pub fn affine_transform_scalar(
        input: &[i8],
        weights: &[i8],
        biases: &[i32],
        output: &mut [i32],
        input_dim: usize,
        output_dim: usize,
    ) {
        // Copy biases
        output[..output_dim].copy_from_slice(&biases[..output_dim]);

        // Matrix multiplication
        for i in 0..output_dim {
            let mut sum = output[i];
            let weight_row = &weights[i * input_dim..(i + 1) * input_dim];

            for j in 0..input_dim {
                sum += input[j] as i32 * weight_row[j] as i32;
            }

            output[i] = sum;
        }
    }

    /// Scalar implementation of ClippedReLU
    #[inline]
    pub fn clipped_relu_scalar(input: &[i32], output: &mut [i8], size: usize) {
        for i in 0..size {
            output[i] = input[i].clamp(0, 127) as i8;
        }
    }

    /// Scalar implementation of feature transformation
    ///
    /// Converts 16-bit accumulated feature values to 8-bit for neural network input.
    /// This quantization step is crucial for performance and memory efficiency.
    #[inline]
    pub fn transform_features_scalar(us: &[i16], them: &[i16], output: &mut [i8], size: usize) {
        // Quantization shift: converts 16-bit accumulated values to 8-bit by dividing by 64
        //
        // Mathematical justification for SHIFT = 6:
        // 1. Accumulator range analysis:
        //    - Feature transformer weights are typically initialized in range [-127, 127]
        //    - With 32 active features average, accumulated sum ≈ 32 * 127 = 4064
        //    - Maximum theoretical: 128 features * 127 = 16256 (but rare in practice)
        //
        // 2. Target range (i8): [-127, 127]
        //    - We use [-127, 127] instead of full [-128, 127] to ensure symmetry
        //
        // 3. Shift calculation:
        //    - Right shift by 6 bits = division by 2^6 = 64
        //    - Maps ±4096 → ±64, safely within i8 range
        //    - Provides 2x safety margin for outliers (±8192 → ±128)
        //
        // 4. Quantization error:
        //    - Error = 1/64 ≈ 1.56% relative error
        //    - Acceptable for neural network inference (validated empirically)
        //
        // 5. Performance:
        //    - Bit shift is faster than division on all architectures
        //    - Power of 2 enables SIMD optimization
        const SHIFT: i32 = 6;

        for i in 0..size {
            output[i] = ((us[i] as i32) >> SHIFT).clamp(-127, 127) as i8;
            output[i + size] = ((them[i] as i32) >> SHIFT).clamp(-127, 127) as i8;
        }
    }

    /// Scalar implementation of accumulator update
    #[inline]
    pub fn update_accumulator_scalar(
        accumulator: &mut [i16],
        weights: &[i16],
        indices: &[usize],
        add: bool,
        row_len: usize,
    ) {
        for &idx in indices {
            let weight_offset = idx * row_len;

            if add {
                for i in 0..row_len {
                    accumulator[i] = accumulator[i].saturating_add(weights[weight_offset + i]);
                }
            } else {
                for i in 0..row_len {
                    accumulator[i] = accumulator[i].saturating_sub(weights[weight_offset + i]);
                }
            }
        }
    }
}

/// CPU feature detection and dispatcher
pub struct SimdDispatcher;

impl SimdDispatcher {
    /// Select the best available implementation for affine transformation
    #[inline]
    pub fn affine_transform(
        input: &[i8],
        weights: &[i8],
        biases: &[i32],
        output: &mut [i32],
        input_dim: usize,
        output_dim: usize,
    ) {
        #[cfg(target_arch = "x86_64")]
        {
            if is_x86_feature_detected!("avx2") {
                unsafe {
                    return x86_64::affine_transform_avx2(
                        input, weights, biases, output, input_dim, output_dim,
                    );
                }
            }

            if is_x86_feature_detected!("sse4.1") {
                unsafe {
                    return x86_64::affine_transform_sse41(
                        input, weights, biases, output, input_dim, output_dim,
                    );
                }
            }
        }

        // Fallback to scalar
        scalar::affine_transform_scalar(input, weights, biases, output, input_dim, output_dim);
    }

    /// Select the best available implementation for ClippedReLU
    #[inline]
    pub fn clipped_relu(input: &[i32], output: &mut [i8], size: usize) {
        #[cfg(target_arch = "x86_64")]
        {
            if is_x86_feature_detected!("avx2") {
                unsafe {
                    return x86_64::clipped_relu_avx2(input, output, size);
                }
            }

            if is_x86_feature_detected!("sse4.1") {
                unsafe {
                    return x86_64::clipped_relu_sse41(input, output, size);
                }
            }
        }

        // Fallback to scalar
        scalar::clipped_relu_scalar(input, output, size);
    }

    /// Select the best available implementation for feature transformation
    #[inline]
    pub fn transform_features(us: &[i16], them: &[i16], output: &mut [i8], size: usize) {
        #[cfg(target_arch = "x86_64")]
        {
            if is_x86_feature_detected!("avx2") {
                unsafe {
                    return x86_64::transform_features_avx2(us, them, output, size);
                }
            }

            if is_x86_feature_detected!("sse4.1") {
                unsafe {
                    return x86_64::transform_features_sse41(us, them, output, size);
                }
            }
        }

        // Fallback to scalar
        scalar::transform_features_scalar(us, them, output, size);
    }

    /// Select the best available implementation for accumulator update
    #[inline]
    pub fn update_accumulator(
        accumulator: &mut [i16],
        weights: &[i16],
        indices: &[usize],
        add: bool,
        row_len: usize,
    ) {
        #[cfg(target_arch = "x86_64")]
        {
            if is_x86_feature_detected!("avx2") {
                unsafe {
                    return x86_64::update_accumulator_avx2(
                        accumulator,
                        weights,
                        indices,
                        add,
                        row_len,
                    );
                }
            }

            if is_x86_feature_detected!("sse4.1") {
                unsafe {
                    return x86_64::update_accumulator_sse41(
                        accumulator,
                        weights,
                        indices,
                        add,
                        row_len,
                    );
                }
            }
        }

        // Fallback to scalar
        scalar::update_accumulator_scalar(accumulator, weights, indices, add, row_len);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scalar_affine_transform() {
        let input = vec![10i8; 4];
        let weights = vec![1i8, 2, 3, 4, 5, 6, 7, 8]; // 2x4 matrix
        let biases = vec![100i32, 200];
        let mut output = vec![0i32; 2];

        scalar::affine_transform_scalar(&input, &weights, &biases, &mut output, 4, 2);

        // output[0] = 100 + 10*(1+2+3+4) = 200
        // output[1] = 200 + 10*(5+6+7+8) = 460
        assert_eq!(output[0], 200);
        assert_eq!(output[1], 460);
    }

    #[test]
    fn test_scalar_clipped_relu() {
        let input = vec![-50, 0, 50, 100, 150];
        let mut output = vec![0i8; 5];

        scalar::clipped_relu_scalar(&input, &mut output, 5);

        assert_eq!(output[0], 0); // -50 -> 0
        assert_eq!(output[1], 0); // 0 -> 0
        assert_eq!(output[2], 50); // 50 -> 50
        assert_eq!(output[3], 100); // 100 -> 100
        assert_eq!(output[4], 127); // 150 -> 127 (clipped)
    }

    #[test]
    fn test_dispatcher_affine_transform() {
        let input = vec![10i8; 4];
        let weights = vec![1i8, 2, 3, 4, 5, 6, 7, 8];
        let biases = vec![100i32, 200];
        let mut output = vec![0i32; 2];

        SimdDispatcher::affine_transform(&input, &weights, &biases, &mut output, 4, 2);

        assert_eq!(output[0], 200);
        assert_eq!(output[1], 460);
    }
}
