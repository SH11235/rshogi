use anyhow::{anyhow, Context, Result};
use clap::{ArgAction, Parser};
use engine_core::evaluation::nnue::single_state::SingleAcc;
use engine_core::evaluation::nnue::weights::load_single_weights;
use engine_core::movegen::MoveGenerator;
use engine_core::shogi::{Move, Position};
use engine_core::usi::{parse_sfen, parse_usi_move};
use rand::{RngCore, SeedableRng};
use rand_xoshiro::Xoshiro256Plus;
use regex::Regex;
use serde_json::json;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

fn parse_u64_any(s: &str) -> std::result::Result<u64, String> {
    let t = s.trim();
    if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).map_err(|e| e.to_string())
    } else {
        t.parse::<u64>().map_err(|e| e.to_string())
    }
}

#[derive(Parser, Debug)]
#[command(
    about = "NNUE SINGLE (diff vs refresh) micro-benchmark with fixed-line mode",
    version
)]
struct Args {
    /// Path to SINGLE_CHANNEL weights (trainer export with END_HEADER)
    #[arg(long, value_name = "FILE")]
    single_weights: String,

    /// Seconds to run for each benchmark section
    #[arg(long, default_value_t = 5)]
    seconds: u64,

    /// Warmup seconds before measurement (cache warm)
    #[arg(long, default_value_t = 2)]
    warmup_seconds: u64,

    /// Threads for benchmark runner (default: 1). Currently single-threaded only.
    #[arg(long, default_value_t = 1)]
    threads: usize,

    /// Enable fixed-line benchmark mode
    #[arg(long, action = ArgAction::SetTrue)]
    fixed_line: bool,

    /// Use startpos as the root position (for fixed-line generation)
    #[arg(long, action = ArgAction::SetTrue)]
    startpos: bool,

    /// SFEN root position (alternative to --startpos)
    #[arg(long)]
    sfen: Option<String>,

    /// Comma-separated USI moves for fixed line (e.g., "7g7f,3c3d,2g2f")
    #[arg(long)]
    moves: Option<String>,

    /// Line file: lines formatted as `sfen <...> moves <usi1> <usi2> ...`
    #[arg(long, value_name = "FILE")]
    line_file: Option<String>,

    /// Deterministic line generation (requires --length). Uses RNG with --seed.
    #[arg(long, action = ArgAction::SetTrue)]
    deterministic_line: bool,

    /// RNG seed for deterministic line generation
    #[arg(long, value_parser = parse_u64_any, value_name = "SEED", help = "RNG seed (decimal or 0xHEX)")]
    seed: Option<u64>,

    /// Line length for deterministic generation
    #[arg(long, default_value_t = 128)]
    length: usize,

    /// Write JSON metrics to file (or '-' for stdout)
    #[arg(long)]
    json: Option<String>,

    /// Write Markdown report to file
    #[arg(long)]
    report: Option<String>,
}

#[derive(Clone)]
struct FixedCase {
    root: Position,
    pre_positions: Vec<Position>, // pre_pos[i] used with moves[i]
    moves: Vec<Move>,
}

fn parse_line_file(path: &str) -> Result<Vec<FixedCase>> {
    let text = fs::read_to_string(path).with_context(|| format!("failed to read {path}"))?;
    let mut cases = Vec::new();
    // 寛容なパース: 先頭/間の空白を包括。例: "sfen <SFEN>   moves    <m1>  <m2> ..."
    // 非貪欲で SFEN を取得し、その後ろの moves 以降をまとめて取得。
    let re = Regex::new(r"(?i)^\s*sfen\s+(.+?)\s+moves\s+(.+?)\s*$")
        .map_err(|e| anyhow!("invalid regex: {e}"))?;
    for (idx, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let caps = re
            .captures(line)
            .ok_or_else(|| anyhow!("line {}: expected 'sfen <...> moves <...>'", idx + 1))?;
        let sfen_str = caps.get(1).map(|m| m.as_str().trim()).unwrap_or("");
        let moves_part = caps.get(2).map(|m| m.as_str().trim()).unwrap_or("");
        let mut root =
            parse_sfen(sfen_str).map_err(|e| anyhow!("line {}: invalid sfen: {e:?}", idx + 1))?;
        let mut pre_positions = Vec::new();
        let mut moves = Vec::new();
        // 任意: デバッグ時のみ合法性チェック（ライン誤記の早期検知）
        let gen = MoveGenerator::new();
        for token in moves_part.split_whitespace() {
            let mv = parse_usi_move(token)
                .map_err(|e| anyhow!("line {}: invalid move '{}': {e:?}", idx + 1, token))?;
            #[cfg(debug_assertions)]
            {
                let legal = gen.generate_all(&root).unwrap_or_default();
                debug_assert!(
                    legal.iter().any(|&m| m == mv),
                    "line {}: illegal move '{}' for given position",
                    idx + 1,
                    token
                );
            }
            // legality check via do_move; capture pre_pos
            pre_positions.push(root.clone());
            let _u = root.do_move(mv);
            moves.push(mv);
        }
        cases.push(FixedCase {
            root,
            pre_positions,
            moves,
        });
    }
    if cases.is_empty() {
        return Err(anyhow!("no valid lines found in line-file"));
    }
    Ok(cases)
}

