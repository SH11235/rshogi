//! search-only A/B ベンチマークツール
//!
//! USI エンジンを fresh process で起動し、`readyok` 後に `perf stat --control`
//! を `enable`、`bestmove` 後に `disable` することで、探索区間だけを測定する。
//! 起動・NNUEロード・`isready` のノイズを外しつつ、before/after の差を比較したい
//! ときの基準計測用ツール。

#[cfg(not(unix))]
fn main() {
    eprintln!("search_only_ab は Linux 専用ツールです（perf stat --control を使用）");
    std::process::exit(1);
}

#[cfg(unix)]
mod unix_main {

    use std::collections::HashSet;
    use std::fs::File;
    use std::io::{BufRead, BufReader, BufWriter, Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd, RawFd};
    use std::path::{Path, PathBuf};
    use std::process::{Child, ChildStdin, Command, Stdio};
    use std::sync::mpsc::{self, Receiver};
    use std::thread;
    use std::time::{Duration, Instant};

    use anyhow::{Context, Result, anyhow, bail};
    use clap::Parser;
    use serde::Serialize;

    use tools::{SystemInfo, collect_system_info};

    const PERF_CTL_FD: RawFd = 20;
    const PERF_ACK_FD: RawFd = 21;
    const DEFAULT_PERF_EVENTS: &str = "cycles,instructions,branches,branch-misses,cache-references,cache-misses,L1-dcache-load-misses";
    const READY_TIMEOUT: Duration = Duration::from_secs(120);
    const ACK_TIMEOUT: Duration = Duration::from_secs(30);
    const QUIT_TIMEOUT: Duration = Duration::from_secs(5);
    const POLL_INTERVAL: Duration = Duration::from_millis(10);

