//! `SessionEventSink` 周辺の event 型・helper 関数のユニットテスト。
//!
//! 全フロー (Connected → ... → Disconnected) を mock CSA + mock USI engine で
//! 駆動するのは別 integration test (`session_events_integration.rs`) で行う。
//! 本ファイルは公開 API 型と helper の挙動を確定させるためのテスト。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use rshogi_csa_client::events::{
    DisconnectReason, GameEndReason, MovePlayer, NoopSessionEventSink, SearchInfoEmitPolicy,
    SearchInfoSnapshot, SearchOrigin, SessionError, SessionEventSink, SessionProgress, Side,
    SinkError,
};

#[test]
fn search_info_emit_policy_default_returns_default_variant() {
    let p = SearchInfoEmitPolicy::default();
    assert!(matches!(p, SearchInfoEmitPolicy::Default));
}

#[test]
fn search_info_emit_policy_default_is_documented_to_match_interval_preset() {
    // doc 上は `Interval { min_ms: 200, emit_on_depth_change: true, emit_final: true }`
    // 相当と明記している。Default variant 自体は別 enum なので enum 同値性は弱いが、
    // build-time に意味がずれないよう、Default variant を直接構築できることを確認する。
    let _ = SearchInfoEmitPolicy::Interval {
        min_ms: 200,
        emit_on_depth_change: true,
        emit_final: true,
    };
}

#[test]
fn side_from_color_round_trips() {
    use rshogi_csa::Color;
    assert_eq!(Side::from(Color::Black), Side::Black);
    assert_eq!(Side::from(Color::White), Side::White);
}

#[test]
fn noop_sink_returns_ok_for_all_events() {
    let mut sink = NoopSessionEventSink;
    assert!(sink.on_event(SessionProgress::Connected).is_ok());
    assert!(sink.on_event(SessionProgress::GameStarted).is_ok());
    assert!(
        sink.on_event(SessionProgress::SearchInfo(SearchInfoSnapshot::default()))
            .is_ok()
    );
    assert!(
        sink.on_event(SessionProgress::Disconnected {
            reason: DisconnectReason::GameOver
        })
        .is_ok()
    );
    // default on_error returns Ok
    let err = SessionError::Shutdown;
    assert!(sink.on_error(&err).is_ok());
    // default should_continue is true
    assert!(sink.should_continue());
}

/// `FnMut(SessionProgress) -> Result<(), SinkError>` が
/// `SessionEventSink` の blanket impl で sink として使えることを確認する。
#[test]
fn closure_is_session_event_sink() {
    let collected = Arc::new(Mutex::new(Vec::<&'static str>::new()));
    let collected_clone = Arc::clone(&collected);
    let mut sink = move |event: SessionProgress| -> Result<(), SinkError> {
        let mut g = collected_clone.lock().unwrap();
        g.push(match event {
            SessionProgress::Connected => "connected",
            SessionProgress::GameStarted => "started",
            SessionProgress::Disconnected { .. } => "disconnected",
            _ => "other",
        });
        Ok(())
    };
    sink.on_event(SessionProgress::Connected).unwrap();
    sink.on_event(SessionProgress::GameStarted).unwrap();
    sink.on_event(SessionProgress::Disconnected {
        reason: DisconnectReason::GameOver,
    })
    .unwrap();
    let g = collected.lock().unwrap();
    assert_eq!(g.as_slice(), &["connected", "started", "disconnected"]);
}

/// `SessionError` は `io::Error` を `Network` に分類する `From<anyhow::Error>` を持つ。
#[test]
fn session_error_from_anyhow_with_io_error_is_network() {
    let io_err = std::io::Error::new(std::io::ErrorKind::ConnectionReset, "test connection reset");
    let any_err: anyhow::Error = io_err.into();
    let session_err: SessionError = any_err.into();
    assert!(
        matches!(session_err, SessionError::Network(_)),
        "io::Error should map to SessionError::Network, got: {session_err:?}"
    );
}

#[test]
fn session_error_from_anyhow_without_io_error_is_other() {
    let any_err: anyhow::Error = anyhow::anyhow!("custom protocol error");
    let session_err: SessionError = any_err.into();
    assert!(
        matches!(session_err, SessionError::Other(_)),
        "non-io anyhow should map to SessionError::Other, got: {session_err:?}"
    );
}

/// `SinkError::Fatal` / `NonFatal` を `Display` で確認できる。
#[test]
fn sink_error_display_includes_kind() {
    let inner: Box<dyn std::error::Error + Send + Sync> = Box::new(std::io::Error::other("boom"));
    let fatal = SinkError::Fatal(inner);
    let s = format!("{fatal}");
    assert!(s.contains("fatal"), "display should mention fatal: {s}");

    let inner: Box<dyn std::error::Error + Send + Sync> = Box::new(std::io::Error::other("warn"));
    let non_fatal = SinkError::NonFatal(inner);
    let s = format!("{non_fatal}");
    assert!(s.contains("non-fatal"), "display should mention non-fatal: {s}");
}

#[test]
fn search_origin_variants_are_distinct() {
    let fresh = SearchOrigin::Fresh;
    let pondhit = SearchOrigin::Ponderhit;
    let miss = SearchOrigin::PonderMiss;
    assert_ne!(fresh, pondhit);
    assert_ne!(pondhit, miss);
    assert_ne!(fresh, miss);
}

#[test]
fn move_player_variants_are_distinct() {
    assert_ne!(MovePlayer::SelfPlayer, MovePlayer::Opponent);
}

#[test]
fn game_end_reason_unknown_preserves_payload() {
    let r = GameEndReason::Unknown("#FUTURE_REASON_X".to_owned());
    if let GameEndReason::Unknown(s) = r {
        assert_eq!(s, "#FUTURE_REASON_X");
    } else {
        panic!("expected Unknown variant");
    }
}

#[test]
fn shutdown_signal_via_arc_atomic_bool_flag_only() {
    // `Arc<AtomicBool>` を `run_*_with_events` に渡す前段の動作確認。
    // 実 session driver は別テスト。ここでは値が共有 Arc 経由で読み書きできること
    // のみを確認する。
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_clone = Arc::clone(&shutdown);
    std::thread::spawn(move || {
        shutdown_clone.store(true, Ordering::SeqCst);
    })
    .join()
    .unwrap();
    assert!(shutdown.load(Ordering::SeqCst));
}
