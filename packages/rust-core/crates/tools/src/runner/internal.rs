//! 内部API直接呼び出しモードでのベンチマーク実行

use std::sync::mpsc;
use std::thread;

use anyhow::{Context, Result};

use engine_core::eval::{set_material_level, MaterialLevel};
use engine_core::nnue::init_nnue;
use engine_core::position::Position;
use engine_core::search::{LimitsType, Search, SearchInfo};

use crate::config::{BenchmarkConfig, LimitType};
use crate::positions::load_positions;
use crate::report::{BenchResult, BenchmarkReport, EvalInfo, ThreadResult};
use crate::system::collect_system_info;
use crate::utils::SEARCH_STACK_SIZE;

/// 内部API直接呼び出しモードでベンチマークを実行
pub fn run_internal_benchmark(config: &BenchmarkConfig) -> Result<BenchmarkReport> {
    // 評価関数の共通設定
    setup_eval(config)?;

    if config.reuse_search {
        run_internal_benchmark_reuse(config)
    } else {
        run_internal_benchmark_standard(config)
    }
}

/// 評価関数の初期化
fn setup_eval(config: &BenchmarkConfig) -> Result<()> {
    // MaterialLevelはNNUE初期化前に設定する必要がある
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
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                println!("NNUE already initialized, skipping");
            }
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "Failed to initialize NNUE from '{}': {e}",
                    nnue_path.display()
                ));
            }
        }
    }
    Ok(())
}

