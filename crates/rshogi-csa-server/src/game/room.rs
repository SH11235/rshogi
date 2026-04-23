//! 1 対局のライフサイクル全体を駆動する `GameRoom`。
//!
//! - I/O は行わず、外部から `handle_line` で 1 行ずつ駆動される。
//! - 関係者へ送るべき行は [`HandleResult::broadcasts`] に積まれて返り、フロントエンド
//!   が `Broadcaster` 経由で実配信する設計。
//! - 状態機械は `AgreeWaiting → StartWaiting → Playing → Finished` の単調遷移。

use std::fmt;

use rshogi_core::position::{Position, SFEN_HIRATE};
use rshogi_core::types::EnteringKingRule;

use crate::error::{ProtocolError, ServerError, StateError};
use crate::game::clock::{ClockResult, TimeClock};
use crate::game::result::{GameResult, IllegalReason};
use crate::game::validator::{KachiOutcome, RepetitionVerdict, Validator, Violation};
use crate::protocol::command::{ClientCommand, parse_command};
use crate::types::{Color, CsaLine, CsaMoveToken, GameId, PlayerName};

/// 対局ルームの状態機械（4 状態）。
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
/// フロントエンドがフィルタする（受信者ごとに 1 回ずつ「理由→勝敗」が届くよう
/// 宛先は重複しない区分にしている）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BroadcastTarget {
    /// 先手対局者だけに送る。
    Black,
    /// 後手対局者だけに送る。
    White,
    /// 両対局者に送る（観戦者は含めない）。
    Players,
    /// 観戦者だけに送る（観戦機能未実装の本構成でも経路だけ用意しておく）。
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
        /// 次手番側の **本体持ち時間** の残り (ms)。秒読みは含まない（表示・ログ用）。
        /// deadline 計算には [`GameRoom::clock_turn_budget_ms`] を使うこと。
        remaining_main_ms: i64,
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
    /// 最大手数（既定 256）。これに達したら `#MAX_MOVES`。
    pub max_moves: u32,
    /// 通信マージン（ミリ秒）。`consume` 呼び出し前に減算される。
    pub time_margin_ms: u64,
    /// `%KACHI` 判定に使う入玉ルール（既定は 24 点法 = `Point24`）。
    pub entering_king_rule: EnteringKingRule,
    /// 対局の開始局面を表す SFEN。`None` なら平手（`SFEN_HIRATE`）を使う。
    ///
    /// 駒落ち・ブイ・フォーク対局では本フィールドを `Some(sfen)` で渡す。
    /// **契約**: Game_Summary の `position_section` / `to_move` と、棋譜の
    /// `initial_position` は本フィールドから派生させること（三点一致）。
    /// フロントエンドはこの SFEN を
    /// [`crate::protocol::summary::position_section_from_sfen`] に渡して
    /// `position_section` を取得し、同じ関数の出力をそのまま棋譜の
    /// `initial_position` にも使う。手番は SFEN 由来の `side_to_move` を
    /// `to_move` にセットする。これにより 3 経路間で局面が食い違う事故を防ぐ。
    pub initial_sfen: Option<String>,
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
            .field("initial_sfen", &self.initial_sfen)
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
    /// `GameRoomConfig::initial_sfen` に従って対局ルームを構築する。
    ///
    /// - `initial_sfen = None`: 平手 (`SFEN_HIRATE`) で初期化。SFEN_HIRATE は
    ///   const で `set_sfen` が失敗するのは rshogi-core 側のバグなので、この
    ///   パスは内部で `expect` する。
    /// - `initial_sfen = Some(sfen)`: 渡された SFEN で `Position::set_sfen`。
    ///   SFEN 不正時は `Err(ServerError::Protocol(Malformed))` を返し、
    ///   呼び出し側が適切に拒否 / ログ出力できるようにする (Codex review
    ///   PR #470 4th round P2)。プロセス全体 / DO を panic で落とさないため
    ///   の設計。
    ///
    /// Game_Summary の `position_section` / `to_move` と棋譜の `initial_position`
    /// は同一 SFEN から派生させる契約なので、呼び出し側は同じ `initial_sfen` を
    /// GameRoom / GameSummaryBuilder / KifuRecord に横断して渡すこと。
    pub fn new(config: GameRoomConfig, clock: Box<dyn TimeClock>) -> Result<Self, ServerError> {
        let mut pos = Position::new();
        match config.initial_sfen.as_deref() {
            Some(sfen) => pos.set_sfen(sfen).map_err(|e| {
                ServerError::Protocol(ProtocolError::Malformed(format!(
                    "invalid initial_sfen {sfen:?}: {e:?}"
                )))
            })?,
            None => {
                // 平手は const。失敗するのはコアのバグなので expect で落としていい。
                pos.set_sfen(SFEN_HIRATE).expect("SFEN_HIRATE must be valid");
            }
        }
        let validator = Validator::new(config.entering_king_rule);
        Ok(Self {
            config,
            pos,
            clock,
            validator,
            status: GameStatus::AgreeWaiting,
            moves_played: 0,
            turn_started_at_ms: None,
        })
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

    /// 指定側の **本体持ち時間** の残り（ミリ秒）。秒読みは含まない。
    ///
    /// 表示・ログ・棋譜メタデータ用。deadline 計算には
    /// [`Self::clock_turn_budget_ms`] を使うこと（秒読みを含んだ予算が必要なため）。
    pub fn clock_remaining_main_ms(&self, color: Color) -> i64 {
        self.clock.remaining_main_ms(color)
    }

    /// 指定側が今の 1 手で使える最大時間（ミリ秒、秒読み込み）。
    ///
    /// `run_loop` で時計切れアラームを設定する際に使う。秒読みは手番ごとに
    /// リセットされるため、本 API は `本体残り + byoyomi` 全量を返す。
    pub fn clock_turn_budget_ms(&self, color: Color) -> i64 {
        self.clock.turn_budget_ms(color)
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
            // x1 拡張コマンドは本クレートでは未サポートとして弾く。
            other => {
                Err(ServerError::Protocol(ProtocolError::X1NotEnabled(command_static_name(&other))))
            }
        }
    }

    /// 外部タイマーが時間切れを検出したときに呼ぶ。
    ///
    /// `loser` は時間を使い切った側。`Playing` 状態でのみ有効で、それ以外で呼ばれた
    /// 場合は内部不変条件違反として `Internal` エラーを返さず、no-op で `Continue` を
    /// 返す（タイマーの spurious 起動は許容する方針）。
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

    /// 切断を検出したときに呼ぶ。再接続猶予 0 秒で即時 `#ABNORMAL` 確定。
    ///
    /// 勝者確定は「対局中の切断」に限り、それ以前
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
            // REJECT は対局不成立を双方に通知して終了。
            // CSA 仕様上は `#ABNORMAL` を送らないため、ここでは finish() ではなく
            // 専用経路で `REJECT:<game_id> by <rejector>` のみ配信する。
            // 内部状態は Finished(Abnormal{None}) を流用する（REJECT 専用の
            // GameResult variant を増やさずに既存 enum で表現する割り切り）。
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
        // 手番判定。手番外からの指し手はプロトコルエラーで拒否し、
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
        // 1. 経過時間を計算し通信マージンを差し引いて時計を消費。
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

        // 4. 関係者に `<token>,T<sec>` を配信。
        let mut broadcasts = vec![BroadcastEntry {
            target: BroadcastTarget::All,
            line: CsaLine::new(format!("{},T{}", token.as_str(), elapsed_sec)),
        }];

        // 5. 千日手・連続王手千日手判定。
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
                // `OuteSennichiteLose` ⇔ `Position::repetition_state` の `Lose` で、
                // 「手番側 (= side-to-move after the last move = from.opposite()) が
                // 連続王手していた側で反則負け」を意味する。循環の最終手 (from) が
                // 非王手 (=受け手の escape) で閉じ、from.opposite() がサイクル中ずっと
                // 王手を連続して掛けていた場合に発火する。従って敗者は from.opposite()。
                let mut result = self.finish(GameResult::OuteSennichite {
                    loser: from.opposite(),
                });
                broadcasts.append(&mut result.broadcasts);
                return Ok(HandleResult {
                    outcome: result.outcome,
                    broadcasts,
                });
            }
            RepetitionVerdict::OuteSennichiteWin => {
                // `OuteSennichiteWin` ⇔ `Position::repetition_state` の `Win` で、
                // 「手番側 (from.opposite()) が勝ち」= 直前に指した from が連続王手
                // していた側で反則負け。循環の最終手が from による王手で閉じた場合に発火。
                let mut result = self.finish(GameResult::OuteSennichite { loser: from });
                broadcasts.append(&mut result.broadcasts);
                return Ok(HandleResult {
                    outcome: result.outcome,
                    broadcasts,
                });
            }
        }

        // 6. 最大手数到達判定。
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
        let remaining_main_ms = self.clock.remaining_main_ms(core_next.into());
        Ok(HandleResult {
            outcome: HandleOutcome::MoveAccepted {
                next_turn,
                remaining_main_ms,
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
        // %CHUDAN は対局中断。`#ABNORMAL`（勝者なし）として終局する。
        if !matches!(self.status, GameStatus::Playing) {
            return Err(ServerError::State(StateError::InvalidForState {
                current: format!("{:?}", self.status),
            }));
        }
        Ok(self.finish(GameResult::Abnormal { winner: None }))
    }

    fn finish(&mut self, result: GameResult) -> HandleResult {
        // result.server_messages() の順序通りに BroadcastEntry を組む。
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
            initial_sfen: None,
        };
        let clock = Box::new(SecondsCountdownClock::new(60, 5));
        GameRoom::new(config, clock).expect("valid test config")
    }

    /// 任意 SFEN から対局を開始するためのテスト専用 helper。
    ///
    /// `GameRoomConfig::initial_sfen` 契約の配線が完了したので、テストは
    /// 本番 API 経由で任意 SFEN を流し込める。private フィールドを触る旧方式は
    /// 廃止して、`config.initial_sfen = Some(sfen)` をそのまま `GameRoom::new`
    /// に委ねる形に書き換えた。
    fn room_with_sfen(rule: EnteringKingRule, sfen: &str) -> GameRoom {
        let config = GameRoomConfig {
            game_id: GameId::new("20140101120000"),
            black: PlayerName::new("alice"),
            white: PlayerName::new("bob"),
            max_moves: 256,
            time_margin_ms: 0,
            entering_king_rule: rule,
            initial_sfen: Some(sfen.to_owned()),
        };
        let clock = Box::new(SecondsCountdownClock::new(60, 5));
        GameRoom::new(config, clock).expect("valid test config")
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
        // 受信者ごとに 1 回ずつ「理由 → 勝敗」が届く。
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
            initial_sfen: None,
        };
        let clock = Box::new(SecondsCountdownClock::new(60, 0));
        let mut room = GameRoom::new(config, clock).expect("valid test config");
        agree_both(&mut room);
        // 経過 4000ms, margin 1500ms → consume(2500ms)。整数秒切り捨てで 2 秒消費。
        let r = room.handle_line(Color::Black, &line("+7776FU"), 4_000).unwrap();
        assert_eq!(r.broadcasts[0].line.as_str(), "+7776FU,T2");
    }

    // ---- 通信マージン境界値テスト ----

    fn room_with(time_margin_ms: u64, total_sec: u32, byoyomi_sec: u32) -> GameRoom {
        let config = GameRoomConfig {
            game_id: GameId::new("g"),
            black: PlayerName::new("a"),
            white: PlayerName::new("b"),
            max_moves: 256,
            time_margin_ms,
            entering_king_rule: EnteringKingRule::Point24,
            initial_sfen: None,
        };
        let clock = Box::new(SecondsCountdownClock::new(total_sec, byoyomi_sec));
        GameRoom::new(config, clock).expect("valid test config")
    }

    #[test]
    fn time_margin_larger_than_elapsed_clamps_to_zero_consume() {
        // (a) time_margin_ms > elapsed_ms の場合、saturating_sub で consume 引数が 0
        //     になり、手番は時間消費ゼロで受理される（margin が elapsed を完全吸収）。
        let mut room = room_with(5_000, 60, 0);
        agree_both(&mut room);
        let r = room.handle_line(Color::Black, &line("+7776FU"), 1_000).unwrap();
        assert_eq!(r.broadcasts[0].line.as_str(), "+7776FU,T0");
    }

    #[test]
    fn move_at_exact_main_time_boundary_enters_byoyomi_without_timeup() {
        // (c) 秒読み突入直前: 本体 5 秒を使い切って consume(5000ms) を渡すと
        //     ClockResult::Continue で秒読み区間に乗り換える。時間切れにならず、
        //     対局は続行。`consume` 単体の境界は clock.rs::enters_byoyomi_when_main_exhausted
        //     でカバーされているが、`GameRoom` 層での挙動もここで固定する。
        let mut room = room_with(0, 5, 10);
        agree_both(&mut room);
        // margin=0 なので elapsed_ms がそのまま consume に渡る。
        let r = room.handle_line(Color::Black, &line("+7776FU"), 5_000).unwrap();
        assert!(matches!(
            r.outcome,
            HandleOutcome::MoveAccepted {
                next_turn: Color::White,
                ..
            }
        ));
        assert_eq!(r.broadcasts[0].line.as_str(), "+7776FU,T5");
    }

    #[test]
    fn move_at_exact_byoyomi_limit_is_still_accepted() {
        // (d) 秒読み使い切り直前: 本体 0 + 秒読み 10 秒 + ぴったり 10 秒経過で
        //     consume(10_000ms) = over_sec 10 * 1000 == byoyomi_ms なので境界条件
        //     `over_sec * 1000 > byoyomi_ms` は false → Continue。
        //     Handle_line はこれを MoveAccepted として受理する必要がある。
        let mut room = room_with(0, 0, 10);
        agree_both(&mut room);
        let r = room.handle_line(Color::Black, &line("+7776FU"), 10_000).unwrap();
        assert!(matches!(
            r.outcome,
            HandleOutcome::MoveAccepted {
                next_turn: Color::White,
                ..
            }
        ));
        assert_eq!(r.broadcasts[0].line.as_str(), "+7776FU,T10");
    }

    #[test]
    fn move_one_ms_over_byoyomi_boundary_times_out() {
        // (d') 秒読み使い切り直後: consume(11_000ms) は over_sec=11、11*1000 > 10_000
        //      で TimeUp。(d) と (d') で境界条件の両側をテストする。
        let mut room = room_with(0, 0, 10);
        agree_both(&mut room);
        let r = room.handle_line(Color::Black, &line("+7776FU"), 11_000).unwrap();
        assert!(matches!(
            r.outcome,
            HandleOutcome::GameEnded(GameResult::TimeUp {
                loser: Color::Black
            })
        ));
    }

    #[test]
    fn move_at_main_plus_margin_boundary_is_accepted() {
        // (e) 本体 + margin の合算ちょうど: 本体 5 秒 + margin 1500ms のとき
        //     elapsed 6500ms で着手 → consume(5000ms) → Black の本体を使い切って 0 に
        //     落ちる（Continue 扱いで対局続行）。MoveAccepted.remaining_main_ms は
        //     「次手番側 (White) の本体残り」なので、White 未消費の 5000ms が返る。
        //     Black 側が実際に 0 まで落ちたことは broadcast の `,T5` で確認する。
        let mut room = room_with(1_500, 5, 0);
        agree_both(&mut room);
        let r = room.handle_line(Color::Black, &line("+7776FU"), 6_500).unwrap();
        assert!(matches!(
            r.outcome,
            HandleOutcome::MoveAccepted {
                next_turn: Color::White,
                remaining_main_ms: 5_000,
            }
        ));
        assert_eq!(r.broadcasts[0].line.as_str(), "+7776FU,T5");
        // Black 側の本体持ち時間が 0 まで落ちていることを直接確認。
        assert_eq!(room.clock_remaining_main_ms(Color::Black), 0);
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
        // REJECT は `#ABNORMAL` を送らない。送信は 1 行のみ。
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
            initial_sfen: None,
        };
        let clock = Box::new(SecondsCountdownClock::new(60, 5));
        let mut room = GameRoom::new(config, clock).expect("valid test config");
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

    #[test]
    fn kachi_accepted_from_point27_sfen_ends_with_jishogi() {
        // Validator の `evaluate_kachi_accepted_for_27pt_position` と同じ入玉局面を流用。
        // 先手が 28 点を満たしており、両者 AGREE → 先手 %KACHI → `GameResult::Kachi` 確定。
        let mut room =
            room_with_sfen(EnteringKingRule::Point27, "LNSGKGSNL/4BR3/9/9/9/9/9/9/4k4 b RB 1");
        agree_both(&mut room);
        let r = room.handle_line(Color::Black, &line("%KACHI"), 0).unwrap();
        match &r.outcome {
            HandleOutcome::GameEnded(GameResult::Kachi {
                winner: Color::Black,
            }) => {}
            other => panic!("unexpected outcome: {other:?}"),
        }
        // `#JISHOGI` + `#WIN/#LOSE` の 3 宛先 × 2 行 = 6 行。
        assert_eq!(r.broadcasts.len(), 6);
        assert!(r.broadcasts.iter().any(|b| b.line.as_str() == "#JISHOGI"));
        assert!(r.broadcasts.iter().any(|b| b.line.as_str() == "#WIN"));
        assert!(r.broadcasts.iter().any(|b| b.line.as_str() == "#LOSE"));
    }

    #[test]
    fn uchifuzume_pawn_drop_ends_with_illegal_move_reason_uchifuzume() {
        // Validator の `validate_move_detects_uchifuzume` と同じ盤面:
        // 後手玉 1 一、先手 と 1 三、先手金 3 二、先手玉 5 九、手駒に歩。
        // 先手 +0012FU で打ち歩詰 → `IllegalMove{reason: Uchifuzume}` が確定する。
        let mut room = room_with_sfen(EnteringKingRule::Point24, "8k/6G2/8+P/9/9/9/9/9/4K4 b P 1");
        agree_both(&mut room);
        let r = room.handle_line(Color::Black, &line("+0012FU"), 0).unwrap();
        match &r.outcome {
            HandleOutcome::GameEnded(GameResult::IllegalMove {
                loser: Color::Black,
                reason: IllegalReason::Uchifuzume,
            }) => {}
            other => panic!("unexpected outcome: {other:?}"),
        }
        assert!(r.broadcasts.iter().any(|b| b.line.as_str() == "#ILLEGAL_MOVE"));
    }

    #[test]
    fn sennichite_ends_game_after_12_ply_gold_dance() {
        // 平手初期局面で左金を 4 九 ↔ 4 八 / 4 一 ↔ 4 二 と循環させて 3 サイクル
        // (12 手) 経過 → 初期局面 4 回目の到達 → `GameResult::Sennichite`。
        let mut room = make_room();
        agree_both(&mut room);
        let cycle = [
            (Color::Black, "+4948KI"),
            (Color::White, "-4142KI"),
            (Color::Black, "+4849KI"),
            (Color::White, "-4241KI"),
        ];
        for _ in 0..2 {
            for (c, tok) in &cycle {
                let r = room.handle_line(*c, &line(tok), 0).unwrap();
                assert!(matches!(r.outcome, HandleOutcome::MoveAccepted { .. }));
            }
        }
        for (c, tok) in cycle.iter().take(3) {
            let r = room.handle_line(*c, &line(tok), 0).unwrap();
            assert!(matches!(r.outcome, HandleOutcome::MoveAccepted { .. }));
        }
        // 3 サイクル目の最終手 (-4241KI) で 4 回目の初期局面到達 → Sennichite。
        let last = room.handle_line(Color::White, &line("-4241KI"), 0).unwrap();
        match &last.outcome {
            HandleOutcome::GameEnded(GameResult::Sennichite) => {}
            other => panic!("unexpected outcome: {other:?}"),
        }
        assert!(last.broadcasts.iter().any(|b| b.line.as_str() == "#SENNICHITE"));
        assert!(last.broadcasts.iter().any(|b| b.line.as_str() == "#DRAW"));
    }

    #[test]
    fn oute_sennichite_win_variant_loses_the_last_checker() {
        // Win variant: 開始 SFEN で白が既に黒飛の王手下にある (side=W)。
        // 4 手 1 サイクル (白退避 → 黒再王手 → 白退避 → 黒再王手) で開始 SFEN に復帰。
        // 連続王手は反則行為なので 1 サイクルで反則確定 (競技将棋ルール準拠)。
        // 循環最終手 (+4838HI) は黒の王手手なので from=Black、連続王手側の黒が敗者。
        let mut room = room_with_sfen(EnteringKingRule::Point24, "9/6k2/9/9/9/9/9/6R2/K8 w - 1");
        agree_both(&mut room);
        let prefix = [
            (Color::White, "-3242OU"),
            (Color::Black, "+3848HI"),
            (Color::White, "-4232OU"),
        ];
        for (c, tok) in &prefix {
            let r = room.handle_line(*c, &line(tok), 0).unwrap();
            assert!(matches!(r.outcome, HandleOutcome::MoveAccepted { .. }));
        }
        let last = room.handle_line(Color::Black, &line("+4838HI"), 0).unwrap();
        match &last.outcome {
            HandleOutcome::GameEnded(GameResult::OuteSennichite {
                loser: Color::Black,
            }) => {}
            other => panic!("unexpected outcome: {other:?}"),
        }
        assert!(last.broadcasts.iter().any(|b| b.line.as_str() == "#OUTE_SENNICHITE"));
    }

    #[test]
    fn oute_sennichite_lose_variant_loses_the_perpetual_checker() {
        // Lose variant: Win variant と駒群は同じだが、開始 SFEN を「白玉 4 二 退避済、
        // 黒番で次の黒の手が王手」に寄せる (黒飛 3 八, 白玉 4 二, side=B, 非王手)。
        // 4 手 1 サイクルで初期 SFEN 復帰。連続王手側 (Black = from.opposite()) が敗者。
        // 最終手 (-3242OU) は白の退避手で非王手のため from=White、連続王手していた
        // 黒が from.opposite()。
        let mut room = room_with_sfen(EnteringKingRule::Point24, "9/5k3/9/9/9/9/9/6R2/K8 b - 1");
        agree_both(&mut room);
        let prefix = [
            (Color::Black, "+3848HI"),
            (Color::White, "-4232OU"),
            (Color::Black, "+4838HI"),
        ];
        for (c, tok) in &prefix {
            let r = room.handle_line(*c, &line(tok), 0).unwrap();
            assert!(matches!(r.outcome, HandleOutcome::MoveAccepted { .. }));
        }
        let last = room.handle_line(Color::White, &line("-3242OU"), 0).unwrap();
        match &last.outcome {
            HandleOutcome::GameEnded(GameResult::OuteSennichite {
                loser: Color::Black,
            }) => {}
            other => panic!("unexpected outcome: {other:?}"),
        }
        assert!(last.broadcasts.iter().any(|b| b.line.as_str() == "#OUTE_SENNICHITE"));
    }
}
