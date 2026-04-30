//! `SessionEventSink` API: 対局途中の進捗を consumer (例: Tauri 製 frontend) に
//! push 通知するためのイベント型・trait・error 型を提供する。
//!
//! # 概要
//!
//! [`run_game_session_with_events`] / [`run_resumed_session_with_events`] に
//! [`SessionEventSink`] 実装を渡すと、対局ループの各イベント
//! ([`SessionProgress`]) が逐次 [`SessionEventSink::on_event`] に流れる。
//!
//! ## sink 内の長時間処理について
//!
//! sink の `on_event` は対局メインループのスレッド上で同期実行される。重い処理
//! (DB write / network 越しの publish 等) を直接行うと、対局ループが遅延し
//! USI engine の探索開始や CSA サーバへの応答にも影響する。consumer は sink 内
//! では軽量な channel 送信だけを行い、重い処理は別 thread / async runtime に
//! 委譲することを推奨する。
//!
//! ## sink エラーの扱い
//!
//! - [`SinkError::NonFatal`]: warn ログを出して対局を継続する。`on_error` は
//!   呼ばれない (= sink 自身が一時的不具合を許容している扱い)。
//! - [`SinkError::Fatal`]: 対局を中断し、CSA `%CHUDAN` → `LOGOUT` →
//!   transport close → `on_error(&SessionError::SinkAborted(..))` →
//!   `Disconnected { reason: SinkAborted }` を best-effort attempt at clean
//!   closure として実行する。`run_*` 関数は [`SessionError::SinkAborted`] で
//!   return する。
//!
//! `run_*` は best-effort attempt at clean closure を行うが、CSA server 側で
//! 「正常切断」確定となることは保証しない。`%CHUDAN` 送信直後に transport を
//! 閉じるため、server が確定処理を完了する前に切断される可能性がある。
//!
//! ## resume 時の event 順序
//!
//! 通常対局:
//!
//! ```text
//! Connected → GameSummary → GameStarted → 通常対局
//! ```
//!
//! 再接続成立後の resume:
//!
//! ```text
//! Connected → Resumed { summary, state } → GameStarted → 通常対局
//! ```
//!
//! resume 経路では指し手 history の replay は行わない。CSA reconnect protocol
//! は切断時点の局面 (`GameSummary.position_section`) のみを送信し、それまでの
//! 指し手列を再送しないため。consumer は [`ReconnectState::last_sfen`] から
//! 盤面を再構築すること。指し手単位の history が必要な場合は consumer 側で
//! local 永続化を行う。
//!
//! ## SearchInfoSnapshot の semantics
//!
//! [`SearchInfoSnapshot`] は対局ループが USI `info` 行を観測する都度 **累積
//! snapshot** として更新する値である (depth / score / pv 等は最後に観測した
//! 値で上書き)。`emit_on_depth_change` や `emit_final` のフラグはこの累積値を
//! いつ [`SessionProgress::SearchInfo`] として発火するかを制御するだけで、
//! snapshot の中身そのものは差分ではなく常に「現時点までで最も新しい値の集合」
//! を表す。
//!
//! ## rustls CryptoProvider に関する注意
//!
//! crate 全体の注意事項として、`websocket` feature 有効時は consumer 側 `main`
//! で `rustls::crypto::ring::default_provider().install_default()` を 1 度だけ
//! 呼ぶ必要がある。詳細は crate root の doc を参照。

use std::sync::Arc;

use crate::protocol::{GameResult, GameSummary};
use crate::record::GameRecord;

// ────────────────────────────────────────────
// Side / MovePlayer / SearchOrigin / 単純 enum 群
// ────────────────────────────────────────────

/// 先手 / 後手。CSA `+` / `-` の象徴的な再エクスポート。
///
/// `rshogi_csa::Color` と semantically 等価だが、event payload の public API
/// 専用に純粋な定義を持たせて consumer (例: Tauri 製 frontend) が
/// `rshogi_csa` を取り込まずに済むようにしている。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// 先手 (CSA `+`)
    Black,
    /// 後手 (CSA `-`)
    White,
}

impl From<rshogi_csa::Color> for Side {
    fn from(color: rshogi_csa::Color) -> Self {
        match color {
            rshogi_csa::Color::Black => Side::Black,
            rshogi_csa::Color::White => Side::White,
        }
    }
}

/// その手を指したのが自エンジンか相手かを区別する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MovePlayer {
    /// 自エンジンが指した手
    SelfPlayer,
    /// 対戦相手 (CSA サーバから受信した) の手
    Opponent,
}

