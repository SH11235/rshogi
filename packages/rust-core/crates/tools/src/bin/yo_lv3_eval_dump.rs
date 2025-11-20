use anyhow::{anyhow, Context, Result};
use clap::Parser;
use engine_core::evaluation::yo_material::evaluate_yo_material_lv3;
use engine_core::Position;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::Duration;

/// Simple USI client for YaneuraOu MATERIAL Lv.3 that dumps static eval (compute_eval) vs Rust yo_lv3.
#[derive(Parser, Debug)]
#[command(
    name = "yo_lv3_eval_dump",
    about = "Compare YaneuraOu MATERIAL Lv.3 static eval and Rust evaluate_yo_material_lv3 for given SFEN(s)"
)]
struct Args {
    /// SFEN 文字列（複数指定可）。先頭に `sfen` が付いていてもよい。
    #[arg(long = "sfen")]
    sfens: Vec<String>,

    /// Path to YaneuraOu MATERIAL Lv3 engine (USI).
    #[arg(long = "engine-path", default_value = "memo/yaneuraou-material-lv3.sh")]
    engine_path: String,
}

struct UsiEngine {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl UsiEngine {
    fn spawn(path: &str) -> Result<Self> {
        let mut child = Command::new(path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("failed to start engine: {path}"))?;

        let stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;
        let stdout = BufReader::new(stdout);

        Ok(Self { child, stdin, stdout })
    }

    fn write_line(&mut self, line: &str) -> Result<()> {
        self.stdin
            .write_all(line.as_bytes())
            .context("failed to write to engine stdin")?;
        self.stdin
            .write_all(b"\n")
            .context("failed to write newline to engine stdin")?;
        self.stdin.flush().ok();
        Ok(())
    }

    fn read_line_with_timeout(&mut self, timeout: Duration) -> Result<Option<String>> {
        use std::io::Read;
        use std::time::Instant;

        let start = Instant::now();

        loop {
            if self.stdout.fill_buf()?.is_empty() && start.elapsed() >= timeout {
                return Ok(None);
            }
            let mut buf = String::new();
            let read = self.stdout.read_line(&mut buf)?;
            if read == 0 {
                return Ok(None);
            }
            if buf.is_empty() {
                continue;
            }
            return Ok(Some(buf.trim_end_matches(&['\r', '\n'][..]).to_string()));
        }
    }
}

impl Drop for UsiEngine {
    fn drop(&mut self) {
        let _ = self.write_line("quit");
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn parse_cp_from_info(line: &str) -> Option<i32> {
    if !line.starts_with("info ") {
        return None;
    }
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let mut depth0 = false;
    let mut i = 1;
    while i < tokens.len() {
        match tokens[i] {
            "depth" if i + 1 < tokens.len() => {
                if let Ok(d) = tokens[i + 1].parse::<i32>() {
                    depth0 = d == 0;
                }
                i += 2;
            }
            "score" if i + 2 < tokens.len() => {
                if tokens[i + 1] == "cp" {
                    if let Ok(v) = tokens[i + 2].parse::<i32>() {
                        if depth0 {
                            return Some(v);
                        }
                    }
                }
                i += 3;
            }
            _ => {
                i += 1;
            }
        }
    }
    None
}

fn main() -> Result<()> {
    let args = Args::parse();
    if args.sfens.is_empty() {
        return Err(anyhow!("no --sfen arguments provided"));
    }

    let mut engine = UsiEngine::spawn(&args.engine_path)?;

    // USI handshake
    engine.write_line("usi")?;
    loop {
        if let Some(line) = engine.read_line_with_timeout(Duration::from_millis(1000))? {
            if line.contains("usiok") {
                break;
            }
        }
    }
    engine.write_line("setoption name Threads value 1")?;
    engine.write_line("setoption name USI_Hash value 16")?;
    engine.write_line("isready")?;
    loop {
        if let Some(line) = engine.read_line_with_timeout(Duration::from_millis(1000))? {
            if line.contains("readyok") {
                break;
            }
        }
    }

    for (idx, raw) in args.sfens.iter().enumerate() {
        let sfen = raw.trim();
        let sfen_core = sfen
            .strip_prefix("sfen ")
            .unwrap_or_else(|| sfen.strip_prefix("position sfen ").unwrap_or(sfen));

        let pos =
            Position::from_sfen(sfen_core).map_err(|e| anyhow!("invalid SFEN: {sfen_core}: {e}"))?;
        let rust_lv3 = evaluate_yo_material_lv3(&pos);

        // position + go depth 0
        engine.write_line(&format!("position sfen {sfen_core}"))?;
        engine.write_line("go depth 0")?;

        let mut yo_cp: Option<i32> = None;
        loop {
            if let Some(line) = engine.read_line_with_timeout(Duration::from_millis(1000))? {
                if let Some(cp) = parse_cp_from_info(&line) {
                    yo_cp = Some(cp);
                }
                if line.starts_with("bestmove") {
                    break;
                }
            } else {
                break;
            }
        }

        println!("SFEN[{}]: {}", idx, sfen_core);
        println!("  rust_yo_lv3_cp={}", rust_lv3);
        match yo_cp {
            Some(cp) => println!("  yo_material_lv3_cp={}", cp),
            None => println!("  yo_material_lv3_cp=<none depth0>"),
        }
        println!();
    }

    Ok(())
}

