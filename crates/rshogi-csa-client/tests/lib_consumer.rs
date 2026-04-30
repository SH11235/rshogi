//! crate root の `pub use` シンボルが build target として壊れていないことを保証する
//! build-only smoke test。
//!
//! consumer (例: Tauri 製 frontend) から `use rshogi_csa_client::{...};` で参照
//! することを想定しているシンボルを 1 箇所で名前解決させ、`cargo build --tests`
//! の段階で壊れたら検出する。実 IO や engine spawn は行わない。
//!
//! TCP only / WebSocket only など複数 feature 構成でも build pass させたいので、
//! WS 専用シンボル (`CsaTransport::WebSocket` バリアント等) は参照しない。

use rshogi_csa_client::{
    BestMoveEvent, BestMoveResult, ConnectOpts, CsaClientConfig, CsaConnection, CsaTransport,
    DisconnectReason, Event, GameEndEvent, GameEndReason, GameRecord, GameResult, GameSummary,
    MoveEvent, MovePlayer, NoopSessionEventSink, ReconnectState, RecordedMove, SearchInfo,
    SearchInfoEmitPolicy, SearchInfoSnapshot, SearchOrigin, SearchOutcome, SessionError,
    SessionEventSink, SessionOutcome, SessionProgress, Side, SinkError, TransportTarget, UsiEngine,
    run_game_session, run_game_session_with_events, run_resumed_session,
    run_resumed_session_with_events,
};

/// build only: 上の `use` がそのまま resolve できれば pass。型を実体化したり
/// 関数を呼んだりはしない。pub 型 / 関数を `&dyn`-相当の参照位置に並べることで
/// 未使用警告を防ぎつつシンボル名前解決を強制する。
#[test]
fn build_only() {
    // 型を引数として要求するクロージャを式値として `let _ = ...;` 経由で
    // 評価することで、上の `use` で取り込んだ型がコンパイル単位に組み込まれる
    // ことを保証する。クロージャ自体は呼ばない。
    let consume_types = |_: CsaClientConfig,
                         _: UsiEngine,
                         _: BestMoveResult,
                         _: SearchOutcome,
                         _: SearchInfo,
                         _: Event,
                         _: CsaConnection,
                         _: GameSummary,
                         _: GameRecord,
                         _: RecordedMove,
                         _: GameResult,
                         _: CsaTransport,
                         _: TransportTarget,
                         _: ConnectOpts,
                         _: BestMoveEvent,
                         _: DisconnectReason,
                         _: GameEndEvent,
                         _: GameEndReason,
                         _: MoveEvent,
                         _: MovePlayer,
                         _: NoopSessionEventSink,
                         _: ReconnectState,
                         _: SearchInfoEmitPolicy,
                         _: SearchInfoSnapshot,
                         _: SearchOrigin,
                         _: SessionError,
                         _: SessionOutcome,
                         _: SessionProgress,
                         _: Side,
                         _: SinkError| {};
    let consume_funcs: (fn(_, _, _, _) -> _, fn(_, _, _, _) -> _) =
        (run_game_session, run_resumed_session);
    // 新 API は generic なので関数ポインタ型にキャストせず、`run_game_session_with_events`
    // / `run_resumed_session_with_events` の名前解決だけ強制する。
    type WithEventsFn = fn(
        &CsaClientConfig,
        &mut CsaConnection,
        &mut UsiEngine,
        std::sync::Arc<std::sync::atomic::AtomicBool>,
        &mut NoopSessionEventSink,
    ) -> Result<SessionOutcome, SessionError>;
    let _events_funcs: (WithEventsFn, WithEventsFn) =
        (run_game_session_with_events, run_resumed_session_with_events);
    // dyn-coercion 確認
    let _: Option<&mut dyn SessionEventSink> = None;
    let _ = (consume_types, consume_funcs);
}