/// 自エンジンが指した手の探索が、どの種類の探索から得られたかを示す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchOrigin {
    /// 自分の手番開始時点で position+go を新規に走らせた fresh search の結果。
    /// 直前に ponder が走っていない通常パスの探索。
    Fresh,
    /// 相手の手が ponder 予測と一致し、`ponderhit` を送って継続した探索の結果。
    Ponderhit,
    /// ponder miss 後に開始した fresh search の結果。`Fresh` と探索アルゴリズム的
    /// には同じだが、直前の ponder 探索は外れて discard 済みであるため、UI 側は
    /// 「ponder が外れて生まれた fresh search」として通常の `Fresh` と区別できる。
    PonderMiss,
}

// ────────────────────────────────────────────
// Reconnect / Disconnect / GameEnd 系 payload
// ────────────────────────────────────────────

/// `Resumed` event に同梱される、再接続時の局面と残時間情報。
///
/// `last_sfen` は resume 時点の **完全な局面** を SFEN として表現したもので、
/// CSA サーバから受信した `GameSummary.position_section` を OSS lib 側で
/// 変換した結果である。consumer は履歴 replay を期待してはいけない (CSA
/// reconnect protocol が指し手列を再送しないため)。指し手単位の history が
/// 必要なら consumer 側で local 永続化を行うこと。
#[derive(Debug, Clone)]
pub struct ReconnectState {
    /// 切断時点までに進んだ ply 数 (再接続後に次に指す手の `ply - 1`)。
    pub last_ply: u32,
    /// 切断時点の **完全な局面** を SFEN として表現したもの。consumer はこの
    /// SFEN から盤面を再構築する。指し手 history は含まれない。
    pub last_sfen: String,
    /// 切断時点での手番。
    pub side_to_move: Side,
    /// 切断時点での自エンジン側残時間 (秒)。サーバが値を提供しないとき `None`。
    pub remaining_time_sec_self: Option<u32>,
    /// 切断時点での相手側残時間 (秒)。サーバが値を提供しないとき `None`。
    pub remaining_time_sec_opp: Option<u32>,
}

/// 対局終了 (`GameEnded`) event の payload。
#[derive(Debug, Clone)]
pub struct GameEndEvent {
    /// CSA サーバから受信した最終結果。
    pub result: GameResult,
    /// 終局理由 (#TIME_UP / #ILLEGAL_MOVE 等から推定)。
    pub reason: GameEndReason,
    /// 勝者。引き分け / 中断時は `None`。
    pub winner: Option<Side>,
    /// `#WIN` / `#LOSE` / `#DRAW` 等の生の最終結果行 (サーバが送ってきた文字列)。
    pub raw_result_line: Option<String>,
    /// `#TIME_UP` / `#ILLEGAL_MOVE` 等、最終結果行の直前に来た終局理由行 (あれば)。
    pub raw_reason_line: Option<String>,
}

/// CSA サーバ側の終局理由。`raw_reason_line` を字句解析した結果を normalize した分類。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GameEndReason {
    /// 投了 (`%TORYO`)
    Resign,
    /// 時間切れ (`#TIME_UP`)
    TimeUp,
    /// 禁手 (`#ILLEGAL_MOVE`)
    IllegalMove,
    /// 持将棋 (`#JISHOGI`)
    Jishogi,
    /// 千日手 (`#SENNICHITE`)
    Sennichite,
    /// 最大手数到達 (`#MAX_MOVES`)
    MaxMoves,
    /// 検閲・反則扱い (`#CENSORED`)
    Censored,
    /// `#CHUDAN` 等の中断扱い
    Interrupted,
    /// 接続断などサーバ側理由不明の切断
    OtherDisconnect,
    /// 上記いずれにも該当しない理由文字列 (前方互換)
    Unknown(String),
}

/// `Disconnected` event の理由分類。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisconnectReason {
    /// 通常の対局終了に伴う切断 (`GameEnded` を emit した後)。
    GameOver,
    /// 外部からの shutdown 要求 (`AtomicBool` 経由) による中断。
    Shutdown,
    /// sink が `Fatal` を返したことによる中断。
    SinkAborted,
    /// transport レベルのエラー (TCP / WebSocket I/O 失敗等)。
    TransportError(String),
    /// 上記以外の理由不明な切断。
    Unknown,
}

// ────────────────────────────────────────────
// SearchInfo / Move / BestMove payload
// ────────────────────────────────────────────

