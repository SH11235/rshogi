// .github/workflows/nnue-bench-regression.yml uses this to combine multiple nnue_benchmark runs.

use anyhow::{bail, Context, Result};
use clap::Parser;
use serde_json::{json, Value};
use std::fs;

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Combine multiple nnue_benchmark JSON files by taking medians of metrics"
)]
struct Args {
    /// Output JSON path
    #[arg(short, long, value_name = "FILE")]
    output: String,

    /// Input JSON files (at least 2 recommended)
    #[arg(required = true)]
    inputs: Vec<String>,
}

fn read_json(path: &str) -> Result<Value> {
    let s = fs::read_to_string(path).with_context(|| format!("Failed to read {path}"))?;
    let v: Value = serde_json::from_str(&s).with_context(|| format!("Invalid JSON: {path}"))?;
    Ok(v)
}

fn median(mut xs: Vec<f64>) -> Option<f64> {
    if xs.is_empty() {
        return None;
    }
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = xs.len();
    if n % 2 == 1 {
        Some(xs[n / 2])
    } else {
        Some((xs[n / 2 - 1] + xs[n / 2]) / 2.0)
    }
}

fn get_num(v: &Value, path: &[&str]) -> Option<f64> {
    let mut cur = v;
    for k in path {
        cur = cur.get(*k)?;
    }
    cur.as_f64().or_else(|| cur.as_u64().map(|u| u as f64))
}

fn main() -> Result<()> {
    let args = Args::parse();

    // Load existing inputs only
    let mut js: Vec<Value> = Vec::new();
    for p in &args.inputs {
        match read_json(p) {
            Ok(v) => js.push(v),
            Err(_) => {
                // Silently skip missing/invalid files to mirror CI behavior
                // (the workflow ensures at least 2 exist or fails)
            }
        }
    }
    if js.len() < 2 {
        bail!("Insufficient inputs: found {} (<2)", js.len());
    }

    // Keys to combine via median
    let keys = [
        "refresh_update_eps",
        "apply_update_eps",
        "chain_update_eps",
        "refresh_eval_eps",
        "apply_eval_eps",
        "chain_eval_eps",
    ];

    // env from first
    let mut out = json!({
        "env": js[0].get("env").cloned().unwrap_or(json!({})),
    });

    // line meta: combined cases and average line_len (rounded)
    let mut lens: Vec<f64> = Vec::new();
    for j in &js {
        if let Some(len) = get_num(j, &["line", "line_len"]) {
            lens.push(len);
        }
    }
    let line_len_avg = if lens.is_empty() {
        0
    } else {
        let sum: f64 = lens.iter().sum();
        (sum / lens.len() as f64).round() as i64
    };
    out["line"] = json!({
        "mode": "combined",
        "cases": js.len(),
        "line_len": line_len_avg,
    });

    let mut metrics = serde_json::Map::new();
    for k in keys.iter() {
        let mut vals: Vec<f64> = Vec::new();
        for j in &js {
            if let Some(v) = get_num(j, &["metrics", k]) {
                vals.push(v);
            }
        }
        if let Some(m) = median(vals) {
            metrics.insert((*k).to_string(), json!(m.round() as i64));
        }
    }
    // seconds median
    let mut secs: Vec<f64> = Vec::new();
    for j in &js {
        if let Some(s) = get_num(j, &["metrics", "seconds"]) {
            secs.push(s);
        }
    }
    if let Some(m) = median(secs) {
        metrics.insert("seconds".to_string(), json!(m as i64));
    }
    out["metrics"] = Value::Object(metrics);

    // Write
    fs::write(&args.output, serde_json::to_string_pretty(&out)?)
        .with_context(|| format!("Failed to write output JSON: {}", args.output))?;

    Ok(())
}
