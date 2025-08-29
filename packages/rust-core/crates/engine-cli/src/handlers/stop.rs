use crate::command_handler::CommandContext;
use crate::emit_utils::log_tsv;
use crate::emit_utils::{build_meta, log_on_stop_source};
use crate::helpers::generate_fallback_move;
use crate::types::BestmoveSource;
use crate::usi::send_info_string;
use crate::worker::lock_or_recover_adapter;
use anyhow::Result;

pub(crate) fn handle_stop_command(ctx: &mut CommandContext) -> Result<()> {
    // If nothing to stop, return
    if !ctx.search_state.is_searching() {
        return Ok(());
    }

    // Signal stop to worker
    ctx.search_state.request_stop();
    if let Some(ref stop_flag) = *ctx.current_stop_flag {
        stop_flag.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    // Emit diagnostic snapshot for race analysis
    let diag = log_tsv(&[
        ("kind", "on_stop"),
        ("state", &format!("{:?}", *ctx.search_state)),
        (
            "ponder",
            if *ctx.current_search_is_ponder {
                "1"
            } else {
                "0"
            },
        ),
        (
            "session",
            if ctx.current_session.is_some() {
                "1"
            } else {
                "0"
            },
        ),
        (
            "partial",
            if ctx.last_partial_result.is_some() {
                "1"
            } else {
                "0"
            },
        ),
        (
            "pre_session_fallback",
            if ctx.pre_session_fallback.is_some() {
                "1"
            } else {
                "0"
            },
        ),
    ]);
    let _ = send_info_string(diag);

    // Ponder stop: emit immediately for GUI compatibility
    if *ctx.current_search_is_ponder {
        *ctx.current_search_is_ponder = false;

        // 1) Committed session
        if let Some(session) = ctx.current_session.clone() {
            if ctx.emit_best_from_session(
                &session,
                BestmoveSource::SessionOnStop,
                None,
                "PonderSessionOnStop",
            )? {
                log_on_stop_source("session");
                return Ok(());
            }
        }

        // 2) Partial result
        if let Some((mv, d, s)) = ctx.last_partial_result.clone() {
            if let Ok((move_str, _)) =
                generate_fallback_move(ctx.engine, Some((mv, d, s)), ctx.allow_null_move)
            {
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

        // 3) Pre-session fallback captured at go-time (with hash verification)
        if let Some(_move_str) = ctx.pre_session_fallback.as_ref() {
            // Verify hash matches current position
            let adapter = lock_or_recover_adapter(ctx.engine);
            let current_hash = adapter.get_position().map(|p| p.zobrist_hash());

            if current_hash == *ctx.pre_session_fallback_hash {
                let move_str = ctx.pre_session_fallback.take().unwrap();
                let meta = build_meta(BestmoveSource::SessionOnStop, 0, None, None, None);
                log_on_stop_source("pre_session");
                ctx.emit_and_finalize(move_str, None, meta, "PonderPreSessionOnStop")?;
                return Ok(());
            } else {
                log::debug!(
                    "Pre-session fallback hash mismatch: expected {:?}, got {:?}",
                    ctx.pre_session_fallback_hash,
                    current_hash
                );
                // Clear invalid fallback
                *ctx.pre_session_fallback = None;
                *ctx.pre_session_fallback_hash = None;
            }
        }

        // 4) Emergency fallback
        let (move_str, from) = match generate_fallback_move(ctx.engine, None, ctx.allow_null_move) {
            Ok((m, _)) => (m, BestmoveSource::SessionOnStop),
            Err(_) => ("resign".to_string(), BestmoveSource::SessionOnStop),
        };
        let meta = build_meta(from, 0, None, None, None);
        log_on_stop_source("emergency");
        ctx.emit_and_finalize(move_str, None, meta, "PonderEmergencyOnStop")?;
        return Ok(());
    }

    // Normal stop: emit immediately (session → partial → pre_session → emergency)
    if let Some(session) = ctx.current_session.clone() {
        if ctx.emit_best_from_session(
            &session,
            BestmoveSource::SessionOnStop,
            None,
            "SessionOnStop",
        )? {
            log_on_stop_source("session");
            return Ok(());
        }
    }

    if let Some((mv, d, s)) = ctx.last_partial_result.clone() {
        if let Ok((move_str, _)) =
            generate_fallback_move(ctx.engine, Some((mv, d, s)), ctx.allow_null_move)
        {
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

    // Pre-session fallback captured at go-time (with hash verification)
    if let Some(_move_str) = ctx.pre_session_fallback.as_ref() {
        // Verify hash matches current position
        let adapter = lock_or_recover_adapter(ctx.engine);
        let current_hash = adapter.get_position().map(|p| p.zobrist_hash());

        if current_hash == *ctx.pre_session_fallback_hash {
            let move_str = ctx.pre_session_fallback.take().unwrap();
            let meta = build_meta(BestmoveSource::SessionOnStop, 0, None, None, None);
            log_on_stop_source("pre_session");
            ctx.emit_and_finalize(move_str, None, meta, "ImmediatePreSessionOnStop")?;
            return Ok(());
        } else {
            log::debug!(
                "Pre-session fallback hash mismatch (normal stop): expected {:?}, got {:?}",
                ctx.pre_session_fallback_hash,
                current_hash
            );
            // Clear invalid fallback
            *ctx.pre_session_fallback = None;
            *ctx.pre_session_fallback_hash = None;
        }
    }

    let (move_str, source) = match generate_fallback_move(ctx.engine, None, ctx.allow_null_move) {
        Ok((m, _)) => (m, BestmoveSource::EmergencyFallbackTimeout),
        Err(_) => ("resign".to_string(), BestmoveSource::ResignTimeout),
    };
    log_on_stop_source("emergency");
    let meta = build_meta(source, 0, None, None, None);
    ctx.emit_and_finalize(move_str, None, meta, "ImmediateEmergencyOnStop")?;
    Ok(())
}
