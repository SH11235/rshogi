use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Inspect SINGLE_CHANNEL FP32 NNUE weights (dims/uid/range)"
)]
struct Cli {
    /// Path to SINGLE fp32 weights (trainer format with END_HEADER)
    #[arg(value_name = "FILE")]
    path: PathBuf,
}

fn main() -> Result<()> {
    env_logger::init();
    let cli = Cli::parse();
    let p = cli.path.to_string_lossy().to_string();
    let net = engine_core::evaluation::nnue::weights::load_single_weights(&p)
        .with_context(|| format!("failed to load SINGLE weights at {}", p))?;

    let uid = net.uid;
    let input_dim = net.n_feat;
    let acc_dim = net.acc_dim;
    let (w2_min, w2_max) = min_max_f32(&net.w2);
    let b2 = net.b2;
    let b0_present = net.b0.as_ref().map(|b| b.len()).unwrap_or(0);

    println!(
        "single_inspect path={} input_dim={} acc_dim={} uid=0x{:016x} w2_min={} w2_max={} b2={} b0_len={} w0_len={}",
        p,
        input_dim,
        acc_dim,
        uid,
        w2_min,
        w2_max,
        b2,
        b0_present,
        net.w0.len()
    );

    Ok(())
}

fn min_max_f32(xs: &[f32]) -> (f32, f32) {
    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;
    for &v in xs {
        if v.is_finite() {
            if v < min {
                min = v;
            }
            if v > max {
                max = v;
            }
        }
    }
    if !min.is_finite() {
        min = 0.0;
    }
    if !max.is_finite() {
        max = 0.0;
    }
    (min, max)
}
