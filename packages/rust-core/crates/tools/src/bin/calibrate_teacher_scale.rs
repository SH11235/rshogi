use anyhow::{anyhow, Context, Result};
use clap::{ArgAction, Parser, ValueEnum};
use serde_json::Value as JsonValue;
use std::fs::File;
use std::io::{BufRead, Write};
use tools::io_detect::open_maybe_compressed_reader;

#[derive(Copy, Clone, Debug, ValueEnum)]
enum ObjectiveKind {
    #[clap(name = "mse")]
    Mse,
}

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Calibrate logistic parameters (mu, S) mapping cp -> WDL probability"
)]
struct Cli {
    /// Input JSONL files (compressed .gz/.zst supported)
    #[arg(long = "in", value_name = "FILE", num_args = 1..)]
    inputs: Vec<String>,

    /// Output JSON with {mu, scale, objective, samples, used_labels}
    #[arg(long)]
    out: String,

    /// Field name for WDL label in [0,1] (optional)
    #[arg(long, default_value = "label")]
    label_field: String,

    /// Objective to minimize when labels are available
    #[arg(long, value_enum, default_value_t = ObjectiveKind::Mse)]
    objective: ObjectiveKind,

    /// Max number of samples to use
    #[arg(long, default_value_t = 100_000usize)]
    max_samples: usize,

    /// Minimum labeled samples required to fit parameters (fallback otherwise)
    #[arg(long, default_value_t = 100usize)]
    min_labeled: usize,

    /// JSON field containing per-sample weight (default: teacher_weight)
    #[arg(long, default_value = "teacher_weight")]
    weight_field: String,

    /// Ignore teacher_weight field
    #[arg(long, action = ArgAction::SetTrue)]
    no_weight: bool,
}

fn extract_cp(obj: &JsonValue) -> Option<i32> {
    // teacher_score(type=cp) -> teacher_cp -> eval -> lines[0].score_cp
    if let Some(ts) = obj.get("teacher_score").and_then(|v| v.as_object()) {
        let kind = ts.get("type").and_then(|v| v.as_str());
        let val = ts.get("value").and_then(|v| v.as_i64());
        if kind == Some("cp") {
            return val.map(|x| x as i32);
        }
    }
    if let Some(v) = obj.get("teacher_cp").and_then(|v| v.as_i64()) {
        return Some(v as i32);
    }
    if let Some(v) = obj.get("eval").and_then(|v| v.as_i64()) {
        return Some(v as i32);
    }
    if let Some(lines) = obj.get("lines").and_then(|v| v.as_array()) {
        if let Some(first) = lines.first() {
            if let Some(v) = first.get("score_cp").and_then(|v| v.as_i64()) {
                return Some(v as i32);
            }
        }
    }
    None
}

fn extract_label(obj: &JsonValue, field: &str) -> Option<f32> {
    obj.get(field)
        .and_then(|v| v.as_f64())
        .map(|x| x as f32)
        .filter(|p| *p >= 0.0 && *p <= 1.0)
}

fn logistic(cp: f32, mu: f32, scale: f32) -> f32 {
    let x = ((cp - mu) / scale).clamp(-16.0, 16.0);
    1.0 / (1.0 + (-x).exp())
}

fn median(vals: &mut [f32]) -> f32 {
    vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = vals.len();
    if n == 0 {
        return 0.0;
    }
    if n % 2 == 1 {
        vals[n / 2]
    } else {
        0.5 * (vals[n / 2 - 1] + vals[n / 2])
    }
}

