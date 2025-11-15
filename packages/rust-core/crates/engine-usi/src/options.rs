use anyhow::Result;
use std::error::Error as StdError;

use engine_core::engine::controller::EngineType;
use engine_core::evaluation::nnue::error::NNUEError;
use engine_core::search::ab::SearchProfile;

use crate::io::{info_string, usi_println};
use crate::state::{EngineState, LogProfile, ProfileMode, UsiOptions};
use std::sync::OnceLock;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum OptionSection {
    Core,
    Time,
    Search,
    Finalize,
    Diagnostics,
    Utility,
}

struct PrintableOption {
    section: OptionSection,
    sort_key: String,
    text: String,
}

impl PrintableOption {
    fn new(section: OptionSection, name: &str, text: String) -> Self {
        Self {
            section,
            sort_key: name.to_ascii_lowercase(),
            text,
        }
    }
}

#[derive(Default)]
struct OptionBuilder {
    entries: Vec<PrintableOption>,
}

impl OptionBuilder {
    fn push(&mut self, section: OptionSection, name: &'static str, text: String) {
        self.entries.push(PrintableOption::new(section, name, text));
    }

    fn core(&mut self, name: &'static str, text: String) {
        self.push(OptionSection::Core, name, text);
    }

    fn time(&mut self, name: &'static str, text: String) {
        self.push(OptionSection::Time, name, text);
    }

    fn search(&mut self, name: &'static str, text: String) {
        self.push(OptionSection::Search, name, text);
    }

    fn finalize(&mut self, name: &'static str, text: String) {
        self.push(OptionSection::Finalize, name, text);
    }

    fn diagnostics(&mut self, name: &'static str, text: String) {
        self.push(OptionSection::Diagnostics, name, text);
    }

    fn utility(&mut self, name: &'static str, text: String) {
        self.push(OptionSection::Utility, name, text);
    }

    fn finish(mut self) -> Vec<String> {
        self.entries
            .sort_by(|a, b| a.section.cmp(&b.section).then_with(|| a.sort_key.cmp(&b.sort_key)));
        self.entries.into_iter().map(|entry| entry.text).collect()
    }
}

fn bool_to_usi(value: bool) -> &'static str {
    if value {
        "true"
    } else {
        "false"
    }
}

pub fn send_id_and_options(opts: &UsiOptions) {
    usi_println("id name RustShogi USI (core)");
    usi_println("id author RustShogi Team");

    let mut builder = OptionBuilder::default();
    collect_core_options(opts, &mut builder);
    collect_time_options(opts, &mut builder);
    collect_search_options(opts, &mut builder);
    collect_finalize_options(opts, &mut builder);
    collect_diagnostics_options(opts, &mut builder);
    collect_utility_options(opts, &mut builder);

    for line in builder.finish() {
        usi_println(&line);
    }
}

fn collect_core_options(opts: &UsiOptions, builder: &mut OptionBuilder) {
    builder.core("ClearHash", "option name ClearHash type button".to_string());
    builder.core(
        "EngineType",
        "option name EngineType type combo default Enhanced var Material var Enhanced var Nnue var EnhancedNnue"
            .to_string(),
    );
    builder.core("EvalFile", "option name EvalFile type filename default ".to_string());
    builder.core(
        "LogProfile",
        format!(
            "option name LogProfile type combo default {} var Prod var QA var Dev",
            opts.log_profile.as_str()
        ),
    );
    builder.core(
        "MultiPV",
        format!("option name MultiPV type spin default {} min 1 max 20", opts.multipv),
    );
    builder.core(
        "NNUE_Simd",
        "option name NNUE_Simd type combo default Auto var Auto var Scalar var SSE41 var AVX2"
            .to_string(),
    );
    builder.core(
        "SIMDMaxLevel",
        "option name SIMDMaxLevel type combo default Auto var Auto var Scalar var SSE2 var AVX var AVX512F"
            .to_string(),
    );
    builder.core(
        "Threads",
        format!("option name Threads type spin default {} min 1 max 256", opts.threads),
    );
    builder.core(
        "USI_Hash",
        format!("option name USI_Hash type spin default {} min 1 max 1024", opts.hash_mb),
    );
    builder.core(
        "USI_Ponder",
        format!("option name USI_Ponder type check default {}", bool_to_usi(opts.ponder)),
    );
}

fn collect_time_options(opts: &UsiOptions, builder: &mut OptionBuilder) {
    builder.time(
        "ByoyomiDeadlineLeadMs",
        format!(
            "option name ByoyomiDeadlineLeadMs type spin default {} min 0 max 2000",
            opts.byoyomi_deadline_lead_ms
        ),
    );
    builder.time(
        "ByoyomiEarlyFinishRatio",
        format!(
            "option name ByoyomiEarlyFinishRatio type spin default {} min 50 max 95",
            opts.byoyomi_early_finish_ratio
        ),
    );
    builder.time(
        "ByoyomiOverheadMs",
        format!(
            "option name ByoyomiOverheadMs type spin default {} min 0 max 5000",
            opts.network_delay2_ms
        ),
    );
    builder.time(
        "ByoyomiPeriods",
        format!(
            "option name ByoyomiPeriods type spin default {} min 1 max 10",
            opts.byoyomi_periods
        ),
    );
    builder.time(
        "ByoyomiSafetyMs",
        format!(
            "option name ByoyomiSafetyMs type spin default {} min 0 max 2000",
            opts.byoyomi_safety_ms
        ),
    );
    builder.time(
        "ForceTerminateOnHardDeadline",
        format!(
            "option name ForceTerminateOnHardDeadline type check default {}",
            bool_to_usi(opts.force_terminate_on_hard_deadline)
        ),
    );
    builder.time(
        "MaxTimeRatioPct",
        format!(
            "option name MaxTimeRatioPct type spin default {} min 100 max 800",
            opts.max_time_ratio_pct
        ),
    );
    builder.time(
        "MinThinkMs",
        format!("option name MinThinkMs type spin default {} min 0 max 10000", opts.min_think_ms),
    );
    builder.time(
        "MoveHorizonMinMoves",
        format!(
            "option name MoveHorizonMinMoves type spin default {} min 0 max 200",
            opts.move_horizon_min_moves
        ),
    );
    builder.time(
        "MoveHorizonTriggerMs",
        format!(
            "option name MoveHorizonTriggerMs type spin default {} min 0 max 600000",
            opts.move_horizon_trigger_ms
        ),
    );
    builder.time(
        "OverheadMs",
        format!("option name OverheadMs type spin default {} min 0 max 5000", opts.overhead_ms),
    );
    builder.time(
        "PVStabilityBase",
        format!(
            "option name PVStabilityBase type spin default {} min 10 max 200",
            opts.pv_stability_base
        ),
    );
    builder.time(
        "PVStabilitySlope",
        format!(
            "option name PVStabilitySlope type spin default {} min 0 max 20",
            opts.pv_stability_slope
        ),
    );
    builder.time(
        "SlowMover",
        format!("option name SlowMover type spin default {} min 50 max 200", opts.slow_mover_pct),
    );
    builder.time(
        "Stochastic_Ponder",
        format!(
            "option name Stochastic_Ponder type check default {}",
            bool_to_usi(opts.stochastic_ponder)
        ),
    );
    builder.time(
        "StopWaitMs",
        format!("option name StopWaitMs type spin default {} min 0 max 2000", opts.stop_wait_ms),
    );
    builder.time(
        "USI_ByoyomiPeriods",
        format!(
            "option name USI_ByoyomiPeriods type spin default {} min 1 max 10",
            opts.byoyomi_periods
        ),
    );
    builder.time(
        "WatchdogPollMs",
        format!(
            "option name WatchdogPollMs type spin default {} min 1 max 20",
            opts.watchdog_poll_ms
        ),
    );
}

