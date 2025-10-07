use anyhow::Result;
use std::error::Error as StdError;

use engine_core::engine::controller::EngineType;
use engine_core::evaluation::nnue::error::NNUEError;
use engine_core::search::ab::SearchProfile;

use crate::io::{info_string, usi_println};
use crate::state::{EngineState, UsiOptions};

fn profile_for_engine_type(engine_type: EngineType) -> SearchProfile {
    match engine_type {
        EngineType::Material => SearchProfile::basic_material(),
        EngineType::Enhanced => SearchProfile::enhanced_material(),
        EngineType::Nnue => SearchProfile::basic_nnue(),
        EngineType::EnhancedNnue => SearchProfile::enhanced_nnue(),
    }
}

fn current_profile(state: &EngineState) -> Option<SearchProfile> {
    state
        .engine
        .lock()
        .ok()
        .map(|eng| profile_for_engine_type(eng.get_engine_type()))
}

fn profile_allows_iid(state: &EngineState) -> bool {
    use EngineType::Enhanced;
    if matches!(state.opts.engine_type, Enhanced) {
        false
    } else {
        current_profile(state).map(|p| p.prune.enable_iid).unwrap_or(true)
    }
}

fn profile_allows_probcut(state: &EngineState) -> bool {
    use EngineType::{Enhanced, Material};
    if matches!(state.opts.engine_type, Material | Enhanced) {
        false
    } else {
        current_profile(state).map(|p| p.prune.enable_probcut).unwrap_or(true)
    }
}

fn profile_allows_razor(state: &EngineState) -> bool {
    current_profile(state).map(|p| p.prune.enable_razor).unwrap_or(true)
}

fn profile_allows_qs_checks(state: &EngineState) -> bool {
    current_profile(state).map(|p| p.tuning.enable_qs_checks).unwrap_or(true)
}

