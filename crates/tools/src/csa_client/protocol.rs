//! CSAプロトコル通信層
//!
//! `transport` モジュール（TCP / WebSocket）の上にテキスト行ベースの
//! CSA プロトコルを乗せる。送信側は `serialize_client_command` 経由で
//! `ClientCommand` バリアントから 1 行を組み立てる。

use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};

use rshogi_csa::{Color, CsaMove, ParsedMove, Position, parse_csa_full};
use rshogi_csa_server::protocol::command::{ClientCommand, serialize_client_command};
use rshogi_csa_server::types::{CsaMoveToken, GameId, PlayerName, Secret};

use super::event::Event;
use super::transport::{ConnectOpts, CsaTransport, TransportTarget};

/// 先後共通または個別の時間設定
#[derive(Clone, Debug, Default)]
pub struct TimeConfig {
    /// 持ち時間（ミリ秒）
    pub total_time_ms: i64,
    /// 秒読み（ミリ秒）
    pub byoyomi_ms: i64,
    /// フィッシャー increment（ミリ秒）
    pub increment_ms: i64,
}

/// CSAサーバーから受信した対局情報
#[derive(Clone, Debug)]
pub struct GameSummary {
    pub game_id: String,
    pub my_color: Color,
    /// 先手番の名前
    pub sente_name: String,
    /// 後手番の名前
    pub gote_name: String,
    /// 初期局面
    pub position: Position,
    /// 途中からの再開手順
    pub initial_moves: Vec<CsaMove>,
    /// 先手の時間設定
    pub black_time: TimeConfig,
    /// 後手の時間設定
    pub white_time: TimeConfig,
}

/// サーバーから受信した指し手
#[derive(Clone, Debug)]
pub struct ServerMove {
    /// CSA形式の指し手 (例: "+7776FU")
    pub mv: String,
    /// 消費時間（秒）
    pub time_sec: u32,
}

/// サーバーからの対局結果
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GameResult {
    Win,
    Lose,
    Draw,
    /// 中断
    Censored,
    Interrupted,
}

/// CSAプロトコルクライアント
pub struct CsaConnection {
    /// 下層 transport（TCP / WebSocket）。
    transport: CsaTransport,
    last_activity_time: Instant,
    /// パスワードマスク用
    password: String,
    /// 直前に受信した終局理由行（#TIME_UP 等）
    pub pending_end_reason: Option<String>,
}

impl CsaConnection {
    /// 既存呼び出し互換: TCP 経路に絞った接続。
    pub fn connect(host: &str, port: u16, tcp_keepalive: bool) -> Result<Self> {
        Self::connect_with_target(
            &TransportTarget::from_host_port(host, port),
            &ConnectOpts {
                tcp_keepalive,
                ws_origin: None,
            },
        )
    }

    /// 解析済み `TransportTarget` と接続オプションから接続する。WebSocket 経路は
    /// 必ず本関数経由で開く（`host` に `ws://` / `wss://` を含めれば
    /// `connect()` でも転送される）。
    pub fn connect_with_target(target: &TransportTarget, opts: &ConnectOpts) -> Result<Self> {
        let transport = CsaTransport::connect(target, opts)?;
        Ok(Self {
            transport,
            last_activity_time: Instant::now(),
            password: String::new(),
            pending_end_reason: None,
        })
    }

    /// ログイン
    pub fn login(&mut self, id: &str, password: &str) -> Result<()> {
        self.password = password.to_string();
        let cmd = serialize_client_command(&ClientCommand::Login {
            name: PlayerName::new(id),
            password: Secret::new(password),
            x1: false,
            reconnect: None,
        });
        self.send_line(&cmd)?;
        let response = self.recv_line_blocking(Duration::from_secs(15))?;
        if response.starts_with("LOGIN:") && response.contains("OK") {
            log::info!("[CSA] ログイン成功: {id}");
            Ok(())
        } else {
            bail!("ログイン失敗: {response}");
        }
    }