fn collect_search_options(opts: &UsiOptions, builder: &mut OptionBuilder) {
    builder.search("Abdada", "option name Abdada type check default false".to_string());
    builder.search("BenchAllRun", "option name BenchAllRun type check default false".to_string());
    builder.search(
        "BenchStopOnMate",
        "option name BenchStopOnMate type check default true".to_string(),
    );
    builder.search(
        "HelperAspiration",
        "option name HelperAspiration type combo default Wide var Off var Wide".to_string(),
    );
    builder.search(
        "HelperAspirationDelta",
        "option name HelperAspirationDelta type spin default 350 min 50 max 600".to_string(),
    );
    builder.search(
        "QSearchChecks",
        "option name QSearchChecks type combo default On var On var Off".to_string(),
    );
    // Material evaluator (lightweight) evaluation knobs
    builder.search(
        "Eval.Material.TempoCp",
        "option name Eval.Material.TempoCp type spin default 10 min -200 max 200".to_string(),
    );
    builder.search(
        "Eval.Material.RookMobilityCp",
        "option name Eval.Material.RookMobilityCp type spin default 2 min 0 max 50".to_string(),
    );
    builder.search(
        "Eval.Material.RookTrappedPenalty",
        "option name Eval.Material.RookTrappedPenalty type spin default 30 min 0 max 500"
            .to_string(),
    );
    builder.search(
        "Eval.Material.KingEarlyMovePenaltyCp",
        "option name Eval.Material.KingEarlyMovePenaltyCp type spin default 12 min 0 max 200"
            .to_string(),
    );
    builder.search(
        "Eval.Material.KingEarlyMoveMaxPly",
        "option name Eval.Material.KingEarlyMoveMaxPly type spin default 20 min 0 max 100"
            .to_string(),
    );
    builder.search(
        "RootSeeGate",
        format!("option name RootSeeGate type check default {}", bool_to_usi(opts.root_see_gate)),
    );
    builder.search(
        "RootSeeGate.XSEE",
        format!(
            "option name RootSeeGate.XSEE type spin default {} min 0 max 5000",
            opts.root_see_gate_xsee_cp
        ),
    );
    builder.search(
        "Search.CaptureFutility",
        "option name Search.CaptureFutility type check default true".to_string(),
    );
    builder.search(
        "Search.CaptureFutility.ScalePct",
        "option name Search.CaptureFutility.ScalePct type spin default 75 min 25 max 150"
            .to_string(),
    );
    builder.search(
        "Search.NMP.Verify",
        "option name Search.NMP.Verify type check default true".to_string(),
    );
    builder.search(
        "Search.NMP.VerifyMinDepth",
        "option name Search.NMP.VerifyMinDepth type spin default 16 min 2 max 64".to_string(),
    );
    builder.search(
        "Search.QuietSeeGuard",
        "option name Search.QuietSeeGuard type check default true".to_string(),
    );
    builder.search(
        "Search.ShallowGate",
        "option name Search.ShallowGate type check default false".to_string(),
    );
    builder.search(
        "Search.ShallowGate.Depth",
        "option name Search.ShallowGate.Depth type spin default 3 min 1 max 8".to_string(),
    );
    builder.search(
        "Search.Singular.Enabled",
        "option name Search.Singular.Enabled type check default false".to_string(),
    );
    builder.search(
        "Search.Singular.MarginBase",
        "option name Search.Singular.MarginBase type spin default 56 min 0 max 512".to_string(),
    );
    builder.search(
        "Search.Singular.MarginScalePct",
        "option name Search.Singular.MarginScalePct type spin default 70 min 10 max 300"
            .to_string(),
    );
    builder.search(
        "Search.Singular.MinDepth",
        "option name Search.Singular.MinDepth type spin default 6 min 2 max 64".to_string(),
    );
    builder.search(
        "SearchParams.CaptureHistoryWeight",
        "option name SearchParams.CaptureHistoryWeight type spin default 2 min -32 max 32"
            .to_string(),
    );
    builder.search(
        "SearchParams.ContinuationHistoryWeight",
        "option name SearchParams.ContinuationHistoryWeight type spin default 2 min -32 max 32"
            .to_string(),
    );
    builder.search(
        "SearchParams.EnableIID",
        "option name SearchParams.EnableIID type check default true".to_string(),
    );
    builder.search(
        "SearchParams.EnableNMP",
        "option name SearchParams.EnableNMP type check default true".to_string(),
    );
    builder.search(
        "SearchParams.EnableProbCut",
        "option name SearchParams.EnableProbCut type check default true".to_string(),
    );
    builder.search(
        "SearchParams.EnableStaticBeta",
        "option name SearchParams.EnableStaticBeta type check default true".to_string(),
    );
    builder.search(
        "SearchParams.FUT_Dyn_Base",
        "option name SearchParams.FUT_Dyn_Base type spin default 100 min 0 max 2000".to_string(),
    );
    builder.search(
        "SearchParams.FUT_Dyn_Enable",
        "option name SearchParams.FUT_Dyn_Enable type check default true".to_string(),
    );
    builder.search(
        "SearchParams.FUT_Dyn_Slope",
        "option name SearchParams.FUT_Dyn_Slope type spin default 80 min 0 max 500".to_string(),
    );
    builder.search(
        "SearchParams.FutStatDen",
        "option name SearchParams.FutStatDen type spin default 356 min 1 max 10000".to_string(),
    );
    builder.search(
        "SearchParams.HP_DepthScale",
        "option name SearchParams.HP_DepthScale type spin default 3200 min 0 max 20000".to_string(),
    );
    builder.search(
        "SearchParams.HP_Threshold",
        "option name SearchParams.HP_Threshold type spin default -2000 min -10000 max 10000"
            .to_string(),
    );
    builder.search(
        "SearchParams.IID_MinDepth",
        "option name SearchParams.IID_MinDepth type spin default 6 min 0 max 20".to_string(),
    );
    builder.search(
        "SearchParams.LMP_D1",
        "option name SearchParams.LMP_D1 type spin default 8 min 0 max 64".to_string(),
    );
    builder.search(
        "SearchParams.LMP_D2",
        "option name SearchParams.LMP_D2 type spin default 14 min 0 max 64".to_string(),
    );
    builder.search(
        "SearchParams.LMP_D3",
        "option name SearchParams.LMP_D3 type spin default 20 min 0 max 64".to_string(),
    );
    builder.search(
        "SearchParams.LMR",
        "option name SearchParams.LMR type check default true".to_string(),
    );
    builder.search(
        "SearchParams.LMR_K_x100",
        "option name SearchParams.LMR_K_x100 type spin default 170 min 80 max 400".to_string(),
    );
    builder.search(
        "SearchParams.LMR_StatDenBase",
        "option name SearchParams.LMR_StatDenBase type spin default 8192 min 1 max 65536"
            .to_string(),
    );
    builder.search(
        "SearchParams.LMR_StatNum",
        "option name SearchParams.LMR_StatNum type spin default 1 min 0 max 64".to_string(),
    );
    builder.search(
        "SearchParams.ProbCut_D5",
        "option name SearchParams.ProbCut_D5 type spin default 250 min 0 max 2000".to_string(),
    );
    builder.search(
        "SearchParams.ProbCut_D6P",
        "option name SearchParams.ProbCut_D6P type spin default 300 min 0 max 2000".to_string(),
    );
    builder.search(
        "SearchParams.ProbCut_SkipVerifyLt4",
        "option name SearchParams.ProbCut_SkipVerifyLt4 type check default false".to_string(),
    );
    builder.search(
        "SearchParams.QS.CheckSEEMargin",
        format!(
            "option name SearchParams.QS.CheckSEEMargin type spin default {} min -5000 max 5000",
            engine_core::search::params::qs_check_see_margin()
        ),
    );
    builder.search(
        "SearchParams.QS_BadCaptureMin",
        "option name SearchParams.QS_BadCaptureMin type spin default 450 min 0 max 5000"
            .to_string(),
    );
    builder.search(
        "SearchParams.QS_CheckPruneMargin",
        "option name SearchParams.QS_CheckPruneMargin type spin default 150 min 0 max 5000"
            .to_string(),
    );
    builder.search(
        "SearchParams.QS_MarginCapture",
        "option name SearchParams.QS_MarginCapture type spin default 150 min 0 max 5000"
            .to_string(),
    );
    builder.search(
        "SearchParams.QuietHistoryWeight",
        "option name SearchParams.QuietHistoryWeight type spin default 4 min -64 max 64"
            .to_string(),
    );
    builder.search(
        "SearchParams.RootBeamForceFullCount",
        "option name SearchParams.RootBeamForceFullCount type spin default 0 min 0 max 8"
            .to_string(),
    );
    builder.search(
        "SearchParams.RootMultiPV1",
        "option name SearchParams.RootMultiPV1 type spin default 50000 min -200000 max 200000"
            .to_string(),
    );
    builder.search(
        "SearchParams.RootMultiPV2",
        "option name SearchParams.RootMultiPV2 type spin default 25000 min -200000 max 200000"
            .to_string(),
    );
    builder.search(
        "SearchParams.RootPrevScoreScale",
        "option name SearchParams.RootPrevScoreScale type spin default 200 min 0 max 2000"
            .to_string(),
    );
    builder.search(
        "SearchParams.RootTTBonus",
        "option name SearchParams.RootTTBonus type spin default 1500000 min 0 max 5000000"
            .to_string(),
    );
    builder.search(
        "SearchParams.SBP_D1",
        "option name SearchParams.SBP_D1 type spin default 200 min 0 max 2000".to_string(),
    );
    builder.search(
        "SearchParams.SBP_D2",
        "option name SearchParams.SBP_D2 type spin default 300 min 0 max 2000".to_string(),
    );
    builder.search(
        "SearchParams.SBP_Dyn_Base",
        "option name SearchParams.SBP_Dyn_Base type spin default 120 min 0 max 2000".to_string(),
    );
    builder.search(
        "SearchParams.SBP_Dyn_Enable",
        "option name SearchParams.SBP_Dyn_Enable type check default true".to_string(),
    );
    builder.search(
        "SearchParams.SBP_Dyn_Slope",
        "option name SearchParams.SBP_Dyn_Slope type spin default 60 min 0 max 500".to_string(),
    );
    builder.search(
        "SearchParams.SafePruning",
        "option name SearchParams.SafePruning type check default true".to_string(),
    );
    builder.search(
        "SearchParams.SameToExtension",
        "option name SearchParams.SameToExtension type check default false".to_string(),
    );
}

