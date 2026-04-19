//! rshogi-csa-server — CSA プロトコル準拠の将棋対局サーバーのコアロジック。
//!
//! I/O には直接依存せず、[`port`] モジュールで定義された trait 群を介して
//! TCP 版と Cloudflare Workers 版の双方のフロントエンドから再利用できるよう
//! 設計されている。
//!
//! 現在のスコープは仕様書 `.kiro/specs/rshogi-csa-server/` の Phase 1 MVP。
//! Phase 2〜5 で想定されている Cloudflare Workers 対応、Floodgate 定期運用、
//! 再接続プロトコル等は段階的に導入する（スケジュールは `tasks.md` を参照）。

// Phase 3 defensive gate: `phase3-features` を有効にした依存グラフは、
// Phase 2 の受入 (tasks.md §9.7) が合格して Phase 3 実装が入るまで
// 本 compile_error! で全ビルドを停止する。フロントエンド crate 側の
// phase_gate.rs と二重で張り、誤って shared crate の feature だけを立てた
// 場合でも CI・ローカルで検知できるようにする。
#[cfg(feature = "phase3-features")]
compile_error!(
    "rshogi-csa-server: Phase 3 features are gated. Phase 2 acceptance (tasks.md §9.7) must \
     complete and both frontend phase_gate.rs modules must be updated before this feature \
     can be enabled."
);

pub mod error;
pub mod types;

pub mod game;
pub mod matching;
pub mod port;
pub mod protocol;
pub mod record;
pub mod storage;

pub use error::{ProtocolError, ServerError, StateError, StorageError, TransportError};
pub use game::clock::{ClockResult, SecondsCountdownClock, TimeClock};
pub use game::result::{GameResult, IllegalReason};
pub use game::room::{
    BroadcastEntry, BroadcastTarget, GameRoom, GameRoomConfig, GameStatus, HandleOutcome,
    HandleResult,
};
#[cfg(feature = "tokio-transport")]
pub use game::run_loop::run_room;
pub use game::validator::{KachiOutcome, RepetitionVerdict, Validator, Violation};
pub use matching::league::{League, LoginResult, MatchedPair, PairingCandidate, PlayerStatus};
pub use matching::pairing::{DirectMatchStrategy, PairingLogic};
pub use port::{
    BroadcastTag, Broadcaster, BuoyStorage, ClientTransport, KifuStorage, RateDecision, RateStorage,
};
pub use protocol::command::{ClientCommand, parse_command};
pub use protocol::summary::{GameSummaryBuilder, standard_initial_position_block};
pub use record::kifu::{
    KifuMove, KifuRecord, format_zerozero_list_line, illegal_reason_subcode, primary_result_code,
    winner_of,
};
#[cfg(feature = "tokio-transport")]
pub use storage::file::FileKifuStorage;
pub use types::{
    AdminId, Color, CsaLine, CsaMoveToken, GameId, GameName, IpKey, PlayerName, ReconnectToken,
    RoomId, Secret, StorageKey,
};
