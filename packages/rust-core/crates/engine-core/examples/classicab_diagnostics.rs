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
}

fn main() {
    let args = Args::parse();
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
    let backend =
        engine_core::search::ab::ClassicBackend::with_tt(Arc::new(MaterialEvaluator), tt.clone());

    println!("ClassicAB Diagnostics (depth 4..6)\n");
    for (name, sfen) in tests {
        let pos = Position::from_sfen(sfen).expect("valid sfen");
        println!("[{name}] {sfen}");
        for depth in args.depth_min..=args.depth_max {
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
                    eprintln!("  [event] hashfull {h}");
                }
                InfoEvent::CurrMove { mv, number } => {
                    if number % sample_every == 1 {
                        // sample to avoid spam
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
            let res = backend.think_blocking(&pos, &limits, Some(info));
            let elapsed = start.elapsed();
            let nps = (res.stats.nodes as f64 / elapsed.as_secs_f64()) as u64;
            let hf = tt.hashfull_permille();
            let tt_hits = res.stats.tt_hits.unwrap_or(0);
            let lmr = res.stats.lmr_count.unwrap_or(0);
            let beta = res.stats.root_fail_high_count.unwrap_or(0);
            let asp_fail_high = asp_fail_high.load(Ordering::Relaxed);
            let asp_fail_low = asp_fail_low.load(Ordering::Relaxed);
            println!(
                "  depth {:>2}  nodes {:>10}  nps {:>9}  hashfull {:>4}  score {:>6}  tt_hits {:>8}  lmr {:>8}  beta_cuts {:>8}  aspFH {:>3}  aspFL {:>3}",
                depth, res.stats.nodes, nps, hf, res.score, tt_hits, lmr, beta, asp_fail_high, asp_fail_low
            );
        }
        println!();
    }
}
