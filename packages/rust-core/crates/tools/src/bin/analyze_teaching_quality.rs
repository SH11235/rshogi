use clap::Parser;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::fs;

use rand::Rng;
use rand::SeedableRng;
use rand_xoshiro::Xoshiro256StarStar;
use serde::Deserialize;
use serde_json::de::Deserializer;
use serde_json::{json, Value};
use tools::common::io::open_reader;
use tools::stats::{compute_stats_exact, quantile_sorted, OnlineP2, OnlineTDigest};

// Phase 1-1: 型定義とストリーミング基盤

#[derive(Debug, Deserialize, Default)]
#[allow(dead_code)]
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
#[allow(dead_code)]
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
    #[serde(default)]
    meta: Option<String>,
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
    inv_no_legal_but_empty: usize,
    inv_fallback_true_no_reason: usize,
    inv_mate_mixed_into_no_mate: usize,
    // bounds distribution
    b1_exact: usize,
    b1_lower: usize,
    b1_upper: usize,
    b1_other: usize,
    b2_exact: usize,
    b2_lower: usize,
    b2_upper: usize,
    b2_other: usize,
    // tt/null/lmr
    tt_rates: Vec<f64>,
    used_null_sum: usize,
    lmr_sum: u64,
    lmr_per_node: Vec<f64>,
    // seldepth deficit
    seldef: Vec<i64>,
    // non-exact reasons (hint)
    non_exact_total: usize,
    non_exact_budget: usize,
    non_exact_aspiration: usize,
    non_exact_fail_high: usize,
    non_exact_unknown: usize,
    // online approx (P2)
    times_p2: Option<OnlineP2>,
    nodes_p2: Option<OnlineP2>,
    seldef_p2: Option<OnlineP2>,
    gaps_nm_p2: Option<OnlineP2>,
    gaps_all_p2: Option<OnlineP2>,
    // online approx (TDigest)
    times_td: Option<OnlineTDigest>,
    nodes_td: Option<OnlineTDigest>,
    seldef_td: Option<OnlineTDigest>,
    gaps_nm_td: Option<OnlineTDigest>,
    gaps_all_td: Option<OnlineTDigest>,
    // ambiguous counts (online)
    amb_le20_cnt: usize,
    amb_le30_cnt: usize,
}

