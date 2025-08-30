use crate::command_handler::CommandContext;
use crate::emit_utils::log_tsv;
use crate::emit_utils::{build_meta, log_on_stop_snapshot, log_on_stop_source};
use crate::helpers::generate_fallback_move;
use crate::types::BestmoveSource;
use crate::usi::send_info_string;
use anyhow::Result;
use engine_core::movegen::MoveGenerator;

pub(crate) fn handle_stop_command(ctx: &mut CommandContext) -> Result<()> {
    let _ = send_info_string(log_tsv(&[("kind", "stop_begin")]));
    // If nothing to stop, return
    if !ctx.search_state.is_searching() {
        let _ = send_info_string(log_tsv(&[("kind", "stop_noop")]));
        return Ok(());
    }

    // Signal stop to worker
    ctx.search_state.request_stop();
    if let Some(ref stop_flag) = *ctx.current_stop_flag {
        stop_flag.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    // Emit diagnostic snapshot for race analysis (standardized)
    log_on_stop_snapshot(
        &format!("{:?}", *ctx.search_state),
        *ctx.current_search_is_ponder,
        ctx.current_committed.is_some(),
        ctx.last_partial_result.is_some(),
        ctx.pre_session_fallback.is_some(),
    );

    // Central finalize is deferred to the end to allow pre_session/emergency precedence.

    // Early pre_session attempt (both normal and ponder): prioritize known-safe fallback（段階撤去予定）
    if let Some(saved_move) = ctx.pre_session_fallback.clone() {
        if let Ok(adapter) = ctx.engine.try_lock() {
            let t0 = std::time::Instant::now();
            let current_hash = adapter.get_position().map(|p| p.zobrist_hash());
            if current_hash == *ctx.pre_session_fallback_hash {
                if let Some(pos) = adapter.get_position() {
                    if let Some(norm_a) =
                        engine_core::util::usi_helpers::normalize_usi_move_str_logged(
                            pos,
                            &saved_move,
                        )
                    {
                        // If PositionState exists, double-check; otherwise accept adapter result
                        let accept = if let Some(state) = ctx.position_state.as_ref() {
                            if let Ok(pos2) = engine_core::usi::restore_snapshot_and_verify(
                                &state.sfen_snapshot,
                                state.root_hash,
                            ) {
                                let mg = MoveGenerator::new();
                                if let Ok(legal) = mg.generate_all(&pos2) {
                                    if legal.as_slice().is_empty() {
                                        let _ = send_info_string(log_tsv(&[("kind", "stop_pre_session_skip"), ("reason", "no_legal_moves")]));
                                        false
                                    } else if let Some(norm_s) = engine_core::util::usi_helpers::normalize_usi_move_str_logged(&pos2, &saved_move) {
                                        norm_a == norm_s
                                    } else {
                                        let _ = send_info_string(log_tsv(&[("kind", "stop_pre_session_skip"), ("reason", "normalize_failed_state")]));
                                        false
                                    }
                                } else {
                                    false
                                }
                            } else {
                                false
                            }
                        } else {
                            true
                        };

                        if accept {
                            *ctx.pre_session_fallback = None;
                            *ctx.pre_session_fallback_hash = None;
                            let _ = send_info_string(log_tsv(&[
                                ("kind", "stop_pre_session_ok"),
                                ("us", &t0.elapsed().as_micros().to_string()),
                            ]));
                            // Inject final PV for pre_session to align with bestmove
                            let info = crate::usi::output::SearchInfo {
                                multipv: Some(1),
                                pv: vec![norm_a.clone()],
                                ..Default::default()
                            };
                            ctx.inject_final_pv(info, "stop_pre_session_early");
                            let meta =
                                build_meta(BestmoveSource::SessionOnStop, 0, None, None, None);
                            log_on_stop_source("pre_session");
                            ctx.emit_and_finalize(norm_a, None, meta, "EarlyPreSessionOnStop")?;
                            return Ok(());
                        }
                    } else {
                        let _ = send_info_string(log_tsv(&[
                            ("kind", "stop_pre_session_skip"),
                            ("reason", "normalize_failed"),
                        ]));
                    }
                }
            } else {
                let _ = send_info_string(log_tsv(&[
                    ("kind", "stop_pre_session_skip"),
                    ("reason", "hash_mismatch"),
                ]));
            }
        } else {
            let _ = send_info_string(log_tsv(&[
                ("kind", "stop_pre_session_skip"),
                ("reason", "adapter_lock_busy"),
            ]));
        }
        // 不採用の場合は削除（のちの正常パスで再計算・Emergencyへ）
        *ctx.pre_session_fallback = None;
        *ctx.pre_session_fallback_hash = None;
    }

    // Early: PositionState-based no-legal-move detection (lock-free)
    if let Some(state) = ctx.position_state.as_ref() {
        if let Ok(pos_verified) =
            engine_core::usi::restore_snapshot_and_verify(&state.sfen_snapshot, state.root_hash)
        {
            let mg = MoveGenerator::new();
            if let Ok(legal) = mg.generate_all(&pos_verified) {
                if legal.as_slice().is_empty() {
                    let _ = send_info_string(log_tsv(&[
                        ("kind", "stop_pre_session_skip"),
                        ("reason", "no_legal_moves"),
                    ]));
                    // Inject final PV for resign on stop
                    let info = crate::usi::output::SearchInfo {
                        multipv: Some(1),
                        pv: vec!["resign".to_string()],
                        ..Default::default()
                    };
                    ctx.inject_final_pv(info, "stop_no_legal_moves");
                    log_on_stop_source("emergency_resign");
                    let meta = build_meta(BestmoveSource::SessionOnStop, 0, None, None, None);
                    ctx.emit_and_finalize("resign".to_string(), None, meta, "StopNoLegalMoves")?;
                    return Ok(());
                }
            }
        }
    }

    // Non-ponder: prefer committed result if available
    if !*ctx.current_search_is_ponder {
        if let Some(committed) = ctx.current_committed.clone() {
            if ctx.emit_best_from_committed(
                &committed,
                BestmoveSource::SessionOnStop,
                None,
                "StopCommitted",
            )? {
                log_on_stop_source("committed");
                return Ok(());
            }
        }
    }

    // Non-ponder: emergency fallback (fast path) before central finalize
    if !*ctx.current_search_is_ponder {
        if let Ok((move_str, _)) =
            generate_fallback_move(ctx.engine, None, ctx.allow_null_move, true)
        {
            // Inject final PV so GUI PV aligns with bestmove
            let info = crate::usi::output::SearchInfo {
                multipv: Some(1),
                pv: vec![move_str.clone()],
                ..Default::default()
            };
            ctx.inject_final_pv(info, "stop_emergency_fast");
            log_on_stop_source("emergency");
            let meta = build_meta(BestmoveSource::SessionOnStop, 0, None, None, None);
            ctx.emit_and_finalize(move_str, None, meta, "ImmediateEmergencyOnStop")?;
            return Ok(());
        }
    }

    // Ponder stop: emit immediately for GUI compatibility
    if *ctx.current_search_is_ponder {
        *ctx.current_search_is_ponder = false;

        // 1) Committed iteration
        if let Some(committed) = ctx.current_committed.clone() {
            if ctx.emit_best_from_committed(
                &committed,
                BestmoveSource::SessionOnStop,
                None,
                "PonderCommittedOnStop",
            )? {
                log_on_stop_source("committed");
                return Ok(());
            }
        }

        // 2) Partial result
        if let Some((mv, d, s)) = ctx.last_partial_result.clone() {
            if let Ok((move_str, _)) =
                generate_fallback_move(ctx.engine, Some((mv, d, s)), ctx.allow_null_move, true)
            {
                // Inject a final info pv from partial result so PV aligns with bestmove
                let info = crate::usi::output::SearchInfo {
                    depth: Some(d as u32),
                    score: Some(crate::utils::to_usi_score(s)),
                    pv: vec![move_str.clone()],
                    ..Default::default()
                };
                ctx.inject_final_pv(info, "stop_partial_ponder");
                let meta = build_meta(
                    BestmoveSource::SessionOnStop,
                    d,
                    None,
                    Some(format!("cp {s}")),
                    None,
                );
                log_on_stop_source("partial");
                ctx.emit_and_finalize(move_str, None, meta, "PonderPartialOnStop")?;
                return Ok(());
            }
        }

        // 3) Pre-session fallback（ハッシュ一致時のみ使用。try_lockで非ブロッキング検査）
        if let Some(saved_move) = ctx.pre_session_fallback.clone() {
            if let Ok(adapter) = ctx.engine.try_lock() {
                let t0 = std::time::Instant::now();
                let current_hash = adapter.get_position().map(|p| p.zobrist_hash());
                if current_hash == *ctx.pre_session_fallback_hash {
                    if let Some(pos) = adapter.get_position() {
                        if let Some(norm_a) =
                            engine_core::util::usi_helpers::normalize_usi_move_str_logged(
                                pos,
                                &saved_move,
                            )
                        {
                            // State側でも同じ手を再正規化して一致を確認
                            if let Some(state) = ctx.position_state.as_ref() {
                                if let Ok(pos2) = engine_core::usi::restore_snapshot_and_verify(
                                    &state.sfen_snapshot,
                                    state.root_hash,
                                ) {
                                    // まず合法手が存在するかチェック
                                    let mg = MoveGenerator::new();
                                    if let Ok(legal) = mg.generate_all(&pos2) {
                                        if legal.as_slice().is_empty() {
                                            let _ = send_info_string(log_tsv(&[
                                                ("kind", "stop_pre_session_skip"),
                                                ("reason", "no_legal_moves"),
                                            ]));
                                            *ctx.pre_session_fallback = None;
                                            *ctx.pre_session_fallback_hash = None;
                                            // fall through to emergency below
                                        } else if let Some(norm_s) = engine_core::util::usi_helpers::normalize_usi_move_str_logged(&pos2, &saved_move)
                                        {
                                            let us = t0.elapsed().as_micros();
                                            if norm_a == norm_s && us <= 1000 {
                                                // 採用（Adapter/Stateで一致、≤1ms）
                                                *ctx.pre_session_fallback = None;
                                                *ctx.pre_session_fallback_hash = None;
                                                let _ = send_info_string(log_tsv(&[
                                                    ("kind", "stop_pre_session_ok"),
                                                    ("us", &us.to_string()),
                                                ]));
                                            // Inject final PV for pre_session (ponder)
                                            let info = crate::usi::output::SearchInfo {
                                                multipv: Some(1),
                                                pv: vec![norm_a.clone()],
                                                ..Default::default()
                                            };
                                            ctx.inject_final_pv(info, "stop_pre_session_ponder");
                                            let meta = build_meta(
                                                BestmoveSource::SessionOnStop,
                                                0,
                                                None,
                                                None,
                                                None,
                                                );
                                                log_on_stop_source("pre_session");
                                                ctx.emit_and_finalize(
                                                    norm_a,
                                                    None,
                                                    meta,
                                                    "PonderPreSessionOnStop",
                                                )?;
                                                return Ok(());
                                            } else if us > 1000 {
                                                let _ = send_info_string(log_tsv(&[
                                                    ("kind", "stop_pre_session_skip"),
                                                    ("reason", "recheck_slow"),
                                                    ("us", &us.to_string()),
                                                ]));
                                            } else {
                                                let _ = send_info_string(log_tsv(&[
                                                    ("kind", "stop_pre_session_skip"),
                                                    ("reason", "state_mismatch"),
                                                ]));
                                            }
                                        } else {
                                            let _ = send_info_string(log_tsv(&[
                                                ("kind", "stop_pre_session_skip"),
                                                ("reason", "normalize_failed_state"),
                                            ]));
                                        }
                                    }
                                }
                            } else {
                                // PositionState が無い場合は Adapter の結果を採用
                                *ctx.pre_session_fallback = None;
                                *ctx.pre_session_fallback_hash = None;
                                let _ = send_info_string(log_tsv(&[
                                    ("kind", "stop_pre_session_ok"),
                                    ("note", "state_absent"),
                                ]));
                                let meta =
                                    build_meta(BestmoveSource::SessionOnStop, 0, None, None, None);
                                log_on_stop_source("pre_session");
                                ctx.emit_and_finalize(
                                    norm_a,
                                    None,
                                    meta,
                                    "PonderPreSessionOnStop",
                                )?;
                                return Ok(());
                            }
                        } else {
                            let _ = send_info_string(log_tsv(&[
                                ("kind", "stop_pre_session_skip"),
                                ("reason", "normalize_failed"),
                            ]));
                        }
                    }
                } else {
                    let _ = send_info_string(log_tsv(&[
                        ("kind", "stop_pre_session_skip"),
                        ("reason", "hash_mismatch"),
                    ]));
                }
                // 不一致・不正なら削除
                *ctx.pre_session_fallback = None;
                *ctx.pre_session_fallback_hash = None;
            } else {
                // アダプタがビジーならプリセッションは使わず次へ（Emergency にフォールバック）
                let _ = send_info_string(log_tsv(&[
                    ("kind", "stop_pre_session_skip"),
                    ("reason", "adapter_lock_busy"),
                ]));
            }
        }

        // 4) Emergency fallback（PositionState優先でロック不要の生成を試す）
        let (move_str, from) = if let Some(state) = ctx.position_state.as_ref() {
            if let Some(m) = crate::helpers::emergency_move_from_state(state) {
                (m, BestmoveSource::SessionOnStop)
            } else {
                match generate_fallback_move(ctx.engine, None, ctx.allow_null_move, true) {
                    Ok((m, _)) => (m, BestmoveSource::SessionOnStop),
                    Err(_) => ("resign".to_string(), BestmoveSource::SessionOnStop),
                }
            }
        } else {
            match generate_fallback_move(ctx.engine, None, ctx.allow_null_move, true) {
                Ok((m, _)) => (m, BestmoveSource::SessionOnStop),
                Err(_) => ("resign".to_string(), BestmoveSource::SessionOnStop),
            }
        };
        // Inject final PV for emergency (ponder)
        let info = crate::usi::output::SearchInfo {
            multipv: Some(1),
            pv: vec![move_str.clone()],
            ..Default::default()
        };
        ctx.inject_final_pv(info, "stop_emergency_ponder");
        let meta = build_meta(from, 0, None, None, None);
        log_on_stop_source("emergency");
        ctx.emit_and_finalize(move_str, None, meta, "PonderEmergencyOnStop")?;
        return Ok(());
    }

    // Normal stop: emit immediately (committed → partial → pre_session → emergency)
    if let Some(committed) = ctx.current_committed.clone() {
        if ctx.emit_best_from_committed(
            &committed,
            BestmoveSource::SessionOnStop,
            None,
            "CommittedOnStop",
        )? {
            log_on_stop_source("committed");
            return Ok(());
        }
    }

    // Fallback: central finalize attempt for non-ponder (minimal guard)
    if !*ctx.current_search_is_ponder {
        let stop_info = engine_core::search::types::StopInfo {
            reason: engine_core::search::types::TerminationReason::UserStop,
            elapsed_ms: 0,
            nodes: 0,
            depth_reached: ctx.current_committed.as_ref().map(|c| c.depth).unwrap_or(0),
            hard_timeout: false,
            soft_limit_ms: 0,
            hard_limit_ms: 0,
        };
        if ctx.finalize_emit_if_possible("stop", Some(stop_info.clone()))? {
            return Ok(());
        }
        // As a last resort, ensure we emit a move and finalize
        let meta = build_meta(BestmoveSource::ResignOnFinish, 0, None, None, Some(stop_info));
        let info = crate::usi::output::SearchInfo {
            multipv: Some(1),
            pv: vec!["resign".to_string()],
            ..Default::default()
        };
        ctx.inject_final_pv(info, "stop_resign_minimal");
        ctx.emit_and_finalize("resign".to_string(), None, meta, "StopMinimalResign")?;
        let _ = send_info_string(log_tsv(&[("kind", "stop_end")]));
        return Ok(());
    }

    if let Some((mv, d, s)) = ctx.last_partial_result.clone() {
        if let Ok((move_str, _)) =
            generate_fallback_move(ctx.engine, Some((mv, d, s)), ctx.allow_null_move, true)
        {
            // Inject a final info pv from partial result so PV aligns with bestmove
            let info = crate::usi::output::SearchInfo {
                depth: Some(d as u32),
                score: Some(crate::utils::to_usi_score(s)),
                pv: vec![move_str.clone()],
                ..Default::default()
            };
            ctx.inject_final_pv(info, "stop_partial");
            let meta = build_meta(
                BestmoveSource::PartialResultTimeout,
                d,
                None,
                Some(format!("cp {s}")),
                None,
            );
            log_on_stop_source("partial");
            ctx.emit_and_finalize(move_str, None, meta, "ImmediatePartialOnStop")?;
            return Ok(());
        }
    }

    // Pre-session fallback（通常 stop でもハッシュ一致時のみ使用。try_lock で検査）
    if let Some(saved_move) = ctx.pre_session_fallback.clone() {
        if let Ok(adapter) = ctx.engine.try_lock() {
            let t0 = std::time::Instant::now();
            let current_hash = adapter.get_position().map(|p| p.zobrist_hash());
            if current_hash == *ctx.pre_session_fallback_hash {
                if let Some(pos) = adapter.get_position() {
                    if let Some(norm_a) =
                        engine_core::util::usi_helpers::normalize_usi_move_str_logged(
                            pos,
                            &saved_move,
                        )
                    {
                        // State側でも一致を確認
                        if let Some(state) = ctx.position_state.as_ref() {
                            if let Ok(pos2) = engine_core::usi::restore_snapshot_and_verify(
                                &state.sfen_snapshot,
                                state.root_hash,
                            ) {
                                let mg = MoveGenerator::new();
                                if let Ok(legal) = mg.generate_all(&pos2) {
                                    if legal.as_slice().is_empty() {
                                        let _ = send_info_string(log_tsv(&[("kind", "stop_pre_session_skip"), ("reason", "no_legal_moves")]));
                                        *ctx.pre_session_fallback = None;
                                        *ctx.pre_session_fallback_hash = None;
                                    } else if let Some(norm_s) = engine_core::util::usi_helpers::normalize_usi_move_str_logged(&pos2, &saved_move) {
                                        let us = t0.elapsed().as_micros();
                                        if norm_a == norm_s && us <= 1000 {
                                            *ctx.pre_session_fallback = None;
                                            *ctx.pre_session_fallback_hash = None;
                                            let _ = send_info_string(log_tsv(&[("kind", "stop_pre_session_ok"), ("us", &us.to_string())]));
                                            let meta = build_meta(BestmoveSource::SessionOnStop, 0, None, None, None);
                                            log_on_stop_source("pre_session");
                                            ctx.emit_and_finalize(norm_a, None, meta, "ImmediatePreSessionOnStop")?;
                                            return Ok(());
                                        } else if us > 1000 {
                                            let _ = send_info_string(log_tsv(&[("kind", "stop_pre_session_skip"), ("reason", "recheck_slow"), ("us", &us.to_string())]));
                                        } else {
                                            let _ = send_info_string(log_tsv(&[("kind", "stop_pre_session_skip"), ("reason", "state_mismatch")]));
                                        }
                                    } else {
                                        let _ = send_info_string(log_tsv(&[("kind", "stop_pre_session_skip"), ("reason", "normalize_failed_state")]));
                                    }
                                }
                            } else {
                                // PositionState が無い場合は Adapter の結果を採用
                                *ctx.pre_session_fallback = None;
                                *ctx.pre_session_fallback_hash = None;
                                let _ = send_info_string(log_tsv(&[
                                    ("kind", "stop_pre_session_ok"),
                                    ("note", "state_absent"),
                                ]));
                                // Inject final PV for pre_session immediate path
                                let info = crate::usi::output::SearchInfo {
                                    multipv: Some(1),
                                    pv: vec![norm_a.clone()],
                                    ..Default::default()
                                };
                                ctx.inject_final_pv(info, "stop_pre_session_immediate");
                                let meta =
                                    build_meta(BestmoveSource::SessionOnStop, 0, None, None, None);
                                log_on_stop_source("pre_session");
                                ctx.emit_and_finalize(
                                    norm_a,
                                    None,
                                    meta,
                                    "ImmediatePreSessionOnStop",
                                )?;
                                return Ok(());
                            }
                        }
                    } else {
                        let _ = send_info_string(log_tsv(&[
                            ("kind", "stop_pre_session_skip"),
                            ("reason", "normalize_failed"),
                        ]));
                    }
                }
            } else {
                let _ = send_info_string(log_tsv(&[
                    ("kind", "stop_pre_session_skip"),
                    ("reason", "hash_mismatch"),
                ]));
            }
            // 不一致・不正なら削除
            *ctx.pre_session_fallback = None;
            *ctx.pre_session_fallback_hash = None;
        } else {
            let _ = send_info_string(log_tsv(&[
                ("kind", "stop_pre_session_skip"),
                ("reason", "adapter_lock_busy"),
            ]));
        }
    }

    let (move_str, source) = if let Some(state) = ctx.position_state.as_ref() {
        if let Some(m) = crate::helpers::emergency_move_from_state(state) {
            (m, BestmoveSource::EmergencyFallbackTimeout)
        } else {
            // state側で合法手ゼロ → 必ず resign
            let _ = send_info_string(log_tsv(&[
                ("kind", "emergency_resign"),
                ("reason", "no_legal_moves"),
            ]));
            ("resign".to_string(), BestmoveSource::SessionOnStop)
        }
    } else {
        // Stateが無ければ従来どおり（ただしエラー時はresign）
        match generate_fallback_move(ctx.engine, None, false, true) {
            Ok((m, _)) => (m, BestmoveSource::EmergencyFallbackTimeout),
            Err(_) => ("resign".to_string(), BestmoveSource::SessionOnStop),
        }
    };
    // Inject final PV for emergency immediate path
    let info = crate::usi::output::SearchInfo {
        multipv: Some(1),
        pv: vec![move_str.clone()],
        ..Default::default()
    };
    ctx.inject_final_pv(info, "stop_emergency_immediate");
    log_on_stop_source("emergency");
    let meta = build_meta(source, 0, None, None, None);
    ctx.emit_and_finalize(move_str, None, meta, "ImmediateEmergencyOnStop")?;
    let _ = send_info_string(log_tsv(&[("kind", "stop_end")]));
    Ok(())
}
