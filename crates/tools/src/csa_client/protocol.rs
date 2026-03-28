//! CSAプロトコル通信層
//!
//! TCP接続によるCSAサーバーとのテキスト行ベース通信を管理する。

use std::io::{BufRead, BufReader, BufWriter, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::common::csa::{Color, CsaMove, ParsedMove, Position, parse_csa_full};

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
    /// 持ち時間（秒）
    pub total_time_sec: u32,
    /// 秒読み（秒）
    pub byoyomi_sec: u32,
    /// フィッシャー increment（秒）
    pub increment_sec: u32,
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
    reader: BufReader<TcpStream>,
    writer: BufWriter<TcpStream>,
    last_send_time: Instant,
    /// パスワードマスク用
    password: String,
}

impl CsaConnection {
    /// CSAサーバーに接続する
    pub fn connect(host: &str, port: u16, tcp_keepalive: bool) -> Result<Self> {
        let addr = format!("{host}:{port}");
        log::info!("[CSA] 接続中: {addr}");
        let stream = TcpStream::connect_timeout(
            &addr.parse().context("invalid server address")?,
            Duration::from_secs(15),
        )
        .with_context(|| format!("CSAサーバー接続失敗: {addr}"))?;

        if tcp_keepalive {
            set_tcp_keepalive(&stream)?;
        }
        // Nagle 無効化（低遅延のため）
        let _ = stream.set_nodelay(true);
        // 読み取りタイムアウト: keep-alive チェック用に30秒
        stream.set_read_timeout(Some(Duration::from_secs(30)))?;

        let reader = BufReader::new(stream.try_clone()?);
        let writer = BufWriter::new(stream);

        Ok(Self {
            reader,
            writer,
            last_send_time: Instant::now(),
            password: String::new(),
        })
    }

    /// ログイン
    pub fn login(&mut self, id: &str, password: &str) -> Result<()> {
        self.password = password.to_string();
        let cmd = format!("LOGIN {id} {password}");
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
    pub fn recv_game_summary(&mut self) -> Result<GameSummary> {
        log::info!("[CSA] 対局待機中...");
        // "BEGIN Game_Summary" を待つ
        loop {
            let line = self.recv_line_blocking(Duration::from_secs(3600))?;
            if line == "BEGIN Game_Summary" {
                break;
            }
        }

        let mut game_id = String::new();
        let mut my_color = Color::Black;
        let mut sente_name = String::new();
        let mut gote_name = String::new();
        let mut total_time_sec: u32 = 0;
        let mut byoyomi_sec: u32 = 0;
        let mut increment_sec: u32 = 0;
        let mut position_lines = Vec::new();
        let mut in_position = false;
        let mut in_time = false;

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
            if line.starts_with("BEGIN Time") {
                in_time = true;
                continue;
            }
            if line.starts_with("END Time") {
                in_time = false;
                continue;
            }

            if in_position {
                position_lines.push(line);
                continue;
            }

            if in_time {
                if let Some(val) = line.strip_prefix("Total_Time:") {
                    total_time_sec = val.trim().parse().unwrap_or(0);
                } else if let Some(val) = line.strip_prefix("Byoyomi:") {
                    byoyomi_sec = val.trim().parse().unwrap_or(0);
                } else if let Some(val) = line.strip_prefix("Increment:") {
                    increment_sec = val.trim().parse().unwrap_or(0);
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
            } else if let Some(val) = line.strip_prefix("Total_Time:") {
                total_time_sec = val.trim().parse().unwrap_or(0);
            } else if let Some(val) = line.strip_prefix("Byoyomi:") {
                byoyomi_sec = val.trim().parse().unwrap_or(0);
            } else if let Some(val) = line.strip_prefix("Increment:") {
                increment_sec = val.trim().parse().unwrap_or(0);
            }
        }

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
            total_time_sec,
            byoyomi_sec,
            increment_sec,
        };
        log::info!(
            "[CSA] 対局情報受信: {} ({}手目から) {}vs{} 持ち時間:{}秒 秒読み:{}秒 inc:{}秒",
            summary.game_id,
            summary.initial_moves.len() + 1,
            summary.sente_name,
            summary.gote_name,
            summary.total_time_sec,
            summary.byoyomi_sec,
            summary.increment_sec,
        );
        Ok(summary)
    }

    /// AGREE を送信して START を待つ
    pub fn agree_and_wait_start(&mut self, game_id: &str) -> Result<()> {
        self.send_line(&format!("AGREE {game_id}"))?;
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
        match self.recv_line_nonblocking() {
            Ok(Some(line)) => {
                // 終局判定
                if line.starts_with('#') {
                    let result = parse_game_result(&line);
                    return Ok(Some(RecvEvent::GameEnd(result, line)));
                }
                // 指し手
                if line.starts_with('+') || line.starts_with('-') {
                    let (mv, time_sec) = parse_server_move(&line);
                    return Ok(Some(RecvEvent::Move(ServerMove { mv, time_sec })));
                }
                // その他（無視）
                Ok(None)
            }
            Ok(None) => Ok(None), // タイムアウト
            Err(e) => Err(e),
        }
    }

