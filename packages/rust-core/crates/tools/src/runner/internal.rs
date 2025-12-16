//! 内部API直接呼び出しモードでのベンチマーク実行

use std::thread;

use anyhow::{Context, Result};

use engine_core::eval::{set_material_level, MaterialLevel};
use engine_core::nnue::init_nnue;
use engine_core::position::Position;
use engine_core::search::{init_search_module, LimitsType, Search, SearchInfo};

use crate::config::{BenchmarkConfig, LimitType};
use crate::positions::load_positions;
use crate::report::{BenchResult, BenchmarkReport, EvalInfo, ThreadResult};
use crate::system::collect_system_info;
use crate::utils::SEARCH_STACK_SIZE;

/// 内部API直接呼び出しモードでベンチマークを実行
pub fn run_internal_benchmark(config: &BenchmarkConfig) -> Result<BenchmarkReport> {
    // 探索モジュール初期化
    init_search_module();

    // 評価関数設定
    // MaterialLevel設定
    if let Some(level) = MaterialLevel::from_value(config.eval_config.material_level) {
        set_material_level(level);
        println!("MaterialLevel set to: {}", config.eval_config.material_level);
    } else {
        eprintln!(
            "Warning: Invalid MaterialLevel {}, using default",
            config.eval_config.material_level
        );
    }

    // NNUE初期化（指定時のみ）
    if let Some(nnue_path) = &config.eval_config.nnue_file {
        match init_nnue(nnue_path) {
            Ok(()) => {
                println!("NNUE initialized from: {}", nnue_path.display());
            }
            Err(e) => {
                // 既に初期化済みの場合はスキップ（OnceLock）
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    println!("NNUE already initialized, skipping");
                } else {
                    return Err(anyhow::anyhow!("Failed to initialize NNUE: {e}"));
                }
            }
        }
    }

    let positions = load_positions(config)?;
    let mut all_results = Vec::new();

    for threads in &config.threads {
        if *threads > 1 {
            eprintln!("Warning: Multi-threading not yet implemented in internal API mode");
            eprintln!("         Running with single thread instead.");
        }

        println!("=== Threads: {} ===", threads);

        let mut thread_results = Vec::new();
        let tt_mb = config.tt_mb;

        for iteration in 0..config.iterations {
            if config.iterations > 1 {
                println!("Iteration {}/{}", iteration + 1, config.iterations);
            }

            for (name, sfen) in &positions {
                if config.verbose {
                    println!("  Position: {name}");
                }

                // 局面設定
                let mut pos = Position::new();
                pos.set_sfen(sfen).with_context(|| format!("Invalid SFEN: {sfen}"))?;

                // 制限設定
                let mut limits = LimitsType::default();
                limits.set_start_time();

                match config.limit_type {
                    LimitType::Depth => limits.depth = config.limit as i32,
                    LimitType::Nodes => limits.nodes = config.limit,
                    LimitType::Movetime => limits.movetime = config.limit as i64,
                }

                // 探索実行（専用スタックサイズのスレッドで実行）
                // 各局面ごとに新しいSearchオブジェクトを作成
                let verbose = config.verbose;
                let sfen_clone = sfen.to_string();
                let bench_result = thread::Builder::new()
                    .stack_size(SEARCH_STACK_SIZE)
                    .spawn(move || {
                        // スレッド内でSearchエンジンを作成
                        let mut search = Search::new(tt_mb as usize);

                        let mut last_info: Option<SearchInfo> = None;
                        let result = search.go(
                            &mut pos,
                            limits,
                            Some(|info: &SearchInfo| {
                                last_info = Some(info.clone());
                                if verbose {
                                    println!("    {}", info.to_usi_string());
                                }
                            }),
                        );

                        // 結果収集
                        BenchResult {
                            sfen: sfen_clone,
                            depth: result.depth,
                            nodes: result.nodes,
                            time_ms: last_info.as_ref().map(|i| i.time_ms).unwrap_or(0),
                            nps: last_info.as_ref().map(|i| i.nps).unwrap_or(0),
                            hashfull: last_info.as_ref().map(|i| i.hashfull).unwrap_or(0),
                            bestmove: result.best_move.to_usi(),
                        }
                    })
                    .with_context(|| "Failed to spawn search thread")?
                    .join()
                    .map_err(|e| {
                        let panic_msg = if let Some(s) = e.downcast_ref::<&str>() {
                            s.to_string()
                        } else if let Some(s) = e.downcast_ref::<String>() {
                            s.clone()
                        } else {
                            "Unknown panic".to_string()
                        };
                        anyhow::anyhow!("Search thread panicked: {panic_msg}")
                    })?;

                if config.verbose {
                    println!(
                        "    depth={} nodes={} time={}ms nps={}",
                        bench_result.depth,
                        bench_result.nodes,
                        bench_result.time_ms,
                        bench_result.nps
                    );
                }

                thread_results.push(bench_result);
            }
        }

        all_results.push(ThreadResult {
            threads: *threads,
            results: thread_results,
        });
    }

    Ok(BenchmarkReport {
        system_info: collect_system_info(),
        engine_name: Some("internal".to_string()),
        engine_path: None,
        eval_info: Some(EvalInfo::from(&config.eval_config)),
        results: all_results,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::EvalConfig;

    fn test_config(limit_type: LimitType, limit: u64) -> BenchmarkConfig {
        BenchmarkConfig {
            threads: vec![1],
            tt_mb: 16,
            limit_type,
            limit,
            sfens: None,
            iterations: 1,
            verbose: false,
            eval_config: EvalConfig::default(),
        }
    }

    #[test]
    fn test_benchmark_with_default_positions() {
        let config = test_config(LimitType::Depth, 5);
        let result = run_internal_benchmark(&config);
        assert!(result.is_ok(), "Benchmark failed: {:?}", result.err());

        let report = result.unwrap();

        assert_eq!(report.results.len(), 1);
        assert_eq!(report.results[0].threads, 1);
        assert_eq!(report.results[0].results.len(), 4, "Should have 4 default positions");

        for (i, bench_result) in report.results[0].results.iter().enumerate() {
            assert!(!bench_result.sfen.is_empty(), "Position {i}: SFEN should not be empty");
            assert!(bench_result.depth >= 1, "Position {i}: Depth should be at least 1");
            assert!(bench_result.nodes > 0, "Position {i}: Nodes should be positive");
            assert_ne!(bench_result.bestmove, "none", "Position {i}: Bestmove should be valid");
        }
    }

    #[test]
    fn test_benchmark_multiple_iterations() {
        let mut config = test_config(LimitType::Depth, 3);
        config.iterations = 2;

        let result = run_internal_benchmark(&config);
        assert!(result.is_ok());

        let report = result.unwrap();
        // 2 iterations × 4 positions = 8 results
        assert_eq!(report.results[0].results.len(), 8);
    }

    #[test]
    fn test_benchmark_nodes_limit() {
        let config = test_config(LimitType::Nodes, 1000);
        let result = run_internal_benchmark(&config);
        assert!(result.is_ok());

        let report = result.unwrap();
        for bench_result in &report.results[0].results {
            // ノード数が制限値付近であること（多少のオーバーランは許容）
            assert!(
                bench_result.nodes <= 2000,
                "Nodes {} should be close to limit 1000",
                bench_result.nodes
            );
        }
    }
}