/// USI `info` 行から抽出した探索情報の累積 snapshot。
///
/// 対局ループが USI `info` 行を観測する都度、最後に観測した値で各 field を
/// 上書きする (= 累積 snapshot)。各 field は USI engine が送ってこない場合
/// `None` のまま。`pv` は最後に観測した PV 一式。
///
/// この snapshot は [`SessionProgress::SearchInfo`] / [`BestMoveEvent::search`]
/// / [`MoveEvent::search`] で同型として使われる。
#[derive(Debug, Clone, Default)]
pub struct SearchInfoSnapshot {
    /// `depth N`
    pub depth: Option<u32>,
    /// `seldepth N`
    pub seldepth: Option<u32>,
    /// `score cp N` (mate と排他)。
    pub score_cp: Option<i32>,
    /// `score mate N` (cp と排他)。
    pub mate: Option<i32>,
    /// `nodes N`
    pub nodes: Option<u64>,
    /// `nps N`
    pub nps: Option<u64>,
    /// `time N` (ms)
    pub time_ms: Option<u64>,
    /// `pv ...` の USI 表記列。
    pub pv: Vec<String>,
    /// 最後に観測した生の `info` 行 (デバッグ・raw 表示用)。
    pub raw_line: Option<String>,
}

/// `SearchInfo` event の発火頻度ポリシー。`CsaClientConfig` 経由で設定する。
#[derive(Debug, Clone)]
pub enum SearchInfoEmitPolicy {
    /// `SearchInfo` を一切発火しない。consumer 側の表示が不要な運用向け。
    Disabled,
    /// Library が推奨する preset (`Interval { min_ms: 200, emit_on_depth_change: true, emit_final: true }`
    /// 相当)。preset の具体内容は将来の lib バージョンで調整され得るため、
    /// 固定挙動が必要なら [`SearchInfoEmitPolicy::Interval`] を直接指定すること。
    Default,
    /// 観測した `info` 行を 1 行ごとに発火する。USI engine が大量に出すと
    /// sink への push が高頻度になるため注意。
    EveryLine,
    /// 時間間隔と depth 変化で発火を制御する。
    Interval {
        /// 直前 emit からの最低経過時間 (ms)。0 のとき間隔制限なし。
        min_ms: u32,
        /// `depth` field が変化した時は `min_ms` を待たずに emit する。
        emit_on_depth_change: bool,
        /// bestmove 直前の最後の累積値を必ず emit する。
        emit_final: bool,
    },
}

impl Default for SearchInfoEmitPolicy {
    /// `SearchInfoEmitPolicy::Default` (library 推奨 preset) を返す。
    fn default() -> Self {
        Self::Default
    }
}

/// 自エンジンが指した手の event payload。`SessionProgress::BestMoveSelected`
/// として、USI engine から bestmove が返り CSA 文字列に変換された直後に
/// 発火する (CSA サーバへの送信前)。
#[derive(Debug, Clone)]
pub struct BestMoveEvent {
    /// USI 形式の bestmove (`7g7f` 等)。`resign` / `win` 等の特殊値は
    /// [`SessionProgress::BestMoveSelected`] では発火させない (代わりに対局
    /// 終了系の event を発火する)。
    pub usi_move: String,
    /// CSA 形式に変換した bestmove (`+7776FU` 等)。USI -> CSA 変換に失敗した場合
    /// `None` (例: `resign` / `win` / 不正手等)。
    pub csa_move_candidate: Option<String>,
    /// USI engine が返した ponder 予測手 (USI 表記)。なければ `None`。
    pub ponder: Option<String>,
    /// この手を指した側 (= 自エンジン) の手番。
    pub side: Side,
    /// この手の手数 (1 始まり、適用後の手数ではなくこの手自身の番号)。
    pub ply: u32,
    /// この bestmove を生み出した探索の種別。
    pub search_origin: SearchOrigin,
    /// この bestmove を出した時点の累積 [`SearchInfoSnapshot`]。
    /// USI engine から `info` 行が 1 度も来なかった場合は `None`。
    pub search: Option<SearchInfoSnapshot>,
}

