//! mate_bench - df-pn 詰み探索 (`go mate` と同じ solver) の局面集ベンチ
//!
//! # 使用方法
//!
//! ```bash
//! # やねうら王の詰将棋データセットの 11 手詰めから 1000 局面 (1000 行おき) を解き、
//! # 「最短 11 手の詰み」と照合する
//! cargo run --release -p tools --bin mate_bench -- \
//!   --input $SHOGI_DATA/tsume/yaneuraou-2020/mate11.sfen \
//!   --stride 1000 --limit 1000 --expect-mate-len 11 \
//!   --nodes 1000000 --out mate11.tsv
//!
//! # 実戦局面を手番そのままと手番反転 (詰めろ判定の使い方) の両方で解く
//! cargo run --release -p tools --bin mate_bench -- \
//!   --input positions.sfen --flip --time-ms 1000 --out real.tsv
//! ```
//!
//! 入力は読みながら解くので、メモリは局面数に依存しない (分位点は対数ヒストグラムで近似する)。
//! 出力の TSV は入力順で、1 行 1 局面の `sfen` `result` `pv_len` `nodes` `micros` `pv`。

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use clap::Parser;

use rshogi_core::mate::dfpn::{DfpnSolver, MateLimits, MateResult};
use rshogi_core::position::Position;

#[derive(Parser)]
#[command(name = "mate_bench", about = "df-pn 詰み探索の局面集ベンチ")]
struct Cli {
    /// 入力 SFEN ファイル (1 行 1 局面。`sfen ` 接頭辞は外し、空行と `#` で始まる行は読み飛ばす)
    #[arg(long)]
    input: PathBuf,

    /// 有効な行のうち `index % stride == offset` の行だけを使う (調整用と評価用を分ける)
    #[arg(long, default_value_t = 1)]
    stride: usize,

    /// `--stride` と組で使う開始位置
    #[arg(long, default_value_t = 0)]
    offset: usize,

    /// 解く局面数の上限 (反転局面を含む。0 は無制限)
    #[arg(long, default_value_t = 0)]
    limit: usize,

    /// 手番を反転した局面も解く (元の手番側が王手されていない局面だけ)
    #[arg(long)]
    flip: bool,

    /// 1 局面あたりのノード数の上限 (0 は無制限)
    #[arg(long, default_value_t = 0)]
    nodes: u64,

    /// 1 局面あたりの時間の上限 (ミリ秒、0 は無制限)
    #[arg(long, default_value_t = 0)]
    time_ms: u64,

    /// 置換表のサイズ (MB、スレッドごと)
    #[arg(long, default_value_t = 64)]
    hash_mb: usize,

    /// 正解の最短詰み手数。指定すると、不詰・非対応の結果を誤りとして数える。
    /// この手数より短い詰み手順は、局面集の正解の側の誤りもありうるので「要確認」として並べる
    #[arg(long)]
    expect_mate_len: Option<usize>,

    /// 局面を並列に解くスレッド数 (時間を比べるときは 1)
    #[arg(long, default_value_t = 1)]
    jobs: usize,

    /// 局面ごとの結果を書く TSV
    #[arg(long)]
    out: Option<PathBuf>,
}

struct Outcome {
    sfen: String,
    result: MateResult,
    nodes: u64,
    micros: u64,
}

impl Outcome {
    fn tag(&self) -> &'static str {
        match self.result {
            MateResult::Mate(_) => "mate",
            MateResult::NoMate => "nomate",
            MateResult::Unknown => "unknown",
            MateResult::Unsupported => "unsupported",
        }
    }

    fn pv_len(&self) -> usize {
        match &self.result {
            MateResult::Mate(pv) => pv.len(),
            _ => 0,
        }
    }

    fn pv_usi(&self) -> String {
        match &self.result {
            MateResult::Mate(pv) => pv.iter().map(|m| m.to_usi()).collect::<Vec<_>>().join(" "),
            _ => String::new(),
        }
    }
}

/// 値の分布を対数の区間で数える (1 オクターブを 8 区間)。分位点は区間の上端で近似する。
struct LogHistogram {
    counts: Vec<u64>,
    total: u64,
}

impl LogHistogram {
    const SUB: f64 = 8.0;

