//! Phase 4: Enhanced TT benchmark with 4-mode testing and detailed metrics
//!
//! Tests:
//! - NoTT: Pure search baseline
//! - TT (no CAS): TT lookup only (Relaxed load, no store)
//! - TTOnly: Full TT with CAS operations
//! - TT+Pref: Full TT with adaptive prefetching

use engine_core::{
    evaluation::evaluate::{Evaluator, MaterialEvaluator},
    movegen::MoveGen,
    search::{NodeType, TranspositionTable},
    shogi::moves::{Move, MoveList},
    usi::parse_sfen,
    Position,
};
use std::sync::Arc;
use std::time::Instant;

/// Score type for benchmark (using i32 to avoid overflow)
type Score = i32;

/// Test modes for comprehensive benchmarking
#[derive(Debug, Clone, Copy, PartialEq)]
enum TestMode {
    NoTT,       // No transposition table
    TTNoCAS,    // TT with read-only access (no CAS)
    TTOnly,     // TT with full CAS operations
    TTPrefetch, // TT with prefetch enabled
}

impl TestMode {
    fn as_str(&self) -> &'static str {
        match self {
            TestMode::NoTT => "NoTT",
            TestMode::TTNoCAS => "TT (no CAS)",
            TestMode::TTOnly => "TTOnly",
            TestMode::TTPrefetch => "TT+Pref",
        }
    }
}

/// Test position for benchmarking
struct TestPosition {
    name: &'static str,
    sfen: &'static str,
}

/// Benchmark result for a single test
struct BenchResult {
    mode: TestMode,
    nodes: u64,
    time_ms: u128,
    nps: u64,
    tt_hits: Option<u64>,
    tt_probes: Option<u64>,
    hashfull: Option<u16>,
    prefetch_stats: Option<PrefetchData>,
}

struct PrefetchData {
    hit_rate: f64,
    distance: usize,
}

impl BenchResult {
    fn format_row(&self, baseline_nps: Option<u64>) -> String {
        let nps_diff = if let Some(base) = baseline_nps {
            if base > 0 {
                let diff = ((self.nps as f64 / base as f64) - 1.0) * 100.0;
                format!("{diff:+6.1}%")
            } else {
                "    N/A".to_string()
            }
        } else {
            "baseline".to_string()
        };

        let tt_info = if let (Some(hits), Some(probes)) = (self.tt_hits, self.tt_probes) {
            let hit_rate = if probes > 0 {
                (hits as f64 / probes as f64) * 100.0
            } else {
                0.0
            };
            format!(" | TT: {hit_rate:.1}%")
        } else {
            String::new()
        };

        let hashfull_info = if let Some(hf) = self.hashfull {
            format!(" | HF: {:.1}%", hf as f64 / 10.0)
        } else {
            String::new()
        };

        let prefetch_info = if let Some(pf) = &self.prefetch_stats {
            format!(" | PF: {:.1}% d={}", pf.hit_rate * 100.0, pf.distance)
        } else {
            String::new()
        };

        format!(
            "{:<12} {:>10} {:>10} {:>8} {:>9}{}{}{}",
            self.mode.as_str(),
            self.nodes,
            self.nps,
            self.time_ms,
            nps_diff,
            tt_info,
            hashfull_info,
            prefetch_info
        )
    }
}

/// Enhanced search engine with mode-specific behavior
struct BenchmarkEngine {
    position: Position,
    tt: Option<Arc<TranspositionTable>>,
    mode: TestMode,
    nodes_searched: u64,
    tt_hits: u64,
    tt_probes: u64,
}

impl BenchmarkEngine {
    fn new(position: Position, mode: TestMode, tt: Option<Arc<TranspositionTable>>) -> Self {
        Self {
            position,
            tt,
            mode,
            nodes_searched: 0,
            tt_hits: 0,
            tt_probes: 0,
        }
    }

    /// Search with mode-specific TT handling
    fn search(&mut self, depth: u8, prefetch_enabled: bool) -> (Option<Move>, Score) {
        self.nodes_searched = 0;
        self.tt_hits = 0;
        self.tt_probes = 0;

        let start = Instant::now();
        let (best_move, score) = self.alpha_beta_root(depth, -30000, 30000, prefetch_enabled);
        let _elapsed = start.elapsed();

        (best_move, score)
    }

