//! `--nps-scaling` の NPS bench (spsa と tournament で共有)。
//!
//! 全 slot で同時に固定深さの探索を走らせ、1 thread あたりの NPS を測る。時計の倍率は
//! 基準 NPS ÷ 実測 NPS で、持ち時間と加算の両方に掛ける。

use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, bail};

use super::EngineProcess;
use super::types::{SearchRequest, TimeArgs};

/// 局面集合・深さ・集計方法の版。これらを変えたら上げる。spsa の run fingerprint にも入る。
pub const BENCH_VERSION: &str = "spsa-bench-v1";
pub const DEFAULT_REFERENCE_NPS: u64 = 350_000;
pub const DEFAULT_MIN_NPS_RATIO: f64 = 0.5;
/// 測る局面は benchmark ツールの既定局面 (YaneuraOu の bench と同じ 4 局面)。
const BENCH_DEPTH: u32 = 16;
/// cache と thread の立ち上がりを均すための warmup の深さ。同じ局面集合を 1 周する。
const WARMUP_DEPTH: u32 = 12;
/// 1 局面の探索の期限。超えたら bench を失敗にする。
const BENCH_SEARCH_TIMEOUT_MS: u64 = 600_000;

/// 他の slot の失敗で bench を打ち切ったことを表す。
#[derive(Debug)]
pub struct BenchCancelled;

impl std::fmt::Display for BenchCancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NPS bench cancelled")
    }
}

impl std::error::Error for BenchCancelled {}

/// 1 回の bench の結果。
pub struct BenchResult {
    /// slot ごとの 1 thread あたりの NPS (nodes ÷ 秒 ÷ Threads)。
    pub slot_nps: Vec<f64>,
    /// slot 間の平均。
    pub nps: f64,
    pub depth: u32,
    pub warmup_depth: u32,
    pub positions: usize,
}

impl BenchResult {
    pub fn summary(&self) -> String {
        format!(
            "nps bench: {:.0} nps/thread (slots: {})",
            self.nps,
            self.slot_nps.iter().map(|v| format!("{v:.0}")).collect::<Vec<_>>().join(", ")
        )
    }

    /// 実測が基準の `min_ratio` 倍を下回る機械では起動しない。
    pub fn ensure_above_floor(&self, reference_nps: u64, min_ratio: f64) -> Result<()> {
        let reference = reference_nps as f64;
        if self.nps < reference * min_ratio {
            bail!(
                "この機械の NPS {:.0} (1 thread あたり) が基準 {reference:.0} の {min_ratio} 倍を下回るので起動しません",
                self.nps
            );
        }
        Ok(())
    }
}

/// `slots` 個の slot で同時に bench を走らせる。
///
/// `prepare(idx, cancel)` は本番と同じ設定 (Threads・Hash・USI option・CPU affinity) で起動した
/// 探索側の engine と、対局と同じく常駐させておくだけの engine 群を返す。探索するのは前者だけ
/// (対局中も探索するのは手番側だけ)。
pub fn measure<K, F>(
    slots: usize,
    threads: usize,
    timeout_margin_ms: u64,
    prepare: F,
) -> Result<BenchResult>
where
    F: Fn(usize, &AtomicBool) -> Result<(EngineProcess, K)> + Sync,
{
    let cancel = AtomicBool::new(false);
    let per_slot: Vec<Result<(u64, u64)>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..slots)
            .map(|idx| {
                let cancel = &cancel;
                let prepare = &prepare;
                scope.spawn(move || {
                    let result = bench_slot(idx, timeout_margin_ms, prepare, cancel);
                    if result.is_err() {
                        cancel.store(true, Ordering::Release);
                    }
                    result
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_else(|_| Err(anyhow::anyhow!("bench thread panic"))))
            .collect()
    });
    let mut slot_nps = Vec::with_capacity(slots);
    for (idx, result) in per_slot.into_iter().enumerate() {
        let (nodes, elapsed_ms) = result.with_context(|| format!("NPS bench slot {idx}"))?;
        slot_nps.push(per_thread_nps(nodes, elapsed_ms, threads)?);
    }
    let nps = slot_nps.iter().sum::<f64>() / slot_nps.len() as f64;
    Ok(BenchResult {
        slot_nps,
        nps,
        depth: BENCH_DEPTH,
        warmup_depth: WARMUP_DEPTH,
        positions: crate::DEFAULT_POSITIONS.len(),
    })
}

fn per_thread_nps(nodes: u64, elapsed_ms: u64, threads: usize) -> Result<f64> {
    if elapsed_ms == 0 || threads == 0 {
        bail!("bench の計測時間が 0 ms です (nodes={nodes})");
    }
    Ok(nodes as f64 * 1000.0 / elapsed_ms as f64 / threads as f64)
}