/// 標準モードでベンチマークを実行（各局面ごとに新しいSearchを作成）
fn run_internal_benchmark_standard(config: &BenchmarkConfig) -> Result<BenchmarkReport> {
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
                let verbose = config.verbose;
                let sfen_clone = sfen.to_string();
                let bench_result = thread::Builder::new()
                    .stack_size(SEARCH_STACK_SIZE)
                    .spawn(move || {
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

                        BenchResult {
                            sfen: sfen_clone,
                            depth: result.depth,
                            nodes: result.nodes,
                            time_ms: last_info.as_ref().map(|i| i.time_ms).unwrap_or(0),
                            nps: last_info.as_ref().map(|i| i.nps).unwrap_or(0),
                            hashfull: last_info.as_ref().map(|i| i.hashfull).unwrap_or(0),
                            bestmove: result.best_move.to_usi(),
                            is_warmup: None,
                            search_run_index: None,
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

/// Search再利用モードでベンチマークを実行（履歴統計の蓄積効果を測定）
fn run_internal_benchmark_reuse(config: &BenchmarkConfig) -> Result<BenchmarkReport> {
    let positions = load_positions(config)?;
    let mut all_results = Vec::new();

    for threads in &config.threads {
        if *threads > 1 {
            eprintln!("Warning: Multi-threading not yet implemented in internal API mode");
            eprintln!("         Running with single thread instead.");
        }

        println!("=== Threads: {} (reuse_search mode) ===", threads);

        // 設定値をキャプチャ
        let tt_mb = config.tt_mb;
        let positions_clone = positions.clone();
        let iterations = config.iterations;
        let warmup = config.warmup;
        let verbose = config.verbose;
        let limit_type = config.limit_type;
        let limit = config.limit;

        // チャネルで結果を受け取る
        let (tx, rx) = mpsc::channel::<BenchResult>();

        // 専用ワーカースレッドで全局面を探索
        let handle = thread::Builder::new()
            .stack_size(SEARCH_STACK_SIZE)
            .spawn(move || {
                // Searchインスタンスを1回だけ作成
                let mut search = Search::new(tt_mb as usize);
                let mut search_run_index: u32 = 0;

                // ウォームアップフェーズ
                for warmup_iter in 0..warmup {
                    if verbose {
                        println!("Warmup {}/{}", warmup_iter + 1, warmup);
                    }
                    for (name, sfen) in &positions_clone {
                        if verbose {
                            println!("  Position: {name} (warmup)");
                        }
                        let result = run_single_search(
                            &mut search,
                            sfen,
                            limit_type,
                            limit,
                            verbose,
                            true,
                            search_run_index,
                        );
                        let _ = tx.send(result);
                        search_run_index += 1;
                    }
                }

                // 本番フェーズ
                for iteration in 0..iterations {
                    if iterations > 1 {
                        println!("Iteration {}/{}", iteration + 1, iterations);
                    }
                    for (name, sfen) in &positions_clone {
                        if verbose {
                            println!("  Position: {name}");
                        }
                        let result = run_single_search(
                            &mut search,
                            sfen,
                            limit_type,
                            limit,
                            verbose,
                            false,
                            search_run_index,
                        );
                        if verbose {
                            println!(
                                "    depth={} nodes={} time={}ms nps={}",
                                result.depth, result.nodes, result.time_ms, result.nps
                            );
                        }
                        let _ = tx.send(result);
                        search_run_index += 1;
                    }
                }
            })
            .with_context(|| "Failed to spawn worker thread")?;

        // スレッド終了を待機（tx はスレッド内でスコープ終了時にドロップされる）
        handle.join().map_err(|_| anyhow::anyhow!("Worker thread panicked"))?;

        // 結果を収集（スレッド終了後、チャネルは自動的にクローズされている）
        let thread_results: Vec<BenchResult> = rx.into_iter().collect();

        all_results.push(ThreadResult {
            threads: *threads,
            results: thread_results,
        });
    }

    Ok(BenchmarkReport {
        system_info: collect_system_info(),
        engine_name: Some("internal (reuse_search)".to_string()),
        engine_path: None,
        eval_info: Some(EvalInfo::from(&config.eval_config)),
        results: all_results,
    })
}

/// 単一局面の探索を実行（ヘルパー関数）
fn run_single_search(
    search: &mut Search,
    sfen: &str,
    limit_type: LimitType,
    limit: u64,
    verbose: bool,
    is_warmup: bool,
    search_run_index: u32,
) -> BenchResult {
    let mut pos = Position::new();
    if let Err(e) = pos.set_sfen(sfen) {
        eprintln!("Error setting SFEN: {e}");
        return BenchResult {
            sfen: sfen.to_string(),
            depth: 0,
            nodes: 0,
            time_ms: 0,
            nps: 0,
            hashfull: 0,
            bestmove: "none".to_string(),
            is_warmup: Some(is_warmup),
            search_run_index: Some(search_run_index),
        };
    }

    let mut limits = LimitsType::default();
    limits.set_start_time();
    match limit_type {
        LimitType::Depth => limits.depth = limit as i32,
        LimitType::Nodes => limits.nodes = limit,
        LimitType::Movetime => limits.movetime = limit as i64,
    }

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

    BenchResult {
        sfen: sfen.to_string(),
        depth: result.depth,
        nodes: result.nodes,
        time_ms: last_info.as_ref().map(|i| i.time_ms).unwrap_or(0),
        nps: last_info.as_ref().map(|i| i.nps).unwrap_or(0),
        hashfull: last_info.as_ref().map(|i| i.hashfull).unwrap_or(0),
        bestmove: result.best_move.to_usi(),
        is_warmup: Some(is_warmup),
        search_run_index: Some(search_run_index),
    }
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
            reuse_search: false,
            warmup: 0,
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

    #[test]
    fn test_material_level_configuration() {
        let mut config = test_config(LimitType::Nodes, 10000);
        config.eval_config.material_level = 1;

        let result = run_internal_benchmark(&config);
        assert!(result.is_ok());

        let report = result.unwrap();
        assert!(report.eval_info.is_some());
        assert_eq!(report.eval_info.unwrap().material_level, 1);
    }

    #[test]
    fn test_invalid_material_level_uses_default() {
        let mut config = test_config(LimitType::Nodes, 10000);
        config.eval_config.material_level = 99; // 不正な値

        // 不正な値でも実行は成功し、デフォルト値が使用されるべき
        let result = run_internal_benchmark(&config);
        assert!(result.is_ok());
    }

    #[test]
    fn test_reuse_search_mode() {
        let mut config = test_config(LimitType::Depth, 3);
        config.reuse_search = true;
        config.iterations = 2;

        let result = run_internal_benchmark(&config);
        assert!(result.is_ok(), "Reuse search benchmark failed: {:?}", result.err());

        let report = result.unwrap();
        // 2 iterations × 4 positions = 8 results
        assert_eq!(report.results[0].results.len(), 8);

        // search_run_indexが連番になっている
        for (i, r) in report.results[0].results.iter().enumerate() {
            assert_eq!(r.search_run_index, Some(i as u32));
            assert_eq!(r.is_warmup, Some(false));
        }
    }

    #[test]
    fn test_reuse_search_with_warmup() {
        let mut config = test_config(LimitType::Depth, 2);
        config.reuse_search = true;
        config.warmup = 1;
        config.iterations = 1;

        let result = run_internal_benchmark(&config);
        assert!(result.is_ok());

        let report = result.unwrap();
        // 1 warmup × 4 positions + 1 iteration × 4 positions = 8 results
        assert_eq!(report.results[0].results.len(), 8);

        // 最初の4つはウォームアップ
        for r in &report.results[0].results[..4] {
            assert_eq!(r.is_warmup, Some(true));
        }
        // 残りは本番
        for r in &report.results[0].results[4..] {
            assert_eq!(r.is_warmup, Some(false));
        }
    }
}