    /// 指し手をサーバーに送信する
    pub fn send_move(&mut self, csa_move: &str) -> Result<()> {
        self.send_line(csa_move)
    }

    /// 指し手 + floodgate コメント（評価値・PV）を送信する
    pub fn send_move_with_comment(&mut self, csa_move: &str, comment: Option<&str>) -> Result<()> {
        if let Some(c) = comment {
            self.send_line(c)?;
        }
        self.send_line(csa_move)
    }

    /// 投了を送信
    pub fn send_resign(&mut self) -> Result<()> {
        self.send_line("%TORYO")
    }

    /// 入玉宣言勝ちを送信
    pub fn send_win(&mut self) -> Result<()> {
        self.send_line("%KACHI")
    }

    /// ログアウト
    pub fn logout(&mut self) -> Result<()> {
        let _ = self.send_line("LOGOUT");
        Ok(())
    }

    /// keep-alive 空行を送信（必要な場合）
    pub fn maybe_send_keepalive(&mut self, interval_sec: u64) -> Result<()> {
        if interval_sec == 0 {
            return Ok(());
        }
        if self.last_send_time.elapsed() >= Duration::from_secs(interval_sec) {
            self.send_raw(b"\n")?;
            self.last_send_time = Instant::now();
        }
        Ok(())
    }

    fn send_line(&mut self, line: &str) -> Result<()> {
        // パスワードをマスクしてログ出力
        let masked = if line.contains(&self.password) && !self.password.is_empty() {
            line.replace(&self.password, "*****")
        } else {
            line.to_string()
        };
        log::debug!("[CSA] > {masked}");
        self.writer.write_all(line.as_bytes())?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;
        self.last_send_time = Instant::now();
        Ok(())
    }

    fn send_raw(&mut self, data: &[u8]) -> Result<()> {
        self.writer.write_all(data)?;
        self.writer.flush()?;
        Ok(())
    }

    /// ブロッキング読み取り（タイムアウト付き）
    fn recv_line_blocking(&mut self, timeout: Duration) -> Result<String> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining =
                deadline.checked_duration_since(Instant::now()).unwrap_or(Duration::ZERO);
            if remaining.is_zero() {
                bail!("サーバー応答タイムアウト");
            }
            self.reader
                .get_ref()
                .set_read_timeout(Some(remaining.min(Duration::from_secs(5))))?;
            let mut line = String::new();
            match self.reader.read_line(&mut line) {
                Ok(0) => bail!("サーバー切断"),
                Ok(_) => {
                    let trimmed = line.trim_end().to_string();
                    if !trimmed.is_empty() {
                        log::debug!("[CSA] < {trimmed}");
                        return Ok(trimmed);
                    }
                }
                Err(ref e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    continue;
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// ノンブロッキング読み取り。データがなければ Ok(None)
    fn recv_line_nonblocking(&mut self) -> Result<Option<String>> {
        self.reader.get_ref().set_read_timeout(Some(Duration::from_millis(100)))?;
        let mut line = String::new();
        match self.reader.read_line(&mut line) {
            Ok(0) => bail!("サーバー切断"),
            Ok(_) => {
                let trimmed = line.trim_end().to_string();
                if trimmed.is_empty() {
                    Ok(None)
                } else {
                    log::debug!("[CSA] < {trimmed}");
                    Ok(Some(trimmed))
                }
            }
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                Ok(None)
            }
            Err(e) => Err(e.into()),
        }
    }
}

/// サーバーから受信したイベント
pub enum RecvEvent {
    Move(ServerMove),
    GameEnd(GameResult, String),
}

fn parse_server_move(line: &str) -> (String, u32) {
    // "+7776FU,T30" or "+7776FU"
    if let Some(comma_pos) = line.find(",T") {
        let mv = line[..7.min(comma_pos)].to_string();
        let time_sec = line[comma_pos + 2..].parse::<u32>().unwrap_or(0);
        (mv, time_sec)
    } else if line.len() >= 7 {
        (line[..7].to_string(), 0)
    } else {
        (line.to_string(), 0)
    }
}

fn parse_game_result(line: &str) -> GameResult {
    if line.contains("WIN") {
        GameResult::Win
    } else if line.contains("LOSE") {
        GameResult::Lose
    } else if line.contains("DRAW") {
        GameResult::Draw
    } else if line.contains("CHUDAN") {
        GameResult::Interrupted
    } else {
        GameResult::Censored
    }
}

/// TCP SO_KEEPALIVE を有効化する
#[cfg(unix)]
fn set_tcp_keepalive(stream: &TcpStream) -> Result<()> {
    use std::os::unix::io::AsRawFd;
    let fd = stream.as_raw_fd();
    let optval: libc::c_int = 1;
    // SAFETY: fd は有効なソケット。optval は有効なポインタ。
    let ret = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_KEEPALIVE,
            &optval as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if ret != 0 {
        log::warn!("SO_KEEPALIVE 設定失敗: {}", std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(unix))]
fn set_tcp_keepalive(_stream: &TcpStream) -> Result<()> {
    Ok(())
}
