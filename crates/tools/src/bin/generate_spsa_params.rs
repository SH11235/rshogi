use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use rshogi_core::search::SearchTuneParams;

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "SearchTuneParams::option_specs() から SPSA .params を生成する"
)]
struct Cli {
    /// 出力先 .params ファイル
    #[arg(long)]
    output: PathBuf,
}

/// Fishtest 互換の c_end（最終摂動幅）。`(max - min) / 20` で最低 1。
fn c_end(min: i32, max: i32) -> i64 {
    let range = (max - min) as f64;
    ((range / 20.0).round() as i64).max(1)
}

/// Fishtest 互換の r_end（最終学習率係数）。固定 0.002。
fn r_end() -> f64 {
    0.002
}

fn format_float(value: f64) -> String {
    let mut rendered = format!("{value:.6}");
    while rendered.contains('.') && rendered.ends_with('0') {
        rendered.pop();
    }
    if rendered.ends_with('.') {
        rendered.pop();
    }
    rendered
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let file = File::create(&cli.output)
        .with_context(|| format!("failed to create {}", cli.output.display()))?;
    let mut writer = BufWriter::new(file);

    for spec in SearchTuneParams::option_specs() {
        let c = c_end(spec.min, spec.max);
        let r = r_end();
        writeln!(
            writer,
            "{},int,{},{},{},{},{}",
            spec.usi_name,
            spec.default,
            spec.min,
            spec.max,
            c,
            format_float(r)
        )?;
    }
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn c_end_has_minimum_one() {
        assert_eq!(c_end(0, 5), 1);
        assert_eq!(c_end(-3, 3), 1);
    }

    #[test]
    fn c_end_is_range_div_20() {
        assert_eq!(c_end(0, 1000), 50);
        assert_eq!(c_end(-1000, 1000), 100);
        assert_eq!(c_end(0, 20), 1);
    }

    #[test]
    fn r_end_is_constant() {
        assert!((r_end() - 0.002).abs() < f64::EPSILON);
    }

    #[test]
    fn format_float_trims_trailing_zeros() {
        assert_eq!(format_float(12.0), "12");
        assert_eq!(format_float(0.125000), "0.125");
        assert_eq!(format_float(0.000001), "0.000001");
    }
}
