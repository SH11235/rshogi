use std::cmp::Ordering;
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

#[cfg(feature = "zstd")]
use zstd::stream::read::Decoder as ZstdDecoder;

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

fn exactness_from_bounds(v: &Value) -> Exactness {
    // Prefer explicit bound1/bound2 when present
    let b1 = v.get("bound1").and_then(|x| x.as_str());
    let b2 = v.get("bound2").and_then(|x| x.as_str());
    match (b1 == Some("Exact"), b2 == Some("Exact")) {
        (true, true) => return Exactness::Both,
        (true, false) => return Exactness::Top1,
        _ => {}
    }
    // Fallback: inspect first two lines[].bound
    if let Some(lines) = v.get("lines").and_then(|x| x.as_array()) {
        let b = |i: usize| {
            lines
                .get(i)
                .and_then(|l| l.get("bound"))
                .and_then(|x| x.as_str())
        };
        match (b(0) == Some("Exact"), b(1) == Some("Exact")) {
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
    Some(Rec { v, sfen, depth, seldepth, exact, nodes, time_ms, file_idx, line_idx })
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
    b.file_idx
        .cmp(&a.file_idx)
        .then(b.line_idx.cmp(&a.line_idx))
}

fn open_reader<P: AsRef<Path>>(path: P) -> io::Result<Box<dyn BufRead>> {
    let p = path.as_ref();
    if p.to_string_lossy() == "-" {
        return Ok(Box::new(BufReader::new(io::stdin())));
    }
    let f = File::open(p)?;
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    if ext == "gz" {
        let dec = flate2::read::GzDecoder::new(f);
        return Ok(Box::new(BufReader::new(dec)));
    }
    #[cfg(feature = "zstd")]
    if ext == "zst" {
        let dec = ZstdDecoder::new(f)?;
        return Ok(Box::new(BufReader::new(dec)));
    }
    Ok(Box::new(BufReader::new(f)))
}

fn write_aggregated_manifest(
    output_path: &Path,
    inputs: &[String],
    mode: Mode,
    total_read: usize,
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
        "total_positions": total_read,
        "deduplicated_positions": output_items.map(|v| v.len()).unwrap_or(total_read),
        "config": {
            "multipv": unique_or_varies_i64(&multipv_vals),
            "teacher_profile": unique_or_varies_str(&teacher_vals),
            "hash_mb": unique_or_varies_i64(&hash_vals),
        },
        "generated_at_range": {
            "min": gen_ats.iter().min().cloned(),
            "max": gen_ats.iter().max().cloned(),
        }
    });

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

    if let Some(dir) = output_path.parent() {
        let out = dir.join("manifest.json");
        let _ = std::fs::write(out, serde_json::to_string_pretty(&manifest).unwrap_or_default());
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!(
            "Usage: {} <input1.jsonl> <...> <output.jsonl> [--dedup-by-sfen] [--prefer-deeper]",
            args[0]
        );
        std::process::exit(1);
    }

    let mut input_paths = Vec::new();
    let mut dedup_by_sfen = false;
    let mut prefer_deeper = false;

    // Collect until we hit an option; last non-option is output
    let mut i = 1;
    while i < args.len() && !args[i].starts_with('-') {
        input_paths.push(args[i].clone());
        i += 1;
    }
    if input_paths.len() < 2 {
        eprintln!("Need at least one input and one output");
        std::process::exit(1);
    }
    let output_path = PathBuf::from(input_paths.pop().unwrap());

    while i < args.len() {
        match args[i].as_str() {
            "--dedup-by-sfen" => {
                dedup_by_sfen = true;
                i += 1;
            }
            "--prefer-deeper" => {
                prefer_deeper = true;
                i += 1;
            }
            other => {
                eprintln!("Unknown option: {}", other);
                std::process::exit(1);
            }
        }
    }

    let mode = if prefer_deeper { Mode::DepthFirst } else { Mode::ExactFirst };

    // Prepare output
    let mut out = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&output_path)?;

    let mut total_read: usize = 0;

    if !dedup_by_sfen {
        // Non-dedup: stream-concatenate inputs preserving order. Validate JSON minimally.
        for (file_idx, path) in input_paths.iter().enumerate() {
            let reader = open_reader(path)?;
            for (line_idx, line) in reader.lines().enumerate() {
                match line {
                    Ok(l) => {
                        if l.trim().is_empty() {
                            continue;
                        }
                        total_read += 1;
                        match serde_json::from_str::<Value>(&l) {
                            Ok(_) => {
                                writeln!(out, "{}", l)?;
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
        write_aggregated_manifest(&output_path, &input_paths, mode, total_read, None);
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
            total_read += 1;
            let v: Value = match serde_json::from_str(&line) {
                Ok(v) => v,
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
    }

    write_aggregated_manifest(&output_path, &input_paths, mode, total_read, Some(&items));

    Ok(())
}
