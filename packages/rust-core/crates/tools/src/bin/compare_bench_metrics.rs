use anyhow::{Context, Result};
use clap::Parser;
use serde_json::{json, Value};
use std::fs;

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Compare benchmark metrics between head and base"
)]
struct Args {
    /// Path to the head metrics JSON file
    head_json: String,

    /// Path to the base metrics JSON file
    base_json: String,
}

fn calculate_percent_delta(new_val: f64, base_val: f64) -> Option<f64> {
    if base_val == 0.0 {
        None
    } else {
        Some(100.0 * (new_val - base_val) / base_val)
    }
}

fn compare_metrics(head: &Value, base: &Value) -> (Vec<String>, Value) {
    let mut warnings = Vec::new();

    // Thresholds for warnings
    const SAMPLES_PER_SEC_NEG_THRESHOLD: f64 = -15.0; // head slower than base by >15%
    const HWM_POS_THRESHOLD: f64 = 20.0; // head HWM higher than base by >20%
    const TIME_POS_THRESHOLD: f64 = 20.0; // head seconds higher than base by >20%

    // Compare samples_per_sec (lower is bad)
    if let (Some(head_sps), Some(base_sps)) = (
        head.get("samples_per_sec").and_then(Value::as_f64),
        base.get("samples_per_sec").and_then(Value::as_f64),
    ) {
        if let Some(delta) = calculate_percent_delta(head_sps, base_sps) {
            if delta < SAMPLES_PER_SEC_NEG_THRESHOLD {
                warnings.push(format!(
                    "samples_per_sec regression: {:.1}% (head={}, base={})",
                    delta, head_sps, base_sps
                ));
            }
        }
    }

    // Compare hwm_mb_last (higher is bad)
    if let (Some(head_hwm), Some(base_hwm)) = (
        head.get("hwm_mb_last").and_then(Value::as_u64),
        base.get("hwm_mb_last").and_then(Value::as_u64),
    ) {
        if let Some(delta) = calculate_percent_delta(head_hwm as f64, base_hwm as f64) {
            if delta > HWM_POS_THRESHOLD {
                warnings.push(format!(
                    "peak RSS increase: +{:.1}% (head={}MB, base={}MB)",
                    delta, head_hwm, base_hwm
                ));
            }
        }
    }

    // Compare seconds (higher is bad)
    if let (Some(head_seconds), Some(base_seconds)) = (
        head.get("seconds").and_then(Value::as_f64),
        base.get("seconds").and_then(Value::as_f64),
    ) {
        if let Some(delta) = calculate_percent_delta(head_seconds, base_seconds) {
            if delta > TIME_POS_THRESHOLD {
                warnings.push(format!(
                    "elapsed time increase: +{:.1}% (head={:.3}s, base={:.3}s)",
                    delta, head_seconds, base_seconds
                ));
            }
        }
    }

    let result = json!({
        "head": head,
        "base": base,
        "warns": warnings
    });

    (warnings, result)
}

fn main() -> Result<()> {
    let args = Args::parse();

    // Read JSON files
    let head_content = fs::read_to_string(&args.head_json)
        .with_context(|| format!("Failed to read head file: {}", args.head_json))?;
    let base_content = fs::read_to_string(&args.base_json)
        .with_context(|| format!("Failed to read base file: {}", args.base_json))?;

    let head: Value =
        serde_json::from_str(&head_content).with_context(|| "Failed to parse head JSON")?;
    let base: Value =
        serde_json::from_str(&base_content).with_context(|| "Failed to parse base JSON")?;

    let (warnings, result) = compare_metrics(&head, &base);

    // Output JSON result
    println!("{}", serde_json::to_string(&result)?);

    // Output warnings if any
    if !warnings.is_empty() {
        println!("WARN: performance regressions detected:");
        for warning in &warnings {
            println!("- {}", warning);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_no_regression() {
        let head = json!({
            "samples_per_sec": 100.0,
            "hwm_mb_last": 500,
            "seconds": 10.0
        });
        let base = json!({
            "samples_per_sec": 95.0,
            "hwm_mb_last": 490,
            "seconds": 10.5
        });

        let (warnings, _) = compare_metrics(&head, &base);
        assert!(warnings.is_empty());
    }

    #[test]
    fn test_samples_per_sec_regression() {
        let head = json!({
            "samples_per_sec": 80.0
        });
        let base = json!({
            "samples_per_sec": 100.0
        });

        let (warnings, _) = compare_metrics(&head, &base);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("samples_per_sec regression"));
        assert!(warnings[0].contains("-20.0%"));
    }

    #[test]
    fn test_hwm_increase() {
        let head = json!({
            "hwm_mb_last": 625
        });
        let base = json!({
            "hwm_mb_last": 500
        });

        let (warnings, _) = compare_metrics(&head, &base);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("peak RSS increase"));
        assert!(warnings[0].contains("+25.0%"));
    }

    #[test]
    fn test_time_increase() {
        let head = json!({
            "seconds": 13.0
        });
        let base = json!({
            "seconds": 10.0
        });

        let (warnings, _) = compare_metrics(&head, &base);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("elapsed time increase"));
        assert!(warnings[0].contains("+30.0%"));
    }

    #[test]
    fn test_division_by_zero() {
        let head = json!({
            "samples_per_sec": 100.0
        });
        let base = json!({
            "samples_per_sec": 0.0
        });

        let (warnings, _) = compare_metrics(&head, &base);
        assert!(warnings.is_empty()); // Should not crash, just skip comparison
    }
}