    /// Alpha-beta search with mode-specific TT handling
    fn alpha_beta_root(
        &mut self,
        depth: u8,
        mut alpha: Score,
        beta: Score,
        prefetch_enabled: bool,
    ) -> (Option<Move>, Score) {
        self.nodes_searched += 1;

        if depth == 0 {
            let evaluator = MaterialEvaluator;
            return (None, evaluator.evaluate(&self.position) as Score);
        }

        let hash = self.position.zobrist_hash();

        // TT probe based on mode
        if let Some(ref tt) = self.tt {
            match self.mode {
                TestMode::NoTT => {
                    // No TT access
                }
                TestMode::TTNoCAS => {
                    // Read-only probe (no store) - measures pure lookup overhead
                    // Always results in 0% hit rate as no entries are stored
                    self.tt_probes += 1;
                    if let Some(entry) = tt.probe(hash) {
                        self.tt_hits += 1;
                        if entry.depth() >= depth {
                            return (entry.get_move(), entry.score() as Score);
                        }
                    }
                }
                TestMode::TTOnly | TestMode::TTPrefetch => {
                    // Full TT with CAS
                    self.tt_probes += 1;

                    // Prefetch for TTPrefetch mode
                    if self.mode == TestMode::TTPrefetch && prefetch_enabled {
                        // Prefetch child positions
                        let mut gen = MoveGen;
                        let mut move_list = MoveList::new();
                        gen.generate_all(&self.position, &mut move_list);
                        let moves = move_list;
                        for (i, mv) in moves.iter().take(4).enumerate() {
                            // Make move temporarily to get hash
                            let undo_info = self.position.do_move(*mv);
                            let child_hash = self.position.zobrist_hash();
                            self.position.undo_move(*mv, undo_info);

                            // Adaptive prefetch based on depth and priority
                            if depth <= 4 && i == 0 {
                                tt.prefetch_l1(child_hash); // Shallow depth, first move: L1
                            } else if depth <= 6 {
                                tt.prefetch_l2(child_hash); // Medium depth: L2
                            } else {
                                tt.prefetch_l3(child_hash); // Deep depth: L3
                            }
                        }
                    }

                    if let Some(entry) = tt.probe(hash) {
                        self.tt_hits += 1;
                        if entry.depth() >= depth {
                            return (entry.get_move(), entry.score() as Score);
                        }
                    }
                }
            }
        }

        // Regular search
        let mut gen = MoveGen;
        let mut move_list = MoveList::new();
        gen.generate_all(&self.position, &mut move_list);
        let moves = move_list;
        if moves.is_empty() {
            return (None, -30000); // Checkmate
        }

        let mut best_move = None;
        let mut best_score: Score = -30001;

        for mv in moves {
            // Make move
            let undo_info = self.position.do_move(mv);

            let score = -self.alpha_beta(depth - 1, -beta, -alpha, prefetch_enabled);

            // Unmake move
            self.position.undo_move(mv, undo_info);

            if score > best_score {
                best_score = score;
                best_move = Some(mv);
            }

            if score > alpha {
                alpha = score;
            }

            if alpha >= beta {
                break; // Beta cutoff
            }
        }

        // Store in TT (only for full TT modes)
        if let Some(ref tt) = self.tt {
            if self.mode == TestMode::TTOnly || self.mode == TestMode::TTPrefetch {
                let node_type = if best_score >= beta {
                    NodeType::LowerBound
                } else if best_score <= alpha {
                    NodeType::UpperBound
                } else {
                    NodeType::Exact
                };

                tt.store(hash, best_move, best_score as i16, 0, depth, node_type);
            }
        }

        (best_move, best_score)
    }

