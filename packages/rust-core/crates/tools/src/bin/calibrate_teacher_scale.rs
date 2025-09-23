use anyhow::{anyhow, Context, Result};
use clap::{Parser, ValueEnum};
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

    /// Output JSON with {mu, scale, method, samples, used_labels}
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
    vals.sort_by(|a, b| a.partial_cmp(b).unwrap());
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

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let cli = Cli::parse();
    if cli.inputs.is_empty() {
        return Err(anyhow!("--in is required"));
    }

    let mut cps: Vec<f32> = Vec::new();
    let mut labels: Vec<f32> = Vec::new();
    let mut used_labels = false;

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
            if let Some(cp) = extract_cp(&v) {
                let cp_f = cp as f32;
                if let Some(y) = extract_label(&v, &cli.label_field) {
                    if y.is_finite() {
                        cps.push(cp_f);
                        labels.push(y);
                    }
                } else {
                    cps.push(cp_f);
                }
            }
            if cps.len() >= cli.max_samples {
                break;
            }
        }
        if cps.len() >= cli.max_samples {
            break;
        }
    }

    if cps.is_empty() {
        return Err(anyhow!("no usable samples"));
    }

    let mu: f32;
    let scale: f32;
    if labels.len() >= 100 {
        used_labels = true;
        // coarse grid search for (mu, scale)
        let mut cps_copy = cps.clone();
        let med = median(&mut cps_copy);
        let mu_min = med - 300.0;
        let mu_max = med + 300.0;
        let mut best = (f32::INFINITY, 0.0, 0.0);
        let mut try_pair = |mu_c: f32, s_c: f32| {
            let mut sse = 0.0f64;
            for (i, &cp) in cps.iter().enumerate().take(labels.len()) {
                let p = logistic(cp, mu_c, s_c) as f64;
                let y = labels[i] as f64;
                let d = p - y;
                sse += d * d;
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
                try_pair(m, s);
                s += s_step;
            }
            m += mu_step;
        }
        mu = best.1;
        scale = best.2;
    } else {
        // fallback: robust defaults
        let mut cps_copy = cps.clone();
        mu = median(&mut cps_copy);
        scale = 620.0;
    }

    // emit JSON
    let out = serde_json::json!({
        "mu": mu,
        "scale": scale,
        "objective": format!("{:?}", cli.objective).to_lowercase(),
        "samples": cps.len(),
        "used_labels": used_labels,
    });
    let mut w: Box<dyn Write> = if cli.out.ends_with(".gz") || cli.out.ends_with(".zst") {
        // simple: write plain even if extension suggests compressed, to avoid extra deps here
        Box::new(File::create(&cli.out).with_context(|| format!("create {}", &cli.out))?)
    } else {
        Box::new(File::create(&cli.out).with_context(|| format!("create {}", &cli.out))?)
    };
    serde_json::to_writer_pretty(&mut w, &out)?;
    w.write_all(b"\n")?;
    Ok(())
}
