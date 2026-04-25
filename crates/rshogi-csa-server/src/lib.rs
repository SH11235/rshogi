//! rshogi-csa-server — CSA プロトコル準拠の将棋対局サーバーのコアロジック。
//!
//! I/O には直接依存せず、[`port`] モジュールで定義された trait 群を介して
//! TCP 版と Cloudflare Workers 版の双方のフロントエンドから再利用できる。

pub mod config;
pub mod error;
pub mod types;

pub mod game;
pub mod matching;
pub mod port;
pub mod protocol;
pub mod record;
pub mod storage;

pub use config::{
    FloodgateFeatureIntent, parse_allow_floodgate_features, validate_floodgate_feature_gate,
};
pub use error::{ProtocolError, ServerError, StateError, StorageError, TransportError};
pub use game::clock::{
    ClockResult, ClockSpec, FischerClock, SecondsCountdownClock, StopWatchClock, TimeClock,
};
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
pub use matching::registry::{GameListing, GameRegistry};
pub use port::{
    BroadcastTag, Broadcaster, BuoyStorage, ClientTransport, KifuStorage, RateDecision, RateStorage,
};
pub use protocol::command::{ClientCommand, parse_command};
pub use protocol::info::{help_lines, list_lines, show_lines, version_lines, who_lines};
pub use protocol::summary::{GameSummaryBuilder, standard_initial_position_block};
pub use record::kifu::{
    KifuMove, KifuRecord, fork_initial_sfen_from_kifu, format_zerozero_list_line,
    illegal_reason_subcode, initial_sfen_from_csa_moves, primary_result_code, winner_of,
};
#[cfg(feature = "tokio-transport")]
pub use storage::buoy::FileBuoyStorage;
#[cfg(feature = "tokio-transport")]
pub use storage::file::FileKifuStorage;
#[cfg(feature = "tokio-transport")]
pub use storage::players_yaml::PlayersYamlRateStorage;
pub use types::{
    AdminId, Color, CsaLine, CsaMoveToken, GameId, GameName, IpKey, PlayerName, ReconnectToken,
    RoomId, Secret, StorageKey,
};