fn collect_finalize_options(opts: &UsiOptions, builder: &mut OptionBuilder) {
    builder.finalize(
        "FailSafeGuard",
        format!(
            "option name FailSafeGuard type check default {}",
            bool_to_usi(opts.fail_safe_guard)
        ),
    );
    builder.finalize(
        "FinalizeSanity.AllowSEElt0Alt",
        format!(
            "option name FinalizeSanity.AllowSEElt0Alt type check default {}",
            bool_to_usi(opts.finalize_allow_see_lt0_alt)
        ),
    );
    builder.finalize(
        "FinalizeSanity.BudgetMs",
        format!(
            "option name FinalizeSanity.BudgetMs type spin default {} min 0 max 10",
            opts.finalize_sanity_budget_ms
        ),
    );
    builder.finalize(
        "FinalizeSanity.CheckPenaltyCp",
        format!(
            "option name FinalizeSanity.CheckPenaltyCp type spin default {} min 0 max 100",
            opts.finalize_sanity_check_penalty_cp
        ),
    );
    builder.finalize(
        "FinalizeSanity.DefenseSEE_NegFloorCp",
        format!(
            "option name FinalizeSanity.DefenseSEE_NegFloorCp type spin default {} min -500 max 0",
            opts.finalize_defense_see_neg_floor_cp
        ),
    );
    builder.finalize(
        "FinalizeSanity.Enabled",
        format!(
            "option name FinalizeSanity.Enabled type check default {}",
            bool_to_usi(opts.finalize_sanity_enabled)
        ),
    );
    builder.finalize(
        "FinalizeSanity.KingAltMinGainCp",
        format!(
            "option name FinalizeSanity.KingAltMinGainCp type spin default {} min 0 max 1000",
            opts.finalize_sanity_king_alt_min_gain_cp
        ),
    );
    builder.finalize(
        "FinalizeSanity.KingAltPenaltyCp",
        format!(
            "option name FinalizeSanity.KingAltPenaltyCp type spin default {} min 0 max 300",
            opts.finalize_sanity_king_alt_penalty_cp
        ),
    );
    builder.finalize(
        "FinalizeSanity.MateProbe.Depth",
        format!(
            "option name FinalizeSanity.MateProbe.Depth type spin default {} min 1 max 8",
            opts.finalize_mate_probe_depth
        ),
    );
    builder.finalize(
        "FinalizeSanity.MateProbe.Enabled",
        format!(
            "option name FinalizeSanity.MateProbe.Enabled type check default {}",
            bool_to_usi(opts.finalize_mate_probe_enabled)
        ),
    );
    builder.finalize(
        "FinalizeSanity.MateProbe.TimeMs",
        format!(
            "option name FinalizeSanity.MateProbe.TimeMs type spin default {} min 1 max 20",
            opts.finalize_mate_probe_time_ms
        ),
    );
    builder.finalize(
        "FinalizeSanity.MinMs",
        format!(
            "option name FinalizeSanity.MinMs type spin default {} min 0 max 10",
            opts.finalize_sanity_min_ms
        ),
    );
    builder.finalize(
        "FinalizeSanity.MiniDepth",
        format!(
            "option name FinalizeSanity.MiniDepth type spin default {} min 1 max 3",
            opts.finalize_sanity_mini_depth
        ),
    );
    builder.finalize(
        "FinalizeSanity.NonPromoteMajorPenaltyCp",
        format!(
            "option name FinalizeSanity.NonPromoteMajorPenaltyCp type spin default {} min 0 max 300",
            opts.finalize_non_promote_major_penalty_cp
        ),
    );
    builder.finalize(
        "FinalizeSanity.OppSEE_MinCp",
        format!(
            "option name FinalizeSanity.OppSEE_MinCp type spin default {} min 0 max 5000",
            opts.finalize_sanity_opp_see_min_cp
        ),
    );
    builder.finalize(
        "FinalizeSanity.OppSEE_PenaltyCapCp",
        format!(
            "option name FinalizeSanity.OppSEE_PenaltyCapCp type spin default {} min 0 max 5000",
            opts.finalize_sanity_opp_see_penalty_cap_cp
        ),
    );
    builder.finalize(
        "FinalizeSanity.RiskMinDeltaCp",
        format!(
            "option name FinalizeSanity.RiskMinDeltaCp type spin default {} min 0 max 600",
            opts.finalize_risk_min_delta_cp
        ),
    );
    builder.finalize(
        "FinalizeSanity.SEE_MinCp",
        format!(
            "option name FinalizeSanity.SEE_MinCp type spin default {} min -1000 max 1000",
            opts.finalize_sanity_see_min_cp
        ),
    );
    builder.finalize(
        "FinalizeSanity.SwitchMarginCp",
        format!(
            "option name FinalizeSanity.SwitchMarginCp type spin default {} min 0 max 500",
            opts.finalize_sanity_switch_margin_cp
        ),
    );
    builder.finalize(
        "FinalizeSanity.Threat2_BeamK",
        format!(
            "option name FinalizeSanity.Threat2_BeamK type spin default {} min 0 max 64",
            opts.finalize_threat2_beam_k
        ),
    );
    builder.finalize(
        "FinalizeSanity.Threat2_ExtremeMinCp",
        format!(
            "option name FinalizeSanity.Threat2_ExtremeMinCp type spin default {} min 0 max 5000",
            opts.finalize_threat2_extreme_min_cp
        ),
    );
    builder.finalize(
        "FinalizeSanity.Threat2_ExtremeWinDisableCp",
        format!(
            "option name FinalizeSanity.Threat2_ExtremeWinDisableCp type spin default {} min 0 max 5000",
            opts.finalize_threat2_extreme_win_disable_cp
        ),
    );
    builder.finalize(
        "FinalizeSanity.Threat2_MinCp",
        format!(
            "option name FinalizeSanity.Threat2_MinCp type spin default {} min 0 max 5000",
            opts.finalize_threat2_min_cp
        ),
    );
    builder.finalize(
        "GameoverSendsBestmove",
        format!(
            "option name GameoverSendsBestmove type check default {}",
            bool_to_usi(opts.gameover_sends_bestmove)
        ),
    );
    builder.finalize(
        "InstantMateMove.CheckAllPV",
        format!(
            "option name InstantMateMove.CheckAllPV type check default {}",
            bool_to_usi(opts.instant_mate_check_all_pv)
        ),
    );
    builder.finalize(
        "InstantMateMove.Enabled",
        format!(
            "option name InstantMateMove.Enabled type check default {}",
            bool_to_usi(opts.instant_mate_move_enabled)
        ),
    );
    builder.finalize(
        "InstantMateMove.MaxDistance",
        format!(
            "option name InstantMateMove.MaxDistance type spin default {} min 1 max 5",
            opts.instant_mate_move_max_distance
        ),
    );
    builder.finalize(
        "InstantMateMove.MinDepth",
        format!(
            "option name InstantMateMove.MinDepth type spin default {} min 0 max 64",
            opts.instant_mate_min_depth
        ),
    );
    builder.finalize(
        "InstantMateMove.MinRespectMs",
        format!(
            "option name InstantMateMove.MinRespectMs type spin default {} min 0 max 1000",
            opts.instant_mate_min_respect_ms
        ),
    );
    let snapshot_default = if opts.instant_mate_require_stable {
        "Stable"
    } else {
        "Any"
    };
    builder.finalize(
        "InstantMateMove.RequiredSnapshot",
        format!(
            "option name InstantMateMove.RequiredSnapshot type combo default {} var Stable var Any",
            snapshot_default
        ),
    );
    builder.finalize(
        "InstantMateMove.RespectMinThinkMs",
        format!(
            "option name InstantMateMove.RespectMinThinkMs type check default {}",
            bool_to_usi(opts.instant_mate_respect_min_think_ms)
        ),
    );
    let verify_mode = match opts.instant_mate_verify_mode {
        crate::state::InstantMateVerifyMode::Off => "Off",
        crate::state::InstantMateVerifyMode::CheckOnly => "CheckOnly",
        crate::state::InstantMateVerifyMode::QSearch => "QSearch",
    };
    builder.finalize(
        "InstantMateMove.VerifyMode",
        format!(
            "option name InstantMateMove.VerifyMode type combo default {} var Off var CheckOnly var QSearch",
            verify_mode
        ),
    );
    builder.finalize(
        "InstantMateMove.VerifyNodes",
        format!(
            "option name InstantMateMove.VerifyNodes type spin default {} min 0 max 100000",
            opts.instant_mate_verify_nodes
        ),
    );
    builder.finalize(
        "MateEarlyStop",
        format!(
            "option name MateEarlyStop type check default {}",
            bool_to_usi(opts.mate_early_stop)
        ),
    );
    builder.finalize(
        "MateGate.FastOkMinDepth",
        format!(
            "option name MateGate.FastOkMinDepth type spin default {} min 0 max 64",
            opts.mate_gate_fast_ok_min_depth
        ),
    );
    builder.finalize(
        "MateGate.FastOkMinElapsedMs",
        format!(
            "option name MateGate.FastOkMinElapsedMs type spin default {} min 0 max 5000",
            opts.mate_gate_fast_ok_min_elapsed_ms
        ),
    );
    builder.finalize(
        "MateGate.MinStableDepth",
        format!(
            "option name MateGate.MinStableDepth type spin default {} min 0 max 64",
            opts.mate_gate_min_stable_depth
        ),
    );
}

