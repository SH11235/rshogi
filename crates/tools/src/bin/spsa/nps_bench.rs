//! `--nps-scaling`: 開始時に全 slot で同時に固定深さの探索を走らせ、1 thread あたりの NPS を測る。
//!
//! 時計の倍率 = 基準 NPS ÷ 実測 NPS。測るのは run 開始時の θ₀ を送った engine で、摂動や
//! 更新で NPS が動いても持ち時間は変えない (倍率は run 中は固定し、resume のたびに測り直す)。

use super::*;
use tools::selfplay::types::{SearchRequest, TimeArgs};

/// 局面集合・深さ・集計方法の版。これらを変えたら上げる。
pub(super) const BENCH_VERSION: &str = "spsa-bench-v1";
/// 測る局面は benchmark ツールの既定局面 (YaneuraOu の bench と同じ 4 局面)。
const BENCH_DEPTH: u32 = 16;
/// cache と thread の立ち上がりを均すための warmup の深さ。同じ局面集合を 1 周する。
const WARMUP_DEPTH: u32 = 12;
/// 1 局面の探索の期限。超えたら bench を失敗にする。
const BENCH_SEARCH_TIMEOUT_MS: u64 = 600_000;

/// 1 回の bench の結果。
pub(super) struct BenchResult {
    /// slot ごとの 1 thread あたりの NPS (nodes ÷ 秒 ÷ Threads)。
    pub(super) slot_nps: Vec<f64>,
    /// slot 間の平均。
    pub(super) nps: f64,
    pub(super) depth: u32,
    pub(super) warmup_depth: u32,
    pub(super) positions: usize,
}

/// `slots` 個の slot で同時に bench を走らせる。engine は本番と同じ設定 (Threads・Hash・USI option・
/// CPU affinity) で plus/minus の 2 本を起動し、plus 側だけが探索する (対局中も探索するのは手番側だけ)。
pub(super) fn measure(
    ctx: WorkerContext<'_>,
    slots: usize,
    theta0: &[f64],
    threads: usize,
) -> Result<BenchResult> {
    let cancel = AtomicBool::new(false);
    let per_slot: Vec<Result<(u64, u64)>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..slots)
            .map(|idx| {
                let cancel = &cancel;
                scope.spawn(move || {
                    let result = bench_slot(ctx, idx, theta0, cancel);
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
        positions: tools::DEFAULT_POSITIONS.len(),
    })
}

fn per_thread_nps(nodes: u64, elapsed_ms: u64, threads: usize) -> Result<f64> {
    if elapsed_ms == 0 || threads == 0 {
        bail!("bench の計測時間が 0 ms です (nodes={nodes})");
    }
    Ok(nodes as f64 * 1000.0 / elapsed_ms as f64 / threads as f64)
}

/// warmup の後に局面集合を固定深さで探索し、nodes と経過時間の合計を返す。
fn bench_slot(
    ctx: WorkerContext<'_>,
    idx: usize,
    theta0: &[f64],
    cancel: &AtomicBool,
) -> Result<(u64, u64)> {
    let (mut plus, _minus) = spawn_worker_engines(ctx, idx, cancel, true)?;
    apply_parameter_vector(&mut plus, ctx.params, theta0, ctx.translator, ctx.application_mask)?;
    search_positions(&mut plus, ctx.game_cfg.timeout_margin_ms, WARMUP_DEPTH, cancel)?;
    search_positions(&mut plus, ctx.game_cfg.timeout_margin_ms, BENCH_DEPTH, cancel)
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
    for (_, sfen) in tools::DEFAULT_POSITIONS {
        if cancel.load(Ordering::Acquire) {
            return Err(WorkerCancelled.into());
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
pub(super) fn scale_clock(reference_ms: u64, scale: f64) -> u64 {
    if reference_ms == 0 {
        return 0;
    }
    ((reference_ms as f64 * scale).round() as u64).max(1)
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
}
