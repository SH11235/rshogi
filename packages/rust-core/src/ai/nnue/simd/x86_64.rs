//! x86_64 SIMD implementations
//!
//! AVX2 and SSE4.1 optimized versions of NNUE operations

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

/// Apply an affine transformation **(AVX2)** to a tile of `input`.
///
/// * `input`  : 1-D vector of length **`input_dim`** (i8)
/// * `weights`: row-major matrix `output_dim × input_dim` (i8)
/// * `biases` : length `output_dim` (i32)
/// * `output` : **mutable** slice with the same length as `biases`, will be **overwritten**
///
/// # Safety
///
/// * **CPU feature**: The caller must ensure the current CPU supports **AVX2**  
///   (e.g. `is_x86_feature_detected!("avx2")` returns `true`) before calling.
/// * **Slice lengths**:  
///   - `input.len()  >= input_dim`  
///   - `weights.len() == input_dim * output_dim`  
///   - `biases.len()  >= output_dim`  
///   - `output.len()  >= output_dim`
/// * **Aliasing**: `output` must not alias `weights` or `input`.
/// * **Alignment**: Unaligned loads (`_mm256_loadu_si256`) are used,  
///   so 32-byte alignment is **not** required, but the pointers must remain valid
///   for the life of the call.
#[target_feature(enable = "avx2")]
#[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
pub unsafe fn affine_transform_avx2(
    input: &[i8],
    weights: &[i8],
    biases: &[i32],
    output: &mut [i32],
    input_dim: usize,
    output_dim: usize,
) {
    // Debug assertions for boundary checks
    debug_assert!(input.len() >= input_dim, "Input buffer too small");
    debug_assert!(weights.len() >= input_dim * output_dim, "Weights buffer too small");
    debug_assert!(biases.len() >= output_dim, "Biases buffer too small");
    debug_assert!(output.len() >= output_dim, "Output buffer too small");

    const TILE_HEIGHT: usize = 8;
    const TILE_WIDTH: usize = 32;

    // 256-bit 定数（16 × i16 = 1）を 1 回だけ生成
    let ones = _mm256_set1_epi16(1);

    // Initialize output with biases
    output[..output_dim].copy_from_slice(&biases[..output_dim]);

    // Process 8 outputs at a time
    let mut out_idx = 0;
    while out_idx + TILE_HEIGHT <= output_dim {
        // Initialize accumulators to zero (biases already in output)
        let mut acc = [_mm256_setzero_si256(); TILE_HEIGHT];

        // Process input in chunks of 32
        let mut in_idx = 0;
        while in_idx + TILE_WIDTH <= input_dim {
            // Load 32 input values
            let input_vec = _mm256_loadu_si256(input.as_ptr().add(in_idx) as *const __m256i);

            // De-interleave input 8-bit → 16-bit (1 回で済ませる)
            let input_lo = _mm256_cvtepi8_epi16(_mm256_extracti128_si256(input_vec, 0));
            let input_hi = _mm256_cvtepi8_epi16(_mm256_extracti128_si256(input_vec, 1));

            // Process each output row
            for (i, acc_cell) in acc.iter_mut().take(TILE_HEIGHT).enumerate() {
                let weight_offset = (out_idx + i) * input_dim + in_idx;
                let weight_vec =
                    _mm256_loadu_si256(weights.as_ptr().add(weight_offset) as *const __m256i);

                let weight_lo = _mm256_cvtepi8_epi16(_mm256_extracti128_si256(weight_vec, 0));
                let weight_hi = _mm256_cvtepi8_epi16(_mm256_extracti128_si256(weight_vec, 1));

                // Multiply
                let prod_lo = _mm256_mullo_epi16(input_lo, weight_lo);
                let prod_hi = _mm256_mullo_epi16(input_hi, weight_hi);

                // Sum pairs and convert to i32
                let sum_lo = _mm256_madd_epi16(prod_lo, ones);
                let sum_hi = _mm256_madd_epi16(prod_hi, ones);

                // Accumulate
                *acc_cell = _mm256_add_epi32(*acc_cell, sum_lo);
                *acc_cell = _mm256_add_epi32(*acc_cell, sum_hi);
            }

            in_idx += TILE_WIDTH;
        }
        // Handle remaining input elements
        while in_idx < input_dim {
            for i in 0..TILE_HEIGHT {
                let weight_offset = (out_idx + i) * input_dim + in_idx;
                output[out_idx + i] += input[in_idx] as i32 * weights[weight_offset] as i32;
            }
            in_idx += 1;
        }

        // Store results (horizontal sum)
        for i in 0..TILE_HEIGHT {
            // Sum all elements in the vector
            let sum = hsum_epi32_avx2(acc[i]);
            output[out_idx + i] += sum;
        }

        out_idx += TILE_HEIGHT;
    }

    // Handle remaining outputs with scalar code
    while out_idx < output_dim {
        output[out_idx] = biases[out_idx];
        for (in_idx, &x) in input.iter().take(input_dim).enumerate() {
            let weight_offset = out_idx * input_dim + in_idx;
            output[out_idx] += x as i32 * weights[weight_offset] as i32;
        }

        out_idx += 1;
    }
}