fn collect_diagnostics_options(opts: &UsiOptions, builder: &mut OptionBuilder) {
    if cfg!(any(test, feature = "diagnostics")) {
        builder.diagnostics(
            "PostVerify",
            format!("option name PostVerify type check default {}", bool_to_usi(opts.post_verify)),
        );
        builder.diagnostics(
            "PostVerify.DisadvantageCp",
            format!(
                "option name PostVerify.DisadvantageCp type spin default {} min -10000 max 0",
                opts.post_verify_disadvantage_cp
            ),
        );
        builder.diagnostics(
            "PostVerify.ExactMinDepth",
            format!(
                "option name PostVerify.ExactMinDepth type spin default {} min 0 max 64",
                opts.mate_postverify_exact_min_depth
            ),
        );
        builder.diagnostics(
            "PostVerify.ExactMinElapsedMs",
            format!(
                "option name PostVerify.ExactMinElapsedMs type spin default {} min 0 max 10000",
                opts.mate_postverify_exact_min_elapsed_ms
            ),
        );
        builder.diagnostics(
            "PostVerify.ExtendMs",
            format!(
                "option name PostVerify.ExtendMs type spin default {} min 0 max 5000",
                opts.post_verify_extend_ms
            ),
        );
        builder.diagnostics(
            "PostVerify.RequirePass",
            format!(
                "option name PostVerify.RequirePass type check default {}",
                bool_to_usi(opts.post_verify_require_pass)
            ),
        );
        builder.diagnostics(
            "PostVerify.SkipMateDistance",
            format!(
                "option name PostVerify.SkipMateDistance type spin default {} min 1 max 32",
                opts.mate_postverify_skip_max_dist
            ),
        );
        builder.diagnostics(
            "PostVerify.YDrop",
            format!(
                "option name PostVerify.YDrop type spin default {} min 0 max 5000",
                opts.y_drop_cp
            ),
        );
        builder.diagnostics(
            "PromoteVerify",
            format!(
                "option name PromoteVerify type check default {}",
                bool_to_usi(opts.promote_verify)
            ),
        );
        builder.diagnostics(
            "PromoteVerify.BiasCp",
            format!(
                "option name PromoteVerify.BiasCp type spin default {} min -1000 max 1000",
                opts.promote_bias_cp
            ),
        );
    }
}

