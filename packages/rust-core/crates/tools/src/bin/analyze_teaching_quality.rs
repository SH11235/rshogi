use std::fs;
use std::fs::File;
use std::io::{BufRead, BufReader};

use serde::Deserialize;
use serde_json::json;

// Phase 1-1: 型定義とストリーミング基盤

#[derive(Debug, Deserialize, Default)]
struct LineRec {
    #[serde(default)]
    multipv: Option<u8>,
    #[serde(default)]
    r#move: Option<String>,
    #[serde(default)]
    score_cp: Option<i32>,
    #[serde(default)]
    score_internal: Option<i32>,
    #[serde(default)]
    bound: Option<String>,
    #[serde(default)]
    depth: Option<u8>,
    #[serde(default)]
    seldepth: Option<u8>,
    #[serde(default)]
    exact_exhausted: Option<bool>,
    #[serde(default)]
    exhaust_reason: Option<String>,
    #[serde(default)]
    mate_distance: Option<i32>,
    #[serde(default)]
    pv: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Default)]
struct Record {
    #[serde(default)]
    sfen: Option<String>,
    #[serde(default)]
    lines: Vec<LineRec>,
    #[serde(default)]
    best2_gap_cp: Option<i32>,
    #[serde(default)]
    bound1: Option<String>,
    #[serde(default)]
    bound2: Option<String>,
    #[serde(default)]
    eval: Option<i32>,
    #[serde(default)]
    depth: Option<u8>,
    #[serde(default)]
    seldepth: Option<u8>,
    #[serde(default)]
    nodes: Option<u64>,
    #[serde(default)]
    nodes_q: Option<u64>,
    #[serde(default)]
    time_ms: Option<u64>,
    #[serde(default)]
    aspiration_retries: Option<i64>,
    #[serde(default)]
    pv_changed: Option<bool>,
    #[serde(default)]
    root_fail_high_count: Option<u64>,
    #[serde(default)]
    used_null: Option<bool>,
    #[serde(default)]
    lmr_applied: Option<u64>,
    #[serde(default)]
    tt_hit_rate: Option<f64>,
    // optional flags possibly present in some datasets
    #[serde(default)]
    mate_boundary: Option<bool>,
    #[serde(default)]
    no_legal_move: Option<bool>,
    #[serde(default)]
    fallback_used: Option<bool>,
}

#[derive(Default)]
struct Agg {
    total: usize,
    // exact metrics
    top1_exact: usize,
    both_exact: usize,
    both_exact_denom: usize,
    // gaps
    gaps_all: Vec<i64>,
    gaps_no_mate: Vec<i64>,
    lines_ge2: usize,
    // time
    times_ms: Vec<i64>,
    nodes: Vec<i64>,
    // fallback
    fallback_record: usize,
    fallback_top1: usize,
    fallback_top2: usize,
    // invariants (Phase 1-4 minimal)
    inv_mpv_lt_expected: usize,
    inv_gap_with_non_exact: usize,
}

impl Agg {
    fn ingest(&mut self, rec: &Record, expected_mpv: usize) {
        self.total += 1;

        // bound1/bound2 を優先。無ければ lines[0..1] から推定
        let b1 = rec
            .bound1
            .as_deref()
            .or_else(|| rec.lines.get(0).and_then(|l| l.bound.as_deref()))
            .unwrap_or("");
        let b2 = rec
            .bound2
            .as_deref()
            .or_else(|| rec.lines.get(1).and_then(|l| l.bound.as_deref()))
            .unwrap_or("");

        if b1 == "Exact" {
            self.top1_exact += 1;
        }
        if b1 == "Exact" && b2 == "Exact" {
            self.both_exact += 1;
        }
        if rec.lines.len() >= 2 {
            self.both_exact_denom += 1;
            self.lines_ge2 += 1;
        }

        if let Some(g) = rec.best2_gap_cp {
            self.gaps_all.push(g as i64);
        }
        if let Some(t) = rec.time_ms {
            self.times_ms.push(t as i64);
        }

        // gap2_no_mate: lines>=2, both Exact & non-mate, need score_cp on both lines
        if rec.lines.len() >= 2 {
            let l0 = &rec.lines[0];
            let l1 = &rec.lines[1];
            let bound_ok =
                l0.bound.as_deref() == Some("Exact") && l1.bound.as_deref() == Some("Exact");
            let mate_free = l0.mate_distance.is_none() && l1.mate_distance.is_none();
            if bound_ok && mate_free {
                if let (Some(s0), Some(s1)) = (l0.score_cp, l1.score_cp) {
                    self.gaps_no_mate.push((s0 - s1).abs() as i64);
                }
            }
        }

        // nodes
        if let Some(n) = rec.nodes {
            self.nodes.push(n as i64);
        }

        // fallback detection (record-level + line-level top1/top2)
        let mut rec_has_fallback = false;
        for (idx, l) in rec.lines.iter().enumerate() {
            if let Some(reason) = l.exhaust_reason.as_deref() {
                let r = reason.to_ascii_lowercase();
                if r == "fallback" || r == "post_fallback" {
                    rec_has_fallback = true;
                    if idx == 0 {
                        self.fallback_top1 += 1;
                    } else if idx == 1 {
                        self.fallback_top2 += 1;
                    }
                }
            }
        }
        if rec_has_fallback {
            self.fallback_record += 1;
        }

        // Invariants (minimal set)
        if expected_mpv >= 2 && rec.lines.len() < expected_mpv {
            self.inv_mpv_lt_expected += 1;
        }
        if rec.best2_gap_cp.is_some() && !(b1 == "Exact" && b2 == "Exact") {
            self.inv_gap_with_non_exact += 1;
        }
    }
}

