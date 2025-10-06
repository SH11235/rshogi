//! Diagnostics runner for ClassicAB ordering/pruning
//!
//! Run:
//!   cargo run --release --example classicab_diagnostics
//! Options:
//!   --depth-min 4 --depth-max 6 --time-ms 10000 --tt-mb 64 --sample-every 10
//!   --no-time-limit  (depthのみ)
//!
//! 例: 深さ固定（time制限なし）
//!   cargo run --release --example classicab_diagnostics -- --no-time-limit --depth-min 4 --depth-max 6

use engine_core::evaluation::evaluate::MaterialEvaluator;
use engine_core::search::api::{InfoEvent, InfoEventCallback, SearcherBackend};
use engine_core::search::tt::TTProbe;
use engine_core::search::{SearchLimitsBuilder, TranspositionTable};
use engine_core::shogi::Position;
use std::sync::Arc;
use std::time::Instant;

use clap::Parser;

#[derive(Parser, Debug, Clone)]
#[command(name = "classicab_diagnostics", disable_help_subcommand = true)]
struct Args {
    /// 最小深さ
    #[arg(long = "depth-min", default_value_t = 4)]
    depth_min: u8,
    /// 最大深さ
    #[arg(long = "depth-max", default_value_t = 6)]
    depth_max: u8,
    /// 1手あたりの固定時間(ms)。--no-time-limit 指定時は無視
    #[arg(long = "time-ms", default_value_t = 10_000)]
    time_ms: u64,
    /// TTサイズ(MB)
    #[arg(long = "tt-mb", default_value_t = 64)]
    tt_mb: usize,
    /// CurrMoveをサンプリング出力する間隔
    #[arg(long = "sample-every", default_value_t = 10)]
    sample_every: u32,
    /// 時間制限なし（depthのみ）
    #[arg(long = "no-time-limit", default_value_t = false)]
    no_time_limit: bool,
    /// 対象局面フィルタ（部分一致）。複数指定可。
    #[arg(long = "only", value_name = "NAME", num_args = 0..)]
    only: Vec<String>,
    /// 各depthの計測前にTTをクリア
    #[arg(long = "clear-tt-per-depth", default_value_t = false)]
    clear_tt_per_depth: bool,
    /// 出力フォーマット: text|csv|json
    #[arg(long = "format", default_value = "text")]
    format: String,
    /// 出力ファイル（未指定なら標準出力）。text時は無視。
    #[arg(long = "out")]
    out: Option<std::path::PathBuf>,
    /// IID無効化
    #[arg(long = "disable-iid", default_value_t = false)]
    disable_iid: bool,
    /// NMP無効化
    #[arg(long = "disable-nmp", default_value_t = false)]
    disable_nmp: bool,
    /// 各depthを独立プロセスで実行（time固定の相互影響を排除）
    #[arg(long = "per-depth-process", default_value_t = false)]
    per_depth_process: bool,
    /// IIDのA/B比較（ON vs OFF）を同条件で実行し、差分サマリを表示
    #[arg(long = "compare-iid", default_value_t = false)]
    compare_iid: bool,
}

