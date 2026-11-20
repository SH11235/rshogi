use std::io::{self, BufWriter, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use rshogi_core::nnue::{
    FeatureTransformerLayerStacks, LsFeatureSpec, NNUENetwork, NUM_THREAT_CLASSES,
    ls_dispatch_ft_size, threat_pair_ranges,
};

#[derive(Parser)]
#[command(about = "LayerStacks Threat モデルの量子化 FT 重みをペアクラス別に集計")]
struct Cli {
    /// LayerStacks Threat モデルのパス
    nnue: PathBuf,
}

#[derive(Clone, Copy, Default)]
struct WeightStats {
    rows: usize,
    abs_sum: f64,
    square_sum: f64,
    row_l2_sum: f64,
}

impl WeightStats {
    fn accumulate<T: Copy + Into<f64>>(&mut self, weights: &[T], width: usize) {
        for row in weights.chunks_exact(width) {
            let mut row_square_sum = 0.0;
            for &weight in row {
                let weight = weight.into();
                self.abs_sum += weight.abs();
                row_square_sum += weight * weight;
            }
            self.rows += 1;
            self.square_sum += row_square_sum;
            self.row_l2_sum += row_square_sum.sqrt();
        }
    }

    fn write(self, out: &mut impl Write, pair: &str, width: usize) -> io::Result<()> {
        if self.rows == 0 {
            return writeln!(out, "{pair}\t0\t{width}\tNA\tNA\tNA");
        }
        let elements = (self.rows * width) as f64;
        let mean_abs = self.abs_sum / elements;
        let rms = (self.square_sum / elements).sqrt();
        let mean_row_l2 = self.row_l2_sum / self.rows as f64;
        writeln!(out, "{pair}\t{}\t{width}\t{mean_abs:.9}\t{rms:.9}\t{mean_row_l2:.9}", self.rows)
    }
}

fn print_stats<const L1: usize, FT: LsFeatureSpec>(
    ft: &FeatureTransformerLayerStacks<L1, FT>,
) -> Result<()> {
    let (threat_width, threat_weights) =
        ft.threat_ft_weights().context("モデルに Threat FT 重みがありません")?;
    let mut threat = [[[WeightStats::default(); 2]; NUM_THREAT_CLASSES]; NUM_THREAT_CLASSES];
    for (ac, dc, same, rows) in threat_pair_ranges() {
        threat[ac][dc][usize::from(!same)].accumulate(
            &threat_weights[rows.start * threat_width..rows.end * threat_width],
            threat_width,
        );
    }

    let mut out = BufWriter::new(io::stdout().lock());
    writeln!(out, "pair_class\trows\twidth\tmean_abs\trms\tmean_row_l2")?;
    for (ac, targets) in threat.iter().enumerate() {
        for (dc, by_side) in targets.iter().enumerate() {
            for (stats, side) in by_side.iter().zip(["same", "opp"]) {
                stats.write(&mut out, &format!("{ac}_{dc}_{side}"), threat_width)?;
            }
        }
    }
    let mut piece = WeightStats::default();
    piece.accumulate(ft.piece_ft_weights(), L1);
    piece.write(&mut out, "piece", L1)?;
    out.flush()?;
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let net = NNUENetwork::load_static_layer_stacks(&cli.nnue)
        .with_context(|| format!("LayerStacks モデルを読み込めません: {}", cli.nnue.display()))?;
    ls_dispatch_ft_size!(
        &net,
        |concrete_net| print_stats(&concrete_net.feature_transformer),
        _ => anyhow::bail!("このビルドで有効な LayerStacks 構成がありません"),
    )
}
