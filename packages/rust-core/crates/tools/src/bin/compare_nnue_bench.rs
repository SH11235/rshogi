use anyhow::{Context, Result};
use clap::Parser;
use serde_json::{json, Value};
use std::cmp::Ordering;
use std::fs;

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Compare nnue_benchmark JSON metrics (fixed-line EPS)"
)]
struct Args {
    /// Path to the head JSON file (new measurements)
    head_json: String,
    /// Path to the base JSON file (baseline measurements)
    base_json: String,

    /// Update-only negative threshold in percent (e.g., -15 means 15% slower triggers warn)
    #[arg(long, default_value_t = -15.0)]
    update_threshold: f64,
    /// Eval-included negative threshold in percent
    #[arg(long, default_value_t = -10.0)]
    eval_threshold: f64,
    /// Minimum baseline EPS for update metrics to consider threshold
    #[arg(long, default_value_t = 100_000.0)]
    update_baseline_min: f64,
    /// Minimum baseline EPS for eval metrics to consider threshold
    #[arg(long, default_value_t = 50_000.0)]
    eval_baseline_min: f64,

    /// Exit non-zero when warnings are present (for CI gating)
    #[arg(long, action = clap::ArgAction::SetTrue)]
    fail_on_warn: bool,
}

fn read_json(path: &str) -> Result<Value> {
    let content = fs::read_to_string(path).with_context(|| format!("Failed to read {path}"))?;
    let v: Value =
        serde_json::from_str(&content).with_context(|| format!("Failed to parse JSON: {path}"))?;
    Ok(v)
}

fn get_metric(v: &Value, key: &str) -> Option<f64> {
    v.get("metrics")
        .and_then(|m| m.get(key))
        .and_then(|x| x.as_f64().or_else(|| x.as_u64().map(|u| u as f64)))
}

fn pct_delta(head: f64, base: f64) -> Option<f64> {
    if base == 0.0 {
        None
    } else {
        Some(100.0 * (head - base) / base)
    }
}

#[derive(Debug)]
struct WarnItem {
    metric: String,
    base: f64,
    head: f64,
    delta: f64, // percent
}