/// warmup の後に局面集合を固定深さで探索し、nodes と経過時間の合計を返す。
fn bench_slot<K, F>(
    idx: usize,
    timeout_margin_ms: u64,
    prepare: &F,
    cancel: &AtomicBool,
) -> Result<(u64, u64)>
where
    F: Fn(usize, &AtomicBool) -> Result<(EngineProcess, K)>,
{
    let (mut engine, _resident) = prepare(idx, cancel)?;
    search_positions(&mut engine, timeout_margin_ms, WARMUP_DEPTH, cancel)?;
    search_positions(&mut engine, timeout_margin_ms, BENCH_DEPTH, cancel)
}

/// 局面集合を `depth` で 1 周し、engine が報告した nodes と、go から bestmove までの時間を合計する。
fn search_positions(
    engine: &mut EngineProcess,
    timeout_margin_ms: u64,
    depth: u32,
    cancel: &AtomicBool,
) -> Result<(u64, u64)> {
    let mut nodes = 0u64;
    let mut elapsed_ms = 0u64;
    for (_, sfen) in crate::DEFAULT_POSITIONS {
        if cancel.load(Ordering::Acquire) {
            return Err(BenchCancelled.into());
        }
        engine.new_game()?;
        let request = SearchRequest {
            sfen,
            moves: "",
            time_args: TimeArgs {
                btime: 0,
                wtime: 0,
                byoyomi: 0,
                binc: 0,
                winc: 0,
            },
            think_limit_ms: 0,
            timeout_margin_ms,
            limit_only_timeout_ms: Some(BENCH_SEARCH_TIMEOUT_MS),
            game_id: 0,
            ply: 1,
            side: rshogi_core::types::Color::Black,
            engine_label: engine.label.clone(),
            pass_rights: None,
            go_depth: Some(depth),
            go_nodes: None,
        };
        let outcome = engine.search(&request, None)?;
        if outcome.watchdog_fired || outcome.bestmove.is_none() {
            bail!(
                "depth {depth} の探索が {} s 内に終わりませんでした",
                BENCH_SEARCH_TIMEOUT_MS / 1000
            );
        }
        let searched = outcome
            .eval
            .and_then(|eval| eval.nodes)
            .context("engine が info の nodes を出しませんでした")?;
        nodes = nodes.saturating_add(searched);
        elapsed_ms = elapsed_ms.saturating_add(outcome.elapsed_ms);
    }
    Ok((nodes, elapsed_ms))
}

/// 基準 TC に倍率を掛けた時計。0 でない値は 1 ms 未満に丸めない。
pub fn scale_clock(reference_ms: u64, scale: f64) -> u64 {
    if reference_ms == 0 {
        return 0;
    }
    ((reference_ms as f64 * scale).round() as u64).max(1)
}

/// bench は `--threads` で割るので、USI option で Threads を上書きしていないことを確かめる。
pub fn sets_threads_option<'a>(options: impl IntoIterator<Item = &'a String>) -> bool {
    options.into_iter().any(|o| {
        o.split_once('=')
            .is_some_and(|(name, _)| name.trim().eq_ignore_ascii_case("Threads"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_thread_nps_divides_by_seconds_and_threads() {
        assert_eq!(per_thread_nps(7_000_000, 2_500, 8).unwrap(), 350_000.0);
        assert!(per_thread_nps(1, 0, 8).is_err());
    }

    #[test]
    fn clock_scaling_rounds_and_keeps_nonzero_values() {
        // 速い機械 (実測 700k) では基準 350k の時計が半分になる。
        let scale = 350_000.0 / 700_000.0;
        assert_eq!(scale_clock(120_000, scale), 60_000);
        assert_eq!(scale_clock(2_000, scale), 1_000);
        assert_eq!(scale_clock(0, scale), 0);
        assert_eq!(scale_clock(1, 0.001), 1);
        assert_eq!(scale_clock(1_001, 0.5), 501);
    }

    #[test]
    fn floor_rejects_machines_below_ratio() {
        let result = BenchResult {
            slot_nps: vec![170_000.0],
            nps: 170_000.0,
            depth: BENCH_DEPTH,
            warmup_depth: WARMUP_DEPTH,
            positions: 4,
        };
        assert!(result.ensure_above_floor(350_000, 0.5).is_err());
        assert!(result.ensure_above_floor(340_000, 0.5).is_ok());
    }

    #[test]
    fn threads_option_is_detected_case_insensitively() {
        let options = ["USI_Hash=256".to_owned(), " threads = 4".to_owned()];
        assert!(sets_threads_option(&options));
        assert!(!sets_threads_option(&options[..1]));
    }
}