fn profile_allows_nmp(state: &EngineState) -> bool {
    current_profile(state).map(|p| p.prune.enable_nmp).unwrap_or(true)
}

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
    // Diagnostics / policy knobs
    usi_println("option name QSearchChecks type combo default On var On var Off");
    // Search parameter knobs (runtime-adjustable)
    usi_println("option name SearchParams.LMR_K_x100 type spin default 170 min 80 max 400");
    usi_println("option name SearchParams.LMP_D1 type spin default 6 min 0 max 64");
    usi_println("option name SearchParams.LMP_D2 type spin default 12 min 0 max 64");
    usi_println("option name SearchParams.LMP_D3 type spin default 18 min 0 max 64");
    usi_println(
        "option name SearchParams.HP_Threshold type spin default -2000 min -10000 max 10000",
    );
    usi_println("option name SearchParams.SBP_D1 type spin default 200 min 0 max 2000");
    usi_println("option name SearchParams.SBP_D2 type spin default 300 min 0 max 2000");
    usi_println("option name SearchParams.ProbCut_D5 type spin default 250 min 0 max 2000");
    usi_println("option name SearchParams.ProbCut_D6P type spin default 300 min 0 max 2000");
    usi_println("option name SearchParams.IID_MinDepth type spin default 6 min 0 max 20");
    usi_println("option name SearchParams.Razor type check default true");
    usi_println("option name SearchParams.EnableNMP type check default true");
    usi_println("option name SearchParams.EnableIID type check default true");
    usi_println("option name SearchParams.EnableProbCut type check default true");
    usi_println("option name SearchParams.EnableStaticBeta type check default true");
    usi_println("option name SearchParams.QS_MarginCapture type spin default 150 min 0 max 5000");
    usi_println("option name SearchParams.QS_BadCaptureMin type spin default 450 min 0 max 5000");
    usi_println(
        "option name SearchParams.QS_CheckPruneMargin type spin default 150 min 0 max 5000",
    );
    usi_println("option name SearchParams.QuietHistoryWeight type spin default 4 min -64 max 64");
    usi_println(
        "option name SearchParams.ContinuationHistoryWeight type spin default 2 min -32 max 32",
    );
    usi_println("option name SearchParams.CaptureHistoryWeight type spin default 2 min -32 max 32");
    usi_println("option name SearchParams.RootTTBonus type spin default 1500000 min 0 max 5000000");
    usi_println("option name SearchParams.RootPrevScoreScale type spin default 200 min 0 max 2000");
    usi_println(
        "option name SearchParams.RootMultiPV1 type spin default 50000 min -200000 max 200000",
    );
    usi_println(
        "option name SearchParams.RootMultiPV2 type spin default 25000 min -200000 max 200000",
    );
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
                    if t == 1 {
                        info_string("threads_note=Single-threaded search mode");
                    } else {
                        info_string(format!(
                            "threads_note=LazySMP parallel search with {} threads",
                            t
                        ));
                    }
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
        "QSearchChecks" => {
            if let Some(v) = value_ref {
                let on = matches!(v.to_lowercase().as_str(), "on" | "true" | "1");
                if on && !profile_allows_qs_checks(state) {
                    info_string(
                        "qsearch_note=Profile defaults suppress quiet-check extensions; runtime On may still be limited",
                    );
                }
                engine_core::search::params::set_qs_checks_enabled(on);
                info_string(if on {
                    "qsearch_checks=On"
                } else {
                    "qsearch_checks=Off"
                });
            }
        }
        "ClearHash" => {
            if let Ok(mut eng) = state.engine.lock() {
                eng.set_multipv_persistent(state.opts.multipv);
                eng.clear_hash();
            }
        }
        // --- Search parameters (runtime) ---
        "SearchParams.LMR_K_x100" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<u32>() {
                    engine_core::search::params::set_lmr_k_x100(x);
                }
            }
        }
        "SearchParams.LMP_D1" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<usize>() {
                    engine_core::search::params::set_lmp_d1(x);
                }
            }
        }
        "SearchParams.LMP_D2" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<usize>() {
                    engine_core::search::params::set_lmp_d2(x);
                }
            }
        }
        "SearchParams.LMP_D3" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<usize>() {
                    engine_core::search::params::set_lmp_d3(x);
                }
            }
        }
        "SearchParams.HP_Threshold" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    engine_core::search::params::set_hp_threshold(x);
                }
            }
        }
        "SearchParams.SBP_D1" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    engine_core::search::params::set_sbp_d1(x);
                }
            }
        }
        "SearchParams.SBP_D2" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    engine_core::search::params::set_sbp_d2(x);
                }
            }
        }
        "SearchParams.ProbCut_D5" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    engine_core::search::params::set_probcut_d5(x);
                }
            }
        }
        "SearchParams.ProbCut_D6P" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    engine_core::search::params::set_probcut_d6p(x);
                }
            }
        }
        "SearchParams.IID_MinDepth" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    engine_core::search::params::set_iid_min_depth(x);
                }
            }
        }
        "SearchParams.Razor" => {
            if let Some(v) = value_ref {
                let on = matches!(v.to_lowercase().as_str(), "on" | "true" | "1");
                if on && !profile_allows_razor(state) {
                    info_string(
                        "pruning_note=Razor pruning is disabled by the active SearchProfile",
                    );
                }
                engine_core::search::params::set_razor_enabled(on);
            }
        }
        "SearchParams.EnableNMP" => {
            if let Some(v) = value_ref {
                let on = matches!(v.to_lowercase().as_str(), "on" | "true" | "1");
                if on && !profile_allows_nmp(state) {
                    info_string("pruning_note=NMP is disabled by the active SearchProfile");
                }
                engine_core::search::params::set_nmp_enabled(on);
            }
        }
        "SearchParams.EnableIID" => {
            if let Some(v) = value_ref {
                let on = matches!(v.to_lowercase().as_str(), "on" | "true" | "1");
                if on && !profile_allows_iid(state) {
                    info_string("pruning_note=IID is disabled by the active SearchProfile");
                }
                engine_core::search::params::set_iid_enabled(on);
            }
        }
        "SearchParams.EnableProbCut" => {
            if let Some(v) = value_ref {
                let on = matches!(v.to_lowercase().as_str(), "on" | "true" | "1");
                if on && !profile_allows_probcut(state) {
                    info_string("pruning_note=ProbCut is disabled by the active SearchProfile");
                }
                engine_core::search::params::set_probcut_enabled(on);
            }
        }
        "SearchParams.EnableStaticBeta" => {
            if let Some(v) = value_ref {
                let on = matches!(v.to_lowercase().as_str(), "on" | "true" | "1");
                engine_core::search::params::set_static_beta_enabled(on);
            }
        }
        "SearchParams.QS_MarginCapture" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    engine_core::search::params::set_qs_margin_capture(x);
                }
            }
        }
        "SearchParams.QS_BadCaptureMin" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    engine_core::search::params::set_qs_bad_capture_min(x);
                }
            }
        }
        "SearchParams.QS_CheckPruneMargin" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    engine_core::search::params::set_qs_check_prune_margin(x);
                }
            }
        }
        "SearchParams.QuietHistoryWeight" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    engine_core::search::params::set_quiet_history_weight(x);
                }
            }
        }
        "SearchParams.ContinuationHistoryWeight" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    engine_core::search::params::set_continuation_history_weight(x);
                }
            }
        }
        "SearchParams.CaptureHistoryWeight" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    engine_core::search::params::set_capture_history_weight(x);
                }
            }
        }
        "SearchParams.RootTTBonus" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    engine_core::search::params::set_root_tt_bonus(x);
                }
            }
        }
        "SearchParams.RootPrevScoreScale" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    engine_core::search::params::set_root_prev_score_scale(x);
                }
            }
        }
        "SearchParams.RootMultiPV1" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    engine_core::search::params::set_root_multipv_bonus(1, x);
                }
            }
        }
        "SearchParams.RootMultiPV2" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    engine_core::search::params::set_root_multipv_bonus(2, x);
                }
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
        "ByoyomiDeadlineLeadMs" => {
            if let Some(v) = value_ref {
                if let Ok(ms) = v.parse::<u64>() {
                    state.opts.byoyomi_deadline_lead_ms = ms.min(2000);
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
        "WatchdogPollMs" => {
            if let Some(v) = value_ref {
                if let Ok(ms) = v.parse::<u64>() {
                    state.opts.watchdog_poll_ms = ms.clamp(1, 20);
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
        "option name ByoyomiDeadlineLeadMs type spin default {} min 0 max 2000",
        opts.byoyomi_deadline_lead_ms
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
        "option name WatchdogPollMs type spin default {} min 1 max 20",
        opts.watchdog_poll_ms
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
