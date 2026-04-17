//! 1 対局のライフサイクル全体を駆動する `GameRoom`。
//!
//! - I/O は行わず、外部から `handle_line` で 1 行ずつ駆動される（Requirement 8.3）。
//! - 関係者へ送るべき行は [`HandleResult::broadcasts`] に積まれて返り、フロントエンド
//!   が `Broadcaster` 経由で実配信する設計（Phase 1 MVP）。
//! - 状態機械は `AgreeWaiting → StartWaiting → Playing → Finished` の単調遷移
//!   （Requirement 2.1, 2.4）。

use std::fmt;

use rshogi_core::position::{Position, SFEN_HIRATE};
use rshogi_core::types::EnteringKingRule;

use crate::error::{ProtocolError, ServerError, StateError};
use crate::game::clock::{ClockResult, TimeClock};
use crate::game::result::{GameResult, IllegalReason};
use crate::game::validator::{KachiOutcome, RepetitionVerdict, Validator, Violation};
use crate::protocol::command::{ClientCommand, parse_command};
use crate::types::{Color, CsaLine, CsaMoveToken, GameId, PlayerName};

/// 対局ルームの状態機械（Phase 1 で使用する 4 状態）。
///
/// 設計書 §GameRoom State Management で示されている `AgreeWaiting → StartWaiting →
/// Playing → Finished` の単調遷移をそのまま表現する。`StartWaiting` は片方が AGREE
/// 済みで相方の AGREE を待つ状態。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GameStatus {
    /// マッチ成立直後、双方の AGREE 待ち。
    AgreeWaiting,
    /// 片方 AGREE 済み、相方の AGREE 待ち。
    StartWaiting {
        /// 既に AGREE を送ってきた側。
        agreed_by: Color,
    },
    /// 双方 AGREE 完了、対局進行中。
    Playing,
    /// 終局確定。最終結果を保持する。
    Finished(GameResult),
}

/// `BroadcastEntry::target` の宛先区分。
///
/// 各受信者は自分が属するカテゴリ宛のエントリだけを 1 回受け取る前提で
/// フロントエンドがフィルタする（Requirement 4.7 の「受信者ごとに 1 回ずつ
/// 理由→勝敗」を満たすため、宛先は重複しない区分にしている）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BroadcastTarget {
    /// 先手対局者だけに送る。
    Black,
    /// 後手対局者だけに送る。
    White,
    /// 両対局者に送る（観戦者は含めない）。
    Players,
    /// 観戦者だけに送る（Phase 3 で実体化、Phase 1 では実体ゼロでも経路だけ用意）。
    Spectators,
    /// 両対局者 + 同一ルームの全観戦者に送る（引き分け・無勝負時の同報）。
    All,
}

/// `HandleResult` が返す 1 行分の送信指示。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BroadcastEntry {
    /// 宛先区分。
    pub target: BroadcastTarget,
    /// 送る生 CSA テキスト（末尾改行はフロントエンドで付ける）。
    pub line: CsaLine,
}

/// `GameRoom::handle_line` の 1 件分の戻り値。
///
/// 発生した状態遷移を [`HandleOutcome`] で示し、関係者に送る行列を `broadcasts`
/// に積んで返す。フロントエンドは `broadcasts` を順序通り配信したのち、`outcome`
/// を見て次の挙動（次行の受信、終局確定処理など）を決める。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandleResult {
    /// 主たる結果（状態遷移カテゴリ）。
    pub outcome: HandleOutcome,
    /// 配信指示の順序付きリスト。空でもよい。
    pub broadcasts: Vec<BroadcastEntry>,
}

/// `handle_line` の状態遷移カテゴリ（設計書 §HandleOutcome に対応）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandleOutcome {
    /// 入力を受け付けたが状態は変えない（空行 keep-alive、AGREE 1 件目など）。
    Continue,
    /// 双方 AGREE が揃い、対局を開始した。`broadcasts` に `START:<game_id>` が積まれる。
    GameStarted,
    /// 指し手を受理した。次手番情報と現在の残時間を返す。
    MoveAccepted {
        /// 次に手を指す対局者。
        next_turn: Color,
        /// 次手番側の残時間 (ms)。
        remaining_ms: i64,
    },
    /// 終局確定。`broadcasts` に終局理由 → 勝敗コード列が積まれる。
    GameEnded(GameResult),
}

