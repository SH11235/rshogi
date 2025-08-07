//! Benchmark comparison tool
//!
//! Compares current benchmark results with a baseline to detect regressions
//!
//! Usage: benchmark_compare <BASELINE> <CURRENT> [OPTIONS]

use anyhow::{Context, Result};
use clap::Parser;
use engine_core::benchmark::metrics::{
    compare_benchmarks, format_regression_report, BenchmarkSummary,
};
use std::{fs, path::PathBuf};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Baseline benchmark results file
    baseline: PathBuf,

    /// Current benchmark results file
    current: PathBuf,

    /// Regression tolerance percentage
    #[arg(short, long, default_value_t = 2.0)]
    tolerance: f64,

    /// Output format (text, json, markdown)
    #[arg(short, long, default_value = "text")]
    format: String,

    /// Output file (defaults to stdout)
    #[arg(short, long)]
    output: Option<PathBuf>,
}

fn main() -> Result<()> {
    let args = Args::parse();

    // Load baseline results
    let baseline_json = fs::read_to_string(&args.baseline)
        .with_context(|| format!("Failed to read baseline file: {}", args.baseline.display()))?;
    let baseline: BenchmarkSummary = serde_json::from_str(&baseline_json)
        .context("Failed to parse baseline benchmark results")?;

    // Load current results
    let current_json = fs::read_to_string(&args.current)
        .with_context(|| format!("Failed to read current file: {}", args.current.display()))?;
    let current: BenchmarkSummary =
        serde_json::from_str(&current_json).context("Failed to parse current benchmark results")?;

    // Compare benchmarks
    let report = compare_benchmarks(&baseline, &current, args.tolerance);

    // Format output
    let output = match args.format.as_str() {
        "json" => serde_json::to_string_pretty(&report)?,
        "markdown" | "md" => format_regression_report_markdown(&report),
        _ => format_regression_report(&report),
    };

    // Write output
    if let Some(output_file) = args.output {
        fs::write(output_file, output)?;
    } else {
        print!("{output}");
    }

    // Exit with error code if regression detected
    if report.has_regression {
        std::process::exit(1);
    }

    Ok(())
}

/// Format regression report as markdown (for GitHub comments)
fn format_regression_report_markdown(
    report: &engine_core::benchmark::metrics::RegressionReport,
) -> String {
    let mut output = String::new();

    output.push_str("## 🔍 Benchmark Comparison Report\n\n");

    if !report.has_regression {
        output.push_str("### ✅ No performance regressions detected\n\n");
        output.push_str("All benchmarks are within acceptable tolerance.\n");
        return output;
    }

    output.push_str("### ❌ Performance regressions detected\n\n");

    // Format individual regressions as a table
    if !report.regressions.is_empty() {
        output.push_str("#### Metric Regressions\n\n");
        output.push_str("| Metric | Threads | Baseline | Current | Change |\n");
        output.push_str("|--------|---------|----------|---------|--------|\n");

        for reg in &report.regressions {
            output.push_str(&format!(
                "| {} | {} | {:.1} | {:.1} | {:+.1}% |\n",
                reg.metric,
                reg.thread_count,
                reg.baseline_value,
                reg.current_value,
                reg.change_percent
            ));
        }
        output.push('\n');
    }

    // Format target degradations
    if !report.targets_degraded.is_empty() {
        output.push_str("#### Performance Targets No Longer Met\n\n");
        for target in &report.targets_degraded {
            output.push_str(&format!("- ⚠️ {target}\n"));
        }
        output.push('\n');
    }

    // Add commit information
    if let (Some(baseline), Some(current)) = (&report.baseline_commit, &report.current_commit) {
        output.push_str(&format!("📊 Comparing `{baseline}` (baseline) → `{current}` (current)\n"));
    }

    output
}