    fn new() -> Self {
        Self {
            counts: vec![0; 64 * 8 + 1],
            total: 0,
        }
    }

    fn bucket(value: u64) -> usize {
        if value == 0 {
            0
        } else {
            1 + ((value as f64).log2() * Self::SUB).floor() as usize
        }
    }

    fn upper(bucket: usize) -> u64 {
        if bucket == 0 {
            0
        } else {
            (((bucket as f64) / Self::SUB).exp2()).ceil() as u64
        }
    }

    fn add(&mut self, value: u64) {
        let b = Self::bucket(value).min(self.counts.len() - 1);
        self.counts[b] += 1;
        self.total += 1;
    }

    fn percentile(&self, p: u64) -> u64 {
        if self.total == 0 {
            return 0;
        }
        let rank = (self.total * p).div_ceil(100).max(1);
        let mut seen = 0;
        for (b, &c) in self.counts.iter().enumerate() {
            seen += c;
            if seen >= rank {
                return Self::upper(b);
            }
        }
        Self::upper(self.counts.len() - 1)
    }
}

/// 集計 (局面数に依存しないメモリで持つ)
struct Summary {
    mate: usize,
    nomate: usize,
    unknown: usize,
    unsupported: usize,
    total_nodes: u64,
    nodes_hist: LogHistogram,
    micros_hist: LogHistogram,
    max_nodes: u64,
    max_micros: u64,
    /// ノード予算ごとの判定数
    within_budget: [usize; 5],
    expect_wrong: usize,
    expect_too_short: usize,
    expect_shortest: usize,
}

const BUDGETS: [u64; 5] = [1_000, 10_000, 100_000, 1_000_000, 10_000_000];
/// 誤りとして標準出力に並べる局面数の上限
const MAX_REPORTED_ERRORS: usize = 50;

impl Summary {
    fn new() -> Self {
        Self {
            mate: 0,
            nomate: 0,
            unknown: 0,
            unsupported: 0,
            total_nodes: 0,
            nodes_hist: LogHistogram::new(),
            micros_hist: LogHistogram::new(),
            max_nodes: 0,
            max_micros: 0,
            within_budget: [0; 5],
            expect_wrong: 0,
            expect_too_short: 0,
            expect_shortest: 0,
        }
    }

    /// 1 局面を足す。正解と食い違うか要確認なら true。
    fn add(&mut self, o: &Outcome, expect: Option<usize>) -> bool {
        self.total_nodes += o.nodes;
        match o.result {
            MateResult::Mate(_) => self.mate += 1,
            MateResult::NoMate => self.nomate += 1,
            MateResult::Unknown => self.unknown += 1,
            MateResult::Unsupported => self.unsupported += 1,
        }
        if matches!(o.result, MateResult::Mate(_) | MateResult::NoMate) {
            self.nodes_hist.add(o.nodes);
            self.micros_hist.add(o.micros);
            self.max_nodes = self.max_nodes.max(o.nodes);
            self.max_micros = self.max_micros.max(o.micros);
            for (count, &budget) in self.within_budget.iter_mut().zip(&BUDGETS) {
                *count += usize::from(o.nodes <= budget);
            }
        }
        let Some(expect) = expect else {
            return false;
        };
        match o.result {
            MateResult::NoMate | MateResult::Unsupported => {
                self.expect_wrong += 1;
                true
            }
            MateResult::Mate(_) if o.pv_len() < expect => {
                self.expect_too_short += 1;
                true
            }
            MateResult::Mate(_) => {
                self.expect_shortest += usize::from(o.pv_len() == expect);
                false
            }
            MateResult::Unknown => false,
        }
    }
}

fn flip_side_to_move(sfen: &str) -> String {
    let mut fields: Vec<&str> = sfen.split_whitespace().collect();
    if fields.len() > 1 {
        fields[1] = if fields[1] == "b" { "w" } else { "b" };
    }
    fields.join(" ")
}