/// Horizontal sum of 8 i32 values in a __m256i vector **(AVX2)**.
///
/// Computes the sum of all 8 32-bit integers packed in the input vector.
///
/// # Safety
///
/// * **CPU feature**: The caller must ensure the current CPU supports **AVX2**  
///   (e.g. `is_x86_feature_detected!("avx2")` returns `true`) before calling.
/// * **Valid input**: The input must be a valid __m256i vector.
#[inline]
#[target_feature(enable = "avx2")]
unsafe fn hsum_epi32_avx2(v: __m256i) -> i32 {
    // Add upper 128 bits to lower 128 bits
    let sum128 = _mm_add_epi32(_mm256_castsi256_si128(v), _mm256_extracti128_si256(v, 1));

    // Horizontal add
    let sum64 = _mm_hadd_epi32(sum128, sum128);
    let sum32 = _mm_hadd_epi32(sum64, sum64);

    _mm_extract_epi32(sum32, 0)
}

/// Apply ClippedReLU activation function **(AVX2)**: `output[i] = clamp(input[i], 0, 127)`.
///
/// * `input` : array of i32 values to be clamped
/// * `output`: **mutable** array where clamped i8 values will be written
/// * `size`  : number of elements to process
///
/// # Safety
///
/// * **CPU feature**: The caller must ensure the current CPU supports **AVX2**  
///   (e.g. `is_x86_feature_detected!("avx2")` returns `true`) before calling.
/// * **Slice lengths**:  
///   - `input.len()  >= size`  
///   - `output.len() >= size`
/// * **Aliasing**: `output` must not alias `input`.
/// * **Alignment**: Unaligned loads/stores are used, so alignment is not required.
#[target_feature(enable = "avx2")]
#[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
pub unsafe fn clipped_relu_avx2(input: &[i32], output: &mut [i8], size: usize) {
    // Debug assertions for boundary checks
    debug_assert!(input.len() >= size, "Input buffer too small");
    debug_assert!(output.len() >= size, "Output buffer too small");
    const CHUNK_SIZE: usize = 32;

    let zero = _mm256_setzero_si256();
    let max_val = _mm256_set1_epi32(127);

    let mut i = 0;
    while i + CHUNK_SIZE <= size {
        // Load 32 i32 values (8 per register)
        let v0 = _mm256_loadu_si256(input.as_ptr().add(i) as *const __m256i);
        let v1 = _mm256_loadu_si256(input.as_ptr().add(i + 8) as *const __m256i);
        let v2 = _mm256_loadu_si256(input.as_ptr().add(i + 16) as *const __m256i);
        let v3 = _mm256_loadu_si256(input.as_ptr().add(i + 24) as *const __m256i);

        // Clamp to [0, 127]
        let c0 = _mm256_min_epi32(_mm256_max_epi32(v0, zero), max_val);
        let c1 = _mm256_min_epi32(_mm256_max_epi32(v1, zero), max_val);
        let c2 = _mm256_min_epi32(_mm256_max_epi32(v2, zero), max_val);
        let c3 = _mm256_min_epi32(_mm256_max_epi32(v3, zero), max_val);

        // Pack i32 to i16
        let p0 = _mm256_packs_epi32(c0, c1);
        let p1 = _mm256_packs_epi32(c2, c3);

        // Pack i16 to i8
        let result = _mm256_packs_epi16(p0, p1);

        // Permute to correct order
        let perm = _mm256_setr_epi32(0, 4, 1, 5, 2, 6, 3, 7);
        let result = _mm256_permutevar8x32_epi32(result, perm);

        // Store 32 i8 values
        _mm256_storeu_si256(output.as_mut_ptr().add(i) as *mut __m256i, result);

        i += CHUNK_SIZE;
    }

    // Handle remaining elements
    while i < size {
        output[i] = input[i].clamp(0, 127) as i8;
        i += 1;
    }
}