/// `GameRoom` 構築時の不変パラメータ。
pub struct GameRoomConfig {
    /// 対局 ID（`20140101123000` 形式等）。
    pub game_id: GameId,
    /// 先手プレイヤ名。
    pub black: PlayerName,
    /// 後手プレイヤ名。
    pub white: PlayerName,
    /// 最大手数（既定 256）。これに達したら `#MAX_MOVES`（Requirement 4.4）。
    pub max_moves: u32,
    /// 通信マージン（ミリ秒）。`consume` 呼び出し前に減算される（Requirement 3.6）。
    pub time_margin_ms: u64,
    /// `%KACHI` 判定に使う入玉ルール（Phase 1 既定は `Point24`）。
    pub entering_king_rule: EnteringKingRule,
}

impl fmt::Debug for GameRoomConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GameRoomConfig")
            .field("game_id", &self.game_id)
            .field("black", &self.black)
            .field("white", &self.white)
            .field("max_moves", &self.max_moves)
            .field("time_margin_ms", &self.time_margin_ms)
            .field("entering_king_rule", &self.entering_king_rule)
            .finish()
    }
}

/// 1 対局のライフサイクルを所有する状態機械。
pub struct GameRoom {
    config: GameRoomConfig,
    pos: Position,
    clock: Box<dyn TimeClock>,
    validator: Validator,
    status: GameStatus,
    moves_played: u32,
    /// 現在の手番が開始した瞬間の単調時刻（ミリ秒）。`Playing` 中のみ意味を持つ。
    turn_started_at_ms: Option<u64>,
}

impl fmt::Debug for GameRoom {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GameRoom")
            .field("config", &self.config)
            .field("status", &self.status)
            .field("moves_played", &self.moves_played)
            .field("turn_started_at_ms", &self.turn_started_at_ms)
            .finish()
    }
}

impl GameRoom {
    /// 平手初期局面で対局ルームを構築する。
    ///
    /// 駒落ちやブイは Phase 4 で別 API を用意する想定。
    pub fn new(config: GameRoomConfig, clock: Box<dyn TimeClock>) -> Self {
        let mut pos = Position::new();
        // SFEN_HIRATE は const なので set_sfen は失敗し得ない。万一失敗した場合は
        // ライブラリ側の不具合なので panic で即時検知する。
        pos.set_sfen(SFEN_HIRATE).expect("SFEN_HIRATE must be valid");
        let validator = Validator::new(config.entering_king_rule);
        Self {
            config,
            pos,
            clock,
            validator,
            status: GameStatus::AgreeWaiting,
            moves_played: 0,
            turn_started_at_ms: None,
        }
    }

    /// 現在の状態。
    pub fn status(&self) -> &GameStatus {
        &self.status
    }

    /// 内部 `Position`（観戦応答や Game_Summary 生成での読み取り用）。
    pub fn position(&self) -> &Position {
        &self.pos
    }

    /// 既消費手数。
    pub fn moves_played(&self) -> u32 {
        self.moves_played
    }

    /// 指定側の現在残時間（ミリ秒）。`run_loop` で時計切れアラームを設定する際に使う。
    pub fn clock_remaining_ms(&self, color: Color) -> i64 {
        self.clock.remaining_ms(color)
    }

    /// 設定済みの通信マージン（ミリ秒）。`run_loop` の `compute_deadline` から参照される。
    pub fn time_margin_ms(&self) -> u64 {
        self.config.time_margin_ms
    }

    /// 単調時刻 `now_ms` における 1 行入力を処理する。
    ///
    /// `from` は物理的に「どの対局者が送ってきたか」。手番外の指し手はここで弾き、
    /// 千日手・最大手数・時間切れの判定もこの内部で行う。
    pub fn handle_line(
        &mut self,
        from: Color,
        line: &CsaLine,
        now_ms: u64,
    ) -> Result<HandleResult, ServerError> {
        // 終局後の呼び出しは契約違反（Postcondition）。状態機械を内部不変条件として弾く。
        if let GameStatus::Finished(_) = &self.status {
            return Err(ServerError::State(StateError::InvalidForState {
                current: format!("{:?}", self.status),
            }));
        }

        let cmd = parse_command(line)?;
        match cmd {
            ClientCommand::KeepAlive => Ok(HandleResult {
                outcome: HandleOutcome::Continue,
                broadcasts: Vec::new(),
            }),
            ClientCommand::Agree { game_id } => {
                self.verify_game_id(game_id.as_ref())?;
                self.handle_agree(from, now_ms)
            }
            ClientCommand::Reject { game_id } => {
                self.verify_game_id(game_id.as_ref())?;
                self.handle_reject(from)
            }
            ClientCommand::Move { token, .. } => self.handle_move(from, &token, now_ms),
            ClientCommand::Toryo => self.handle_toryo(from),
            ClientCommand::Kachi => self.handle_kachi(from),
            ClientCommand::Chudan => self.handle_chudan(from),
            // LOGIN/LOGOUT は接続ハンドラ側の責務。GameRoom には到達しない想定。
            ClientCommand::Login { .. } | ClientCommand::Logout => {
                Err(ServerError::State(StateError::InvalidForState {
                    current: format!("{:?}", self.status),
                }))
            }
            // Phase 3 の x1 拡張コマンドは Phase 1 では受け付けない。
            other => {
                Err(ServerError::Protocol(ProtocolError::X1NotEnabled(command_static_name(&other))))
            }
        }
    }

