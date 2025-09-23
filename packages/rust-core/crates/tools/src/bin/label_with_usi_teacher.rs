use anyhow::{anyhow, Context, Result};
use clap::{ArgAction, Parser, ValueEnum};
use serde_json::json;
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::File;
use std::fs::File as StdFile;
use std::io::{self, BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tools::io_detect::open_maybe_compressed_reader;

#[derive(Copy, Clone, Debug, ValueEnum)]
enum WriteMode {
    #[clap(name = "teacher_score")]
    TeacherScore,
    #[clap(name = "teacher_cp")]
    TeacherCp,
    #[clap(name = "eval")]
    Eval,
}

#[derive(Clone, Debug)]
struct WeightParams {
    depth_d0: f32,
    bound_upper: f32,
    bound_lower: f32,
    enabled: bool,
}

#[derive(Clone, Debug)]
struct ResourceParams {
    threads: u32,
    hash_mb: u32,
    multipv: u32,
}

#[derive(Clone, Debug)]
struct EngineInit {
    engine_path: PathBuf,
    engine_args: Vec<String>,
    nn_path: Option<PathBuf>,
    nn_option_name: Option<String>, // None => auto
    usi_init: Option<PathBuf>,
    resources: ResourceParams,
}

#[derive(Clone, Debug)]
struct JobSpec {
    raw_json: String,
}

#[derive(Clone, Debug)]
struct JobResult {
    output_jsonl: String,
    log_jsonl: Option<String>,
}

#[derive(Clone, Debug)]
struct QueryResult {
    valid: bool,
    method: String,
    score_kind: Option<String>, // "cp" | "mate"
    score_value: Option<i32>,
    bound: Option<String>, // exact|upper|lower
    depth: Option<i32>,
    seldepth: Option<i32>,
    nodes: Option<u64>,
    time_ms: Option<u64>,
    nps: Option<u64>,
    pv_head: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct InfoSnapshot {
    score_kind: Option<String>,
    score_value: Option<i32>,
    bound: Option<String>,
    depth: Option<i32>,
    seldepth: Option<i32>,
    nodes: Option<u64>,
    time_ms: Option<u64>,
    nps: Option<u64>,
    pv_head: Option<String>,
}

#[derive(Clone, Debug)]
enum MethodKind {
    Depth0,
    Nodes1,
    Movetime1,
}

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Label JSONL with external USI engine as a black-box teacher"
)]
struct Cli {
    /// Path to USI engine binary
    #[arg(long, value_name = "FILE")]
    engine: PathBuf,

    /// Additional args for USI engine (pass multiple values)
    #[arg(long, num_args = 1..)]
    engine_args: Option<Vec<String>>,

    /// Optional NN weights path (engine-specific). Will be set via option autodetect or --nn-option
    #[arg(long, value_name = "FILE")]
    nn: Option<PathBuf>,

    /// Explicit option name to pass the NN path to (e.g., EvalFile, NNUEFile). Default: auto-detect
    #[arg(long, value_name = "NAME")]
    nn_option: Option<String>,

    /// USI init script (one command per line)
    #[arg(long, value_name = "FILE")]
    usi_init: Option<PathBuf>,

    /// Input JSONL(s). Compression auto-detected (.gz/.zst)
    #[arg(long = "in", value_name = "FILE", num_args = 1..)]
    inputs: Vec<String>,

    /// Output JSONL. Compression decided by extension
    #[arg(long, value_name = "FILE")]
    out: String,

    /// Method order (comma-separated): depth0,nodes1,movetime1
    #[arg(long, default_value = "depth0,nodes1,movetime1")]
    method_order: String,

    /// Number of parallel engine workers
    #[arg(long)]
    jobs: Option<usize>,

    /// Per-query timeout in milliseconds
    #[arg(long, default_value_t = 300u64)]
    timeout_ms: u64,