fn open_writer(path: &str) -> Result<Box<dyn Write>> {
    if path.ends_with(".gz") {
        let f = File::create(path).with_context(|| format!("create {}", path))?;
        let enc = flate2::write::GzEncoder::new(f, flate2::Compression::default());
        Ok(Box::new(enc))
    } else if path.ends_with(".zst") {
        #[cfg(feature = "zstd")]
        {
            let f = File::create(path).with_context(|| format!("create {}", path))?;
            let enc = zstd::Encoder::new(f, 0)?;
            Ok(Box::new(enc.auto_finish()))
        }
        #[cfg(not(feature = "zstd"))]
        {
            Err(anyhow!(
                "output path ends with .zst, but this binary was built without 'zstd' feature"
            ))
        }
    } else {
        let f = File::create(path).with_context(|| format!("create {}", path))?;
        Ok(Box::new(std::io::BufWriter::new(f)))
    }
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let cli = Cli::parse();
    if cli.inputs.is_empty() {
        return Err(anyhow!("--in is required"));
    }

    let mut pairs: Vec<(f32, f32, f32)> = Vec::new(); // (cp, label, weight)
    let mut cp_all: Vec<f32> = Vec::new();
    let mut used_labels = false;

    let mut weight_used_any = false;

    for path in &cli.inputs {
        let mut r =
            open_maybe_compressed_reader(path, 4 * 1024 * 1024).map_err(|e| anyhow!("{}", e))?;
        let mut line = String::new();
        loop {
            line.clear();
            if r.read_line(&mut line)? == 0 {
                break;
            }
            if line.trim().is_empty() {
                continue;
            }
            let v: JsonValue = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let Some(cp_i32) = extract_cp(&v) else {
                continue;
            };
            let cp_f = cp_i32 as f32;

            let valid = v.get("teacher_valid").and_then(|b| b.as_bool()).unwrap_or(true);
            let bound_ok = match v.get("teacher_bound").and_then(|b| b.as_str()) {
                Some(b) if b.eq_ignore_ascii_case("exact") || b.eq_ignore_ascii_case("unknown") => {
                    true
                }
                None => true,
                Some(_) => false,
            };

            if valid {
                cp_all.push(cp_f);
            }

            if !valid || !bound_ok {
                continue;
            }

            if let Some(y) = extract_label(&v, &cli.label_field) {
                if y.is_finite() {
                    let mut weight = if cli.no_weight || cli.weight_field.is_empty() {
                        1.0
                    } else {
                        v.get(&cli.weight_field)
                            .and_then(|w| w.as_f64())
                            .map(|w| w as f32)
                            .filter(|w| w.is_finite() && *w > 0.0)
                            .unwrap_or(1.0)
                    };
                    if !weight.is_finite() || weight <= 0.0 {
                        weight = 1.0;
                    }
                    if (weight - 1.0).abs() > f32::EPSILON {
                        weight_used_any = true;
                    }
                    pairs.push((cp_f, y, weight));
                }
            }

            if pairs.len() + cp_all.len() >= cli.max_samples {
                break;
            }
        }
        if pairs.len() + cp_all.len() >= cli.max_samples {
            break;
        }
    }

    if pairs.is_empty() && cp_all.is_empty() {
        return Err(anyhow!("no usable samples"));
    }

    let mu: f32;
    let scale: f32;
    if pairs.len() >= cli.min_labeled {
        used_labels = true;
        // coarse grid search for (mu, scale)
        let mut cps_copy: Vec<f32> = pairs.iter().map(|(cp, _, _)| *cp).collect();
        let med = median(&mut cps_copy);
        let mu_min = med - 300.0;
        let mu_max = med + 300.0;
        let mut best = (f32::INFINITY, 0.0, 0.0);
        let mut try_pair = |mu_c: f32, s_c: f32| {
            let mut sse = 0.0f64;
            let mut w_sum = 0.0f64;
            for &(cp, y, w) in &pairs {
                let p = logistic(cp, mu_c, s_c) as f64;
                let d = p - y as f64;
                let ww = w as f64;
                sse += ww * d * d;
                w_sum += ww;
            }
            if w_sum > 0.0 {
                sse /= w_sum;
            }
            if sse < best.0 as f64 {
                best = (sse as f32, mu_c, s_c);
            }
        };
        let mu_step = 20.0;
        let s_min = 300.0f32;
        let s_max = 1000.0f32;
        let s_step = 20.0f32;
        let mut m = mu_min;
        while m <= mu_max {
            let mut s = s_min;
            while s <= s_max {
                if s > 1.0 {
                    try_pair(m, s);
                }
                s += s_step;
            }
            m += mu_step;
        }
        // fine search around best
        let mut fine_best = best;
        let mu_lo = best.1 - 40.0;
        let mu_hi = best.1 + 40.0;
        let s_lo = (best.2 - 80.0).max(10.0);
        let s_hi = best.2 + 80.0;
        let mut m = mu_lo;
        while m <= mu_hi {
            let mut s = s_lo;
            while s <= s_hi {
                if s > 1.0 {
                    let mut sse = 0.0f64;
                    let mut w_sum = 0.0f64;
                    for &(cp, y, w) in &pairs {
                        let p = logistic(cp, m, s) as f64;
                        let d = p - y as f64;
                        let ww = w as f64;
                        sse += ww * d * d;
                        w_sum += ww;
                    }
                    if w_sum > 0.0 {
                        sse /= w_sum;
                    }
                    if sse < fine_best.0 as f64 {
                        fine_best = (sse as f32, m, s);
                    }
                }
                s += 5.0;
            }
            m += 5.0;
        }
        best = fine_best;
        mu = best.1;
        scale = best.2;
    } else {
        // fallback: robust defaults
        let mut cp_all = cp_all;
        if cp_all.is_empty() {
            cp_all = pairs.iter().map(|(cp, _, _)| *cp).collect();
        }
        mu = median(&mut cp_all);
        scale = 620.0;
    }

    // emit JSON
    let out = serde_json::json!({
        "mu": mu,
        "scale": scale,
        "objective": format!("{:?}", cli.objective).to_lowercase(),
        "samples": pairs.len(),
        "used_labels": used_labels,
        "min_labeled": cli.min_labeled,
        "weights_used": weight_used_any && !cli.no_weight && !cli.weight_field.is_empty(),
    });
    let mut w = open_writer(&cli.out)?;
    serde_json::to_writer_pretty(&mut w, &out)?;
    w.write_all(b"\n")?;
    Ok(())
}