/// 1 手分の指し手 event payload。`MoveSent` (自エンジンが送出した手) と
/// `MoveConfirmed` (サーバから confirm された手) の両方で同型として使われる。
///
/// - 自エンジンの手の場合 (`player == SelfPlayer`):
///     - `MoveSent`: CSA サーバへ送信した直後に発火。
///     - `MoveConfirmed`: サーバが `+...` / `-...` echo を返してきた時に発火 (時間が確定する)。
/// - 相手の手の場合 (`player == Opponent`):
///     - `MoveConfirmed` のみ発火 (`MoveSent` は発火しない)。
#[derive(Debug, Clone)]
pub struct MoveEvent {
    /// この手を指したのが自エンジンか相手か。
    pub player: MovePlayer,
    /// CSA 形式の指し手 (`+7776FU` 等)。
    pub csa_move: String,
    /// USI 形式の指し手 (`7g7f` 等)。
    pub usi_move: String,
    /// この手を指した側の手番。
    pub side: Side,
    /// この手の手数 (1 始まり)。
    pub ply: u32,
    /// この手の消費時間 (秒)。`MoveSent` 時は `None`、`MoveConfirmed` 時は
    /// サーバ報告値があれば `Some`。
    pub time_sec: Option<u32>,
    /// この手を指す前の SFEN。
    pub sfen_before: String,
    /// この手を適用した後の SFEN。
    pub sfen_after: String,
    /// 自エンジンが指した手の場合のみ `Some`。相手の手は `None`。
    pub search_origin: Option<SearchOrigin>,
    /// 自エンジンが指した手の場合のみ `Some` (探索情報 snapshot)。相手の手は `None`。
    pub search: Option<SearchInfoSnapshot>,
}

// ────────────────────────────────────────────
// SessionProgress (発火される event 本体)
// ────────────────────────────────────────────

/// 対局途中の進捗を 1 件で表す event。
///
/// 通常対局の発火順:
///
/// ```text
/// Connected
///   → GameSummary
///   → GameStarted
///   → (BestMoveSelected → MoveSent → MoveConfirmed)*  // 自エンジンの手番
///   → (MoveConfirmed)*                                // 相手の手番
///   → (SearchInfo)*                                   // 任意の頻度で挟まる
///   → GameEnded
///   → Disconnected
/// ```
///
/// resume の場合は `GameSummary` の代わりに `Resumed { summary, state }` が
/// 1 度だけ発火し、その後の流れは通常対局と同じ。詳細は本モジュールの
/// crate-level doc を参照。
#[derive(Debug, Clone)]
pub enum SessionProgress {
    /// CSA transport の物理接続 + LOGIN 成功直後の marker。Game_Summary 受信前。
    Connected,
    /// `BEGIN Game_Summary` ... `END Game_Summary` を受信し終えた時点で 1 度発火。
    GameSummary(Arc<GameSummary>),
    /// 再接続成立後 (`run_resumed_session_with_events`) に 1 度発火し、resume 用
    /// の Game_Summary と Reconnect_State を同梱する。詳細は [`ReconnectState`]
    /// の doc を参照。
    Resumed {
        /// resume 用に再受信した Game_Summary。`Reconnect_Token` も含まれる。
        summary: Arc<GameSummary>,
        /// 切断時点の局面・残時間。
        state: ReconnectState,
    },
    /// 対局メインループに入る直前の marker。通常対局では `START` 受信後、
    /// resume では `Reconnect_State` 受信後にそれぞれ 1 度発火する。
    GameStarted,
    /// USI engine が bestmove を返し CSA に変換できた時に発火 (CSA サーバへの
    /// 送信前)。`resign` / `win` 等の特殊値は本 event では発火させない。
    BestMoveSelected(BestMoveEvent),
    /// 自エンジンの手を CSA サーバへ送信した直後に発火。
    MoveSent(MoveEvent),
    /// CSA サーバから手の echo (`+7776FU,T30` 等) を受信した時に発火。自エンジンの
    /// 手と相手の手の両方で発火する (相手の手は `MoveSent` を伴わない)。
    MoveConfirmed(MoveEvent),
    /// USI `info` 行を観測したとき、emit policy に基づいて発火する累積 snapshot。
    SearchInfo(SearchInfoSnapshot),
    /// `#WIN` / `#LOSE` / `#DRAW` / `#CHUDAN` / `#CENSORED` 等の最終結果行を
    /// 受信した時に発火。
    GameEnded(GameEndEvent),
    /// transport を閉じた直後の最終 event。すべてのケース (正常終了 / shutdown /
    /// sink fatal / transport error) でこの event が最後に 1 度発火する。
    Disconnected {
        /// 切断理由。
        reason: DisconnectReason,
    },
}

// ────────────────────────────────────────────
// SessionOutcome / SessionEventSink / Errors
// ────────────────────────────────────────────

