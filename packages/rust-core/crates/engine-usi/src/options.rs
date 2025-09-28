use anyhow::Result;
use std::error::Error as StdError;

use engine_core::engine::controller::EngineType;
use engine_core::evaluation::nnue::error::NNUEError;

use crate::io::{info_string, usi_println};
use crate::state::{EngineState, UsiOptions};

pub fn send_id_and_options(opts: &UsiOptions) {
    usi_println("id name RustShogi USI (core)");
    usi_println("id author RustShogi Team");

    usi_println(&format!(
        "option name USI_Hash type spin default {} min 1 max 1024",
        opts.hash_mb
    ));
    usi_println(&format!("option name Threads type spin default {} min 1 max 256", opts.threads));
    usi_println("option name USI_Ponder type check default true");
    usi_println(&format!("option name MultiPV type spin default {} min 1 max 20", opts.multipv));
    usi_println(&format!(
        "option name MinThinkMs type spin default {} min 0 max 10000",
        opts.min_think_ms
    ));
    print_engine_type_options();
    usi_println("option name EvalFile type filename default ");
    usi_println("option name ClearHash type button");
    usi_println("option name SIMDMaxLevel type combo default Auto var Auto var Scalar var SSE2 var AVX var AVX512F");
    usi_println(
        "option name NNUE_Simd type combo default Auto var Auto var Scalar var SSE41 var AVX2",
    );
    print_time_policy_options(opts);
    usi_println("option name Stochastic_Ponder type check default false");
    usi_println("option name ForceTerminateOnHardDeadline type check default true");
    usi_println("option name MateEarlyStop type check default true");
}

