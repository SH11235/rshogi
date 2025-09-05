use chrono::{DateTime, FixedOffset, Utc};
use clap::{Parser, ValueEnum};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use tools::common::io::{open_reader, open_writer, Writer};

/// Degree of exactness for tie-breaking.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Debug)]
enum Exactness {
    None = 0,
    Top1 = 1,
    Both = 2,
}

#[derive(Copy, Clone)]
enum Mode {
    ExactFirst,
    DepthFirst,
}

#[derive(Clone)]
struct Rec {
    v: Value,
    sfen: String,
    depth: i64,
    seldepth: i64,
    exact: Exactness,
    nodes: i64,
    time_ms: i64,
    file_idx: usize,
    line_idx: usize,
}

fn as_i64(v: &Value) -> i64 {
    if let Some(i) = v.as_i64() {
        i
    } else if let Some(u) = v.as_u64() {
        i64::try_from(u).unwrap_or(i64::MAX)
    } else if let Some(f) = v.as_f64() {
        f as i64
    } else {
        0
    }
}

fn is_exact_str(s: &str) -> bool {
    s.eq_ignore_ascii_case("exact")
}

fn exactness_from_bounds(v: &Value) -> Exactness {
    // Prefer explicit bound1/bound2 when present
    let b1 = v.get("bound1").and_then(|x| x.as_str());
    let b2 = v.get("bound2").and_then(|x| x.as_str());
    match (b1.map(is_exact_str).unwrap_or(false), b2.map(is_exact_str).unwrap_or(false)) {
        (true, true) => return Exactness::Both,
        (true, false) => return Exactness::Top1,
        _ => {}
    }
    // Fallback: inspect first two lines[].bound
    if let Some(lines) = v.get("lines").and_then(|x| x.as_array()) {
        let b = |i: usize| lines.get(i).and_then(|l| l.get("bound")).and_then(|x| x.as_str());
        match (b(0).map(is_exact_str).unwrap_or(false), b(1).map(is_exact_str).unwrap_or(false)) {
            (true, true) => Exactness::Both,
            (true, false) => Exactness::Top1,
            _ => Exactness::None,
        }
    } else {
        Exactness::None
    }
}

fn parse_rec(v: Value, file_idx: usize, line_idx: usize) -> Option<Rec> {
    let sfen = v.get("sfen")?.as_str()?.to_owned();
    let depth = v.get("depth").map(as_i64).unwrap_or(0);
    let seldepth = v.get("seldepth").map(as_i64).unwrap_or(0);
    let nodes = v.get("nodes").map(as_i64).unwrap_or(0);
    let time_ms = v.get("time_ms").map(as_i64).unwrap_or(0);
    let exact = exactness_from_bounds(&v);
    Some(Rec {
        v,
        sfen,
        depth,
        seldepth,
        exact,
        nodes,
        time_ms,
        file_idx,
        line_idx,
    })
}

/// Return Ordering::Greater when `a` is better than `b`.
fn cmp_rec(a: &Rec, b: &Rec, mode: Mode) -> Ordering {
    let ord = match mode {
        Mode::ExactFirst => a
            .exact
            .cmp(&b.exact)
            .then(a.depth.cmp(&b.depth))
            .then(a.seldepth.cmp(&b.seldepth))
            .then(a.nodes.cmp(&b.nodes))
            .then(a.time_ms.cmp(&b.time_ms)),
        Mode::DepthFirst => a
            .depth
            .cmp(&b.depth)
            .then(a.seldepth.cmp(&b.seldepth))
            .then(a.exact.cmp(&b.exact))
            .then(a.nodes.cmp(&b.nodes))
            .then(a.time_ms.cmp(&b.time_ms)),
    };
    if ord != Ordering::Equal {
        return ord;
    }
    // Stabilize: earlier (smaller file/line idx) wins on ties
    b.file_idx.cmp(&a.file_idx).then(b.line_idx.cmp(&a.line_idx))
}

#[derive(Clone, Copy)]
struct LineCounts {
    read_lines: usize,
    valid_json_lines: usize,
    written_lines: usize,
}