/// `run_*_session*` の戻り値。将来 field 追加耐性のため struct 化している。
///
/// 既存呼び出し側は `outcome.result` / `outcome.record` / `outcome.summary` を
/// 直接参照すること (タプル分解は不可)。
#[derive(Debug)]
pub struct SessionOutcome {
    /// CSA サーバ側の最終結果。
    pub result: GameResult,
    /// 蓄積した棋譜。
    pub record: GameRecord,
    /// 対局中に受信した Game_Summary (resume 時は新 token 付きの再受信版)。
    /// 接続自体に失敗した等で受信前に終了した場合 `None`。
    pub summary: Option<GameSummary>,
}

/// 対局途中の進捗を受け取る consumer 用 trait。
///
/// `on_event` は対局メインループ thread から同期呼び出しされる。重い処理を直接
/// 行うと対局ループを遅らせるため、軽量な channel 送信のみを行うこと。詳細は
/// 本モジュールの crate-level doc を参照。
pub trait SessionEventSink {
    /// 1 件の event を処理する。
    ///
    /// 戻り値:
    /// - `Ok(())`: 通常通り対局を継続する。
    /// - `Err(SinkError::NonFatal(_))`: warn ログを出して対局を継続する。
    /// - `Err(SinkError::Fatal(_))`: 対局を中断する (best-effort attempt at clean closure)。
    fn on_event(&mut self, event: SessionProgress) -> Result<(), SinkError>;

    /// session が abort / shutdown された際に best-effort で 1 度呼ばれる。
    /// default 実装は no-op。戻り値は呼び出し側で warn ログのみに使われ、
    /// 対局フローには影響しない。
    fn on_error(&mut self, _error: &SessionError) -> Result<(), SinkError> {
        Ok(())
    }

    /// 対局ループ各イテレーションの先頭で呼ばれ、`false` を返すと sink fatal と
    /// 同等の `best-effort attempt at clean closure` をトリガする。default は `true`。
    fn should_continue(&self) -> bool {
        true
    }
}

/// 何もしない sink。`run_game_session` / `run_resumed_session` (event なし版) が
/// 内部的に渡す default。
pub struct NoopSessionEventSink;

impl SessionEventSink for NoopSessionEventSink {
    fn on_event(&mut self, _event: SessionProgress) -> Result<(), SinkError> {
        Ok(())
    }
}

impl<F> SessionEventSink for F
where
    F: FnMut(SessionProgress) -> Result<(), SinkError>,
{
    fn on_event(&mut self, event: SessionProgress) -> Result<(), SinkError> {
        (self)(event)
    }
}

/// sink が返す error 型。`thiserror` 由来で `Send + Sync` な inner error を抱える。
#[derive(thiserror::Error, Debug)]
pub enum SinkError {
    /// 対局を継続させたい一時的なエラー。warn ログのみ出される。
    #[error("sink: non-fatal: {0}")]
    NonFatal(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// 対局を中断させたい致命的エラー。`best-effort attempt at clean closure` をトリガする。
    #[error("sink: fatal: {0}")]
    Fatal(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// `run_*_session_with_events` が返す public error 型。
///
/// 既存の `anyhow::Error` 経路も `Other(anyhow::Error)` として包んで返す。
#[derive(thiserror::Error, Debug)]
pub enum SessionError {
    /// transport / IO 系エラー (TCP / WebSocket 切断、書き込み失敗等)。
    #[error("network: {0}")]
    Network(#[source] std::io::Error),
    /// CSA プロトコルレベルの異常応答 (Game_Summary 不正等)。
    #[error("protocol: {0}")]
    Protocol(String),
    /// USI engine 側のエラー (子プロセス異常終了、応答 timeout 等)。
    #[error("engine: {0}")]
    Engine(String),
    /// 外部 shutdown 要求 (`AtomicBool::store(true, _)`) による中断。
    #[error("shutdown")]
    Shutdown,
    /// sink が `Fatal` を返したことによる中断。
    #[error("sink aborted")]
    SinkAborted(#[source] SinkError),
    /// 上記分類に当てはめられない包括的エラー (`anyhow` から包んだもの)。
    #[error("other: {0}")]
    Other(#[source] anyhow::Error),
}

impl From<anyhow::Error> for SessionError {
    fn from(err: anyhow::Error) -> Self {
        // `io::Error` を内部に持つなら Network に分類する。
        if let Some(io_err) = err.downcast_ref::<std::io::Error>() {
            // io_err は move 不可なため、kind とメッセージから等価な io::Error を生成する。
            return SessionError::Network(std::io::Error::new(io_err.kind(), io_err.to_string()));
        }
        SessionError::Other(err)
    }
}