    fn alpha_beta(
        &mut self,
        depth: u8,
        mut alpha: Score,
        beta: Score,
        prefetch_enabled: bool,
    ) -> Score {
        self.nodes_searched += 1;

        if depth == 0 {
            let evaluator = MaterialEvaluator;
            return evaluator.evaluate(&self.position) as Score;
        }

        let hash = self.position.zobrist_hash();

        // TT probe based on mode
        if let Some(ref tt) = self.tt {
            match self.mode {
                TestMode::NoTT => {
                    // No TT access
                }
                TestMode::TTNoCAS => {
                    // Read-only probe - measures pure lookup overhead without CAS
                    // Note: This mode never stores entries, so hit% will be 0%
                    // This is intentional to isolate read-only access cost
                    self.tt_probes += 1;
                    if let Some(entry) = tt.probe(hash) {
                        self.tt_hits += 1;
                        if entry.depth() >= depth {
                            return entry.score() as Score;
                        }
                    }
                }
                TestMode::TTOnly | TestMode::TTPrefetch => {
                    // Full TT with prefetch
                    self.tt_probes += 1;

                    if self.mode == TestMode::TTPrefetch && prefetch_enabled && depth > 2 {
                        // Adaptive prefetching
                        let mut gen = MoveGen;
                        let mut move_list = MoveList::new();
                        gen.generate_all(&self.position, &mut move_list);
                        let moves = move_list;
                        let prefetch_count = (depth as usize / 2).min(3);

                        for (i, mv) in moves.iter().take(prefetch_count).enumerate() {
                            // Make move temporarily to get hash
                            let undo_info = self.position.do_move(*mv);
                            let child_hash = self.position.zobrist_hash();
                            self.position.undo_move(*mv, undo_info);

                            // Adaptive prefetch based on depth and priority
                            if depth <= 4 && i == 0 {
                                tt.prefetch_l1(child_hash); // Shallow depth, first move: L1
                            } else if depth <= 6 {
                                tt.prefetch_l2(child_hash); // Medium depth: L2
                            } else {
                                tt.prefetch_l3(child_hash); // Deep depth: L3 (avoid L1 pollution)
                            }
                        }
                    }

                    if let Some(entry) = tt.probe(hash) {
                        self.tt_hits += 1;
                        if entry.depth() >= depth {
                            return entry.score() as Score;
                        }
                    }
                }
            }
        }

        // Generate and search moves
        let mut gen = MoveGen;
        let mut move_list = MoveList::new();
        gen.generate_all(&self.position, &mut move_list);
        let moves = move_list;
        if moves.is_empty() {
            return -30000; // Simplified checkmate score
        }

        let mut best_score: Score = -30001;

        for mv in moves {
            // Make move
            let undo_info = self.position.do_move(mv);

            let score = -self.alpha_beta(depth - 1, -beta, -alpha, prefetch_enabled);

            // Unmake move
            self.position.undo_move(mv, undo_info);

            if score > best_score {
                best_score = score;
            }

            if score > alpha {
                alpha = score;
            }

            if alpha >= beta {
                break;
            }
        }

        // Store in TT for full modes
        if let Some(ref tt) = self.tt {
            if self.mode == TestMode::TTOnly || self.mode == TestMode::TTPrefetch {
                let node_type = if best_score >= beta {
                    NodeType::LowerBound
                } else if best_score <= alpha {
                    NodeType::UpperBound
                } else {
                    NodeType::Exact
                };

                tt.store(hash, None, best_score as i16, 0, depth, node_type);
            }
        }

        best_score
    }
}

/// Run benchmark for a specific configuration
fn run_benchmark(
    position: &TestPosition,
    depth: u8,
    mode: TestMode,
    tt: Option<Arc<TranspositionTable>>,
) -> BenchResult {
    let pos = parse_sfen(position.sfen).expect("Invalid SFEN");
    let mut engine = BenchmarkEngine::new(pos, mode, tt.clone());

    // Enable prefetch stats for TTPrefetch mode
    if mode == TestMode::TTPrefetch {
        if let Some(_tt) = &tt {
            // Note: This requires mutable access to TT
            // In real implementation, we'd need to handle this differently
        }
    }

    let start = Instant::now();
    let prefetch_enabled = mode == TestMode::TTPrefetch;
    let (_move, _score) = engine.search(depth, prefetch_enabled);
    let elapsed = start.elapsed();

    let time_ms = elapsed.as_millis();
    let nps = if time_ms > 0 {
        (engine.nodes_searched as u128 * 1000 / time_ms) as u64
    } else {
        0
    };

    // Collect statistics
    let (hashfull, prefetch_stats) = if let Some(ref tt) = tt {
        let hf = if mode != TestMode::NoTT {
            Some(tt.hashfull())
        } else {
            None
        };

        let pf = if mode == TestMode::TTPrefetch {
            tt.prefetch_stats().map(|stats| PrefetchData {
                hit_rate: stats.hit_rate,
                distance: stats.distance,
            })
        } else {
            None
        };

        (hf, pf)
    } else {
        (None, None)
    };

    BenchResult {
        mode,
        nodes: engine.nodes_searched,
        time_ms,
        nps,
        tt_hits: if mode != TestMode::NoTT {
            Some(engine.tt_hits)
        } else {
            None
        },
        tt_probes: if mode != TestMode::NoTT {
            Some(engine.tt_probes)
        } else {
            None
        },
        hashfull,
        prefetch_stats,
    }
}