/// 入力を読み、使う局面に通し番号を付けて送る。`tickets` で未処理の件数に上限をかける。
fn feed(cli: &Cli, jobs_tx: SyncSender<(usize, String)>, tickets: Receiver<()>) -> Result<()> {
    let file = std::fs::File::open(&cli.input)
        .with_context(|| format!("{} を開けない", cli.input.display()))?;
    let mut line_index = 0usize;
    let mut sent = 0usize;
    for line in BufReader::new(file).lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if cli.limit != 0 && sent >= cli.limit {
            return Ok(());
        }
        let selected = line_index % cli.stride == cli.offset;
        line_index += 1;
        if !selected {
            continue;
        }
        let sfen = line.strip_prefix("sfen ").unwrap_or(line).to_string();
        let mut pos = Position::new();
        pos.set_sfen(&sfen)
            .map_err(|e| anyhow::anyhow!("SFEN を読めない: {sfen}: {e:?}"))?;
        let flipped = (cli.flip && !pos.in_check()).then(|| flip_side_to_move(&sfen));
        for s in std::iter::once(sfen).chain(flipped) {
            if cli.limit != 0 && sent >= cli.limit {
                return Ok(());
            }
            if tickets.recv().is_err() || jobs_tx.send((sent, s)).is_err() {
                return Ok(());
            }
            sent += 1;
        }
    }
    Ok(())
}