fn collect_utility_options(opts: &UsiOptions, builder: &mut OptionBuilder) {
    builder.utility(
        "ForcedMove.EmitEval",
        format!(
            "option name ForcedMove.EmitEval type check default {}",
            bool_to_usi(opts.forced_move_emit_eval)
        ),
    );
    builder.utility(
        "ForcedMove.MinSearchMs",
        format!(
            "option name ForcedMove.MinSearchMs type spin default {} min 0 max 50",
            opts.forced_move_min_search_ms
        ),
    );
    builder.utility(
        "Profile.ApplyAutoDefaults",
        "option name Profile.ApplyAutoDefaults type button".to_string(),
    );
    let profile_mode_default = match opts.profile_mode {
        crate::state::ProfileMode::Auto => "Auto",
        crate::state::ProfileMode::T1 => "T1",
        crate::state::ProfileMode::T8 => "T8",
        crate::state::ProfileMode::Off => "Off",
    };
    builder.utility(
        "Profile.Mode",
        format!(
            "option name Profile.Mode type combo default {} var Auto var T1 var T8 var Off",
            profile_mode_default
        ),
    );
    builder.utility(
        "Warmup.Ms",
        format!("option name Warmup.Ms type spin default {} min 0 max 60000", opts.warmup_ms),
    );
    builder.utility(
        "Warmup.PrevMoves",
        format!(
            "option name Warmup.PrevMoves type spin default {} min 0 max 20",
            opts.warmup_prev_moves
        ),
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
            mark_override(state, "Threads");
        }
        "USI_Ponder" => {
            if let Some(v) = value_ref {
                let v = v.to_lowercase();
                state.opts.ponder = matches!(v.as_str(), "true" | "1" | "on");
            }
        }
        "LogProfile" => {
            if let Some(v) = value_ref {
                if let Some(profile) = LogProfile::from_str(v) {
                    state.opts.log_profile = profile;
                    info_string(format!("log_profile_set={}", profile.as_str()));
                } else {
                    info_string(format!("log_profile_invalid value={}", v));
                }
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
                    _ => EngineType::Enhanced,
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
        // --- Material evaluator (lightweight) knobs ---
        "Eval.Material.TempoCp" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    engine_core::evaluation::evaluate::set_material_tempo_cp(x);
                    info_string(format!("eval_material_tempo_cp={}", x.clamp(-200, 200)));
                }
            }
        }
        "Eval.Material.RookMobilityCp" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    engine_core::evaluation::evaluate::set_material_rook_mobility_cp(x);
                    info_string(format!("eval_material_rook_mob_cp={}", x.clamp(0, 50)));
                }
            }
        }
        "Eval.Material.RookTrappedPenalty" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    engine_core::evaluation::evaluate::set_material_rook_trapped_penalty_cp(x);
                    info_string(format!("eval_material_rook_trap_cp={}", x.clamp(0, 500)));
                }
            }
        }
        "Eval.Material.KingEarlyMovePenaltyCp" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    engine_core::evaluation::evaluate::set_material_king_early_move_penalty_cp(x);
                    info_string(format!("eval_material_king_early_pen_cp={}", x.clamp(0, 200)));
                }
            }
        }
        "Eval.Material.KingEarlyMoveMaxPly" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    engine_core::evaluation::evaluate::set_material_king_early_move_max_ply(x);
                    info_string(format!("eval_material_king_early_max_ply={}", x.clamp(0, 100)));
                }
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
        "SearchParams.FutStatDen" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    engine_core::search::params::set_fut_stat_den(x);
                }
            }
        }
        "SearchParams.LMR_StatNum" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    engine_core::search::params::set_lmr_stat_num(x);
                }
            }
        }
        "SearchParams.LMR_StatDenBase" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    engine_core::search::params::set_lmr_stat_den_base(x);
                }
            }
        }
        "SearchParams.LMR" => {
            if let Some(v) = value_ref {
                let on = matches!(v.to_lowercase().as_str(), "on" | "true" | "1");
                engine_core::search::params::set_lmr_enabled(on);
                info_string(if on { "lmr=On" } else { "lmr=Off" });
                mark_override(state, "SearchParams.LMR");
            }
        }
        "SearchParams.SameToExtension" => {
            if let Some(v) = value_ref {
                let on = matches!(v.to_lowercase().as_str(), "on" | "true" | "1");
                engine_core::search::params::set_same_to_extension(on);
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
        "SearchParams.QS.CheckSEEMargin" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    engine_core::search::params::set_qs_check_see_margin(x);
                }
            }
        }
        "Search.NMP.Verify" => {
            if let Some(v) = value_ref {
                let on = matches!(v.to_lowercase().as_str(), "on" | "true" | "1");
                engine_core::search::policy::set_nmp_verify_enabled(on);
                info_string(if on {
                    "nmp_verify=On"
                } else {
                    "nmp_verify=Off"
                });
                mark_override(state, "Search.NMP.Verify");
            }
        }
        "Search.NMP.VerifyMinDepth" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    engine_core::search::policy::set_nmp_verify_min_depth(x);
                    info_string(format!("nmp_verify_min_depth={}", x.clamp(2, 64)));
                    mark_override(state, "Search.NMP.VerifyMinDepth");
                }
            }
        }
        // Quiet SEE Gate (Non-PV静止のxSEEゲート)
        "Search.QuietSeeGuard" => {
            if let Some(v) = value_ref {
                let on = matches!(v.to_lowercase().as_str(), "on" | "true" | "1");
                engine_core::search::policy::set_quiet_see_guard_enabled(on);
                info_string(if on {
                    "quiet_see_guard=On"
                } else {
                    "quiet_see_guard=Off"
                });
                mark_override(state, "Search.QuietSeeGuard");
            }
        }
        // Capture Futility + SEE（深さ比例）
        "Search.CaptureFutility" => {
            if let Some(v) = value_ref {
                let on = matches!(v.to_lowercase().as_str(), "on" | "true" | "1");
                engine_core::search::policy::set_capture_futility_enabled(on);
                info_string(if on {
                    "capture_fut=On"
                } else {
                    "capture_fut=Off"
                });
                mark_override(state, "Search.CaptureFutility");
            }
        }
        "Search.CaptureFutility.ScalePct" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    engine_core::search::policy::set_capture_futility_scale_pct(x);
                    info_string(format!("capture_fut_scale_pct={}", x.clamp(25, 150)));
                    mark_override(state, "Search.CaptureFutility.ScalePct");
                }
            }
        }
        // Singular Extension (runtime toggles)
        "Search.Singular.Enabled" => {
            if let Some(v) = value_ref {
                let on = matches!(v.to_lowercase().as_str(), "on" | "true" | "1");
                engine_core::search::policy::set_singular_enabled(on);
                info_string(if on { "singular=On" } else { "singular=Off" });
                mark_override(state, "Search.Singular.Enabled");
            }
        }
        "Search.Singular.MinDepth" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    engine_core::search::policy::set_singular_min_depth(x);
                    info_string(format!("singular_min_depth={}", x.clamp(2, 64)));
                    mark_override(state, "Search.Singular.MinDepth");
                }
            }
        }
        "Search.Singular.MarginBase" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    engine_core::search::policy::set_singular_margin_base(x);
                    info_string(format!("singular_margin_base={}", x.clamp(0, 512)));
                    mark_override(state, "Search.Singular.MarginBase");
                }
            }
        }
        "Search.Singular.MarginScalePct" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    engine_core::search::policy::set_singular_margin_scale_pct(x);
                    info_string(format!("singular_margin_scale_pct={}", x.clamp(10, 300)));
                    mark_override(state, "Search.Singular.MarginScalePct");
                }
            }
        }
        // ShallowGate runtime toggles (previously env-only)
        "Search.ShallowGate" => {
            if let Some(v) = value_ref {
                let on = matches!(v.to_lowercase().as_str(), "on" | "true" | "1");
                engine_core::search::params::set_shallow_gate_enabled(on);
                info_string(if on {
                    "shallow_gate=On"
                } else {
                    "shallow_gate=Off"
                });
                mark_override(state, "Search.ShallowGate");
            }
        }
        "Search.ShallowGate.Depth" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    engine_core::search::params::set_shallow_gate_depth(x);
                    info_string(format!("shallow_gate_depth={}", x.clamp(1, 8)));
                    mark_override(state, "Search.ShallowGate.Depth");
                }
            }
        }
        "SearchParams.RootBeamForceFullCount" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<usize>() {
                    engine_core::search::params::set_root_beam_force_full_count(x);
                }
            }
        }
        // --- Root guard rails (revived) ---
        "RootSeeGate" => {
            if let Some(v) = value_ref {
                let on = matches!(v.to_lowercase().as_str(), "on" | "true" | "1");
                state.opts.root_see_gate = on;
                engine_core::search::config::set_root_see_gate_enabled(on);
                mark_override(state, "RootSeeGate");
            }
        }
        "RootSeeGate.XSEE" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    state.opts.root_see_gate_xsee_cp = x.clamp(0, 5000);
                    engine_core::search::config::set_root_see_gate_xsee_cp(
                        state.opts.root_see_gate_xsee_cp,
                    );
                    mark_override(state, "RootSeeGate.XSEE");
                }
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
            // Ensure MinMs <= BudgetMs
            if state.opts.finalize_sanity_min_ms > state.opts.finalize_sanity_budget_ms {
                state.opts.finalize_sanity_min_ms = state.opts.finalize_sanity_budget_ms;
                info_string("finalize_minms_clamped=1");
            }
            mark_override(state, "FinalizeSanity.BudgetMs");
        }
        "FinalizeSanity.MinMs" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<u64>() {
                    state.opts.finalize_sanity_min_ms = x.min(10);
                }
            }
            // Ensure MinMs <= BudgetMs
            if state.opts.finalize_sanity_min_ms > state.opts.finalize_sanity_budget_ms {
                state.opts.finalize_sanity_min_ms = state.opts.finalize_sanity_budget_ms;
                info_string("finalize_minms_clamped=1");
            }
            mark_override(state, "FinalizeSanity.MinMs");
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
        "FinalizeSanity.Threat2_MinCp" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    state.opts.finalize_threat2_min_cp = x.clamp(0, 5000);
                }
            }
            mark_override(state, "FinalizeSanity.Threat2_MinCp");
        }
        "FinalizeSanity.Threat2_BeamK" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<u8>() {
                    state.opts.finalize_threat2_beam_k = x.min(64);
                }
            }
        }
        "FinalizeSanity.Threat2_ExtremeMinCp" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    state.opts.finalize_threat2_extreme_min_cp = x.clamp(0, 5000);
                }
            }
        }
        "FinalizeSanity.Threat2_ExtremeWinDisableCp" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    state.opts.finalize_threat2_extreme_win_disable_cp = x.clamp(0, 5000);
                }
            }
        }
        "FinalizeSanity.AllowSEElt0Alt" => {
            if let Some(v) = value_ref {
                let on = matches!(v.to_lowercase().as_str(), "on" | "true" | "1");
                state.opts.finalize_allow_see_lt0_alt = on;
            }
        }
        "FinalizeSanity.DefenseSEE_NegFloorCp" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    state.opts.finalize_defense_see_neg_floor_cp = x.clamp(-500, 0);
                }
            }
            mark_override(state, "FinalizeSanity.DefenseSEE_NegFloorCp");
        }
        "FinalizeSanity.RiskMinDeltaCp" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    state.opts.finalize_risk_min_delta_cp = x.clamp(0, 600);
                }
            }
            mark_override(state, "FinalizeSanity.RiskMinDeltaCp");
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
        "MateGate.MinStableDepth" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<u8>() {
                    state.opts.mate_gate_min_stable_depth = x.clamp(0, 64);
                    info_string(format!(
                        "mate_gate_min_stable_depth={}",
                        state.opts.mate_gate_min_stable_depth
                    ));
                }
            }
        }
        "MateGate.FastOkMinDepth" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<u8>() {
                    state.opts.mate_gate_fast_ok_min_depth = x.clamp(0, 64);
                    info_string(format!(
                        "mate_gate_fast_ok_min_depth={}",
                        state.opts.mate_gate_fast_ok_min_depth
                    ));
                }
            }
        }
        "MateGate.FastOkMinElapsedMs" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<u64>() {
                    state.opts.mate_gate_fast_ok_min_elapsed_ms = x.min(5000);
                    info_string(format!(
                        "mate_gate_fast_ok_min_elapsed_ms={}",
                        state.opts.mate_gate_fast_ok_min_elapsed_ms
                    ));
                }
            }
        }
        // Finalize: optional mini mate-probe knobs
        "FinalizeSanity.MateProbe.Enabled" => {
            if let Some(v) = value_ref {
                let on = matches!(v.to_lowercase().as_str(), "true" | "1" | "on");
                state.opts.finalize_mate_probe_enabled = on;
            }
        }
        "FinalizeSanity.MateProbe.Depth" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<u8>() {
                    state.opts.finalize_mate_probe_depth = x.clamp(1, 8);
                }
            }
        }
        "FinalizeSanity.MateProbe.TimeMs" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<u64>() {
                    state.opts.finalize_mate_probe_time_ms = x.min(20);
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
        // RootSeeGate 系は廃止
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
        "PostVerify.RequirePass" => {
            if let Some(v) = value_ref {
                let on = matches!(v.to_lowercase().as_str(), "true" | "1" | "on");
                state.opts.post_verify_require_pass = on;
            }
            // 明示上書きの印を付ける（Threads連動の自動既定で巻き戻さない／ログに可視化）
            mark_override(state, "PostVerify.RequirePass");
        }
        "PostVerify.ExtendMs" => {
            if let Some(v) = value_ref {
                if let Ok(ms) = v.parse::<u64>() {
                    state.opts.post_verify_extend_ms = ms.min(5000);
                }
            }
            // 明示上書きの印を付ける（Threads連動の自動既定で巻き戻さない／ログに可視化）
            mark_override(state, "PostVerify.ExtendMs");
        }
        "PostVerify.DisadvantageCp" => {
            if let Some(v) = value_ref {
                if let Ok(cp) = v.parse::<i32>() {
                    state.opts.post_verify_disadvantage_cp = cp.clamp(-10000, 0);
                }
            }
        }
        "PostVerify.SkipMateDistance" => {
            if let Some(v) = value_ref {
                if let Ok(d) = v.parse::<u32>() {
                    state.opts.mate_postverify_skip_max_dist = d.clamp(1, 32);
                }
            }
            mark_override(state, "PostVerify.SkipMateDistance");
        }
        "PostVerify.ExactMinDepth" => {
            if let Some(v) = value_ref {
                if let Ok(d) = v.parse::<u32>() {
                    state.opts.mate_postverify_exact_min_depth = d.min(64) as u8;
                }
            }
            mark_override(state, "PostVerify.ExactMinDepth");
        }
        "PostVerify.ExactMinElapsedMs" => {
            if let Some(v) = value_ref {
                if let Ok(ms) = v.parse::<u64>() {
                    state.opts.mate_postverify_exact_min_elapsed_ms = ms.min(10_000);
                }
            }
            mark_override(state, "PostVerify.ExactMinElapsedMs");
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
        "FinalizeSanity.KingAltMinGainCp" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    state.opts.finalize_sanity_king_alt_min_gain_cp = x.clamp(0, 1000);
                }
            }
            mark_override(state, "FinalizeSanity.KingAltMinGainCp");
        }
        "FinalizeSanity.KingAltPenaltyCp" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    state.opts.finalize_sanity_king_alt_penalty_cp = x.clamp(0, 300);
                }
            }
            mark_override(state, "FinalizeSanity.KingAltPenaltyCp");
        }
        "FinalizeSanity.NonPromoteMajorPenaltyCp" => {
            if let Some(v) = value_ref {
                if let Ok(x) = v.parse::<i32>() {
                    state.opts.finalize_non_promote_major_penalty_cp = x.clamp(0, 300);
                }
            }
            mark_override(state, "FinalizeSanity.NonPromoteMajorPenaltyCp");
        }
        "PVStabilityBase" => {
            if let Some(v) = value_ref {
                if let Ok(ms) = v.parse::<u64>() {
                    state.opts.pv_stability_base = ms.clamp(10, 200);
                }
            }
        }
        "ForcedMove.EmitEval" => {
            if let Some(v) = value_ref {
                let on = matches!(v.to_lowercase().as_str(), "true" | "1" | "on");
                state.opts.forced_move_emit_eval = on;
            }
            mark_override(state, "ForcedMove.EmitEval");
        }
        "ForcedMove.MinSearchMs" => {
            if let Some(v) = value_ref {
                if let Ok(ms) = v.parse::<u64>() {
                    state.opts.forced_move_min_search_ms = ms.min(50);
                }
            }
            mark_override(state, "ForcedMove.MinSearchMs");
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
                    "PostVerify",
                    "PostVerify.YDrop",
                    "FinalizeSanity.SwitchMarginCp",
                    "FinalizeSanity.OppSEE_MinCp",
                    "FinalizeSanity.BudgetMs",
                    "FinalizeSanity.KingAltMinGainCp",
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
                    // Emit detailed NNUE diagnostics for easier field troubleshooting
                    if let Some((diag, start_cp)) = eng.nnue_diag_for_logging() {
                        match (diag.backend, diag.single.as_ref(), diag.classic.as_ref()) {
                            ("single", Some(s), _) => {
                                info_string(format!(
                                    "nnue_single dims input_dim={} acc_dim={} uid=0x{:016x}",
                                    s.input_dim, s.acc_dim, s.uid
                                ));
                            }
                            ("classic", _, Some(c)) => {
                                info_string(format!(
                                    "nnue_classic dims acc_dim={} input_dim={} h1_dim={} h2_dim={}",
                                    c.acc_dim, c.input_dim, c.h1_dim, c.h2_dim
                                ));
                            }
                            (other, _, _) => {
                                info_string(format!("nnue_diag backend={} (no dims)", other));
                            }
                        }
                        info_string(format!("nnue_startpos_cp={}", start_cp));
                    }
                }
            }
        }
        // If NNUE is active, disable Material-only lightweight heuristics to avoid double counting
        if matches!(state.opts.engine_type, EngineType::Nnue | EngineType::EnhancedNnue) {
            engine_core::evaluation::evaluate::set_material_tempo_cp(0);
            engine_core::evaluation::evaluate::set_material_rook_mobility_cp(0);
            engine_core::evaluation::evaluate::set_material_rook_trapped_penalty_cp(0);
            engine_core::evaluation::evaluate::set_material_king_early_move_penalty_cp(0);
        }
    }
    engine_core::search::config::set_mate_early_stop_enabled(state.opts.mate_early_stop);
    // Align engine-core early mate stop distance with USI option (InstantMateMove.MaxDistance)
    engine_core::search::config::set_mate_early_stop_max_distance(
        state.opts.instant_mate_move_max_distance as u8,
    );
    engine_core::search::config::set_post_verify_enabled(state.opts.post_verify);
    engine_core::search::config::set_post_verify_ydrop_cp(state.opts.y_drop_cp);
    // Root SEE Gate (revived)
    engine_core::search::config::set_root_see_gate_enabled(state.opts.root_see_gate);
    engine_core::search::config::set_root_see_gate_xsee_cp(state.opts.root_see_gate_xsee_cp);
    // Root retry (one-shot) は廃止（YO準拠）。
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
        set_if_absent("PostVerify", &mut || state.opts.post_verify = true);
        set_if_absent("PostVerify.YDrop", &mut || state.opts.y_drop_cp = 225);
        set_if_absent("PostVerify.RequirePass", &mut || {
            state.opts.post_verify_require_pass = false
        });
        set_if_absent("PostVerify.ExtendMs", &mut || state.opts.post_verify_extend_ms = 300);
        set_if_absent("FinalizeSanity.SwitchMarginCp", &mut || {
            state.opts.finalize_sanity_switch_margin_cp = 40
        });
        set_if_absent("FinalizeSanity.OppSEE_MinCp", &mut || {
            state.opts.finalize_sanity_opp_see_min_cp = 100
        });
        set_if_absent("FinalizeSanity.BudgetMs", &mut || state.opts.finalize_sanity_budget_ms = 8);
        set_if_absent("FinalizeSanity.MinMs", &mut || state.opts.finalize_sanity_min_ms = 2);
        set_if_absent("FinalizeSanity.Threat2_MinCp", &mut || {
            state.opts.finalize_threat2_min_cp = 200
        });
        set_if_absent("FinalizeSanity.Threat2_BeamK", &mut || {
            state.opts.finalize_threat2_beam_k = 4
        });
        set_if_absent("FinalizeSanity.KingAltMinGainCp", &mut || {
            state.opts.finalize_sanity_king_alt_min_gain_cp = 150
        });
        set_if_absent("FinalizeSanity.AllowSEElt0Alt", &mut || {
            state.opts.finalize_allow_see_lt0_alt = false
        });
        // ルートビーム: 上位2手までは必ずフル探索する
        set_if_absent("SearchParams.RootBeamForceFullCount", &mut || {
            engine_core::search::params::set_root_beam_force_full_count(2);
        });
        set_if_absent("MultiPV", &mut || state.opts.multipv = 1);
        // ShallowGate: enable in common profile by default (YO寄せ; 浅層の過剰刈り抑制)
        engine_core::search::params::set_shallow_gate_enabled(true);
        engine_core::search::params::set_shallow_gate_depth(3);
    } else {
        // T1 profile

        set_if_absent("PostVerify", &mut || state.opts.post_verify = true);
        set_if_absent("PostVerify.YDrop", &mut || state.opts.y_drop_cp = 225);
        set_if_absent("PostVerify.RequirePass", &mut || {
            state.opts.post_verify_require_pass = false
        });
        set_if_absent("PostVerify.ExtendMs", &mut || state.opts.post_verify_extend_ms = 300);
        set_if_absent("FinalizeSanity.SwitchMarginCp", &mut || {
            state.opts.finalize_sanity_switch_margin_cp = 40
        });
        set_if_absent("FinalizeSanity.OppSEE_MinCp", &mut || {
            state.opts.finalize_sanity_opp_see_min_cp = 120
        });
        set_if_absent("FinalizeSanity.BudgetMs", &mut || state.opts.finalize_sanity_budget_ms = 4);
        set_if_absent("FinalizeSanity.MinMs", &mut || state.opts.finalize_sanity_min_ms = 2);
        set_if_absent("FinalizeSanity.Threat2_MinCp", &mut || {
            state.opts.finalize_threat2_min_cp = 200
        });
        set_if_absent("FinalizeSanity.Threat2_BeamK", &mut || {
            state.opts.finalize_threat2_beam_k = 4
        });
        set_if_absent("FinalizeSanity.KingAltMinGainCp", &mut || {
            state.opts.finalize_sanity_king_alt_min_gain_cp = 150
        });
        set_if_absent("FinalizeSanity.AllowSEElt0Alt", &mut || {
            state.opts.finalize_allow_see_lt0_alt = false
        });
        set_if_absent("MultiPV", &mut || state.opts.multipv = 1);
        // ShallowGate for T1 as well (保守的既定)
        engine_core::search::params::set_shallow_gate_enabled(true);
        engine_core::search::params::set_shallow_gate_depth(3);
    }

    if !state.opts.finalize_sanity_enabled
        && !state.user_overrides.contains("PostVerify.RequirePass")
    {
        state.opts.post_verify_require_pass = false;
        static REQUIRE_PASS_NOTICE: OnceLock<()> = OnceLock::new();
        if REQUIRE_PASS_NOTICE.set(()).is_ok() {
            crate::io::info_string(
                "post_verify_require_pass_default=0 (post_verify_require_pass disabled by default)",
            );
        }
    }
}

