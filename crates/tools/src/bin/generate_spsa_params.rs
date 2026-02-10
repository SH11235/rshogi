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

fn int_step(min: i32, max: i32) -> i64 {
    let range = (max - min) as f64;
    ((range / 200.0).round() as i64).max(1)
}

fn delta(min: i32, max: i32) -> f64 {
    (max - min) as f64 / 20.0
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
        let step = int_step(spec.min, spec.max);
        let delta = delta(spec.min, spec.max);
        writeln!(
            writer,
            "{},int,{},{},{},{},{}",
            spec.usi_name,
            spec.default,
            spec.min,
            spec.max,
            step,
            format_float(delta)
        )?;
    }
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn int_step_has_minimum_one() {
        assert_eq!(int_step(0, 5), 1);
        assert_eq!(int_step(-3, 3), 1);
    }

    #[test]
    fn int_step_rounds_range_div_200() {
        assert_eq!(int_step(0, 1000), 5);
        assert_eq!(int_step(0, 260), 1);
        assert_eq!(int_step(0, 340), 2);
    }

    #[test]
    fn delta_is_range_div_20() {
        assert_eq!(delta(0, 100), 5.0);
        assert_eq!(delta(-20, 20), 2.0);
    }

    #[test]
    fn format_float_trims_trailing_zeros() {
        assert_eq!(format_float(12.0), "12");
        assert_eq!(format_float(0.125000), "0.125");
        assert_eq!(format_float(0.000001), "0.000001");
    }
}
