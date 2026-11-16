use anyhow::{Result, bail, ensure};
use rshogi_core::position::Position;
use rshogi_core::types::{EnteringKingRule, Move};
use rshogi_dl::features::winrate_to_cp;
use rshogi_dl::onnx::{OnnxConfig, OnnxEvaluator};
use rshogi_dl_usi::{Go, parse_position};
use rshogi_mcts::{Config, History, Search, SearchResult};
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

type Runtime = (OnnxEvaluator, Search);
struct Engine {
    position: Position,
    history: History,
    config: Config,
    model: PathBuf,
    device: i32,
    overhead: u64,
    runtime: Option<Runtime>,
    worker: Option<JoinHandle<Runtime>>,
    stop: Arc<AtomicBool>,
}

fn output(line: &str) {
    let stdout = io::stdout();
    let mut lock = stdout.lock();
    let _ = writeln!(lock, "{line}");
    let _ = lock.flush();
}

impl Engine {
    fn new() -> Result<Self> {
        let (position, history) = parse_position("position startpos")?;
        Ok(Self {
            position,
            history,
            config: Config::default(),
            model: "model.onnx".into(),
            device: 0,
            overhead: 50,
            runtime: None,
            worker: None,
            stop: Arc::new(AtomicBool::new(false)),
        })
    }
    fn join(&mut self, stop: bool) -> Result<()> {
        if stop {
            self.stop.store(true, Ordering::Relaxed);
        }
        if let Some(worker) = self.worker.take() {
            self.runtime =
                Some(worker.join().map_err(|_| anyhow::anyhow!("search worker panicked"))?);
        }
        Ok(())
    }
    fn initialize(&mut self) -> Result<()> {
        self.join(true)?;
        if self.runtime.is_none() {
            let mut evaluator = OnnxEvaluator::new(&OnnxConfig {
                model_path: self.model.clone(),
                gpu_id: self.device,
                use_tensorrt: false,
                tensorrt_cache: None,
                batch_size: 1,
            })?;
            let mut logits = vec![0.0; rshogi_dl::features::MAX_MOVE_LABEL_NUM];
            rshogi_dl::Evaluator::evaluate(&mut evaluator, &self.position, &mut logits)?;
            self.runtime = Some((evaluator, Search::new(self.config.clone())?));
        }
        Ok(())
    }
    fn option(&mut self, command: &str) -> Result<()> {
        self.join(true)?;
        let text = command
            .strip_prefix("setoption name ")
            .ok_or_else(|| anyhow::anyhow!("expected option name"))?;
        let (name, value) = text.split_once(" value ").unwrap_or((text, ""));
        match name {
            "ModelFile" => {
                ensure!(!value.is_empty(), "empty ModelFile");
                self.model = value.into();
            }
            "Device" => {
                let v: i32 = value.parse()?;
                ensure!((-1..=15).contains(&v), "invalid Device");
                self.device = v;
            }
            "TreeMemoryMB" => {
                let v = value.parse()?;
                ensure!((1..=1024).contains(&v), "invalid TreeMemoryMB");
                self.config.tree_memory_mb = v;
            }
            "CpuctMilli" => {
                let v: u32 = value.parse()?;
                ensure!((1..=100000).contains(&v), "invalid CpuctMilli");
                self.config.cpuct = v as f32 / 1000.0;
            }
            "PolicyTemperatureMilli" => {
                let v: u32 = value.parse()?;
                ensure!((1..=100000).contains(&v), "invalid temperature");
                self.config.temperature = v as f32 / 1000.0;
            }
            "MaxGamePly" => {
                let v = value.parse()?;
                ensure!((1..=100000).contains(&v), "invalid MaxGamePly");
                self.config.max_game_ply = v;
            }
            "MoveOverhead" => {
                let v = value.parse()?;
                ensure!(v <= 10000, "invalid MoveOverhead");
                self.overhead = v;
            }
            "EnteringKingRule" => {
                self.config.entering_king_rule = EnteringKingRule::from_usi(value)
                    .ok_or_else(|| anyhow::anyhow!("invalid EnteringKingRule"))?
            }
            "Threads" => ensure!(value == "1", "only one search worker is supported"),
            _ => bail!("unknown option {name}"),
        }
        self.runtime = None;
        Ok(())
    }
    fn go(&mut self, command: &str) -> Result<()> {
        self.join(true)?;
        let go = Go::parse(command)?;
        self.initialize()?;
        let limits = go.limits(self.position.side_to_move(), self.overhead);
        let mut position = self.position.clone();
        let mut history = self.history.clone();
        let (mut evaluator, mut search) = self.runtime.take().expect("runtime was initialized");
        self.stop = Arc::new(AtomicBool::new(false));
        let stop = self.stop.clone();
        self.worker =
            Some(thread::Builder::new().name("mcts".into()).stack_size(8 * 1024 * 1024).spawn(
                move || {
                    let start = Instant::now();
                    let result =
                        search.run(&mut position, &mut history, &mut evaluator, limits, &stop);
                    if go.infinite {
                        while !stop.load(Ordering::Relaxed) {
                            thread::park_timeout(Duration::from_millis(5));
                        }
                    }
                    match result {
                        Ok(result) => print_result(&result, start.elapsed()),
                        Err(error) => {
                            output(&format!("info string Error: search failed: {error}"));
                            output("bestmove resign");
                        }
                    }
                    (evaluator, search)
                },
            )?);
        Ok(())
    }
    fn command(&mut self, command: &str) -> Result<bool> {
        let command = command.trim();
        match command.split_whitespace().next().unwrap_or("") {
            "usi" => {
                output("id name rshogi DL");
                output("id author rshogi contributors");
                for option in [
                    "ModelFile type string default model.onnx",
                    "Device type spin default 0 min -1 max 15",
                    "TreeMemoryMB type spin default 16 min 1 max 1024",
                    "CpuctMilli type spin default 1500 min 1 max 100000",
                    "PolicyTemperatureMilli type spin default 1000 min 1 max 100000",
                    "MaxGamePly type spin default 512 min 1 max 100000",
                    "MoveOverhead type spin default 50 min 0 max 10000",
                    "EnteringKingRule type combo default CSARule27 var NoEnteringKing var CSARule24 var CSARule24H var CSARule27 var CSARule27H var TryRule",
                    "Threads type spin default 1 min 1 max 1",
                ] {
                    output(&format!("option name {option}"));
                }
                output("usiok");
            }
            "isready" => {
                if self.worker.is_none() {
                    self.initialize()?;
                }
                output("readyok");
            }
            "setoption" => self.option(command)?,
            "position" => {
                self.join(true)?;
                let (position, history) = parse_position(command)?;
                self.position = position;
                self.history = history;
            }
            "go" => self.go(command)?,
            "stop" => self.join(true)?,
            "usinewgame" | "gameover" => self.join(true)?,
            "quit" => {
                self.join(true)?;
                return Ok(false);
            }
            "" => {}
            other => bail!("unsupported command {other}"),
        }
        Ok(true)
    }
}