fn parse_fixed_line_from_args(args: &Args) -> Result<Vec<FixedCase>> {
    if let Some(ref lf) = args.line_file {
        return parse_line_file(lf);
    }
    // startpos/sfen + moves
    let mut root = if args.startpos || args.sfen.is_none() {
        Position::startpos()
    } else {
        parse_sfen(args.sfen.as_ref().unwrap()).map_err(|e| anyhow!("invalid sfen: {e:?}"))?
    };

    if let Some(ref s) = args.moves {
        let mut pre_positions = Vec::new();
        let mut moves = Vec::new();
        let gen = MoveGenerator::new();
        for tok in s.split(',') {
            let t = tok.trim();
            if t.is_empty() {
                continue;
            }
            let mv = parse_usi_move(t).map_err(|e| anyhow!("invalid move '{}': {e:?}", t))?;
            #[cfg(debug_assertions)]
            {
                let legal = gen.generate_all(&root).unwrap_or_default();
                debug_assert!(
                    legal.iter().any(|&m| m == mv),
                    "illegal move '{}' for given position",
                    t
                );
            }
            pre_positions.push(root.clone());
            let _u = root.do_move(mv);
            moves.push(mv);
        }
        return Ok(vec![FixedCase {
            root,
            pre_positions,
            moves,
        }]);
    }

    if args.deterministic_line {
        let len = args.length.max(1);
        let mut rng = Xoshiro256Plus::seed_from_u64(args.seed.unwrap_or(0x00C0_FFEE));
        let mut pre_positions = Vec::with_capacity(len);
        let mut moves = Vec::with_capacity(len);
        let gen = MoveGenerator::new();
        let mut cur = root.clone();
        for _ in 0..len {
            let legal = gen.generate_all(&cur).unwrap_or_default();
            if legal.is_empty() {
                break;
            }
            let idx = (rng.next_u64() as usize) % legal.len();
            let mv = legal[idx];
            pre_positions.push(cur.clone());
            let _u = cur.do_move(mv);
            moves.push(mv);
        }
        return Ok(vec![FixedCase {
            root,
            pre_positions,
            moves,
        }]);
    }

    Err(anyhow!(
        "fixed-line mode requires one of: --line-file, --moves, or --deterministic-line"
    ))
}

fn bench_refresh_update(
    cases: &[FixedCase],
    net: &engine_core::evaluation::nnue::single::SingleChannelNet,
    seconds: u64,
) -> u64 {
    let target = Duration::from_secs(seconds);
    let mut iters = 0u64;
    let start = Instant::now();
    let mut i = 0usize;
    while start.elapsed() < target {
        let case = &cases[i % cases.len()];
        let _acc = SingleAcc::refresh(&case.root, net);
        iters += 1;
        i = i.wrapping_add(1);
    }
    let dur = start.elapsed().as_secs_f64();
    (iters as f64 / dur) as u64
}

fn bench_refresh_eval(
    cases: &[FixedCase],
    net: &engine_core::evaluation::nnue::single::SingleChannelNet,
    seconds: u64,
) -> u64 {
    let target = Duration::from_secs(seconds);
    let mut iters = 0u64;
    let start = Instant::now();
    let mut i = 0usize;
    while start.elapsed() < target {
        let case = &cases[i % cases.len()];
        let acc = SingleAcc::refresh(&case.root, net);
        let _s = net.evaluate_from_accumulator(acc.acc_for(case.root.side_to_move));
        iters += 1;
        i = i.wrapping_add(1);
    }
    let dur = start.elapsed().as_secs_f64();
    (iters as f64 / dur) as u64
}

