use anyhow::{anyhow, Context, Result};
use clap::Parser;
use serde::Serialize;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Parser, Debug)]
#[command(
    name = "usi-gauntlet",
    about = "USI vs USI gauntlet with per-side init scripts"
)]
struct Cli {
    /// Path to engine binary (USI)
    #[arg(long, default_value = "target/release/engine-usi")]
    engine: String,
    /// Optional distinct engine binary for baseline side
    #[arg(long, value_name = "FILE")]
    base_engine: Option<String>,
    /// Optional distinct engine binary for candidate side
    #[arg(long, value_name = "FILE")]
    cand_engine: Option<String>,
    /// Init script for baseline (lines: setoption..., Threads/MultiPV etc.)
    #[arg(long, value_name = "FILE")]
    base_init: PathBuf,
    /// Init script for candidate (safety presetなど)
    #[arg(long, value_name = "FILE")]
    cand_init: PathBuf,
    /// Opening book file (lines starting with 'sfen ')
    #[arg(long, value_name = "FILE")]
    book: PathBuf,
    /// Total games (must be even)
    #[arg(long, default_value_t = 400)]
    games: usize,
    /// Byoyomi increment per move in milliseconds (default: 100ms)
    #[arg(long, default_value_t = 100)]
    byoyomi_ms: u64,
    /// Max plies before declaring a draw (cap)
    #[arg(long, default_value_t = 256)]
    max_plies: u32,
    /// Optional extra env for baseline engine (KEY=VAL, repeatable)
    #[arg(long = "base-env", value_name = "K=V")]
    base_env: Vec<String>,
    /// Optional extra env for candidate engine (KEY=VAL, repeatable)
    #[arg(long = "cand-env", value_name = "K=V")]
    cand_env: Vec<String>,
    /// Optional EvalFile path (if present, passed to both engines)
    #[arg(long)]
    eval_file: Option<PathBuf>,
    /// Enable adjudication (resign) when eval trend is clearly losing
    #[arg(long)]
    adj_enable: bool,
    /// Adjudication minimum ply before checking (full plies)
    #[arg(long, default_value_t = 80)]
    adj_ply_min: u32,
    /// Adjudication sliding window size (moves by the same engine)
    #[arg(long, default_value_t = 8)]
    adj_window: usize,
    /// Adjudication threshold in centipawns (avg <= -threshold triggers resign)
    #[arg(long, default_value_t = 600)]
    adj_threshold: i32,
    /// Also adjudicate immediately on explicit mate info (score mate +/-N)
    #[arg(long, default_value_t = true)]
    adj_mate_enable: bool,
    /// Output directory for results
    #[arg(long, default_value = "runs/gauntlet_usi/auto")]
    out: PathBuf,
    /// Number of parallel matches (each worker holds two persistent engines)
    #[arg(long, default_value_t = 1)]
    concurrency: usize,
    /// Override engine Threads setoption (sent for both engines after usiok)
    #[arg(long)]
    engine_threads: Option<u32>,
    /// Override USI_Hash (MB) setoption (sent for both engines after usiok)
    #[arg(long)]
    hash_mb: Option<usize>,
}

struct USIProc {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<String>,
    last_eval: Option<LastEval>,
    // lightweight diagnostics counters parsed from stdout
    cnt_oob_verify_fail: usize,
    cnt_oob_switch: usize,
    cnt_finalize_event: usize,
}