pub fn handle_setoption(cmd: &str, state: &mut EngineState) -> Result<()> {
    if !cmd.starts_with("setoption") {
        return Ok(());
    }

    let body = cmd.strip_prefix("setoption").unwrap_or("").trim();
    if body.is_empty() {
        return Ok(());
    }

    let (name, value) = if let Some(name_pos) = body.find("name") {
        let after_name = body[name_pos + 4..].trim_start();
        if let Some(value_pos) = after_name.find(" value ") {
            (
                Some(after_name[..value_pos].trim().to_string()),
                Some(after_name[value_pos + 7..].trim().to_string()),
            )
        } else {
            (Some(after_name.trim().to_string()), None)
        }
    } else {
        (None, None)
    };

    let Some(name) = name.filter(|n| !n.is_empty()) else {
        return Ok(());
    };
    let value_ref = value.as_deref();

    match name.as_str() {
        "USI_Hash" => {
            if let Some(v) = value_ref {
                if let Ok(mb) = v.parse::<usize>() {
                    state.opts.hash_mb = mb;
                }
            }
        }
        "Threads" => {
            if let Some(v) = value_ref {
                if let Ok(t) = v.parse::<usize>() {
                    state.opts.threads = t;
                }
            }
        }
        "USI_Ponder" => {
            if let Some(v) = value_ref {
                let v = v.to_lowercase();
                state.opts.ponder = matches!(v.as_str(), "true" | "1" | "on");
            }
        }
        "MultiPV" => {
            if let Some(v) = value_ref {
                if let Ok(k) = v.parse::<u8>() {
                    state.opts.multipv = k.clamp(1, 20);
                    if let Ok(mut eng) = state.engine.lock() {
                        eng.set_multipv_persistent(state.opts.multipv);
                    }
                }
            }
        }
        "MinThinkMs" => {
            if let Some(v) = value_ref {
                if let Ok(ms) = v.parse::<u64>() {
                    state.opts.min_think_ms = ms;
                }
            }
        }
        "SIMDMaxLevel" => {
            if let Some(v) = value_ref {
                let env_val = match v.to_lowercase().as_str() {
                    "auto" => None,
                    "scalar" => Some("scalar"),
                    "sse2" => Some("sse2"),
                    "avx" => Some("avx"),
                    "avx512f" | "avx512" => Some("avx512f"),
                    _ => None,
                };
                state.opts.simd_max_level = env_val.map(|s| s.to_string());
                if let Some(ref e) = state.opts.simd_max_level {
                    std::env::set_var("SHOGI_SIMD_MAX", e);
                    info_string(format!("simd_clamp={}", e));
                } else {
                    std::env::remove_var("SHOGI_SIMD_MAX");
                    info_string("simd_clamp=auto");
                }
                info_string("simd_clamp_note=may_not_apply_after_init");
            }
        }
        "NNUE_Simd" => {
            if let Some(v) = value_ref {
                let env_val = match v.to_lowercase().as_str() {
                    "auto" => None,
                    "scalar" => Some("scalar"),
                    "sse41" | "sse4.1" => Some("sse41"),
                    "avx2" => Some("avx2"),
                    _ => None,
                };
                state.opts.nnue_simd = env_val.map(|s| s.to_string());
                if let Some(ref e) = state.opts.nnue_simd {
                    std::env::set_var("SHOGI_NNUE_SIMD", e);
                    info_string(format!("nnue_simd_clamp={}", e));
                } else {
                    std::env::remove_var("SHOGI_NNUE_SIMD");
                    info_string("nnue_simd_clamp=auto");
                }
                info_string("nnue_simd_note=may_not_apply_after_init");
            }
        }
        "EngineType" => {
            if let Some(val) = value_ref {
                let et = match val.trim() {
                    "Material" => EngineType::Material,
                    "Enhanced" => EngineType::Enhanced,
                    "Nnue" => EngineType::Nnue,
                    "EnhancedNnue" => EngineType::EnhancedNnue,
                    _ => EngineType::Material,
                };
                state.opts.engine_type = et;
            }
        }
        "EvalFile" => {
            if let Some(v) = value_ref {
                state.opts.eval_file = Some(v.to_string());
            }
        }
        "ClearHash" => {
            if let Ok(mut eng) = state.engine.lock() {
                eng.set_multipv_persistent(state.opts.multipv);
                eng.clear_hash();
            }
        }
        "OverheadMs" => {
            if let Some(v) = value_ref {
                if let Ok(ms) = v.parse::<u64>() {
                    state.opts.overhead_ms = ms;
                }
            }
        }
        "ByoyomiOverheadMs" => {
            if let Some(v) = value_ref {
                if let Ok(ms) = v.parse::<u64>() {
                    state.opts.network_delay2_ms = ms;
                }
            }
        }
        "ByoyomiSafetyMs" => {
            if let Some(v) = value_ref {
                if let Ok(ms) = v.parse::<u64>() {
                    state.opts.byoyomi_safety_ms = ms;
                }
            }
        }
        "ByoyomiPeriods" => {
            if let Some(v) = value_ref {
                if let Ok(p) = v.parse::<u32>() {
                    state.opts.byoyomi_periods = p.clamp(1, 10);
                }
            }
        }
        "ByoyomiEarlyFinishRatio" => {
            if let Some(v) = value_ref {
                if let Ok(r) = v.parse::<u8>() {
                    state.opts.byoyomi_early_finish_ratio = r.clamp(50, 95);
                }
            }
        }
        "PVStabilityBase" => {
            if let Some(v) = value_ref {
                if let Ok(ms) = v.parse::<u64>() {
                    state.opts.pv_stability_base = ms.clamp(10, 200);
                }
            }
        }
        "PVStabilitySlope" => {
            if let Some(v) = value_ref {
                if let Ok(ms) = v.parse::<u64>() {
                    state.opts.pv_stability_slope = ms.clamp(0, 20);
                }
            }
        }
        "SlowMover" => {
            if let Some(v) = value_ref {
                if let Ok(p) = v.parse::<u8>() {
                    state.opts.slow_mover_pct = p.clamp(50, 200);
                }
            }
        }
        "MaxTimeRatioPct" => {
            if let Some(v) = value_ref {
                if let Ok(p) = v.parse::<u32>() {
                    state.opts.max_time_ratio_pct = p.clamp(100, 800);
                }
            }
        }
        "MoveHorizonTriggerMs" => {
            if let Some(v) = value_ref {
                if let Ok(ms) = v.parse::<u64>() {
                    state.opts.move_horizon_trigger_ms = ms;
                }
            }
        }
        "MoveHorizonMinMoves" => {
            if let Some(v) = value_ref {
                if let Ok(m) = v.parse::<u32>() {
                    state.opts.move_horizon_min_moves = m;
                }
            }
        }
        "Stochastic_Ponder" => {
            if let Some(v) = value_ref {
                let v = v.to_lowercase();
                state.opts.stochastic_ponder = matches!(v.as_str(), "true" | "1" | "on");
            }
        }
        "ForceTerminateOnHardDeadline" => {
            if let Some(v) = value_ref {
                let v = v.to_lowercase();
                state.opts.force_terminate_on_hard_deadline =
                    matches!(v.as_str(), "true" | "1" | "on");
            }
        }
        "MateEarlyStop" => {
            if let Some(v) = value_ref {
                let v = v.to_lowercase();
                state.opts.mate_early_stop = matches!(v.as_str(), "true" | "1" | "on");
                engine_core::search::config::set_mate_early_stop_enabled(
                    state.opts.mate_early_stop,
                );
            }
        }
        "StopWaitMs" => {
            if let Some(v) = value_ref {
                if let Ok(ms) = v.parse::<u64>() {
                    state.opts.stop_wait_ms = ms.min(2000);
                }
            }
        }
        "GameoverSendsBestmove" => {
            if let Some(v) = value_ref {
                let v = v.to_lowercase();
                state.opts.gameover_sends_bestmove = matches!(v.as_str(), "true" | "1" | "on");
            }
        }
        "FailSafeGuard" => {
            if let Some(v) = value_ref {
                let v = v.to_lowercase();
                state.opts.fail_safe_guard = matches!(v.as_str(), "true" | "1" | "on");
            }
        }
        _ => {}
    }

    Ok(())
}