/// Transform 16-bit features to 8-bit with quantization **(AVX2)**.
///
/// * `us`    : perspective features for side-to-move (i16)
/// * `them`  : perspective features for opponent (i16)
/// * `output`: **mutable** array where quantized features are written
/// * `size`  : number of features per perspective
///
/// The output layout is: `[us[0..size], them[0..size]]` after shifting right by 6 bits
/// and clamping to [-127, 127].
///
/// # Safety
///
/// * **CPU feature**: The caller must ensure the current CPU supports **AVX2**  
///   (e.g. `is_x86_feature_detected!("avx2")` returns `true`) before calling.
/// * **Slice lengths**:  
///   - `us.len()     >= size`  
///   - `them.len()   >= size`  
///   - `output.len() >= size * 2`
/// * **Aliasing**: `output` must not alias `us` or `them`.
/// * **Alignment**: Unaligned loads/stores are used, so alignment is not required.
#[target_feature(enable = "avx2")]
#[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
pub unsafe fn transform_features_avx2(us: &[i16], them: &[i16], output: &mut [i8], size: usize) {
    // Debug assertions for boundary checks
    debug_assert!(us.len() >= size, "'us' buffer too small");
    debug_assert!(them.len() >= size, "'them' buffer too small");
    debug_assert!(output.len() >= size * 2, "Output buffer too small");
    // For simplicity, just call the scalar version
    // A real AVX2 implementation would use SIMD operations
    super::scalar::transform_features_scalar(us, them, output, size);
}

/// Update accumulator by adding or subtracting feature weights **(AVX2)**.
///
/// * `accumulator`: **mutable** array of 256 i16 values (current accumulator state)
/// * `weights`    : feature transformer weights array
/// * `indices`    : list of feature indices to add/remove
/// * `add`        : if true, add weights; if false, subtract weights
///
/// For each index in `indices`, the corresponding 256 weights starting at
/// `weights[index * 256]` are added to or subtracted from the accumulator
/// using saturating arithmetic.
///
/// # Safety
///
/// * **CPU feature**: The caller must ensure the current CPU supports **AVX2**  
///   (e.g. `is_x86_feature_detected!("avx2")` returns `true`) before calling.
/// * **Slice lengths**:  
///   - `accumulator.len() >= 256`  
///   - `weights.len() >= max(indices) * 256 + 256`
/// * **Index validity**: All values in `indices` must satisfy `index * 256 + 256 <= weights.len()`.
/// * **Aliasing**: `accumulator` must not alias `weights`.
/// * **Alignment**: Unaligned loads/stores are used, so alignment is not required.
#[target_feature(enable = "avx2")]
#[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
pub unsafe fn update_accumulator_avx2(
    accumulator: &mut [i16],
    weights: &[i16],
    indices: &[usize],
    add: bool,
) {
    // Debug assertions for boundary checks
    debug_assert!(accumulator.len() >= 256, "Accumulator must have at least 256 elements");
    for &idx in indices {
        debug_assert!(idx * 256 + 256 <= weights.len(), "Weight index {} out of bounds", idx);
    }
    const CHUNK_SIZE: usize = 16;

    for &idx in indices {
        let weight_offset = idx * 256;
        let weight_ptr = weights.as_ptr().add(weight_offset);

        let mut i = 0;
        while i + CHUNK_SIZE <= 256 {
            let acc_vec = _mm256_loadu_si256(accumulator.as_ptr().add(i) as *const __m256i);
            let weight_vec = _mm256_loadu_si256(weight_ptr.add(i) as *const __m256i);

            let result = if add {
                _mm256_adds_epi16(acc_vec, weight_vec)
            } else {
                _mm256_subs_epi16(acc_vec, weight_vec)
            };

            _mm256_storeu_si256(accumulator.as_mut_ptr().add(i) as *mut __m256i, result);

            i += CHUNK_SIZE;
        }

        // Handle remaining elements
        while i < 256 {
            if add {
                accumulator[i] = accumulator[i].saturating_add(weights[weight_offset + i]);
            } else {
                accumulator[i] = accumulator[i].saturating_sub(weights[weight_offset + i]);
            }
            i += 1;
        }
    }
}