fn main() {
    let args = Args::parse();
    if args.compare_iid {
        run_compare_iid(&args);
        return;
    }
    let tests = vec![
        ("Initial", "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1"),
        (
            "Midgame",
            "l6nl/5+P1gk/2np1S3/p1p4Pp/3P2Sp1/1PPb2P1P/P5GS1/R8/LN4bKL w GR5pnsg 1",
        ),
        (
            "Tactical",
            "ln1g1g1nl/1ks2r3/1pppp1bpp/p6p1/9/P1P4P1/1P1PPPP1P/1BK1GS1R1/LNSG3NL b Pp 1",
        ),
        ("Endgame", "8l/7p1/6gk1/5Sp1p/9/5G1PP/7K1/9/7NL b RBG2S2N2L13P2rbgsnl 1"),
    ];

    let tt = Arc::new(TranspositionTable::new(args.tt_mb));
    let toggles = engine_core::search::ab::PruneToggles {
        enable_iid: !args.disable_iid,
        enable_nmp: !args.disable_nmp,
        enable_razor: true,
        enable_probcut: true,
        enable_static_beta_pruning: true,
    };
    let backend = engine_core::search::ab::ClassicBackend::with_tt_and_toggles(
        Arc::new(MaterialEvaluator),
        tt.clone(),
        toggles,
    );

    let text_mode = args.format == "text";
    if text_mode {
        println!("ClassicAB Diagnostics (depth {}..{})\n", args.depth_min, args.depth_max);
    }
    #[derive(serde::Serialize)]
    struct Row<'a> {
        name: &'a str,
        sfen: &'a str,
        depth: u8,
        nodes: u64,
        nps: u64,
        hashfull: u16,
        duplication_pct: f64,
        score: i32,
        tt_hits: u64,
        lmr: u64,
        lmr_trials: u64,
        beta_cuts: u64,
        asp_fail_high: u32,
        asp_fail_low: u32,
        tt_root_match: u8,
        tt_root_depth: u8,
        root_tt_hint_exists: u64,
        root_tt_hint_used: u64,
        heur_quiet_max: i16,
        heur_cont_max: i16,
        heur_capture_max: i16,
        heur_counter_filled: u32,
    }
    let mut rows: Vec<Row> = Vec::new();
    for (name, sfen) in tests {
        if !args.only.is_empty() && !args.only.iter().any(|f| name.contains(f)) {
            continue;
        }
        let pos = Position::from_sfen(sfen).expect("valid sfen");
        if text_mode {
            println!("[{name}] {sfen}");
        }
        for depth in args.depth_min..=args.depth_max {
            if args.per_depth_process {
                // 再帰起動（独立プロセス）
                let mut child_args: Vec<String> = vec![
                    "--depth-min".into(),
                    depth.to_string(),
                    "--depth-max".into(),
                    depth.to_string(),
                    "--time-ms".into(),
                    args.time_ms.to_string(),
                    "--tt-mb".into(),
                    args.tt_mb.to_string(),
                    "--sample-every".into(),
                    args.sample_every.to_string(),
                    "--format".into(),
                    args.format.clone(),
                ];
                if args.no_time_limit {
                    child_args.push("--no-time-limit".into());
                }
                if args.clear_tt_per_depth {
                    child_args.push("--clear-tt-per-depth".into());
                }
                if args.disable_iid {
                    child_args.push("--disable-iid".into());
                }
                if args.disable_nmp {
                    child_args.push("--disable-nmp".into());
                }
                for f in &args.only {
                    child_args.push("--only".into());
                    child_args.push(f.clone());
                }
                let exe = std::env::current_exe().expect("exe");
                let out = std::process::Command::new(exe).args(child_args).output().expect("spawn");
                if args.format == "text" {
                    print!("{}", String::from_utf8_lossy(&out.stdout));
                } else if args.format == "csv" {
                    // ヘッダ重複を避けて2行目以降を連結
                    let s = String::from_utf8_lossy(&out.stdout);
                    let mut lines = s.lines();
                    let header = lines.next().unwrap_or("");
                    if rows.is_empty() {
                        // 初回のみヘッダを出す
                        println!("{}", header);
                    }
                    for line in lines {
                        if !line.trim().is_empty() {
                            println!("{}", line);
                        }
                    }
                } else {
                    // jsonは子のstdoutをそのまま出力（複数depthなら親側での集約を推奨）
                    print!("{}", String::from_utf8_lossy(&out.stdout));
                }
                continue;
            }
            let mut builder = SearchLimitsBuilder::default().depth(depth);
            if !args.no_time_limit {
                builder = builder.fixed_time_ms(args.time_ms);
            }
            let limits = builder.build();
            let start = Instant::now();
            // Counters from events
            use std::sync::atomic::{AtomicU32, Ordering};
            use std::sync::Arc as StdArc;
            let asp_fail_high = StdArc::new(AtomicU32::new(0));
            let asp_fail_low = StdArc::new(AtomicU32::new(0));
            let sample_every = args.sample_every;
            // Collect minimal InfoEvent diagnostics
            let asp_fh_cb = asp_fail_high.clone();
            let asp_fl_cb = asp_fail_low.clone();
            let info: InfoEventCallback = Arc::new(move |evt| match evt {
                InfoEvent::Hashfull(h) => {
                    if text_mode {
                        eprintln!("  [event] hashfull {h}");
                    }
                }
                InfoEvent::CurrMove { mv, number } => {
                    if text_mode && number % sample_every == 1 {
                        eprintln!(
                            "  [event] currmove {} #{number}",
                            engine_core::usi::move_to_usi(&mv)
                        );
                    }
                }
                InfoEvent::Aspiration { outcome, .. } => match outcome {
                    engine_core::search::api::AspirationOutcome::FailHigh => {
                        asp_fh_cb.fetch_add(1, Ordering::Relaxed);
                    }
                    engine_core::search::api::AspirationOutcome::FailLow => {
                        asp_fl_cb.fetch_add(1, Ordering::Relaxed);
                    }
                },
                _ => {}
            });
            if args.clear_tt_per_depth {
                tt.clear_in_place();
            }
            let res = backend.think_blocking(&pos, &limits, Some(info));
            let elapsed = start.elapsed();
            let nps = (res.stats.nodes as f64 / elapsed.as_secs_f64()) as u64;
            let hf = tt.hashfull_permille();
            let tt_hits = res.stats.tt_hits.unwrap_or(0);
            let lmr = res.stats.lmr_count.unwrap_or(0);
            let lmr_trials = res.stats.lmr_trials.unwrap_or(lmr);
            let beta = res.stats.root_fail_high_count.unwrap_or(0);
            let asp_fail_high = asp_fail_high.load(Ordering::Relaxed);
            let asp_fail_low = asp_fail_low.load(Ordering::Relaxed);
            let duplication = res.stats.duplication_percentage.unwrap_or(0.0);
            let heur_summary =
                res.stats.heuristics.as_ref().map(|h| h.summary()).unwrap_or_default();
            // Probe TT at root to check adoption
            let (tt_root_match, tt_root_depth) = {
                let entry = tt.probe(pos.zobrist_hash(), pos.side_to_move);
                match (entry, res.best_move) {
                    (Some(e), Some(bm)) => {
                        let tt_mv = e.get_move();
                        ((tt_mv == Some(bm)) as u8, e.depth())
                    }
                    _ => (0, 0),
                }
            };
            if args.format == "text" {
                println!(
                    "  depth {:>2}  nodes {:>10}  nps {:>9}  hashfull {:>4}  dup {:>6.1}  score {:>6}  tt_hits {:>8}  lmr {:>8}  lmr_trials {:>8}  beta_cuts {:>8}  aspFH {:>3}  aspFL {:>3}  root_hint_exist {:>1}  root_hint_used {:>1}  tt_root_match {:>1}  tt_root_depth {:>2}  heur_quiet_max {:>5}  heur_cont_max {:>5}  heur_capture_max {:>5}  heur_counter {:>6}",
                    depth,
                    res.stats.nodes,
                    nps,
                    hf,
                    duplication,
                    res.score,
                    tt_hits,
                    lmr,
                    lmr_trials,
                    beta,
                    asp_fail_high,
                    asp_fail_low,
                    res.stats.root_tt_hint_exists.unwrap_or(0),
                    res.stats.root_tt_hint_used.unwrap_or(0),
                    tt_root_match,
                    tt_root_depth,
                    heur_summary.quiet_max,
                    heur_summary.continuation_max,
                    heur_summary.capture_max,
                    heur_summary.counter_filled
                );
            } else {
                rows.push(Row {
                    name,
                    sfen,
                    depth,
                    nodes: res.stats.nodes,
                    nps,
                    hashfull: hf,
                    duplication_pct: duplication,
                    score: res.score,
                    tt_hits,
                    lmr,
                    lmr_trials,
                    beta_cuts: beta,
                    asp_fail_high,
                    asp_fail_low,
                    tt_root_match,
                    tt_root_depth,
                    root_tt_hint_exists: res.stats.root_tt_hint_exists.unwrap_or(0),
                    root_tt_hint_used: res.stats.root_tt_hint_used.unwrap_or(0),
                    heur_quiet_max: heur_summary.quiet_max,
                    heur_cont_max: heur_summary.continuation_max,
                    heur_capture_max: heur_summary.capture_max,
                    heur_counter_filled: heur_summary.counter_filled,
                });
            }
        }
        println!();
    }

    if args.format == "csv" || args.format == "json" {
        if args.format == "csv" {
            let mut out = String::new();
            out.push_str("name,sfen,depth,nodes,nps,hashfull,duplication_pct,score,tt_hits,lmr,lmr_trials,beta_cuts,aspFH,aspFL,root_hint_exist,root_hint_used,tt_root_match,tt_root_depth,heur_quiet_max,heur_cont_max,heur_capture_max,heur_counter_filled\n");
            for r in &rows {
                out.push_str(&format!(
                    "{},{},{},{},{},{},{:.2},{},{},{},{},{},{},{},{},{},{},{},{},{},{}\n",
                    r.name,
                    r.sfen,
                    r.depth,
                    r.nodes,
                    r.nps,
                    r.hashfull,
                    r.duplication_pct,
                    r.score,
                    r.tt_hits,
                    r.lmr,
                    r.lmr_trials,
                    r.beta_cuts,
                    r.asp_fail_high,
                    r.asp_fail_low,
                    r.root_tt_hint_exists,
                    r.root_tt_hint_used,
                    r.tt_root_match,
                    r.tt_root_depth,
                    r.heur_quiet_max,
                    r.heur_cont_max,
                    r.heur_capture_max,
                    r.heur_counter_filled
                ));
            }
            if let Some(p) = args.out {
                std::fs::write(p, out).expect("write csv");
            } else {
                print!("{}", out);
            }
        } else {
            let s = serde_json::to_string_pretty(&rows).expect("json");
            if let Some(p) = args.out {
                std::fs::write(p, s).expect("write json");
            } else {
                println!("{}", s);
            }
        }
    }
}