    /// Retries on timeout/failure
    #[arg(long, default_value_t = 2u32)]
    retry: u32,

    /// Force Threads setoption value
    #[arg(long, default_value_t = 1u32)]
    threads: u32,

    /// Force Hash (MB) setoption value
    #[arg(long, default_value_t = 32u32)]
    hash_mb: u32,

    /// Force MultiPV setoption value
    #[arg(long, default_value_t = 1u32)]
    multipv: u32,

    /// JSON key for SFEN
    #[arg(long, default_value = "sfen")]
    sfen_field: String,

    /// Write mode: teacher_score|teacher_cp|eval
    #[arg(long, value_enum, default_value_t = WriteMode::TeacherScore)]
    write: WriteMode,

    /// Structured log JSONL path
    #[arg(long)]
    log: Option<String>,

    /// Resume file (processed counters). Not implemented yet, reserved.
    #[arg(long)]
    resume: Option<String>,

    /// Enable sample weighting; supply depth threshold and bound weights
    #[arg(long, default_value = "depth=8,upper=0.5,lower=0.5")]
    weight: String,

    /// Disable weighting
    #[arg(long, action = ArgAction::SetTrue)]
    no_weight: bool,

    /// CP clip value for mapping mate to cp (only when producing teacher_cp via --write)
    #[arg(long, default_value_t = 3000i32)]
    mate_cp_clip: i32,
}

fn parse_method_order(s: &str) -> Vec<MethodKind> {
    s.split(',')
        .filter_map(|t| match t.trim().to_ascii_lowercase().as_str() {
            "depth0" => Some(MethodKind::Depth0),
            "nodes1" => Some(MethodKind::Nodes1),
            "movetime1" => Some(MethodKind::Movetime1),
            _ => None,
        })
        .collect()
}

fn parse_weight(s: &str, enabled: bool) -> WeightParams {
    let mut params = WeightParams {
        depth_d0: 8.0,
        bound_upper: 0.5,
        bound_lower: 0.5,
        enabled,
    };
    for part in s.split(',') {
        let kv: Vec<&str> = part.split('=').collect();
        if kv.len() != 2 {
            continue;
        }
        let k = kv[0].trim().to_ascii_lowercase();
        let v = kv[1].trim();
        match k.as_str() {
            "depth" => {
                if let Ok(f) = v.parse::<f32>() {
                    if f > 0.0 {
                        params.depth_d0 = f;
                    }
                }
            }
            "upper" => {
                if let Ok(f) = v.parse::<f32>() {
                    params.bound_upper = f.clamp(0.0, 1.0);
                }
            }
            "lower" => {
                if let Ok(f) = v.parse::<f32>() {
                    params.bound_lower = f.clamp(0.0, 1.0);
                }
            }
            _ => {}
        }
    }
    params
}

fn physical_cores() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
}

