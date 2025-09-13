use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub fn version() -> String {
    format!("engine-wasm {}", env!("CARGO_PKG_VERSION"))
}

#[wasm_bindgen]
pub fn bench_add_row_scaled(len: usize, k: f32, reps: u32) -> f64 {
    // Prepare deterministic input
    let mut dst = vec![0.0f32; len];
    let mut row = vec![0.0f32; len];
    // Use iterator enumeration (clippy: needless_range_loop)
    for (i, v) in row.iter_mut().enumerate() {
        if i == len {
            break;
        } // defensive (though enumerate won't exceed)
        *v = ((i as f32 + 3.0) * 0.002).cos();
    }

    let mut acc: f32 = 0.0;
    for _ in 0..reps {
        engine_core::simd::add_row_scaled_f32(&mut dst, &row, k);
        if !dst.is_empty() {
            acc += dst[dst.len() / 2];
        }
    }
    acc as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::*;

    wasm_bindgen_test_configure!(run_in_browser);

    #[wasm_bindgen_test]
    fn test_bench_stub() {
        let v = bench_add_row_scaled(16, 1.0, 1);
        // Just sanity check to ensure it runs
        assert!(v.is_finite());
    }
}
