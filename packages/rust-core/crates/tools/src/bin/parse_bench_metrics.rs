use anyhow::Result;
use clap::Parser;
use regex::Regex;
use serde_json::json;
use std::fs;
use std::io::{self, Read};

#[derive(Parser, Debug)]
#[command(author, version, about = "Parse benchmark metrics from log file")]
struct Args {
    /// Path to the benchmark log file (use "-" for stdin)
    log_file: String,
}

fn parse_log_content(content: &str) -> serde_json::Value {
    let mut result = json!({});

    // Pattern: "Processed {samples} samples in {seconds}s"
    let processed_re = Regex::new(r"Processed\s+(\d+)\s+samples\s+in\s+([0-9.]+)s").unwrap();
    if let Some(caps) = processed_re.captures(content) {
        let samples = caps[1].parse::<u64>().unwrap_or(0);
        let seconds = caps[2].parse::<f64>().unwrap_or(0.0);

        result["samples"] = json!(samples);
        result["seconds"] = json!(seconds);

        if seconds > 0.0 {
            let samples_per_sec = (samples as f64) / seconds;
            result["samples_per_sec"] = json!((samples_per_sec * 100.0).round() / 100.0);
        }
    }

    // Pattern: "Cache file size: {size} MB"
    let cache_size_re = Regex::new(r"Cache file size:\s+(\d+) MB").unwrap();
    if let Some(caps) = cache_size_re.captures(content) {
        result["cache_mb"] = json!(caps[1].parse::<u64>().unwrap_or(0));
    }

    // Pattern: "RSS={value}MB"
    let rss_re = Regex::new(r"RSS=(\d+)MB").unwrap();
    let rss_values: Vec<u64> = rss_re
        .captures_iter(content)
        .filter_map(|caps| caps[1].parse::<u64>().ok())
        .collect();
    if let Some(&last_rss) = rss_values.last() {
        result["rss_mb_last"] = json!(last_rss);
    }

    // Pattern: "HWM={value}MB"
    let hwm_re = Regex::new(r"HWM=(\d+)MB").unwrap();
    let hwm_values: Vec<u64> = hwm_re
        .captures_iter(content)
        .filter_map(|caps| caps[1].parse::<u64>().ok())
        .collect();
    if let Some(&last_hwm) = hwm_values.last() {
        result["hwm_mb_last"] = json!(last_hwm);
    }

    result
}

fn main() -> Result<()> {
    let args = Args::parse();

    let content = if args.log_file == "-" {
        let mut buffer = String::new();
        io::stdin().read_to_string(&mut buffer)?;
        buffer
    } else {
        fs::read_to_string(&args.log_file)?
    };

    let result = parse_log_content(&content);
    println!("{}", serde_json::to_string(&result)?);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_complete_log() {
        let log_content = r#"
Starting processing...
Processed 2000 samples in 15.25s
Cache file size: 128 MB
RSS=256MB
RSS=512MB
HWM=300MB
HWM=450MB
Done.
"#;

        let result = parse_log_content(log_content);

        assert_eq!(result["samples"], 2000);
        assert_eq!(result["seconds"], 15.25);
        assert_eq!(result["samples_per_sec"], 131.15);
        assert_eq!(result["cache_mb"], 128);
        assert_eq!(result["rss_mb_last"], 512);
        assert_eq!(result["hwm_mb_last"], 450);
    }

    #[test]
    fn test_parse_partial_log() {
        let log_content = r#"
Processed 1000 samples in 10.0s
RSS=256MB
"#;

        let result = parse_log_content(log_content);

        assert_eq!(result["samples"], 1000);
        assert_eq!(result["seconds"], 10.0);
        assert_eq!(result["samples_per_sec"], 100.0);
        assert_eq!(result["rss_mb_last"], 256);
        assert!(result.get("cache_mb").is_none());
        assert!(result.get("hwm_mb_last").is_none());
    }

    #[test]
    fn test_parse_empty_log() {
        let log_content = "";
        let result = parse_log_content(log_content);

        assert!(result.get("samples").is_none());
        assert!(result.get("seconds").is_none());
    }
}