    /// Game_Summary を受信して解析する
    pub fn recv_game_summary(&mut self, keepalive_interval_sec: u64) -> Result<GameSummary> {
        log::info!("[CSA] 対局待機中...");
        // "BEGIN Game_Summary" を待つ（keep-alive 送信しながら）
        loop {
            match self.recv_line_nonblocking() {
                Ok(Some(line)) if line == "BEGIN Game_Summary" => break,
                Ok(Some(_)) => {} // 他の行は無視
                Ok(None) => {
                    self.maybe_send_keepalive(keepalive_interval_sec)?;
                }
                Err(e) => return Err(e),
            }
        }

        let mut game_id = String::new();
        let mut my_color = Color::Black;
        let mut sente_name = String::new();
        let mut gote_name = String::new();
        let mut position_lines = Vec::new();
        let mut in_position = false;

        // 時間設定: 共通 / 先手別 / 後手別の3レイヤー
        // Time_Unit のデフォルトは秒 (1000ms)
        // header_time_unit_ms: ヘッダレベルの Time_Unit（ブロック外・共通）
        // block_time_unit_ms: 現在の Time ブロック内の Time_Unit
        let mut header_time_unit_ms: i64 = 1000;
        let mut block_time_unit_ms: i64 = 1000;
        let mut common_time = TimeConfig::default();
        let mut black_time: Option<TimeConfig> = None;
        let mut white_time: Option<TimeConfig> = None;
        // 現在パース中の Time ブロックの対象 (None=共通, Some(Black/White)=個別)
        let mut time_target: Option<Option<Color>> = None;

        loop {
            let line = self.recv_line_blocking(Duration::from_secs(30))?;
            if line == "END Game_Summary" {
                break;
            }
            if line == "BEGIN Position" {
                in_position = true;
                continue;
            }
            if line == "END Position" {
                in_position = false;
                continue;
            }
            if line == "BEGIN Time" {
                block_time_unit_ms = header_time_unit_ms;
                time_target = Some(None); // 共通
                continue;
            }
            if line == "BEGIN Time+" {
                block_time_unit_ms = header_time_unit_ms;
                black_time = Some(common_time.clone());
                time_target = Some(Some(Color::Black));
                continue;
            }
            if line == "BEGIN Time-" {
                block_time_unit_ms = header_time_unit_ms;
                white_time = Some(common_time.clone());
                time_target = Some(Some(Color::White));
                continue;
            }
            if line.starts_with("END Time") {
                time_target = None;
                continue;
            }

            if in_position {
                position_lines.push(line);
                continue;
            }

            if let Some(target) = &time_target {
                let tc = match target {
                    None => &mut common_time,
                    Some(Color::Black) => black_time.as_mut().unwrap(),
                    Some(Color::White) => white_time.as_mut().unwrap(),
                };
                if let Some(val) = line.strip_prefix("Time_Unit:") {
                    block_time_unit_ms = parse_time_unit(val.trim());
                } else if let Some(val) = line.strip_prefix("Total_Time:") {
                    let v: i64 = val.trim().parse().unwrap_or(0);
                    tc.total_time_ms = v * block_time_unit_ms;
                } else if let Some(val) = line.strip_prefix("Byoyomi:") {
                    let v: i64 = val.trim().parse().unwrap_or(0);
                    tc.byoyomi_ms = v * block_time_unit_ms;
                } else if let Some(val) = line.strip_prefix("Increment:") {
                    let v: i64 = val.trim().parse().unwrap_or(0);
                    tc.increment_ms = v * block_time_unit_ms;
                }
                continue;
            }

            // ヘッダフィールド
            if let Some(val) = line.strip_prefix("Game_ID:") {
                game_id = val.trim().to_string();
            } else if let Some(val) = line.strip_prefix("Name+:") {
                sente_name = val.trim().to_string();
            } else if let Some(val) = line.strip_prefix("Name-:") {
                gote_name = val.trim().to_string();
            } else if let Some(val) = line.strip_prefix("Your_Turn:") {
                my_color = if val.trim() == "+" {
                    Color::Black
                } else {
                    Color::White
                };
            } else if let Some(val) = line.strip_prefix("Time_Unit:") {
                header_time_unit_ms = parse_time_unit(val.trim());
            } else if let Some(val) = line.strip_prefix("Total_Time:") {
                let v: i64 = val.trim().parse().unwrap_or(0);
                common_time.total_time_ms = v * header_time_unit_ms;
            } else if let Some(val) = line.strip_prefix("Byoyomi:") {
                let v: i64 = val.trim().parse().unwrap_or(0);
                common_time.byoyomi_ms = v * header_time_unit_ms;
            } else if let Some(val) = line.strip_prefix("Increment:") {
                let v: i64 = val.trim().parse().unwrap_or(0);
                common_time.increment_ms = v * header_time_unit_ms;
            }
        }

        // 先後別設定がなければ共通設定をコピー
        let final_black_time = black_time.unwrap_or_else(|| common_time.clone());
        let final_white_time = white_time.unwrap_or(common_time);

        // Position ブロックをパース
        let pos_text = position_lines.join("\n");
        let (position, parsed_moves, _) = parse_csa_full(&pos_text)?;
        let initial_moves: Vec<CsaMove> = parsed_moves
            .into_iter()
            .filter_map(|m| match m {
                ParsedMove::Normal(cm) => Some(cm),
                ParsedMove::Special(_) => None,
            })
            .collect();

        let summary = GameSummary {
            game_id,
            my_color,
            sente_name,
            gote_name,
            position,
            initial_moves,
            black_time: final_black_time,
            white_time: final_white_time,
        };
        log::info!(
            "[CSA] 対局情報受信: {} ({}手目から) {}vs{} 先手:{}ms+{}ms+{}ms 後手:{}ms+{}ms+{}ms",
            summary.game_id,
            summary.initial_moves.len() + 1,
            summary.sente_name,
            summary.gote_name,
            summary.black_time.total_time_ms,
            summary.black_time.byoyomi_ms,
            summary.black_time.increment_ms,
            summary.white_time.total_time_ms,
            summary.white_time.byoyomi_ms,
            summary.white_time.increment_ms,
        );
        Ok(summary)
    }

