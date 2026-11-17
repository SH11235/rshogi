use anyhow::{Result, bail, ensure};
use rshogi_core::position::Position;
use rshogi_core::types::{EnteringKingRule, Move};
use rshogi_dl::cache::{CacheStats, CachedEvaluator};
use rshogi_dl::features::winrate_to_cp;
use rshogi_dl::onnx::{ModelEvaluator, ModelOptions, OnnxConfig};
use rshogi_dl::{Evaluator, InputBatchBuffer};
use rshogi_dl_usi::{Go, parse_position};
use rshogi_mcts::{Config, History, Search, SearchResult};
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

type Runtime = (CachedEvaluator<ModelEvaluator>, Search);
const NN_BATCH_OPTION: &str = "NNBatchSize type spin default 1 min 1 max 256";
struct Engine {
    position: Position,
    history: History,
    config: Config,
    model: PathBuf,
    device: i32,
    cuda_tf32: bool,
    cache_mib: usize,
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

fn prepare_evaluator<E: Evaluator>(
    mut evaluator: E,
    position: &Position,
    cache_mib: usize,
    batch_size: usize,
) -> Result<CachedEvaluator<E>> {
    let mut logits = vec![0.0; rshogi_dl::features::MAX_MOVE_LABEL_NUM];
    evaluator.evaluate(position, &mut logits)?;
    if batch_size == 1 {
        return CachedEvaluator::new(evaluator, cache_mib);
    }
    let format = evaluator
        .input_format()
        .ok_or_else(|| anyhow::anyhow!("batch inference requires an input format"))?;
    let mut inputs = InputBatchBuffer::new(format, batch_size)?;
    for _ in 0..batch_size {
        inputs.push(position)?;
    }
    let mut values = Vec::new();
    values.try_reserve_exact(batch_size)?;
    values.resize(batch_size, 0.0);
    let policy_len = batch_size
        .checked_mul(rshogi_dl::features::MAX_MOVE_LABEL_NUM)
        .ok_or_else(|| anyhow::anyhow!("batch policy size overflow"))?;
    let mut policies = Vec::new();
    policies.try_reserve_exact(policy_len)?;
    policies.resize(policy_len, 0.0);
    // cache の重複除去を通さず、最大 batch を実際に backend へ渡して検証する。
    evaluator.evaluate_encoded_batch(inputs.view(), &mut values, &mut policies)?;
    CachedEvaluator::with_batch_capacity(evaluator, cache_mib, batch_size)
}

fn batch_info(
    batch_size: usize,
    nn_calls: u64,
    nn_positions: u64,
    mcts_workspace: usize,
    cache_workspace: usize,
) -> Option<String> {
    (batch_size > 1).then(|| {
        let mean = if nn_calls == 0 { 0.0 } else { nn_positions as f64 / nn_calls as f64 };
        format!(
            "info string nn_positions {nn_positions} nn_batch_limit {batch_size} nn_batch_mean {mean:.3} mcts_batch_workspace_bytes {mcts_workspace} cache_batch_workspace_bytes {cache_workspace}"
        )
    })
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
            cuda_tf32: true,
            cache_mib: 0,
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
            let evaluator = ModelEvaluator::with_options(
                &self.model_config(),
                &ModelOptions {
                    cuda_tf32: self.cuda_tf32,
                    profiling: None,
                },
            )?;
            self.runtime = Some((
                prepare_evaluator(
                    evaluator,
                    &self.position,
                    self.cache_mib,
                    self.config.batch_size,
                )?,
                Search::new(self.config.clone())?,
            ));
        }
        Ok(())
    }
    fn model_config(&self) -> OnnxConfig {
        OnnxConfig {
            model_path: self.model.clone(),
            gpu_id: self.device,
            use_tensorrt: false,
            tensorrt_cache: None,
            batch_size: self.config.batch_size,
        }
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
            "CudaTF32" => {
                self.cuda_tf32 = value.parse()?;
            }
            "NNCacheMiB" => {
                let v = value.parse()?;
                ensure!(v <= 1024, "invalid NNCacheMiB");
                self.cache_mib = v;
            }
            "NNBatchSize" => {
                let v = value.parse()?;
                ensure!((1..=256).contains(&v), "invalid NNBatchSize");
                self.config.batch_size = v;
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
        let batch_size = self.config.batch_size;
        self.worker =
            Some(thread::Builder::new().name("mcts".into()).stack_size(8 * 1024 * 1024).spawn(
                move || {
                    evaluator.reset_stats();
                    let start = Instant::now();
                    let result =
                        search.run(&mut position, &mut history, &mut evaluator, limits, &stop);
                    if go.infinite {
                        while !stop.load(Ordering::Relaxed) {
                            thread::park_timeout(Duration::from_millis(5));
                        }
                    }
                    match result {
                        Ok(result) => print_result(
                            &result,
                            start.elapsed(),
                            evaluator.stats(),
                            evaluator.allocated_bytes(),
                            batch_info(
                                batch_size,
                                evaluator.stats().nn_calls,
                                evaluator.nn_positions(),
                                search.batch_workspace_bytes(),
                                evaluator.batch_allocated_bytes(),
                            ),
                        ),
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
                    "CudaTF32 type check default true",
                    "NNCacheMiB type spin default 0 min 0 max 1024",
                    NN_BATCH_OPTION,
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
            "usinewgame" | "gameover" => {
                self.join(true)?;
                if let Some((evaluator, _)) = &mut self.runtime {
                    evaluator.clear();
                }
            }
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

fn print_result(
    result: &SearchResult,
    elapsed: Duration,
    stats: CacheStats,
    cache_bytes: usize,
    batch: Option<String>,
) {
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
    output(&format!(
        "info string nn_calls {} cache_hits {} cache_bytes {cache_bytes}",
        stats.nn_calls, stats.hits
    ));
    if let Some(batch) = batch {
        output(&batch);
    }
    let best = if result.best_move == Move::NONE {
        "resign".into()
    } else {
        result.best_move.to_usi()
    };
    output(&format!("bestmove {best}"));
}

fn run_with_input(engine: &mut Engine, input: impl BufRead) -> Result<()> {
    let result = (|| -> Result<()> {
        for line in input.lines() {
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
        Ok(())
    })();
    let cleanup = engine.join(true);
    match (result, cleanup) {
        (Err(primary), Err(secondary)) => {
            Err(primary.context(format!("worker cleanup also failed: {secondary:#}")))
        }
        (Err(primary), _) => Err(primary),
        (Ok(()), cleanup) => cleanup,
    }
}

fn run() -> Result<()> {
    let mut engine = Engine::new()?;
    run_with_input(&mut engine, io::stdin().lock())
}
#[cfg(unix)]
fn exit_success_without_ort_teardown() -> Result<()> {
    // worker と session は run() で解放済み。ORT の process-global な終了処理だけを避ける。
    let stdout = io::stdout().flush();
    let stderr = io::stderr().flush();
    stdout.and(stderr)?;
    // SAFETY: worker/session 解放後、全出力を flush 済みで呼ぶ。_exit は残る
    // デストラクタを実行せずプロセスを終了し、新たなメモリ安全性の前提を要しない。
    unsafe { libc::_exit(0) }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
    #[cfg(unix)]
    if let Err(error) = exit_success_without_ort_teardown() {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rshogi_dl::{EncodedBatch, InputFormat};
    use std::sync::Mutex;

    struct Fake {
        calls: Arc<Mutex<Vec<usize>>>,
        fail_batch: bool,
    }

    impl Evaluator for Fake {
        fn input_format(&self) -> Option<InputFormat> {
            Some(InputFormat::StmTokensV1)
        }
        fn encoded_batch_capacity(&self) -> Option<usize> {
            Some(256)
        }
        fn evaluate(&mut self, _: &Position, logits: &mut [f32]) -> Result<f32> {
            self.calls.lock().unwrap().push(1);
            logits.fill(0.0);
            Ok(0.5)
        }
        fn evaluate_encoded_batch(
            &mut self,
            input: EncodedBatch<'_>,
            values: &mut [f32],
            policies: &mut [f32],
        ) -> Result<()> {
            self.calls.lock().unwrap().push(input.len());
            ensure!(!self.fail_batch, "unsupported runtime batch");
            values.fill(0.5);
            policies.fill(0.0);
            Ok(())
        }
    }

    fn install_waiting_worker(engine: &mut Engine) -> Result<(Arc<AtomicBool>, Arc<AtomicBool>)> {
        let observed_stop = Arc::new(AtomicBool::new(false));
        let finished = Arc::new(AtomicBool::new(false));
        let stop = engine.stop.clone();
        stop.store(false, Ordering::Relaxed);
        let observed = observed_stop.clone();
        let completed = finished.clone();
        let (started, ready) = std::sync::mpsc::channel();
        engine.worker = Some(thread::spawn(move || -> Runtime {
            started.send(()).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while !stop.load(Ordering::Relaxed) {
                assert!(Instant::now() < deadline, "worker did not receive stop");
                thread::park_timeout(Duration::from_millis(1));
            }
            observed.store(true, Ordering::Relaxed);
            completed.store(true, Ordering::Relaxed);
            panic!("fake search worker panicked");
        }));
        if let Err(error) = ready.recv_timeout(Duration::from_secs(5)) {
            let _ = engine.join(true);
            return Err(error.into());
        }
        Ok((observed_stop, finished))
    }

    #[test]
    fn input_error_stops_and_joins_worker_without_losing_its_source() -> Result<()> {
        let mut engine = Engine::new()?;
        let (observed_stop, finished) = install_waiting_worker(&mut engine)?;
        let result = run_with_input(&mut engine, io::Cursor::new([0xff, b'\n']));
        let joined = engine.worker.is_none();
        let observed = observed_stop.load(Ordering::Relaxed);
        let completed = finished.load(Ordering::Relaxed);
        if engine.worker.is_some() {
            let _ = engine.join(true);
        }
        let error = result.unwrap_err();
        assert!(joined && observed && completed);
        assert_eq!(error.downcast_ref::<io::Error>().unwrap().kind(), io::ErrorKind::InvalidData);
        assert!(format!("{error:#}").contains("search worker panicked"));
        Ok(())
    }

    #[test]
    fn input_error_without_worker_preserves_the_io_error() -> Result<()> {
        let mut engine = Engine::new()?;
        engine.stop.store(false, Ordering::Relaxed);
        let error = run_with_input(&mut engine, io::Cursor::new([0xff, b'\n'])).unwrap_err();
        assert_eq!(error.downcast_ref::<io::Error>().unwrap().kind(), io::ErrorKind::InvalidData);
        assert!(engine.stop.load(Ordering::Relaxed));
        assert!(engine.worker.is_none());
        Ok(())
    }

    #[test]
    fn eof_reports_worker_panic_when_input_succeeded() -> Result<()> {
        let mut engine = Engine::new()?;
        let (observed_stop, finished) = install_waiting_worker(&mut engine)?;
        let result = run_with_input(&mut engine, io::Cursor::new([]));
        let joined = engine.worker.is_none();
        let observed = observed_stop.load(Ordering::Relaxed);
        let completed = finished.load(Ordering::Relaxed);
        if engine.worker.is_some() {
            let _ = engine.join(true);
        }
        let error = result.unwrap_err();
        assert!(joined && observed && completed);
        assert_eq!(error.to_string(), "search worker panicked");
        assert!(error.downcast_ref::<io::Error>().is_none());
        Ok(())
    }

    #[test]
    fn recoverable_command_error_keeps_reading_and_eof_stops_cleanly() -> Result<()> {
        let mut engine = Engine::new()?;
        engine.stop.store(false, Ordering::Relaxed);
        run_with_input(
            &mut engine,
            io::Cursor::new(
                b"setoption name NNBatchSize value 0\nsetoption name NNBatchSize value 4\n",
            ),
        )?;
        assert_eq!(engine.config.batch_size, 4);
        assert!(engine.stop.load(Ordering::Relaxed));
        assert!(engine.worker.is_none());
        Ok(())
    }

    #[test]
    fn batch_option_validates_before_changing_search_and_model_capacity() -> Result<()> {
        let mut engine = Engine::new()?;
        assert_eq!(NN_BATCH_OPTION, "NNBatchSize type spin default 1 min 1 max 256");
        assert_eq!(engine.config.batch_size, 1);
        assert_eq!(engine.model_config().batch_size, 1);
        for batch in [256, 1, 4] {
            engine.stop.store(false, Ordering::Relaxed);
            engine.option(&format!("setoption name NNBatchSize value {batch}"))?;
            assert!(engine.stop.load(Ordering::Relaxed));
            assert!(engine.runtime.is_none());
            assert_eq!(engine.config.batch_size, batch);
            assert_eq!(engine.model_config().batch_size, batch);
        }
        for value in ["0", "257", "-1", "abc", ""] {
            assert!(engine.option(&format!("setoption name NNBatchSize value {value}")).is_err());
            assert_eq!(engine.config.batch_size, 4);
            assert_eq!(engine.model_config().batch_size, 4);
        }
        Ok(())
    }

    #[test]
    fn scalar_default_and_batched_warmup_prepare_the_matching_search_path() -> Result<()> {
        for batch_size in [1, 4] {
            for cache_mib in [0, 1] {
                let calls = Arc::new(Mutex::new(Vec::new()));
                let fake = Fake {
                    calls: calls.clone(),
                    fail_batch: false,
                };
                let (mut position, mut history) = parse_position("position startpos")?;
                let mut evaluator = prepare_evaluator(fake, &position, cache_mib, batch_size)?;
                assert_eq!(
                    *calls.lock().unwrap(),
                    if batch_size == 1 { vec![1] } else { vec![1, 4] }
                );
                assert_eq!(evaluator.stats(), CacheStats::default());
                assert_eq!(evaluator.nn_positions(), 0);
                assert_eq!(evaluator.batch_allocated_bytes() == 0, batch_size == 1);
                assert_eq!(
                    evaluator.encoded_batch_capacity(),
                    (batch_size > 1).then_some(batch_size)
                );
                let mut search = Search::new(Config {
                    batch_size,
                    tree_memory_mb: 1,
                    ..Config::default()
                })?;
                let result = search.run(
                    &mut position,
                    &mut history,
                    &mut evaluator,
                    rshogi_mcts::Limits {
                        visits: 9,
                        time: None,
                    },
                    &AtomicBool::new(false),
                )?;
                assert_eq!(result.visits, 9);
                assert_ne!(result.best_move, Move::NONE);
                assert_eq!(search.batch_workspace_bytes() == 0, batch_size == 1);
                assert!(evaluator.stats().nn_calls > 0);
                evaluator.clear();
                assert_eq!(evaluator.stats(), CacheStats::default());
                assert_eq!(evaluator.nn_positions(), 0);
            }
        }
        Ok(())
    }

    #[test]
    fn full_batch_warmup_error_is_returned_without_a_scalar_retry() -> Result<()> {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let fake = Fake {
            calls: calls.clone(),
            fail_batch: true,
        };
        let (position, _) = parse_position("position startpos")?;
        let result = prepare_evaluator(fake, &position, 1, 4);
        assert!(result.is_err());
        assert_eq!(*calls.lock().unwrap(), vec![1, 4]);
        Ok(())
    }

    #[test]
    fn batch_diagnostics_are_separate_and_absent_for_scalar_default() {
        assert!(batch_info(1, 3, 3, 0, 0).is_none());
        assert_eq!(
            batch_info(4, 3, 7, 100, 200).unwrap(),
            "info string nn_positions 7 nn_batch_limit 4 nn_batch_mean 2.333 mcts_batch_workspace_bytes 100 cache_batch_workspace_bytes 200"
        );
        assert!(batch_info(4, 0, 0, 0, 200).unwrap().contains("nn_batch_mean 0.000"));
    }

    #[test]
    fn options_and_non_loading_lifecycle_commands_keep_the_batch_setting() -> Result<()> {
        let mut engine = Engine::new()?;
        engine.command("setoption name NNBatchSize value 8")?;
        for command in [
            "usi",
            "position startpos moves 7g7f",
            "stop",
            "usinewgame",
            "gameover",
        ] {
            assert!(engine.command(command)?);
            assert_eq!(engine.config.batch_size, 8);
            assert!(engine.worker.is_none());
            assert!(engine.runtime.is_none());
        }
        assert!(!engine.command("quit")?);
        assert_eq!(engine.model_config().batch_size, 8);
        Ok(())
    }
}