fn write_aggregated_manifest(
    output_path: &Path,
    manifest_out: Option<&Path>,
    inputs: &[String],
    mode: Mode,
    counts: LineCounts,
    output_items: Option<&[Rec]>,
) {
    // Build sources info and collect config fields
    let mut sources = Vec::new();
    let mut multipv_vals: Vec<i64> = Vec::new();
    let mut teacher_vals: Vec<String> = Vec::new();
    let mut hash_vals: Vec<i64> = Vec::new();
    let mut gen_ats: Vec<String> = Vec::new();

    for p in inputs {
        let path = PathBuf::from(p);
        let mut src = json!({ "path": p });
        if let Some(dir) = path.parent() {
            let cand = dir.join("manifest.json");
            if cand.exists() {
                if let Ok(s) = std::fs::read_to_string(&cand) {
                    if let Ok(v) = serde_json::from_str::<Value>(&s) {
                        // extract configs
                        if let Some(mv) = v.get("multipv").and_then(|x| x.as_i64()) {
                            multipv_vals.push(mv);
                        }
                        if let Some(tp) = v.get("teacher_profile").and_then(|x| x.as_str()) {
                            teacher_vals.push(tp.to_string());
                        }
                        if let Some(hm) = v.get("hash_mb").and_then(|x| x.as_i64()) {
                            hash_vals.push(hm);
                        }
                        if let Some(ts) = v.get("generated_at").and_then(|x| x.as_str()) {
                            gen_ats.push(ts.to_string());
                        }
                        src["manifest"] = v;
                    }
                }
            }
        }
        sources.push(src);
    }

    // Config consistency helpers
    fn unique_or_varies_i64(vs: &[i64]) -> Value {
        if vs.is_empty() {
            Value::Null
        } else if vs.iter().all(|&x| x == vs[0]) {
            Value::from(vs[0])
        } else {
            Value::from("varies")
        }
    }
    fn unique_or_varies_str(vs: &[String]) -> Value {
        if vs.is_empty() {
            Value::Null
        } else if vs.iter().all(|x| x == &vs[0]) {
            Value::from(vs[0].clone())
        } else {
            Value::from("varies")
        }
    }

    // Stats from output items if provided
    let mut agg_obj = json!({
        "read_lines": counts.read_lines,
        "valid_json_lines": counts.valid_json_lines,
        "written_lines": counts.written_lines,
        "total_positions": counts.read_lines, // kept for backward compatibility
        "deduplicated_positions": output_items.map(|v| v.len()).unwrap_or(counts.written_lines),
        "config": {
            "multipv": unique_or_varies_i64(&multipv_vals),
            "teacher_profile": unique_or_varies_str(&teacher_vals),
            "hash_mb": unique_or_varies_i64(&hash_vals),
        },
        "generated_at_range": Value::Null,
    });

    // Safe min/max for timestamps with timezone handling; fallback to string compare
    let mut parsed: Vec<DateTime<Utc>> = Vec::new();
    for ts in &gen_ats {
        if let Ok(dt) = ts.parse::<DateTime<FixedOffset>>() {
            parsed.push(dt.with_timezone(&Utc));
        } else if let Ok(dt) = ts.parse::<DateTime<Utc>>() {
            parsed.push(dt);
        }
    }
    let gen_range_val = if !parsed.is_empty() {
        json!({
            "min": parsed.iter().min().map(|d| d.to_rfc3339()),
            "max": parsed.iter().max().map(|d| d.to_rfc3339()),
        })
    } else {
        json!({
            "min": gen_ats.iter().min().cloned(),
            "max": gen_ats.iter().max().cloned(),
        })
    };
    agg_obj["generated_at_range"] = gen_range_val;

    if let Some(items) = output_items {
        if !items.is_empty() {
            let mut depth_min = i64::MAX;
            let mut depth_max = i64::MIN;
            let mut sel_min = i64::MAX;
            let mut sel_max = i64::MIN;
            let mut nodes_min = i64::MAX;
            let mut nodes_max = i64::MIN;
            let mut time_min = i64::MAX;
            let mut time_max = i64::MIN;
            let mut depth_sum: f64 = 0.0;
            let mut sel_sum: f64 = 0.0;
            let mut nodes_sum: f64 = 0.0;
            let mut time_sum: f64 = 0.0;
            let n = items.len() as f64;
            for r in items {
                depth_min = depth_min.min(r.depth);
                depth_max = depth_max.max(r.depth);
                sel_min = sel_min.min(r.seldepth);
                sel_max = sel_max.max(r.seldepth);
                nodes_min = nodes_min.min(r.nodes);
                nodes_max = nodes_max.max(r.nodes);
                time_min = time_min.min(r.time_ms);
                time_max = time_max.max(r.time_ms);
                depth_sum += r.depth as f64;
                sel_sum += r.seldepth as f64;
                nodes_sum += r.nodes as f64;
                time_sum += r.time_ms as f64;
            }
            agg_obj["stats"] = json!({
                "depth": {"min": depth_min, "max": depth_max, "avg": depth_sum / n},
                "seldepth": {"min": sel_min, "max": sel_max, "avg": sel_sum / n},
                "nodes": {"min": nodes_min, "max": nodes_max, "avg": nodes_sum / n},
                "time_ms": {"min": time_min, "max": time_max, "avg": time_sum / n},
            });
        }
    }

    let manifest = json!({
        "tool": "merge_annotation_results",
        "mode": match mode { Mode::ExactFirst => "exact-first", Mode::DepthFirst => "depth-first" },
        "inputs": inputs,
        "sources": sources,
        "aggregated": agg_obj,
    });

    // Determine manifest destination
    let manifest_path = if let Some(mo) = manifest_out {
        Some(mo.to_path_buf())
    } else {
        output_path.parent().map(|dir| dir.join("manifest.json"))
    };

    if let Some(path) = manifest_path {
        match std::fs::write(&path, serde_json::to_string_pretty(&manifest).unwrap_or_default()) {
            Ok(_) => {}
            Err(e) => eprintln!("[warn] failed to write manifest at {}: {}", path.display(), e),
        }
    } else {
        eprintln!("[warn] manifest output is not specified (use --manifest-out) and output is STDOUT; skipping manifest write");
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[derive(Clone, Copy, Debug, ValueEnum)]
    enum ModeArg {
        #[value(name = "exact-first")]
        ExactFirst,
        #[value(name = "depth-first")]
        DepthFirst,
    }

    #[derive(Parser, Debug)]
    #[command(
        name = "merge_annotation_results",
        about = "Merge JSONL annotations with optional deduplication.",
        disable_help_subcommand = true,
        after_help = "Notes: --mode and --prefer-deeper are mutually exclusive.\n--prefer-deeper is an alias of '--mode depth-first'.\nUse '-' for STDIN/STDOUT and '--manifest-out <PATH>' to save manifest when output is '-'."
    )]
    struct Cli {
        /// Input JSONL files (use '-' for STDIN); last positional is output
        #[arg(value_name="INPUTS...", required=true, num_args=2..)]
        inputs: Vec<String>,
        /// Deduplicate by SFEN
        #[arg(long)]
        dedup_by_sfen: bool,
        /// Prefer deeper search (alias of --mode depth-first). Not compatible with --mode.
        #[arg(long, conflicts_with = "mode")]
        prefer_deeper: bool,
        /// Merge mode: exact-first | depth-first
        #[arg(long, value_enum, conflicts_with = "prefer_deeper")]
        mode: Option<ModeArg>,
        /// Manifest output path (required when output is '-')
        #[arg(long, value_name = "PATH")]
        manifest_out: Option<String>,
    }

    let cli = Cli::parse();
    if cli.inputs.len() < 2 {
        eprintln!("Need at least one input and one output");
        std::process::exit(1);
    }
    let (output_raw, rest_inputs) = {
        let (last, rest) = cli.inputs.split_last().unwrap();
        (last.clone(), rest.to_vec())
    };
    let input_paths = rest_inputs;
    let output_path = PathBuf::from(&output_raw);

    let mode = match cli.mode {
        Some(ModeArg::ExactFirst) => Mode::ExactFirst,
        Some(ModeArg::DepthFirst) => Mode::DepthFirst,
        None => {
            if cli.prefer_deeper {
                Mode::DepthFirst
            } else {
                Mode::ExactFirst
            }
        }
    };

    // Prepare output (support STDOUT and compressed extensions). Use Writer to propagate finish errors.
    let mut out: Writer = open_writer(&output_path)?;

    let mut read_lines: usize = 0; // non-empty lines encountered
    let mut valid_json_lines: usize = 0; // successfully parsed JSON lines
    let mut written_lines: usize = 0; // lines written to output

    if !cli.dedup_by_sfen {
        // Non-dedup: stream-concatenate inputs preserving order. Validate JSON minimally.
        for (file_idx, path) in input_paths.iter().enumerate() {
            let reader = open_reader(path)?;
            for (line_idx, line) in reader.lines().enumerate() {
                match line {
                    Ok(l) => {
                        if l.trim().is_empty() {
                            continue;
                        }
                        read_lines += 1;
                        match serde_json::from_str::<Value>(&l) {
                            Ok(_) => {
                                valid_json_lines += 1;
                                writeln!(out, "{}", l)?;
                                written_lines += 1;
                            }
                            Err(e) => {
                                eprintln!(
                                    "[warn] json parse error at {}:{} -> {}",
                                    path,
                                    line_idx + 1,
                                    e
                                );
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("[warn] read error at {}:{} -> {}", path, line_idx + 1, e);
                    }
                }
            }
            // Avoid unused warnings
            let _ = file_idx;
        }
        // Finalize output stream and then write manifest
        out.close()?;
        let manifest_out = cli.manifest_out.as_ref().map(PathBuf::from);
        let counts = LineCounts {
            read_lines,
            valid_json_lines,
            written_lines,
        };
        write_aggregated_manifest(
            &output_path,
            manifest_out.as_deref(),
            &input_paths,
            mode,
            counts,
            None,
        );
        return Ok(());
    }

    // Dedup path: pick best per SFEN with stable tie-breakers.
    let mut best: HashMap<String, Rec> = HashMap::new();
    for (file_idx, path) in input_paths.iter().enumerate() {
        let reader = open_reader(path)?;
        for (line_idx, line) in reader.lines().enumerate() {
            let line = match line {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("[warn] read error at {}:{} -> {}", path, line_idx + 1, e);
                    continue;
                }
            };
            if line.trim().is_empty() {
                continue;
            }
            read_lines += 1;
            let v: Value = match serde_json::from_str(&line) {
                Ok(v) => {
                    valid_json_lines += 1;
                    v
                }
                Err(e) => {
                    eprintln!("[warn] json parse error at {}:{} -> {}", path, line_idx + 1, e);
                    continue;
                }
            };
            let Some(rec) = parse_rec(v, file_idx, line_idx) else {
                eprintln!("[warn] missing sfen at {}:{} -> skipped", path, line_idx + 1);
                continue;
            };
            best.entry(rec.sfen.clone())
                .and_modify(|prev| {
                    if cmp_rec(&rec, prev, mode) == Ordering::Greater {
                        *prev = Rec { ..rec.clone() };
                    }
                })
                .or_insert(rec);
        }
    }

    // Stable output order: sort by SFEN ascending
    let mut items: Vec<Rec> = best.into_values().collect();
    items.sort_unstable_by(|a, b| a.sfen.cmp(&b.sfen));
    for rec in &items {
        writeln!(out, "{}", rec.v)?;
        written_lines += 1;
    }

    // Finalize output stream and then write manifest
    out.close()?;
    let manifest_out = cli.manifest_out.as_ref().map(PathBuf::from);
    let counts = LineCounts {
        read_lines,
        valid_json_lines,
        written_lines,
    };
    write_aggregated_manifest(
        &output_path,
        manifest_out.as_deref(),
        &input_paths,
        mode,
        counts,
        Some(&items),
    );

    Ok(())
}