    /// 外部タイマーが時間切れを検出したときに呼ぶ。
    ///
    /// `loser` は時間を使い切った側。`Playing` 状態でのみ有効で、それ以外で呼ばれた
    /// 場合は内部不変条件違反として `Internal` エラーを返さず、no-op で `Continue` を
    /// 返す（Phase 1 ではタイマーの spurious 起動を許容する）。
    pub fn force_time_up(&mut self, loser: Color) -> HandleResult {
        if !matches!(self.status, GameStatus::Playing) {
            return HandleResult {
                outcome: HandleOutcome::Continue,
                broadcasts: Vec::new(),
            };
        }
        let result = GameResult::TimeUp { loser };
        self.finish(result)
    }

    /// 切断を検出したときに呼ぶ。Phase 1 は再接続猶予 0 秒なので即時 `#ABNORMAL` 確定。
    ///
    /// 勝者確定は Requirement 2.5 の「対局中の切断」に限り、それ以前
    /// (`AgreeWaiting`/`StartWaiting`) は対局未成立扱いで `winner: None`。
    pub fn force_abnormal(&mut self, disconnected: Color) -> HandleResult {
        let winner = match self.status {
            GameStatus::Playing => Some(disconnected.opposite()),
            GameStatus::AgreeWaiting | GameStatus::StartWaiting { .. } => None,
            GameStatus::Finished(_) => {
                return HandleResult {
                    outcome: HandleOutcome::Continue,
                    broadcasts: Vec::new(),
                };
            }
        };
        self.finish(GameResult::Abnormal { winner })
    }

    fn verify_game_id(&self, requested: Option<&GameId>) -> Result<(), ServerError> {
        let Some(req) = requested else {
            return Ok(());
        };
        if req != &self.config.game_id {
            return Err(ServerError::State(StateError::GameIdMismatch {
                expected: self.config.game_id.to_string(),
                actual: req.to_string(),
            }));
        }
        Ok(())
    }

    fn handle_agree(&mut self, from: Color, now_ms: u64) -> Result<HandleResult, ServerError> {
        match &self.status {
            GameStatus::AgreeWaiting => {
                self.status = GameStatus::StartWaiting { agreed_by: from };
                Ok(HandleResult {
                    outcome: HandleOutcome::Continue,
                    broadcasts: Vec::new(),
                })
            }
            GameStatus::StartWaiting { agreed_by } => {
                if *agreed_by == from {
                    // 同じ側からの 2 度目の AGREE は無視せずプロトコルエラーにする。
                    return Err(ServerError::State(StateError::InvalidForState {
                        current: format!("{:?}", self.status),
                    }));
                }
                self.status = GameStatus::Playing;
                self.moves_played = 0;
                self.turn_started_at_ms = Some(now_ms);
                let line = CsaLine::new(format!("START:{}", self.config.game_id));
                Ok(HandleResult {
                    outcome: HandleOutcome::GameStarted,
                    broadcasts: vec![BroadcastEntry {
                        target: BroadcastTarget::Players,
                        line,
                    }],
                })
            }
            other => Err(ServerError::State(StateError::InvalidForState {
                current: format!("{other:?}"),
            })),
        }
    }

    fn handle_reject(&mut self, from: Color) -> Result<HandleResult, ServerError> {
        if matches!(self.status, GameStatus::AgreeWaiting | GameStatus::StartWaiting { .. }) {
            // Requirement 1.5: REJECT は対局不成立を双方に通知して終了。
            // CSA 仕様上は `#ABNORMAL` を送らないため、ここでは finish() ではなく
            // 専用経路で `REJECT:<game_id> by <rejector>` のみ配信する。
            // 内部状態は Finished(Abnormal{None}) を流用（GameResult enum を増やさず
            // Phase 1 を済ませるための割り切り）。
            let line = CsaLine::new(format!(
                "REJECT:{} by {}",
                self.config.game_id,
                player_name_of(self, from)
            ));
            let result = GameResult::Abnormal { winner: None };
            self.status = GameStatus::Finished(result.clone());
            self.turn_started_at_ms = None;
            Ok(HandleResult {
                outcome: HandleOutcome::GameEnded(result),
                broadcasts: vec![BroadcastEntry {
                    target: BroadcastTarget::Players,
                    line,
                }],
            })
        } else {
            Err(ServerError::State(StateError::InvalidForState {
                current: format!("{:?}", self.status),
            }))
        }
    }

