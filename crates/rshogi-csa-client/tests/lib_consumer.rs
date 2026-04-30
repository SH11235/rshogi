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
    BestMoveResult, ConnectOpts, CsaClientConfig, CsaConnection, CsaTransport, Event, GameRecord,
    GameResult, GameSummary, RecordedMove, SearchInfo, SearchOutcome, TransportTarget, UsiEngine,
    run_game_session, run_resumed_session,
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
                         _: ConnectOpts| {};
    // 関数ポインタとしての参照を取得して値として捨てる (call はしない)。
    let consume_funcs: (fn(_, _, _, _) -> _, fn(_, _, _, _) -> _) =
        (run_game_session, run_resumed_session);

    // どちらも `_var` で未使用警告抑止せず、`let _ = ...` で値を消費する。
    let _ = (consume_types, consume_funcs);
}