fn main() -> Result<()> {
    let args = Args::parse();
    let head = read_json(&args.head_json)?;
    let base = read_json(&args.base_json)?;

    // Metrics to compare (name, threshold, base_min)
    // Focus on apply/chain for both series; also include refresh for visibility (but same thresholds)
    #[derive(Clone, Copy)]
    struct TargetEx<'a> {
        key: &'a str,
        thr: f64,
        base_min: f64,
        warnable: bool,
    }
    let targets = [
        // Update-only (warnable)
        TargetEx {
            key: "apply_update_eps",
            thr: args.update_threshold,
            base_min: args.update_baseline_min,
            warnable: true,
        },
        TargetEx {
            key: "chain_update_eps",
            thr: args.update_threshold,
            base_min: args.update_baseline_min,
            warnable: true,
        },
        TargetEx {
            key: "refresh_update_eps",
            thr: args.update_threshold,
            base_min: args.update_baseline_min,
            warnable: false,
        },
        // Eval-included (warnable)
        TargetEx {
            key: "apply_eval_eps",
            thr: args.eval_threshold,
            base_min: args.eval_baseline_min,
            warnable: true,
        },
        TargetEx {
            key: "chain_eval_eps",
            thr: args.eval_threshold,
            base_min: args.eval_baseline_min,
            warnable: true,
        },
        TargetEx {
            key: "refresh_eval_eps",
            thr: args.eval_threshold,
            base_min: args.eval_baseline_min,
            warnable: false,
        },
    ];

    let mut warns: Vec<WarnItem> = Vec::new();
    let mut deltas_out: Vec<Value> = Vec::new();

    for t in targets.iter() {
        match (get_metric(&head, t.key), get_metric(&base, t.key)) {
            (Some(h), Some(b)) => {
                if let Some(delta) = pct_delta(h, b) {
                    deltas_out.push(json!({ "metric": t.key, "base": b, "head": h, "delta_pct": (delta * 10.0).round()/10.0 }));
                    if t.warnable && b >= t.base_min && delta < t.thr {
                        warns.push(WarnItem {
                            metric: t.key.to_string(),
                            base: b,
                            head: h,
                            delta,
                        });
                    }
                }
            }
            _ => {
                deltas_out.push(json!({ "metric": t.key, "status": "missing" }));
            }
        }
    }

    // Also compare speedup ratios (apply/refresh eval, chain/refresh eval) — optional advisory
    fn safe_ratio(v: Option<f64>, denom: Option<f64>) -> Option<f64> {
        match (v, denom) {
            (Some(n), Some(d)) if d > 0.0 => Some(n / d),
            _ => None,
        }
    }
    let h_apply = get_metric(&head, "apply_eval_eps");
    let h_chain = get_metric(&head, "chain_eval_eps");
    let h_refresh = get_metric(&head, "refresh_eval_eps");
    let b_apply = get_metric(&base, "apply_eval_eps");
    let b_chain = get_metric(&base, "chain_eval_eps");
    let b_refresh = get_metric(&base, "refresh_eval_eps");
    if let (Some(hr), Some(br)) = (safe_ratio(h_apply, h_refresh), safe_ratio(b_apply, b_refresh)) {
        if let Some(delta) = pct_delta(hr, br) {
            deltas_out.push(json!({ "metric": "speedup_apply_eval", "base": br, "head": hr, "delta_pct": (delta * 10.0).round()/10.0 }));
        }
    }
    if let (Some(hr), Some(br)) = (safe_ratio(h_chain, h_refresh), safe_ratio(b_chain, b_refresh)) {
        if let Some(delta) = pct_delta(hr, br) {
            deltas_out.push(json!({ "metric": "speedup_chain_eval", "base": br, "head": hr, "delta_pct": (delta * 10.0).round()/10.0 }));
        }
    }

    // Sort warnings by delta ascending (most negative first)
    warns.sort_by(|a, b| a.delta.partial_cmp(&b.delta).unwrap_or(Ordering::Equal));

    // Build JSON result
    let warns_json: Vec<Value> = warns
        .iter()
        .map(|w| {
            json!({
                "metric": w.metric,
                "base": (w.base * 10.0).round()/10.0,
                "head": (w.head * 10.0).round()/10.0,
                "delta_pct": (w.delta * 10.0).round()/10.0,
            })
        })
        .collect();

    // Environment & line compatibility checks
    let mut notices: Vec<String> = Vec::new();
    let get = |v: &Value, path: &[&str]| -> Option<Value> {
        let mut cur = v;
        for k in path {
            cur = cur.get(*k)?;
        }
        Some(cur.clone())
    };
    if get(&head, &["env", "schema_version"]) != get(&base, &["env", "schema_version"]) {
        notices.push("schema_version differs; comparison may be invalid".into());
    }
    for k in ["uid", "acc_dim", "n_feat"] {
        if get(&head, &["env", "weights", k]) != get(&base, &["env", "weights", k]) {
            notices.push(format!("weights.{k} differs; skip WARN (advisory only)"));
        }
    }
    for k in ["mode", "line_len", "cases"] {
        if get(&head, &["line", k]) != get(&base, &["line", k]) {
            notices.push(format!("line.{k} differs; skip WARN (advisory only)"));
        }
    }
    let suppress_warns = notices.iter().any(|s| s.contains("weights.") || s.contains("line."));
    let warns_json_out = if suppress_warns {
        Vec::new()
    } else {
        warns_json
    };

    let result = json!({
        "head": head.get("env").cloned().unwrap_or(json!({})),
        "base": base.get("env").cloned().unwrap_or(json!({})),
        "compare": deltas_out,
        "warns": warns_json_out,
        "notices": notices,
    });

    // JSON -> stdout
    println!("{}", serde_json::to_string_pretty(&result)?);
    // WARN lines -> stderr
    if !warns.is_empty() && !suppress_warns {
        eprintln!("\nWARN: nnue_benchmark regressions detected (worst first):");
        for w in &warns {
            eprintln!(
                "- {}: {delta:.1}% (head={h:.0}, base={b:.0})",
                w.metric,
                delta = w.delta,
                h = w.head,
                b = w.base
            );
        }
    }

    if !warns.is_empty() && !suppress_warns && args.fail_on_warn {
        std::process::exit(2);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pct_delta() {
        assert_eq!(pct_delta(110.0, 100.0).unwrap().round(), 10.0);
        assert_eq!(pct_delta(90.0, 100.0).unwrap().round(), -10.0);
        assert!(pct_delta(1.0, 0.0).is_none());
    }
}
