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
    UsiEngineDriver, run_game_session, run_game_session_with_events, run_resumed_session,
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
    // 新 API は engine / sink の両方に対して generic なので、関数ポインタ型として
    // `&mut UsiEngine` 受けと `&mut dyn UsiEngineDriver` 受けの両形を確認する。
    // どちらも `run_game_session_with_events` / `run_resumed_session_with_events` の
    // monomorphize 経路を通り、consumer が自前の dyn dispatch を構築できることを保証する。
    type WithEventsFn = fn(
        &CsaClientConfig,
        &mut CsaConnection,
        &mut UsiEngine,
        std::sync::Arc<std::sync::atomic::AtomicBool>,
        &mut NoopSessionEventSink,
    ) -> Result<SessionOutcome, SessionError>;
    let events_funcs: (WithEventsFn, WithEventsFn) =
        (run_game_session_with_events, run_resumed_session_with_events);

    // `&mut dyn UsiEngineDriver` を `run_game_session_with_events` /
    // `run_resumed_session_with_events` に渡せることを build-only で確認する。
    // 関数ポインタへキャストすると `for<'a> &'a mut dyn Trait + 'a` と
    // 個別の lifetime ジェネリクス引数が衝突するため、call site の型推論で固定する。
    fn consume_dyn_engine_with_events(
        cfg: &CsaClientConfig,
        conn: &mut CsaConnection,
        engine: &mut dyn UsiEngineDriver,
        shutdown: std::sync::Arc<std::sync::atomic::AtomicBool>,
        sink: &mut dyn SessionEventSink,
    ) {
        if false {
            let _ = run_game_session_with_events(cfg, conn, engine, shutdown.clone(), sink);
            let _ = run_resumed_session_with_events(cfg, conn, engine, shutdown, sink);
        }
    }

    // `UsiEngine` が `UsiEngineDriver` を実装していることを type-level で固定する。
    // generic 関数 `takes::<UsiEngine>` が monomorphize できれば trait bound check が
    // 通っている証跡になる。値は使わず `assert_usi_engine_implements_driver` 関数として
    // 名前解決自体を `consume_funcs` 経由で参照させる。
    fn takes_engine<E: UsiEngineDriver + ?Sized>(_: &mut E) {}
    let assert_usi_engine_implements_driver: fn(&mut UsiEngine) = takes_engine::<UsiEngine>;

    // dyn-coercion 確認 (None でも `&mut dyn Trait` の型推論を強制)
    let dyn_sink_slot: Option<&mut dyn SessionEventSink> = None;
    let dyn_engine_slot: Option<&mut dyn UsiEngineDriver> = None;

    // 値そのものは使わず、`let _ = (...);` で discard することで未使用警告を抑える
    // (`_var` prefix の警告抑止ではなく、tuple 全体を pattern として _ に束縛する idiom)。
    let _ = (
        consume_types,
        consume_funcs,
        events_funcs,
        consume_dyn_engine_with_events,
        assert_usi_engine_implements_driver,
        dyn_sink_slot,
        dyn_engine_slot,
    );
}