fn print_result(result: &SearchResult, elapsed: Duration) {
    let millis = elapsed.as_millis().max(1);
    let cp = winrate_to_cp(result.value, 600.0);
    let pv = result.pv.iter().map(|m| m.to_usi()).collect::<Vec<_>>().join(" ");
    output(&format!(
        "info depth {} score cp {cp} nodes {} time {millis} nps {} hashfull {} pv {pv}",
        result.pv.len(),
        result.visits,
        (u128::from(result.visits) * 1000 / millis).min(u128::from(u64::MAX)),
        result.hashfull
    ));
    output(&format!(
        "info string evaluations {} memory_full {} outcome {:?}",
        result.evaluations, result.memory_full, result.outcome
    ));
    let best = if result.best_move == Move::NONE {
        "resign".into()
    } else {
        result.best_move.to_usi()
    };
    output(&format!("bestmove {best}"));
}

fn run() -> Result<()> {
    let mut engine = Engine::new()?;
    for line in io::stdin().lock().lines() {
        let line = line?;
        match engine.command(&line) {
            Ok(false) => return Ok(()),
            Ok(true) => {}
            Err(error) => {
                output(&format!("info string Error: {error}"));
                if line.split_whitespace().next() == Some("go") {
                    output("bestmove resign");
                }
            }
        }
    }
    engine.join(true)
}
fn main() {
    if let Err(error) = run() {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}
