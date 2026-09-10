//! search-only A/B ベンチマークツール
//!
//! USI エンジンを fresh process で起動し、探索区間 (`go`〜`bestmove`) だけの
//! HW カウンタを A/B 計測する。起動・NNUEロード・`isready` のノイズを外しつつ、
//! before/after の差を比較したいときの基準計測用ツール。
//!
//! - Linux: `readyok` 後に `perf stat --control` を `enable`、`bestmove` 後に
//!   `disable` することで探索区間だけを測定する。
//! - Windows: ETW NT Kernel Logger の PMC counting (CSwitch イベントへの PMC
//!   カウンタ添付) を使い、探索区間内にエンジンスレッドへ帰属する counter 差分
//!   だけを集計する。要管理者権限。

#[cfg(not(any(unix, windows)))]
fn main() {
    eprintln!("search_only_ab は Linux (perf) / Windows (ETW) 専用ツールです");
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
                let event_raw = fields.next().unwrap_or_default().trim();
                if event_raw.is_empty() {
                    continue;
                }
                // WSL2 等の user-mode only perf では `cycles:u` のように modifier が付く
                let event = event_raw.split(':').next().unwrap_or(event_raw);

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

        // WSL2 等 perf_event_paranoid 制限下では event 名に `:u` modifier が付く
        #[test]
        fn parse_perf_csv_strips_user_mode_modifier() {
            let csv = "\
2574890,,cycles:u,431219,100.00,,\n\
2118685,,instructions:u,729879,100.00,0.82,insn per cycle\n\
443459,,branches:u,729879,100.00,,\n\
1234,,cache-misses:u,729879,100.00,,\n\
5678,,L1-dcache-load-misses:u,729879,100.00,,\n\
";
            let counters = PerfCounters::parse(csv).expect("perf CSV with :u should parse");
            assert_eq!(counters.cycles, Some(2574890));
            assert_eq!(counters.instructions, Some(2118685));
            assert_eq!(counters.branches, Some(443459));
            assert_eq!(counters.cache_misses, Some(1234));
            assert_eq!(counters.l1_dcache_load_misses, Some(5678));
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

/// Windows 実装。
///
/// unix 版 (`perf stat --control`) と CLI / JSON スキーマ互換を保ちつつ、HW カウンタの
/// 取得だけを ETW NT Kernel Logger の PMC counting に置き換える。稼働中の Linux 側
/// perf loop への影響をゼロにするため、unix 版とはコードを共有せず自己完結させる
/// (プロトコル駆動コードの重複は許容する)。
#[cfg(windows)]
mod windows_main {
    use std::collections::{BTreeMap, HashSet};
    use std::fs::File;
    use std::io::{BufRead, BufReader, BufWriter, Write};
    use std::path::{Path, PathBuf};
    use std::process::{Child, ChildStdin, Command, Stdio};
    use std::sync::mpsc::{self, Receiver};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use anyhow::{Context, Result, anyhow, bail};
    use clap::Parser;
    use serde::Serialize;

    use tools::{SystemInfo, collect_system_info};

    use pmc::{MAX_PMC_SOURCES, PmcEngineState};

    const DEFAULT_PMC_SOURCES: &str = "TotalCycles,InstructionRetired";
    const READY_TIMEOUT: Duration = Duration::from_secs(120);
    const QUIT_TIMEOUT: Duration = Duration::from_secs(5);
    const POLL_INTERVAL: Duration = Duration::from_millis(10);
    /// セッション STOP 後、consumer の ProcessTrace が残バッファを処理し終えるのを待つ上限。
    const DRAIN_TIMEOUT: Duration = Duration::from_secs(15);

    #[derive(Parser, Debug, Clone)]
    #[command(
        name = "search_only_ab",
        version,
        about = "ETW PMC counting を使った search-only A/B ベンチマーク (Windows)"
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

        /// CPU pinning。未指定なら affinity を設定しない
        #[arg(long)]
        cpu: Option<usize>,

        /// shard 並列用 CPU 一覧（Windows 版では未対応。指定するとエラー）
        #[arg(long, value_delimiter = ',')]
        cpus: Vec<usize>,

        /// PMC profile source 名の列（カンマ区切り）
        ///
        /// `wpr -pmcsources` で列挙される名前を指定する。cycles/node と
        /// instructions/node の算出に `TotalCycles` と `InstructionRetired` が必須。
        #[arg(long, default_value = DEFAULT_PMC_SOURCES)]
        pmc_sources: String,

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

    /// unix 版の `PerfCounters` と同名フィールドの JSON スキーマ互換構造体。
    ///
    /// PMC source 名を既知のフィールドへマップし、対応の無い source は `extra` に
    /// source 名のまま入れる (`extra` は追加フィールドなので既存の jq 集計を壊さない)。
    #[derive(Debug, Clone, Default, Serialize)]
    struct PerfCounters {
        cycles: Option<u64>,
        instructions: Option<u64>,
        branches: Option<u64>,
        branch_misses: Option<u64>,
        cache_references: Option<u64>,
        cache_misses: Option<u64>,
        l1_dcache_load_misses: Option<u64>,
        #[serde(skip_serializing_if = "BTreeMap::is_empty")]
        extra: BTreeMap<String, u64>,
    }

    impl PerfCounters {
        /// PMC source 名 (CLI 指定順) と counter 合計から構築する。
        fn from_totals(source_names: &[String], totals: &[u64]) -> Self {
            let mut counters = Self::default();
            for (name, value) in source_names.iter().zip(totals) {
                match name.to_ascii_lowercase().as_str() {
                    "totalcycles" => counters.cycles = Some(*value),
                    "instructionretired" | "instructionsretired" => {
                        counters.instructions = Some(*value)
                    }
                    "branchinstructions" => counters.branches = Some(*value),
                    "branchmispredictions" => counters.branch_misses = Some(*value),
                    "cachemisses" => counters.cache_misses = Some(*value),
                    "dcachemisses" => counters.l1_dcache_load_misses = Some(*value),
                    _ => {
                        counters.extra.insert(name.clone(), *value);
                    }
                }
            }
            counters
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
        pmc_sources: String,
        usi_options: Vec<String>,
        baseline_usi_options: Vec<String>,
        candidate_usi_options: Vec<String>,
    }

    /// ETW から切り離した PMC 差分帰属の純ロジック。
    ///
    /// NT Kernel Logger の CSwitch イベントには、設定した PMC source ごとの
    /// per-CPU 自走カウンタ値が添付される。「同一 CPU 上の前回 CSwitch からの差分を、
    /// switch out される旧スレッドの実行区間に帰属させる」規則で、対象プロセスの
    /// スレッドに帰属する差分だけを積算する。
    mod pmc {
        use std::collections::{HashMap, HashSet};

        /// CSwitch 1 件に添付できる PMC source 数の上限。
        pub const MAX_PMC_SOURCES: usize = 8;

        /// CSwitch イベント 1 件分の入力。
        #[derive(Debug, Clone, Copy)]
        pub struct CSwitchSample {
            /// イベントが発生した論理 CPU 番号
            pub cpu: u16,
            /// QPC タイムスタンプ
            pub timestamp: i64,
            /// switch out される旧スレッドの TID
            pub old_tid: u32,
            /// switch in される新スレッドの TID
            pub new_tid: u32,
            /// PMC カウンタ値（先頭 `len` 個が有効）
            pub counters: [u64; MAX_PMC_SOURCES],
            /// 有効なカウンタ数
            pub len: usize,
        }

        /// ETW Thread Start イベントから PID→TID 集合を維持する。
        #[derive(Debug, Default)]
        pub struct ThreadTracker {
            tids_by_pid: HashMap<u32, HashSet<u32>>,
        }

        impl ThreadTracker {
            pub fn on_thread_start(&mut self, pid: u32, tid: u32) {
                // TID は再利用されるため、別 PID に残っている同じ TID の所属を先に外す
                for (other_pid, tids) in self.tids_by_pid.iter_mut() {
                    if *other_pid != pid {
                        tids.remove(&tid);
                    }
                }
                self.tids_by_pid.entry(pid).or_default().insert(tid);
            }

            /// Thread End では所属を外さない (購読もしない)。ETW は per-CPU バッファ単位で
            /// 配送されるため、終了スレッドの最終 CSwitch (switch-out) が End より後に処理
            /// されることがあり、End で即削除するとその最終スライスが非対象扱いになる。
            /// セッションは run ごとに作り直すので、run 終了時にここで全部消す。
            pub fn clear(&mut self) {
                self.tids_by_pid.clear();
            }

            pub fn belongs_to(&self, pid: u32, tid: u32) -> bool {
                self.tids_by_pid.get(&pid).is_some_and(|tids| tids.contains(&tid))
            }
        }

        /// CPU ごとの直前 CSwitch の観測値（実行スライスの始点）。
        #[derive(Debug, Clone, Copy)]
        struct CpuBaseline {
            timestamp: i64,
            counters: [u64; MAX_PMC_SOURCES],
            /// この CSwitch で switch in したスレッド (= 次の CSwitch まで CPU を占有する)
            new_tid: u32,
            new_is_target: bool,
            /// counters が設定数ぶん揃っているか。PMC 未添付 / 不足のイベントでも
            /// switch-in の TID と時刻は基準として保持し、差分だけを無効にする
            counters_valid: bool,
        }

        /// 1 run 分の PMC 差分積算器。
        #[derive(Debug)]
        struct PmcAccumulator {
            target_pid: u32,
            n_counters: usize,
            /// CPU ごとの前回 CSwitch 時の (timestamp, カウンタ値)
            per_cpu_last: HashMap<u16, CpuBaseline>,
            /// 計測区間 (QPC)。start 未設定の間は集計しない
            window_start: Option<i64>,
            window_end: Option<i64>,
            totals: [u64; MAX_PMC_SOURCES],
            /// 集計に入った CSwitch 数（診断用）
            attributed_switches: u64,
            /// 区間内なのにカウンタ巻き戻りで skip した CSwitch 数（診断用）
            regressed_switches: u64,
            /// 対象スレッド実行中に CSwitch の連鎖 (prev.new_tid == cur.old_tid) が途切れた回数
            chain_breaks_target: u64,
            /// 対象スレッドが絡む CSwitch に PMC が付いていなかった回数
            pmc_gaps_target: u64,
            /// run 中に対象所属の TID が別 PID の Thread Start で再利用された回数。
            /// 再利用後は遅延到着した旧スレッドの switch-out と再利用先の切替を
            /// 区別できないため、この run は判別不能として拒否する
            tid_reuse_target: u64,
        }

        impl PmcAccumulator {
            fn new(target_pid: u32, n_counters: usize) -> Self {
                Self {
                    target_pid,
                    n_counters: n_counters.min(MAX_PMC_SOURCES),
                    per_cpu_last: HashMap::new(),
                    window_start: None,
                    window_end: None,
                    totals: [0; MAX_PMC_SOURCES],
                    attributed_switches: 0,
                    regressed_switches: 0,
                    chain_breaks_target: 0,
                    pmc_gaps_target: 0,
                    tid_reuse_target: 0,
                }
            }

            /// 実行スライス `[slice_start, slice_end]` と計測 window の重なり比
            /// (0.0..=1.0)。区間按分の仕様:
            ///
            /// - 各 CSwitch は「同一 CPU の直前イベント timestamp から自身の timestamp
            ///   まで」の実行スライスを表し、counter 差分はスライス内で一様に増えた
            ///   と近似する
            /// - window と重なる長さの比で差分を線形按分する（完全内包は全額、境界
            ///   跨ぎは比例配分）。open 側・close 側とも同じ規則で対称
            /// - 長さ 0 のスライスは終端が window 内（両端 inclusive）なら全額
            /// - window 未 open（start 未設定）は常に 0
            ///
            /// pin された idle CPU ではエンジンスレッドが数百 ms〜秒オーダーで
            /// switch せずスライスが極端に粗くなるため、境界スライスの全額計上 /
            /// 全額除外では数 % 級の量子化誤差が出る（A/A 実測で cycles/node ±1%
            /// 級の残差）。按分はこれを一桁以上圧縮する。一様増加近似の誤差と
            /// 割り込み・DPC 混入は残る。
            fn window_overlap_fraction(&self, slice_start: i64, slice_end: i64) -> f64 {
                let Some(start) = self.window_start else {
                    return 0.0;
                };
                let end = self.window_end.unwrap_or(i64::MAX);
                if slice_end <= slice_start {
                    // 長さ 0（または時計異常で逆転）のスライスは終端の位置で全額判定
                    return if slice_end >= start && slice_end <= end {
                        1.0
                    } else {
                        0.0
                    };
                }
                let overlap_start = slice_start.max(start);
                let overlap_end = slice_end.min(end);
                if overlap_end <= overlap_start {
                    return 0.0;
                }
                (overlap_end - overlap_start) as f64 / (slice_end - slice_start) as f64
            }

            fn record(
                &mut self,
                sample: &CSwitchSample,
                old_tid_is_target: bool,
                new_tid_is_target: bool,
            ) {
                let n = self.n_counters;
                let prev = self.per_cpu_last.get(&sample.cpu).copied();
                let overlaps = prev.is_some_and(|p| {
                    self.window_overlap_fraction(p.timestamp, sample.timestamp) > 0.0
                });
                // 連鎖検査: 前回 switch in したスレッドが今回 switch out するはず。
                // 途切れていればその間のイベントが欠けている (配送ロス / 未観測)。
                let chain_ok = prev
                    .is_none_or(|p| p.new_tid == sample.old_tid && sample.timestamp >= p.timestamp);
                if !chain_ok
                    && overlaps
                    && (old_tid_is_target || prev.is_some_and(|p| p.new_is_target))
                {
                    self.chain_breaks_target += 1;
                }
                let counters_valid = sample.len >= n;
                // 連鎖が正しければ switch-in 時点の所属が正。Thread Start (別 PID での TID
                // 再利用) が遅延到着した旧スレッドの switch-out より先に処理されると
                // old_tid_is_target が false になるため、switch-in 時の所属も見る。
                let slice_is_target = old_tid_is_target || prev.is_some_and(|p| p.new_is_target);
                if chain_ok
                    && slice_is_target
                    && overlaps
                    && let Some(prev) = prev
                {
                    if !counters_valid || !prev.counters_valid {
                        // 対象スライスが window に掛かるのに、その両端どちらかに PMC が無い
                        self.pmc_gaps_target += 1;
                    } else {
                        let fraction =
                            self.window_overlap_fraction(prev.timestamp, sample.timestamp);
                        // カウンタ巻き戻り（セッション再構成等）は差分にできないのでスキップ
                        let monotonic = prev.counters[..n]
                            .iter()
                            .zip(&sample.counters[..n])
                            .all(|(p, c)| c >= p);
                        if monotonic {
                            for (total, (prev_v, now)) in self
                                .totals
                                .iter_mut()
                                .zip(prev.counters[..n].iter().zip(&sample.counters[..n]))
                            {
                                // 一様増加近似での線形按分。最近接丸め
                                *total += (((now - prev_v) as f64) * fraction).round() as u64;
                            }
                            self.attributed_switches += 1;
                        } else {
                            // 帰属すべきだった差分を捨てた回数。0 でない run は要注意
                            self.regressed_switches += 1;
                        }
                    }
                }
                self.per_cpu_last.insert(
                    sample.cpu,
                    CpuBaseline {
                        timestamp: sample.timestamp,
                        counters: sample.counters,
                        new_tid: sample.new_tid,
                        new_is_target: new_tid_is_target,
                        counters_valid,
                    },
                );
            }

            /// window 終了時点でまだ対象スレッドが走っている (最終 switch-out 未到着) CPU 数。
            /// drain 完了後に 0 でなければ最終スライスの CSwitch が届いていない。
            fn unclosed_target_slices(&self) -> usize {
                let end = self.window_end.unwrap_or(i64::MAX);
                self.per_cpu_last
                    .values()
                    .filter(|b| b.new_is_target && b.timestamp < end)
                    .count()
            }
        }

        /// 1 run 分の積算結果と欠落診断。
        #[derive(Debug, Clone)]
        pub struct PmcTotals {
            pub totals: Vec<u64>,
            pub attributed_switches: u64,
            pub regressed_switches: u64,
            pub chain_breaks_target: u64,
            pub pmc_gaps_target: u64,
            pub tid_reuse_target: u64,
            pub unclosed_target_slices: usize,
        }

        /// ThreadTracker と PmcAccumulator を束ねた、ETW コールバックが呼ぶ純ロジック部。
        ///
        /// セッションは run ごとに作り直すので、ThreadTracker は `take_totals` で全消去し、
        /// PmcAccumulator は run ごとに `install` で作り直す。
        #[derive(Debug, Default)]
        pub struct PmcEngineState {
            tracker: ThreadTracker,
            acc: Option<PmcAccumulator>,
        }

        impl PmcEngineState {
            pub fn on_thread_start(&mut self, pid: u32, tid: u32) {
                // 所属を書き換える前に判定する: 対象所属の TID が別 PID で始まったら再利用
                if let Some(acc) = &mut self.acc
                    && pid != acc.target_pid
                    && self.tracker.belongs_to(acc.target_pid, tid)
                {
                    acc.tid_reuse_target += 1;
                }
                self.tracker.on_thread_start(pid, tid);
            }

            pub fn on_cswitch(&mut self, sample: &CSwitchSample) {
                if let Some(acc) = &mut self.acc {
                    let old_is_target = self.tracker.belongs_to(acc.target_pid, sample.old_tid);
                    let new_is_target = self.tracker.belongs_to(acc.target_pid, sample.new_tid);
                    acc.record(sample, old_is_target, new_is_target);
                }
            }

            /// 計測対象プロセスを設定し、積算をリセットする。
            pub fn install(&mut self, target_pid: u32, n_counters: usize) {
                self.acc = Some(PmcAccumulator::new(target_pid, n_counters));
            }

            /// 計測区間を開く（`position` + `go` 送信直前の QPC）。
            pub fn open_window(&mut self, qpc: i64) {
                if let Some(acc) = &mut self.acc {
                    acc.window_start = Some(qpc);
                }
            }

            /// 計測区間を閉じる（`bestmove` 受信直後の QPC）。
            /// 遅延到着した区間内イベントは close 後も集計される。
            pub fn close_window(&mut self, qpc: i64) {
                if let Some(acc) = &mut self.acc {
                    acc.window_end = Some(qpc);
                }
            }

            /// 積算結果と欠落診断を取り出し、積算器と対象 PID の TID 所属を破棄する。
            pub fn take_totals(&mut self) -> Option<PmcTotals> {
                let acc = self.acc.take()?;
                self.tracker.clear();
                Some(PmcTotals {
                    totals: acc.totals[..acc.n_counters].to_vec(),
                    attributed_switches: acc.attributed_switches,
                    regressed_switches: acc.regressed_switches,
                    chain_breaks_target: acc.chain_breaks_target,
                    pmc_gaps_target: acc.pmc_gaps_target,
                    tid_reuse_target: acc.tid_reuse_target,
                    unclosed_target_slices: acc.unclosed_target_slices(),
                })
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            const PID: u32 = 100;
            const TID: u32 = 1001;
            const OTHER: u32 = 999;

            fn cswitch(
                cpu: u16,
                timestamp: i64,
                old_tid: u32,
                new_tid: u32,
                values: &[u64],
            ) -> CSwitchSample {
                let mut counters = [0u64; MAX_PMC_SOURCES];
                counters[..values.len()].copy_from_slice(values);
                CSwitchSample {
                    cpu,
                    timestamp,
                    old_tid,
                    new_tid,
                    counters,
                    len: values.len(),
                }
            }

            fn state_with_target() -> PmcEngineState {
                let mut state = PmcEngineState::default();
                state.on_thread_start(PID, TID);
                state.install(PID, 2);
                state.open_window(0);
                state
            }

            fn state_with_window(open: i64, close: i64) -> PmcEngineState {
                let mut state = PmcEngineState::default();
                state.on_thread_start(PID, TID);
                state.install(PID, 2);
                state.open_window(open);
                state.close_window(close);
                state
            }

            fn assert_complete(t: &PmcTotals) {
                assert_eq!(t.chain_breaks_target, 0);
                assert_eq!(t.pmc_gaps_target, 0);
                assert_eq!(t.tid_reuse_target, 0);
                assert_eq!(t.unclosed_target_slices, 0);
            }

            #[test]
            fn delta_attributed_to_old_thread_in_window() {
                let mut state = state_with_target();
                state.on_cswitch(&cswitch(0, 10, OTHER, TID, &[1000, 2000]));
                state.on_cswitch(&cswitch(0, 20, TID, OTHER, &[1500, 2600]));
                let t = state.take_totals().expect("accumulator installed");
                assert_eq!(t.totals, vec![500, 600]);
                assert_eq!(t.attributed_switches, 1);
                assert_complete(&t);
            }

            #[test]
            fn first_cswitch_on_cpu_only_records_baseline() {
                let mut state = state_with_target();
                // 前回値が無い CPU では差分を計算できない
                state.on_cswitch(&cswitch(0, 10, TID, OTHER, &[1000, 2000]));
                let t = state.take_totals().expect("accumulator installed");
                assert_eq!(t.totals, vec![0, 0]);
                assert_eq!(t.attributed_switches, 0);
                assert_complete(&t);
            }

            #[test]
            fn non_target_tid_updates_baseline_without_attribution() {
                let mut state = state_with_target();
                state.on_cswitch(&cswitch(0, 10, OTHER, 888, &[1000, 2000]));
                state.on_cswitch(&cswitch(0, 20, 888, TID, &[1500, 2600]));
                // 直後に target が switch out: 基準値は 888 の切り替え時点まで進んでいる
                state.on_cswitch(&cswitch(0, 30, TID, OTHER, &[1600, 2700]));
                let t = state.take_totals().expect("accumulator installed");
                assert_eq!(t.totals, vec![100, 100]);
                assert_complete(&t);
            }

            #[test]
            fn events_before_window_open_are_excluded() {
                let mut state = PmcEngineState::default();
                state.on_thread_start(PID, TID);
                state.install(PID, 2);
                // window 未オープンの間は一切集計しない（基準値の更新のみ）
                state.on_cswitch(&cswitch(0, 10, OTHER, TID, &[1000, 2000]));
                state.on_cswitch(&cswitch(0, 20, TID, OTHER, &[1500, 2600]));
                let t = state.take_totals().expect("accumulator installed");
                assert_eq!(t.totals, vec![0, 0]);
                assert_eq!(t.attributed_switches, 0);
                assert_complete(&t);
            }

            #[test]
            fn open_boundary_slice_is_prorated() {
                let mut state = PmcEngineState::default();
                state.on_thread_start(PID, TID);
                state.install(PID, 2);
                state.open_window(100);
                state.on_cswitch(&cswitch(0, 60, OTHER, TID, &[1000, 2000]));
                // スライス [60,140] のうち window [100,∞) と重なるのは後半 40/80 = 50%
                state.on_cswitch(&cswitch(0, 140, TID, OTHER, &[1500, 2600]));
                let t = state.take_totals().expect("accumulator installed");
                assert_eq!(t.totals, vec![250, 300]);
                assert_eq!(t.attributed_switches, 1);
                assert_complete(&t);
            }

            #[test]
            fn close_boundary_slice_is_prorated() {
                let mut state = state_with_target();
                state.on_cswitch(&cswitch(0, 10, OTHER, TID, &[1000, 2000]));
                state.close_window(20);
                // スライス [10,30] のうち window [0,20] と重なるのは前半 10/20 = 50%
                state.on_cswitch(&cswitch(0, 30, TID, OTHER, &[1500, 2600]));
                let t = state.take_totals().expect("accumulator installed");
                assert_eq!(t.totals, vec![250, 300]);
                assert_eq!(t.attributed_switches, 1);
                assert_complete(&t);
            }

            #[test]
            fn slice_spanning_entire_window_is_prorated() {
                let mut state = state_with_window(100, 200);
                state.on_cswitch(&cswitch(0, 50, OTHER, TID, &[1000, 1000]));
                // スライス [50,250] が window [100,200] を両側に跨ぐ → 100/200 = 50%
                state.on_cswitch(&cswitch(0, 250, TID, OTHER, &[1400, 1600]));
                let t = state.take_totals().expect("accumulator installed");
                assert_eq!(t.totals, vec![200, 300]);
                assert_eq!(t.attributed_switches, 1);
                assert_complete(&t);
            }

            #[test]
            fn slice_entirely_outside_window_is_excluded() {
                let mut state = state_with_window(100, 200);
                // close 後に始まり close 後に終わるスライス
                state.on_cswitch(&cswitch(0, 210, OTHER, TID, &[1000, 1000]));
                state.on_cswitch(&cswitch(0, 220, TID, OTHER, &[1500, 1500]));
                // open 前に始まり open 前に終わるスライス（別 CPU）
                state.on_cswitch(&cswitch(1, 10, OTHER, TID, &[3000, 3000]));
                state.on_cswitch(&cswitch(1, 90, TID, OTHER, &[3500, 3500]));
                let t = state.take_totals().expect("accumulator installed");
                assert_eq!(t.totals, vec![0, 0]);
                assert_eq!(t.attributed_switches, 0);
                assert_complete(&t);
            }

            #[test]
            fn proration_rounds_to_nearest() {
                let mut state = state_with_target();
                state.on_cswitch(&cswitch(0, 5, OTHER, TID, &[1000, 1000]));
                state.close_window(10);
                // スライス [5,20] のうち window [0,10] との重なりは 5/15 = 1/3。
                // 差分 [10,20] → [3.33..,6.66..] → 最近接丸めで [3,7]
                state.on_cswitch(&cswitch(0, 20, TID, OTHER, &[1010, 1020]));
                let t = state.take_totals().expect("accumulator installed");
                assert_eq!(t.totals, vec![3, 7]);
                assert_eq!(t.attributed_switches, 1);
            }

            #[test]
            fn zero_length_slice_counts_fully_when_inside_window() {
                let mut state = state_with_target();
                state.on_cswitch(&cswitch(0, 10, OTHER, TID, &[1000, 1000]));
                // 長さ 0 のスライスは終端が window 内なら全額
                state.on_cswitch(&cswitch(0, 10, TID, OTHER, &[1005, 1006]));
                let t = state.take_totals().expect("accumulator installed");
                assert_eq!(t.totals, vec![5, 6]);
                assert_eq!(t.attributed_switches, 1);
            }

            #[test]
            fn late_arriving_event_inside_window_is_still_counted() {
                let mut state = state_with_target();
                state.on_cswitch(&cswitch(0, 10, OTHER, TID, &[1000, 2000]));
                // 区間を閉じた後に、区間内タイムスタンプのイベントが遅延到着するケース
                state.close_window(100);
                state.on_cswitch(&cswitch(0, 50, TID, OTHER, &[1200, 2300]));
                let t = state.take_totals().expect("accumulator installed");
                assert_eq!(t.totals, vec![200, 300]);
                assert_complete(&t);
            }

            #[test]
            fn counter_regression_is_skipped_and_counted() {
                let mut state = state_with_target();
                state.on_cswitch(&cswitch(0, 10, OTHER, TID, &[1000, 2000]));
                // 巻き戻った値は差分にしない（基準値は更新され、診断カウンタに乗る）
                state.on_cswitch(&cswitch(0, 20, TID, OTHER, &[500, 2600]));
                state.on_cswitch(&cswitch(0, 25, OTHER, TID, &[550, 2650]));
                state.on_cswitch(&cswitch(0, 30, TID, OTHER, &[600, 2700]));
                let t = state.take_totals().expect("accumulator installed");
                assert_eq!(t.totals, vec![50, 50]);
                assert_eq!(t.attributed_switches, 1);
                assert_eq!(t.regressed_switches, 1);
            }

            #[test]
            fn per_cpu_baselines_are_independent() {
                let mut state = state_with_target();
                state.on_cswitch(&cswitch(0, 10, OTHER, TID, &[1000, 1000]));
                state.on_cswitch(&cswitch(1, 11, OTHER, TID, &[500, 500]));
                state.on_cswitch(&cswitch(0, 20, TID, OTHER, &[1100, 1200]));
                state.on_cswitch(&cswitch(1, 21, TID, OTHER, &[530, 540]));
                let t = state.take_totals().expect("accumulator installed");
                assert_eq!(t.totals, vec![130, 240]);
                assert_eq!(t.attributed_switches, 2);
                assert_complete(&t);
            }

            #[test]
            fn membership_persists_until_take_totals_clears_tracker() {
                let mut state = state_with_target();
                state.on_cswitch(&cswitch(0, 10, OTHER, TID, &[1000, 2000]));
                state.on_cswitch(&cswitch(0, 20, TID, OTHER, &[1500, 2600]));
                let t = state.take_totals().expect("accumulator installed");
                assert_eq!(t.totals, vec![500, 600]);
                // 同じ PID を再 install しても Thread Start を観測するまでは非対象
                state.install(PID, 2);
                state.open_window(0);
                state.on_cswitch(&cswitch(0, 30, OTHER, TID, &[1000, 2000]));
                state.on_cswitch(&cswitch(0, 40, TID, OTHER, &[1500, 2600]));
                let t = state.take_totals().expect("accumulator installed");
                assert_eq!(t.totals, vec![0, 0]);
            }

            #[test]
            fn thread_start_moves_reused_tid_to_new_pid_and_flags_run() {
                let mut state = state_with_target();
                // 同じ TID が別 PID で再利用されたら、旧 PID の所属からは外れ、run は拒否対象
                state.on_thread_start(PID + 1, TID);
                state.on_cswitch(&cswitch(0, 10, OTHER, TID, &[1000, 2000]));
                state.on_cswitch(&cswitch(0, 20, TID, OTHER, &[1500, 2600]));
                let t = state.take_totals().expect("accumulator installed");
                assert_eq!(t.totals, vec![0, 0]);
                assert_eq!(t.tid_reuse_target, 1);
            }

            #[test]
            fn tid_reuse_before_install_is_not_flagged() {
                let mut state = PmcEngineState::default();
                state.on_thread_start(PID, TID);
                state.on_thread_start(PID + 1, TID);
                state.install(PID, 2);
                state.open_window(0);
                let t = state.take_totals().expect("accumulator installed");
                assert_eq!(t.tid_reuse_target, 0);
            }

            #[test]
            fn thread_start_of_unrelated_tid_is_not_flagged() {
                let mut state = state_with_target();
                state.on_thread_start(PID + 1, 4242);
                let t = state.take_totals().expect("accumulator installed");
                assert_complete(&t);
            }

            #[test]
            fn reused_tid_switch_in_on_other_cpu_flags_run() {
                let mut state = state_with_target();
                // 旧対象スレッド終了後、別 CPU で再利用先 (非対象) の switch-in が
                // 遅延した Thread Start(PID+1, TID) より先に届く配送順。switch-in 時は
                // 残存所属で new_is_target=true になり差分が帰属しうるので run ごと拒否する
                state.on_cswitch(&cswitch(1, 10, OTHER, TID, &[1000, 2000]));
                state.on_thread_start(PID + 1, TID);
                state.on_cswitch(&cswitch(1, 20, TID, OTHER, &[1500, 2600]));
                let t = state.take_totals().expect("accumulator installed");
                assert_eq!(t.tid_reuse_target, 1);
            }

            #[test]
            fn delayed_switch_out_after_tid_reuse_is_attributed_but_flagged() {
                let mut state = state_with_target();
                state.on_cswitch(&cswitch(0, 10, OTHER, TID, &[1000, 2000]));
                // 別 PID で同じ TID の Thread Start が、旧スレッドの switch-out より先に届く。
                // 連鎖が正しいので switch-in 時の所属で帰属はするが、run 自体は再利用として拒否
                state.on_thread_start(PID + 1, TID);
                state.on_cswitch(&cswitch(0, 20, TID, OTHER, &[1500, 2600]));
                let t = state.take_totals().expect("accumulator installed");
                assert_eq!(t.totals, vec![500, 600]);
                assert_eq!(t.attributed_switches, 1);
                assert_eq!(t.tid_reuse_target, 1);
                assert_eq!(t.chain_breaks_target, 0);
            }

            #[test]
            fn install_resets_previous_run() {
                let mut state = state_with_target();
                state.on_cswitch(&cswitch(0, 10, OTHER, TID, &[1000, 2000]));
                state.on_cswitch(&cswitch(0, 20, TID, OTHER, &[1500, 2600]));
                state.install(PID, 2);
                state.open_window(0);
                // per-CPU 基準値もリセットされるため、最初の CSwitch は基準値記録のみ
                state.on_cswitch(&cswitch(0, 30, TID, OTHER, &[9999, 9999]));
                let t = state.take_totals().expect("accumulator installed");
                assert_eq!(t.totals, vec![0, 0]);
                assert_eq!(t.attributed_switches, 0);
            }

            #[test]
            fn pmc_gap_on_target_switch_out_is_counted() {
                let mut state = state_with_target();
                state.on_cswitch(&cswitch(0, 10, OTHER, TID, &[1000, 2000]));
                // 対象の switch-out に counter が足りない → 差分を取れず欠落
                state.on_cswitch(&cswitch(0, 15, TID, OTHER, &[9999]));
                let t = state.take_totals().expect("accumulator installed");
                assert_eq!(t.totals, vec![0, 0]);
                assert_eq!(t.pmc_gaps_target, 1);
            }

            #[test]
            fn pmc_gap_on_target_switch_in_is_detected_at_switch_out() {
                let mut state = state_with_target();
                // switch-in 側に PMC が無い: 基準は TID・時刻だけ保持し、次の対象 switch-out で欠落と判定
                state.on_cswitch(&cswitch(0, 10, OTHER, TID, &[]));
                state.on_cswitch(&cswitch(0, 20, TID, OTHER, &[1500, 2600]));
                let t = state.take_totals().expect("accumulator installed");
                assert_eq!(t.totals, vec![0, 0]);
                assert_eq!(t.attributed_switches, 0);
                assert_eq!(t.pmc_gaps_target, 1);
                assert_eq!(t.chain_breaks_target, 0);
            }

            #[test]
            fn pmc_less_event_between_other_threads_is_harmless() {
                let mut state = state_with_target();
                state.on_cswitch(&cswitch(0, 10, OTHER, 888, &[]));
                state.on_cswitch(&cswitch(0, 20, 888, TID, &[1000, 2000]));
                state.on_cswitch(&cswitch(0, 30, TID, OTHER, &[1100, 2100]));
                let t = state.take_totals().expect("accumulator installed");
                assert_eq!(t.totals, vec![100, 100]);
                assert_complete(&t);
            }

            #[test]
            fn pmc_gap_outside_window_is_not_counted() {
                let mut state = PmcEngineState::default();
                state.on_thread_start(PID, TID);
                state.install(PID, 2);
                state.open_window(100);
                state.on_cswitch(&cswitch(0, 10, OTHER, TID, &[1000, 2000]));
                state.on_cswitch(&cswitch(0, 50, TID, OTHER, &[5]));
                let t = state.take_totals().expect("accumulator installed");
                assert_eq!(t.pmc_gaps_target, 0);
            }

            #[test]
            fn chain_break_while_target_runs_is_counted() {
                let mut state = state_with_target();
                state.on_cswitch(&cswitch(0, 10, OTHER, TID, &[1000, 2000]));
                // TID が走っているはずなのに別スレッドが switch out → 間のイベント欠落
                state.on_cswitch(&cswitch(0, 20, 888, OTHER, &[1500, 2600]));
                let t = state.take_totals().expect("accumulator installed");
                assert_eq!(t.totals, vec![0, 0]);
                assert_eq!(t.chain_breaks_target, 1);
            }

            #[test]
            fn chain_break_outside_window_is_not_counted() {
                let mut state = PmcEngineState::default();
                state.on_thread_start(PID, TID);
                state.install(PID, 2);
                state.open_window(100);
                state.on_cswitch(&cswitch(0, 10, OTHER, TID, &[1000, 2000]));
                state.on_cswitch(&cswitch(0, 20, 888, OTHER, &[1500, 2600]));
                let t = state.take_totals().expect("accumulator installed");
                assert_eq!(t.chain_breaks_target, 0);
            }

            #[test]
            fn chain_break_between_other_threads_is_ignored() {
                let mut state = state_with_target();
                state.on_cswitch(&cswitch(0, 10, OTHER, 888, &[1000, 2000]));
                state.on_cswitch(&cswitch(0, 20, 777, OTHER, &[1500, 2600]));
                let t = state.take_totals().expect("accumulator installed");
                assert_eq!(t.chain_breaks_target, 0);
            }

            #[test]
            fn timestamp_reversal_while_target_runs_is_a_chain_break() {
                let mut state = state_with_target();
                state.on_cswitch(&cswitch(0, 20, OTHER, TID, &[1000, 2000]));
                state.on_cswitch(&cswitch(0, 10, TID, OTHER, &[1500, 2600]));
                let t = state.take_totals().expect("accumulator installed");
                assert_eq!(t.totals, vec![0, 0]);
                assert_eq!(t.chain_breaks_target, 1);
            }

            #[test]
            fn unclosed_target_slice_is_reported() {
                let mut state = state_with_target();
                state.on_cswitch(&cswitch(0, 10, OTHER, TID, &[1000, 2000]));
                state.close_window(100);
                // 最終 switch-out が届かないまま集計 → 未閉鎖
                let t = state.take_totals().expect("accumulator installed");
                assert_eq!(t.unclosed_target_slices, 1);
            }

            #[test]
            fn closed_target_slice_is_not_reported() {
                let mut state = state_with_target();
                state.on_cswitch(&cswitch(0, 10, OTHER, TID, &[1000, 2000]));
                state.close_window(100);
                state.on_cswitch(&cswitch(0, 120, TID, OTHER, &[1500, 2600]));
                let t = state.take_totals().expect("accumulator installed");
                assert_eq!(t.unclosed_target_slices, 0);
            }

            #[test]
            fn target_switched_in_after_close_is_not_unclosed() {
                let mut state = state_with_target();
                state.close_window(100);
                state.on_cswitch(&cswitch(0, 150, OTHER, TID, &[1000, 2000]));
                let t = state.take_totals().expect("accumulator installed");
                assert_eq!(t.unclosed_target_slices, 0);
            }

            #[test]
            fn slice_touching_window_edge_has_zero_overlap() {
                let mut state = state_with_window(100, 200);
                state.on_cswitch(&cswitch(0, 60, OTHER, TID, &[1000, 1000]));
                // スライス [60,100] は終端が open 境界ちょうど → 重なり長 0 → 除外
                state.on_cswitch(&cswitch(0, 100, TID, OTHER, &[1200, 1200]));
                state.on_cswitch(&cswitch(0, 120, OTHER, TID, &[1250, 1250]));
                // スライス [120,200] は window に完全内包 → 全額
                state.on_cswitch(&cswitch(0, 200, TID, OTHER, &[1500, 1500]));
                let t = state.take_totals().expect("accumulator installed");
                assert_eq!(t.totals, vec![250, 250]);
                assert_eq!(t.attributed_switches, 1);
                assert_complete(&t);
            }

            #[test]
            fn extra_counters_beyond_configured_are_ignored() {
                let mut state = state_with_target();
                // 設定 (n=2) より多い counter が届いた場合は先頭 n 個だけを使う
                state.on_cswitch(&cswitch(0, 10, OTHER, TID, &[1000, 2000, 111]));
                state.on_cswitch(&cswitch(0, 20, TID, OTHER, &[1500, 2600, 999]));
                let t = state.take_totals().expect("accumulator installed");
                assert_eq!(t.totals, vec![500, 600]);
                assert_eq!(t.attributed_switches, 1);
            }

            #[test]
            fn install_clamps_counter_count_to_max() {
                let mut state = PmcEngineState::default();
                state.install(PID, MAX_PMC_SOURCES + 5);
                let t = state.take_totals().expect("accumulator installed");
                assert_eq!(t.totals.len(), MAX_PMC_SOURCES);
            }
        }
    }

    /// ETW (NT Kernel Logger) の unsafe FFI 層。
    ///
    /// セッションの開始/停止・PMC 設定・real-time consumer を担い、イベントの解釈は
    /// `pmc` モジュールの純ロジックへ移譲する。
    mod etw {
        use std::ffi::c_void;
        use std::ptr::null;
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::sync::{Arc, Mutex};
        use std::thread::{self, JoinHandle};

        use anyhow::{Context, Result, bail};
        use std::time::{Duration, Instant};
        use windows_sys::Win32::Foundation::{
            ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, ERROR_BAD_LENGTH, ERROR_CANCELLED,
            ERROR_INSUFFICIENT_BUFFER, ERROR_MORE_DATA, ERROR_SUCCESS,
            ERROR_WMI_INSTANCE_NOT_FOUND,
        };
        use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
        use windows_sys::Win32::System::Diagnostics::Etw::{
            CLASSIC_EVENT_ID, CONTROLTRACE_HANDLE, CloseTrace, ControlTraceW,
            EVENT_HEADER_EXT_TYPE_PMC_COUNTERS, EVENT_HEADER_FLAG_PROCESSOR_INDEX, EVENT_RECORD,
            EVENT_TRACE_CONTROL_QUERY, EVENT_TRACE_CONTROL_STOP, EVENT_TRACE_FLAG_CSWITCH,
            EVENT_TRACE_FLAG_THREAD, EVENT_TRACE_LOGFILEW, EVENT_TRACE_LOGFILEW_0,
            EVENT_TRACE_LOGFILEW_1, EVENT_TRACE_PROPERTIES, EVENT_TRACE_REAL_TIME_MODE,
            KERNEL_LOGGER_NAMEW, OpenTraceW, PROCESS_TRACE_MODE_EVENT_RECORD,
            PROCESS_TRACE_MODE_RAW_TIMESTAMP, PROCESS_TRACE_MODE_REAL_TIME, PROCESSTRACE_HANDLE,
            PROFILE_SOURCE_INFO, ProcessTrace, StartTraceW, SystemTraceControlGuid,
            TracePmcCounterListInfo, TracePmcEventListInfo, TraceProfileSourceListInfo,
            TraceQueryInformation, TraceSetInformation, WNODE_FLAG_TRACED_GUID,
        };

        use super::pmc::{CSwitchSample, MAX_PMC_SOURCES, PmcEngineState};

        /// NT カーネル Thread プロバイダの GUID {3d6fa8d1-fe05-11d0-9dda-00c04fd7ba7c}。
        /// CSwitch / Thread Start イベントはこの provider で届く。
        const THREAD_PROVIDER_GUID: windows_sys::core::GUID =
            windows_sys::core::GUID::from_u128(0x3d6fa8d1_fe05_11d0_9dda_00c04fd7ba7c);

        const CSWITCH_OPCODE: u8 = 36;
        const THREAD_START_OPCODE: u8 = 1;
        const THREAD_DC_START_OPCODE: u8 = 3;

        fn guid_eq(a: &windows_sys::core::GUID, b: &windows_sys::core::GUID) -> bool {
            a.data1 == b.data1 && a.data2 == b.data2 && a.data3 == b.data3 && a.data4 == b.data4
        }

        /// LoggerName / LogFileName 用に確保する EVENT_TRACE_PROPERTIES 後続領域（バイト）。
        const PROPS_NAME_AREA: usize = 2 * 1024;

        /// `EVENT_TRACE_PROPERTIES` + 名前領域を 8 byte alignment で確保するバッファ。
        struct PropsBuf(Vec<u64>);

        impl PropsBuf {
            fn new() -> Self {
                let total = size_of::<EVENT_TRACE_PROPERTIES>() + PROPS_NAME_AREA;
                let mut buf = PropsBuf(vec![0u64; total.div_ceil(size_of::<u64>())]);
                let props = buf.props_mut();
                props.Wnode.BufferSize = total as u32;
                props.Wnode.Guid = SystemTraceControlGuid;
                // ClientContext=1: イベントのタイムスタンプを QPC で記録する
                props.Wnode.ClientContext = 1;
                props.Wnode.Flags = WNODE_FLAG_TRACED_GUID;
                props.LoggerNameOffset = size_of::<EVENT_TRACE_PROPERTIES>() as u32;
                props.LogFileNameOffset = 0;
                buf
            }

            fn props_mut(&mut self) -> &mut EVENT_TRACE_PROPERTIES {
                // SAFETY:
                // - バッファは Vec<u64> なので 8 byte alignment を満たし、
                //   EVENT_TRACE_PROPERTIES + 名前領域ぶんの長さを確保済み。
                // - 全ビット 0 は EVENT_TRACE_PROPERTIES の有効なビットパターン。
                unsafe { &mut *(self.0.as_mut_ptr() as *mut EVENT_TRACE_PROPERTIES) }
            }

            fn as_mut_ptr(&mut self) -> *mut EVENT_TRACE_PROPERTIES {
                self.0.as_mut_ptr() as *mut EVENT_TRACE_PROPERTIES
            }
        }

        /// NT Kernel Logger セッションの RAII guard。drop で必ず stop する。
        pub struct KernelSession {
            handle: CONTROLTRACE_HANDLE,
            stopped: bool,
        }

        impl KernelSession {
            /// NT Kernel Logger を real-time mode で開始し、CSwitch への PMC counter
            /// 添付を設定する。既存セッションが生きていたら stop してから開始する。
            pub fn start(pmc_source_indices: &[u32]) -> Result<Self> {
                let mut handle = CONTROLTRACE_HANDLE { Value: 0 };
                let mut status = start_trace_once(&mut handle);
                if status == ERROR_ALREADY_EXISTS {
                    stop_existing_session()?;
                    status = start_trace_once(&mut handle);
                }
                match status {
                    ERROR_SUCCESS => {}
                    ERROR_ACCESS_DENIED => bail!(
                        "NT Kernel Logger セッションを開始できません (ERROR_ACCESS_DENIED)。\n\
                         ETW PMC counting には管理者権限が必要です。管理者として実行し直してください。"
                    ),
                    other => bail!("StartTraceW(NT Kernel Logger) が失敗しました (code {other})"),
                }

                // ここから先のエラーでも Self の Drop でセッションが stop される
                let session = Self {
                    handle,
                    stopped: false,
                };

                // Ctrl+C 等でプロセスが即死するとセッションが OS に残るため、
                // 終了前に stop を試みるハンドラを登録する
                CTRL_SESSION_HANDLE.store(handle.Value, Ordering::SeqCst);
                // SAFETY: console_ctrl_handler は 'static な関数ポインタ。登録に失敗しても
                // 計測自体は続行できるため戻り値は無視する。
                let _ = unsafe { SetConsoleCtrlHandler(Some(console_ctrl_handler), 1) };

                session.set_pmc_counter_list(pmc_source_indices)?;
                session.set_pmc_event_list()?;
                Ok(session)
            }

            fn set_pmc_counter_list(&self, indices: &[u32]) -> Result<()> {
                // SAFETY:
                // - handle は StartTraceW で得た有効なセッションハンドル。
                // - indices は profile source index の配列で、長さをバイト数で渡す。
                let status = unsafe {
                    TraceSetInformation(
                        self.handle,
                        TracePmcCounterListInfo,
                        indices.as_ptr().cast(),
                        size_of_val(indices) as u32,
                    )
                };
                if status != ERROR_SUCCESS {
                    bail!(
                        "TraceSetInformation(TracePmcCounterListInfo) が失敗しました (code {status})。\n\
                         `wpr -pmcsources` に列挙される source 名か、他の PMC 利用セッション\n\
                         (wpr 等) が動いていないかを確認してください。"
                    );
                }
                Ok(())
            }

            fn set_pmc_event_list(&self) -> Result<()> {
                let ids = [CLASSIC_EVENT_ID {
                    EventGuid: THREAD_PROVIDER_GUID,
                    Type: CSWITCH_OPCODE,
                    Reserved: [0; 7],
                }];
                // SAFETY:
                // - handle は有効なセッションハンドル。
                // - ids は CLASSIC_EVENT_ID の配列で、サイズをバイト数で渡す。
                let status = unsafe {
                    TraceSetInformation(
                        self.handle,
                        TracePmcEventListInfo,
                        ids.as_ptr().cast(),
                        size_of_val(&ids) as u32,
                    )
                };
                if status != ERROR_SUCCESS {
                    bail!(
                        "TraceSetInformation(TracePmcEventListInfo) が失敗しました (code {status})"
                    );
                }
                Ok(())
            }

            /// セッション統計からイベントロス数 (EventsLost, RealTimeBuffersLost の累計)
            /// を取得する。run 前後の増分検査に使う。
            pub fn query_lost_counts(&self) -> Result<LostCounts> {
                let mut props = PropsBuf::new();
                // SAFETY: handle は有効なセッションハンドル。props は有効なバッファで、
                // ControlTraceW(query) が統計を書き込む。
                let status = unsafe {
                    ControlTraceW(
                        self.handle,
                        null(),
                        props.as_mut_ptr(),
                        EVENT_TRACE_CONTROL_QUERY,
                    )
                };
                if status != ERROR_SUCCESS {
                    bail!("ControlTraceW(query) が失敗しました (code {status})");
                }
                let p = props.props_mut();
                Ok(LostCounts {
                    events_lost: p.EventsLost,
                    realtime_buffers_lost: p.RealTimeBuffersLost,
                })
            }

            /// セッションを停止する。STOP は CPU が保持中の未満杯バッファも含めて全部を
            /// consumer へ配送してから完了するので、この後 consumer の ProcessTrace が
            /// 自然終了するのを待てば末尾のイベントまで回収できる (FLUSH + 固定時間待ちでは
            /// pin コアの未満杯バッファが配送されないことがある)。戻り値は STOP 時点の累計ロス数。
            pub fn stop(&mut self) -> Result<LostCounts> {
                let mut props = PropsBuf::new();
                // SAFETY: handle は有効なセッションハンドル。props は有効なバッファ。
                let status = unsafe {
                    ControlTraceW(self.handle, null(), props.as_mut_ptr(), EVENT_TRACE_CONTROL_STOP)
                };
                if status != ERROR_SUCCESS && status != ERROR_MORE_DATA {
                    // 失敗時は stopped のままにせず、Drop の停止処理に委ねる
                    bail!("ControlTraceW(stop) が失敗しました (code {status})");
                }
                self.stopped = true;
                CTRL_SESSION_HANDLE.store(0, Ordering::SeqCst);
                // SAFETY: console_ctrl_handler は登録時と同じ 'static な関数ポインタ。
                let _ = unsafe { SetConsoleCtrlHandler(Some(console_ctrl_handler), 0) };
                let p = props.props_mut();
                Ok(LostCounts {
                    events_lost: p.EventsLost,
                    realtime_buffers_lost: p.RealTimeBuffersLost,
                })
            }
        }

        impl Drop for KernelSession {
            fn drop(&mut self) {
                if self.stopped {
                    return;
                }
                // 正常経路で stop するため、Ctrl ハンドラからの二重 stop を解除する
                CTRL_SESSION_HANDLE.store(0, Ordering::SeqCst);
                // SAFETY: console_ctrl_handler は登録時と同じ 'static な関数ポインタ。
                let _ = unsafe { SetConsoleCtrlHandler(Some(console_ctrl_handler), 0) };
                let mut props = PropsBuf::new();
                // SAFETY: handle は有効なセッションハンドル。props は有効なバッファ。
                // drop 中なのでエラーは無視する。
                let _ = unsafe {
                    ControlTraceW(self.handle, null(), props.as_mut_ptr(), EVENT_TRACE_CONTROL_STOP)
                };
            }
        }

        /// ETW セッション統計のイベントロス数 (累計値)。
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub struct LostCounts {
            pub events_lost: u32,
            pub realtime_buffers_lost: u32,
        }

        /// Ctrl+C 等での強制終了時に stop すべきセッションハンドル (0 = なし)。
        static CTRL_SESSION_HANDLE: AtomicU64 = AtomicU64::new(0);

        /// コンソール制御イベント (Ctrl+C / Ctrl+Break / close) のハンドラ。
        ///
        /// プロセスが即死すると NT Kernel Logger セッションが OS に残るため、終了前に
        /// stop を試みる。FALSE を返して既定の終了処理 (プロセス終了) へ委ねる。
        unsafe extern "system" fn console_ctrl_handler(_ctrl_type: u32) -> windows_sys::core::BOOL {
            let value = CTRL_SESSION_HANDLE.swap(0, Ordering::SeqCst);
            if value != 0 {
                let mut props = PropsBuf::new();
                // SAFETY: value は KernelSession::start が登録した有効なセッションハンドル。
                // swap により handler 側の stop は高々 1 回。Drop 側の stop とは重複
                // しうるが、2 回目は ERROR_WMI_INSTANCE_NOT_FOUND になるだけで無害。
                let _ = unsafe {
                    ControlTraceW(
                        CONTROLTRACE_HANDLE { Value: value },
                        null(),
                        props.as_mut_ptr(),
                        EVENT_TRACE_CONTROL_STOP,
                    )
                };
            }
            0
        }

        fn start_trace_once(handle: &mut CONTROLTRACE_HANDLE) -> u32 {
            let mut props = PropsBuf::new();
            {
                let p = props.props_mut();
                p.BufferSize = 64; // KB
                p.MinimumBuffers = 64;
                p.MaximumBuffers = 256;
                p.FlushTimer = 1; // 秒。real-time 配送の遅延上限を短くする
                p.LogFileMode = EVENT_TRACE_REAL_TIME_MODE;
                // 消費するのは Thread Start (TID 追跡) と CSwitch (PMC 添付) のみ
                p.EnableFlags = EVENT_TRACE_FLAG_THREAD | EVENT_TRACE_FLAG_CSWITCH;
            }
            // SAFETY:
            // - props は必要サイズを満たす有効なバッファで、呼び出しの間有効。
            // - KERNEL_LOGGER_NAMEW は NUL 終端の静的ワイド文字列。
            unsafe { StartTraceW(handle, KERNEL_LOGGER_NAMEW, props.as_mut_ptr()) }
        }

        /// 前回の異常終了等で残った NT Kernel Logger セッションを停止する。
        fn stop_existing_session() -> Result<()> {
            // stdout に出す: 昇格 runner (PowerShell 5.1, $ErrorActionPreference=Stop) は
            // native の stderr 出力を終了エラー扱いにしてプロセスを落とすため
            println!(
                "既存の NT Kernel Logger セッションを検出したため停止して回収します \
                 (前回の異常終了、または xperf/wpr 等の並行セッション)"
            );
            let mut props = PropsBuf::new();
            // SAFETY: セッション名指定 (ハンドル 0) の stop。props は有効なバッファ。
            let status = unsafe {
                ControlTraceW(
                    CONTROLTRACE_HANDLE { Value: 0 },
                    KERNEL_LOGGER_NAMEW,
                    props.as_mut_ptr(),
                    EVENT_TRACE_CONTROL_STOP,
                )
            };
            if status != ERROR_SUCCESS
                && status != ERROR_WMI_INSTANCE_NOT_FOUND
                && status != ERROR_MORE_DATA
            {
                bail!("既存の NT Kernel Logger セッションの停止に失敗しました (code {status})");
            }
            Ok(())
        }

        /// システムの PMC profile source 一覧 (名前, index) を取得する。
        /// `wpr -pmcsources` と同等の情報。
        pub fn query_profile_sources() -> Result<Vec<(String, u32)>> {
            let mut buf_len = 64 * 1024usize;
            loop {
                let mut buf = vec![0u64; buf_len.div_ceil(size_of::<u64>())];
                let mut ret_len: u32 = 0;
                // SAFETY: buf は buf_len byte 以上の書き込み可能領域。
                let status = unsafe {
                    TraceQueryInformation(
                        CONTROLTRACE_HANDLE { Value: 0 },
                        TraceProfileSourceListInfo,
                        buf.as_mut_ptr().cast(),
                        buf_len as u32,
                        &mut ret_len,
                    )
                };
                if status == ERROR_SUCCESS {
                    let used = if ret_len == 0 {
                        buf_len
                    } else {
                        (ret_len as usize).min(buf_len)
                    };
                    return Ok(parse_profile_source_list(&buf, used));
                }
                let grow = status == ERROR_INSUFFICIENT_BUFFER
                    || status == ERROR_BAD_LENGTH
                    || status == ERROR_MORE_DATA;
                if grow && buf_len < (1 << 22) {
                    buf_len *= 2;
                    continue;
                }
                bail!(
                    "TraceQueryInformation(TraceProfileSourceListInfo) が失敗しました \
                     (code {status})。この環境では ETW PMC counting が使えない可能性があります。"
                );
            }
        }

        /// PROFILE_SOURCE_INFO の可変長チェイン（NextEntryOffset 連結）をパースする。
        fn parse_profile_source_list(buf: &[u64], byte_len: usize) -> Vec<(String, u32)> {
            let bytes_ptr = buf.as_ptr() as *const u8;
            let byte_len = byte_len.min(size_of_val(buf));
            let fixed_len = size_of::<PROFILE_SOURCE_INFO>();
            let desc_field_offset = std::mem::offset_of!(PROFILE_SOURCE_INFO, Description);
            let mut out = Vec::new();
            let mut offset = 0usize;
            while offset + fixed_len <= byte_len {
                // SAFETY: offset + fixed_len <= byte_len を確認済みなので、固定部を
                // read_unaligned で読める（バッファ境界内）。
                let info = unsafe {
                    (bytes_ptr.add(offset) as *const PROFILE_SOURCE_INFO).read_unaligned()
                };
                // Description は固定部内の可変長フィールド開始位置から NUL 終端 UTF-16
                let mut name_units = Vec::new();
                let mut pos = offset + desc_field_offset;
                while pos + size_of::<u16>() <= byte_len {
                    // SAFETY: pos + 2 <= byte_len を確認済み。
                    let unit = unsafe { (bytes_ptr.add(pos) as *const u16).read_unaligned() };
                    if unit == 0 {
                        break;
                    }
                    name_units.push(unit);
                    pos += size_of::<u16>();
                }
                out.push((String::from_utf16_lossy(&name_units), info.Source));
                if info.NextEntryOffset == 0 {
                    break;
                }
                let next = offset + info.NextEntryOffset as usize;
                if next <= offset {
                    break;
                }
                offset = next;
            }
            out
        }

        /// real-time consumer。OpenTrace + ProcessTrace を専用スレッドで回し、
        /// イベントを `PmcEngineState` へ流し込む。
        pub struct Consumer {
            handle: PROCESSTRACE_HANDLE,
            thread: Option<JoinHandle<u32>>,
            /// コールバックが Context 経由で参照するため、consumer スレッド join まで
            /// Arc を所有し続ける（解放順の保証）。
            shared: Option<Arc<Mutex<PmcEngineState>>>,
        }

        impl Consumer {
            pub fn start(shared: Arc<Mutex<PmcEngineState>>) -> Result<Self> {
                let mut logger_name: Vec<u16> =
                    "NT Kernel Logger".encode_utf16().chain(std::iter::once(0)).collect();
                let mut logfile = EVENT_TRACE_LOGFILEW {
                    LoggerName: logger_name.as_mut_ptr(),
                    Anonymous1: EVENT_TRACE_LOGFILEW_0 {
                        ProcessTraceMode: PROCESS_TRACE_MODE_REAL_TIME
                            | PROCESS_TRACE_MODE_EVENT_RECORD
                            | PROCESS_TRACE_MODE_RAW_TIMESTAMP,
                    },
                    Anonymous2: EVENT_TRACE_LOGFILEW_1 {
                        EventRecordCallback: Some(event_record_callback),
                    },
                    Context: Arc::as_ptr(&shared) as *mut c_void,
                    ..Default::default()
                };

                // SAFETY: logfile と logger_name は呼び出しの間有効。OpenTraceW は内容を
                // コピーするため、復帰後にローカルを解放してよい。
                let handle = unsafe { OpenTraceW(&mut logfile) };
                if handle.Value == u64::MAX {
                    return Err(std::io::Error::last_os_error())
                        .context("OpenTraceW(NT Kernel Logger) が失敗しました");
                }

                let thread_handle = PROCESSTRACE_HANDLE {
                    Value: handle.Value,
                };
                let spawned =
                    thread::Builder::new().name("etw-consumer".to_string()).spawn(move || {
                        // SAFETY: handle は OpenTraceW が返した有効な処理ハンドル。
                        // ProcessTrace はセッション停止か CloseTrace まで block する。
                        unsafe { ProcessTrace(&thread_handle, 1, null(), null()) }
                    });
                let thread = match spawned {
                    Ok(thread) => thread,
                    Err(err) => {
                        // spawn 失敗時に trace handle をリークさせない
                        // SAFETY: handle は OpenTraceW で得た有効なハンドルで、ProcessTrace
                        // は未開始のため close してよい。
                        let _ = unsafe { CloseTrace(handle) };
                        return Err(err).context("ETW consumer スレッドの起動に失敗しました");
                    }
                };

                Ok(Self {
                    handle,
                    thread: Some(thread),
                    shared: Some(shared),
                })
            }
        }

        impl Consumer {
            /// セッション STOP 後に ProcessTrace が自然終了する (= 残りのバッファを
            /// 全部処理した) のを待つ。timeout 内に終わらなければ CloseTrace で
            /// 打ち切り、エラーにする (末尾が欠けた値を正常値として返さない)。
            pub fn drain(mut self, timeout: Duration) -> Result<()> {
                let status = self.shutdown(timeout)?;
                if status != ERROR_SUCCESS && status != ERROR_CANCELLED {
                    bail!("ProcessTrace が異常終了しました (code {status})");
                }
                Ok(())
            }

            /// consumer スレッドを有限時間で片付ける。`natural_wait` の間は ProcessTrace の
            /// 自然終了を待ち、間に合わなければ CloseTrace で打ち切って DETACH_GRACE だけ
            /// 待つ。それでも終わらなければスレッドを detach し、コールバックが Context
            /// 経由で触る Arc は解放せず意図的にリークする。
            fn shutdown(&mut self, natural_wait: Duration) -> Result<u32> {
                let Some(thread) = self.thread.take() else {
                    return Ok(ERROR_SUCCESS);
                };
                let finished = wait_finished(&thread, natural_wait);
                if !finished {
                    // SAFETY: handle は OpenTraceW で得た有効なハンドル。
                    let _ = unsafe { CloseTrace(self.handle) };
                    if !wait_finished(&thread, DETACH_GRACE) {
                        if let Some(shared) = self.shared.take() {
                            std::mem::forget(shared);
                        }
                        drop(thread);
                        bail!(
                            "ETW consumer: {natural_wait:?} + {DETACH_GRACE:?} 以内に ProcessTrace が \
                             終了しませんでした (末尾イベントの回収不能)"
                        );
                    }
                }
                let joined = thread.join();
                if finished {
                    // SAFETY: ProcessTrace 復帰後の handle を閉じる (打ち切り経路では閉じ済み)。
                    let _ = unsafe { CloseTrace(self.handle) };
                }
                self.shared.take();
                let status = joined
                    .map_err(|_| anyhow::anyhow!("ETW consumer スレッドが panic しました"))?;
                if !finished {
                    bail!(
                        "ETW consumer: {natural_wait:?} 以内に ProcessTrace が終了せず打ち切りました \
                         (末尾イベントの回収不能)"
                    );
                }
                Ok(status)
            }
        }

        impl Drop for Consumer {
            fn drop(&mut self) {
                // drain されなかった経路 (初期化失敗 / STOP 失敗) でも無期限に待たない
                let _ = self.shutdown(Duration::ZERO);
            }
        }

        /// CloseTrace 後に ProcessTrace の復帰を待つ猶予。
        const DETACH_GRACE: Duration = Duration::from_secs(5);

        /// スレッドの終了を timeout まで 20ms 間隔で待つ。
        fn wait_finished(thread: &JoinHandle<u32>, timeout: Duration) -> bool {
            let deadline = Instant::now() + timeout;
            while !thread.is_finished() {
                if Instant::now() >= deadline {
                    return false;
                }
                thread::sleep(Duration::from_millis(20));
            }
            true
        }

        /// ProcessTrace から呼ばれるイベントコールバック。
        ///
        /// FFI 境界の外へ panic を漏らせないため、失敗しうる操作（unwrap 等）は置かない。
        unsafe extern "system" fn event_record_callback(record: *mut EVENT_RECORD) {
            if record.is_null() {
                return;
            }
            // SAFETY: ProcessTrace は有効な EVENT_RECORD を渡す契約。
            let record = unsafe { &*record };
            let shared = record.UserContext as *const Mutex<PmcEngineState>;
            if shared.is_null() {
                return;
            }
            // SAFETY: UserContext は Consumer::start が渡した Arc<Mutex<..>> の生ポインタ。
            // Consumer が consumer スレッド join まで Arc を保持するため常に有効。
            let shared = unsafe { &*shared };

            if !guid_eq(&record.EventHeader.ProviderId, &THREAD_PROVIDER_GUID) {
                return;
            }
            match record.EventHeader.EventDescriptor.Opcode {
                CSWITCH_OPCODE => handle_cswitch(record, shared),
                THREAD_START_OPCODE | THREAD_DC_START_OPCODE => {
                    if let Some((pid, tid)) = read_pid_tid(record)
                        && let Ok(mut state) = shared.lock()
                    {
                        state.on_thread_start(pid, tid);
                    }
                }
                _ => {}
            }
        }

        /// CSwitch イベントから (CPU, QPC, OldThreadId, PMC 値) を取り出して積算する。
        fn handle_cswitch(record: &EVENT_RECORD, shared: &Mutex<PmcEngineState>) {
            // CSwitch UserData 先頭 8 byte: NewThreadId(u32), OldThreadId(u32)
            if (record.UserDataLength as usize) < 8 || record.UserData.is_null() {
                return;
            }
            // SAFETY: UserData は UserDataLength byte の有効領域で、8 byte 以上あることを
            // 確認済み。read_unaligned なので alignment 要件はない。
            let old_tid =
                unsafe { (record.UserData as *const u8).add(4).cast::<u32>().read_unaligned() };
            // SAFETY: 同上。先頭 4 byte が NewThreadId。
            let new_tid = unsafe { record.UserData.cast::<u32>().read_unaligned() };

            let mut counters = [0u64; MAX_PMC_SOURCES];
            let mut len = 0usize;
            let ext_count = if record.ExtendedData.is_null() {
                0
            } else {
                record.ExtendedDataCount as usize
            };
            for i in 0..ext_count {
                // SAFETY: ExtendedData は ExtendedDataCount 要素の有効配列。
                let item = unsafe { record.ExtendedData.add(i).read_unaligned() };
                if u32::from(item.ExtType) != EVENT_HEADER_EXT_TYPE_PMC_COUNTERS
                    || item.DataPtr == 0
                {
                    continue;
                }
                let count = (item.DataSize as usize / size_of::<u64>()).min(MAX_PMC_SOURCES);
                for (j, slot) in counters.iter_mut().take(count).enumerate() {
                    // SAFETY: DataPtr は DataSize byte の有効領域を指し、j*8+8 <= DataSize。
                    *slot = unsafe { (item.DataPtr as *const u64).add(j).read_unaligned() };
                }
                len = count;
                break;
            }
            // PMC の付かない CSwitch (len == 0) も積算器へ渡す。差分は取れないが、
            // 対象→他→対象の切替を見落とすと他スレッド分が対象に帰属するため、
            // 基準の無効化と欠落検出には必要。

            // EVENT_HEADER_FLAG_PROCESSOR_INDEX が立っていれば union は u16 の
            // ProcessorIndex として書かれている。立っていなければ従来レイアウト
            // (ProcessorNumber: u8 + Alignment) なので下位 byte だけを読む。
            let flags = u32::from(record.EventHeader.Flags);
            let cpu = if flags & EVENT_HEADER_FLAG_PROCESSOR_INDEX != 0 {
                // SAFETY: flag 設定時は ProcessorIndex (u16) が有効メンバ。
                unsafe { record.BufferContext.Anonymous.ProcessorIndex }
            } else {
                // SAFETY: flag 未設定時は ProcessorNumber (u8) が有効メンバ。
                u16::from(unsafe { record.BufferContext.Anonymous.Anonymous.ProcessorNumber })
            };
            let sample = CSwitchSample {
                cpu,
                // ClientContext=1 + PROCESS_TRACE_MODE_RAW_TIMESTAMP により生の QPC 値
                timestamp: record.EventHeader.TimeStamp,
                old_tid,
                new_tid,
                counters,
                len,
            };
            if let Ok(mut state) = shared.lock() {
                state.on_cswitch(&sample);
            }
        }

        /// Thread Start/End イベント (TypeGroup1) の先頭 8 byte から (PID, TID) を読む。
        fn read_pid_tid(record: &EVENT_RECORD) -> Option<(u32, u32)> {
            if (record.UserDataLength as usize) < 8 || record.UserData.is_null() {
                return None;
            }
            // SAFETY: UserData は 8 byte 以上の有効領域。read_unaligned で alignment 不問。
            let pid = unsafe { record.UserData.cast::<u32>().read_unaligned() };
            // SAFETY: 同上。
            let tid =
                unsafe { (record.UserData as *const u8).add(4).cast::<u32>().read_unaligned() };
            Some((pid, tid))
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            /// PROFILE_SOURCE_INFO 1 エントリ分の合成バイト列。
            /// `next_offset != 0` なら次エントリ開始位置まで 0 で pad する。
            fn entry_bytes(next_offset: u32, source: u32, name: &str) -> Vec<u8> {
                let mut bytes = Vec::new();
                bytes.extend_from_slice(&next_offset.to_le_bytes());
                bytes.extend_from_slice(&source.to_le_bytes());
                bytes.extend_from_slice(&[0u8; 16]); // MinInterval + MaxInterval + Reserved
                for unit in name.encode_utf16() {
                    bytes.extend_from_slice(&unit.to_le_bytes());
                }
                bytes.extend_from_slice(&[0, 0]); // NUL 終端
                if next_offset != 0 {
                    assert!(bytes.len() <= next_offset as usize, "next_offset too small");
                    bytes.resize(next_offset as usize, 0);
                }
                bytes
            }

            /// バイト列を parse_profile_source_list の入力形式 (u64 word 列) にする。
            fn to_words(bytes: &[u8]) -> Vec<u64> {
                let mut padded = bytes.to_vec();
                while !padded.len().is_multiple_of(size_of::<u64>()) {
                    padded.push(0);
                }
                padded
                    .chunks_exact(size_of::<u64>())
                    .map(|chunk| u64::from_le_bytes(chunk.try_into().expect("8-byte chunk")))
                    .collect()
            }

            #[test]
            fn parse_profile_source_list_reads_chained_entries() {
                let mut bytes = entry_bytes(64, 0, "TotalCycles");
                bytes.extend_from_slice(&entry_bytes(0, 2, "InstructionRetired"));
                let words = to_words(&bytes);
                let parsed = parse_profile_source_list(&words, bytes.len());
                assert_eq!(
                    parsed,
                    vec![
                        ("TotalCycles".to_string(), 0),
                        ("InstructionRetired".to_string(), 2)
                    ]
                );
            }

            #[test]
            fn parse_profile_source_list_stops_at_zero_next_offset() {
                let bytes = entry_bytes(0, 5, "CacheMisses");
                let words = to_words(&bytes);
                let parsed = parse_profile_source_list(&words, bytes.len());
                assert_eq!(parsed, vec![("CacheMisses".to_string(), 5)]);
            }

            #[test]
            fn parse_profile_source_list_tolerates_truncated_buffer() {
                let mut bytes = entry_bytes(64, 0, "TotalCycles");
                bytes.extend_from_slice(&entry_bytes(0, 2, "InstructionRetired"));
                let words = to_words(&bytes);
                // 2 エントリ目の固定部が入りきらない長さに切り詰める → 1 件だけ返す
                let parsed = parse_profile_source_list(&words, 80);
                assert_eq!(parsed, vec![("TotalCycles".to_string(), 0)]);
                // 固定部すら入らない長さなら空
                assert!(parse_profile_source_list(&words, 10).is_empty());
            }
        }
    }

    /// USI エンジンプロセス 1 本の wrapper。1 run ごとに fresh spawn する。
    struct UsiEngine {
        child: Child,
        stdin: BufWriter<ChildStdin>,
        stdout_rx: Receiver<String>,
        opt_names: HashSet<String>,
        label: String,
    }

    impl UsiEngine {
        fn spawn(cli: &Cli, variant: Variant, cpu: Option<usize>) -> Result<Self> {
            let path = engine_path(cli, variant);
            let mut cmd = Command::new(path);
            cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
            let mut child = cmd
                .spawn()
                .with_context(|| format!("failed to spawn engine {}", path.display()))?;

            if let Some(cpu) = cpu {
                pin_and_prioritize(&child, cpu)?;
            }

            let stdin = child.stdin.take().context("failed to capture engine stdin")?;
            let stdout = child.stdout.take().context("failed to capture engine stdout")?;

            let (stdout_tx, stdout_rx) = mpsc::channel();
            thread::spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines().map_while(std::io::Result::ok) {
                    if stdout_tx.send(line).is_err() {
                        break;
                    }
                }
            });

            Ok(Self {
                child,
                stdin: BufWriter::new(stdin),
                stdout_rx,
                opt_names: HashSet::new(),
                label: variant.name().to_string(),
            })
        }

        fn pid(&self) -> u32 {
            self.child.id()
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
            match self.stdout_rx.recv_timeout(timeout) {
                Ok(line) => Ok(line),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    bail!("{}: timeout waiting for engine output after {:?}", self.label, timeout)
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    bail!("{}: engine died (stdout closed) while waiting for output", self.label)
                }
            }
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

        fn write_line(&mut self, line: &str) -> Result<()> {
            writeln!(self.stdin, "{line}")?;
            self.stdin.flush()?;
            Ok(())
        }
    }

    impl Drop for UsiEngine {
        fn drop(&mut self) {
            let _ = writeln!(self.stdin, "quit");
            let _ = self.stdin.flush();
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    /// エンジンプロセスを論理 CPU 1 個に pin し、優先度を HIGH にする。
    fn pin_and_prioritize(child: &Child, cpu: usize) -> Result<()> {
        use std::os::windows::io::AsRawHandle;

        use windows_sys::Win32::System::Threading::{
            HIGH_PRIORITY_CLASS, SetPriorityClass, SetProcessAffinityMask,
        };

        if cpu >= usize::BITS as usize {
            bail!(
                "--cpu {cpu} は指定できません（SetProcessAffinityMask の単一 processor group \
                 制約により 0..{} のみ対応）",
                usize::BITS - 1
            );
        }
        let handle = child.as_raw_handle();
        // SAFETY: handle は spawn 直後の子プロセスの有効なハンドルで、Child が所有する。
        // 失敗時は BOOL=0 が返るだけで unsound にはならない。
        let ok = unsafe { SetProcessAffinityMask(handle, 1usize << cpu) };
        if ok == 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("SetProcessAffinityMask(cpu={cpu}) failed"));
        }
        // SAFETY: 同上。
        let ok = unsafe { SetPriorityClass(handle, HIGH_PRIORITY_CLASS) };
        if ok == 0 {
            return Err(std::io::Error::last_os_error()).context("SetPriorityClass failed");
        }
        Ok(())
    }

    /// QueryPerformanceCounter の現在値。ETW イベント (ClientContext=1) と同一時間軸。
    fn qpc_now() -> Result<i64> {
        use windows_sys::Win32::System::Performance::QueryPerformanceCounter;

        let mut value: i64 = 0;
        // SAFETY: value はこのスコープで有効な i64 への排他ポインタ。
        let ok = unsafe { QueryPerformanceCounter(&mut value) };
        if ok == 0 {
            return Err(std::io::Error::last_os_error()).context("QueryPerformanceCounter failed");
        }
        Ok(value)
    }

    /// 1 run: エンジンを fresh spawn し、探索区間の PMC カウンタを集計する。
    fn run_one(
        cli: &Cli,
        source_indices: &[u32],
        shared: &Arc<Mutex<PmcEngineState>>,
        source_names: &[String],
        position: &PositionCase,
        variant: Variant,
        round: u32,
        sequence_index: usize,
    ) -> Result<RunSample> {
        // spawn 前に基準を取ることで、spawn〜initialize 間の Thread Start ロスも
        // lost 検査の窓に含める（この区間のロスは TID 追跡を静かに欠けさせるため）。
        // セッションと consumer は run ごとに作り、run 末尾で STOP → ProcessTrace の
        // 自然終了を待つ。pin した idle コアではエンジンが数秒無中断で走り、bestmove 時の
        // 最終 switch-out 1 件だけがその CPU のバッファに残るが、FLUSH + 固定時間待ちでは
        // この未満杯バッファが配送されず、最終スライス (= 計測区間の大半) が静かに欠ける。
        let mut session = etw::KernelSession::start(source_indices)?;
        let consumer = etw::Consumer::start(Arc::clone(shared))?;
        let lost_before = session.query_lost_counts()?;
        let mut engine = UsiEngine::spawn(cli, variant, cli.cpu)?;
        // spawn 直後に対象 PID を登録し、Thread Start の観測窓を最大化する。ただし
        // ETW は per-CPU バッファ単位で配送され timestamp 順の保証がないため、
        // Thread Start の到着前にその TID の CSwitch が処理される帰属漏れが理論上
        // ありうる（計測中に lazy spawn されるスレッドが典型。既知限界は
        // .claude/skills/usi-perf-measure/SKILL.md を参照）。
        lock_state(shared)?.install(engine.pid(), source_names.len());
        engine.initialize(cli, variant)?;

        let start_qpc = qpc_now()?;
        lock_state(shared)?.open_window(start_qpc);
        engine.write_line(&position.position_cmd)?;
        engine.write_line(&format!("go movetime {}", cli.movetime_ms))?;

        let timeout = Duration::from_millis(cli.movetime_ms.saturating_mul(2) + 5000);
        let mut info = InfoSnapshot::default();
        let mut bestmove = None;
        let start = Instant::now();

        while start.elapsed() < timeout {
            let line = match engine.stdout_rx.recv_timeout(POLL_INTERVAL) {
                Ok(line) => line,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    bail!("{}: engine output channel disconnected", engine.label)
                }
            };
            if cli.verbose {
                eprintln!("[{}] {line}", engine.label);
            }
            if line.starts_with("info ") {
                info.update_from_line(&line);
            } else if let Some(mv) = line.strip_prefix("bestmove ") {
                bestmove = Some(mv.split_whitespace().next().unwrap_or("none").trim().to_string());
                break;
            }
        }

        let bestmove = bestmove.ok_or_else(|| {
            anyhow!(
                "{}: timed out waiting for bestmove for position {}",
                engine.label,
                position.name
            )
        })?;

        let end_qpc = qpc_now()?;
        // 計測 window の境界を跨ぐ実行スライスは重なり比で線形按分される
        // （仕様は pmc::PmcAccumulator::window_overlap_fraction のコメントを参照）。
        lock_state(shared)?.close_window(end_qpc);
        engine.write_line("quit")?;
        let status = wait_child(&mut engine.child, QUIT_TIMEOUT)?;
        if !status.success() {
            bail!("{}: engine exited with status {status}", engine.label);
        }

        // STOP → drain: 残バッファを全部 consumer に流し切ってから集計する
        let lost_after = session.stop()?;
        consumer.drain(DRAIN_TIMEOUT)?;

        // イベントロスがあった run は計測値が静かに欠けるため、集計せず破棄する
        if lost_after != lost_before {
            bail!(
                "{}: ETW イベントロスを検出しました (events_lost +{}, \
                 realtime_buffers_lost +{})。この run の計測値は信頼できないため破棄\
                 します。システム負荷を下げるか、バッファ設定 (BufferSize / \
                 MaximumBuffers) の拡大を検討してください。",
                engine.label,
                lost_after.events_lost.wrapping_sub(lost_before.events_lost),
                lost_after.realtime_buffers_lost.wrapping_sub(lost_before.realtime_buffers_lost),
            );
        }

        let pmc_totals = lock_state(shared)?
            .take_totals()
            .ok_or_else(|| anyhow!("{}: PMC accumulator not installed", engine.label))?;
        // 最終スライス未到着 / 連鎖欠落 / PMC 欠落 / 巻き戻りは静かな過小計測になるため破棄する
        if pmc_totals.unclosed_target_slices > 0
            || pmc_totals.chain_breaks_target > 0
            || pmc_totals.pmc_gaps_target > 0
            || pmc_totals.tid_reuse_target > 0
            || pmc_totals.regressed_switches > 0
        {
            bail!(
                "{}: PMC 積算が不完全です (unclosed_target_slices={}, chain_breaks_target={}, \
                 pmc_gaps_target={}, tid_reuse_target={}, regressed_switches={})。\
                 この run の計測値は破棄します。",
                engine.label,
                pmc_totals.unclosed_target_slices,
                pmc_totals.chain_breaks_target,
                pmc_totals.pmc_gaps_target,
                pmc_totals.tid_reuse_target,
                pmc_totals.regressed_switches,
            );
        }
        let (totals, attributed_switches, regressed_switches) =
            (pmc_totals.totals, pmc_totals.attributed_switches, pmc_totals.regressed_switches);
        if attributed_switches == 0 {
            bail!(
                "{}: 計測区間内にエンジンスレッドへ帰属する PMC 付き CSwitch を観測できません\n\
                 でした。ETW セッションの PMC 設定を確認してください。",
                engine.label
            );
        }
        let mut perf = PerfCounters::from_totals(source_names, &totals);
        // 上の破棄検査を通過した run では常に 0。JSON schema 維持のため残す
        perf.extra.insert("regressed_switches".to_string(), regressed_switches);
        if perf.cycles.is_none() {
            bail!("PMC totals do not contain TotalCycles");
        }
        if perf.instructions.is_none() {
            bail!("PMC totals do not contain InstructionRetired");
        }

        Ok(RunSample {
            variant,
            round,
            sequence_index,
            position_name: position.name.clone(),
            position_cmd: position.position_cmd.clone(),
            bestmove,
            info,
            perf,
        })
    }

    fn lock_state(
        shared: &Arc<Mutex<PmcEngineState>>,
    ) -> Result<std::sync::MutexGuard<'_, PmcEngineState>> {
        shared.lock().map_err(|_| anyhow!("PMC state mutex poisoned"))
    }

    pub fn main() -> Result<()> {
        let cli = Cli::parse();
        if !cli.cpus.is_empty() {
            bail!(
                "--cpus (shard 並列) は Windows 版では未対応です。\
                 --cpu で単一 CPU pinning を使ってください"
            );
        }
        let positions = load_position_cases(&cli.positions)?;
        if positions.is_empty() {
            bail!("no positions loaded from {}", cli.positions.display());
        }
        let pattern = parse_pattern(&cli.pattern)?;
        let source_names = parse_pmc_source_names(&cli.pmc_sources)?;
        let source_indices = resolve_profile_sources(&source_names)?;

        let shared = Arc::new(Mutex::new(PmcEngineState::default()));

        let mut samples = Vec::new();
        for round_idx in 0..cli.rounds {
            for position in &positions {
                for (sequence_index, variant) in pattern.iter().copied().enumerate() {
                    let run_no = samples.len() + 1;
                    println!(
                        "[shard 1][{run_no}] round={} position={} order={} variant={} cpu={}",
                        round_idx + 1,
                        position.name,
                        sequence_index + 1,
                        variant.name(),
                        cli.cpu.map_or_else(|| "-".to_string(), |c| c.to_string())
                    );

                    let sample = run_one(
                        &cli,
                        &source_indices,
                        &shared,
                        &source_names,
                        position,
                        variant,
                        round_idx + 1,
                        sequence_index + 1,
                    )
                    .with_context(|| {
                        format!(
                            "shard 1 failed at position={} order={} variant={}",
                            position.name,
                            sequence_index + 1,
                            variant.name()
                        )
                    })?;
                    println!(
                        "[shard 1] depth={} nodes={} time={}ms nps={} cycles/node={:.1} instructions/node={:.1} regressed_switches={}",
                        sample.info.depth,
                        sample.info.nodes,
                        sample.info.time_ms,
                        sample.info.nps,
                        sample.perf.cycles_per_node(sample.info.nodes).unwrap_or(0.0),
                        sample.perf.instructions_per_node(sample.info.nodes).unwrap_or(0.0),
                        sample.perf.extra.get("regressed_switches").copied().unwrap_or(0),
                    );
                    samples.push(sample);
                }
            }
        }

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
                    pmc_sources: cli.pmc_sources.clone(),
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

    /// カンマ区切りの PMC source 名を検証付きでパースする。
    fn parse_pmc_source_names(spec: &str) -> Result<Vec<String>> {
        let names: Vec<String> = spec
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        if names.is_empty() {
            bail!("--pmc-sources must not be empty");
        }
        if names.len() > MAX_PMC_SOURCES {
            bail!("--pmc-sources は最大 {MAX_PMC_SOURCES} 個までです");
        }
        let lower: Vec<String> = names.iter().map(|n| n.to_ascii_lowercase()).collect();
        if !lower.iter().any(|n| n == "totalcycles") {
            bail!("--pmc-sources には TotalCycles が必要です (cycles/node の算出に使用)");
        }
        if !lower.iter().any(|n| n == "instructionretired" || n == "instructionsretired") {
            bail!(
                "--pmc-sources には InstructionRetired が必要です \
                 (instructions/node の算出に使用)"
            );
        }
        Ok(names)
    }

    /// PMC source 名を profile source index へ解決する（大文字小文字は無視）。
    fn resolve_profile_sources(names: &[String]) -> Result<Vec<u32>> {
        let available = etw::query_profile_sources()?;
        let mut indices = Vec::with_capacity(names.len());
        for name in names {
            let found = available
                .iter()
                .find(|(avail_name, _)| avail_name.eq_ignore_ascii_case(name))
                .map(|(_, index)| *index);
            match found {
                Some(index) => indices.push(index),
                None => {
                    let mut list: Vec<&str> = available.iter().map(|(n, _)| n.as_str()).collect();
                    list.sort_unstable();
                    bail!(
                        "PMC source '{name}' はこのマシンでは利用できません。\n\
                         利用可能な source: {}",
                        list.join(", ")
                    );
                }
            }
        }
        Ok(indices)
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

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn normalize_position_command_accepts_raw_sfen() {
            let raw = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
            assert_eq!(normalize_position_command(raw), format!("position sfen {raw}"));
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
        fn parse_option_name_extracts_multi_word_name() {
            let line = "name Skill Level type spin default 20 min 0 max 20";
            assert_eq!(parse_option_name(line).as_deref(), Some("Skill Level"));
        }

        #[test]
        fn parse_pmc_source_names_requires_cycles_and_instructions() {
            let names = parse_pmc_source_names("TotalCycles,InstructionRetired,CacheMisses")
                .expect("default-like spec should parse");
            assert_eq!(names, vec!["TotalCycles", "InstructionRetired", "CacheMisses"]);
            assert!(parse_pmc_source_names("TotalCycles").is_err());
            assert!(parse_pmc_source_names("InstructionRetired").is_err());
            assert!(parse_pmc_source_names("").is_err());
        }

        #[test]
        fn perf_counters_maps_known_sources_and_keeps_unknown_in_extra() {
            let names: Vec<String> = [
                "TotalCycles",
                "InstructionRetired",
                "BranchInstructions",
                "BranchMispredictions",
                "CacheMisses",
                "DcacheMisses",
                "UnhaltedCoreCycles",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect();
            let totals = [1u64, 2, 3, 4, 5, 6, 7];
            let counters = PerfCounters::from_totals(&names, &totals);
            assert_eq!(counters.cycles, Some(1));
            assert_eq!(counters.instructions, Some(2));
            assert_eq!(counters.branches, Some(3));
            assert_eq!(counters.branch_misses, Some(4));
            assert_eq!(counters.cache_misses, Some(5));
            assert_eq!(counters.l1_dcache_load_misses, Some(6));
            assert_eq!(counters.cache_references, None);
            assert_eq!(counters.extra.get("UnhaltedCoreCycles"), Some(&7));
        }
    }
} // mod windows_main

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    windows_main::main()
}