fn bench_apply_update_only(
    cases: &[FixedCase],
    net: &engine_core::evaluation::nnue::single::SingleChannelNet,
    seconds: u64,
) -> u64 {
    // Precompute acc0 for every pre_pos to avoid mixing refresh cost
    let mut acc0s: Vec<Vec<SingleAcc>> = Vec::with_capacity(cases.len());
    for c in cases {
        let v = c.pre_positions.iter().map(|p| SingleAcc::refresh(p, net)).collect::<Vec<_>>();
        acc0s.push(v);
    }
    let target = Duration::from_secs(seconds);
    let mut iters = 0u64;
    let start = Instant::now();
    let mut outer = 0usize;
    while start.elapsed() < target {
        let ci = outer % cases.len();
        let case = &cases[ci];
        if case.moves.is_empty() {
            outer = outer.wrapping_add(1);
            continue;
        }
        for (k, &mv) in case.moves.iter().enumerate() {
            let acc0 = &acc0s[ci][k];
            let _next = SingleAcc::apply_update(acc0, &case.pre_positions[k], mv, net);
            iters += 1;
            if start.elapsed() >= target {
                break;
            }
        }
        outer = outer.wrapping_add(1);
    }
    let dur = start.elapsed().as_secs_f64();
    (iters as f64 / dur) as u64
}

fn bench_apply_eval(
    cases: &[FixedCase],
    net: &engine_core::evaluation::nnue::single::SingleChannelNet,
    seconds: u64,
) -> u64 {
    let mut acc0s: Vec<Vec<SingleAcc>> = Vec::with_capacity(cases.len());
    for c in cases {
        let v = c.pre_positions.iter().map(|p| SingleAcc::refresh(p, net)).collect::<Vec<_>>();
        acc0s.push(v);
    }
    let target = Duration::from_secs(seconds);
    let mut iters = 0u64;
    let start = Instant::now();
    let mut outer = 0usize;
    while start.elapsed() < target {
        let ci = outer % cases.len();
        let case = &cases[ci];
        if case.moves.is_empty() {
            outer = outer.wrapping_add(1);
            continue;
        }
        for (k, &mv) in case.moves.iter().enumerate() {
            let acc0 = &acc0s[ci][k];
            let next = SingleAcc::apply_update(acc0, &case.pre_positions[k], mv, net);
            // after move, side_to_move flips
            let stm = case.pre_positions[k].side_to_move.opposite();
            let _s = net.evaluate_from_accumulator(next.acc_for(stm));
            iters += 1;
            if start.elapsed() >= target {
                break;
            }
        }
        outer = outer.wrapping_add(1);
    }
    let dur = start.elapsed().as_secs_f64();
    (iters as f64 / dur) as u64
}

fn bench_chain_update_only(
    cases: &[FixedCase],
    net: &engine_core::evaluation::nnue::single::SingleChannelNet,
    seconds: u64,
) -> u64 {
    let target = Duration::from_secs(seconds);
    let mut iters = 0u64;
    let start = Instant::now();
    let mut outer = 0usize;
    while start.elapsed() < target {
        let ci = outer % cases.len();
        let case = &cases[ci];
        if case.moves.is_empty() {
            outer = outer.wrapping_add(1);
            continue;
        }
        let mut acc = SingleAcc::refresh(&case.root, net);
        for (k, &mv) in case.moves.iter().enumerate() {
            acc = SingleAcc::apply_update(&acc, &case.pre_positions[k], mv, net);
            iters += 1;
            if start.elapsed() >= target {
                break;
            }
        }
        outer = outer.wrapping_add(1);
    }
    let dur = start.elapsed().as_secs_f64();
    (iters as f64 / dur) as u64
}

fn bench_chain_eval(
    cases: &[FixedCase],
    net: &engine_core::evaluation::nnue::single::SingleChannelNet,
    seconds: u64,
) -> u64 {
    let target = Duration::from_secs(seconds);
    let mut iters = 0u64;
    let start = Instant::now();
    let mut outer = 0usize;
    while start.elapsed() < target {
        let ci = outer % cases.len();
        let case = &cases[ci];
        if case.moves.is_empty() {
            outer = outer.wrapping_add(1);
            continue;
        }
        let mut acc = SingleAcc::refresh(&case.root, net);
        // stm flips along chain
        let mut stm = case.root.side_to_move;
        for (k, &mv) in case.moves.iter().enumerate() {
            acc = SingleAcc::apply_update(&acc, &case.pre_positions[k], mv, net);
            stm = stm.opposite();
            let _s = net.evaluate_from_accumulator(acc.acc_for(stm));
            iters += 1;
            if start.elapsed() >= target {
                break;
            }
        }
        outer = outer.wrapping_add(1);
    }
    let dur = start.elapsed().as_secs_f64();
    (iters as f64 / dur) as u64
}