/// Emit a one-line info string with the effective profile and key parameters.
pub fn log_effective_profile(state: &EngineState) {
    if state.opts.log_profile.is_prod() {
        return;
    }

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
        "PostVerify",
        "PostVerify.YDrop",
        "PostVerify.RequirePass",
        "PostVerify.ExtendMs",
        "PostVerify.SkipMateDistance",
        "PostVerify.ExactMinDepth",
        "PostVerify.ExactMinElapsedMs",
        "FinalizeSanity.SwitchMarginCp",
        "FinalizeSanity.OppSEE_MinCp",
        "FinalizeSanity.BudgetMs",
        "FinalizeSanity.MinMs",
        "FinalizeSanity.Threat2_MinCp",
        "FinalizeSanity.Threat2_BeamK",
        "FinalizeSanity.AllowSEElt0Alt",
        "FinalizeSanity.DefenseSEE_NegFloorCp",
        "FinalizeSanity.RiskMinDeltaCp",
        "FinalizeSanity.KingAltMinGainCp",
        "FinalizeSanity.KingAltPenaltyCp",
        "FinalizeSanity.NonPromoteMajorPenaltyCp",
        "ForcedMove.EmitEval",
        "ForcedMove.MinSearchMs",
        "MultiPV",
    ] {
        if state.user_overrides.contains(k) {
            overrides.push(k);
        }
    }
    info_string(format!(
        "effective_profile mode={} resolved={} threads={} multipv={} root_see_gate={} xsee={} post_verify={} ydrop={} finalize_enabled={} finalize_switch={} finalize_oppsee={} finalize_budget={} t2_min={} t2_beam_k={} see_lt0_alt={} king_alt_min={} king_alt_pen={} mate_gate_cfg=stable>={}||depth>={}||elapsed>={}ms overrides={} threads_overridden={}",
        mode_str,
        resolved.unwrap_or("-"),
        state.opts.threads,
        state.opts.multipv,
        state.opts.root_see_gate as u8,
        state.opts.root_see_gate_xsee_cp,
        state.opts.post_verify as u8,
        state.opts.y_drop_cp,
        state.opts.finalize_sanity_enabled as u8,
        state.opts.finalize_sanity_switch_margin_cp,
        state.opts.finalize_sanity_opp_see_min_cp,
        state.opts.finalize_sanity_budget_ms,
        state.opts.finalize_threat2_min_cp,
        state.opts.finalize_threat2_beam_k,
        state.opts.finalize_allow_see_lt0_alt as u8,
        state.opts.finalize_sanity_king_alt_min_gain_cp,
        state.opts.finalize_sanity_king_alt_penalty_cp,
        state.opts.mate_gate_min_stable_depth,
        state.opts.mate_gate_fast_ok_min_depth,
        state.opts.mate_gate_fast_ok_min_elapsed_ms,
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

#[cfg(test)]
mod tests {
    use super::handle_setoption;
    use crate::state::EngineState;

    #[test]
    fn apply_auto_defaults_keeps_user_overrides_for_postverify() {
        let mut state = EngineState::new();

        // 明示 override（RequirePass=false, ExtendMs=1234）
        handle_setoption("setoption name PostVerify.RequirePass value false", &mut state)
            .expect("setoption RequirePass false");
        assert!(state.user_overrides.contains("PostVerify.RequirePass"));
        assert!(!state.opts.post_verify_require_pass);

        handle_setoption("setoption name PostVerify.ExtendMs value 1234", &mut state)
            .expect("setoption ExtendMs 1234");
        assert!(state.user_overrides.contains("PostVerify.ExtendMs"));
        assert_eq!(state.opts.post_verify_extend_ms, 1234);

        // T8 プロファイルを明示してから既定適用
        handle_setoption("setoption name Profile.Mode value T8", &mut state)
            .expect("set profile mode");
        handle_setoption("setoption name Profile.ApplyAutoDefaults", &mut state)
            .expect("apply auto defaults");

        // 期待: ユーザー上書きは保持され、既定による上書きはされない
        assert!(state.user_overrides.contains("PostVerify.RequirePass"));
        assert!(state.user_overrides.contains("PostVerify.ExtendMs"));
        assert!(!state.opts.post_verify_require_pass);
        assert_eq!(state.opts.post_verify_extend_ms, 1234);
    }
}