fn open_maybe_compressed_writer(path: &str) -> Result<Box<dyn Write>> {
    if path.ends_with(".gz") {
        let f = File::create(path).with_context(|| format!("create {}", path))?;
        let enc = flate2::write::GzEncoder::new(f, flate2::Compression::default());
        Ok(Box::new(enc))
    } else if path.ends_with(".zst") {
        #[cfg(feature = "zstd")]
        {
            let f = File::create(path).with_context(|| format!("create {}", path))?;
            let enc = zstd::Encoder::new(f, 0)?; // 0 = default level
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
        Ok(Box::new(f))
    }
}

fn compute_sha256(path: &PathBuf) -> Result<String> {
    let mut f =
        StdFile::open(path).with_context(|| format!("compute_sha256: open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = std::io::Read::read(&mut f, &mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

struct UsiProc {
    child: Child,
    stdin: io::BufWriter<ChildStdin>,
    rx: Receiver<String>,
    opt_names: HashSet<String>,
    id_name: Option<String>,
    id_author: Option<String>,
    nn_sha256: Option<String>,
}

impl UsiProc {
    fn spawn(init: &EngineInit) -> Result<Self> {
        let mut cmd = Command::new(&init.engine_path);
        if !init.engine_args.is_empty() {
            cmd.args(&init.engine_args);
        }
        let mut child =
            cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().with_context(|| {
                format!("failed to spawn engine at {}", init.engine_path.display())
            })?;
        let stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;

        let (tx, rx) = mpsc::channel::<String>();
        thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(l) => {
                        let _ = tx.send(l);
                    }
                    Err(_) => break,
                }
            }
            // channel closes on drop
        });

        let mut p = Self {
            child,
            stdin: io::BufWriter::new(stdin),
            rx,
            opt_names: HashSet::new(),
            id_name: None,
            id_author: None,
            nn_sha256: init.nn_path.as_ref().and_then(|p| compute_sha256(p).ok()),
        };
        p.write_line("usi")?;
        // collect id/option until usiok
        loop {
            let l = p.read_line_timeout(Duration::from_secs(10))?;
            if let Some(rest) = l.strip_prefix("id name ") {
                p.id_name = Some(rest.to_string());
            } else if let Some(rest) = l.strip_prefix("id author ") {
                p.id_author = Some(rest.to_string());
            } else if l.starts_with("option ") {
                if let Some(name) = parse_option_name(&l) {
                    p.opt_names.insert(name);
                }
            } else if l == "usiok" {
                break;
            }
        }

        // set resources
        p.set_option_if_available("Threads", &init.resources.threads.to_string())?;
        p.set_option_if_available("USI_Hash", &init.resources.hash_mb.to_string())?;
        p.set_option_if_available("Hash", &init.resources.hash_mb.to_string())?; // some engines
        p.set_option_if_available("MultiPV", &init.resources.multipv.to_string())?;

        // set NN path
        if let Some(nn) = &init.nn_path {
            if let Some(nnopt) = &init.nn_option_name {
                p.set_option_if_available(nnopt, nn.to_string_lossy().as_ref())?;
            } else {
                // autodetect in priority order
                for key in ["EvalFile", "NNUEFile", "EvalDir", "Model", "WeightsFile"] {
                    if p.opt_names.contains(key) {
                        p.set_option_if_available(key, nn.to_string_lossy().as_ref())?;
                        break;
                    }
                }
            }
        }

        // extra usi-init commands
        if let Some(path) = &init.usi_init {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("failed to read usi-init at {}", path.display()))?;
            for raw in text.lines() {
                let line = raw.trim();
                if !line.is_empty() && !line.starts_with('#') {
                    p.write_line(line)?;
                }
            }
        }

        p.sync_ready()?;
        p.write_line("usinewgame")?;
        // warmup 1 query (Nice to Have, cheap)
        let _ = p.write_line("position startpos");
        let _ = p.write_line("go nodes 1");
        let _ = p.drain_until_bestmove(Duration::from_millis(200));
        let _ = p.sync_ready();
        Ok(p)
    }

    fn write_line(&mut self, s: &str) -> Result<()> {
        self.stdin.write_all(s.as_bytes())?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()?;
        Ok(())
    }

    fn read_line_timeout(&self, dur: Duration) -> Result<String> {
        self.rx.recv_timeout(dur).map_err(|_| anyhow!("engine read timeout"))
    }

    fn set_option_if_available(&mut self, name: &str, value: &str) -> Result<()> {
        if self.opt_names.contains(name) {
            self.write_line(&format!("setoption name {} value {}", name, value))?;
        }
        Ok(())
    }

    fn sync_ready(&mut self) -> Result<()> {
        self.write_line("isready")?;
        loop {
            let l = self.read_line_timeout(Duration::from_secs(10))?;
            if l == "readyok" {
                break;
            }
        }
        Ok(())
    }

    fn drain_until_bestmove(&self, max: Duration) -> bool {
        let deadline = Instant::now() + max;
        while Instant::now() < deadline {
            let remain = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(remain) {
                Ok(l) => {
                    if l.starts_with("bestmove ") {
                        return true;
                    }
                }
                Err(_) => break,
            }
        }
        false
    }

    fn query(&mut self, sfen: &str, order: &[MethodKind], timeout_ms: u64) -> QueryResult {
        let mut last = InfoSnapshot::default();
        for method in order {
            let (go_cmd, method_name) = match method {
                MethodKind::Depth0 => ("go depth 0", "depth0"),
                MethodKind::Nodes1 => ("go nodes 1", "nodes1"),
                MethodKind::Movetime1 => ("go movetime 1", "movetime1"),
            };
            // position + go
            if self.write_line(&format!("position sfen {}", sfen)).is_err() {
                continue;
            }
            if self.write_line(go_cmd).is_err() {
                continue;
            }

            let deadline = Instant::now() + Duration::from_millis(timeout_ms);
            let mut saw_bestmove = false;
            while Instant::now() < deadline {
                let remain = deadline.saturating_duration_since(Instant::now());
                let line = match self.rx.recv_timeout(remain) {
                    Ok(l) => l,
                    Err(_) => break,
                };
                if line.starts_with("info ") {
                    parse_info_line(&line, &mut last);
                } else if line.starts_with("bestmove ") {
                    saw_bestmove = true;
                    break;
                }
            }

            if saw_bestmove {
                return QueryResult {
                    valid: last.score_kind.is_some() || last.depth.is_some(),
                    method: method_name.to_string(),
                    score_kind: last.score_kind.clone(),
                    score_value: last.score_value,
                    bound: Some(last.bound.clone().unwrap_or_else(|| "exact".into())),
                    depth: last.depth,
                    seldepth: last.seldepth,
                    nodes: last.nodes,
                    time_ms: last.time_ms,
                    nps: last.nps,
                    pv_head: last.pv_head.clone(),
                };
            } else {
                // stop → drain bestmove → sync
                let _ = self.write_line("stop");
                let _ = self.drain_until_bestmove(Duration::from_millis(timeout_ms.min(100)));
                let _ = self.sync_ready();
            }
        }
        QueryResult {
            valid: false,
            method: String::from("timeout"),
            score_kind: None,
            score_value: None,
            bound: Some("unknown".into()),
            depth: None,
            seldepth: None,
            nodes: None,
            time_ms: None,
            nps: None,
            pv_head: None,
        }
    }
}