    #[derive(Parser, Debug, Clone)]
    #[command(
        name = "search_only_ab",
        version,
        about = "perf --control を使った search-only A/B ベンチマーク"
    )]
    struct Cli {
        /// baseline エンジンのパス
        #[arg(long)]
        baseline: PathBuf,

        /// candidate エンジンのパス
        #[arg(long)]
        candidate: PathBuf,

        /// 局面ファイル
        ///
        /// 1行ごとに以下のいずれかを受け付ける:
        /// - `position ...`
        /// - `startpos` / `startpos moves ...`
        /// - 生の SFEN
        /// - `name | <上記いずれか>`
        #[arg(long)]
        positions: PathBuf,

        /// movetime（ミリ秒）
        #[arg(long, default_value = "10000")]
        movetime_ms: u64,

        /// A/B 順序パターン。例: `abba`, `baab`, `ab`
        #[arg(long, default_value = "abba")]
        pattern: String,

        /// パターン反復回数
        #[arg(long, default_value = "1")]
        rounds: u32,

        /// スレッド数
        #[arg(long, default_value = "1")]
        threads: usize,

        /// TT サイズ（MB）
        #[arg(long, default_value = "256")]
        hash_mb: u32,

        /// NNUE ファイル
        #[arg(long)]
        eval_file: Option<PathBuf>,

        /// MaterialLevel。既定は `none`
        #[arg(long, default_value = "none")]
        material_level: String,

        /// CPU pinning。未指定なら `taskset` を使わない
        #[arg(long)]
        cpu: Option<usize>,

        /// shard 並列用 CPU 一覧（カンマ区切り）
        ///
        /// 指定時は局面を round-robin に分割し、各 CPU に 1 shard を割り当てる。
        #[arg(long, value_delimiter = ',')]
        cpus: Vec<usize>,

        /// `perf stat` のイベント列
        #[arg(long, default_value = DEFAULT_PERF_EVENTS)]
        perf_events: String,

        /// 共通 USI オプション（`Name=Value` 形式, repeatable）
        #[arg(long = "usi-option")]
        usi_options: Vec<String>,

        /// baseline 専用 USI オプション（`Name=Value` 形式, repeatable）
        #[arg(long = "baseline-usi-option")]
        baseline_usi_options: Vec<String>,

        /// candidate 専用 USI オプション（`Name=Value` 形式, repeatable）
        #[arg(long = "candidate-usi-option")]
        candidate_usi_options: Vec<String>,

        /// 実行ログを JSON で保存
        #[arg(long)]
        json_out: Option<PathBuf>,

        /// 詳細ログを表示
        #[arg(long, short = 'v')]
        verbose: bool,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
    enum Variant {
        Baseline,
        Candidate,
    }

    impl Variant {
        fn parse(c: char) -> Result<Self> {
            match c.to_ascii_lowercase() {
                'a' => Ok(Self::Baseline),
                'b' => Ok(Self::Candidate),
                _ => bail!("invalid --pattern character '{c}'. use only 'a' or 'b'"),
            }
        }

        fn name(self) -> &'static str {
            match self {
                Self::Baseline => "baseline",
                Self::Candidate => "candidate",
            }
        }
    }

    #[derive(Debug, Clone, Serialize)]
    struct PositionCase {
        name: String,
        position_cmd: String,
    }

    #[derive(Debug, Clone, Default, Serialize)]
    struct InfoSnapshot {
        depth: i32,
        nodes: u64,
        time_ms: u64,
        nps: u64,
        hashfull: u32,
        raw: String,
    }

    impl InfoSnapshot {
        fn update_from_line(&mut self, line: &str) {
            self.raw.clear();
            self.raw.push_str(line);

            let tokens: Vec<_> = line.split_whitespace().collect();
            let mut i = 0;
            while i < tokens.len() {
                match tokens[i] {
                    "depth" if i + 1 < tokens.len() => {
                        if let Ok(v) = tokens[i + 1].parse() {
                            self.depth = v;
                        }
                        i += 2;
                    }
                    "nodes" if i + 1 < tokens.len() => {
                        if let Ok(v) = tokens[i + 1].parse() {
                            self.nodes = v;
                        }
                        i += 2;
                    }
                    "time" if i + 1 < tokens.len() => {
                        if let Ok(v) = tokens[i + 1].parse() {
                            self.time_ms = v;
                        }
                        i += 2;
                    }
                    "nps" if i + 1 < tokens.len() => {
                        if let Ok(v) = tokens[i + 1].parse() {
                            self.nps = v;
                        }
                        i += 2;
                    }
                    "hashfull" if i + 1 < tokens.len() => {
                        if let Ok(v) = tokens[i + 1].parse() {
                            self.hashfull = v;
                        }
                        i += 2;
                    }
                    _ => i += 1,
                }
            }
        }
    }

    #[derive(Debug, Clone, Default, Serialize)]
    struct PerfCounters {
        cycles: Option<u64>,
        instructions: Option<u64>,
        branches: Option<u64>,
        branch_misses: Option<u64>,
        cache_references: Option<u64>,
        cache_misses: Option<u64>,
        l1_dcache_load_misses: Option<u64>,
    }

    impl PerfCounters {
        fn parse(csv: &str) -> Result<Self> {
            let mut counters = Self::default();

            for line in csv.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }

                let mut fields = line.split(',');
                let count = fields.next().unwrap_or_default().trim();
                let _unit = fields.next().unwrap_or_default();
                let event = fields.next().unwrap_or_default().trim();
                if event.is_empty() {
                    continue;
                }

                let value = parse_perf_count(count)?;
                match event {
                    "cycles" => counters.cycles = value,
                    "instructions" => counters.instructions = value,
                    "branches" => counters.branches = value,
                    "branch-misses" => counters.branch_misses = value,
                    "cache-references" => counters.cache_references = value,
                    "cache-misses" => counters.cache_misses = value,
                    "L1-dcache-load-misses" => counters.l1_dcache_load_misses = value,
                    _ => {}
                }
            }

            if counters.cycles.is_none() {
                bail!("perf stat output does not contain counted 'cycles'");
            }
            if counters.instructions.is_none() {
                bail!("perf stat output does not contain counted 'instructions'");
            }

            Ok(counters)
        }

        fn cycles_per_node(&self, nodes: u64) -> Option<f64> {
            ratio(self.cycles, nodes)
        }

        fn instructions_per_node(&self, nodes: u64) -> Option<f64> {
            ratio(self.instructions, nodes)
        }
    }

    #[derive(Debug, Clone, Serialize)]
    struct RunSample {
        variant: Variant,
        round: u32,
        sequence_index: usize,
        position_name: String,
        position_cmd: String,
        bestmove: String,
        info: InfoSnapshot,
        perf: PerfCounters,
    }

    #[derive(Debug, Clone, Serialize)]
    struct VariantSummary {
        variant: Variant,
        runs: usize,
        total_nodes: u64,
        total_time_ms: u64,
        average_nps: u64,
        average_depth: f64,
        cycles_per_node: f64,
        instructions_per_node: f64,
    }

    #[derive(Debug, Clone, Serialize)]
    struct ComparisonSummary {
        baseline: VariantSummary,
        candidate: VariantSummary,
        nps_delta_pct: f64,
        cycles_per_node_delta_pct: f64,
        instructions_per_node_delta_pct: f64,
    }

    #[derive(Debug, Clone, Serialize)]
    struct JsonReport {
        cli: JsonCli,
        system_info: SystemInfo,
        positions: Vec<PositionCase>,
        samples: Vec<RunSample>,
        summary: ComparisonSummary,
    }

    #[derive(Debug, Clone, Serialize)]
    struct JsonCli {
        baseline: String,
        candidate: String,
        positions: String,
        movetime_ms: u64,
        pattern: String,
        rounds: u32,
        threads: usize,
        hash_mb: u32,
        eval_file: Option<String>,
        material_level: String,
        cpu: Option<usize>,
        cpus: Vec<usize>,
        perf_events: String,
        usi_options: Vec<String>,
        baseline_usi_options: Vec<String>,
        candidate_usi_options: Vec<String>,
    }

    struct PerfWrapper {
        variant: Variant,
        child: Child,
        stdin: BufWriter<ChildStdin>,
        stdout_rx: Receiver<String>,
        ack_rx: Receiver<String>,
        stderr_handle: Option<thread::JoinHandle<Result<String>>>,
        ctl_writer: BufWriter<File>,
        opt_names: HashSet<String>,
        label: String,
    }

    impl PerfWrapper {
        fn spawn(cli: &Cli, variant: Variant, cpu: Option<usize>) -> Result<Self> {
            let pipes = ControlPipes::new()?;
            let mut cmd = Command::new("perf");
            cmd.arg("stat")
                .arg("-D")
                .arg("-1")
                .arg("--control")
                .arg(format!("fd:{PERF_CTL_FD},{PERF_ACK_FD}"))
                .arg("-x,")
                .arg("--no-big-num")
                .arg("-e")
                .arg(&cli.perf_events)
                .arg("--");

            if let Some(cpu) = cpu {
                cmd.arg("taskset").arg("-c").arg(cpu.to_string());
            }
            cmd.arg(engine_path(cli, variant));

            cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());

            let child_ctl_fd = pipes.child_ctl_fd;
            let child_ack_fd = pipes.child_ack_fd;
            let parent_ctl_fd = pipes.parent_ctl.as_raw_fd();
            let parent_ack_fd = pipes.parent_ack.as_raw_fd();

            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;

                // SAFETY:
                // - `pre_exec` は fork 後 exec 前に 1 回だけ実行される。
                // - `dup2` と `close` は async-signal-safe な POSIX 関数。
                // - `child_*_fd` はこのプロセスで有効な pipe end であり、`perf` 子プロセス側に
                //   `PERF_CTL_FD` / `PERF_ACK_FD` として引き継ぐためにのみ使う。
                // - 親側 end は子で明示的に close し、pipe の向きが壊れないようにする。
                unsafe {
                    cmd.pre_exec(move || {
                        if libc::dup2(child_ctl_fd, PERF_CTL_FD) == -1 {
                            return Err(std::io::Error::last_os_error());
                        }
                        if libc::dup2(child_ack_fd, PERF_ACK_FD) == -1 {
                            return Err(std::io::Error::last_os_error());
                        }

                        if child_ctl_fd != PERF_CTL_FD {
                            libc::close(child_ctl_fd);
                        }
                        if child_ack_fd != PERF_ACK_FD {
                            libc::close(child_ack_fd);
                        }

                        libc::close(parent_ctl_fd);
                        libc::close(parent_ack_fd);
                        Ok(())
                    });
                }
            }

            let mut child = cmd.spawn().with_context(|| {
                format!("failed to spawn perf for {}", engine_path(cli, variant).display())
            })?;

            close_fd(child_ctl_fd)?;
            close_fd(child_ack_fd)?;

            let stdin = child.stdin.take().context("failed to capture perf stdin")?;
            let stdout = child.stdout.take().context("failed to capture perf stdout")?;
            let stderr = child.stderr.take().context("failed to capture perf stderr")?;

            let (stdout_tx, stdout_rx) = mpsc::channel();
            thread::spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines().map_while(Result::ok) {
                    if stdout_tx.send(line).is_err() {
                        break;
                    }
                }
            });

            let (ack_tx, ack_rx) = mpsc::channel();
            thread::spawn(move || {
                let reader = BufReader::new(pipes.parent_ack);
                for line in reader.lines().map_while(Result::ok) {
                    if ack_tx.send(line).is_err() {
                        break;
                    }
                }
            });

            let stderr_handle = thread::spawn(move || read_all(stderr));

            let mut wrapper = Self {
                variant,
                child,
                stdin: BufWriter::new(stdin),
                stdout_rx,
                ack_rx,
                stderr_handle: Some(stderr_handle),
                ctl_writer: BufWriter::new(pipes.parent_ctl),
                opt_names: HashSet::new(),
                label: variant.name().to_string(),
            };
            wrapper.initialize(cli, variant)?;
            Ok(wrapper)
        }

        fn initialize(&mut self, cli: &Cli, variant: Variant) -> Result<()> {
            self.write_line("usi")?;
            loop {
                let line = self.recv_line(READY_TIMEOUT)?;
                if let Some(rest) = line.strip_prefix("option ") {
                    if let Some(name) = parse_option_name(rest) {
                        self.opt_names.insert(name);
                    }
                } else if line == "usiok" {
                    break;
                }
            }

            self.set_option_if_available("Threads", &cli.threads.to_string())?;
            let hash = cli.hash_mb.to_string();
            self.set_option_if_available("USI_Hash", &hash)?;
            self.set_option_if_available("Hash", &hash)?;
            self.set_option_if_available("MaterialLevel", &cli.material_level)?;
            if let Some(eval_file) = &cli.eval_file {
                self.set_option_if_available("EvalFile", &eval_file.display().to_string())?;
            }

            for opt in &cli.usi_options {
                self.apply_usi_option(opt)?;
            }
            let extra_options = match variant {
                Variant::Baseline => &cli.baseline_usi_options,
                Variant::Candidate => &cli.candidate_usi_options,
            };
            for opt in extra_options {
                self.apply_usi_option(opt)?;
            }

            self.write_line("isready")?;
            self.wait_for("readyok", READY_TIMEOUT)?;
            Ok(())
        }

        fn run_search(
            mut self,
            cli: &Cli,
            position: &PositionCase,
            round: u32,
            sequence_index: usize,
        ) -> Result<RunSample> {
            self.enable_perf()?;
            self.write_line(&position.position_cmd)?;
            self.write_line(&format!("go movetime {}", cli.movetime_ms))?;

            let timeout = Duration::from_millis(cli.movetime_ms.saturating_mul(2) + 5000);
            let mut info = InfoSnapshot::default();
            let mut bestmove = None;
            let start = Instant::now();

            while start.elapsed() < timeout {
                let line = match self.stdout_rx.recv_timeout(POLL_INTERVAL) {
                    Ok(line) => line,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        bail!("{}: engine output channel disconnected", self.label)
                    }
                };
                if cli.verbose {
                    eprintln!("[{}] {line}", self.label);
                }
                if line.starts_with("info ") {
                    info.update_from_line(&line);
                } else if let Some(mv) = line.strip_prefix("bestmove ") {
                    bestmove =
                        Some(mv.split_whitespace().next().unwrap_or("none").trim().to_string());
                    break;
                }
            }

            let bestmove = bestmove.ok_or_else(|| {
                anyhow!(
                    "{}: timed out waiting for bestmove for position {}",
                    self.label,
                    position.name
                )
            })?;

            self.disable_perf()?;
            self.write_line("quit")?;

            let status = wait_child(&mut self.child, QUIT_TIMEOUT)?;
            let stderr = self.join_stderr()?;
            if !status.success() {
                bail!("perf wrapper exited with status {status}: {stderr}");
            }
            let perf = PerfCounters::parse(&stderr)?;

            Ok(RunSample {
                variant: self.variant,
                round,
                sequence_index,
                position_name: position.name.clone(),
                position_cmd: position.position_cmd.clone(),
                bestmove,
                info,
                perf,
            })
        }

        fn wait_for(&self, expected: &str, timeout: Duration) -> Result<()> {
            let deadline = Instant::now() + timeout;
            while Instant::now() < deadline {
                let remaining = deadline.saturating_duration_since(Instant::now());
                match self.stdout_rx.recv_timeout(remaining.min(POLL_INTERVAL)) {
                    Ok(line) if line.starts_with(expected) => return Ok(()),
                    Ok(_) => continue,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        bail!("{}: engine disconnected while waiting for {expected}", self.label)
                    }
                }
            }
            bail!("{}: timeout waiting for {expected}", self.label)
        }

        fn recv_line(&self, timeout: Duration) -> Result<String> {
            self.stdout_rx.recv_timeout(timeout).map_err(|_| {
                anyhow!("{}: timeout waiting for engine output after {:?}", self.label, timeout)
            })
        }

        fn set_option_if_available(&mut self, name: &str, value: &str) -> Result<()> {
            if self.opt_names.is_empty() || self.opt_names.contains(name) {
                self.write_line(&format!("setoption name {name} value {value}"))?;
            }
            Ok(())
        }

        fn apply_usi_option(&mut self, opt: &str) -> Result<()> {
            if let Some((name, value)) = opt.split_once('=') {
                self.set_option_if_available(name.trim(), value.trim())
            } else {
                self.write_line(&format!("setoption name {}", opt.trim()))
            }
        }

        fn enable_perf(&mut self) -> Result<()> {
            self.control("enable")
        }

        fn disable_perf(&mut self) -> Result<()> {
            self.control("disable")
        }

        fn control(&mut self, cmd: &str) -> Result<()> {
            writeln!(self.ctl_writer, "{cmd}")?;
            self.ctl_writer.flush()?;
            let ack = self
                .ack_rx
                .recv_timeout(ACK_TIMEOUT)
                .map_err(|_| anyhow!("{}: timeout waiting for perf ack for {cmd}", self.label))?;
            let ack = ack.trim_matches(|c: char| c == '\0' || c.is_whitespace());
            if ack != "ack" {
                bail!("{}: unexpected perf ack payload '{ack}'", self.label);
            }
            Ok(())
        }

        fn write_line(&mut self, line: &str) -> Result<()> {
            writeln!(self.stdin, "{line}")?;
            self.stdin.flush()?;
            Ok(())
        }

        fn join_stderr(&mut self) -> Result<String> {
            self.stderr_handle
                .take()
                .ok_or_else(|| anyhow!("perf stderr handle already taken"))?
                .join()
                .map_err(|_| anyhow!("perf stderr reader thread panicked"))?
        }
    }

    impl Drop for PerfWrapper {
        fn drop(&mut self) {
            let _ = writeln!(self.stdin, "quit");
            let _ = self.stdin.flush();
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    struct ControlPipes {
        parent_ctl: File,
        parent_ack: File,
        child_ctl_fd: RawFd,
        child_ack_fd: RawFd,
    }

    impl ControlPipes {
        fn new() -> Result<Self> {
            let mut ctl = [0; 2];
            let mut ack = [0; 2];

            // SAFETY:
            // - `ctl` / `ack` は長さ2の有効な配列で、`pipe(2)` が書き込む領域を満たす。
            // - 成功時に得られる fd はこの関数の返り値で所有権を明確化し、呼び出し側で閉じる。
            unsafe {
                if libc::pipe(ctl.as_mut_ptr()) == -1 {
                    return Err(std::io::Error::last_os_error())
                        .context("pipe() failed for control");
                }
                if libc::pipe(ack.as_mut_ptr()) == -1 {
                    let _ = libc::close(ctl[0]);
                    let _ = libc::close(ctl[1]);
                    return Err(std::io::Error::last_os_error()).context("pipe() failed for ack");
                }
            }

            // SAFETY:
            // - `ctl[1]` / `ack[0]` は直前の `pipe(2)` が返した live fd。
            // - ここで `File` に所有権を移し、以後は `File` drop で 1 回だけ close される。
            let parent_ctl = unsafe { File::from_raw_fd(ctl[1]) };
            // SAFETY:
            // - `ack[0]` は直前の `pipe(2)` が返した live fd。
            // - 所有権を `File` に移して二重 close を防ぐ。
            let parent_ack = unsafe { File::from_raw_fd(ack[0]) };

            Ok(Self {
                parent_ctl,
                parent_ack,
                child_ctl_fd: ctl[0],
                child_ack_fd: ack[1],
            })
        }
    }

    pub fn main() -> Result<()> {
        let cli = Cli::parse();
        let positions = load_position_cases(&cli.positions)?;
        if positions.is_empty() {
            bail!("no positions loaded from {}", cli.positions.display());
        }

        let shard_cpus = resolve_shard_cpus(&cli)?;
        let shards = shard_positions(&positions, shard_cpus.len());
        let mut handles = Vec::new();

        for (shard_index, (cpu, shard_positions)) in shard_cpus.into_iter().zip(shards).enumerate()
        {
            if shard_positions.is_empty() {
                continue;
            }
            let shard_cli = cli.clone();
            handles.push(thread::spawn(move || {
                run_shard(shard_cli, cpu, shard_positions, shard_index + 1)
            }));
        }

        let mut samples = Vec::new();
        for handle in handles {
            let mut shard_samples =
                handle.join().map_err(|_| anyhow!("shard thread panicked"))??;
            samples.append(&mut shard_samples);
        }
        samples.sort_by(|a, b| {
            a.round
                .cmp(&b.round)
                .then(a.position_name.cmp(&b.position_name))
                .then(a.sequence_index.cmp(&b.sequence_index))
                .then(a.variant.name().cmp(b.variant.name()))
        });

        let summary = build_summary(&samples)?;
        print_summary(&summary);

        if let Some(path) = &cli.json_out {
            let report = JsonReport {
                cli: JsonCli {
                    baseline: cli.baseline.display().to_string(),
                    candidate: cli.candidate.display().to_string(),
                    positions: cli.positions.display().to_string(),
                    movetime_ms: cli.movetime_ms,
                    pattern: cli.pattern.clone(),
                    rounds: cli.rounds,
                    threads: cli.threads,
                    hash_mb: cli.hash_mb,
                    eval_file: cli.eval_file.as_ref().map(|p| p.display().to_string()),
                    material_level: cli.material_level.clone(),
                    cpu: cli.cpu,
                    cpus: cli.cpus.clone(),
                    perf_events: cli.perf_events.clone(),
                    usi_options: cli.usi_options.clone(),
                    baseline_usi_options: cli.baseline_usi_options.clone(),
                    candidate_usi_options: cli.candidate_usi_options.clone(),
                },
                system_info: collect_system_info(),
                positions,
                samples,
                summary,
            };
            let file = File::create(path)
                .with_context(|| format!("failed to create JSON report {}", path.display()))?;
            serde_json::to_writer_pretty(file, &report)
                .with_context(|| format!("failed to write JSON report {}", path.display()))?;
            println!("JSON report: {}", path.display());
        }

        Ok(())
    }

    fn load_position_cases(path: &Path) -> Result<Vec<PositionCase>> {
        let file =
            File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
        let reader = BufReader::new(file);
        let mut positions = Vec::new();

        for (idx, line) in reader.lines().enumerate() {
            let line = line?;
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            let (name, payload) = if let Some((name, payload)) = line.split_once('|') {
                (name.trim().to_string(), payload.trim().to_string())
            } else {
                (format!("position_{}", idx + 1), line.to_string())
            };

            positions.push(PositionCase {
                name,
                position_cmd: normalize_position_command(&payload),
            });
        }

        Ok(positions)
    }

    fn resolve_shard_cpus(cli: &Cli) -> Result<Vec<Option<usize>>> {
        if cli.cpu.is_some() && !cli.cpus.is_empty() {
            bail!("use either --cpu or --cpus, not both");
        }
        if !cli.cpus.is_empty() {
            return Ok(cli.cpus.iter().copied().map(Some).collect());
        }
        Ok(vec![cli.cpu])
    }

    fn shard_positions(positions: &[PositionCase], shard_count: usize) -> Vec<Vec<PositionCase>> {
        let mut shards = vec![Vec::new(); shard_count.max(1)];
        let len = shards.len();
        for (index, position) in positions.iter().cloned().enumerate() {
            shards[index % len].push(position);
        }
        shards
    }

    fn run_shard(
        cli: Cli,
        cpu: Option<usize>,
        positions: Vec<PositionCase>,
        shard_index: usize,
    ) -> Result<Vec<RunSample>> {
        let pattern = parse_pattern(&cli.pattern)?;
        let mut samples = Vec::new();

        for round_idx in 0..cli.rounds {
            for position in &positions {
                for (sequence_index, variant) in pattern.iter().copied().enumerate() {
                    let run_no = samples.len() + 1;
                    println!(
                        "[shard {shard_index}][{run_no}] round={} position={} order={} variant={} cpu={}",
                        round_idx + 1,
                        position.name,
                        sequence_index + 1,
                        variant.name(),
                        cpu.map_or_else(|| "-".to_string(), |c| c.to_string())
                    );

                    let wrapper = PerfWrapper::spawn(&cli, variant, cpu)?;
                    let sample = wrapper
                        .run_search(&cli, position, round_idx + 1, sequence_index + 1)
                        .with_context(|| {
                            format!(
                                "shard {shard_index} failed at position={} order={} variant={}",
                                position.name,
                                sequence_index + 1,
                                variant.name()
                            )
                        })?;
                    println!(
                        "[shard {shard_index}] depth={} nodes={} time={}ms nps={} cycles/node={:.1} instructions/node={:.1}",
                        sample.info.depth,
                        sample.info.nodes,
                        sample.info.time_ms,
                        sample.info.nps,
                        sample.perf.cycles_per_node(sample.info.nodes).unwrap_or(0.0),
                        sample.perf.instructions_per_node(sample.info.nodes).unwrap_or(0.0),
                    );
                    samples.push(sample);
                }
            }
        }

        Ok(samples)
    }

    fn normalize_position_command(payload: &str) -> String {
        let trimmed = payload.trim();
        if trimmed.starts_with("position ") {
            trimmed.to_string()
        } else if trimmed == "startpos" || trimmed.starts_with("startpos ") {
            format!("position {trimmed}")
        } else if let Some(rest) = trimmed.strip_prefix("sfen ") {
            format!("position sfen {rest}")
        } else {
            format!("position sfen {trimmed}")
        }
    }

    fn parse_pattern(pattern: &str) -> Result<Vec<Variant>> {
        let pattern = pattern.trim();
        if pattern.is_empty() {
            bail!("--pattern must not be empty");
        }
        pattern.chars().map(Variant::parse).collect()
    }

    fn engine_path(cli: &Cli, variant: Variant) -> &Path {
        match variant {
            Variant::Baseline => &cli.baseline,
            Variant::Candidate => &cli.candidate,
        }
    }

    fn parse_option_name(line: &str) -> Option<String> {
        let mut tokens = line.split_whitespace().peekable();
        while let Some(tok) = tokens.next() {
            if tok == "name" {
                let mut parts = Vec::new();
                while let Some(next) = tokens.peek() {
                    if *next == "type" {
                        break;
                    }
                    parts.push(tokens.next().unwrap_or_default().to_string());
                }
                if !parts.is_empty() {
                    return Some(parts.join(" "));
                }
            }
        }
        None
    }

    fn parse_perf_count(field: &str) -> Result<Option<u64>> {
        let field = field.trim();
        if field.is_empty() || field == "<not counted>" || field == "<not supported>" {
            return Ok(None);
        }
        let value = field
            .parse::<u64>()
            .with_context(|| format!("failed to parse perf count '{field}'"))?;
        Ok(Some(value))
    }

    fn ratio(value: Option<u64>, denom: u64) -> Option<f64> {
        if denom == 0 {
            return None;
        }
        value.map(|v| v as f64 / denom as f64)
    }

    fn wait_child(child: &mut Child, timeout: Duration) -> Result<std::process::ExitStatus> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Some(status) = child.try_wait()? {
                return Ok(status);
            }
            thread::sleep(POLL_INTERVAL);
        }
        let _ = child.kill();
        Ok(child.wait()?)
    }

    fn read_all<R: Read>(mut reader: R) -> Result<String> {
        let mut text = String::new();
        reader.read_to_string(&mut text)?;
        Ok(text)
    }

    fn build_summary(samples: &[RunSample]) -> Result<ComparisonSummary> {
        let baseline = summarize_variant(samples, Variant::Baseline)?;
        let candidate = summarize_variant(samples, Variant::Candidate)?;

        Ok(ComparisonSummary {
            nps_delta_pct: pct_delta(candidate.average_nps as f64, baseline.average_nps as f64),
            cycles_per_node_delta_pct: pct_delta(
                candidate.cycles_per_node,
                baseline.cycles_per_node,
            ),
            instructions_per_node_delta_pct: pct_delta(
                candidate.instructions_per_node,
                baseline.instructions_per_node,
            ),
            baseline,
            candidate,
        })
    }

    fn summarize_variant(samples: &[RunSample], variant: Variant) -> Result<VariantSummary> {
        let filtered: Vec<_> = samples.iter().filter(|s| s.variant == variant).collect();
        if filtered.is_empty() {
            bail!("no samples for {}", variant.name());
        }

        let runs = filtered.len();
        let total_nodes: u64 = filtered.iter().map(|s| s.info.nodes).sum();
        let total_time_ms: u64 = filtered.iter().map(|s| s.info.time_ms).sum();
        let total_cycles: u128 =
            filtered.iter().map(|s| s.perf.cycles.unwrap_or_default() as u128).sum();
        let total_instructions: u128 =
            filtered.iter().map(|s| s.perf.instructions.unwrap_or_default() as u128).sum();
        let depth_sum: i64 = filtered.iter().map(|s| i64::from(s.info.depth)).sum();

        let average_nps = if total_time_ms == 0 {
            0
        } else {
            ((total_nodes as f64) * 1000.0 / (total_time_ms as f64)).round() as u64
        };
        let average_depth = depth_sum as f64 / runs as f64;
        let cycles_per_node = total_cycles as f64 / total_nodes as f64;
        let instructions_per_node = total_instructions as f64 / total_nodes as f64;

        Ok(VariantSummary {
            variant,
            runs,
            total_nodes,
            total_time_ms,
            average_nps,
            average_depth,
            cycles_per_node,
            instructions_per_node,
        })
    }

    fn pct_delta(current: f64, base: f64) -> f64 {
        if base == 0.0 {
            0.0
        } else {
            (current / base - 1.0) * 100.0
        }
    }

    fn print_summary(summary: &ComparisonSummary) {
        println!();
        println!(
            "{:<10} {:>6} {:>14} {:>12} {:>12} {:>14} {:>20}",
            "engine", "runs", "nodes", "time_ms", "avg_nps", "cycles/node", "instructions/node"
        );
        println!("{}", "-".repeat(96));
        for row in [&summary.baseline, &summary.candidate] {
            println!(
                "{:<10} {:>6} {:>14} {:>12} {:>12} {:>14.1} {:>20.1}",
                row.variant.name(),
                row.runs,
                row.total_nodes,
                row.total_time_ms,
                row.average_nps,
                row.cycles_per_node,
                row.instructions_per_node,
            );
        }
        println!();
        println!(
            "candidate vs baseline: NPS {:+.2}%, cycles/node {:+.2}%, instructions/node {:+.2}%",
            summary.nps_delta_pct,
            summary.cycles_per_node_delta_pct,
            summary.instructions_per_node_delta_pct
        );
    }

    fn close_fd(fd: RawFd) -> Result<()> {
        // SAFETY:
        // - `fd` は `pipe(2)` で取得した live fd。
        // - 親プロセス側で子用 end を不要になった時点で 1 回だけ close する。
        let rc = unsafe { libc::close(fd) };
        if rc == -1 {
            return Err(std::io::Error::last_os_error()).context("close() failed");
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn normalize_position_command_accepts_raw_sfen() {
            let raw = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
            assert_eq!(normalize_position_command(raw), format!("position sfen {raw}"));
        }

        #[test]
        fn normalize_position_command_accepts_position_line() {
            let line = "position startpos moves 7g7f 3c3d";
            assert_eq!(normalize_position_command(line), line);
        }

        #[test]
        fn normalize_position_command_accepts_startpos() {
            assert_eq!(normalize_position_command("startpos"), "position startpos");
        }

        #[test]
        fn parse_pattern_supports_abba() {
            let pattern = parse_pattern("abba").expect("pattern should parse");
            assert_eq!(
                pattern,
                vec![
                    Variant::Baseline,
                    Variant::Candidate,
                    Variant::Candidate,
                    Variant::Baseline
                ]
            );
        }

        #[test]
        fn parse_perf_csv_extracts_counts() {
            let csv = "\
2574890,,cycles,431219,59.00,,\n\
2118685,,instructions,729879,100.00,0.82,insn per cycle\n\
443459,,branches,729879,100.00,,\n\
";
            let counters = PerfCounters::parse(csv).expect("perf CSV should parse");
            assert_eq!(counters.cycles, Some(2574890));
            assert_eq!(counters.instructions, Some(2118685));
            assert_eq!(counters.branches, Some(443459));
        }

        #[test]
        fn parse_option_name_extracts_multi_word_name() {
            let line = "name Skill Level type spin default 20 min 0 max 20";
            assert_eq!(parse_option_name(line).as_deref(), Some("Skill Level"));
        }
    }
} // mod unix_main

#[cfg(unix)]
fn main() -> anyhow::Result<()> {
    unix_main::main()
}