    /// AGREE を送信して START を待つ
    pub fn agree_and_wait_start(&mut self, game_id: &str) -> Result<()> {
        let cmd = serialize_client_command(&ClientCommand::Agree {
            game_id: Some(GameId::new(game_id)),
        });
        self.send_line(&cmd)?;
        loop {
            let line = self.recv_line_blocking(Duration::from_secs(60))?;
            if line.starts_with("START:") {
                log::info!("[CSA] 対局開始: {}", line);
                return Ok(());
            }
            if line.starts_with("REJECT:") {
                bail!("対局が拒否されました: {line}");
            }
        }
    }

    /// サーバーから指し手を受信する。
    /// タイムアウト時は Ok(None) を返す（keep-alive チェック用）。
    pub fn recv_move(&mut self) -> Result<Option<RecvEvent>> {
        // 中間行（#TIME_UP 等）をスキップするためループ
        loop {
            match self.recv_line_nonblocking() {
                Ok(Some(line)) => {
                    // 終局判定: #WIN/#LOSE/#DRAW/#CENSORED/#CHUDAN のみ GameEnd。
                    // #TIME_UP, #ILLEGAL_MOVE, #MAX_MOVES 等は中間行なので無視
                    // （直後に #WIN/#LOSE/#DRAW が来る）。
                    if line.starts_with('#') {
                        if let Some(result) = parse_game_result(&line) {
                            let reason = self.pending_end_reason.take();
                            return Ok(Some(RecvEvent::GameEnd(result, line, reason)));
                        }
                        // 中間行（#TIME_UP 等）を保持して次の最終結果行を待つ
                        log::info!("[CSA] 終局理由: {line}");
                        self.pending_end_reason = Some(line);
                        continue;
                    }
                    // 指し手
                    if line.starts_with('+') || line.starts_with('-') {
                        let (mv, time_sec) = parse_server_move(&line);
                        return Ok(Some(RecvEvent::Move(ServerMove { mv, time_sec })));
                    }
                    // その他（無視）
                    return Ok(None);
                }
                Ok(None) => return Ok(None), // タイムアウト
                Err(e) => return Err(e),
            }
        }
    }

    /// 指し手をサーバーに送信する
    pub fn send_move(&mut self, csa_move: &str) -> Result<()> {
        let cmd = serialize_client_command(&ClientCommand::Move {
            token: CsaMoveToken::new(csa_move),
            comment: None,
        });
        self.send_line(&cmd)
    }