pub fn apply_options_to_engine(state: &mut EngineState) {
    if let Ok(ref mut eng) = state.engine.lock() {
        eng.set_engine_type(state.opts.engine_type);
        eng.set_threads(state.opts.threads);
        eng.set_hash_size(state.opts.hash_mb);
        eng.set_multipv_persistent(state.opts.multipv);
        if matches!(state.opts.engine_type, EngineType::Nnue | EngineType::EnhancedNnue) {
            if let Some(ref path) = state.opts.eval_file {
                if !path.is_empty() {
                    if let Err(e) = eng.load_nnue_weights(path) {
                        log_nnue_load_error(path, &*e);
                    }
                    if let Some(kind) = eng.nnue_backend_kind() {
                        info_string(format!("nnue_backend={}", kind));
                    }
                }
            }
        }
    }
    engine_core::search::config::set_mate_early_stop_enabled(state.opts.mate_early_stop);
}

fn log_nnue_load_error(path: &str, err: &(dyn StdError + 'static)) {
    if let Some(ne) = err.downcast_ref::<NNUEError>() {
        match ne {
            NNUEError::Weights(we) => {
                log::error!("[NNUE] Failed to load classic weights '{}': {}", path, we);
                if let Some(src) = we.source() {
                    log::debug!("  caused by: {}", src);
                }
            }
            NNUEError::SingleWeights(se) => {
                log::error!("[NNUE] Failed to load SINGLE weights '{}': {}", path, se);
                if let Some(src) = se.source() {
                    log::debug!("  caused by: {}", src);
                }
            }
            NNUEError::BothWeightsLoadFailed { classic, single } => {
                log::error!(
                    "[NNUE] Failed to load weights '{}': classic={}, single={}",
                    path,
                    classic,
                    single
                );
                if let Some(src) = classic.source() {
                    log::debug!("  classic caused by: {}", src);
                }
                if let Some(src) = single.source() {
                    log::debug!("  single caused by: {}", src);
                }
            }
            NNUEError::Io(ioe) => {
                log::error!("[NNUE] I/O error reading '{}': {}", path, ioe);
            }
            NNUEError::KingNotFound(color) => {
                log::error!("[NNUE] Internal error: king not found for {:?}", color);
            }
            NNUEError::EmptyAccumulatorStack => {
                log::error!("[NNUE] Internal error: empty accumulator stack");
            }
            NNUEError::InvalidPiece(sq) => {
                log::error!("[NNUE] Internal error: invalid piece at {:?}", sq);
            }
            NNUEError::InvalidMove(desc) => {
                log::error!("[NNUE] Internal error: invalid move: {}", desc);
            }
            NNUEError::DimensionMismatch { expected, actual } => {
                log::error!(
                    "[NNUE] Weight dimension mismatch (expected {}, got {}) for '{}': please use matching weights",
                    expected, actual, path
                );
            }
            _ => {
                log::error!("[NNUE] Error while loading weights '{}': {}", path, ne);
            }
        }
        return;
    }

    log::error!("[NNUE] Failed to load NNUE weights '{}': {}", path, err);
}

fn print_engine_type_options() {
    usi_println("option name EngineType type combo default Material var Material var Enhanced var Nnue var EnhancedNnue");
}

fn print_time_policy_options(opts: &UsiOptions) {
    usi_println(&format!(
        "option name OverheadMs type spin default {} min 0 max 5000",
        opts.overhead_ms
    ));
    usi_println(&format!(
        "option name ByoyomiOverheadMs type spin default {} min 0 max 5000",
        opts.network_delay2_ms
    ));
    usi_println(&format!(
        "option name ByoyomiSafetyMs type spin default {} min 0 max 2000",
        opts.byoyomi_safety_ms
    ));
    usi_println(&format!(
        "option name ByoyomiPeriods type spin default {} min 1 max 10",
        opts.byoyomi_periods
    ));
    usi_println(&format!(
        "option name ByoyomiEarlyFinishRatio type spin default {} min 50 max 95",
        opts.byoyomi_early_finish_ratio
    ));
    usi_println(&format!(
        "option name PVStabilityBase type spin default {} min 10 max 200",
        opts.pv_stability_base
    ));
    usi_println(&format!(
        "option name PVStabilitySlope type spin default {} min 0 max 20",
        opts.pv_stability_slope
    ));
    usi_println(&format!(
        "option name SlowMover type spin default {} min 50 max 200",
        opts.slow_mover_pct
    ));
    usi_println(&format!(
        "option name MaxTimeRatioPct type spin default {} min 100 max 800",
        opts.max_time_ratio_pct
    ));
    usi_println(&format!(
        "option name MoveHorizonTriggerMs type spin default {} min 0 max 600000",
        opts.move_horizon_trigger_ms
    ));
    usi_println(&format!(
        "option name MoveHorizonMinMoves type spin default {} min 0 max 200",
        opts.move_horizon_min_moves
    ));
    usi_println(&format!(
        "option name StopWaitMs type spin default {} min 0 max 2000",
        opts.stop_wait_ms
    ));
    usi_println(&format!(
        "option name GameoverSendsBestmove type check default {}",
        if opts.gameover_sends_bestmove {
            "true"
        } else {
            "false"
        }
    ));
    usi_println(&format!(
        "option name FailSafeGuard type check default {}",
        if opts.fail_safe_guard {
            "true"
        } else {
            "false"
        }
    ));
}
