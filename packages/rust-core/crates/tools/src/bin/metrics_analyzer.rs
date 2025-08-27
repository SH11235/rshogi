use std::io::{self, BufRead};

#[derive(Default)]
struct Aggregates {
    total: usize,
    ponder_present: usize,
    pv_len_sum: usize,
    nps_sum: u128,
    nps_count: usize,
}

fn parse_kv(line: &str) -> std::collections::HashMap<&str, &str> {
    let mut map = std::collections::HashMap::new();
    for part in line.split('\t') {
        if let Some((k, v)) = part.split_once('=') {
            map.insert(k.trim(), v.trim());
        }
    }
    map
}

fn main() {
    let stdin = io::stdin();
    let mut agg = Aggregates::default();

    for line in stdin.lock().lines() {
        let Ok(line) = line else { continue };
        if line.contains("kind=bestmove_metrics") {
            let kv = parse_kv(&line);
            agg.total += 1;
            if kv.get("ponder_present").copied() == Some("true") {
                agg.ponder_present += 1;
            }
            if let Some(v) = kv.get("pv_len").and_then(|s| s.parse::<usize>().ok()) {
                agg.pv_len_sum += v;
            }
        } else if line.contains("kind=bestmove_sent") {
            let kv = parse_kv(&line);
            if let Some(v) = kv.get("nps").and_then(|s| s.parse::<u128>().ok()) {
                agg.nps_sum += v;
                agg.nps_count += 1;
            }
        }
    }

    if agg.total == 0 {
        println!("No bestmove_metrics lines found. Pipe engine logs into this tool.");
        return;
    }

    let ponder_rate = (agg.ponder_present as f64) / (agg.total as f64);
    let avg_pv_len = (agg.pv_len_sum as f64) / (agg.total as f64);
    let avg_nps = if agg.nps_count > 0 {
        (agg.nps_sum as f64) / (agg.nps_count as f64)
    } else {
        0.0
    };

    println!("Results:");
    println!("  Samples: {}", agg.total);
    println!("  Ponder rate: {:.2}%", ponder_rate * 100.0);
    println!("  Avg PV length: {:.2}", avg_pv_len);
    println!("  Avg NPS: {:.0}", avg_nps);
}
