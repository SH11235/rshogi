//! `SessionEventSink` 周辺の event 型・helper 関数のユニットテスト。
//!
//! 全フロー (Connected → ... → Disconnected) を mock CSA + mock USI engine で
//! 駆動するのは別 integration test (`session_events_integration.rs`) で行う。
//! 本ファイルは公開 API 型と helper の挙動を確定させるためのテスト。

use std::sync::{Arc, Mutex};

use rshogi_csa_client::events::{
    DisconnectReason, NoopSessionEventSink, SearchInfoEmitPolicy, SessionError, SessionEventSink,
    SessionProgress, Side, SinkError,
};

#[test]
fn search_info_emit_policy_default_returns_default_variant() {
    let p = SearchInfoEmitPolicy::default();
    assert!(matches!(p, SearchInfoEmitPolicy::Default));
}

#[test]
fn side_from_color_round_trips() {
    use rshogi_csa::Color;
    assert_eq!(Side::from(Color::Black), Side::Black);
    assert_eq!(Side::from(Color::White), Side::White);
}

#[test]
fn noop_sink_accepts_events_and_uses_default_control() {
    let mut sink = NoopSessionEventSink;
    assert!(sink.on_event(SessionProgress::Connected).is_ok());
    assert!(sink.on_error(&SessionError::Shutdown).is_ok());
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