// SSE4.1 implementations (fallback for older CPUs)

/// Apply an affine transformation **(SSE4.1)** to a tile of `input`.
///
/// * `input`  : 1-D vector of length **`input_dim`** (i8)
/// * `weights`: row-major matrix `output_dim × input_dim` (i8)
/// * `biases` : length `output_dim` (i32)
/// * `output` : **mutable** slice with the same length as `biases`, will be **overwritten**
///
/// # Safety
///
/// * **CPU feature**: The caller must ensure the current CPU supports **SSE4.1**  
///   (e.g. `is_x86_feature_detected!("sse4.1")` returns `true`) before calling.
/// * **Slice lengths**:  
///   - `input.len()  >= input_dim`  
///   - `weights.len() == input_dim * output_dim`  
///   - `biases.len()  >= output_dim`  
///   - `output.len()  >= output_dim`
/// * **Aliasing**: `output` must not alias `weights` or `input`.
#[target_feature(enable = "sse4.1")]
#[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
pub unsafe fn affine_transform_sse41(
    input: &[i8],
    weights: &[i8],
    biases: &[i32],
    output: &mut [i32],
    input_dim: usize,
    output_dim: usize,
) {
    // Debug assertions for boundary checks
    debug_assert!(input.len() >= input_dim, "Input buffer too small");
    debug_assert!(weights.len() >= input_dim * output_dim, "Weights buffer too small");
    debug_assert!(biases.len() >= output_dim, "Biases buffer too small");
    debug_assert!(output.len() >= output_dim, "Output buffer too small");
    const TILE_HEIGHT: usize = 4; // Process 4 outputs at a time
    const TILE_WIDTH: usize = 16; // Process 16 inputs at a time

    // Initialize output with biases
    output[..output_dim].copy_from_slice(&biases[..output_dim]);

    // Process 4 outputs at a time
    let mut out_idx = 0;
    while out_idx + TILE_HEIGHT <= output_dim {
        // Initialize accumulators to zero (biases already in output)
        let mut acc = [_mm_setzero_si128(); TILE_HEIGHT];

        // Process input in chunks of 16
        let mut in_idx = 0;
        while in_idx + TILE_WIDTH <= input_dim {
            // Load 16 input values
            let input_vec = _mm_loadu_si128(input.as_ptr().add(in_idx) as *const __m128i);

            // Process each output row
            for (i, acc_cell) in acc.iter_mut().take(TILE_HEIGHT).enumerate() {
                let weight_offset = (out_idx + i) * input_dim + in_idx;
                let weight_vec =
                    _mm_loadu_si128(weights.as_ptr().add(weight_offset) as *const __m128i);

                // Convert i8 to i16 for multiplication
                let input_lo = _mm_cvtepi8_epi16(input_vec);
                let input_hi = _mm_cvtepi8_epi16(_mm_srli_si128(input_vec, 8));

                let weight_lo = _mm_cvtepi8_epi16(weight_vec);
                let weight_hi = _mm_cvtepi8_epi16(_mm_srli_si128(weight_vec, 8));

                // Multiply and accumulate
                let prod_lo = _mm_mullo_epi16(input_lo, weight_lo);
                let prod_hi = _mm_mullo_epi16(input_hi, weight_hi);

                // Convert to i32 and accumulate
                let ones = _mm_set1_epi16(1);
                let sum_lo = _mm_madd_epi16(prod_lo, ones);
                let sum_hi = _mm_madd_epi16(prod_hi, ones);

                *acc_cell = _mm_add_epi32(*acc_cell, sum_lo);
                *acc_cell = _mm_add_epi32(*acc_cell, sum_hi);
            }

            in_idx += TILE_WIDTH;
        }

        // Handle remaining input elements
        while in_idx < input_dim {
            for i in 0..TILE_HEIGHT {
                if out_idx + i < output_dim {
                    let weight_offset = (out_idx + i) * input_dim + in_idx;
                    output[out_idx + i] += input[in_idx] as i32 * weights[weight_offset] as i32;
                }
            }
            in_idx += 1;
        }

        // Store results (horizontal sum)
        for i in 0..TILE_HEIGHT {
            if out_idx + i < output_dim {
                // Sum all elements in the vector
                let sum = hsum_epi32_sse41(acc[i]);
                output[out_idx + i] += sum;
            }
        }

        out_idx += TILE_HEIGHT;
    }

    // Handle remaining outputs with scalar code
    while out_idx < output_dim {
        for (in_idx, &x) in input.iter().take(input_dim).enumerate() {
            let weight_offset = out_idx * input_dim + in_idx;
            output[out_idx] += x as i32 * weights[weight_offset] as i32;
        }
        out_idx += 1;
    }
}