/// Print results for a depth
fn print_depth_results(depth: u8, position: &TestPosition, results: &[BenchResult]) {
    println!("\n{} (Depth {}):", position.name, depth);
    println!("{}", "-".repeat(100));
    println!(
        "{:<12} {:>10} {:>10} {:>8} {:>9} | {:>8} | {:>8} | {:>8}",
        "Mode", "Nodes", "NPS", "Time(ms)", "vs NoTT", "TT Hit%", "Hashfull", "Prefetch"
    );
    println!("{}", "-".repeat(100));

    let baseline_nps = results.iter().find(|r| r.mode == TestMode::NoTT).map(|r| r.nps);

    for result in results {
        println!("{}", result.format_row(baseline_nps));
    }

    // Print analysis
    println!("\nAnalysis:");

    // Node reduction
    if let (Some(nott), Some(tt)) = (
        results.iter().find(|r| r.mode == TestMode::NoTT),
        results.iter().find(|r| r.mode == TestMode::TTOnly),
    ) {
        let reduction = 100.0 * (1.0 - (tt.nodes as f64 / nott.nodes as f64));
        println!("  Node reduction: {reduction:.1}%");
    }

    // CAS overhead
    if let (Some(nocas), Some(cas)) = (
        results.iter().find(|r| r.mode == TestMode::TTNoCAS),
        results.iter().find(|r| r.mode == TestMode::TTOnly),
    ) {
        if nocas.time_ms == 0 || cas.time_ms == 0 {
            println!("  CAS overhead: N/A (time too short)");
        } else {
            let overhead = 100.0 * ((cas.time_ms as f64 / nocas.time_ms as f64) - 1.0);
            println!("  CAS overhead: {overhead:.1}%");
        }
    }

    // Prefetch benefit
    if let (Some(tt), Some(pref)) = (
        results.iter().find(|r| r.mode == TestMode::TTOnly),
        results.iter().find(|r| r.mode == TestMode::TTPrefetch),
    ) {
        let benefit = 100.0 * ((pref.nps as f64 / tt.nps as f64) - 1.0);
        println!("  Prefetch benefit: {benefit:.1}%");

        if let Some(pf) = &pref.prefetch_stats {
            println!("  Prefetch hit rate: {:.1}%", pf.hit_rate * 100.0);
            println!("  Prefetch distance: {}", pf.distance);
        }
    }
}

fn main() {
    println!("=== Phase 4: Enhanced TT Benchmark with 4-Mode Testing ===");
    println!("Comparing: NoTT vs TT(no CAS) vs TTOnly vs TT+Prefetch\n");

    // Test positions
    let positions = vec![
        TestPosition {
            name: "Initial position",
            sfen: "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
        },
        TestPosition {
            name: "Standard opening",
            sfen: "lnsgkgsnl/1r5b1/pppppp1pp/6p2/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL w - 2",
        },
        TestPosition {
            name: "Middle game",
            sfen: "ln1g1g1nl/1ks4r1/1pppsb1pp/p3pp3/9/P1P1PP3/1PSPB1PPP/3R3K1/LN1G1G1NL b Pp 1",
        },
        TestPosition {
            name: "Complex endgame",
            sfen: "8l/4k4/4ppp2/8p/9/8P/4PPP2/4S4/4K4 b GS2r2b2g2s3n3l10p 1",
        },
    ];

    // Test depths
    let depths = vec![4, 5, 6, 7];

    // Test each depth
    for depth in depths {
        println!("\n{}", "=".repeat(100));
        println!("DEPTH {depth} RESULTS");
        println!("{}", "=".repeat(100));

        // Test each position
        for position in &positions {
            let mut results = Vec::new();

            // Run each mode
            for mode in [
                TestMode::NoTT,
                TestMode::TTNoCAS,
                TestMode::TTOnly,
                TestMode::TTPrefetch,
            ] {
                // Create fresh TT for each test mode to ensure fair comparison
                let tt = if mode == TestMode::NoTT {
                    None
                } else {
                    let tt = TranspositionTable::new_with_config(128, None);
                    // Enable prefetch statistics for TTPrefetch mode
                    if mode == TestMode::TTPrefetch {
                        // Note: enable_prefetch_stats method no longer exists
                        // Prefetch functionality is always available
                    }
                    Some(Arc::new(tt))
                };

                let result = run_benchmark(position, depth, mode, tt);

                results.push(result);
            }

            print_depth_results(depth, position, &results);

            // Safety check for long searches
            if depth >= 7 && position.name.contains("Middle") {
                println!("\nSkipping remaining positions at depth {depth} for time constraints");
                break;
            }
        }
    }

    println!("\n{}", "=".repeat(100));
    println!("Benchmark complete!");

    println!("\n=== Summary of Key Findings ===");
    println!("1. Pure TT lookup (no CAS) overhead vs NoTT");
    println!("2. CAS operation overhead vs read-only TT");
    println!("3. Prefetch effectiveness at different depths");
    println!("4. Hashfull and actual TT hit rates");
    println!("5. Node reduction effectiveness");
}