    fn handle_move(
        &mut self,
        from: Color,
        token: &CsaMoveToken,
        now_ms: u64,
    ) -> Result<HandleResult, ServerError> {
        if !matches!(self.status, GameStatus::Playing) {
            return Err(ServerError::State(StateError::InvalidForState {
                current: format!("{:?}", self.status),
            }));
        }
        // 手番判定（Requirement 2.3）。手番外からの指し手はプロトコルエラーで拒否し、
        // 状態は変更しない。
        let core_side: rshogi_core::types::Color = from.into();
        if core_side != self.pos.side_to_move() {
            return Err(ServerError::Protocol(ProtocolError::Malformed(format!(
                "out-of-turn move from {from:?}"
            ))));
        }

        match self.validator.validate_move(&self.pos, token) {
            Ok(mv) => self.apply_move(from, token, mv, now_ms),
            Err(violation) => {
                // 構文・手番不一致は protocol error（状態変更なし）。
                // それ以外の合法性違反は反則負けとして終局。
                match violation {
                    Violation::Malformed(msg) => {
                        Err(ServerError::Protocol(ProtocolError::Malformed(msg)))
                    }
                    Violation::WrongTurn { .. } => Err(ServerError::Protocol(
                        ProtocolError::Malformed("CSA token side prefix mismatch".to_owned()),
                    )),
                    other => {
                        let reason = match other {
                            Violation::Uchifuzume => IllegalReason::Uchifuzume,
                            _ => IllegalReason::Generic,
                        };
                        Ok(self.finish(GameResult::IllegalMove {
                            loser: from,
                            reason,
                        }))
                    }
                }
            }
        }
    }

    fn apply_move(
        &mut self,
        from: Color,
        token: &CsaMoveToken,
        mv: rshogi_core::types::Move,
        now_ms: u64,
    ) -> Result<HandleResult, ServerError> {
        // 1. 経過時間を計算し通信マージンを差し引いて時計を消費（Requirement 3.3, 3.6）。
        let started = self.turn_started_at_ms.unwrap_or(now_ms);
        let raw_elapsed_ms = now_ms.saturating_sub(started);
        let elapsed_ms = raw_elapsed_ms.saturating_sub(self.config.time_margin_ms);
        let clock_result = self.clock.consume(from, elapsed_ms);

        // 2. 時間切れなら盤面を進めず終局（手は受理しない）。
        if matches!(clock_result, ClockResult::TimeUp) {
            return Ok(self.finish(GameResult::TimeUp { loser: from }));
        }

        // 3. 局面を進める。
        let gives_check = self.pos.gives_check(mv);
        self.pos.do_move(mv, gives_check);
        self.moves_played += 1;
        let elapsed_sec = elapsed_ms / 1000;

        // 4. 関係者に `<token>,T<sec>` を配信（Requirement 1.6）。
        let mut broadcasts = vec![BroadcastEntry {
            target: BroadcastTarget::All,
            line: CsaLine::new(format!("{},T{}", token.as_str(), elapsed_sec)),
        }];

        // 5. 千日手・連続王手千日手判定（Requirement 4.2, 4.3）。
        match self.validator.classify_repetition(&self.pos) {
            RepetitionVerdict::None => {}
            RepetitionVerdict::Sennichite => {
                let mut result = self.finish(GameResult::Sennichite);
                broadcasts.append(&mut result.broadcasts);
                return Ok(HandleResult {
                    outcome: result.outcome,
                    broadcasts,
                });
            }
            RepetitionVerdict::OuteSennichiteLose => {
                // 連続王手していた側（=直前の手番=from）が反則負け。
                let mut result = self.finish(GameResult::OuteSennichite { loser: from });
                broadcasts.append(&mut result.broadcasts);
                return Ok(HandleResult {
                    outcome: result.outcome,
                    broadcasts,
                });
            }
            RepetitionVerdict::OuteSennichiteWin => {
                // 連続王手されていた側（直前の手番）が勝ち＝相手（手番外）の反則負け。
                let mut result = self.finish(GameResult::OuteSennichite {
                    loser: from.opposite(),
                });
                broadcasts.append(&mut result.broadcasts);
                return Ok(HandleResult {
                    outcome: result.outcome,
                    broadcasts,
                });
            }
        }

        // 6. 最大手数到達判定（Requirement 4.4）。
        if self.moves_played >= self.config.max_moves {
            let mut result = self.finish(GameResult::MaxMoves);
            broadcasts.append(&mut result.broadcasts);
            return Ok(HandleResult {
                outcome: result.outcome,
                broadcasts,
            });
        }

        // 7. 続行 → 次手番開始時刻を更新。
        self.turn_started_at_ms = Some(now_ms);
        let next_turn = from.opposite();
        let core_next: rshogi_core::types::Color = next_turn.into();
        let remaining_ms = self.clock.remaining_ms(core_next.into());
        Ok(HandleResult {
            outcome: HandleOutcome::MoveAccepted {
                next_turn,
                remaining_ms,
            },
            broadcasts,
        })
    }

