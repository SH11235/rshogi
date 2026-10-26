//! `--nps-scaling`: 開始時に全 slot で同時に固定深さの探索を走らせ、1 thread あたりの NPS を測る。
//!
//! 時計の倍率 = 基準 NPS ÷ 実測 NPS。測るのは run 開始時の θ₀ を送った engine で、摂動や
//! 更新で NPS が動いても持ち時間は変えない (倍率は run 中は固定し、resume のたびに測り直す)。

use super::*;
pub(super) use tools::selfplay::nps_bench::{BENCH_VERSION, BenchResult, scale_clock};

/// `slots` 個の slot で同時に bench を走らせる。engine は本番と同じ設定 (Threads・Hash・USI option・
/// CPU affinity) で plus/minus の 2 本を起動し、plus 側だけが探索する。
pub(super) fn measure(
    ctx: WorkerContext<'_>,
    slots: usize,
    theta0: &[f64],
    threads: usize,
) -> Result<BenchResult> {
    tools::selfplay::nps_bench::measure(
        slots,
        threads,
        ctx.game_cfg.timeout_margin_ms,
        |idx, cancel| {
            let (mut plus, minus) = spawn_worker_engines(ctx, idx, cancel, true)?;
            apply_parameter_vector(
                &mut plus,
                ctx.params,
                theta0,
                ctx.translator,
                ctx.application_mask,
            )?;
            Ok((plus, minus))
        },
    )
}