fn gather_env_json(
    args: &Args,
    net: &engine_core::evaluation::nnue::single::SingleChannelNet,
) -> serde_json::Value {
    let rustc = std::process::Command::new("rustc")
        .arg("-V")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "rustc unknown".to_string());
    let git = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let tool_version = env!("CARGO_PKG_VERSION");
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let target = format!(
        "{}-{}",
        std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default(),
        std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default()
    );
    let rustflags = std::env::var("RUSTFLAGS").unwrap_or_default();
    let features = "engine-core:tt_metrics,hashfull_filter,nnue_single_diff,nnue_telemetry";
    json!({
        "schema_version": "nnue_bench_v1",
        "tool_version": tool_version,
        "git_sha": git,
        "rustc": rustc,
        "profile": profile,
        "target": target,
        "rustflags": rustflags,
        "features": features,
        "threads": args.threads,
        "timestamp_utc": chrono::Utc::now().to_rfc3339(),
        "weights": {
            "path": Path::new(&args.single_weights).file_name().and_then(|s| s.to_str()).unwrap_or(&args.single_weights),
            "uid": net.uid,
            "acc_dim": net.acc_dim,
            "n_feat": net.n_feat,
        },
    })
}

fn write_report_md(
    path: &str,
    env: &serde_json::Value,
    line_meta: &serde_json::Value,
    metrics: &serde_json::Value,
) -> Result<()> {
    let mut md = String::new();
    md.push_str("# NNUE Fixed-Line Benchmark\n\n");
    md.push_str("## Environment\n");
    md.push_str(&format!(
        "- rustc: {}\n- profile: {}\n- target: {}\n- features: {}\n- git: {}\n- threads: {}\n- weights: uid={} acc_dim={} n_feat={} file='{}'\n\n",
        env["rustc"].as_str().unwrap_or(""),
        env["profile"].as_str().unwrap_or(""),
        env["target"].as_str().unwrap_or(""),
        env["features"].as_str().unwrap_or(""),
        env["git_sha"].as_str().unwrap_or(""),
        env["threads"],
        env["weights"]["uid"],
        env["weights"]["acc_dim"],
        env["weights"]["n_feat"],
        env["weights"]["path"].as_str().unwrap_or("")
    ));
    md.push_str("## Line\n");
    md.push_str(&format!(
        "- mode: {}\n- cases: {}\n- line_len: {}\n\n",
        line_meta["mode"].as_str().unwrap_or(""),
        line_meta["cases"],
        line_meta["line_len"]
    ));
    md.push_str("## Metrics (EPS)\n");
    md.push_str("- Update-only: refresh_update_eps / apply_update_eps / chain_update_eps\n");
    md.push_str("- Eval-included: refresh_eval_eps / apply_eval_eps / chain_eval_eps\n\n");
    md.push_str(&format!(
        "```\nrefresh_update_eps: {}\napply_update_eps: {}\nchain_update_eps: {}\nrefresh_eval_eps: {}\napply_eval_eps: {}\nchain_eval_eps: {}\nseconds: {}\n```\n",
        metrics["refresh_update_eps"],
        metrics["apply_update_eps"],
        metrics["chain_update_eps"],
        metrics["refresh_eval_eps"],
        metrics["apply_eval_eps"],
        metrics["chain_eval_eps"],
        metrics["seconds"]
    ));
    fs::create_dir_all(Path::new(path).parent().unwrap_or_else(|| Path::new(".")))?;
    fs::write(path, md)?;
    Ok(())
}

#[cfg(test)]
mod tests_seed_parse {
    use super::parse_u64_any;

    #[test]
    fn parse_decimal_ok() {
        assert_eq!(parse_u64_any("12648430").unwrap(), 12_648_430u64);
        assert_eq!(parse_u64_any("0").unwrap(), 0);
        assert!(parse_u64_any("-1").is_err());
    }

    #[test]
    fn parse_hex_ok() {
        assert_eq!(parse_u64_any("0xC0FFEE").unwrap(), 0xC0FFEEu64);
        assert_eq!(parse_u64_any("0XdeadBEEF").unwrap(), 0xDEADBEEFu64);
        assert!(parse_u64_any("0x").is_err());
    }
}