impl Drop for UsiProc {
    fn drop(&mut self) {
        // best-effort graceful shutdown
        let _ = self.write_line("quit");
        // give it a short time to exit
        let deadline = Instant::now() + Duration::from_millis(300);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn parse_option_name(line: &str) -> Option<String> {
    // example: option name Threads type spin ...
    // supports multi-word names: collect tokens between 'name' and 'type'
    if !line.starts_with("option ") {
        return None;
    }
    let mut it = line.split_whitespace().peekable();
    while let Some(tok) = it.next() {
        if tok == "name" {
            let mut buf: Vec<String> = Vec::new();
            while let Some(&t) = it.peek() {
                if t == "type" {
                    break;
                }
                buf.push(it.next().unwrap().to_string());
            }
            if !buf.is_empty() {
                return Some(buf.join(" "));
            }
        }
    }
    None
}

fn parse_info_line(line: &str, out: &mut InfoSnapshot) {
    // Lightweight parser: walk tokens and capture known fields
    let mut it = line.split_whitespace().peekable();
    // skip leading "info"
    let _ = it.next();
    while let Some(tok) = it.next() {
        match tok {
            "depth" => {
                if let Some(v) = it.next().and_then(|s| s.parse::<i32>().ok()) {
                    out.depth = Some(v);
                }
            }
            "seldepth" => {
                if let Some(v) = it.next().and_then(|s| s.parse::<i32>().ok()) {
                    out.seldepth = Some(v);
                }
            }
            "nodes" => {
                if let Some(v) = it.next().and_then(|s| s.parse::<u64>().ok()) {
                    out.nodes = Some(v);
                }
            }
            "time" => {
                if let Some(v) = it.next().and_then(|s| s.parse::<u64>().ok()) {
                    out.time_ms = Some(v);
                }
            }
            "nps" => {
                if let Some(v) = it.next().and_then(|s| s.parse::<u64>().ok()) {
                    out.nps = Some(v);
                }
            }
            "score" => {
                if let Some(kind) = it.next() {
                    match kind {
                        "cp" => {
                            if let Some(v) = it.next().and_then(|s| s.parse::<i32>().ok()) {
                                out.score_kind = Some("cp".into());
                                out.score_value = Some(v);
                            }
                        }
                        "mate" => {
                            if let Some(v) = it.next().and_then(|s| s.parse::<i32>().ok()) {
                                out.score_kind = Some("mate".into());
                                out.score_value = Some(v);
                            }
                        }
                        _ => {}
                    }
                }
            }
            "lowerbound" => {
                out.bound = Some("lower".into());
            }
            "upperbound" => {
                out.bound = Some("upper".into());
            }
            "pv" => {
                if out.pv_head.is_none() {
                    if let Some(mv) = it.next() {
                        out.pv_head = Some(mv.to_string());
                    }
                }
                // do not consume the rest (we only log head move)
                break;
            }
            _ => {}
        }
    }
}

fn compute_weight(depth: Option<i32>, bound: Option<&str>, p: &WeightParams) -> f32 {
    if !p.enabled {
        return 1.0;
    }
    let f_depth = depth.map(|d| (d as f32 / p.depth_d0).min(1.0)).unwrap_or(1.0);
    let f_bound = match bound.unwrap_or("exact") {
        "upper" => p.bound_upper,
        "lower" => p.bound_lower,
        _ => 1.0,
    };
    (f_depth * f_bound).clamp(0.0, 1.0)
}

fn merge_teacher_fields(
    mut obj: serde_json::Map<String, JsonValue>,
    write_mode: WriteMode,
    res: &QueryResult,
    weight: f32,
    mate_cp_clip: i32,
) -> JsonValue {
    // validity
    obj.insert("teacher_valid".into(), JsonValue::Bool(res.valid));

    // score
    match write_mode {
        WriteMode::TeacherScore => {
            let val = match (res.score_kind.as_deref(), res.score_value) {
                (Some("cp"), Some(v)) => json!({"type":"cp","value":v}),
                (Some("mate"), Some(v)) => json!({"type":"mate","value":v}),
                _ => JsonValue::Null,
            };
            obj.insert("teacher_score".into(), val);
        }
        WriteMode::TeacherCp => {
            let mut cpv = None;
            match (res.score_kind.as_deref(), res.score_value) {
                (Some("cp"), Some(v)) => cpv = Some(v),
                (Some("mate"), Some(v)) => {
                    let s = v.signum();
                    cpv = Some(s * mate_cp_clip.abs());
                    obj.insert("teacher_cp_from_mate".into(), JsonValue::Bool(true));
                }
                _ => {}
            }
            obj.insert("teacher_cp".into(), cpv.map(JsonValue::from).unwrap_or(JsonValue::Null));
        }
        WriteMode::Eval => {
            if let (Some("cp"), Some(v)) = (res.score_kind.as_deref(), res.score_value) {
                obj.insert("eval".into(), JsonValue::from(v));
            }
        }
    }

    // meta
    if let Some(b) = res.bound.as_ref() {
        obj.insert("teacher_bound".into(), JsonValue::from(b.clone()));
    }
    if let Some(d) = res.depth {
        obj.insert("teacher_depth".into(), JsonValue::from(d));
    }
    if let Some(sd) = res.seldepth {
        obj.insert("teacher_seldepth".into(), JsonValue::from(sd));
    }
    if let Some(n) = res.nodes {
        obj.insert("teacher_nodes".into(), JsonValue::from(n));
    }
    if let Some(t) = res.time_ms {
        obj.insert("teacher_time_ms".into(), JsonValue::from(t));
    }
    if let Some(nps) = res.nps {
        obj.insert("teacher_nps".into(), JsonValue::from(nps));
    }
    if let Some(pv) = res.pv_head.as_ref() {
        obj.insert("teacher_pv".into(), JsonValue::from(pv.clone()));
    }
    obj.insert("teacher_method".into(), JsonValue::from(res.method.clone()));
    obj.insert("teacher_weight".into(), JsonValue::from(weight));

    JsonValue::Object(obj)
}

fn build_engine_init(cli: &Cli) -> EngineInit {
    EngineInit {
        engine_path: cli.engine.clone(),
        engine_args: cli.engine_args.clone().unwrap_or_default(),
        nn_path: cli.nn.clone(),
        nn_option_name: cli.nn_option.as_ref().and_then(|s| {
            let t = s.trim();
            if t.eq_ignore_ascii_case("auto") || t.is_empty() {
                None
            } else {
                Some(t.to_string())
            }
        }),
        usi_init: cli.usi_init.clone(),
        resources: ResourceParams {
            threads: cli.threads,
            hash_mb: cli.hash_mb,
            multipv: cli.multipv,
        },
    }
}

fn build_log_entry(
    sfen: &str,
    res: &QueryResult,
    elapsed_ms: u128,
    engine_init: &EngineInit,
    id_name: Option<&str>,
    id_author: Option<&str>,
    nn_sha256: Option<&str>,
) -> JsonValue {
    let mut eng_opts = serde_json::Map::new();
    eng_opts.insert("threads".into(), JsonValue::from(engine_init.resources.threads));
    eng_opts.insert("hash_mb".into(), JsonValue::from(engine_init.resources.hash_mb));
    eng_opts.insert("multipv".into(), JsonValue::from(engine_init.resources.multipv));
    let mut eng = serde_json::Map::new();
    eng.insert(
        "name".into(),
        JsonValue::from(id_name.unwrap_or(&engine_init.engine_path.to_string_lossy())),
    );
    if let Some(a) = id_author {
        eng.insert("author".into(), JsonValue::from(a));
    }
    eng.insert(
        "nn_path".into(),
        engine_init
            .nn_path
            .as_ref()
            .map(|p| JsonValue::from(p.to_string_lossy().to_string()))
            .unwrap_or(JsonValue::Null),
    );
    eng.insert("options".into(), JsonValue::Object(eng_opts));
    if let Some(h) = nn_sha256 {
        eng.insert("fingerprint".into(), JsonValue::from(format!("sha256:{}", h)));
    }

    let mut m = serde_json::Map::new();
    m.insert("sfen".into(), JsonValue::from(sfen.to_string()));
    m.insert("elapsed_ms".into(), JsonValue::from(elapsed_ms as u64));
    m.insert("valid".into(), JsonValue::from(res.valid));
    m.insert(
        "score".into(),
        match (res.score_kind.as_deref(), res.score_value) {
            (Some("cp"), Some(v)) => json!({"type":"cp","value":v}),
            (Some("mate"), Some(v)) => json!({"type":"mate","value":v}),
            _ => JsonValue::Null,
        },
    );
    m.insert(
        "bound".into(),
        JsonValue::from(res.bound.clone().unwrap_or_else(|| "unknown".into())),
    );
    if let Some(d) = res.depth {
        m.insert("depth".into(), JsonValue::from(d));
    }
    if let Some(sd) = res.seldepth {
        m.insert("seldepth".into(), JsonValue::from(sd));
    }
    if let Some(n) = res.nodes {
        m.insert("nodes".into(), JsonValue::from(n));
    }
    if let Some(t) = res.time_ms {
        m.insert("time_ms".into(), JsonValue::from(t));
    }
    if let Some(nps) = res.nps {
        m.insert("nps".into(), JsonValue::from(nps));
    }
    if let Some(pv) = res.pv_head.as_ref() {
        m.insert("pv_head".into(), JsonValue::from(pv.clone()));
    }
    m.insert("method".into(), JsonValue::from(res.method.clone()));
    m.insert("engine".into(), JsonValue::Object(eng));
    JsonValue::Object(m)
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let cli = Cli::parse();

    if cli.inputs.is_empty() {
        return Err(anyhow!("--in is required (one or more input files)"));
    }
    let method_order = parse_method_order(&cli.method_order);
    if method_order.is_empty() {
        return Err(anyhow!("--method-order produced empty list"));
    }
    let weight_params = parse_weight(&cli.weight, !cli.no_weight);
    let engine_init = build_engine_init(&cli);

    // writer(s)
    let mut out_writer = open_maybe_compressed_writer(&cli.out)?;
    let mut log_writer: Option<Box<dyn Write>> = match &cli.log {
        Some(p) => Some(open_maybe_compressed_writer(p)?),
        None => None,
    };

    // concurrency
    let phys = physical_cores();
    let default_jobs = phys.saturating_div(2).clamp(1, 8);
    let jobs = cli.jobs.unwrap_or(default_jobs).min(phys).max(1);
    log::info!("workers: {} (physical cores: {})", jobs, phys);

    // Build workers
    let (job_tx, job_rx_raw): (Sender<Option<JobSpec>>, Receiver<Option<JobSpec>>) =
        mpsc::channel();
    let job_rx = Arc::new(Mutex::new(job_rx_raw));
    let (res_tx, res_rx): (Sender<JobResult>, Receiver<JobResult>) = mpsc::channel();
    let method_order_arc = Arc::new(method_order);
    let engine_init_arc = Arc::new(engine_init.clone());
    let weight_params_arc = Arc::new(weight_params.clone());
    for wid in 0..jobs {
        let rx = job_rx.clone();
        let tx = res_tx.clone();
        let method = method_order_arc.clone();
        let einit = engine_init_arc.clone();
        let weightp = weight_params_arc.clone();
        let write_mode = cli.write;
        let sfen_field = cli.sfen_field.clone();
        let timeout_ms = cli.timeout_ms;
        let retry = cli.retry;
        let mate_cp_clip = cli.mate_cp_clip;
        thread::spawn(move || {
            let mut engine = match UsiProc::spawn(&einit) {
                Ok(p) => p,
                Err(e) => {
                    log::error!("worker {}: failed to start engine: {}", wid, e);
                    return;
                }
            };
            loop {
                let msg = { rx.lock().unwrap().recv() };
                let job_opt: Option<JobSpec> = msg.unwrap_or_default();
                let Some(job) = job_opt else { break };
                // parse JSON line (1:1出力保証)
                let mut obj: JsonValue = match serde_json::from_str(&job.raw_json) {
                    Ok(v) => v,
                    Err(_) => {
                        let mut m = serde_json::Map::new();
                        m.insert("teacher_valid".into(), JsonValue::Bool(false));
                        m.insert("teacher_error".into(), JsonValue::from("malformed_json"));
                        let out_line = serde_json::to_string(&JsonValue::Object(m)).unwrap() + "\n";
                        let _ = tx.send(JobResult {
                            output_jsonl: out_line,
                            log_jsonl: None,
                        });
                        continue;
                    }
                };
                let Some(map) = obj.as_object_mut() else {
                    let mut m = serde_json::Map::new();
                    m.insert("teacher_valid".into(), JsonValue::Bool(false));
                    m.insert("teacher_error".into(), JsonValue::from("non_object_json"));
                    let out_line = serde_json::to_string(&JsonValue::Object(m)).unwrap() + "\n";
                    let _ = tx.send(JobResult {
                        output_jsonl: out_line,
                        log_jsonl: None,
                    });
                    continue;
                };
                let sfen_val = map.get(&sfen_field).cloned().unwrap_or(JsonValue::Null);
                let sfen = sfen_val.as_str().unwrap_or("").to_string();
                if sfen.is_empty() {
                    map.insert("teacher_valid".into(), JsonValue::Bool(false));
                    map.insert("teacher_error".into(), JsonValue::from("missing_sfen"));
                    let out_line = serde_json::to_string(&obj).unwrap() + "\n";
                    let _ = tx.send(JobResult {
                        output_jsonl: out_line,
                        log_jsonl: None,
                    });
                    continue;
                }
                let mut attempt = 0u32;
                let mut final_res = QueryResult {
                    valid: false,
                    method: String::from(""),
                    score_kind: None,
                    score_value: None,
                    bound: Some("unknown".into()),
                    depth: None,
                    seldepth: None,
                    nodes: None,
                    time_ms: None,
                    nps: None,
                    pv_head: None,
                };
                let t0 = Instant::now();
                while attempt <= retry {
                    let res = engine.query(&sfen, &method, timeout_ms);
                    if res.valid {
                        final_res = res;
                        break;
                    } else {
                        attempt += 1;
                        // try restart engine once on failure
                        if attempt <= retry {
                            if let Ok(newp) = UsiProc::spawn(&einit) {
                                engine = newp;
                            }
                        }
                    }
                }
                let elapsed = t0.elapsed().as_millis();
                // compute weight
                let wt = compute_weight(final_res.depth, final_res.bound.as_deref(), &weightp);
                // merge
                let merged =
                    merge_teacher_fields(map.clone(), write_mode, &final_res, wt, mate_cp_clip);
                let out_line =
                    serde_json::to_string(&merged).unwrap_or_else(|_| job.raw_json.clone());
                let log_line = serde_json::to_string(&build_log_entry(
                    &sfen,
                    &final_res,
                    elapsed,
                    &einit,
                    engine.id_name.as_deref(),
                    engine.id_author.as_deref(),
                    engine.nn_sha256.as_deref(),
                ))
                .ok();
                let _ = tx.send(JobResult {
                    output_jsonl: out_line + "\n",
                    log_jsonl: log_line.map(|s| s + "\n"),
                });
            }
        });
    }

    // Producer: read input files and feed jobs
    let mut total: u64 = 0;
    for path in &cli.inputs {
        let mut reader =
            open_maybe_compressed_reader(path, 4 * 1024 * 1024).map_err(|e| anyhow!("{}", e))?; // map non-Send error into anyhow
        let mut buf = String::new();
        loop {
            buf.clear();
            let n = reader.read_line(&mut buf)?;
            if n == 0 {
                break;
            }
            if buf.trim().is_empty() {
                continue;
            }
            // fast check for sfen key existence; if not present, still enqueue and let worker decide
            // assign an index
            total += 1;
            job_tx
                .send(Some(JobSpec {
                    raw_json: buf.clone(),
                }))
                .unwrap();
        }
    }
    // send shutdown to workers
    for _ in 0..jobs {
        let _ = job_tx.send(None);
    }

    // Collect and write outputs as they come
    let mut written: u64 = 0;
    while let Ok(res) = res_rx.recv_timeout(Duration::from_secs(1)) {
        out_writer.write_all(res.output_jsonl.as_bytes())?;
        if let Some(l) = res.log_jsonl.as_ref() {
            if let Some(w) = log_writer.as_mut() {
                w.write_all(l.as_bytes())?;
            }
        }
        written += 1;
        if written >= total {
            break;
        }
    }
    // flush writers
    out_writer.flush()?;
    if let Some(mut w) = log_writer {
        w.flush()?;
    }

    log::info!("processed {} samples", written);
    Ok(())
}
