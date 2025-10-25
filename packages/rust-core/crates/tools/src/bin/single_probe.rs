use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Probe SINGLE fp32 net on a given SFEN (or startpos)"
)]
struct Cli {
    /// Path to SINGLE weights
    #[arg(long, value_name = "FILE")]
    weights: PathBuf,
    /// Optional SFEN (default: startpos)
    #[arg(long)]
    sfen: Option<String>,
}

fn main() -> Result<()> {
    env_logger::init();
    let cli = Cli::parse();
    let p = cli.weights.to_string_lossy().to_string();
    let net = engine_core::evaluation::nnue::weights::load_single_weights(&p)
        .with_context(|| format!("failed to load SINGLE weights at {}", p))?;

    let pos = if let Some(sfen) = cli.sfen.as_deref() {
        match engine_core::Position::from_sfen(sfen) {
            Ok(p) => p,
            Err(e) => anyhow::bail!("invalid sfen: {} ({})", sfen, e),
        }
    } else {
        engine_core::Position::startpos()
    };

    // Build pre (both sides) using refresh
    let acc = engine_core::evaluation::nnue::single_state::SingleAcc::refresh(&pos, &net);

    let pre = acc.acc_for(pos.side_to_move);

    // Compute cp as f32 with same logic as SingleChannelNet::evaluate_from_accumulator_pre
    let mut cp_f = net.b2;
    for (&w, &p) in net.w2.iter().zip(pre.iter()) {
        let a = p.max(0.0);
        cp_f += w * a;
    }
    let cp_i = cp_f.clamp(-32000.0, 32000.0) as i32;
    let cp_scaled_i = (cp_f * net.scale).clamp(-32000.0, 32000.0) as i32;

    println!(
        "single_probe input_dim={} acc_dim={} uid=0x{:016x} cp_f={:.6} cp_i={} cp_i_scaled={} scale={}",
        net.n_feat, net.acc_dim, net.uid, cp_f, cp_i, cp_scaled_i, net.scale
    );

    Ok(())
}