fn solve_worker(
    cli: &Cli,
    jobs_rx: &Mutex<Receiver<(usize, String)>>,
    results_tx: SyncSender<(usize, Outcome)>,
) {
    let stop = AtomicBool::new(false);
    let limits = MateLimits {
        nodes: cli.nodes,
        time_ms: cli.time_ms,
    };
    let mut solver = DfpnSolver::new(cli.hash_mb);
    loop {
        let job = jobs_rx.lock().expect("受け口のロック").recv();
        let Ok((index, sfen)) = job else { break };
        let mut pos = Position::new();
        pos.set_sfen(&sfen).expect("読み込み時に検査済み");
        let start = Instant::now();
        let result = solver.solve(&mut pos, &limits, &stop, |_| {});
        let outcome = Outcome {
            sfen,
            result,
            nodes: solver.nodes(),
            micros: start.elapsed().as_micros() as u64,
        };
        if results_tx.send((index, outcome)).is_err() {
            break;
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    // worker が panic すると、その局面の結果を待つ並べ替えが止まって終わらなくなるので、panic したら即終了する
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default_hook(info);
        std::process::exit(101);
    }));
    if cli.stride == 0 || cli.offset >= cli.stride {
        bail!("--offset は --stride 未満にする");
    }
    let jobs = cli.jobs.max(1);
    // 解いている途中と書き出し待ちの局面数の上限 (遅い局面があっても並べ替え待ちが膨らまない)
    let window = jobs * 64;

    let (jobs_tx, jobs_rx) = sync_channel::<(usize, String)>(jobs * 2);
    let (results_tx, results_rx) = sync_channel::<(usize, Outcome)>(jobs * 2);
    let (tickets_tx, tickets_rx) = sync_channel::<()>(window);
    for _ in 0..window {
        tickets_tx.send(()).expect("容量内");
    }
    let jobs_rx = Mutex::new(jobs_rx);

    let mut writer = match &cli.out {
        Some(path) => {
            let mut w = BufWriter::new(
                std::fs::File::create(path)
                    .with_context(|| format!("{} を作れない", path.display()))?,
            );
            writeln!(w, "sfen\tresult\tpv_len\tnodes\tmicros\tpv")?;
            Some(w)
        }
        None => None,
    };

    let start = Instant::now();
    let mut summary = Summary::new();
    let mut reported = 0usize;
    // TSV の書き込みに失敗しても結果は最後まで受け取る (途中で受け取りをやめると、
    // 入力スレッドと worker が送り先の空きを待ち続けて終わらない)
    let mut write_error = None;
    let feed_result = std::thread::scope(|scope| -> Result<()> {
        let feeder = scope.spawn(|| feed(&cli, jobs_tx, tickets_rx));
        for _ in 0..jobs {
            let jobs_rx = &jobs_rx;
            let results_tx = results_tx.clone();
            let cli = &cli;
            std::thread::Builder::new()
                // 詰み探索の再帰は手数に比例して深くなる
                .stack_size(64 * 1024 * 1024)
                .spawn_scoped(scope, move || solve_worker(cli, jobs_rx, results_tx))
                .expect("スレッドを起動できない");
        }
        drop(results_tx);

        let mut pending: BTreeMap<usize, Outcome> = BTreeMap::new();
        let mut next = 0usize;
        for (index, outcome) in results_rx {
            pending.insert(index, outcome);
            while let Some(o) = pending.remove(&next) {
                if let Some(w) = writer.as_mut()
                    && write_error.is_none()
                    && let Err(e) = writeln!(
                        w,
                        "{}\t{}\t{}\t{}\t{}\t{}",
                        o.sfen,
                        o.tag(),
                        o.pv_len(),
                        o.nodes,
                        o.micros,
                        o.pv_usi()
                    )
                {
                    write_error = Some(e);
                }
                if summary.add(&o, cli.expect_mate_len) && reported < MAX_REPORTED_ERRORS {
                    reported += 1;
                    let label = if o.tag() == "mate" {
                        "要確認 (最短未満)"
                    } else {
                        "誤り"
                    };
                    println!(
                        "{label}: {} (手順 {} 手) {} {}",
                        o.tag(),
                        o.pv_len(),
                        o.sfen,
                        o.pv_usi()
                    );
                }
                next += 1;
                // 書き出したぶん次の局面を読ませる (読み手が先に終わっていれば送れなくてよい)
                let _ = tickets_tx.send(());
            }
        }
        feeder.join().expect("入力スレッドが panic した")
    });
    feed_result?;
    if let Some(e) = write_error {
        return Err(e).context("TSV に書き込めない");
    }
    if let Some(mut w) = writer {
        w.flush()?;
    }
    let wall_ms = start.elapsed().as_millis().max(1) as u64;

    let s = &summary;
    let total = s.mate + s.nomate + s.unknown + s.unsupported;
    println!(
        "局面 {total}: mate {} / nomate {} / unknown {} / unsupported {}",
        s.mate, s.nomate, s.unknown, s.unsupported
    );
    println!(
        "nodes {}、壁時計 {wall_ms} ms (jobs={jobs})、{} knps (全 job 合計)",
        s.total_nodes,
        s.total_nodes / wall_ms
    );
    println!(
        "判定できた局面 (分位点は区間の上端で近似): nodes 中央値 ~{} / 90% ~{} / 99% ~{} / 最大 {}、時間 (μs) 中央値 ~{} / 90% ~{} / 99% ~{} / 最大 {}",
        s.nodes_hist.percentile(50),
        s.nodes_hist.percentile(90),
        s.nodes_hist.percentile(99),
        s.max_nodes,
        s.micros_hist.percentile(50),
        s.micros_hist.percentile(90),
        s.micros_hist.percentile(99),
        s.max_micros
    );
    let curve: Vec<String> =
        BUDGETS.iter().zip(&s.within_budget).map(|(b, c)| format!("{b}:{c}")).collect();
    println!("ノード予算ごとの判定数: {}", curve.join(" "));
    if let Some(expect) = cli.expect_mate_len {
        println!(
            "正解 (最短 {expect} 手の詰み) との照合: 誤り (不詰・非対応) {} / 手順が最短未満 (要確認) {} / 最短手順 {} / 詰み {}",
            s.expect_wrong, s.expect_too_short, s.expect_shortest, s.mate
        );
        if s.expect_wrong > 0 {
            bail!("詰みの正解に対して不詰・非対応を返した局面がある");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_histogram_percentiles_bound_the_values() {
        let mut h = LogHistogram::new();
        for v in 1..=1000u64 {
            h.add(v);
        }
        let p50 = h.percentile(50);
        // 区間の上端なので真値 (500) 以上、1/8 オクターブ (約 9%) 以内
        assert!((500..=546).contains(&p50), "{p50}");
        assert!(h.percentile(100) >= 1000);
        assert_eq!(LogHistogram::new().percentile(50), 0);
    }

    #[test]
    fn flip_swaps_only_the_side_to_move() {
        assert_eq!(flip_side_to_move("4k4/9/9/9/9/9/9/9/4K4 b G 1"), "4k4/9/9/9/9/9/9/9/4K4 w G 1");
        assert_eq!(flip_side_to_move("4k4/9/9/9/9/9/9/9/4K4 w - 3"), "4k4/9/9/9/9/9/9/9/4K4 b - 3");
    }
}
