use anyhow::Result;
use std::error::Error as StdError;

use engine_core::engine::controller::EngineType;
use engine_core::evaluation::nnue::error::NNUEError;
use engine_core::search::ab::SearchProfile;

use crate::io::{info_string, usi_println};
use crate::state::{EngineState, ProfileMode, UsiOptions};
fn mark_override(state: &mut EngineState, key: &str) {
    state.user_overrides.insert(key.to_string());
}

fn profile_for_engine_type(engine_type: EngineType) -> SearchProfile {
    match engine_type {
        EngineType::Material => SearchProfile::basic_material(),
        EngineType::Enhanced => SearchProfile::enhanced_material(),
        EngineType::Nnue => SearchProfile::basic_nnue(),
        EngineType::EnhancedNnue => SearchProfile::enhanced_nnue(),
    }
}

fn profile_allows_iid(state: &EngineState) -> bool {
    let profile = profile_for_engine_type(state.opts.engine_type);
    profile.prune.enable_iid
}

fn profile_allows_probcut(state: &EngineState) -> bool {
    let profile = profile_for_engine_type(state.opts.engine_type);
    profile.prune.enable_probcut
}

fn profile_allows_razor(state: &EngineState) -> bool {
    let profile = profile_for_engine_type(state.opts.engine_type);
    profile.prune.enable_razor
}

fn profile_allows_qs_checks(state: &EngineState) -> bool {
    let profile = profile_for_engine_type(state.opts.engine_type);
    profile.tuning.enable_qs_checks
}

fn profile_allows_nmp(state: &EngineState) -> bool {
    let profile = profile_for_engine_type(state.opts.engine_type);
    profile.prune.enable_nmp
}