fn run_compare_iid(args: &Args) {
    // 子プロセスで同一条件（format=csv, per-depth）を baseline(ON) と OFF の2回実行
    fn run_once(args: &Args, disable_iid: bool) -> String {
        let mut child_args: Vec<String> = vec![
            "--depth-min".into(),
            args.depth_min.to_string(),
            "--depth-max".into(),
            args.depth_max.to_string(),
            "--tt-mb".into(),
            args.tt_mb.to_string(),
            "--sample-every".into(),
            args.sample_every.to_string(),
            "--format".into(),
            "csv".into(),
            "--per-depth-process".into(),
        ];
        if !args.no_time_limit {
            child_args.push("--time-ms".into());
            child_args.push(args.time_ms.to_string());
        } else {
            child_args.push("--no-time-limit".into());
        }
        if args.clear_tt_per_depth {
            child_args.push("--clear-tt-per-depth".into());
        }
        if args.disable_nmp {
            child_args.push("--disable-nmp".into());
        }
        for f in &args.only {
            child_args.push("--only".into());
            child_args.push(f.clone());
        }
        if disable_iid {
            child_args.push("--disable-iid".into());
        }
        let exe = std::env::current_exe().expect("exe");
        let out = std::process::Command::new(exe)
            .args(child_args)
            .output()
            .expect("spawn compare child");
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    let on = run_once(args, false);
    let off = run_once(args, true);
    // 簡易CSVパーサ（ヘッダ名に依存）
    fn parse_csv(s: &str) -> Vec<std::collections::HashMap<String, String>> {
        let mut lines = s.lines();
        // skip leading empties
        let mut header_opt = None;
        while let Some(l) = lines.next() {
            if !l.trim().is_empty() {
                header_opt = Some(l.to_string());
                break;
            }
        }
        let header = match header_opt {
            Some(h) => h,
            None => return vec![],
        };
        let cols: Vec<String> = header.split(',').map(|x| x.trim().to_string()).collect();
        lines
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                let vals: Vec<String> = l.split(',').map(|x| x.trim().to_string()).collect();
                let mut m = std::collections::HashMap::new();
                for (i, k) in cols.iter().enumerate() {
                    if let Some(v) = vals.get(i) {
                        m.insert(k.clone(), v.clone());
                    }
                }
                m
            })
            .collect()
    }
    let on_rows = parse_csv(&on);
    let off_rows = parse_csv(&off);
    use std::collections::HashMap;
    let mut off_map: HashMap<(String, String), HashMap<String, String>> = HashMap::new();
    for r in off_rows {
        off_map.insert(
            (
                r.get("name").cloned().unwrap_or_default(),
                r.get("depth").cloned().unwrap_or_default(),
            ),
            r,
        );
    }
    // サマリ出力（CSV）
    println!("name,depth,tt_hits_on,tt_hits_off,delta_hits,beta_on,beta_off,delta_beta,nodes_on,nodes_off,nodes_ratio,root_hint_used_on,root_hint_used_off");
    let mut sum_hits_on: u64 = 0;
    let mut sum_hits_off: u64 = 0;
    let mut sum_beta_on: u64 = 0;
    let mut sum_beta_off: u64 = 0;
    let mut sum_nodes_on: u128 = 0;
    let mut sum_nodes_off: u128 = 0;
    for r in on_rows {
        let key = (
            r.get("name").cloned().unwrap_or_default(),
            r.get("depth").cloned().unwrap_or_default(),
        );
        if let Some(offr) = off_map.get(&key) {
            let g = |m: &HashMap<String, String>, k: &str| -> u64 {
                m.get(k).and_then(|v| v.parse().ok()).unwrap_or(0)
            };
            let name = &key.0;
            let depth = &key.1;
            let tt_on = g(&r, "tt_hits");
            let tt_off = g(offr, "tt_hits");
            let b_on = g(&r, "beta_cuts");
            let b_off = g(offr, "beta_cuts");
            let n_on = g(&r, "nodes");
            let n_off = g(offr, "nodes");
            let used_on = g(&r, "root_hint_used");
            let used_off = g(offr, "root_hint_used");
            let ratio = if n_off > 0 {
                (n_on as f64) / (n_off as f64)
            } else {
                0.0
            };
            println!(
                "{},{},{},{},{},{},{},{},{},{},{:.3},{},{}",
                name,
                depth,
                tt_on,
                tt_off,
                tt_on as i64 - tt_off as i64,
                b_on,
                b_off,
                b_on as i64 - b_off as i64,
                n_on,
                n_off,
                ratio,
                used_on,
                used_off
            );
            sum_hits_on += tt_on;
            sum_hits_off += tt_off;
            sum_beta_on += b_on;
            sum_beta_off += b_off;
            sum_nodes_on += n_on as u128;
            sum_nodes_off += n_off as u128;
        }
    }
    if sum_nodes_off > 0 {
        let ratio = (sum_nodes_on as f64) / (sum_nodes_off as f64);
        println!(
            "TOTAL,,{},{},{},{},{},{},{},{},{:.3},,",
            sum_hits_on,
            sum_hits_off,
            sum_hits_on as i64 - sum_hits_off as i64,
            sum_beta_on,
            sum_beta_off,
            sum_beta_on as i64 - sum_beta_off as i64,
            sum_nodes_on,
            sum_nodes_off,
            ratio
        );
    }
}