/// Horizontal sum of 4 i32 values in a __m128i vector **(SSE4.1)**
///
/// # Safety
///
/// * **CPU feature**: The caller must ensure the current CPU supports **SSE4.1**
/// * **Valid input**: The input must be a valid __m128i vector
#[inline]
#[target_feature(enable = "sse4.1")]
unsafe fn hsum_epi32_sse41(v: __m128i) -> i32 {
    // Horizontal add pairs
    let sum64 = _mm_hadd_epi32(v, v);
    let sum32 = _mm_hadd_epi32(sum64, sum64);

    _mm_extract_epi32(sum32, 0)
}

/// Apply ClippedReLU activation function **(SSE4.1)**: `output[i] = clamp(input[i], 0, 127)`.
///
/// * `input` : array of i32 values to be clamped
/// * `output`: **mutable** array where clamped i8 values will be written
/// * `size`  : number of elements to process
///
/// # Safety
///
/// * **CPU feature**: The caller must ensure the current CPU supports **SSE4.1**  
///   (e.g. `is_x86_feature_detected!("sse4.1")` returns `true`) before calling.
/// * **Slice lengths**:  
///   - `input.len()  >= size`  
///   - `output.len() >= size`
/// * **Aliasing**: `output` must not alias `input`.
/// * **Alignment**: Unaligned loads/stores are used, so alignment is not required.
#[target_feature(enable = "sse4.1")]
#[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
pub unsafe fn clipped_relu_sse41(input: &[i32], output: &mut [i8], size: usize) {
    // Debug assertions for boundary checks
    debug_assert!(input.len() >= size, "Input buffer too small");
    debug_assert!(output.len() >= size, "Output buffer too small");
    const CHUNK_SIZE: usize = 16;

    let zero = _mm_setzero_si128();
    let max_val = _mm_set1_epi32(127);

    let mut i = 0;
    while i + CHUNK_SIZE <= size {
        // Load 16 i32 values (4 per register)
        let v0 = _mm_loadu_si128(input.as_ptr().add(i) as *const __m128i);
        let v1 = _mm_loadu_si128(input.as_ptr().add(i + 4) as *const __m128i);
        let v2 = _mm_loadu_si128(input.as_ptr().add(i + 8) as *const __m128i);
        let v3 = _mm_loadu_si128(input.as_ptr().add(i + 12) as *const __m128i);

        // Clamp to [0, 127]
        let c0 = _mm_min_epi32(_mm_max_epi32(v0, zero), max_val);
        let c1 = _mm_min_epi32(_mm_max_epi32(v1, zero), max_val);
        let c2 = _mm_min_epi32(_mm_max_epi32(v2, zero), max_val);
        let c3 = _mm_min_epi32(_mm_max_epi32(v3, zero), max_val);

        // Pack i32 to i16
        let p0 = _mm_packs_epi32(c0, c1); // 8 x i16
        let p1 = _mm_packs_epi32(c2, c3); // 8 x i16

        // Pack i16 to i8
        let result = _mm_packs_epi16(p0, p1); // 16 x i8

        // Store 16 i8 values
        _mm_storeu_si128(output.as_mut_ptr().add(i) as *mut __m128i, result);

        i += CHUNK_SIZE;
    }

    // Handle remaining elements
    while i < size {
        output[i] = input[i].clamp(0, 127) as i8;
        i += 1;
    }
}