fn profile_allows_static_beta(state: &EngineState) -> bool {
    let profile = profile_for_engine_type(state.opts.engine_type);
    profile.prune.enable_static_beta_pruning
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
    // Parallel policy knobs (LazySMP)
    usi_println("option name BenchAllRun type check default false");
    usi_println("option name BenchStopOnMate type check default true");
    usi_println("option name HelperAspiration type combo default Wide var Off var Wide");
    usi_println("option name HelperAspirationDelta type spin default 350 min 50 max 600");
    usi_println("option name Abdada type check default false");
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
    usi_println("option name SearchParams.SBP_Dyn_Enable type check default true");
    usi_println("option name SearchParams.SBP_Dyn_Base type spin default 120 min 0 max 2000");
    usi_println("option name SearchParams.SBP_Dyn_Slope type spin default 60 min 0 max 500");
    usi_println("option name SearchParams.FUT_Dyn_Enable type check default true");
    usi_println("option name SearchParams.FUT_Dyn_Base type spin default 100 min 0 max 2000");
    usi_println("option name SearchParams.FUT_Dyn_Slope type spin default 80 min 0 max 500");
    usi_println("option name SearchParams.ProbCut_D5 type spin default 250 min 0 max 2000");
    usi_println("option name SearchParams.ProbCut_D6P type spin default 300 min 0 max 2000");
    usi_println("option name SearchParams.IID_MinDepth type spin default 6 min 0 max 20");
    usi_println("option name SearchParams.Razor type check default true");
    usi_println("option name SearchParams.EnableNMP type check default true");
    usi_println("option name SearchParams.EnableIID type check default true");
    usi_println("option name SearchParams.EnableProbCut type check default true");
    usi_println("option name SearchParams.EnableStaticBeta type check default true");
    usi_println("option name SearchParams.ProbCut_SkipVerifyLt4 type check default false");
    // Finalize sanity (light guard before emitting bestmove)
    usi_println("option name FinalizeSanity.Enabled type check default true");
    usi_println(&format!(
        "option name FinalizeSanity.BudgetMs type spin default {} min 0 max 10",
        opts.finalize_sanity_budget_ms
    ));
    usi_println("option name FinalizeSanity.MiniDepth type spin default 2 min 1 max 3");
    usi_println("option name FinalizeSanity.SEE_MinCp type spin default -90 min -1000 max 1000");
    usi_println("option name FinalizeSanity.SwitchMarginCp type spin default 30 min 0 max 500");
    usi_println("option name SearchParams.SafePruning type check default true");
    usi_println("option name SearchParams.QS_MarginCapture type spin default 150 min 0 max 5000");
    usi_println("option name SearchParams.QS_BadCaptureMin type spin default 450 min 0 max 5000");
    usi_println(
        "option name SearchParams.QS_CheckPruneMargin type spin default 150 min 0 max 5000",
    );
    usi_println("option name SearchParams.HP_DepthScale type spin default 4361 min 0 max 20000");
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
    // Instant mate move options
    usi_println("option name InstantMateMove.Enabled type check default true");
    usi_println("option name InstantMateMove.MaxDistance type spin default 1 min 1 max 5");
    // Opponent SEE gate for finalize sanity
    usi_println(&format!(
        "option name FinalizeSanity.OppSEE_MinCp type spin default {} min 0 max 5000",
        opts.finalize_sanity_opp_see_min_cp
    ));
    // Independent penalty cap for opponent capture SEE in finalize sanity
    usi_println(
        "option name FinalizeSanity.OppSEE_PenaltyCapCp type spin default 200 min 0 max 5000",
    );
    // Symmetric check-move penalty for finalize sanity
    usi_println("option name FinalizeSanity.CheckPenaltyCp type spin default 15 min 0 max 100");

    // --- Root guard rails (flags; default OFF). Only printed; logic is flag-gated elsewhere.
    usi_println(&format!(
        "option name RootSeeGate type check default {}",
        if opts.root_see_gate { "true" } else { "false" }
    ));
    usi_println(&format!(
        "option name RootSeeGate.XSEE type spin default {} min -2000 max 5000",
        opts.x_see_cp
    ));
    usi_println(&format!(
        "option name PostVerify type check default {}",
        if opts.post_verify { "true" } else { "false" }
    ));
    usi_println(&format!(
        "option name PostVerify.YDrop type spin default {} min 0 max 5000",
        opts.y_drop_cp
    ));
    usi_println(&format!(
        "option name PromoteVerify type check default {}",
        if opts.promote_verify { "true" } else { "false" }
    ));
    usi_println(&format!(
        "option name PromoteVerify.BiasCp type spin default {} min -1000 max 1000",
        opts.promote_bias_cp
    ));
    // Reproduction helpers
    usi_println(&format!(
        "option name Warmup.Ms type spin default {} min 0 max 60000",
        opts.warmup_ms
    ));
    usi_println(&format!(
        "option name Warmup.PrevMoves type spin default {} min 0 max 20",
        opts.warmup_prev_moves
    ));
    // Profile controls (GUIで自動既定を明示適用)
    // Profile.Mode default is now taken from opts to avoid drift
    let profile_mode_default = match opts.profile_mode {
        crate::state::ProfileMode::Auto => "Auto",
        crate::state::ProfileMode::T1 => "T1",
        crate::state::ProfileMode::T8 => "T8",
        crate::state::ProfileMode::Off => "Off",
    };
    usi_println(&format!(
        "option name Profile.Mode type combo default {} var Auto var T1 var T8 var Off",
        profile_mode_default
    ));
    usi_println("option name Profile.ApplyAutoDefaults type button");
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
            mark_override(state, "Threads");
        }
        "USI_Ponder" => {
            if let Some(v) = value_ref {
                let v = v.to_lowercase();
                state.opts.ponder = matches!(v.as_str(), "true" | "1" | "on");
            }
        }
        "BenchAllRun" => {
            if let Some(v) = value_ref {
                let on = matches!(v.to_lowercase().as_str(), "true" | "1" | "on");
                if on {
                    std::env::set_var("SHOGI_PAR_BENCH_ALLRUN", "1");
                } else {
                    std::env::remove_var("SHOGI_PAR_BENCH_ALLRUN");
                }
                engine_core::search::policy::set_bench_allrun(on);
                info_string(format!("bench_allrun={}", if on { 1 } else { 0 }));
            }
        }
        "BenchStopOnMate" => {
            if let Some(v) = value_ref {
                let on = !matches!(v.to_lowercase().as_str(), "false" | "0" | "off");
                if on {
                    std::env::remove_var("SHOGI_BENCH_STOP_ON_MATE"); // default on
                } else {
                    std::env::set_var("SHOGI_BENCH_STOP_ON_MATE", "0");
                }
                engine_core::search::policy::set_bench_stop_on_mate(on);
                info_string(format!("bench_stop_on_mate={}", if on { 1 } else { 0 }));
            }
        }
        "HelperAspiration" => {
            if let Some(v) = value_ref {
                let mode = v.to_ascii_lowercase();
                match mode.as_str() {
                    "off" => {
                        std::env::set_var("SHOGI_HELPER_ASP_MODE", "off");
                        // 一括 setter（delta -> mode の順）で整合性を担保
                        let delta = engine_core::search::policy::helper_asp_delta_value();
                        engine_core::search::policy::set_helper_asp(0, delta);
                        info_string("helper_asp_mode=off");
                    }
                    _ => {
                        std::env::set_var("SHOGI_HELPER_ASP_MODE", "wide");
                        let delta = engine_core::search::policy::helper_asp_delta_value();
                        engine_core::search::policy::set_helper_asp(1, delta);
                        info_string("helper_asp_mode=wide");
                    }
                }
            }
        }
        "HelperAspirationDelta" => {
            if let Some(v) = value_ref {
                if let Ok(delta) = v.parse::<i32>() {
                    let clamped = delta.clamp(50, 600);
                    std::env::set_var("SHOGI_HELPER_ASP_DELTA", clamped.to_string());
                    // 一括 setter（delta -> mode の順）で整合性を担保
                    let mode = engine_core::search::policy::helper_asp_mode_value();
                    engine_core::search::policy::set_helper_asp(mode, clamped);
                    info_string(format!("helper_asp_delta={}", clamped));
                }
            }
        }
        "Abdada" => {
            if let Some(v) = value_ref {
                let on = matches!(v.to_lowercase().as_str(), "true" | "1" | "on");
                if on {
                    std::env::set_var("SHOGI_ABDADA", "1");
                } else {
                    std::env::remove_var("SHOGI_ABDADA");
                }
                engine_core::search::policy::set_abdada(on);
                info_string(format!("abdada={}", if on { 1 } else { 0 }));
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
            mark_override(state, "MultiPV");
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
        "SearchParams.SBP_Dyn_Enable" => {
            if let Some(v) = value_ref {
                let on = matches!(v.to_lowercase().as_str(), "on" | "true" | "1");
                engine_core::search::params::set_sbp_dynamic_enabled(on);
            }
        }
        "SearchParams.SBP_Dyn_Base" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    engine_core::search::params::set_sbp_base(x);
                }
            }
        }
        "SearchParams.SBP_Dyn_Slope" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    engine_core::search::params::set_sbp_slope(x);
                }
            }
        }
        "SearchParams.FUT_Dyn_Enable" => {
            if let Some(v) = value_ref {
                let on = matches!(v.to_lowercase().as_str(), "on" | "true" | "1");
                engine_core::search::params::set_fut_dynamic_enabled(on);
            }
        }
        "SearchParams.FUT_Dyn_Base" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    engine_core::search::params::set_fut_base(x);
                }
            }
        }
        "SearchParams.FUT_Dyn_Slope" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    engine_core::search::params::set_fut_slope(x);
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
                    info_string("pruning_note=Razor is disabled by the active SearchProfile");
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
                if on && !profile_allows_static_beta(state) {
                    info_string("pruning_note=StaticBeta is disabled by the active SearchProfile");
                }
                engine_core::search::params::set_static_beta_enabled(on);
            }
        }
        "SearchParams.ProbCut_SkipVerifyLt4" => {
            if let Some(v) = value_ref {
                let on = matches!(v.to_lowercase().as_str(), "on" | "true" | "1");
                engine_core::search::params::set_probcut_skip_verify_lt4(on);
                info_string(if on {
                    "pc_skip_verify_lt4=On"
                } else {
                    "pc_skip_verify_lt4=Off"
                });
            }
        }
        // --- Finalize sanity options ---
        "FinalizeSanity.Enabled" => {
            if let Some(v) = value_ref {
                let on = matches!(v.to_lowercase().as_str(), "on" | "true" | "1");
                state.opts.finalize_sanity_enabled = on;
                info_string(if on {
                    "finalize_sanity=On"
                } else {
                    "finalize_sanity=Off"
                });
            }
        }
        "FinalizeSanity.BudgetMs" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<u64>() {
                    state.opts.finalize_sanity_budget_ms = x.min(10);
                }
            }
            mark_override(state, "FinalizeSanity.BudgetMs");
        }
        "FinalizeSanity.MiniDepth" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<u8>() {
                    state.opts.finalize_sanity_mini_depth = x.clamp(1, 3);
                }
            }
        }
        "FinalizeSanity.SEE_MinCp" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    state.opts.finalize_sanity_see_min_cp = x.clamp(-2000, 2000);
                }
            }
        }
        "FinalizeSanity.SwitchMarginCp" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    state.opts.finalize_sanity_switch_margin_cp = x.clamp(0, 1000);
                }
            }
            mark_override(state, "FinalizeSanity.SwitchMarginCp");
        }
        "FinalizeSanity.OppSEE_MinCp" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    state.opts.finalize_sanity_opp_see_min_cp = x.clamp(0, 5000);
                }
            }
            mark_override(state, "FinalizeSanity.OppSEE_MinCp");
        }
        "FinalizeSanity.OppSEE_PenaltyCapCp" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    state.opts.finalize_sanity_opp_see_penalty_cap_cp = x.clamp(0, 5000);
                }
            }
        }
        "FinalizeSanity.CheckPenaltyCp" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    state.opts.finalize_sanity_check_penalty_cp = x.clamp(0, 100);
                }
            }
        }
        "SearchParams.SafePruning" => {
            if let Some(v) = value_ref {
                let on = matches!(v.to_lowercase().as_str(), "on" | "true" | "1");
                engine_core::search::params::set_pruning_safe_mode(on);
                info_string(if on {
                    "pruning_safe_mode=On"
                } else {
                    "pruning_safe_mode=Off"
                });
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
        "SearchParams.HP_DepthScale" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    engine_core::search::params::set_hp_depth_scale(x);
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
        "InstantMateMove.Enabled" => {
            if let Some(v) = value_ref {
                let v = v.to_lowercase();
                state.opts.instant_mate_move_enabled = matches!(v.as_str(), "true" | "1" | "on");
                info_string(if state.opts.instant_mate_move_enabled {
                    "instant_mate_move=On"
                } else {
                    "instant_mate_move=Off"
                });
            }
        }
        "InstantMateMove.MaxDistance" => {
            if let Some(v) = value_ref {
                if let Ok(d) = v.parse::<u32>() {
                    state.opts.instant_mate_move_max_distance = d.clamp(1, 5);
                }
            }
        }
        "InstantMateMove.CheckAllPV" => {
            if let Some(v) = value_ref {
                let v = v.to_lowercase();
                state.opts.instant_mate_check_all_pv = matches!(v.as_str(), "true" | "1" | "on");
            }
        }
        "InstantMateMove.RequiredSnapshot" => {
            if let Some(v) = value_ref {
                let v = v.to_ascii_lowercase();
                // Stable / Any
                state.opts.instant_mate_require_stable = !matches!(v.as_str(), "any");
            }
        }
        "InstantMateMove.MinDepth" => {
            if let Some(v) = value_ref {
                if let Ok(d) = v.parse::<u32>() {
                    state.opts.instant_mate_min_depth = d.min(64) as u8;
                }
            }
        }
        "InstantMateMove.VerifyMode" => {
            if let Some(v) = value_ref {
                use crate::state::InstantMateVerifyMode as M;
                let m = match v.to_ascii_lowercase().as_str() {
                    "off" => M::Off,
                    "qsearch" => M::QSearch,
                    _ => M::CheckOnly,
                };
                state.opts.instant_mate_verify_mode = m;
            }
        }
        "InstantMateMove.VerifyNodes" => {
            if let Some(v) = value_ref {
                if let Ok(n) = v.parse::<u32>() {
                    state.opts.instant_mate_verify_nodes = n;
                }
            }
        }
        "InstantMateMove.RespectMinThinkMs" => {
            if let Some(v) = value_ref {
                let v = v.to_lowercase();
                state.opts.instant_mate_respect_min_think_ms =
                    matches!(v.as_str(), "true" | "1" | "on");
            }
        }
        "InstantMateMove.MinRespectMs" => {
            if let Some(v) = value_ref {
                if let Ok(ms) = v.parse::<u64>() {
                    state.opts.instant_mate_min_respect_ms = ms.min(1000);
                }
            }
        }
        // --- Root guard rails & warmup knobs
        "RootSeeGate" => {
            if let Some(v) = value_ref {
                let on = matches!(v.to_lowercase().as_str(), "true" | "1" | "on");
                state.opts.root_see_gate = on;
                info_string(format!("root_see_gate={}", on as u8));
            }
            mark_override(state, "RootSeeGate");
        }
        "RootSeeGate.XSEE" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    state.opts.x_see_cp = x.clamp(-2000, 5000);
                }
            }
            mark_override(state, "RootSeeGate.XSEE");
        }
        "PostVerify" => {
            if let Some(v) = value_ref {
                let on = matches!(v.to_lowercase().as_str(), "true" | "1" | "on");
                state.opts.post_verify = on;
                info_string(format!("post_verify={}", on as u8));
            }
            mark_override(state, "PostVerify");
        }
        "PostVerify.YDrop" => {
            if let Some(v) = value_ref {
                if let Ok(y) = v.parse::<i32>() {
                    state.opts.y_drop_cp = y.clamp(0, 5000);
                }
            }
            mark_override(state, "PostVerify.YDrop");
        }
        "PromoteVerify" => {
            if let Some(v) = value_ref {
                let on = matches!(v.to_lowercase().as_str(), "true" | "1" | "on");
                state.opts.promote_verify = on;
                info_string(format!("promote_verify={}", on as u8));
            }
        }
        "PromoteVerify.BiasCp" => {
            if let Some(v) = value_ref {
                if let Ok(b) = v.parse::<i32>() {
                    state.opts.promote_bias_cp = b.clamp(-1000, 1000);
                }
            }
        }
        "Warmup.Ms" => {
            if let Some(v) = value_ref {
                if let Ok(ms) = v.parse::<u64>() {
                    state.opts.warmup_ms = ms.min(60000);
                }
            }
        }
        "Warmup.PrevMoves" => {
            if let Some(v) = value_ref {
                if let Ok(k) = v.parse::<u32>() {
                    state.opts.warmup_prev_moves = k.min(20);
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
        "ByoyomiPeriods" | "USI_ByoyomiPeriods" => {
            if let Some(v) = value_ref {
                let v_l = v.to_ascii_lowercase();
                if v_l == "default" {
                    state.opts.byoyomi_periods = UsiOptions::default().byoyomi_periods;
                } else if let Ok(p) = v.parse::<u32>() {
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
        // --- Profile controls (GUI)
        "Profile.Mode" => {
            if let Some(v) = value_ref {
                match v.to_ascii_lowercase().as_str() {
                    "auto" => state.opts.profile_mode = ProfileMode::Auto,
                    "t1" => state.opts.profile_mode = ProfileMode::T1,
                    "t8" => state.opts.profile_mode = ProfileMode::T8,
                    "off" => state.opts.profile_mode = ProfileMode::Off,
                    _ => {}
                }
                info_string(match state.opts.profile_mode {
                    ProfileMode::Auto => "profile_mode=Auto".to_string(),
                    ProfileMode::T1 => "profile_mode=T1".to_string(),
                    ProfileMode::T8 => "profile_mode=T8".to_string(),
                    ProfileMode::Off => "profile_mode=Off".to_string(),
                });
            }
        }
        "Profile.ApplyAutoDefaults" => {
            if state.searching {
                info_string("profile_applied=0 reason=busy");
            } else {
                let keys = [
                    "RootSeeGate",
                    "RootSeeGate.XSEE",
                    "PostVerify",
                    "PostVerify.YDrop",
                    "FinalizeSanity.SwitchMarginCp",
                    "FinalizeSanity.OppSEE_MinCp",
                    "FinalizeSanity.BudgetMs",
                    "MultiPV",
                ];
                let mut cleared = 0usize;
                for k in keys {
                    if state.user_overrides.remove(k) {
                        cleared += 1;
                    }
                }
                // Apply and reflect
                maybe_apply_thread_based_defaults(state);
                apply_options_to_engine(state);
                let mode_str = match state.opts.profile_mode {
                    ProfileMode::Auto => "Auto",
                    ProfileMode::T1 => "T1",
                    ProfileMode::T8 => "T8",
                    ProfileMode::Off => "Off",
                };
                let resolved = match state.opts.profile_mode {
                    ProfileMode::T1 => "T1",
                    ProfileMode::T8 => "T8",
                    ProfileMode::Auto => {
                        if state.opts.threads >= 4 {
                            "T8"
                        } else {
                            "T1"
                        }
                    }
                    ProfileMode::Off => "-",
                };
                info_string(format!(
                    "profile_applied=1 mode={} resolved={} cleared_overrides={}",
                    mode_str, resolved, cleared
                ));
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
    // Align engine-core early mate stop distance with USI option (InstantMateMove.MaxDistance)
    engine_core::search::config::set_mate_early_stop_max_distance(
        state.opts.instant_mate_move_max_distance as u8,
    );
    // Root guard rails & verify parameters (global config)
    engine_core::search::config::set_root_see_gate_enabled(state.opts.root_see_gate);
    engine_core::search::config::set_root_see_x_cp(state.opts.x_see_cp);
    engine_core::search::config::set_post_verify_enabled(state.opts.post_verify);
    engine_core::search::config::set_post_verify_ydrop_cp(state.opts.y_drop_cp);
    engine_core::search::config::set_promote_verify_enabled(state.opts.promote_verify);
    engine_core::search::config::set_promote_bias_cp(state.opts.promote_bias_cp);
}

/// Apply Threads-dependent auto-defaults unless the user explicitly overrode the values.
/// This function does NOT print options; it only updates `state.opts`.
pub fn maybe_apply_thread_based_defaults(state: &mut EngineState) {
    let is_t8 = match state.opts.profile_mode {
        ProfileMode::T1 => false,
        ProfileMode::T8 => true,
        ProfileMode::Off => return, // 明示OFF
        ProfileMode::Auto => state.opts.threads >= 4,
    };
    // Helper: set if not overridden by user
    let set_if_absent = |key: &str, apply: &mut dyn FnMut()| {
        if !state.user_overrides.contains(key) {
            apply();
        }
    };
    if is_t8 {
        set_if_absent("RootSeeGate", &mut || state.opts.root_see_gate = true);
        set_if_absent("RootSeeGate.XSEE", &mut || state.opts.x_see_cp = 100);
        set_if_absent("PostVerify", &mut || state.opts.post_verify = true);
        set_if_absent("PostVerify.YDrop", &mut || state.opts.y_drop_cp = 250);
        set_if_absent("FinalizeSanity.SwitchMarginCp", &mut || {
            state.opts.finalize_sanity_switch_margin_cp = 30
        });
        set_if_absent("FinalizeSanity.OppSEE_MinCp", &mut || {
            state.opts.finalize_sanity_opp_see_min_cp = 100
        });
        set_if_absent("FinalizeSanity.BudgetMs", &mut || state.opts.finalize_sanity_budget_ms = 8);
        set_if_absent("MultiPV", &mut || state.opts.multipv = 1);
    } else {
        // T1 profile
        set_if_absent("RootSeeGate", &mut || state.opts.root_see_gate = true);
        set_if_absent("RootSeeGate.XSEE", &mut || state.opts.x_see_cp = 100);
        set_if_absent("PostVerify", &mut || state.opts.post_verify = true);
        set_if_absent("PostVerify.YDrop", &mut || state.opts.y_drop_cp = 225);
        set_if_absent("FinalizeSanity.SwitchMarginCp", &mut || {
            state.opts.finalize_sanity_switch_margin_cp = 35
        });
        set_if_absent("FinalizeSanity.OppSEE_MinCp", &mut || {
            state.opts.finalize_sanity_opp_see_min_cp = 120
        });
        set_if_absent("FinalizeSanity.BudgetMs", &mut || state.opts.finalize_sanity_budget_ms = 4);
        set_if_absent("MultiPV", &mut || state.opts.multipv = 1);
    }
}

/// Emit a one-line info string with the effective profile and key parameters.
pub fn log_effective_profile(state: &EngineState) {
    let mode_str = match state.opts.profile_mode {
        ProfileMode::Auto => "Auto",
        ProfileMode::T1 => "T1",
        ProfileMode::T8 => "T8",
        ProfileMode::Off => "Off",
    };
    let resolved = match state.opts.profile_mode {
        ProfileMode::T1 => Some("T1"),
        ProfileMode::T8 => Some("T8"),
        ProfileMode::Auto => Some(if state.opts.threads >= 4 { "T8" } else { "T1" }),
        ProfileMode::Off => None,
    };
    let mut overrides: Vec<&str> = Vec::new();
    for k in [
        "RootSeeGate",
        "RootSeeGate.XSEE",
        "PostVerify",
        "PostVerify.YDrop",
        "FinalizeSanity.SwitchMarginCp",
        "FinalizeSanity.OppSEE_MinCp",
        "FinalizeSanity.BudgetMs",
        "MultiPV",
    ] {
        if state.user_overrides.contains(k) {
            overrides.push(k);
        }
    }
    info_string(format!(
        "effective_profile mode={} resolved={} threads={} multipv={} root_see_gate={} xsee={} post_verify={} ydrop={} finalize_enabled={} finalize_switch={} finalize_oppsee={} finalize_budget={} overrides={} threads_overridden={}",
        mode_str,
        resolved.unwrap_or("-"),
        state.opts.threads,
        state.opts.multipv,
        state.opts.root_see_gate as u8,
        state.opts.x_see_cp,
        state.opts.post_verify as u8,
        state.opts.y_drop_cp,
        state.opts.finalize_sanity_enabled as u8,
        state.opts.finalize_sanity_switch_margin_cp,
        state.opts.finalize_sanity_opp_see_min_cp,
        state.opts.finalize_sanity_budget_ms,
        if overrides.is_empty() { "-".to_string() } else { overrides.join(",") },
        if state.user_overrides.contains("Threads") { 1 } else { 0 }
    ));
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
    // Alias for GUI compatibility
    usi_println(&format!(
        "option name USI_ByoyomiPeriods type spin default {} min 1 max 10",
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
    // Instant-mate detection policy (USI-led)
    usi_println(&format!(
        "option name InstantMateMove.CheckAllPV type check default {}",
        if opts.instant_mate_check_all_pv {
            "true"
        } else {
            "false"
        }
    ));
    // Snapshot gating
    usi_println(&format!(
        "option name InstantMateMove.RequiredSnapshot type combo default {} var Stable var Any",
        if opts.instant_mate_require_stable {
            "Stable"
        } else {
            "Any"
        }
    ));
    usi_println(&format!(
        "option name InstantMateMove.MinDepth type spin default {} min 0 max 64",
        opts.instant_mate_min_depth
    ));
    // Verification
    let verify_mode = match opts.instant_mate_verify_mode {
        crate::state::InstantMateVerifyMode::Off => "Off",
        crate::state::InstantMateVerifyMode::CheckOnly => "CheckOnly",
        crate::state::InstantMateVerifyMode::QSearch => "QSearch",
    };
    usi_println(&format!(
        "option name InstantMateMove.VerifyMode type combo default {} var Off var CheckOnly var QSearch",
        verify_mode
    ));
    usi_println(&format!(
        "option name InstantMateMove.VerifyNodes type spin default {} min 0 max 100000",
        opts.instant_mate_verify_nodes
    ));
    usi_println(&format!(
        "option name InstantMateMove.RespectMinThinkMs type check default {}",
        if opts.instant_mate_respect_min_think_ms {
            "true"
        } else {
            "false"
        }
    ));
    usi_println(&format!(
        "option name InstantMateMove.MinRespectMs type spin default {} min 0 max 1000",
        opts.instant_mate_min_respect_ms
    ));
}