impl Agg {
    fn ingest(
        &mut self,
        rec: &Record,
        expected_mpv: usize,
        exclude_mate: bool,
        seldef_delta: i32,
        qbackend: QuantilesBackend,
    ) {
        self.total += 1;

        // bound1/bound2 を優先。無ければ lines[0..1] から推定
        let b1 = rec
            .bound1
            .as_deref()
            .or_else(|| rec.lines.first().and_then(|l| l.bound.as_deref()))
            .unwrap_or("");
        let b2 = rec
            .bound2
            .as_deref()
            .or_else(|| rec.lines.get(1).and_then(|l| l.bound.as_deref()))
            .unwrap_or("");
        // Case-insensitive checks for Exact and bound synonyms
        let is_exact = |s: &str| s.eq_ignore_ascii_case("exact");
        let norm = |s: &str| s.to_ascii_lowercase();

        if is_exact(b1) {
            self.top1_exact += 1;
        }
        if is_exact(b1) && is_exact(b2) {
            self.both_exact += 1;
        }
        if rec.lines.len() >= 2 {
            self.both_exact_denom += 1;
            self.lines_ge2 += 1;
        }

        // bound distribution
        match norm(b1).as_str() {
            "exact" => self.b1_exact += 1,
            "lower" | "lowerbound" => self.b1_lower += 1,
            "upper" | "upperbound" => self.b1_upper += 1,
            _ => self.b1_other += 1,
        }
        match norm(b2).as_str() {
            "exact" => self.b2_exact += 1,
            "lower" | "lowerbound" => self.b2_lower += 1,
            "upper" | "upperbound" => self.b2_upper += 1,
            _ => self.b2_other += 1,
        }

        if let Some(g) = rec.best2_gap_cp {
            let v = g as i64;
            match qbackend {
                QuantilesBackend::Exact => self.gaps_all.push(v),
                QuantilesBackend::P2 => ensure_p2(&mut self.gaps_all_p2).add(v),
                QuantilesBackend::TDigest => ensure_td(&mut self.gaps_all_td).add(v),
            }
        }
        if let Some(t) = rec.time_ms {
            let v = t as i64;
            match qbackend {
                QuantilesBackend::Exact => self.times_ms.push(v),
                QuantilesBackend::P2 => ensure_p2(&mut self.times_p2).add(v),
                QuantilesBackend::TDigest => ensure_td(&mut self.times_td).add(v),
            }
        }

        // gap2_no_mate: lines>=2, both Exact & non-mate, need score_cp on both lines
        if rec.lines.len() >= 2 {
            let l0 = &rec.lines[0];
            let l1 = &rec.lines[1];
            let bound_ok = l0
                .bound
                .as_deref()
                .map(|s| s.eq_ignore_ascii_case("Exact"))
                .unwrap_or(false)
                && l1.bound.as_deref().map(|s| s.eq_ignore_ascii_case("Exact")).unwrap_or(false);
            let mate_free = l0.mate_distance.is_none() && l1.mate_distance.is_none();
            let mut inserted_nm = false;
            if bound_ok && (mate_free || !exclude_mate) {
                if let (Some(s0), Some(s1)) = (l0.score_cp, l1.score_cp) {
                    let v = (s0 - s1).abs() as i64;
                    match qbackend {
                        QuantilesBackend::Exact => self.gaps_no_mate.push(v),
                        QuantilesBackend::P2 => ensure_p2(&mut self.gaps_nm_p2).add(v),
                        QuantilesBackend::TDigest => ensure_td(&mut self.gaps_nm_td).add(v),
                    }
                    if v <= 20 {
                        self.amb_le20_cnt += 1;
                    }
                    if v <= 30 {
                        self.amb_le30_cnt += 1;
                    }
                    inserted_nm = true;
                }
            }
            // Invariant: mate_boundary==true なのに gap_no_mate に混入
            if inserted_nm && rec.mate_boundary.unwrap_or(false) {
                self.inv_mate_mixed_into_no_mate += 1;
            }
        }

        // nodes
        if let Some(n) = rec.nodes {
            let v = n as i64;
            match qbackend {
                QuantilesBackend::Exact => self.nodes.push(v),
                QuantilesBackend::P2 => ensure_p2(&mut self.nodes_p2).add(v),
                QuantilesBackend::TDigest => ensure_td(&mut self.nodes_td).add(v),
            }
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
        // Invariant: fallback_used==true なのに exhaust_reason 由来の fallback 無し
        if rec.fallback_used.unwrap_or(false) && !rec_has_fallback {
            self.inv_fallback_true_no_reason += 1;
        }

        // Invariants (minimal set)
        if expected_mpv >= 2 && rec.lines.len() < expected_mpv {
            self.inv_mpv_lt_expected += 1;
        }
        if rec.best2_gap_cp.is_some() && !(is_exact(b1) && is_exact(b2)) {
            self.inv_gap_with_non_exact += 1;
        }
        // Invariant: no_legal_move==false なのに lines が空
        if rec.no_legal_move == Some(false) && rec.lines.is_empty() {
            self.inv_no_legal_but_empty += 1;
        }

        // tt/null/lmr
        if let Some(mut r) = rec.tt_hit_rate {
            if !r.is_finite() {
                r = 0.0;
            }
            r = r.clamp(0.0, 1.0);
            self.tt_rates.push(r);
        }
        if rec.used_null.unwrap_or(false) {
            self.used_null_sum += 1;
        }
        if let Some(lmr) = rec.lmr_applied {
            self.lmr_sum = self.lmr_sum.saturating_add(lmr);
        }
        if let (Some(lmr), Some(n)) = (rec.lmr_applied, rec.nodes) {
            if n > 0 {
                self.lmr_per_node.push((lmr as f64) / (n as f64));
            }
        }

        // seldepth deficit
        if let (Some(d), Some(sd)) = (rec.depth, rec.seldepth) {
            let deficit = (d as i32 + seldef_delta - sd as i32).max(0) as i64;
            match qbackend {
                QuantilesBackend::Exact => self.seldef.push(deficit),
                QuantilesBackend::P2 => ensure_p2(&mut self.seldef_p2).add(deficit),
                QuantilesBackend::TDigest => ensure_td(&mut self.seldef_td).add(deficit),
            }
        }

        // non-exact reason hint aggregation (only when non-exact)
        let is_non_exact = !(is_exact(b1) && is_exact(b2));
        if is_non_exact {
            self.non_exact_total += 1;
            let mut budget = false;
            let mut aspiration = false;
            let mut fail_high = false;
            for l in &rec.lines {
                if let Some(r) = l.exhaust_reason.as_deref() {
                    let rr = r.to_ascii_lowercase();
                    if rr.contains("time")
                        || rr.contains("timeout")
                        || l.exact_exhausted == Some(true)
                    {
                        budget = true;
                    }
                }
            }
            if rec.meta.as_deref().unwrap_or("").contains("timeout_") {
                budget = true;
            }
            if rec.aspiration_retries.unwrap_or(0) > 0 {
                aspiration = true;
            }
            if rec.root_fail_high_count.unwrap_or(0) > 0 {
                fail_high = true;
            }

            if budget {
                self.non_exact_budget += 1;
            }
            if aspiration {
                self.non_exact_aspiration += 1;
            }
            if fail_high {
                self.non_exact_fail_high += 1;
            }
            if !budget && !aspiration && !fail_high {
                self.non_exact_unknown += 1;
            }
        }
        // (duplicate tt/null/lmr aggregation removed; counted above)
    }
}

impl Agg {
    // Flush TDigest-backed online fields so we can read without cloning
    fn finalize_online(&mut self) {
        if let Some(ref mut o) = self.times_td {
            o.flush();
        }
        if let Some(ref mut o) = self.nodes_td {
            o.flush();
        }
        if let Some(ref mut o) = self.seldef_td {
            o.flush();
        }
        if let Some(ref mut o) = self.gaps_nm_td {
            o.flush();
        }
        if let Some(ref mut o) = self.gaps_all_td {
            o.flush();
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

fn csv_escape(s: &str) -> String {
    let needs_quotes = s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r');
    if needs_quotes {
        let escaped = s.replace('"', "\"\"");
        format!("\"{}\"", escaped)
    } else {
        s.to_string()
    }
}

fn ensure_p2(slot: &mut Option<OnlineP2>) -> &mut OnlineP2 {
    if slot.is_none() {
        *slot = Some(OnlineP2::new());
    }
    slot.as_mut().unwrap()
}
fn ensure_td(slot: &mut Option<OnlineTDigest>) -> &mut OnlineTDigest {
    if slot.is_none() {
        *slot = Some(OnlineTDigest::new());
    }
    slot.as_mut().unwrap()
}

fn make_rng(seed: Option<u64>) -> Xoshiro256StarStar {
    match seed {
        Some(s) => Xoshiro256StarStar::seed_from_u64(s),
        None => {
            let mut tr = rand::rng();
            Xoshiro256StarStar::from_rng(&mut tr)
        }
    }
}
// Build aggregate from a single JSONL file with optional reservoir sampling and limit
#[allow(clippy::too_many_arguments)]
fn build_agg_for(
    path: &str,
    expected_mpv: usize,
    exclude_mate: bool,
    seldef_delta: i32,
    qbackend: QuantilesBackend,
    limit: Option<usize>,
    sample_n: Option<usize>,
    seed: Option<u64>,
) -> Agg {
    let mut a = Agg::default();
    if let Ok(reader) = open_reader(path) {
        let cap = sample_n.unwrap_or(0);
        let use_res = cap > 0;
        let mut rng = make_rng(seed);
        let stream = Deserializer::from_reader(reader).into_iter::<Record>();
        if !use_res {
            let mut ing = 0usize;
            for rec in stream {
                let Ok(rec) = rec else { continue };
                a.ingest(&rec, expected_mpv, exclude_mate, seldef_delta, qbackend);
                ing += 1;
                if let Some(m) = limit {
                    if ing >= m {
                        break;
                    }
                }
            }
        } else {
            let mut seen = 0usize;
            let mut reservoir: Vec<Record> = Vec::new();
            for rec in stream {
                let Ok(rec) = rec else { continue };
                seen += 1;
                if reservoir.len() < cap {
                    reservoir.push(rec);
                } else {
                    let j = rng.random_range(0..seen);
                    if j < cap {
                        reservoir[j] = rec;
                    }
                }
            }
            for rec in reservoir.into_iter() {
                a.ingest(&rec, expected_mpv, exclude_mate, seldef_delta, qbackend);
            }
        }
    }
    a
}

struct PairAgg {
    a1: Agg,
    a2: Agg,
}

#[allow(clippy::too_many_arguments)]
fn build_pair_agg(
    path1: &str,
    path2: &str,
    expected_mpv: usize,
    exclude_mate: bool,
    seldef_delta: i32,
    qbackend: QuantilesBackend,
    limit: Option<usize>,
    sample_n: Option<usize>,
    seed: Option<u64>,
) -> PairAgg {
    let mut a1 = build_agg_for(
        path1,
        expected_mpv,
        exclude_mate,
        seldef_delta,
        qbackend,
        limit,
        sample_n,
        seed,
    );
    let mut a2 = build_agg_for(
        path2,
        expected_mpv,
        exclude_mate,
        seldef_delta,
        qbackend,
        limit,
        sample_n,
        seed,
    );
    a1.finalize_online();
    a2.finalize_online();
    PairAgg { a1, a2 }
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
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
    #[serde(default)]
    ambiguous_rate_20_max: Option<f64>,
    // invariants (counts)
    #[serde(default)]
    mpv_lt_expected_max: Option<usize>,
    #[serde(default)]
    gap_with_non_exact_max: Option<usize>,
    #[serde(default)]
    no_legal_but_empty_max: Option<usize>,
    #[serde(default)]
    fallback_true_no_reason_max: Option<usize>,
    #[serde(default)]
    mate_mixed_into_no_mate_max: Option<usize>,
    // invariant rate gates
    #[serde(default)]
    mpv_lt_expected_rate_max: Option<f64>,
    #[serde(default)]
    gap_with_non_exact_rate_max: Option<f64>,
    #[serde(default)]
    no_legal_but_empty_rate_max: Option<f64>,
    #[serde(default)]
    fallback_true_no_reason_rate_max: Option<f64>,
    #[serde(default)]
    mate_mixed_into_no_mate_rate_max: Option<f64>,
    // delta gates (only when exactly 2 inputs & no dedup)
    #[serde(default)]
    delta_exact_both_min: Option<f64>,
    #[serde(default)]
    delta_gap_no_mate_median_min: Option<i64>,
    #[serde(default)]
    delta_time_ms_median_max: Option<i64>,
    #[serde(default)]
    delta_time_ms_p95_max: Option<i64>,
    #[serde(default)]
    delta_nodes_median_max: Option<i64>,
    #[serde(default)]
    delta_nodes_p95_max: Option<i64>,
    #[serde(default)]
    delta_tt_hit_rate_mean_min: Option<f64>,
    #[serde(default)]
    delta_null_rate_max: Option<f64>,
    #[serde(default)]
    delta_lmr_mean_max: Option<f64>,
    // seldepth deficit gates
    #[serde(default)]
    seldepth_deficit_median_max: Option<i64>,
    #[serde(default)]
    seldepth_deficit_p90_max: Option<i64>,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
enum GateMode {
    #[value(name = "warn")]
    Warn,
    #[value(name = "fail")]
    Fail,
}

#[derive(clap::ValueEnum, Clone, Debug, PartialEq, Eq)]
enum ReportKind {
    #[value(name = "exact-rate")]
    ExactRate,
    #[value(name = "gap-distribution")]
    GapDistribution,
    #[value(name = "gap2")]
    Gap2,
    #[value(name = "time-distribution")]
    TimeDistribution,
    #[value(name = "nodes-distribution")]
    NodesDistribution,
    #[value(name = "bound-distribution")]
    BoundDistribution,
    #[value(name = "tt")]
    Tt,
    #[value(name = "null-lmr")]
    NullLmr,
    #[value(name = "invariants")]
    Invariants,
    #[value(name = "fallback")]
    Fallback,
    #[value(name = "seldepth-deficit")]
    SeldepthDeficit,
    #[value(name = "non-exact-reason")]
    NonExactReason,
    #[value(name = "ambiguous")]
    Ambiguous,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
enum QuantilesBackendArg {
    #[value(name = "exact")]
    Exact,
    #[value(name = "p2", alias = "approx")]
    P2,
    #[value(name = "tdigest", alias = "t-digest", alias = "td")]
    TDigest,
}

#[derive(Parser)]
#[command(
    name = "analyze_teaching_quality",
    about = "Teacher data quality analyzer",
    disable_help_subcommand = true,
    after_help = "Reports: exact-rate, gap-distribution, gap2, time-distribution, nodes-distribution, bound-distribution, tt, null-lmr, invariants, fallback, seldepth-deficit, non-exact-reason, ambiguous\nExample: --gate crates/tools/ci_gate.sample.json --gate-mode fail"
)]
struct Cli {
    /// Primary input JSONL file
    input: String,
    /// Reports to print (repeatable)
    #[arg(long = "report", value_enum)]
    report: Vec<ReportKind>,
    /// Expected MultiPV for invariants
    #[arg(long = "expected-multipv", alias = "expected-mpv", default_value_t = 2)]
    expected_mpv: usize,
    /// Print human-readable summary
    #[arg(long = "summary")]
    summary: bool,
    /// Print machine JSON summary
    #[arg(long = "json")]
    json: bool,
    /// Print machine CSV summary
    #[arg(long = "csv")]
    csv: bool,
    /// Include CSV header line
    #[arg(long = "csv-header")]
    csv_header: bool,
    /// Additional input files
    #[arg(long = "inputs")]
    inputs: Vec<String>,
    /// Deduplicate by SFEN
    #[arg(long = "dedup-by-sfen")]
    dedup_by_sfen: bool,
    /// Include mate positions in gap stats
    #[arg(long = "with-mate")]
    with_mate: bool,
    /// Seldepth deficit delta
    #[arg(long = "seldepth-deficit-delta", default_value_t = 6)]
    seldepth_deficit_delta: i32,
    /// Manifest path to embed
    #[arg(long = "manifest")]
    manifest: Option<String>,
    /// Disable manifest auto-detection
    #[arg(long = "no-manifest-autoload")]
    no_manifest_autoload: bool,
    /// Limit processed records
    #[arg(long = "limit")]
    limit: Option<usize>,
    /// Reservoir sample size (0 disables)
    #[arg(long = "sample")]
    sample: Option<usize>,
    /// Quantiles backend name (exact|p2|tdigest); --approx-quantiles forces p2
    #[arg(long = "quantiles-backend", value_enum)]
    quantiles_backend: Option<QuantilesBackendArg>,
    /// Shorthand for --quantiles-backend p2
    #[arg(long = "approx-quantiles")]
    approx_quantiles: bool,
    /// Gate config (file path or inline JSON)
    #[arg(long = "gate")]
    gate: Option<String>,
    /// Gate mode (warn|fail)
    #[arg(long = "gate-mode", value_enum, default_value_t = GateMode::Warn)]
    gate_mode: GateMode,
    /// Seed for reproducible sampling (optional)
    #[arg(long = "seed")]
    seed: Option<u64>,
}

fn parse_quantiles_backend(
    s: &Option<QuantilesBackendArg>,
    approx: bool,
) -> Option<QuantilesBackend> {
    if approx {
        return Some(QuantilesBackend::P2);
    }
    s.as_ref().map(|v| match v {
        QuantilesBackendArg::Exact => QuantilesBackend::Exact,
        QuantilesBackendArg::P2 => QuantilesBackend::P2,
        QuantilesBackendArg::TDigest => QuantilesBackend::TDigest,
    })
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let input = &cli.input;
    let reports: Vec<ReportKind> = cli.report.clone();
    let expected_mpv: usize = cli.expected_mpv.max(1);
    let want_summary = cli.summary;
    let want_json = cli.json;
    let want_csv = cli.csv;
    let want_csv_header = cli.csv_header;
    let gate_mode_fail = matches!(cli.gate_mode, GateMode::Fail);
    let mut gate: Option<GateConfig> = None;
    let more_inputs: Vec<String> = cli.inputs.clone();
    let dedup_by_sfen = cli.dedup_by_sfen;
    let exclude_mate = !cli.with_mate;
    let seldef_delta: i32 = cli.seldepth_deficit_delta;
    let manifest_path: Option<String> = cli.manifest.clone();
    let no_manifest_autoload = cli.no_manifest_autoload;
    let limit: Option<usize> = cli.limit;
    let sample_n: Option<usize> = cli.sample;
    let quant_backend: Option<QuantilesBackend> =
        parse_quantiles_backend(&cli.quantiles_backend, cli.approx_quantiles);
    if let Some(spec) = cli.gate.as_ref() {
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
    }

    // Build input list (primary input + optional more_inputs)
    let mut inputs: Vec<String> = Vec::new();
    inputs.push(input.clone());
    inputs.extend(more_inputs);

    let mut agg = Agg::default();
    let qbackend = quant_backend.unwrap_or(QuantilesBackend::Exact);

    // Load manifest (explicit or auto-detect near primary input)
    let mut manifest_json: Option<Value> = None;
    if let Some(mp) = manifest_path.as_ref() {
        if let Ok(s) = fs::read_to_string(mp) {
            if let Ok(v) = serde_json::from_str::<Value>(&s) {
                manifest_json = Some(v);
            }
        }
    } else if !no_manifest_autoload {
        if let Some(primary) = inputs.first() {
            if let Some(dir) = std::path::Path::new(primary).parent() {
                let candidate = dir.join("manifest.json");
                if candidate.exists() {
                    if let Ok(s) = fs::read_to_string(&candidate) {
                        if let Ok(v) = serde_json::from_str::<Value>(&s) {
                            manifest_json = Some(v);
                        }
                    }
                }
            }
        }
    }

    if !dedup_by_sfen {
        let mut seen: usize = 0;
        let mut reservoir: Vec<Record> = Vec::new();
        let mut rng = make_rng(cli.seed);
        let sample_cap = sample_n.unwrap_or(0);
        let use_reservoir = sample_cap > 0;
        let mut ingested: usize = 0;
        'outer: for path in &inputs {
            let reader = open_reader(path)?;
            let stream = Deserializer::from_reader(reader).into_iter::<Record>();
            for rec in stream {
                let rec = match rec {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if use_reservoir {
                    seen += 1;
                    if reservoir.len() < sample_cap {
                        reservoir.push(rec);
                    } else if sample_cap > 0 {
                        let j = rng.random_range(0..seen);
                        if j < sample_cap {
                            reservoir[j] = rec;
                        }
                    }
                } else {
                    agg.ingest(&rec, expected_mpv, exclude_mate, seldef_delta, qbackend);
                    ingested += 1;
                    if let Some(maxn) = limit {
                        if ingested >= maxn {
                            break 'outer;
                        }
                    }
                }
            }
        }
        if use_reservoir {
            for rec in reservoir.into_iter() {
                agg.ingest(&rec, expected_mpv, exclude_mate, seldef_delta, qbackend);
            }
        }
    } else {
        // Deduplicate by SFEN with stable tiebreakers
        #[derive(Clone)]
        struct Key {
            depth: u32,
            seldepth: u32,
            exact_score: u8, // 2=both exact, 1=top1 exact, 0=else
            nodes: u64,
            time_ms: u64,
            file_idx: usize,
            line_idx: usize,
        }
        fn make_key(rec: &Record, file_idx: usize, line_idx: usize) -> Key {
            let depth = rec.depth.unwrap_or(0) as u32;
            let seldepth = rec.seldepth.unwrap_or(0) as u32;
            let b1 = rec
                .bound1
                .as_deref()
                .or_else(|| rec.lines.first().and_then(|l| l.bound.as_deref()));
            let b2 = rec
                .bound2
                .as_deref()
                .or_else(|| rec.lines.get(1).and_then(|l| l.bound.as_deref()));
            let is_exact = |s: &str| s.eq_ignore_ascii_case("exact");
            let exact_score =
                match (b1.map(is_exact).unwrap_or(false), b2.map(is_exact).unwrap_or(false)) {
                    (true, true) => 2,
                    (true, false) => 1,
                    _ => 0,
                };
            let nodes = rec.nodes.unwrap_or(0);
            let time_ms = rec.time_ms.unwrap_or(0);
            Key {
                depth,
                seldepth,
                exact_score,
                nodes,
                time_ms,
                file_idx,
                line_idx,
            }
        }
        fn better(a: &Key, b: &Key) -> Ordering {
            // deeper first
            a.depth
                .cmp(&b.depth)
                .then(a.seldepth.cmp(&b.seldepth))
                .then(a.exact_score.cmp(&b.exact_score))
                .then(a.nodes.cmp(&b.nodes))
                .then(a.time_ms.cmp(&b.time_ms))
                .then(a.file_idx.cmp(&b.file_idx))
                .then(a.line_idx.cmp(&b.line_idx))
        }

        let mut best: HashMap<String, (Record, Key)> = HashMap::new();
        for (file_idx, path) in inputs.iter().enumerate() {
            let reader = open_reader(path)?;
            let stream = Deserializer::from_reader(reader).into_iter::<Record>();
            let mut line_idx: usize = 0;
            for rec in stream {
                line_idx += 1;
                let rec = match rec {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let sfen = match rec.sfen.as_ref() {
                    Some(s) => s.clone(),
                    None => continue,
                };
                let key = make_key(&rec, file_idx, line_idx);
                if let Some((ref mut cur_rec, ref mut cur_key)) = best.get_mut(&sfen) {
                    if better(&key, cur_key) == Ordering::Greater {
                        *cur_rec = rec;
                        *cur_key = key;
                    }
                } else {
                    best.insert(sfen, (rec, key));
                }
            }
        }
        // Collect, sort by key (best first), then apply sample/limit
        let mut items: Vec<(Record, Key)> = best.into_values().collect();
        items.sort_by(|a, b| better(&a.1, &b.1));
        items.reverse();
        if let Some(k) = sample_n.filter(|&x| x > 0) {
            // reservoir over items
            let mut res: Vec<(Record, Key)> = Vec::new();
            let mut rng = make_rng(cli.seed);
            for (idx, it) in items.into_iter().enumerate() {
                let seen = idx + 1;
                if res.len() < k {
                    res.push(it);
                } else {
                    let j = rng.random_range(0..seen);
                    if j < k {
                        res[j] = it;
                    }
                }
            }
            for (rec, _) in res.into_iter() {
                agg.ingest(&rec, expected_mpv, exclude_mate, seldef_delta, qbackend);
            }
        } else if let Some(maxn) = limit {
            for (idx, (rec, _)) in items.into_iter().enumerate() {
                if idx >= maxn {
                    break;
                }
                agg.ingest(&rec, expected_mpv, exclude_mate, seldef_delta, qbackend);
            }
        } else {
            for (rec, _) in items.into_iter() {
                agg.ingest(&rec, expected_mpv, exclude_mate, seldef_delta, qbackend);
            }
        }
    }

    // Finalize online quantiles once (avoid clone + repeated compress)
    agg.finalize_online();

    // Precompute pair aggregation once when exactly two inputs and not dedup-by-sfen
    let mut pair: Option<PairAgg> = if inputs.len() == 2 && !dedup_by_sfen {
        Some(build_pair_agg(
            &inputs[0],
            &inputs[1],
            expected_mpv,
            exclude_mate,
            seldef_delta,
            qbackend,
            limit,
            sample_n,
            cli.seed,
        ))
    } else {
        None
    };

    for r in reports {
        match r {
            ReportKind::ExactRate => {
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
            ReportKind::GapDistribution => match qbackend {
                QuantilesBackend::Exact => {
                    if agg.gaps_all.is_empty() {
                        println!("gap_distribution: no-data");
                    } else {
                        let v = &agg.gaps_all;
                        let avg = v.iter().sum::<i64>() as f64 / v.len() as f64;
                        let min = *v.iter().min().unwrap();
                        let max = *v.iter().max().unwrap();
                        println!(
                            "gap_distribution: count={} min={} max={} avg={:.1}",
                            v.len(),
                            min,
                            max,
                            avg
                        );
                    }
                }
                QuantilesBackend::P2 => {
                    if let Some(ref o) = agg.gaps_all_p2 {
                        let avg = o.sum as f64 / (o.count as f64);
                        println!(
                            "gap_distribution: count={} min={} max={} avg={:.1}",
                            o.count, o.min, o.max, avg
                        );
                    } else {
                        println!("gap_distribution: no-data");
                    }
                }
                QuantilesBackend::TDigest => {
                    if let Some(ref o) = agg.gaps_all_td {
                        let avg = o.sum as f64 / (o.count as f64);
                        println!(
                            "gap_distribution: count={} min={} max={} avg={:.1}",
                            o.count, o.min, o.max, avg
                        );
                    } else {
                        println!("gap_distribution: no-data");
                    }
                }
            },
            ReportKind::Gap2 => {
                // gap2_no_mate (robust) + gap2_all summary
                if agg.lines_ge2 == 0 {
                    println!("gap2: lines_ge2=0");
                }
                // compute using backend
                let (cnt, min, max, mean, med, p05, p95) = match qbackend {
                    QuantilesBackend::Exact => {
                        if agg.gaps_no_mate.is_empty() {
                            (0, 0, 0, 0.0, 0, 0, 0)
                        } else {
                            let v = &agg.gaps_no_mate;
                            let mean = mean_i64(v);
                            let min = *v.iter().min().unwrap();
                            let max = *v.iter().max().unwrap();
                            let mut vv = v.clone();
                            vv.sort_unstable();
                            (
                                v.len(),
                                min,
                                max,
                                mean,
                                quantile_sorted(&vv, 0.5),
                                quantile_sorted(&vv, 0.05),
                                quantile_sorted(&vv, 0.95),
                            )
                        }
                    }
                    QuantilesBackend::P2 => {
                        if let Some(ref o) = agg.gaps_nm_p2 {
                            if let Some(s) = o.stats() {
                                (
                                    o.count,
                                    o.min,
                                    o.max,
                                    o.sum as f64 / (o.count as f64),
                                    s.p50,
                                    o.q05(),
                                    s.p95,
                                )
                            } else {
                                (0, 0, 0, 0.0, 0, 0, 0)
                            }
                        } else {
                            (0, 0, 0, 0.0, 0, 0, 0)
                        }
                    }
                    QuantilesBackend::TDigest => {
                        if let Some(ref mut od) = agg.gaps_nm_td {
                            (
                                od.count,
                                od.min,
                                od.max,
                                od.sum as f64 / (od.count as f64),
                                od.q(0.5),
                                od.q(0.05),
                                od.q(0.95),
                            )
                        } else {
                            (0, 0, 0, 0.0, 0, 0, 0)
                        }
                    }
                };
                if cnt == 0 {
                    println!("gap2_no_mate: no-data (coverage {}/{})", 0, agg.lines_ge2);
                } else {
                    println!("gap2_no_mate: count={} min={} max={} mean={:.1} median={} p05={} p95={} coverage={}/{}", cnt, min, max, mean, med, p05, p95, cnt, agg.lines_ge2);
                }

                let (cnt2, min2, max2, mean2, med2) = match qbackend {
                    QuantilesBackend::Exact => {
                        if agg.gaps_all.is_empty() {
                            (0, 0, 0, 0.0, 0)
                        } else {
                            let v = &agg.gaps_all;
                            let mean = mean_i64(v);
                            let min = *v.iter().min().unwrap();
                            let max = *v.iter().max().unwrap();
                            let mut vv = v.clone();
                            vv.sort_unstable();
                            (v.len(), min, max, mean, quantile_sorted(&vv, 0.5))
                        }
                    }
                    QuantilesBackend::P2 => {
                        if let Some(ref o) = agg.gaps_all_p2 {
                            if let Some(s) = o.stats() {
                                (o.count, o.min, o.max, o.sum as f64 / (o.count as f64), s.p50)
                            } else {
                                (0, 0, 0, 0.0, 0)
                            }
                        } else {
                            (0, 0, 0, 0.0, 0)
                        }
                    }
                    QuantilesBackend::TDigest => {
                        if let Some(ref mut od) = agg.gaps_all_td {
                            (od.count, od.min, od.max, od.sum as f64 / (od.count as f64), od.q(0.5))
                        } else {
                            (0, 0, 0, 0.0, 0)
                        }
                    }
                };
                if cnt2 == 0 {
                    println!("gap2_all: no-data");
                } else {
                    println!(
                        "gap2_all: count={} min={} max={} mean={:.1} median={}",
                        cnt2, min2, max2, mean2, med2
                    );
                }
            }
            ReportKind::TimeDistribution => match qbackend {
                QuantilesBackend::Exact => {
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
                        println!("time_distribution: count={} min={}ms max={}ms mean={:.1}ms median={}ms p90={}ms p99={}ms", v.len(), min, max, mean, p50, p90, p99);
                    }
                }
                QuantilesBackend::P2 => {
                    if let Some(ref o) = agg.times_p2 {
                        if let Some(s) = o.stats() {
                            println!("time_distribution: count={} min={}ms max={}ms mean={:.1}ms median={}ms p90={}ms p99={}ms", s.count, s.min, s.max, s.mean, s.p50, s.p90, s.p99);
                        } else {
                            println!("time_distribution: no-data");
                        }
                    } else {
                        println!("time_distribution: no-data");
                    }
                }
                QuantilesBackend::TDigest => {
                    if let Some(ref mut td) = agg.times_td {
                        let count = td.count;
                        let min = td.min;
                        let max = td.max;
                        let mean = td.sum as f64 / (td.count as f64);
                        let p50 = td.q(0.5);
                        let p90 = td.q(0.9);
                        let p99 = td.q(0.99);
                        println!("time_distribution: count={} min={}ms max={}ms mean={:.1}ms median={}ms p90={}ms p99={}ms", count, min, max, mean, p50, p90, p99);
                    } else {
                        println!("time_distribution: no-data");
                    }
                }
            },
            ReportKind::BoundDistribution => {
                println!(
                    "bound1: exact={} lower={} upper={} other={} | bound2: exact={} lower={} upper={} other={}",
                    agg.b1_exact, agg.b1_lower, agg.b1_upper, agg.b1_other,
                    agg.b2_exact, agg.b2_lower, agg.b2_upper, agg.b2_other
                );
            }
            ReportKind::Tt => {
                if agg.tt_rates.is_empty() {
                    println!("tt: no-data");
                } else {
                    let mean = agg.tt_rates.iter().sum::<f64>() / agg.tt_rates.len() as f64;
                    let mut v = agg.tt_rates.clone();
                    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
                    let med = v[v.len() / 2];
                    println!("tt: mean={:.3} median={:.3} samples={}", mean, med, v.len());
                }
            }
            ReportKind::NullLmr => {
                let used_rate = if agg.total > 0 {
                    agg.used_null_sum as f64 / agg.total as f64
                } else {
                    0.0
                };
                let lmr_mean = agg.lmr_sum as f64 / (agg.total as f64).max(1.0);
                let lmr_per_node_mean = if agg.lmr_per_node.is_empty() {
                    0.0
                } else {
                    agg.lmr_per_node.iter().sum::<f64>() / agg.lmr_per_node.len() as f64
                };
                println!(
                    "null_lmr: used_null_sum={} used_null_rate={:.3} lmr_sum={} lmr_mean={:.2} lmr_per_node_mean={:.6}",
                    agg.used_null_sum, used_rate, agg.lmr_sum, lmr_mean, lmr_per_node_mean
                );
            }
            ReportKind::SeldepthDeficit => match qbackend {
                QuantilesBackend::Exact => {
                    if agg.seldef.is_empty() {
                        println!("seldepth_deficit: no-data");
                    } else {
                        let mut v = agg.seldef.clone();
                        v.sort_unstable();
                        let mean = mean_i64(&v);
                        let min = *v.first().unwrap();
                        let max = *v.last().unwrap();
                        let p50 = quantile_sorted(&v, 0.5);
                        let p90 = quantile_sorted(&v, 0.9);
                        let p99 = quantile_sorted(&v, 0.99);
                        println!("seldepth_deficit: count={} min={} max={} mean={:.1} median={} p90={} p99={}", v.len(), min, max, mean, p50, p90, p99);
                    }
                }
                QuantilesBackend::P2 => {
                    if let Some(ref o) = agg.seldef_p2 {
                        if let Some(s) = o.stats() {
                            println!("seldepth_deficit: count={} min={} max={} mean={:.1} median={} p90={} p99={}", s.count, s.min, s.max, s.mean, s.p50, s.p90, s.p99);
                        } else {
                            println!("seldepth_deficit: no-data");
                        }
                    } else {
                        println!("seldepth_deficit: no-data");
                    }
                }
                QuantilesBackend::TDigest => {
                    if let Some(ref mut td) = agg.seldef_td {
                        let count = td.count;
                        let min = td.min;
                        let max = td.max;
                        let mean = td.sum as f64 / (td.count as f64);
                        let p50 = td.q(0.5);
                        let p90 = td.q(0.9);
                        let p99 = td.q(0.99);
                        println!("seldepth_deficit: count={} min={} max={} mean={:.1} median={} p90={} p99={}", count, min, max, mean, p50, p90, p99);
                    } else {
                        println!("seldepth_deficit: no-data");
                    }
                }
            },
            ReportKind::NonExactReason => {
                let t = agg.non_exact_total as f64;
                if agg.non_exact_total == 0 {
                    println!("non_exact_reason: no-data");
                } else {
                    println!(
                        "non_exact_reason: total={} budget={:.3} aspiration={:.3} fail_high={:.3} unknown={:.3}",
                        agg.non_exact_total,
                        (agg.non_exact_budget as f64)/t,
                        (agg.non_exact_aspiration as f64)/t,
                        (agg.non_exact_fail_high as f64)/t,
                        (agg.non_exact_unknown as f64)/t
                    );
                }
            }
            ReportKind::Ambiguous => {
                let n_amb = agg.lines_ge2;
                if n_amb == 0 {
                    println!("ambiguous: no-data");
                } else {
                    let (k20, k30) = match qbackend {
                        QuantilesBackend::Exact => {
                            let k20 = agg.gaps_no_mate.iter().filter(|&&g| g <= 20).count();
                            let k30 = agg.gaps_no_mate.iter().filter(|&&g| g <= 30).count();
                            (k20, k30)
                        }
                        _ => (agg.amb_le20_cnt, agg.amb_le30_cnt),
                    };
                    let r20 = k20 as f64 / n_amb as f64;
                    let (l20, h20) = wilson_ci(k20, n_amb, 1.96);
                    let r30 = k30 as f64 / n_amb as f64;
                    let (l30, h30) = wilson_ci(k30, n_amb, 1.96);
                    println!("ambiguous: n={} rate20={:.3} ci95=[{:.3},{:.3}] rate30={:.3} ci95=[{:.3},{:.3}]", n_amb, r20, l20, h20, r30, l30, h30);
                }
            }
            ReportKind::NodesDistribution => match qbackend {
                QuantilesBackend::Exact => {
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
                        println!("nodes_distribution: count={} min={} max={} mean={:.1} median={} p90={} p99={}", v.len(), min, max, mean, p50, p90, p99);
                    }
                }
                QuantilesBackend::P2 => {
                    if let Some(ref o) = agg.nodes_p2 {
                        if let Some(s) = o.stats() {
                            println!("nodes_distribution: count={} min={} max={} mean={:.1} median={} p90={} p99={}", s.count, s.min, s.max, s.mean, s.p50, s.p90, s.p99);
                        } else {
                            println!("nodes_distribution: no-data");
                        }
                    } else {
                        println!("nodes_distribution: no-data");
                    }
                }
                QuantilesBackend::TDigest => {
                    if let Some(ref mut td) = agg.nodes_td {
                        let count = td.count;
                        let min = td.min;
                        let max = td.max;
                        let mean = td.sum as f64 / (td.count as f64);
                        let p50 = td.q(0.5);
                        let p90 = td.q(0.9);
                        let p99 = td.q(0.99);
                        println!("nodes_distribution: count={} min={} max={} mean={:.1} median={} p90={} p99={}", count, min, max, mean, p50, p90, p99);
                    } else {
                        println!("nodes_distribution: no-data");
                    }
                }
            },
            ReportKind::Fallback => {
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
            ReportKind::Invariants => {
                let total = agg.total.max(1);
                let r1 = agg.inv_mpv_lt_expected as f64 / total as f64;
                let r2 = agg.inv_gap_with_non_exact as f64 / total as f64;
                let r3 = agg.inv_no_legal_but_empty as f64 / total as f64;
                let r4 = agg.inv_fallback_true_no_reason as f64 / total as f64;
                let r5 = agg.inv_mate_mixed_into_no_mate as f64 / total as f64;
                println!(
                    "invariants: mpv_lt_expected={} ({:.3}), gap_with_non_exact={} ({:.3}), no_legal_but_empty={} ({:.3}), fallback_true_no_reason={} ({:.3}), mate_mixed_into_no_mate={} ({:.3}), total={} expected_mpv={}",
                    agg.inv_mpv_lt_expected, r1,
                    agg.inv_gap_with_non_exact, r2,
                    agg.inv_no_legal_but_empty, r3,
                    agg.inv_fallback_true_no_reason, r4,
                    agg.inv_mate_mixed_into_no_mate, r5,
                    agg.total, expected_mpv
                );
            }
        }
    }

    // Summary / JSON / Gate
    if want_summary || want_json || want_csv || gate.is_some() {
        // exact metrics
        let (n1, d1) = (agg.top1_exact, agg.total);
        let (l1, h1) = wilson_ci(n1, d1, 1.96);
        let r1 = if d1 > 0 { n1 as f64 / d1 as f64 } else { 0.0 };

        let (n2, d2) = (agg.both_exact, agg.both_exact_denom);
        let (l2, h2) = wilson_ci(n2, d2, 1.96);
        let r2 = if d2 > 0 { n2 as f64 / d2 as f64 } else { 0.0 };

        // gaps
        let gaps_nm_stats = match qbackend {
            QuantilesBackend::Exact => {
                let mut v = agg.gaps_no_mate.clone();
                v.sort_unstable();
                compute_stats_exact(&v)
            }
            QuantilesBackend::P2 => agg.gaps_nm_p2.as_ref().and_then(|o| o.stats()),
            QuantilesBackend::TDigest => agg.gaps_nm_td.as_mut().and_then(|o| o.stats()),
        };
        let gaps_nm_p05 = match qbackend {
            QuantilesBackend::Exact => {
                if agg.gaps_no_mate.is_empty() {
                    None
                } else {
                    let mut v = agg.gaps_no_mate.clone();
                    v.sort_unstable();
                    Some(quantile_sorted(&v, 0.05))
                }
            }
            QuantilesBackend::P2 => agg.gaps_nm_p2.as_ref().map(|o| o.q05()),
            QuantilesBackend::TDigest => agg.gaps_nm_td.as_mut().map(|o| o.q(0.05)),
        };
        let gaps_all_stats = match qbackend {
            QuantilesBackend::Exact => {
                let mut v = agg.gaps_all.clone();
                v.sort_unstable();
                compute_stats_exact(&v)
            }
            QuantilesBackend::P2 => agg.gaps_all_p2.as_ref().and_then(|o| o.stats()),
            QuantilesBackend::TDigest => agg.gaps_all_td.as_mut().and_then(|o| o.stats()),
        };
        let coverage = if agg.lines_ge2 > 0 {
            (match qbackend {
                QuantilesBackend::Exact => agg.gaps_no_mate.len() as f64,
                QuantilesBackend::P2 => {
                    agg.gaps_nm_p2.as_ref().map(|o| o.count).unwrap_or(0) as f64
                }
                QuantilesBackend::TDigest => {
                    agg.gaps_nm_td.as_ref().map(|o| o.count).unwrap_or(0) as f64
                }
            }) / (agg.lines_ge2 as f64)
        } else {
            0.0
        };
        // ambiguous rates (+ Wilson 95% CI)
        let k20 = if matches!(qbackend, QuantilesBackend::Exact) {
            agg.gaps_no_mate.iter().filter(|&&g| g <= 20).count()
        } else {
            agg.amb_le20_cnt
        };
        let k30 = if matches!(qbackend, QuantilesBackend::Exact) {
            agg.gaps_no_mate.iter().filter(|&&g| g <= 30).count()
        } else {
            agg.amb_le30_cnt
        };
        let n_amb = agg.lines_ge2;
        let amb20 = if n_amb > 0 {
            k20 as f64 / n_amb as f64
        } else {
            0.0
        };
        let amb30 = if n_amb > 0 {
            k30 as f64 / n_amb as f64
        } else {
            0.0
        };
        let (amb20_ci_low, amb20_ci_high) = wilson_ci(k20, n_amb, 1.96);
        let (amb30_ci_low, amb30_ci_high) = wilson_ci(k30, n_amb, 1.96);

        // time/nodes
        let t_stats = match qbackend {
            QuantilesBackend::Exact => compute_stats_exact(&agg.times_ms),
            QuantilesBackend::P2 => agg.times_p2.as_ref().and_then(|o| o.stats()),
            QuantilesBackend::TDigest => agg.times_td.as_mut().and_then(|o| o.stats()),
        };
        let n_stats = match qbackend {
            QuantilesBackend::Exact => compute_stats_exact(&agg.nodes),
            QuantilesBackend::P2 => agg.nodes_p2.as_ref().and_then(|o| o.stats()),
            QuantilesBackend::TDigest => agg.nodes_td.as_mut().and_then(|o| o.stats()),
        };
        let seldef_stats = match qbackend {
            QuantilesBackend::Exact => compute_stats_exact(&agg.seldef),
            QuantilesBackend::P2 => agg.seldef_p2.as_ref().and_then(|o| o.stats()),
            QuantilesBackend::TDigest => agg.seldef_td.as_mut().and_then(|o| o.stats()),
        };

        if want_summary {
            println!("summary:");
            if !matches!(qbackend, QuantilesBackend::Exact) {
                println!(
                    "  quantiles: approx (using {} backend)",
                    if matches!(qbackend, QuantilesBackend::P2) {
                        "p2"
                    } else {
                        "tdigest"
                    }
                );
            }
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
            if let Some(ss) = seldef_stats {
                println!("  seldef: median={} p90={} (delta={})", ss.p50, ss.p90, seldef_delta);
            }
            println!(
                "  ambiguous: rate20={:.3} [ci95 {:.3}-{:.3}] rate30={:.3} [ci95 {:.3}-{:.3}]",
                amb20, amb20_ci_low, amb20_ci_high, amb30, amb30_ci_low, amb30_ci_high
            );
            // Also print coverage-based ambiguous rates (divide by gap2_no_mate count)
            let amb20_cov = if let Some(s) = gaps_nm_stats {
                if s.count > 0 {
                    if matches!(qbackend, QuantilesBackend::Exact) {
                        agg.gaps_no_mate.iter().filter(|&&g| g <= 20).count() as f64
                            / s.count as f64
                    } else {
                        agg.amb_le20_cnt as f64 / s.count as f64
                    }
                } else {
                    0.0
                }
            } else {
                0.0
            };
            let amb30_cov = if let Some(s) = gaps_nm_stats {
                if s.count > 0 {
                    if matches!(qbackend, QuantilesBackend::Exact) {
                        agg.gaps_no_mate.iter().filter(|&&g| g <= 30).count() as f64
                            / s.count as f64
                    } else {
                        agg.amb_le30_cnt as f64 / s.count as f64
                    }
                } else {
                    0.0
                }
            } else {
                0.0
            };
            println!("  ambiguous_covered: rate20={:.3} rate30={:.3}", amb20_cov, amb30_cov);
            // compact bound/tt/null-lmr line
            let tt_mean = if agg.tt_rates.is_empty() {
                None
            } else {
                Some(agg.tt_rates.iter().sum::<f64>() / agg.tt_rates.len() as f64)
            };
            let tt_median = if agg.tt_rates.is_empty() {
                None
            } else {
                let mut v = agg.tt_rates.clone();
                v.sort_by(|a, b| a.partial_cmp(b).unwrap());
                Some(v[v.len() / 2])
            };
            let null_rate = if agg.total > 0 {
                Some(agg.used_null_sum as f64 / agg.total as f64)
            } else {
                None
            };
            let lmr_mean = if agg.total > 0 {
                Some(agg.lmr_sum as f64 / agg.total as f64)
            } else {
                None
            };
            let lmr_pn_mean = if agg.lmr_per_node.is_empty() {
                None
            } else {
                Some(agg.lmr_per_node.iter().sum::<f64>() / agg.lmr_per_node.len() as f64)
            };
            println!(
                "  bound: b1(e/l/u)={}/{}/{} b2(e/l/u)={}/{}/{}",
                agg.b1_exact, agg.b1_lower, agg.b1_upper, agg.b2_exact, agg.b2_lower, agg.b2_upper
            );
            println!(
                "  tt/null-lmr: tt_mean={:.3} tt_med={:.3} null_rate={:.3} lmr_mean={:.2} lmr_per_node_mean={:.6}",
                tt_mean.unwrap_or(0.0), tt_median.unwrap_or(0.0), null_rate.unwrap_or(0.0), lmr_mean.unwrap_or(0.0), lmr_pn_mean.unwrap_or(0.0)
            );
            // invariants summary (one line)
            let total = agg.total.max(1);
            let r1 = agg.inv_mpv_lt_expected as f64 / total as f64;
            let r2 = agg.inv_gap_with_non_exact as f64 / total as f64;
            let r3 = agg.inv_no_legal_but_empty as f64 / total as f64;
            let r4 = agg.inv_fallback_true_no_reason as f64 / total as f64;
            let r5 = agg.inv_mate_mixed_into_no_mate as f64 / total as f64;
            println!(
                "  invariants: mpv_lt_expected={}({:.3}) gap_with_non_exact={}({:.3}) no_legal_but_empty={}({:.3}) fallback_true_no_reason={}({:.3}) mate_mixed_into_no_mate={}({:.3})",
                agg.inv_mpv_lt_expected, r1,
                agg.inv_gap_with_non_exact, r2,
                agg.inv_no_legal_but_empty, r3,
                agg.inv_fallback_true_no_reason, r4,
                agg.inv_mate_mixed_into_no_mate, r5
            );
            if agg.non_exact_total > 0 {
                let t = agg.non_exact_total as f64;
                println!(
                    "  non_exact_reason: budget={:.3} aspiration={:.3} fail_high={:.3} unknown={:.3} (n={})",
                    (agg.non_exact_budget as f64) / t,
                    (agg.non_exact_aspiration as f64) / t,
                    (agg.non_exact_fail_high as f64) / t,
                    (agg.non_exact_unknown as f64) / t,
                    agg.non_exact_total
                );
            }

            // If exactly 2 inputs and no dedup, show simple delta summary (file2 - file1)
            if let Some(ref mut p) = pair {
                let a1 = &mut p.a1;
                let a2 = &mut p.a2;
                let eb1 = if a1.both_exact_denom > 0 {
                    a1.both_exact as f64 / a1.both_exact_denom as f64
                } else {
                    0.0
                };
                let eb2 = if a2.both_exact_denom > 0 {
                    a2.both_exact as f64 / a2.both_exact_denom as f64
                } else {
                    0.0
                };
                // respect backend for deltas
                let t1 = match qbackend {
                    QuantilesBackend::Exact => compute_stats_exact(&a1.times_ms),
                    QuantilesBackend::P2 => a1.times_p2.as_ref().and_then(|o| o.stats()),
                    QuantilesBackend::TDigest => a1.times_td.as_mut().and_then(|o| o.stats()),
                };
                let t2 = match qbackend {
                    QuantilesBackend::Exact => compute_stats_exact(&a2.times_ms),
                    QuantilesBackend::P2 => a2.times_p2.as_ref().and_then(|o| o.stats()),
                    QuantilesBackend::TDigest => a2.times_td.as_mut().and_then(|o| o.stats()),
                };
                let n1 = match qbackend {
                    QuantilesBackend::Exact => compute_stats_exact(&a1.nodes),
                    QuantilesBackend::P2 => a1.nodes_p2.as_ref().and_then(|o| o.stats()),
                    QuantilesBackend::TDigest => a1.nodes_td.as_mut().and_then(|o| o.stats()),
                };
                let n2 = match qbackend {
                    QuantilesBackend::Exact => compute_stats_exact(&a2.nodes),
                    QuantilesBackend::P2 => a2.nodes_p2.as_ref().and_then(|o| o.stats()),
                    QuantilesBackend::TDigest => a2.nodes_td.as_mut().and_then(|o| o.stats()),
                };
                let g1 = match qbackend {
                    QuantilesBackend::Exact => compute_stats_exact(&a1.gaps_no_mate),
                    QuantilesBackend::P2 => a1.gaps_nm_p2.as_ref().and_then(|o| o.stats()),
                    QuantilesBackend::TDigest => a1.gaps_nm_td.as_mut().and_then(|o| o.stats()),
                };
                let g2 = match qbackend {
                    QuantilesBackend::Exact => compute_stats_exact(&a2.gaps_no_mate),
                    QuantilesBackend::P2 => a2.gaps_nm_p2.as_ref().and_then(|o| o.stats()),
                    QuantilesBackend::TDigest => a2.gaps_nm_td.as_mut().and_then(|o| o.stats()),
                };
                let delta_gap_p50 = if let (Some(s1), Some(s2)) = (g1, g2) {
                    Some(s2.p50 - s1.p50)
                } else {
                    None
                };
                let delta_t_p50 = if let (Some(s1), Some(s2)) = (t1, t2) {
                    Some(s2.p50 - s1.p50)
                } else {
                    None
                };
                let delta_t_p95 = if let (Some(s1), Some(s2)) = (t1, t2) {
                    Some(s2.p95 - s1.p95)
                } else {
                    None
                };
                let delta_n_p50 = if let (Some(s1), Some(s2)) = (n1, n2) {
                    Some(s2.p50 - s1.p50)
                } else {
                    None
                };
                let delta_n_p95 = if let (Some(s1), Some(s2)) = (n1, n2) {
                    Some(s2.p95 - s1.p95)
                } else {
                    None
                };
                // summary delta line (compact)
                let tt_str = if !a1.tt_rates.is_empty() && !a2.tt_rates.is_empty() {
                    let v = a2.tt_rates.iter().sum::<f64>() / a2.tt_rates.len() as f64
                        - a1.tt_rates.iter().sum::<f64>() / a1.tt_rates.len() as f64;
                    format!("{:+.3}", v)
                } else {
                    "NA".to_string()
                };
                let null_str = if a1.total > 0 && a2.total > 0 {
                    let v = a2.used_null_sum as f64 / a2.total as f64
                        - a1.used_null_sum as f64 / a1.total as f64;
                    format!("{:+.3}", v)
                } else {
                    "NA".to_string()
                };
                let lmr_delta = a2.lmr_sum as f64 / (a2.total as f64).max(1.0)
                    - a1.lmr_sum as f64 / (a1.total as f64).max(1.0);
                println!(
                    "delta: exact_both={:+.3} gap_p50={} time_ms_p50={} time_ms_p95={} nodes_p50={} nodes_p95={} tt_mean={} null_rate={} lmr_mean={:+.2}",
                    eb2 - eb1,
                    delta_gap_p50.map(|v| v.to_string()).unwrap_or_else(|| "NA".into()),
                    delta_t_p50.map(|v| v.to_string()).unwrap_or_else(|| "NA".into()),
                    delta_t_p95.map(|v| v.to_string()).unwrap_or_else(|| "NA".into()),
                    delta_n_p50.map(|v| v.to_string()).unwrap_or_else(|| "NA".into()),
                    delta_n_p95.map(|v| v.to_string()).unwrap_or_else(|| "NA".into()),
                    tt_str,
                    null_str,
                    lmr_delta,
                );
            }
        }

        if want_json || want_csv {
            let mut obj = serde_json::Map::new();
            let qb = match qbackend {
                QuantilesBackend::Exact => "exact",
                QuantilesBackend::P2 => "p2",
                QuantilesBackend::TDigest => "tdigest",
            };
            obj.insert("quantiles_backend".into(), json!(qb));

            obj.insert("exact_top1".into(), json!(r1));
            obj.insert("exact_top1_n".into(), json!(n1));
            obj.insert("exact_top1_denom".into(), json!(d1));
            obj.insert("exact_top1_ci_low".into(), json!(l1));
            obj.insert("exact_top1_ci_high".into(), json!(h1));
            obj.insert("exact_both".into(), json!(r2));
            obj.insert("exact_both_n".into(), json!(n2));
            obj.insert("exact_both_denom".into(), json!(d2));
            obj.insert("exact_both_ci_low".into(), json!(l2));
            obj.insert("exact_both_ci_high".into(), json!(h2));

            match gaps_nm_stats {
                Some(s) => {
                    obj.insert("gap_no_mate_count".into(), json!(s.count));
                    obj.insert("gap_no_mate_mean".into(), json!(s.mean));
                    obj.insert("gap_no_mate_median".into(), json!(s.p50));
                    obj.insert("gap_no_mate_p95".into(), json!(s.p95));
                }
                None => {
                    obj.insert("gap_no_mate_count".into(), Value::Null);
                    obj.insert("gap_no_mate_mean".into(), Value::Null);
                    obj.insert("gap_no_mate_median".into(), Value::Null);
                    obj.insert("gap_no_mate_p95".into(), Value::Null);
                }
            }
            match gaps_nm_p05 {
                Some(x) => {
                    obj.insert("gap_no_mate_p05".into(), json!(x));
                }
                None => {
                    obj.insert("gap_no_mate_p05".into(), Value::Null);
                }
            }
            obj.insert("gap_no_mate_coverage".into(), json!(coverage));
            match gaps_all_stats {
                Some(s) => {
                    obj.insert("gap_all_median".into(), json!(s.p50));
                }
                None => {
                    obj.insert("gap_all_median".into(), Value::Null);
                }
            }

            obj.insert("fallback_used_count".into(), json!(agg.fallback_record));
            obj.insert(
                "fallback_used_rate".into(),
                json!(if agg.total > 0 {
                    (agg.fallback_record as f64) / (agg.total as f64)
                } else {
                    0.0
                }),
            );
            obj.insert("bound1_exact".into(), json!(agg.b1_exact));
            obj.insert("bound1_lower".into(), json!(agg.b1_lower));
            obj.insert("bound1_upper".into(), json!(agg.b1_upper));
            obj.insert("bound1_other".into(), json!(agg.b1_other));
            obj.insert("bound2_exact".into(), json!(agg.b2_exact));
            obj.insert("bound2_lower".into(), json!(agg.b2_lower));
            obj.insert("bound2_upper".into(), json!(agg.b2_upper));
            obj.insert("bound2_other".into(), json!(agg.b2_other));
            obj.insert("used_null_sum".into(), json!(agg.used_null_sum));
            obj.insert(
                "used_null_rate".into(),
                json!(if agg.total > 0 {
                    (agg.used_null_sum as f64) / (agg.total as f64)
                } else {
                    0.0
                }),
            );
            obj.insert("lmr_sum".into(), json!(agg.lmr_sum));
            obj.insert("lmr_mean".into(), json!(agg.lmr_sum as f64 / (agg.total as f64).max(1.0)));
            // seldepth deficit
            match seldef_stats {
                Some(s) => {
                    obj.insert("seldepth_deficit_median".into(), json!(s.p50));
                    obj.insert("seldepth_deficit_p90".into(), json!(s.p90));
                }
                None => {
                    obj.insert("seldepth_deficit_median".into(), Value::Null);
                    obj.insert("seldepth_deficit_p90".into(), Value::Null);
                }
            }
            obj.insert("seldepth_deficit_delta".into(), json!(seldef_delta));
            obj.insert(
                "lmr_per_node_mean".into(),
                json!(if agg.lmr_per_node.is_empty() {
                    0.0
                } else {
                    agg.lmr_per_node.iter().sum::<f64>() / agg.lmr_per_node.len() as f64
                }),
            );
            obj.insert(
                "tt_hit_rate_mean".into(),
                json!(if agg.tt_rates.is_empty() {
                    0.0
                } else {
                    agg.tt_rates.iter().sum::<f64>() / agg.tt_rates.len() as f64
                }),
            );
            if agg.tt_rates.is_empty() {
                obj.insert("tt_hit_rate_median".into(), json!(0.0));
            } else {
                let mut v = agg.tt_rates.clone();
                v.sort_by(|a, b| a.partial_cmp(b).unwrap());
                obj.insert("tt_hit_rate_median".into(), json!(v[v.len() / 2]));
            }
            match t_stats {
                Some(s) => {
                    obj.insert("time_ms_min".into(), json!(s.min));
                    obj.insert("time_ms_median".into(), json!(s.p50));
                    obj.insert("time_ms_p90".into(), json!(s.p90));
                    obj.insert("time_ms_p95".into(), json!(s.p95));
                    obj.insert("time_ms_p99".into(), json!(s.p99));
                }
                None => {
                    obj.insert("time_ms_min".into(), Value::Null);
                    obj.insert("time_ms_median".into(), Value::Null);
                    obj.insert("time_ms_p90".into(), Value::Null);
                    obj.insert("time_ms_p95".into(), Value::Null);
                    obj.insert("time_ms_p99".into(), Value::Null);
                }
            }
            match n_stats {
                Some(s) => {
                    obj.insert("nodes_min".into(), json!(s.min));
                    obj.insert("nodes_median".into(), json!(s.p50));
                    obj.insert("nodes_p90".into(), json!(s.p90));
                    obj.insert("nodes_p95".into(), json!(s.p95));
                    obj.insert("nodes_p99".into(), json!(s.p99));
                }
                None => {
                    obj.insert("nodes_min".into(), Value::Null);
                    obj.insert("nodes_median".into(), Value::Null);
                    obj.insert("nodes_p90".into(), Value::Null);
                    obj.insert("nodes_p95".into(), Value::Null);
                    obj.insert("nodes_p99".into(), Value::Null);
                }
            }
            obj.insert("ambiguous_rate_20".into(), json!(amb20));
            obj.insert("ambiguous_rate_20_ci_low".into(), json!(amb20_ci_low));
            obj.insert("ambiguous_rate_20_ci_high".into(), json!(amb20_ci_high));
            obj.insert("ambiguous_rate_30".into(), json!(amb30));
            obj.insert("ambiguous_rate_30_ci_low".into(), json!(amb30_ci_low));
            obj.insert("ambiguous_rate_30_ci_high".into(), json!(amb30_ci_high));
            // invariants -> JSON
            let total_json = agg.total.max(1);
            obj.insert("inv_mpv_lt_expected_count".into(), json!(agg.inv_mpv_lt_expected));
            obj.insert(
                "inv_mpv_lt_expected_rate".into(),
                json!(agg.inv_mpv_lt_expected as f64 / total_json as f64),
            );
            obj.insert("inv_gap_with_non_exact_count".into(), json!(agg.inv_gap_with_non_exact));
            obj.insert(
                "inv_gap_with_non_exact_rate".into(),
                json!(agg.inv_gap_with_non_exact as f64 / total_json as f64),
            );
            obj.insert("inv_no_legal_but_empty_count".into(), json!(agg.inv_no_legal_but_empty));
            obj.insert(
                "inv_no_legal_but_empty_rate".into(),
                json!(agg.inv_no_legal_but_empty as f64 / total_json as f64),
            );
            obj.insert(
                "inv_fallback_true_no_reason_count".into(),
                json!(agg.inv_fallback_true_no_reason),
            );
            obj.insert(
                "inv_fallback_true_no_reason_rate".into(),
                json!(agg.inv_fallback_true_no_reason as f64 / total_json as f64),
            );
            obj.insert(
                "inv_mate_mixed_into_no_mate_count".into(),
                json!(agg.inv_mate_mixed_into_no_mate),
            );
            obj.insert(
                "inv_mate_mixed_into_no_mate_rate".into(),
                json!(agg.inv_mate_mixed_into_no_mate as f64 / total_json as f64),
            );

            // manifest embedding
            if let Some(mv) = manifest_json.as_ref() {
                obj.insert("manifest".into(), mv.clone());
            } else {
                obj.insert("manifest".into(), Value::Null);
            }

            // Extract selected manifest fields for CSV columns and convenience
            fn pick_str(v: &Value, keys: &[&str]) -> Option<String> {
                for k in keys {
                    if let Some(val) = v.get(*k) {
                        match val {
                            Value::String(s) => return Some(s.clone()),
                            Value::Number(n) => return Some(n.to_string()),
                            Value::Bool(b) => {
                                return Some(if *b { "true" } else { "false" }.to_string())
                            }
                            _ => {}
                        }
                    }
                }
                None
            }
            if let Some(mv) = manifest_json.as_ref() {
                if let Some(s) =
                    pick_str(mv, &["git_hash", "git", "git_commit", "commit", "commit_hash"])
                {
                    obj.insert("manifest_git_commit".into(), json!(s));
                }
                if let Some(s) = pick_str(mv, &["teacher_profile", "profile"]) {
                    obj.insert("manifest_teacher_profile".into(), json!(s));
                }
                if let Some(s) = pick_str(mv, &["multipv"]) {
                    obj.insert("manifest_multipv".into(), json!(s));
                }
                if let Some(s) = pick_str(mv, &["nodes", "nodes_per_pos", "nodes_budget"]) {
                    obj.insert("manifest_nodes".into(), json!(s));
                }
                if let Some(s) = pick_str(mv, &["min_depth", "minDepth"]) {
                    obj.insert("manifest_min_depth".into(), json!(s));
                }
                if let Some(s) = pick_str(mv, &["hash_mb", "tt_mb", "hash"]) {
                    obj.insert("manifest_hash_mb".into(), json!(s));
                }
                if let Some(s) = pick_str(mv, &["threads", "num_threads", "search_threads"]) {
                    obj.insert("manifest_threads".into(), json!(s));
                }
            } else {
                obj.insert("manifest_git_commit".into(), Value::Null);
                obj.insert("manifest_teacher_profile".into(), Value::Null);
                obj.insert("manifest_multipv".into(), Value::Null);
                obj.insert("manifest_nodes".into(), Value::Null);
                obj.insert("manifest_min_depth".into(), Value::Null);
                obj.insert("manifest_hash_mb".into(), Value::Null);
                obj.insert("manifest_threads".into(), Value::Null);
            }
            // non-exact reason breakdown
            obj.insert("non_exact_total".into(), json!(agg.non_exact_total));
            let tne = agg.non_exact_total.max(1) as f64;
            obj.insert("non_exact_reason_budget_count".into(), json!(agg.non_exact_budget));
            obj.insert(
                "non_exact_reason_budget_rate".into(),
                json!(agg.non_exact_budget as f64 / tne),
            );
            obj.insert("non_exact_reason_aspiration_count".into(), json!(agg.non_exact_aspiration));
            obj.insert(
                "non_exact_reason_aspiration_rate".into(),
                json!(agg.non_exact_aspiration as f64 / tne),
            );
            obj.insert("non_exact_reason_fail_high_count".into(), json!(agg.non_exact_fail_high));
            obj.insert(
                "non_exact_reason_fail_high_rate".into(),
                json!(agg.non_exact_fail_high as f64 / tne),
            );
            obj.insert("non_exact_reason_unknown_count".into(), json!(agg.non_exact_unknown));
            obj.insert(
                "non_exact_reason_unknown_rate".into(),
                json!(agg.non_exact_unknown as f64 / tne),
            );

            // Add ambiguous rates (coverage-based)
            let amb20_cov = if let Some(s) = gaps_nm_stats {
                if s.count > 0 {
                    if matches!(qbackend, QuantilesBackend::Exact) {
                        agg.gaps_no_mate.iter().filter(|&&g| g <= 20).count() as f64
                            / s.count as f64
                    } else {
                        agg.amb_le20_cnt as f64 / s.count as f64
                    }
                } else {
                    0.0
                }
            } else {
                0.0
            };
            let amb30_cov = if let Some(s) = gaps_nm_stats {
                if s.count > 0 {
                    if matches!(qbackend, QuantilesBackend::Exact) {
                        agg.gaps_no_mate.iter().filter(|&&g| g <= 30).count() as f64
                            / s.count as f64
                    } else {
                        agg.amb_le30_cnt as f64 / s.count as f64
                    }
                } else {
                    0.0
                }
            } else {
                0.0
            };
            obj.insert("ambiguous_rate_20_covered".into(), json!(amb20_cov));
            obj.insert("ambiguous_rate_30_covered".into(), json!(amb30_cov));

            // Optional: add delta to JSON (use precomputed pair)
            if let Some(ref mut p) = pair {
                let a1 = &mut p.a1;
                let a2 = &mut p.a2;
                let eb1 = if a1.both_exact_denom > 0 {
                    a1.both_exact as f64 / a1.both_exact_denom as f64
                } else {
                    0.0
                };
                let eb2 = if a2.both_exact_denom > 0 {
                    a2.both_exact as f64 / a2.both_exact_denom as f64
                } else {
                    0.0
                };
                obj.insert("delta_exact_both".into(), json!(eb2 - eb1));

                // time/nodes/gap deltas with backend respect
                let t1 = match qbackend {
                    QuantilesBackend::Exact => compute_stats_exact(&a1.times_ms),
                    QuantilesBackend::P2 => a1.times_p2.as_ref().and_then(|o| o.stats()),
                    QuantilesBackend::TDigest => a1.times_td.as_mut().and_then(|o| o.stats()),
                };
                let t2 = match qbackend {
                    QuantilesBackend::Exact => compute_stats_exact(&a2.times_ms),
                    QuantilesBackend::P2 => a2.times_p2.as_ref().and_then(|o| o.stats()),
                    QuantilesBackend::TDigest => a2.times_td.as_mut().and_then(|o| o.stats()),
                };
                let n1 = match qbackend {
                    QuantilesBackend::Exact => compute_stats_exact(&a1.nodes),
                    QuantilesBackend::P2 => a1.nodes_p2.as_ref().and_then(|o| o.stats()),
                    QuantilesBackend::TDigest => a1.nodes_td.as_mut().and_then(|o| o.stats()),
                };
                let n2 = match qbackend {
                    QuantilesBackend::Exact => compute_stats_exact(&a2.nodes),
                    QuantilesBackend::P2 => a2.nodes_p2.as_ref().and_then(|o| o.stats()),
                    QuantilesBackend::TDigest => a2.nodes_td.as_mut().and_then(|o| o.stats()),
                };
                let g1 = match qbackend {
                    QuantilesBackend::Exact => compute_stats_exact(&a1.gaps_no_mate),
                    QuantilesBackend::P2 => a1.gaps_nm_p2.as_ref().and_then(|o| o.stats()),
                    QuantilesBackend::TDigest => a1.gaps_nm_td.as_mut().and_then(|o| o.stats()),
                };
                let g2 = match qbackend {
                    QuantilesBackend::Exact => compute_stats_exact(&a2.gaps_no_mate),
                    QuantilesBackend::P2 => a2.gaps_nm_p2.as_ref().and_then(|o| o.stats()),
                    QuantilesBackend::TDigest => a2.gaps_nm_td.as_mut().and_then(|o| o.stats()),
                };

                match (g1, g2) {
                    (Some(s1), Some(s2)) => {
                        obj.insert("delta_gap_no_mate_median".into(), json!(s2.p50 - s1.p50));
                    }
                    _ => {
                        obj.insert("delta_gap_no_mate_median".into(), Value::Null);
                    }
                }
                match (t1, t2) {
                    (Some(s1), Some(s2)) => {
                        obj.insert("delta_time_ms_median".into(), json!(s2.p50 - s1.p50));
                        obj.insert("delta_time_ms_p95".into(), json!(s2.p95 - s1.p95));
                    }
                    _ => {
                        obj.insert("delta_time_ms_median".into(), Value::Null);
                        obj.insert("delta_time_ms_p95".into(), Value::Null);
                    }
                }
                match (n1, n2) {
                    (Some(s1), Some(s2)) => {
                        obj.insert("delta_nodes_median".into(), json!(s2.p50 - s1.p50));
                        obj.insert("delta_nodes_p95".into(), json!(s2.p95 - s1.p95));
                    }
                    _ => {
                        obj.insert("delta_nodes_median".into(), Value::Null);
                        obj.insert("delta_nodes_p95".into(), Value::Null);
                    }
                }
                if !a1.tt_rates.is_empty() && !a2.tt_rates.is_empty() {
                    obj.insert(
                        "delta_tt_hit_rate_mean".into(),
                        json!(
                            a2.tt_rates.iter().sum::<f64>() / a2.tt_rates.len() as f64
                                - a1.tt_rates.iter().sum::<f64>() / a1.tt_rates.len() as f64
                        ),
                    );
                } else {
                    obj.insert("delta_tt_hit_rate_mean".into(), Value::Null);
                }
                if a1.total > 0 && a2.total > 0 {
                    obj.insert(
                        "delta_null_rate".into(),
                        json!(
                            a2.used_null_sum as f64 / a2.total as f64
                                - a1.used_null_sum as f64 / a1.total as f64
                        ),
                    );
                } else {
                    obj.insert("delta_null_rate".into(), Value::Null);
                }
                obj.insert(
                    "delta_lmr_mean".into(),
                    json!(
                        a2.lmr_sum as f64 / (a2.total as f64).max(1.0)
                            - a1.lmr_sum as f64 / (a1.total as f64).max(1.0)
                    ),
                );
            } else {
                obj.insert("delta_exact_both".into(), Value::Null);
                obj.insert("delta_gap_no_mate_median".into(), Value::Null);
                obj.insert("delta_time_ms_median".into(), Value::Null);
                obj.insert("delta_time_ms_p95".into(), Value::Null);
                obj.insert("delta_nodes_median".into(), Value::Null);
                obj.insert("delta_nodes_p95".into(), Value::Null);
                obj.insert("delta_tt_hit_rate_mean".into(), Value::Null);
                obj.insert("delta_null_rate".into(), Value::Null);
                obj.insert("delta_lmr_mean".into(), Value::Null);
            }

            let json_obj = Value::Object(obj);
            if want_json {
                println!("{}", json_obj);
            }
            if want_csv {
                // stable header and row
                let header = [
                    "exact_top1",
                    "exact_top1_n",
                    "exact_top1_denom",
                    "exact_top1_ci_low",
                    "exact_top1_ci_high",
                    "exact_both",
                    "exact_both_n",
                    "exact_both_denom",
                    "exact_both_ci_low",
                    "exact_both_ci_high",
                    "gap_no_mate_count",
                    "gap_no_mate_mean",
                    "gap_no_mate_median",
                    "gap_no_mate_p05",
                    "gap_no_mate_p95",
                    "gap_no_mate_coverage",
                    "gap_all_median",
                    "ambiguous_rate_20",
                    "ambiguous_rate_20_ci_low",
                    "ambiguous_rate_20_ci_high",
                    "ambiguous_rate_30",
                    "ambiguous_rate_30_ci_low",
                    "ambiguous_rate_30_ci_high",
                    "ambiguous_rate_20_covered",
                    "ambiguous_rate_30_covered",
                    "fallback_used_count",
                    "fallback_used_rate",
                    "bound1_exact",
                    "bound1_lower",
                    "bound1_upper",
                    "bound1_other",
                    "bound2_exact",
                    "bound2_lower",
                    "bound2_upper",
                    "bound2_other",
                    "used_null_sum",
                    "used_null_rate",
                    "lmr_sum",
                    "lmr_mean",
                    "lmr_per_node_mean",
                    "tt_hit_rate_mean",
                    "tt_hit_rate_median",
                    "time_ms_min",
                    "time_ms_median",
                    "time_ms_p90",
                    "time_ms_p95",
                    "time_ms_p99",
                    "nodes_min",
                    "nodes_median",
                    "nodes_p90",
                    "nodes_p95",
                    "nodes_p99",
                    "seldepth_deficit_median",
                    "seldepth_deficit_p90",
                    "seldepth_deficit_delta",
                    // invariants (counts and rates)
                    "inv_mpv_lt_expected_count",
                    "inv_mpv_lt_expected_rate",
                    "inv_gap_with_non_exact_count",
                    "inv_gap_with_non_exact_rate",
                    "inv_no_legal_but_empty_count",
                    "inv_no_legal_but_empty_rate",
                    "inv_fallback_true_no_reason_count",
                    "inv_fallback_true_no_reason_rate",
                    "inv_mate_mixed_into_no_mate_count",
                    "inv_mate_mixed_into_no_mate_rate",
                    // manifest flattened fields
                    "manifest_git_commit",
                    "manifest_teacher_profile",
                    "manifest_multipv",
                    "manifest_nodes",
                    "manifest_min_depth",
                    "manifest_hash_mb",
                    "manifest_threads",
                    "non_exact_total",
                    "non_exact_reason_budget_count",
                    "non_exact_reason_budget_rate",
                    "non_exact_reason_aspiration_count",
                    "non_exact_reason_aspiration_rate",
                    "non_exact_reason_fail_high_count",
                    "non_exact_reason_fail_high_rate",
                    "non_exact_reason_unknown_count",
                    "non_exact_reason_unknown_rate",
                    // optional delta (when exactly two inputs & no dedup)
                    "delta_exact_both",
                    "delta_gap_no_mate_median",
                    "delta_time_ms_median",
                    "delta_time_ms_p95",
                    "delta_nodes_median",
                    "delta_nodes_p95",
                    "delta_tt_hit_rate_mean",
                    "delta_null_rate",
                    "delta_lmr_mean",
                    // backend info
                    "quantiles_backend",
                ];
                if want_csv_header {
                    println!("{}", header.join(","));
                }
                // Pull values from json (they may be null)
                let v = json_obj;
                let mut row = Vec::with_capacity(header.len());
                for &k in &header {
                    let s = match v.get(k) {
                        Some(serde_json::Value::Null) | None => String::new(),
                        Some(serde_json::Value::Number(n)) => n.to_string(),
                        Some(serde_json::Value::Bool(b)) => {
                            if *b {
                                "1".into()
                            } else {
                                "0".into()
                            }
                        }
                        Some(serde_json::Value::String(s)) => csv_escape(s),
                        _ => String::new(),
                    };
                    row.push(s);
                }
                println!("{}", row.join(","));
            }
        }

        if let Some(g) = gate {
            let mut failed = false;
            let mut pass_cnt: usize = 0;
            let mut fail_cnt: usize = 0;
            // Evaluate each threshold if present
            if let Some(th) = g.exact_top1_min {
                let ok = r1 >= th;
                println!(
                    "GATE exact_top1_min {:.3} vs {:.3}: {}",
                    r1,
                    th,
                    if ok { "PASS" } else { "FAIL" }
                );
                if ok {
                    pass_cnt += 1;
                } else {
                    fail_cnt += 1;
                }
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
                if ok {
                    pass_cnt += 1;
                } else {
                    fail_cnt += 1;
                }
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
                if ok {
                    pass_cnt += 1;
                } else {
                    fail_cnt += 1;
                }
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
                if ok {
                    pass_cnt += 1;
                } else {
                    fail_cnt += 1;
                }
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
                if ok {
                    pass_cnt += 1;
                } else {
                    fail_cnt += 1;
                }
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
                if ok {
                    pass_cnt += 1;
                } else {
                    fail_cnt += 1;
                }
                failed |= !ok;
            }
            if let Some(th) = g.ambiguous_rate_20_max {
                let ok = amb20 <= th;
                println!(
                    "GATE ambiguous_rate_20_max {:.3} <= {:.3}: {}",
                    amb20,
                    th,
                    if ok { "PASS" } else { "FAIL" }
                );
                if ok {
                    pass_cnt += 1;
                } else {
                    fail_cnt += 1;
                }
                failed |= !ok;
            }
            // invariants (counts)
            if let Some(th) = g.mpv_lt_expected_max {
                let ok = agg.inv_mpv_lt_expected <= th;
                println!(
                    "GATE mpv_lt_expected_max {} <= {}: {}",
                    agg.inv_mpv_lt_expected,
                    th,
                    if ok { "PASS" } else { "FAIL" }
                );
                if ok {
                    pass_cnt += 1;
                } else {
                    fail_cnt += 1;
                }
                failed |= !ok;
            }
            if let Some(th) = g.mpv_lt_expected_rate_max {
                let total = agg.total.max(1) as f64;
                let rate = (agg.inv_mpv_lt_expected as f64) / total;
                let ok = rate <= th;
                println!(
                    "GATE mpv_lt_expected_rate_max {:.3} <= {:.3}: {}",
                    rate,
                    th,
                    if ok { "PASS" } else { "FAIL" }
                );
                if ok {
                    pass_cnt += 1;
                } else {
                    fail_cnt += 1;
                }
                failed |= !ok;
            }
            if let Some(th) = g.gap_with_non_exact_max {
                let ok = agg.inv_gap_with_non_exact <= th;
                println!(
                    "GATE gap_with_non_exact_max {} <= {}: {}",
                    agg.inv_gap_with_non_exact,
                    th,
                    if ok { "PASS" } else { "FAIL" }
                );
                if ok {
                    pass_cnt += 1;
                } else {
                    fail_cnt += 1;
                }
                failed |= !ok;
            }
            if let Some(th) = g.gap_with_non_exact_rate_max {
                let total = agg.total.max(1) as f64;
                let rate = (agg.inv_gap_with_non_exact as f64) / total;
                let ok = rate <= th;
                println!(
                    "GATE gap_with_non_exact_rate_max {:.3} <= {:.3}: {}",
                    rate,
                    th,
                    if ok { "PASS" } else { "FAIL" }
                );
                if ok {
                    pass_cnt += 1;
                } else {
                    fail_cnt += 1;
                }
                failed |= !ok;
            }
            if let Some(th) = g.no_legal_but_empty_max {
                let ok = agg.inv_no_legal_but_empty <= th;
                println!(
                    "GATE no_legal_but_empty_max {} <= {}: {}",
                    agg.inv_no_legal_but_empty,
                    th,
                    if ok { "PASS" } else { "FAIL" }
                );
                if ok {
                    pass_cnt += 1;
                } else {
                    fail_cnt += 1;
                }
                failed |= !ok;
            }
            if let Some(th) = g.no_legal_but_empty_rate_max {
                let total = agg.total.max(1) as f64;
                let rate = (agg.inv_no_legal_but_empty as f64) / total;
                let ok = rate <= th;
                println!(
                    "GATE no_legal_but_empty_rate_max {:.3} <= {:.3}: {}",
                    rate,
                    th,
                    if ok { "PASS" } else { "FAIL" }
                );
                if ok {
                    pass_cnt += 1;
                } else {
                    fail_cnt += 1;
                }
                failed |= !ok;
            }
            if let Some(th) = g.fallback_true_no_reason_max {
                let ok = agg.inv_fallback_true_no_reason <= th;
                println!(
                    "GATE fallback_true_no_reason_max {} <= {}: {}",
                    agg.inv_fallback_true_no_reason,
                    th,
                    if ok { "PASS" } else { "FAIL" }
                );
                if ok {
                    pass_cnt += 1;
                } else {
                    fail_cnt += 1;
                }
                failed |= !ok;
            }
            if let Some(th) = g.fallback_true_no_reason_rate_max {
                let total = agg.total.max(1) as f64;
                let rate = (agg.inv_fallback_true_no_reason as f64) / total;
                let ok = rate <= th;
                println!(
                    "GATE fallback_true_no_reason_rate_max {:.3} <= {:.3}: {}",
                    rate,
                    th,
                    if ok { "PASS" } else { "FAIL" }
                );
                if ok {
                    pass_cnt += 1;
                } else {
                    fail_cnt += 1;
                }
                failed |= !ok;
            }
            if let Some(th) = g.mate_mixed_into_no_mate_max {
                let ok = agg.inv_mate_mixed_into_no_mate <= th;
                println!(
                    "GATE mate_mixed_into_no_mate_max {} <= {}: {}",
                    agg.inv_mate_mixed_into_no_mate,
                    th,
                    if ok { "PASS" } else { "FAIL" }
                );
                if ok {
                    pass_cnt += 1;
                } else {
                    fail_cnt += 1;
                }
                failed |= !ok;
            }
            if let Some(th) = g.mate_mixed_into_no_mate_rate_max {
                let total = agg.total.max(1) as f64;
                let rate = (agg.inv_mate_mixed_into_no_mate as f64) / total;
                let ok = rate <= th;
                println!(
                    "GATE mate_mixed_into_no_mate_rate_max {:.3} <= {:.3}: {}",
                    rate,
                    th,
                    if ok { "PASS" } else { "FAIL" }
                );
                if ok {
                    pass_cnt += 1;
                } else {
                    fail_cnt += 1;
                }
                failed |= !ok;
            }

            // Delta gates (only when exactly 2 inputs & no dedup)
            let mut delta_eb: Option<f64> = None;
            let mut delta_gap50: Option<i64> = None;
            let mut delta_t50: Option<i64> = None;
            let mut delta_t95: Option<i64> = None;
            let mut delta_n50: Option<i64> = None;
            let mut delta_n95: Option<i64> = None;
            let mut delta_tt_mean: Option<f64> = None;
            let mut delta_null_rate: Option<f64> = None;
            let mut delta_lmr_mean: Option<f64> = None;
            if let Some(ref mut p) = pair {
                let a1 = &mut p.a1;
                let a2 = &mut p.a2;
                let eb1 = if a1.both_exact_denom > 0 {
                    a1.both_exact as f64 / a1.both_exact_denom as f64
                } else {
                    0.0
                };
                let eb2 = if a2.both_exact_denom > 0 {
                    a2.both_exact as f64 / a2.both_exact_denom as f64
                } else {
                    0.0
                };
                delta_eb = Some(eb2 - eb1);
                // time deltas (respect backend)
                let t1 = match qbackend {
                    QuantilesBackend::Exact => compute_stats_exact(&a1.times_ms),
                    QuantilesBackend::P2 => a1.times_p2.as_ref().and_then(|o| o.stats()),
                    QuantilesBackend::TDigest => a1.times_td.as_mut().and_then(|o| o.stats()),
                };
                let t2 = match qbackend {
                    QuantilesBackend::Exact => compute_stats_exact(&a2.times_ms),
                    QuantilesBackend::P2 => a2.times_p2.as_ref().and_then(|o| o.stats()),
                    QuantilesBackend::TDigest => a2.times_td.as_mut().and_then(|o| o.stats()),
                };
                if let (Some(s1), Some(s2)) = (t1, t2) {
                    delta_t50 = Some(s2.p50 - s1.p50);
                    delta_t95 = Some(s2.p95 - s1.p95);
                }
                // nodes deltas (respect backend)
                let n1 = match qbackend {
                    QuantilesBackend::Exact => compute_stats_exact(&a1.nodes),
                    QuantilesBackend::P2 => a1.nodes_p2.as_ref().and_then(|o| o.stats()),
                    QuantilesBackend::TDigest => a1.nodes_td.as_mut().and_then(|o| o.stats()),
                };
                let n2 = match qbackend {
                    QuantilesBackend::Exact => compute_stats_exact(&a2.nodes),
                    QuantilesBackend::P2 => a2.nodes_p2.as_ref().and_then(|o| o.stats()),
                    QuantilesBackend::TDigest => a2.nodes_td.as_mut().and_then(|o| o.stats()),
                };
                if let (Some(s1), Some(s2)) = (n1, n2) {
                    delta_n50 = Some(s2.p50 - s1.p50);
                    delta_n95 = Some(s2.p95 - s1.p95);
                }
                // gap2_no_mate median delta (respect backend)
                let g1 = match qbackend {
                    QuantilesBackend::Exact => compute_stats_exact(&a1.gaps_no_mate),
                    QuantilesBackend::P2 => a1.gaps_nm_p2.as_ref().and_then(|o| o.stats()),
                    QuantilesBackend::TDigest => a1.gaps_nm_td.as_mut().and_then(|o| o.stats()),
                };
                let g2 = match qbackend {
                    QuantilesBackend::Exact => compute_stats_exact(&a2.gaps_no_mate),
                    QuantilesBackend::P2 => a2.gaps_nm_p2.as_ref().and_then(|o| o.stats()),
                    QuantilesBackend::TDigest => a2.gaps_nm_td.as_mut().and_then(|o| o.stats()),
                };
                if let (Some(s1), Some(s2)) = (g1, g2) {
                    delta_gap50 = Some(s2.p50 - s1.p50);
                }
                if !a1.tt_rates.is_empty() && !a2.tt_rates.is_empty() {
                    delta_tt_mean = Some(
                        a2.tt_rates.iter().sum::<f64>() / a2.tt_rates.len() as f64
                            - a1.tt_rates.iter().sum::<f64>() / a1.tt_rates.len() as f64,
                    );
                }
                if a1.total > 0 && a2.total > 0 {
                    delta_null_rate = Some(
                        a2.used_null_sum as f64 / a2.total as f64
                            - a1.used_null_sum as f64 / a1.total as f64,
                    );
                }
                delta_lmr_mean = Some(
                    a2.lmr_sum as f64 / (a2.total as f64).max(1.0)
                        - a1.lmr_sum as f64 / (a1.total as f64).max(1.0),
                );
            }
            if let Some(th) = g.delta_exact_both_min {
                if let Some(v) = delta_eb {
                    let ok = v >= th;
                    println!(
                        "GATE delta_exact_both_min {:+.3} >= {:+.3}: {}",
                        v,
                        th,
                        if ok { "PASS" } else { "FAIL" }
                    );
                    if ok {
                        pass_cnt += 1;
                    } else {
                        fail_cnt += 1;
                    }
                    failed |= !ok;
                }
            }
            if let Some(th) = g.delta_gap_no_mate_median_min {
                if let Some(v) = delta_gap50 {
                    let ok = v >= th;
                    println!(
                        "GATE delta_gap_no_mate_median_min {:+} >= {:+}: {}",
                        v,
                        th,
                        if ok { "PASS" } else { "FAIL" }
                    );
                    if ok {
                        pass_cnt += 1;
                    } else {
                        fail_cnt += 1;
                    }
                    failed |= !ok;
                }
            }
            if let Some(th) = g.delta_time_ms_median_max {
                if let Some(v) = delta_t50 {
                    let ok = v <= th;
                    println!(
                        "GATE delta_time_ms_median_max {:+} <= {:+}: {}",
                        v,
                        th,
                        if ok { "PASS" } else { "FAIL" }
                    );
                    if ok {
                        pass_cnt += 1;
                    } else {
                        fail_cnt += 1;
                    }
                    failed |= !ok;
                }
            }
            if let Some(th) = g.delta_time_ms_p95_max {
                if let Some(v) = delta_t95 {
                    let ok = v <= th;
                    println!(
                        "GATE delta_time_ms_p95_max {:+} <= {:+}: {}",
                        v,
                        th,
                        if ok { "PASS" } else { "FAIL" }
                    );
                    if ok {
                        pass_cnt += 1;
                    } else {
                        fail_cnt += 1;
                    }
                    failed |= !ok;
                }
            }
            if let Some(th) = g.delta_nodes_median_max {
                if let Some(v) = delta_n50 {
                    let ok = v <= th;
                    println!(
                        "GATE delta_nodes_median_max {:+} <= {:+}: {}",
                        v,
                        th,
                        if ok { "PASS" } else { "FAIL" }
                    );
                    if ok {
                        pass_cnt += 1;
                    } else {
                        fail_cnt += 1;
                    }
                    failed |= !ok;
                }
            }
            if let Some(th) = g.delta_nodes_p95_max {
                if let Some(v) = delta_n95 {
                    let ok = v <= th;
                    println!(
                        "GATE delta_nodes_p95_max {:+} <= {:+}: {}",
                        v,
                        th,
                        if ok { "PASS" } else { "FAIL" }
                    );
                    if ok {
                        pass_cnt += 1;
                    } else {
                        fail_cnt += 1;
                    }
                    failed |= !ok;
                }
            }
            if let Some(th) = g.delta_tt_hit_rate_mean_min {
                if let Some(v) = delta_tt_mean {
                    let ok = v >= th;
                    println!(
                        "GATE delta_tt_hit_rate_mean_min {:+.3} >= {:+.3}: {}",
                        v,
                        th,
                        if ok { "PASS" } else { "FAIL" }
                    );
                    if ok {
                        pass_cnt += 1;
                    } else {
                        fail_cnt += 1;
                    }
                    failed |= !ok;
                }
            }
            if let Some(th) = g.delta_null_rate_max {
                if let Some(v) = delta_null_rate {
                    let ok = v <= th;
                    println!(
                        "GATE delta_null_rate_max {:+.3} <= {:+.3}: {}",
                        v,
                        th,
                        if ok { "PASS" } else { "FAIL" }
                    );
                    if ok {
                        pass_cnt += 1;
                    } else {
                        fail_cnt += 1;
                    }
                    failed |= !ok;
                }
            }
            if let Some(th) = g.delta_lmr_mean_max {
                if let Some(v) = delta_lmr_mean {
                    let ok = v <= th;
                    println!(
                        "GATE delta_lmr_mean_max {:+.2} <= {:+.2}: {}",
                        v,
                        th,
                        if ok { "PASS" } else { "FAIL" }
                    );
                    if ok {
                        pass_cnt += 1;
                    } else {
                        fail_cnt += 1;
                    }
                    failed |= !ok;
                }
            }

            // seldepth deficit gates
            if let Some(th) = g.seldepth_deficit_median_max {
                let v = seldef_stats.map(|s| s.p50).unwrap_or(0);
                let ok = v <= th;
                println!(
                    "GATE seldepth_deficit_median_max {} <= {}: {}",
                    v,
                    th,
                    if ok { "PASS" } else { "FAIL" }
                );
                if ok {
                    pass_cnt += 1;
                } else {
                    fail_cnt += 1;
                }
                failed |= !ok;
            }
            if let Some(th) = g.seldepth_deficit_p90_max {
                let v = seldef_stats.map(|s| s.p90).unwrap_or(0);
                let ok = v <= th;
                println!(
                    "GATE seldepth_deficit_p90_max {} <= {}: {}",
                    v,
                    th,
                    if ok { "PASS" } else { "FAIL" }
                );
                if ok {
                    pass_cnt += 1;
                } else {
                    fail_cnt += 1;
                }
                failed |= !ok;
            }

            println!("GATE SUMMARY: PASS {} / FAIL {}", pass_cnt, fail_cnt);

            if failed && gate_mode_fail {
                std::process::exit(1);
            }
        }
    }

    Ok(())
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum QuantilesBackend {
    Exact,
    P2,
    TDigest,
}

// Note: parsing moved to clap ValueEnum `QuantilesBackendArg` + `parse_quantiles_backend`.