fn main() -> Result<()> {
    env_logger::init();
    let args = Args::parse();
    if args.threads != 1 {
        eprintln!("WARN: nnue_benchmark runs single-threaded; ignoring --threads={}", args.threads);
    }

    let net = load_single_weights(&args.single_weights)
        .map_err(|e| anyhow!("failed to load SINGLE weights: {e}"))?;

    // Build cases depending on mode
    let cases: Vec<FixedCase> = if args.fixed_line {
        parse_fixed_line_from_args(&args)?
    } else {
        // Fallback: simple suite (startpos + K vs K endgame)
        let mut cases = Vec::new();
        let root = Position::startpos();
        // create a shallow deterministic line from startpos (first 32 legal)
        let gen = MoveGenerator::new();
        let legal = gen.generate_all(&root).unwrap_or_default();
        let n = legal.len().min(32);
        let mut pre_positions = Vec::new();
        let mut cur = root.clone();
        let mut moves = Vec::new();
        for i in 0..n {
            let mv = legal[i];
            pre_positions.push(cur.clone());
            let _u = cur.do_move(mv);
            moves.push(mv);
        }
        cases.push(FixedCase {
            root,
            pre_positions,
            moves,
        });
        // K vs K position
        {
            let mut p = Position::empty();
            use engine_core::usi::parse_usi_square;
            use engine_core::{Color, Piece, PieceType};
            p.board.put_piece(
                parse_usi_square("5i").unwrap(),
                Piece::new(PieceType::King, Color::Black),
            );
            p.board.put_piece(
                parse_usi_square("5a").unwrap(),
                Piece::new(PieceType::King, Color::White),
            );
            cases.push(FixedCase {
                root: p,
                pre_positions: Vec::new(),
                moves: Vec::new(),
            });
        }
        cases
    };

    // Warmup
    if args.warmup_seconds > 0 {
        let _ = bench_refresh_eval(&cases, &net, args.warmup_seconds);
    }

    // Measure 2系×3バリアント
    let refresh_update_eps = bench_refresh_update(&cases, &net, args.seconds);
    let apply_update_eps = bench_apply_update_only(&cases, &net, args.seconds);
    let chain_update_eps = bench_chain_update_only(&cases, &net, args.seconds);
    let refresh_eval_eps = bench_refresh_eval(&cases, &net, args.seconds);
    let apply_eval_eps = bench_apply_eval(&cases, &net, args.seconds);
    let chain_eval_eps = bench_chain_eval(&cases, &net, args.seconds);

    println!("=== NNUE Single Benchmark ===");
    println!("Weights: {}", args.single_weights);
    println!(
        "Update-only EPS: refresh={} apply={} chain={}",
        refresh_update_eps, apply_update_eps, chain_update_eps
    );
    println!(
        "Eval-included EPS: refresh={} apply={} chain={}",
        refresh_eval_eps, apply_eval_eps, chain_eval_eps
    );
    if refresh_eval_eps > 0 {
        println!(
            "Speedup (apply/refresh eval): {:.2}x",
            apply_eval_eps as f64 / refresh_eval_eps as f64
        );
        println!(
            "Speedup (chain/refresh eval): {:.2}x",
            chain_eval_eps as f64 / refresh_eval_eps as f64
        );
    }

    // JSON and Markdown outputs
    let env_json = gather_env_json(&args, &net);
    let line_mode = if args.fixed_line {
        if args.line_file.is_some() {
            "file"
        } else if args.deterministic_line {
            "deterministic"
        } else {
            "fixed"
        }
    } else {
        "suite"
    };
    let avg_len = if cases.is_empty() {
        0
    } else {
        (cases.iter().map(|c| c.moves.len()).sum::<usize>() as f64 / cases.len() as f64).round()
            as usize
    };
    let metrics = json!({
        "refresh_update_eps": refresh_update_eps,
        "apply_update_eps": apply_update_eps,
        "chain_update_eps": chain_update_eps,
        "refresh_eval_eps": refresh_eval_eps,
        "apply_eval_eps": apply_eval_eps,
        "chain_eval_eps": chain_eval_eps,
        "seconds": args.seconds,
    });
    let line_meta = json!({
        "mode": line_mode,
        "cases": cases.len(),
        "line_len": avg_len,
    });
    let out = json!({
        "env": env_json,
        "line": line_meta,
        "metrics": metrics,
    });
    if let Some(ref p) = args.json {
        if p == "-" {
            println!("{}", serde_json::to_string_pretty(&out)?);
        } else {
            fs::create_dir_all(Path::new(p).parent().unwrap_or_else(|| Path::new(".")))?;
            fs::write(p, serde_json::to_string_pretty(&out)?)?;
        }
    }
    if let Some(ref p) = args.report {
        write_report_md(p, &out["env"], &out["line"], &out["metrics"])?;
    }

    Ok(())
}