    fn handle_toryo(&mut self, from: Color) -> Result<HandleResult, ServerError> {
        if !matches!(self.status, GameStatus::Playing) {
            return Err(ServerError::State(StateError::InvalidForState {
                current: format!("{:?}", self.status),
            }));
        }
        Ok(self.finish(GameResult::Toryo {
            winner: from.opposite(),
        }))
    }

    fn handle_kachi(&mut self, from: Color) -> Result<HandleResult, ServerError> {
        if !matches!(self.status, GameStatus::Playing) {
            return Err(ServerError::State(StateError::InvalidForState {
                current: format!("{:?}", self.status),
            }));
        }
        let core_side: rshogi_core::types::Color = from.into();
        if core_side != self.pos.side_to_move() {
            return Err(ServerError::Protocol(ProtocolError::Malformed(format!(
                "out-of-turn %KACHI from {from:?}"
            ))));
        }
        match self.validator.evaluate_kachi(&self.pos) {
            KachiOutcome::Accepted => Ok(self.finish(GameResult::Kachi { winner: from })),
            KachiOutcome::Rejected => Ok(self.finish(GameResult::IllegalMove {
                loser: from,
                reason: IllegalReason::IllegalKachi,
            })),
        }
    }

    fn handle_chudan(&mut self, _from: Color) -> Result<HandleResult, ServerError> {
        // %CHUDAN は対局中断。Phase 1 では `#ABNORMAL`（勝者なし）として終局する。
        if !matches!(self.status, GameStatus::Playing) {
            return Err(ServerError::State(StateError::InvalidForState {
                current: format!("{:?}", self.status),
            }));
        }
        Ok(self.finish(GameResult::Abnormal { winner: None }))
    }

    fn finish(&mut self, result: GameResult) -> HandleResult {
        // result.server_messages() の順序通りに BroadcastEntry を組む（Requirement 4.7）。
        let messages = result.server_messages();
        let mut broadcasts = Vec::new();
        for (audience, lines) in &messages.sends {
            let target = match audience {
                crate::game::result::Audience::Winner => match result.winner() {
                    Some(Color::Black) => BroadcastTarget::Black,
                    Some(Color::White) => BroadcastTarget::White,
                    None => BroadcastTarget::Players,
                },
                crate::game::result::Audience::Loser => match result.winner() {
                    Some(Color::Black) => BroadcastTarget::White,
                    Some(Color::White) => BroadcastTarget::Black,
                    None => BroadcastTarget::Players,
                },
                crate::game::result::Audience::Spectator => BroadcastTarget::Spectators,
                crate::game::result::Audience::All => BroadcastTarget::All,
            };
            for line in lines {
                broadcasts.push(BroadcastEntry {
                    target,
                    line: CsaLine::new(line.clone()),
                });
            }
        }
        self.status = GameStatus::Finished(result.clone());
        self.turn_started_at_ms = None;
        HandleResult {
            outcome: HandleOutcome::GameEnded(result),
            broadcasts,
        }
    }
}

fn player_name_of(room: &GameRoom, color: Color) -> &PlayerName {
    match color {
        Color::Black => &room.config.black,
        Color::White => &room.config.white,
    }
}