impl Drop for USIProc {
    fn drop(&mut self) {
        // Try graceful shutdown first
        let _ = self.stdin.write_all(b"quit\n");
        let _ = self.stdin.flush();
        // If the child is still alive, attempt to kill to avoid zombie processes
        match self.child.try_wait() {
            Ok(Some(_status)) => {
                // already exited
            }
            _ => {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum LastEval {
    Cp(i32),
    Mate(i32),
}

impl USIProc {
    fn spawn(path: &str, log_path: &PathBuf, envs: &[(String, String)]) -> Result<Self> {
        let mut cmd = Command::new(path);
        for (k, v) in envs.iter() {
            cmd.env(k, v);
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(fs::OpenOptions::new().create(true).append(true).open(log_path)?)
            .spawn()
            .with_context(|| format!("failed to start engine: {}", path))?;
        let stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;
        let (tx, rx) = mpsc::channel::<String>();
        thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                let _ = tx.send(line);
            }
        });
        Ok(Self {
            child,
            stdin,
            rx,
            last_eval: None,
            cnt_oob_verify_fail: 0,
            cnt_oob_switch: 0,
            cnt_finalize_event: 0,
        })
    }

    fn send<S: AsRef<str>>(&mut self, s: S) -> Result<()> {
        let line = s.as_ref();
        self.stdin
            .write_all(format!("{}\n", line).as_bytes())
            .context("write_all failed")?;
        self.stdin.flush().ok();
        Ok(())
    }

    fn wait_for(&self, pred: impl Fn(&str) -> bool, timeout_ms: u64) -> Result<()> {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        while Instant::now() < deadline {
            if let Ok(line) = self.rx.recv_timeout(Duration::from_millis(20)) {
                if pred(&line) {
                    return Ok(());
                }
            }
        }
        Err(anyhow!("timeout waiting for condition"))
    }

    fn recv_bestmove(&mut self, timeout_ms: u64) -> Result<(String, Option<LastEval>)> {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms + 3000);
        while Instant::now() < deadline {
            if let Ok(line) = self.rx.recv_timeout(Duration::from_millis(20)) {
                // count simple diagnostics tags (engine stdout)
                if line.contains("oob_verify_fail") {
                    self.cnt_oob_verify_fail += 1;
                }
                if line.contains("oob_switch") {
                    self.cnt_oob_switch += 1;
                }
                if line.contains("finalize_event") {
                    self.cnt_finalize_event += 1;
                }
                if let Some(idx) = line.find("score cp ") {
                    // parse integer after "score cp "
                    let s = &line[idx + 9..];
                    let mut num = String::new();
                    for ch in s.chars() {
                        if ch == '-' || ch == '+' || ch.is_ascii_digit() {
                            num.push(ch);
                        } else {
                            break;
                        }
                    }
                    if let Ok(v) = num.parse::<i32>() {
                        self.last_eval = Some(LastEval::Cp(v));
                    }
                }
                if let Some(idx) = line.find("score mate ") {
                    let s = &line[idx + 11..];
                    let mut num = String::new();
                    for ch in s.chars() {
                        if ch == '-' || ch == '+' || ch.is_ascii_digit() {
                            num.push(ch);
                        } else {
                            break;
                        }
                    }
                    if let Ok(v) = num.parse::<i32>() {
                        self.last_eval = Some(LastEval::Mate(v));
                    }
                }
                if let Some(m) = line.strip_prefix("bestmove ") {
                    let mv = m.trim().to_string();
                    return Ok((mv, self.last_eval.take()));
                }
            }
        }
        Ok(("resign".to_string(), self.last_eval.take()))
    }

    fn take_counters(&mut self) -> (usize, usize, usize) {
        let snap = (self.cnt_oob_verify_fail, self.cnt_oob_switch, self.cnt_finalize_event);
        self.cnt_oob_verify_fail = 0;
        self.cnt_oob_switch = 0;
        self.cnt_finalize_event = 0;
        snap
    }
}

#[derive(Serialize, Default)]
struct Summary {
    games: usize,
    cand_wins: usize,
    base_wins: usize,
    draws: usize,
    score_rate: f64,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    fs::create_dir_all(&cli.out)?;
    let book_text = fs::read_to_string(&cli.book)
        .with_context(|| format!("failed to read book: {}", cli.book.display()))?;
    let mut openings: Vec<String> = Vec::new();
    for line in book_text.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if let Some(rest) = t.strip_prefix("sfen ") {
            openings.push(rest.trim().to_string());
        }
    }
    if openings.is_empty() {
        return Err(anyhow!("book has no 'sfen ' lines"));
    }
    if cli.games % 2 != 0 {
        return Err(anyhow!("--games must be even"));
    }

    fn parse_env(v: &[String]) -> Vec<(String, String)> {
        v.iter()
            .filter_map(|s| s.split_once('='))
            .map(|(k, val)| (k.to_string(), val.to_string()))
            .collect()
    }
    // simple progress log
    let mut progress =
        fs::OpenOptions::new().create(true).append(true).open(cli.out.join("run.log"))?;
    writeln!(progress, "spawn engines at {}", chrono::Utc::now().to_rfc3339())?;
    progress.flush().ok();
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    let games_csv_path = cli.out.join("games.csv");
    let csv_w = Arc::new(Mutex::new(
        fs::OpenOptions::new().create(true).append(true).open(&games_csv_path)?,
    ));
    if games_csv_path.metadata().map(|m| m.len()).unwrap_or(0) == 0 {
        let mut w = csv_w.lock().unwrap();
        writeln!(&mut *w, "game_index,open_index,result,plies")?;
        w.flush().ok();
    }
    let cand_wins = Arc::new(AtomicUsize::new(0));
    let base_wins = Arc::new(AtomicUsize::new(0));
    let draws = Arc::new(AtomicUsize::new(0));
    let games_done = Arc::new(AtomicUsize::new(0));
    // global counters for stdout-diagnostics
    let g_oob_verify_fail = Arc::new(AtomicUsize::new(0));
    let g_oob_switch = Arc::new(AtomicUsize::new(0));
    let g_finalize_event = Arc::new(AtomicUsize::new(0));