/// Transform 16-bit features to 8-bit with quantization **(SSE4.1)**.
///
/// * `us`    : perspective features for side-to-move (i16)
/// * `them`  : perspective features for opponent (i16)
/// * `output`: **mutable** array where quantized features are written
/// * `size`  : number of features per perspective
///
/// The output layout is: `[us[0..size], them[0..size]]` after shifting right by 6 bits
/// and clamping to [-127, 127].
///
/// # Safety
///
/// * **CPU feature**: The caller must ensure the current CPU supports **SSE4.1**  
///   (e.g. `is_x86_feature_detected!("sse4.1")` returns `true`) before calling.
/// * **Slice lengths**:  
///   - `us.len()     >= size`  
///   - `them.len()   >= size`  
///   - `output.len() >= size * 2`
/// * **Aliasing**: `output` must not alias `us` or `them`.
#[target_feature(enable = "sse4.1")]
#[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
pub unsafe fn transform_features_sse41(us: &[i16], them: &[i16], output: &mut [i8], size: usize) {
    // Debug assertions for boundary checks
    debug_assert!(us.len() >= size, "'us' buffer too small");
    debug_assert!(them.len() >= size, "'them' buffer too small");
    debug_assert!(output.len() >= size * 2, "Output buffer too small");
    // For simplicity, just call the scalar version
    // A real SSE4.1 implementation would use SIMD operations
    super::scalar::transform_features_scalar(us, them, output, size);
}