/// 対応していない x1 拡張コマンドの static 名前をエラーへ載せる。
fn command_static_name(cmd: &ClientCommand) -> &'static str {
    match cmd {
        ClientCommand::Who => "%%WHO",
        ClientCommand::List => "%%LIST",
        ClientCommand::Show { .. } => "%%SHOW",
        ClientCommand::Monitor2On { .. } => "%%MONITOR2ON",
        ClientCommand::Monitor2Off { .. } => "%%MONITOR2OFF",
        ClientCommand::Chat { .. } => "%%CHAT",
        ClientCommand::Version => "%%VERSION",
        ClientCommand::Help => "%%HELP",
        ClientCommand::SetBuoy { .. } => "%%SETBUOY",
        ClientCommand::DeleteBuoy { .. } => "%%DELETEBUOY",
        ClientCommand::GetBuoyCount { .. } => "%%GETBUOYCOUNT",
        ClientCommand::Fork { .. } => "%%FORK",
        _ => "<unknown>",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::clock::SecondsCountdownClock;

    fn make_room() -> GameRoom {
        let config = GameRoomConfig {
            game_id: GameId::new("20140101120000"),
            black: PlayerName::new("alice"),
            white: PlayerName::new("bob"),
            max_moves: 256,
            time_margin_ms: 0,
            entering_king_rule: EnteringKingRule::Point24,
        };
        let clock = Box::new(SecondsCountdownClock::new(60, 5));
        GameRoom::new(config, clock)
    }

    fn line(s: &str) -> CsaLine {
        CsaLine::new(s)
    }

    fn agree_both(room: &mut GameRoom) -> HandleResult {
        let _ = room.handle_line(Color::Black, &line("AGREE"), 0).unwrap();
        room.handle_line(Color::White, &line("AGREE"), 0).unwrap()
    }

    #[test]
    fn agree_then_start_emits_start_line() {
        let mut room = make_room();
        let r1 = room.handle_line(Color::Black, &line("AGREE"), 0).unwrap();
        assert_eq!(r1.outcome, HandleOutcome::Continue);
        assert!(r1.broadcasts.is_empty());

        let r2 = room.handle_line(Color::White, &line("AGREE"), 0).unwrap();
        assert_eq!(r2.outcome, HandleOutcome::GameStarted);
        assert_eq!(r2.broadcasts.len(), 1);
        assert_eq!(r2.broadcasts[0].target, BroadcastTarget::Players);
        assert_eq!(r2.broadcasts[0].line.as_str(), "START:20140101120000");
        assert!(matches!(room.status(), GameStatus::Playing));
    }

    #[test]
    fn move_accepted_broadcasts_with_elapsed_time() {
        let mut room = make_room();
        agree_both(&mut room);
        // 3000ms 経過後に +7776FU を投げる → broadcast `+7776FU,T3`
        let r = room.handle_line(Color::Black, &line("+7776FU"), 3_000).unwrap();
        assert!(matches!(
            r.outcome,
            HandleOutcome::MoveAccepted {
                next_turn: Color::White,
                ..
            }
        ));
        assert_eq!(r.broadcasts.len(), 1);
        assert_eq!(r.broadcasts[0].target, BroadcastTarget::All);
        assert_eq!(r.broadcasts[0].line.as_str(), "+7776FU,T3");
    }

    #[test]
    fn rejects_out_of_turn_move() {
        let mut room = make_room();
        agree_both(&mut room);
        // 後手から先手の手番中に -3334FU → out-of-turn protocol error
        let err = room.handle_line(Color::White, &line("-3334FU"), 1_000).unwrap_err();
        assert!(matches!(err, ServerError::Protocol(ProtocolError::Malformed(_))));
        // 状態は不変
        assert!(matches!(room.status(), GameStatus::Playing));
    }

    #[test]
    fn toryo_ends_with_resign_messages_in_order() {
        let mut room = make_room();
        agree_both(&mut room);
        let r = room.handle_line(Color::Black, &line("%TORYO"), 1_000).unwrap();
        match &r.outcome {
            HandleOutcome::GameEnded(GameResult::Toryo {
                winner: Color::White,
            }) => {}
            other => panic!("unexpected outcome: {other:?}"),
        }
        // Requirement 4.7: 受信者ごとに 1 回ずつ「理由 → 勝敗」が届く。
        // 宛先は Winner=White / Loser=Black / Spectators の 3 系列で、各 2 行 = 計 6 行。
        assert_eq!(r.broadcasts.len(), 6);
        let by_target = |t: BroadcastTarget| -> Vec<String> {
            r.broadcasts
                .iter()
                .filter(|b| b.target == t)
                .map(|b| b.line.as_str().to_owned())
                .collect()
        };
        assert_eq!(by_target(BroadcastTarget::White), vec!["#RESIGN", "#WIN"]);
        assert_eq!(by_target(BroadcastTarget::Black), vec!["#RESIGN", "#LOSE"]);
        assert_eq!(by_target(BroadcastTarget::Spectators), vec!["#RESIGN", "#WIN"]);
    }

    #[test]
    fn illegal_move_ends_game_as_loser() {
        let mut room = make_room();
        agree_both(&mut room);
        // 先手の不可能な手 (+9988UM 等は src に駒なし) → 反則負け
        let r = room.handle_line(Color::Black, &line("+5544FU"), 1_000).unwrap();
        match &r.outcome {
            HandleOutcome::GameEnded(GameResult::IllegalMove {
                loser: Color::Black,
                reason: IllegalReason::Generic,
            }) => {}
            other => panic!("unexpected outcome: {other:?}"),
        }
        assert!(r.broadcasts.iter().any(|b| b.line.as_str() == "#ILLEGAL_MOVE"));
    }

    #[test]
    fn time_up_when_elapsed_exceeds_total() {
        let mut room = make_room();
        agree_both(&mut room);
        // 60 秒 + 5 秒 = 65 秒 持つので 70 秒経過後の手で TimeUp。
        let r = room.handle_line(Color::Black, &line("+7776FU"), 70_000).unwrap();
        match &r.outcome {
            HandleOutcome::GameEnded(GameResult::TimeUp {
                loser: Color::Black,
            }) => {}
            other => panic!("unexpected outcome: {other:?}"),
        }
    }

    #[test]
    fn time_margin_is_subtracted_before_consume() {
        let config = GameRoomConfig {
            game_id: GameId::new("g"),
            black: PlayerName::new("a"),
            white: PlayerName::new("b"),
            max_moves: 256,
            time_margin_ms: 1_500,
            entering_king_rule: EnteringKingRule::Point24,
        };
        let clock = Box::new(SecondsCountdownClock::new(60, 0));
        let mut room = GameRoom::new(config, clock);
        agree_both(&mut room);
        // 経過 4000ms, margin 1500ms → consume(2500ms)。整数秒切り捨てで 2 秒消費。
        let r = room.handle_line(Color::Black, &line("+7776FU"), 4_000).unwrap();
        assert_eq!(r.broadcasts[0].line.as_str(), "+7776FU,T2");
    }

    #[test]
    fn keep_alive_does_not_change_state() {
        let mut room = make_room();
        let r = room.handle_line(Color::Black, &line(""), 0).unwrap();
        assert_eq!(r.outcome, HandleOutcome::Continue);
        assert!(matches!(room.status(), GameStatus::AgreeWaiting));
    }

    #[test]
    fn second_agree_from_same_side_is_protocol_error() {
        let mut room = make_room();
        room.handle_line(Color::Black, &line("AGREE"), 0).unwrap();
        let err = room.handle_line(Color::Black, &line("AGREE"), 0).unwrap_err();
        assert!(matches!(err, ServerError::State(StateError::InvalidForState { .. })));
    }

    #[test]
    fn agree_with_mismatched_game_id_returns_error() {
        let mut room = make_room();
        let err = room.handle_line(Color::Black, &line("AGREE other"), 0).unwrap_err();
        assert!(matches!(err, ServerError::State(StateError::GameIdMismatch { .. })));
    }

    #[test]
    fn reject_during_agree_waiting_emits_only_reject_line() {
        let mut room = make_room();
        let r = room.handle_line(Color::Black, &line("REJECT"), 0).unwrap();
        match &r.outcome {
            HandleOutcome::GameEnded(GameResult::Abnormal { winner: None }) => {}
            other => panic!("unexpected outcome: {other:?}"),
        }
        // Requirement 1.5: REJECT は `#ABNORMAL` を送らない。送信は 1 行のみ。
        assert_eq!(r.broadcasts.len(), 1);
        assert_eq!(r.broadcasts[0].target, BroadcastTarget::Players);
        assert_eq!(r.broadcasts[0].line.as_str(), "REJECT:20140101120000 by alice");
    }

    #[test]
    fn force_abnormal_during_play_marks_winner_as_opposite() {
        let mut room = make_room();
        agree_both(&mut room);
        let r = room.force_abnormal(Color::Black);
        match &r.outcome {
            HandleOutcome::GameEnded(GameResult::Abnormal {
                winner: Some(Color::White),
            }) => {}
            other => panic!("unexpected outcome: {other:?}"),
        }
    }

    #[test]
    fn force_time_up_outside_play_is_noop() {
        let mut room = make_room();
        // AgreeWaiting 中に呼んでも no-op
        let r = room.force_time_up(Color::Black);
        assert_eq!(r.outcome, HandleOutcome::Continue);
        assert!(matches!(room.status(), GameStatus::AgreeWaiting));
    }

    #[test]
    fn handle_line_after_finished_returns_error() {
        let mut room = make_room();
        agree_both(&mut room);
        let _ = room.handle_line(Color::Black, &line("%TORYO"), 0).unwrap();
        let err = room.handle_line(Color::White, &line("%TORYO"), 0).unwrap_err();
        assert!(matches!(err, ServerError::State(StateError::InvalidForState { .. })));
    }

    #[test]
    fn x1_command_in_phase1_is_not_enabled() {
        let mut room = make_room();
        let err = room.handle_line(Color::Black, &line("%%WHO"), 0).unwrap_err();
        assert!(matches!(err, ServerError::Protocol(ProtocolError::X1NotEnabled(_))));
    }

    #[test]
    fn kachi_rejected_is_treated_as_illegal_move() {
        let mut room = make_room();
        agree_both(&mut room);
        // 平手初期局面で %KACHI → 24 点法不成立 → IllegalKachi 反則負け。
        let r = room.handle_line(Color::Black, &line("%KACHI"), 0).unwrap();
        match &r.outcome {
            HandleOutcome::GameEnded(GameResult::IllegalMove {
                loser: Color::Black,
                reason: IllegalReason::IllegalKachi,
            }) => {}
            other => panic!("unexpected outcome: {other:?}"),
        }
    }

    #[test]
    fn chudan_during_play_finishes_abnormal_no_winner() {
        let mut room = make_room();
        agree_both(&mut room);
        let r = room.handle_line(Color::Black, &line("%CHUDAN"), 0).unwrap();
        match &r.outcome {
            HandleOutcome::GameEnded(GameResult::Abnormal { winner: None }) => {}
            other => panic!("unexpected outcome: {other:?}"),
        }
        // 全員に同一 #ABNORMAL（draws/cancellation 用 All 系列）が 1 行ずつ。
        assert!(r.broadcasts.iter().any(|b| b.line.as_str() == "#ABNORMAL"));
    }

    #[test]
    fn time_up_does_not_advance_position_or_move_count() {
        let mut room = make_room();
        agree_both(&mut room);
        let initial_ply = room.position().game_ply();
        let r = room.handle_line(Color::Black, &line("+7776FU"), 70_000).unwrap();
        match &r.outcome {
            HandleOutcome::GameEnded(GameResult::TimeUp {
                loser: Color::Black,
            }) => {}
            other => panic!("unexpected outcome: {other:?}"),
        }
        // 局面が進んでいない（do_move されない）。
        assert_eq!(room.position().game_ply(), initial_ply);
        // 手数カウンタも 0 のまま。
        assert_eq!(room.moves_played(), 0);
    }

    #[test]
    fn move_with_wrong_csa_prefix_returns_protocol_error_without_state_change() {
        let mut room = make_room();
        agree_both(&mut room);
        // from は正しい先手だが、CSA 手プレフィックスが `-`（後手）になっている。
        // ProtocolError として弾かれ、Playing 状態は保持される。
        let err = room.handle_line(Color::Black, &line("-3334FU"), 1_000).unwrap_err();
        assert!(matches!(err, ServerError::Protocol(ProtocolError::Malformed(_))));
        assert!(matches!(room.status(), GameStatus::Playing));
        assert_eq!(room.moves_played(), 0);
    }

    #[test]
    fn force_abnormal_during_start_waiting_has_no_winner() {
        let mut room = make_room();
        // 先手だけ AGREE → StartWaiting
        room.handle_line(Color::Black, &line("AGREE"), 0).unwrap();
        let r = room.force_abnormal(Color::White);
        match &r.outcome {
            HandleOutcome::GameEnded(GameResult::Abnormal { winner: None }) => {}
            other => panic!("unexpected outcome: {other:?}"),
        }
    }

    #[test]
    fn max_moves_reaches_max_moves_endpoint() {
        // max_moves=2 にして 2 手指せば即 #MAX_MOVES 終了。
        let config = GameRoomConfig {
            game_id: GameId::new("g"),
            black: PlayerName::new("a"),
            white: PlayerName::new("b"),
            max_moves: 2,
            time_margin_ms: 0,
            entering_king_rule: EnteringKingRule::Point24,
        };
        let clock = Box::new(SecondsCountdownClock::new(60, 5));
        let mut room = GameRoom::new(config, clock);
        agree_both(&mut room);
        let _ = room.handle_line(Color::Black, &line("+7776FU"), 0).unwrap();
        let r = room.handle_line(Color::White, &line("-3334FU"), 0).unwrap();
        match &r.outcome {
            HandleOutcome::GameEnded(GameResult::MaxMoves) => {}
            other => panic!("unexpected outcome: {other:?}"),
        }
        // 2 手目の手送信 + 終局（All 1 系列で #MAX_MOVES, #CENSORED の 2 行）。
        assert_eq!(r.broadcasts.len(), 3);
        assert_eq!(r.broadcasts[0].line.as_str(), "-3334FU,T0");
        assert!(r.broadcasts.iter().any(|b| b.line.as_str() == "#MAX_MOVES"));
        assert!(r.broadcasts.iter().any(|b| b.line.as_str() == "#CENSORED"));
    }
}