    /// 指し手 + floodgate コメント（評価値・PV）を送信する。
    /// `comment` には `'` プレフィックスを含まない本体（例: `* 123 +7776FU -3334FU`）を渡す。
    /// 送信時は `+7776FU,'* 123 +7776FU -3334FU` のように `,'<comment>` 形式で付加される。
    pub fn send_move_with_comment(&mut self, csa_move: &str, comment: Option<&str>) -> Result<()> {
        let cmd = serialize_client_command(&ClientCommand::Move {
            token: CsaMoveToken::new(csa_move),
            comment: comment.map(|c| c.to_owned()),
        });
        self.send_line(&cmd)
    }

    /// 投了を送信
    pub fn send_resign(&mut self) -> Result<()> {
        self.send_line(&serialize_client_command(&ClientCommand::Toryo))
    }

    /// 入玉宣言勝ちを送信
    pub fn send_win(&mut self) -> Result<()> {
        self.send_line(&serialize_client_command(&ClientCommand::Kachi))
    }

    /// ログアウト
    pub fn logout(&mut self) -> Result<()> {
        let _ = self.send_line(&serialize_client_command(&ClientCommand::Logout));
        Ok(())
    }

    /// keep-alive 空行を送信（必要な場合）
    pub fn maybe_send_keepalive(&mut self, interval_sec: u64) -> Result<()> {
        if interval_sec == 0 {
            return Ok(());
        }
        if self.last_activity_time.elapsed() >= Duration::from_secs(interval_sec) {
            self.transport.write_keepalive()?;
            self.last_activity_time = Instant::now();
        }
        Ok(())
    }

    fn send_line(&mut self, line: &str) -> Result<()> {
        if !self.password.is_empty() && line.contains(&self.password) {
            let masked = line.replace(&self.password, "*****");
            log::debug!("[CSA] > {masked}");
        } else {
            log::debug!("[CSA] > {line}");
        }
        self.transport.write_line(line)?;
        self.last_activity_time = Instant::now();
        Ok(())
    }

    /// サーバー受信を別スレッドに移し、共通チャネルに `Event::ServerLine` を送信する。
    /// 対局開始後に呼ぶ。以降、`recv_move` / 内部 `recv_line_*` は使用不可。
    pub fn start_reader_thread(&mut self, tx: mpsc::Sender<Event>) -> Result<()> {
        self.transport.start_reader_thread(tx)
    }

    fn recv_line_blocking(&mut self, timeout: Duration) -> Result<String> {
        let line = self.transport.read_line_blocking(timeout)?;
        self.last_activity_time = Instant::now();
        Ok(line)
    }

    fn recv_line_nonblocking(&mut self) -> Result<Option<String>> {
        let opt = self.transport.read_line_nonblocking()?;
        if opt.is_some() {
            self.last_activity_time = Instant::now();
        }
        Ok(opt)
    }
}

/// サーバーから受信したイベント
pub enum RecvEvent {
    Move(ServerMove),
    /// (最終結果, 結果行, 終局理由行（#TIME_UP等、あれば）)
    GameEnd(GameResult, String, Option<String>),
}

pub(crate) fn parse_server_move(line: &str) -> (String, u32) {
    // "+7776FU,T30" or "+7776FU"
    if let Some(comma_pos) = line.find(",T") {
        let mv = line.get(..7.min(comma_pos)).unwrap_or(line).to_string();
        let time_sec = line[comma_pos + 2..].parse::<u32>().unwrap_or(0);
        (mv, time_sec)
    } else {
        let mv = line.get(..7).unwrap_or(line).to_string();
        (mv, 0)
    }
}

fn parse_time_unit(v: &str) -> i64 {
    if v.contains("msec") || v.contains("ms") {
        1
    } else if v.contains("min") {
        60000
    } else {
        1000
    }
}

/// 最終結果行のみ Some を返す。中間行（#TIME_UP, #ILLEGAL_MOVE 等）は None。
pub(crate) fn parse_game_result(line: &str) -> Option<GameResult> {
    if line.contains("#WIN") {
        Some(GameResult::Win)
    } else if line.contains("#LOSE") {
        Some(GameResult::Lose)
    } else if line.contains("#DRAW") {
        Some(GameResult::Draw)
    } else if line.contains("#CHUDAN") {
        Some(GameResult::Interrupted)
    } else if line.contains("#CENSORED") {
        Some(GameResult::Censored)
    } else {
        None // #TIME_UP, #ILLEGAL_MOVE, #SENNICHITE 等は中間行
    }
}
