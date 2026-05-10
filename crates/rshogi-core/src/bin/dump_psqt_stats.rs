//! PSQT 重み統計ダンプツール
//!
//! quantised.bin から PSQT 重み・バイアスを読み出し、各 bucket ごとの L2 norm
//! と全体統計を出力する。複数 checkpoint を渡せば step 進行に対する変化を
//! 観察できる。
//!
//! 使用例:
//! ```bash
//! cargo run --release --bin dump_psqt_stats --features nnue-psqt -- \
//!   /path/to/v101-100/quantised.bin \
//!   /path/to/v101-200/quantised.bin \
//!   /path/to/v101-380/quantised.bin
//! ```

#[cfg(not(feature = "nnue-psqt"))]
fn main() {
    eprintln!("Error: dump_psqt_stats requires --features nnue-psqt");
    std::process::exit(1);
}

#[cfg(feature = "nnue-psqt")]
fn main() {
    use rshogi_core::nnue::{NUM_LAYER_STACK_BUCKETS, NetworkLayerStacks1536x16x32};
    use std::path::Path;

    // PSQT bucket 数はライブラリ側の定数を直接参照し、
    // モデル仕様変更時にツール側を追従させる必要をなくす。
    const NUM_BUCKETS: usize = NUM_LAYER_STACK_BUCKETS;

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("Usage: dump_psqt_stats <quantised.bin> [<quantised.bin> ...]");
        std::process::exit(1);
    }

    println!(
        "{:<60} {:>12} {:>12} {:>10} {:>10} {:>10} {:>10}",
        "model", "L2_total", "L2_bias", "min_w", "max_w", "mean|w|", "nonzero%"
    );
    println!("{}", "-".repeat(140));

    for path_str in &args {
        let path = Path::new(path_str);
        let net = match NetworkLayerStacks1536x16x32::load(path) {
            Ok(n) => n,
            Err(e) => {
                eprintln!("Error loading {}: {e}", path.display());
                continue;
            }
        };

        let ft = &net.feature_transformer;
        if !ft.has_psqt() {
            println!("{:<60} (no PSQT)", path.display());
            continue;
        }

        let weights: &[i32] = ft.psqt_weights();
        let biases = ft.psqt_biases();

        // 全体統計
        let total_count = weights.len();
        let nonzero = weights.iter().filter(|&&w| w != 0).count();
        let nonzero_pct = nonzero as f64 / total_count as f64 * 100.0;
        let min_w = *weights.iter().min().unwrap_or(&0);
        let max_w = *weights.iter().max().unwrap_or(&0);
        let abs_sum: u64 = weights.iter().map(|&w| (w as i64).unsigned_abs()).sum();
        let mean_abs = abs_sum as f64 / total_count as f64;

        // L2 norm (overall)
        let l2_total: f64 = (weights.iter().map(|&w| (w as f64).powi(2)).sum::<f64>()).sqrt();
        let l2_bias: f64 = (biases.iter().map(|&b| (b as f64).powi(2)).sum::<f64>()).sqrt();

        let label = path
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string());

        println!(
            "{:<60} {:>12.0} {:>12.0} {:>10} {:>10} {:>10.2} {:>9.2}%",
            label, l2_total, l2_bias, min_w, max_w, mean_abs, nonzero_pct
        );

        // Per-bucket L2 norm
        debug_assert_eq!(
            total_count % NUM_BUCKETS,
            0,
            "psqt_weights length must be divisible by NUM_BUCKETS"
        );
        let halfka_dim = total_count / NUM_BUCKETS;
        let mut per_bucket_l2 = [0.0f64; NUM_BUCKETS];
        let mut per_bucket_max = [i32::MIN; NUM_BUCKETS];
        let mut per_bucket_min = [i32::MAX; NUM_BUCKETS];
        for feature_idx in 0..halfka_dim {
            for bucket in 0..NUM_BUCKETS {
                let w = weights[feature_idx * NUM_BUCKETS + bucket];
                per_bucket_l2[bucket] += (w as f64).powi(2);
                per_bucket_max[bucket] = per_bucket_max[bucket].max(w);
                per_bucket_min[bucket] = per_bucket_min[bucket].min(w);
            }
        }
        for v in &mut per_bucket_l2 {
            *v = v.sqrt();
        }

        print!("    per-bucket L2: ");
        for (i, l2) in per_bucket_l2.iter().enumerate() {
            print!("b{i}={l2:.0} ");
        }
        println!();
        print!("    per-bucket bias: ");
        for (i, b) in biases.iter().enumerate() {
            print!("b{i}={b} ");
        }
        println!();
        print!("    per-bucket [min,max]: ");
        for i in 0..NUM_BUCKETS {
            print!("b{i}=[{},{}] ", per_bucket_min[i], per_bucket_max[i]);
        }
        println!();
        println!();
    }
}