fn wilson_ci(k: usize, n: usize, z: f64) -> (f64, f64) {
    if n == 0 {
        return (0.0, 0.0);
    }
    let p = k as f64 / n as f64;
    let z2 = z * z;
    let denom = 1.0 + z2 / (n as f64);
    let center = p + z2 / (2.0 * n as f64);
    let spread = (p * (1.0 - p) / (n as f64) + z2 / (4.0 * (n * n) as f64)).sqrt();
    let low = (center - z * spread) / denom;
    let high = (center + z * spread) / denom;
    (low.clamp(0.0, 1.0), high.clamp(0.0, 1.0))
}

fn mean_i64(v: &[i64]) -> f64 {
    if v.is_empty() {
        0.0
    } else {
        v.iter().sum::<i64>() as f64 / v.len() as f64
    }
}

fn quantile_sorted(v: &[i64], q: f64) -> i64 {
    if v.is_empty() {
        return 0;
    }
    let n = v.len();
    let idx = ((n - 1) as f64 * q).round() as usize; // nearest-rank-ish
    v[idx]
}

#[derive(Default, Clone, Copy)]
struct StatsI64 {
    count: usize,
    min: i64,
    max: i64,
    mean: f64,
    p50: i64,
    p90: i64,
    p95: i64,
    p99: i64,
}

fn compute_stats(mut v: Vec<i64>) -> Option<StatsI64> {
    if v.is_empty() {
        return None;
    }
    v.sort_unstable();
    let mean = mean_i64(&v);
    let count = v.len();
    let min = *v.first().unwrap();
    let max = *v.last().unwrap();
    let p50 = quantile_sorted(&v, 0.5);
    let p90 = quantile_sorted(&v, 0.9);
    let p95 = quantile_sorted(&v, 0.95);
    let p99 = quantile_sorted(&v, 0.99);
    Some(StatsI64 {
        count,
        min,
        max,
        mean,
        p50,
        p90,
        p95,
        p99,
    })
}