/// Update accumulator by adding or subtracting feature weights **(SSE4.1)**.
///
/// * `accumulator`: **mutable** array of 256 i16 values (current accumulator state)
/// * `weights`    : feature transformer weights array
/// * `indices`    : list of feature indices to add/remove
/// * `add`        : if true, add weights; if false, subtract weights
///
/// For each index in `indices`, the corresponding 256 weights starting at
/// `weights[index * 256]` are added to or subtracted from the accumulator
/// using saturating arithmetic.
///
/// # Safety
///
/// * **CPU feature**: The caller must ensure the current CPU supports **SSE4.1**  
///   (e.g. `is_x86_feature_detected!("sse4.1")` returns `true`) before calling.
/// * **Slice lengths**:  
///   - `accumulator.len() >= 256`  
///   - `weights.len() >= max(indices) * 256 + 256`
/// * **Index validity**: All values in `indices` must satisfy `index * 256 + 256 <= weights.len()`.
/// * **Aliasing**: `accumulator` must not alias `weights`.
/// * **Alignment**: Unaligned loads/stores are used, so alignment is not required.
#[target_feature(enable = "sse4.1")]
#[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
pub unsafe fn update_accumulator_sse41(
    accumulator: &mut [i16],
    weights: &[i16],
    indices: &[usize],
    add: bool,
) {
    // Debug assertions for boundary checks
    debug_assert!(accumulator.len() >= 256, "Accumulator must have at least 256 elements");
    for &idx in indices {
        debug_assert!(idx * 256 + 256 <= weights.len(), "Weight index {} out of bounds", idx);
    }
    const CHUNK_SIZE: usize = 32; // Process 32 elements per iteration (4 x 128-bit)

    for &idx in indices {
        let weight_offset = idx * 256;
        let weight_ptr = weights.as_ptr().add(weight_offset);

        let mut i = 0;
        // Process 32 elements at a time using 4 SSE registers
        while i + CHUNK_SIZE <= 256 {
            // Load 32 elements (4 x 8 i16)
            let acc0 = _mm_loadu_si128(accumulator.as_ptr().add(i) as *const __m128i);
            let acc1 = _mm_loadu_si128(accumulator.as_ptr().add(i + 8) as *const __m128i);
            let acc2 = _mm_loadu_si128(accumulator.as_ptr().add(i + 16) as *const __m128i);
            let acc3 = _mm_loadu_si128(accumulator.as_ptr().add(i + 24) as *const __m128i);

            let weight0 = _mm_loadu_si128(weight_ptr.add(i) as *const __m128i);
            let weight1 = _mm_loadu_si128(weight_ptr.add(i + 8) as *const __m128i);
            let weight2 = _mm_loadu_si128(weight_ptr.add(i + 16) as *const __m128i);
            let weight3 = _mm_loadu_si128(weight_ptr.add(i + 24) as *const __m128i);

            let result0 = if add {
                _mm_adds_epi16(acc0, weight0)
            } else {
                _mm_subs_epi16(acc0, weight0)
            };
            let result1 = if add {
                _mm_adds_epi16(acc1, weight1)
            } else {
                _mm_subs_epi16(acc1, weight1)
            };
            let result2 = if add {
                _mm_adds_epi16(acc2, weight2)
            } else {
                _mm_subs_epi16(acc2, weight2)
            };
            let result3 = if add {
                _mm_adds_epi16(acc3, weight3)
            } else {
                _mm_subs_epi16(acc3, weight3)
            };

            _mm_storeu_si128(accumulator.as_mut_ptr().add(i) as *mut __m128i, result0);
            _mm_storeu_si128(accumulator.as_mut_ptr().add(i + 8) as *mut __m128i, result1);
            _mm_storeu_si128(accumulator.as_mut_ptr().add(i + 16) as *mut __m128i, result2);
            _mm_storeu_si128(accumulator.as_mut_ptr().add(i + 24) as *mut __m128i, result3);

            i += CHUNK_SIZE;
        }

        // Handle remaining elements with SSE
        while i + 8 <= 256 {
            let acc_vec = _mm_loadu_si128(accumulator.as_ptr().add(i) as *const __m128i);
            let weight_vec = _mm_loadu_si128(weight_ptr.add(i) as *const __m128i);

            let result = if add {
                _mm_adds_epi16(acc_vec, weight_vec)
            } else {
                _mm_subs_epi16(acc_vec, weight_vec)
            };

            _mm_storeu_si128(accumulator.as_mut_ptr().add(i) as *mut __m128i, result);
            i += 8;
        }

        // Handle remaining elements (should not happen with 256 elements)
        while i < 256 {
            if add {
                accumulator[i] = accumulator[i].saturating_add(weights[weight_offset + i]);
            } else {
                accumulator[i] = accumulator[i].saturating_sub(weights[weight_offset + i]);
            }
            i += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simd_correctness() {
        // Test that SIMD implementations produce the same results as scalar
        let input = vec![10i8; 64];
        let weights = vec![1i8; 64 * 8]; // 8x64 matrix
        let biases = vec![100i32; 8];

        let mut output_scalar = vec![0i32; 8];
        let mut output_simd = vec![0i32; 8];

        // Scalar version
        super::super::scalar::affine_transform_scalar(
            &input,
            &weights,
            &biases,
            &mut output_scalar,
            64,
            8,
        );

        // SIMD version (if available)
        super::super::SimdDispatcher::affine_transform(
            &input,
            &weights,
            &biases,
            &mut output_simd,
            64,
            8,
        );

        // Results should match
        assert_eq!(output_scalar, output_simd);
    }

    #[test]
    fn test_avx2_affine_transform() {
        if !is_x86_feature_detected!("avx2") {
            eprintln!("AVX2 not available, skipping test");
            return;
        }

        let input = vec![5i8; 512];
        let weights = vec![2i8; 512 * 32]; // 32x512 matrix
        let biases = vec![1000i32; 32];
        let mut output = vec![0i32; 32];

        unsafe {
            affine_transform_avx2(&input, &weights, &biases, &mut output, 512, 32);
        }

        // Expected: 1000 + 5 * 2 * 512 = 1000 + 5120 = 6120
        for &val in &output {
            assert_eq!(val, 6120);
        }
    }

    #[test]
    fn test_sse41_affine_transform() {
        if !is_x86_feature_detected!("sse4.1") {
            eprintln!("SSE4.1 not available, skipping test");
            return;
        }

        let input = vec![3i8; 256];
        let weights = vec![4i8; 256 * 16]; // 16x256 matrix
        let biases = vec![500i32; 16];
        let mut output = vec![0i32; 16];

        unsafe {
            affine_transform_sse41(&input, &weights, &biases, &mut output, 256, 16);
        }

        // Expected: 500 + 3 * 4 * 256 = 500 + 3072 = 3572
        for &val in &output {
            assert_eq!(val, 3572);
        }
    }

    #[test]
    fn test_avx2_clipped_relu() {
        if !is_x86_feature_detected!("avx2") {
            eprintln!("AVX2 not available, skipping test");
            return;
        }

        let input = vec![-150i32, -50, 0, 50, 100, 150, 200];
        let mut output = vec![0i8; 7];

        unsafe {
            clipped_relu_avx2(&input, &mut output, 7);
        }

        assert_eq!(output, vec![0, 0, 0, 50, 100, 127, 127]);
    }

    #[test]
    fn test_sse41_clipped_relu() {
        if !is_x86_feature_detected!("sse4.1") {
            eprintln!("SSE4.1 not available, skipping test");
            return;
        }

        let input = vec![-100i32, -25, 0, 25, 50, 75, 100, 125, 150];
        let mut output = vec![0i8; 9];

        unsafe {
            clipped_relu_sse41(&input, &mut output, 9);
        }

        assert_eq!(output, vec![0, 0, 0, 25, 50, 75, 100, 125, 127]);
    }

    #[test]
    fn test_avx2_update_accumulator() {
        if !is_x86_feature_detected!("avx2") {
            eprintln!("AVX2 not available, skipping test");
            return;
        }

        let mut accumulator = vec![100i16; 256];
        let weights = vec![10i16; 512]; // Two features worth of weights
        let indices = vec![0, 1];

        // Test addition
        unsafe {
            update_accumulator_avx2(&mut accumulator, &weights, &indices, true);
        }

        // Each element should be 100 + 10 + 10 = 120
        for &val in &accumulator {
            assert_eq!(val, 120);
        }

        // Test subtraction
        unsafe {
            update_accumulator_avx2(&mut accumulator, &weights, &indices, false);
        }

        // Each element should be 120 - 10 - 10 = 100
        for &val in &accumulator {
            assert_eq!(val, 100);
        }
    }

    #[test]
    fn test_sse41_update_accumulator() {
        if !is_x86_feature_detected!("sse4.1") {
            eprintln!("SSE4.1 not available, skipping test");
            return;
        }

        let mut accumulator = vec![50i16; 256];
        let weights = vec![5i16; 256];
        let indices = vec![0];

        // Test addition
        unsafe {
            update_accumulator_sse41(&mut accumulator, &weights, &indices, true);
        }

        // Each element should be 50 + 5 = 55
        for &val in &accumulator {
            assert_eq!(val, 55);
        }
    }

    #[test]
    fn test_transform_features_alignment() {
        if !is_x86_feature_detected!("avx2") {
            eprintln!("AVX2 not available, skipping test");
            return;
        }

        // Test with various sizes to ensure alignment handling works
        for size in [256, 257, 300, 512].iter() {
            let us = vec![100i16; *size];
            let them = vec![200i16; *size];
            let mut output = vec![0i8; *size * 2];

            unsafe {
                transform_features_avx2(&us, &them, &mut output, *size);
            }

            // Verify the transformation
            for i in 0..*size {
                // Check that values are quantized correctly
                // 100 >> 6 = 1, 200 >> 6 = 3
                let expected_us = 1i8;
                let expected_them = 3i8;
                assert_eq!(output[i], expected_us, "Failed at index {} for us", i);
                assert_eq!(output[i + size], expected_them, "Failed at index {} for them", i);
            }
        }
    }
}
