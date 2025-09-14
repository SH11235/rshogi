use anyhow::{Context, Result};
use clap::Parser;
use serde_json::Value;
use std::fs;

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Summarize compare_nnue_bench JSON into Markdown table + worst regression"
)]
struct Args {
    /// Path to compare.json produced by compare_nnue_bench
    input: String,
}

fn read_json(path: &str) -> Result<Value> {
    let s = fs::read_to_string(path).with_context(|| format!("Failed to read {path}"))?;
    let v: Value = serde_json::from_str(&s).with_context(|| format!("Invalid JSON: {path}"))?;
    Ok(v)
}

fn main() -> Result<()> {
    let args = Args::parse();
    let v = read_json(&args.input)?;

    // Key metrics table
    println!("\n### Key metrics\n");
    println!("| metric | base | head | delta% |");
    println!("|---|---:|---:|---:|");
    if let Some(arr) = v.get("compare").and_then(|x| x.as_array()) {
        for key in [
            "apply_update_eps",
            "chain_update_eps",
            "apply_eval_eps",
            "chain_eval_eps",
        ] {
            if let Some(item) =
                arr.iter().find(|it| it.get("metric").and_then(|m| m.as_str()) == Some(key))
            {
                let b = item
                    .get("base")
                    .and_then(|x| x.as_f64())
                    .map(|x| format!("{:.0}", x))
                    .unwrap_or("-".into());
                let h = item
                    .get("head")
                    .and_then(|x| x.as_f64())
                    .map(|x| format!("{:.0}", x))
                    .unwrap_or("-".into());
                let d = item
                    .get("delta_pct")
                    .and_then(|x| x.as_f64())
                    .map(|x| format!("{:.1}", x))
                    .unwrap_or("-".into());
                println!("| {key} | {b} | {h} | {d}% |");
            }
        }
    }

    // Worst regression section (if any)
    if let Some(warns) = v.get("warns").and_then(|x| x.as_array()) {
        if let Some(worst) = warns.iter().min_by(|a, b| {
            let da = a.get("delta_pct").and_then(|x| x.as_f64()).unwrap_or(0.0);
            let db = b.get("delta_pct").and_then(|x| x.as_f64()).unwrap_or(0.0);
            da.partial_cmp(&db).unwrap()
        }) {
            let metric = worst.get("metric").and_then(|x| x.as_str()).unwrap_or("");
            let base = worst.get("base").and_then(|x| x.as_f64()).unwrap_or(0.0);
            let head = worst.get("head").and_then(|x| x.as_f64()).unwrap_or(0.0);
            let delta = worst.get("delta_pct").and_then(|x| x.as_f64()).unwrap_or(0.0);
            println!("\n### Worst regression\n");
            println!("- {}: {:.1}%  (head={:.0}, base={:.0})\n", metric, delta, head, base);
        }
    }

    Ok(())
}