#[derive(Debug, Deserialize, Default)]
struct GateConfig {
    #[serde(default)]
    exact_top1_min: Option<f64>,
    #[serde(default)]
    exact_both_min: Option<f64>,
    #[serde(default)]
    gap_no_mate_median_min: Option<i64>,
    #[serde(default)]
    gap_no_mate_p05_min: Option<i64>,
    #[serde(default)]
    fallback_used_max: Option<usize>,
    #[serde(default)]
    ambiguous_rate_30_max: Option<f64>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!(
            "Usage: {} <input.jsonl> [--report <name> ...] [--summary] [--json] [--gate <file|inline-json>] [--gate-mode warn|fail] [--expected-multipv <N>]",
            args[0]
        );
        std::process::exit(1);
    }
    let input = &args[1];
    let mut reports: Vec<String> = Vec::new();
    let mut expected_mpv: usize = 2;
    let mut want_summary = false;
    let mut want_json = false;
    let mut gate: Option<GateConfig> = None;
    let mut gate_mode_fail = false; // false=warn, true=fail
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--report" => {
                if let Some(r) = args.get(i + 1) {
                    reports.push(r.clone());
                    i += 2;
                } else {
                    break;
                }
            }
            "--expected-multipv" | "--expected-mpv" => {
                if let Some(v) = args.get(i + 1).and_then(|s| s.parse::<usize>().ok()) {
                    expected_mpv = v.max(1);
                    i += 2;
                } else {
                    eprintln!("Error: --expected-multipv requires an integer value");
                    std::process::exit(1);
                }
            }
            "--summary" => {
                want_summary = true;
                i += 1;
            }
            "--json" => {
                want_json = true;
                i += 1;
            }
            "--gate" => {
                if let Some(spec) = args.get(i + 1) {
                    // Try file, else inline JSON
                    let cfg: GateConfig = if std::path::Path::new(spec).exists() {
                        let s = fs::read_to_string(spec)?;
                        serde_json::from_str(&s).map_err(|e| {
                            eprintln!("Failed to parse gate file: {}", e);
                            e
                        })?
                    } else {
                        serde_json::from_str(spec).map_err(|e| {
                            eprintln!("Failed to parse inline gate JSON: {}", e);
                            e
                        })?
                    };
                    gate = Some(cfg);
                    i += 2;
                } else {
                    eprintln!("Error: --gate requires a file path or inline JSON");
                    std::process::exit(1);
                }
            }
            "--gate-mode" => {
                if let Some(mode) = args.get(i + 1) {
                    match mode.as_str() {
                        "fail" => gate_mode_fail = true,
                        "warn" => gate_mode_fail = false,
                        other => {
                            eprintln!("Error: --gate-mode must be warn|fail (got {})", other);
                            std::process::exit(1);
                        }
                    }
                    i += 2;
                } else {
                    eprintln!("Error: --gate-mode requires a value (warn|fail)");
                    std::process::exit(1);
                }
            }
            other => {
                eprintln!("Unknown option: {}", other);
                std::process::exit(1);
            }
        }
    }

    let f = File::open(input)?;
    let reader = BufReader::new(f);

    let mut agg = Agg::default();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        if line.trim().is_empty() {
            continue;
        }
        let rec: Record = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        agg.ingest(&rec, expected_mpv);
    }

    for r in reports {
        match r.as_str() {
            "exact-rate" => {
                // exact_top1
                let (n1, d1) = (agg.top1_exact, agg.total);
                let (l1, h1) = wilson_ci(n1, d1, 1.96);
                let r1 = if d1 > 0 { n1 as f64 / d1 as f64 } else { 0.0 };
                println!("exact_top1: {:.3} ({} / {}), ci95=[{:.3},{:.3}]", r1, n1, d1, l1, h1);

                // exact_both
                let (n2, d2) = (agg.both_exact, agg.both_exact_denom);
                let (l2, h2) = wilson_ci(n2, d2, 1.96);
                let r2 = if d2 > 0 { n2 as f64 / d2 as f64 } else { 0.0 };
                println!("exact_both: {:.3} ({} / {}), ci95=[{:.3},{:.3}]", r2, n2, d2, l2, h2);
            }
            "gap-distribution" => {
                if agg.gaps_all.is_empty() {
                    println!("gap_distribution: no-data");
                    continue;
                }
                let avg = agg.gaps_all.iter().sum::<i64>() as f64 / agg.gaps_all.len() as f64;
                let max = *agg.gaps_all.iter().max().unwrap();
                let min = *agg.gaps_all.iter().min().unwrap();
                println!(
                    "gap_distribution: count={} min={} max={} avg={:.1}",
                    agg.gaps_all.len(),
                    min,
                    max,
                    avg
                );
            }
            "gap2" => {
                // gap2_no_mate (robust) + gap2_all summary
                if agg.lines_ge2 == 0 {
                    println!("gap2: lines_ge2=0");
                }
                if agg.gaps_no_mate.is_empty() {
                    println!(
                        "gap2_no_mate: no-data (coverage {}/{})",
                        agg.gaps_no_mate.len(),
                        agg.lines_ge2
                    );
                } else {
                    let mut v = agg.gaps_no_mate.clone();
                    v.sort_unstable();
                    let mean = mean_i64(&v);
                    let min = *v.first().unwrap();
                    let max = *v.last().unwrap();
                    let p50 = quantile_sorted(&v, 0.5);
                    let p05 = quantile_sorted(&v, 0.05);
                    let p95 = quantile_sorted(&v, 0.95);
                    println!(
                        "gap2_no_mate: count={} min={} max={} mean={:.1} median={} p05={} p95={} coverage={}/{}",
                        v.len(), min, max, mean, p50, p05, p95, v.len(), agg.lines_ge2
                    );
                }

                if agg.gaps_all.is_empty() {
                    println!("gap2_all: no-data");
                } else {
                    let mut v = agg.gaps_all.clone();
                    v.sort_unstable();
                    let mean = mean_i64(&v);
                    let min = *v.first().unwrap();
                    let max = *v.last().unwrap();
                    let p50 = quantile_sorted(&v, 0.5);
                    println!(
                        "gap2_all: count={} min={} max={} mean={:.1} median={}",
                        v.len(),
                        min,
                        max,
                        mean,
                        p50
                    );
                }
            }
            "time-distribution" => {
                if agg.times_ms.is_empty() {
                    println!("time_distribution: no-data");
                } else {
                    let mut v = agg.times_ms.clone();
                    v.sort_unstable();
                    let mean = mean_i64(&v);
                    let min = *v.first().unwrap();
                    let max = *v.last().unwrap();
                    let p50 = quantile_sorted(&v, 0.5);
                    let p90 = quantile_sorted(&v, 0.9);
                    let p99 = quantile_sorted(&v, 0.99);
                    println!(
                        "time_distribution: count={} min={}ms max={}ms mean={:.1}ms median={}ms p90={}ms p99={}ms",
                        v.len(), min, max, mean, p50, p90, p99
                    );
                }
            }
            "nodes-distribution" => {
                if agg.nodes.is_empty() {
                    println!("nodes_distribution: no-data");
                } else {
                    let mut v = agg.nodes.clone();
                    v.sort_unstable();
                    let mean = mean_i64(&v);
                    let min = *v.first().unwrap();
                    let max = *v.last().unwrap();
                    let p50 = quantile_sorted(&v, 0.5);
                    let p90 = quantile_sorted(&v, 0.9);
                    let p99 = quantile_sorted(&v, 0.99);
                    println!(
                        "nodes_distribution: count={} min={} max={} mean={:.1} median={} p90={} p99={}",
                        v.len(), min, max, mean, p50, p90, p99
                    );
                }
            }
            "fallback" => {
                let n = agg.total;
                let r = if n > 0 {
                    agg.fallback_record as f64 / n as f64
                } else {
                    0.0
                };
                println!(
                    "fallback: records={} rate={:.3} top1={} top2={}",
                    agg.fallback_record, r, agg.fallback_top1, agg.fallback_top2
                );
            }
            "invariants" => {
                let total = agg.total.max(1);
                let r1 = agg.inv_mpv_lt_expected as f64 / total as f64;
                let r2 = agg.inv_gap_with_non_exact as f64 / total as f64;
                println!(
                    "invariants: mpv_lt_expected={} ({:.3}), gap_with_non_exact={} ({:.3}), total={} expected_mpv={}",
                    agg.inv_mpv_lt_expected, r1, agg.inv_gap_with_non_exact, r2, agg.total, expected_mpv
                );
            }
            other => println!("Unknown report: {}", other),
        }
    }

    // Summary / JSON / Gate
    if want_summary || want_json || gate.is_some() {
        // exact metrics
        let (n1, d1) = (agg.top1_exact, agg.total);
        let (l1, h1) = wilson_ci(n1, d1, 1.96);
        let r1 = if d1 > 0 { n1 as f64 / d1 as f64 } else { 0.0 };

        let (n2, d2) = (agg.both_exact, agg.both_exact_denom);
        let (l2, h2) = wilson_ci(n2, d2, 1.96);
        let r2 = if d2 > 0 { n2 as f64 / d2 as f64 } else { 0.0 };

        // gaps
        let mut v_nm = agg.gaps_no_mate.clone();
        v_nm.sort_unstable();
        let gaps_nm_stats = compute_stats(v_nm.clone());
        let gaps_nm_p05 = if v_nm.is_empty() {
            None
        } else {
            Some(quantile_sorted(&v_nm, 0.05))
        };
        let gaps_all_stats = compute_stats(agg.gaps_all.clone());
        let coverage = if agg.lines_ge2 > 0 {
            agg.gaps_no_mate.len() as f64 / agg.lines_ge2 as f64
        } else {
            0.0
        };
        // ambiguous (Phase 1-5: 30のみ集計し、gate対応)
        let amb30 = if agg.lines_ge2 > 0 {
            agg.gaps_no_mate.iter().filter(|&&g| g <= 30).count() as f64 / agg.lines_ge2 as f64
        } else {
            0.0
        };

        // time/nodes
        let t_stats = compute_stats(agg.times_ms.clone());
        let n_stats = compute_stats(agg.nodes.clone());

        if want_summary {
            println!("summary:");
            println!("  exact_top1: {:.3} ({} / {}), ci95=[{:.3},{:.3}]", r1, n1, d1, l1, h1);
            println!("  exact_both: {:.3} ({} / {}), ci95=[{:.3},{:.3}]", r2, n2, d2, l2, h2);
            if let Some(s) = gaps_nm_stats {
                let p05 = gaps_nm_p05.unwrap_or(0);
                println!(
                    "  gap2_no_mate: p50={} p05={} (count={} coverage={:.3})",
                    s.p50, p05, s.count, coverage
                );
            } else {
                println!("  gap2_no_mate: no-data");
            }
            println!("  fallback_used: {}", agg.fallback_record);
            if let Some(ts) = t_stats {
                println!("  time_ms: p50={} p95={}", ts.p50, ts.p95);
            }
            if let Some(ns) = n_stats {
                println!("  nodes: p50={} p95={}", ns.p50, ns.p95);
            }
        }

        if want_json {
            let json_obj = json!({
                "exact_top1": r1, "exact_top1_n": n1, "exact_top1_denom": d1,
                "exact_top1_ci_low": l1, "exact_top1_ci_high": h1,
                "exact_both": r2, "exact_both_n": n2, "exact_both_denom": d2,
                "exact_both_ci_low": l2, "exact_both_ci_high": h2,
                "gap_no_mate_count": gaps_nm_stats.map(|s| s.count),
                "gap_no_mate_mean": gaps_nm_stats.map(|s| s.mean),
                "gap_no_mate_median": gaps_nm_stats.map(|s| s.p50),
                "gap_no_mate_p05": gaps_nm_p05,
                "gap_no_mate_p95": gaps_nm_stats.map(|s| s.p95),
                "gap_no_mate_coverage": coverage,
                "gap_all_median": gaps_all_stats.map(|s| s.p50),
                "fallback_used_count": agg.fallback_record,
                "fallback_used_rate": if agg.total>0 { (agg.fallback_record as f64)/(agg.total as f64) } else { 0.0 },
                "time_ms_min": t_stats.map(|s| s.min),
                "time_ms_median": t_stats.map(|s| s.p50),
                "time_ms_p90": t_stats.map(|s| s.p90),
                "time_ms_p95": t_stats.map(|s| s.p95),
                "nodes_min": n_stats.map(|s| s.min),
                "nodes_median": n_stats.map(|s| s.p50),
                "nodes_p90": n_stats.map(|s| s.p90),
                "nodes_p95": n_stats.map(|s| s.p95),
                "ambiguous_rate_30": amb30,
            });
            println!("{}", json_obj);
        }

        if let Some(g) = gate {
            let mut failed = false;
            // Evaluate each threshold if present
            if let Some(th) = g.exact_top1_min {
                let ok = r1 >= th;
                println!(
                    "GATE exact_top1_min {:.3} vs {:.3}: {}",
                    r1,
                    th,
                    if ok { "PASS" } else { "FAIL" }
                );
                failed |= !ok;
            }
            if let Some(th) = g.exact_both_min {
                let ok = r2 >= th;
                println!(
                    "GATE exact_both_min {:.3} vs {:.3}: {}",
                    r2,
                    th,
                    if ok { "PASS" } else { "FAIL" }
                );
                failed |= !ok;
            }
            if let Some(th) = g.gap_no_mate_median_min {
                let med = gaps_nm_stats.map(|s| s.p50).unwrap_or(0);
                let ok = med >= th;
                println!(
                    "GATE gap_no_mate_median_min {} vs {}: {}",
                    med,
                    th,
                    if ok { "PASS" } else { "FAIL" }
                );
                failed |= !ok;
            }
            if let Some(th) = g.gap_no_mate_p05_min {
                let p05 = gaps_nm_p05.unwrap_or(0);
                let ok = p05 >= th;
                println!(
                    "GATE gap_no_mate_p05_min {} vs {}: {}",
                    p05,
                    th,
                    if ok { "PASS" } else { "FAIL" }
                );
                failed |= !ok;
            }
            if let Some(th) = g.fallback_used_max {
                let ok = agg.fallback_record <= th;
                println!(
                    "GATE fallback_used_max {} <= {}: {}",
                    agg.fallback_record,
                    th,
                    if ok { "PASS" } else { "FAIL" }
                );
                failed |= !ok;
            }
            if let Some(th) = g.ambiguous_rate_30_max {
                let ok = amb30 <= th;
                println!(
                    "GATE ambiguous_rate_30_max {:.3} <= {:.3}: {}",
                    amb30,
                    th,
                    if ok { "PASS" } else { "FAIL" }
                );
                failed |= !ok;
            }

            if failed && gate_mode_fail {
                std::process::exit(1);
            }
        }
    }

    Ok(())
}