    let chunks = {
        let c = cli.concurrency.max(1);
        let mut v = Vec::new();
        let per = cli.games.div_ceil(c);
        let mut start = 0usize;
        while start < cli.games {
            let end = (start + per).min(cli.games);
            v.push((start, end));
            start = end;
        }
        v
    };

    let openings = Arc::new(openings);
    let out_dir = cli.out.clone();
    let base_engine_path = cli.base_engine.clone().unwrap_or_else(|| cli.engine.clone());
    let cand_engine_path = cli.cand_engine.clone().unwrap_or_else(|| cli.engine.clone());
    let base_init = cli.base_init.clone();
    let cand_init = cli.cand_init.clone();
    let eval_file = cli.eval_file.clone();
    let base_env = parse_env(&cli.base_env);
    let cand_env = parse_env(&cli.cand_env);
    let byoyomi = cli.byoyomi_ms;
    let max_plies = cli.max_plies;
    let adj_enable = cli.adj_enable;
    let adj_window = cli.adj_window;
    let adj_ply_min = cli.adj_ply_min;
    let adj_threshold = cli.adj_threshold;
    let adj_mate_enable = cli.adj_mate_enable;
    let engine_threads = cli.engine_threads;
    let hash_mb = cli.hash_mb;

    let mut handles = Vec::new();
    for (wid, (g0, g1)) in chunks.into_iter().enumerate() {
        let openings = openings.clone();
        let csv_w = csv_w.clone();
        let cand_wins = cand_wins.clone();
        let base_wins = base_wins.clone();
        let draws = draws.clone();
        let games_done = games_done.clone();
        let out_dir = out_dir.clone();
        let base_engine_path = base_engine_path.clone();
        let cand_engine_path = cand_engine_path.clone();
        let base_init = base_init.clone();
        let cand_init = cand_init.clone();
        let eval_file = eval_file.clone();
        let base_env = base_env.clone();
        let cand_env = cand_env.clone();
        // clone global diagnostics counters into this worker
        let w_ov = g_oob_verify_fail.clone();
        let w_sw = g_oob_switch.clone();
        let w_fn = g_finalize_event.clone();
        let progress_path = out_dir.join(format!("run_worker_{}.log", wid));
        let handle = thread::spawn(move || -> Result<()> {
            let mut progress =
                fs::OpenOptions::new().create(true).append(true).open(progress_path)?;
            writeln!(progress, "worker {}: spawn engines", wid)?;
            let mut base = USIProc::spawn(
                &base_engine_path,
                &out_dir.join(format!("base.{}.stderr.log", wid)),
                &base_env,
            )?;
            let mut cand = USIProc::spawn(
                &cand_engine_path,
                &out_dir.join(format!("cand.{}.stderr.log", wid)),
                &cand_env,
            )?;
            for (proc, init) in [(&mut base, &base_init), (&mut cand, &cand_init)] {
                proc.send("usi")?;
                proc.wait_for(|l| l.contains("usiok"), 7000)?;
                if let Some(eval) = eval_file.as_ref() {
                    if eval.exists() {
                        proc.send(format!("setoption name EvalFile value {}", eval.display()))?;
                    }
                }
                if let Some(th) = engine_threads {
                    proc.send(format!("setoption name Threads value {}", th))?;
                }
                if let Some(hm) = hash_mb {
                    proc.send(format!("setoption name USI_Hash value {}", hm))?;
                }
                let text = fs::read_to_string(init)?;
                for line in text.lines() {
                    let t = line.trim();
                    if t.is_empty() || t.starts_with('#') {
                        continue;
                    }
                    proc.send(t)?;
                }
                proc.send("isready")?;
                proc.wait_for(|l| l.contains("readyok"), 7000)?;
                proc.send("usinewgame")?;
            }
            use std::collections::VecDeque;
            for g in g0..g1 {
                let open_idx = (g / 2) % openings.len();
                let sfen = &openings[open_idx];
                let mut moves: Vec<String> = Vec::new();
                let mut plies: u32 = 0;
                let cand_black = g % 2 == 1;
                let pos_cmd = |moves: &Vec<String>| {
                    if moves.is_empty() {
                        format!("position sfen {}", sfen)
                    } else {
                        format!("position sfen {} moves {}", sfen, moves.join(" "))
                    }
                };
                base.send(pos_cmd(&moves))?;
                cand.send(pos_cmd(&moves))?;
                let mut eval_base: VecDeque<i32> = VecDeque::with_capacity(adj_window);
                let mut eval_cand: VecDeque<i32> = VecDeque::with_capacity(adj_window);
                let result = loop {
                    if plies >= max_plies {
                        break "draw";
                    }
                    let stm_black = plies.is_multiple_of(2);
                    let to_move_is_cand = (cand_black && stm_black) || (!cand_black && !stm_black);
                    let to_move = if to_move_is_cand {
                        &mut cand
                    } else {
                        &mut base
                    };
                    to_move.send(format!("go byoyomi {}", byoyomi))?;
                    let (bm, eval) = to_move.recv_bestmove(byoyomi + 200)?;
                    // accumulate lightweight counters from the side that just searched
                    let (ovf, swc, fin) = to_move.take_counters();
                    w_ov.fetch_add(ovf, Ordering::Relaxed);
                    w_sw.fetch_add(swc, Ordering::Relaxed);
                    w_fn.fetch_add(fin, Ordering::Relaxed);
                    if adj_enable {
                        match eval {
                            Some(LastEval::Mate(m)) if adj_mate_enable => {
                                if m > 0 {
                                    if to_move_is_cand {
                                        cand_wins.fetch_add(1, Ordering::Relaxed);
                                    } else {
                                        base_wins.fetch_add(1, Ordering::Relaxed);
                                    }
                                    break if to_move_is_cand {
                                        "cand_win"
                                    } else {
                                        "base_win"
                                    };
                                }
                                if m < 0 {
                                    if to_move_is_cand {
                                        base_wins.fetch_add(1, Ordering::Relaxed);
                                    } else {
                                        cand_wins.fetch_add(1, Ordering::Relaxed);
                                    }
                                    break if to_move_is_cand {
                                        "base_win"
                                    } else {
                                        "cand_win"
                                    };
                                }
                            }
                            Some(LastEval::Cp(v)) => {
                                let dq = if to_move_is_cand {
                                    &mut eval_cand
                                } else {
                                    &mut eval_base
                                };
                                dq.push_back(v);
                                if dq.len() > adj_window {
                                    dq.pop_front();
                                }
                                if plies >= adj_ply_min && dq.len() == adj_window {
                                    let avg: f64 = dq.iter().map(|&x| x as f64).sum::<f64>()
                                        / (adj_window as f64);
                                    if avg <= -(adj_threshold as f64) {
                                        if to_move_is_cand {
                                            base_wins.fetch_add(1, Ordering::Relaxed);
                                            break "base_win";
                                        } else {
                                            cand_wins.fetch_add(1, Ordering::Relaxed);
                                            break "cand_win";
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    let mv = bm.split_whitespace().next().unwrap_or("resign");
                    if mv == "resign" || mv == "none" {
                        if to_move_is_cand {
                            base_wins.fetch_add(1, Ordering::Relaxed);
                            break "base_win";
                        } else {
                            cand_wins.fetch_add(1, Ordering::Relaxed);
                            break "cand_win";
                        }
                    }
                    moves.push(mv.to_string());
                    plies += 1;
                    let cmd = pos_cmd(&moves);
                    base.send(&cmd)?;
                    cand.send(&cmd)?;
                };
                if result == "draw" {
                    draws.fetch_add(1, Ordering::Relaxed);
                }
                games_done.fetch_add(1, Ordering::Relaxed);
                let mut w = csv_w.lock().unwrap();
                writeln!(&mut *w, "{},{},{},{}", g, open_idx, result, plies)?;
                w.flush().ok();
            }
            Ok(())
        });
        handles.push(handle);
    }
    // join threads
    let mut any_err: Option<anyhow::Error> = None;
    for h in handles {
        if let Err(e) = h.join().unwrap_or(Ok(())) {
            any_err = Some(e);
        }
    }
    if let Some(e) = any_err {
        return Err(e);
    }
    // Final summary
    let games = games_done.load(Ordering::Relaxed);
    let cw = cand_wins.load(Ordering::Relaxed);
    let bw = base_wins.load(Ordering::Relaxed);
    let dr = draws.load(Ordering::Relaxed);
    let score_rate = if games > 0 {
        (cw as f64 + 0.5 * (dr as f64)) / (games as f64)
    } else {
        0.5
    };
    let ovf = g_oob_verify_fail.load(Ordering::Relaxed);
    let swc = g_oob_switch.load(Ordering::Relaxed);
    let fin = g_finalize_event.load(Ordering::Relaxed);
    fs::write(
        cli.out.join("summary.txt"),
        format!(
            "USI Gauntlet (byoyomi={}ms, games={})\n- Candidate wins: {}\n- Baseline wins:  {}\n- Draws:          {}\n- Score rate:     {:.3}\n- oob_verify_fail: {}\n- oob_switch:     {}\n- finalize_event: {}\n",
            cli.byoyomi_ms, games, cw, bw, dr, score_rate, ovf, swc, fin
        ),
    )?;
    fs::write(
        cli.out.join("result.json"),
        serde_json::to_string_pretty(&Summary {
            games,
            cand_wins: cw,
            base_wins: bw,
            draws: dr,
            score_rate,
        })?,
    )?;
    Ok(())
}
