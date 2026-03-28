//! CSAプロトコル通信層
//!
//! TCP接続によるCSAサーバーとのテキスト行ベース通信を管理する。

use std::io::{BufRead, BufReader, BufWriter, Write};
use std::net::TcpStream;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::common::csa::{Color, CsaMove, ParsedMove, Position, parse_csa_full};

use super::event::Event;

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
    /// 対局開始前はブロッキング読み取りに使用。`start_reader_thread` 後は None。
    reader: Option<BufReader<TcpStream>>,
    writer: BufWriter<TcpStream>,
    last_activity_time: Instant,
    /// パスワードマスク用
    password: String,
    /// 直前に受信した終局理由行（#TIME_UP 等）
    pub pending_end_reason: Option<String>,
}

impl CsaConnection {
    /// CSAサーバーに接続する
    pub fn connect(host: &str, port: u16, tcp_keepalive: bool) -> Result<Self> {
        let addr_str = format!("{host}:{port}");
        log::info!("[CSA] 接続中: {addr_str}");
        // DNS名を解決し、解決済みアドレスを順に試す（IPv6/IPv4 両対応）
        use std::net::ToSocketAddrs;
        let addrs: Vec<_> = addr_str
            .to_socket_addrs()
            .with_context(|| format!("名前解決失敗: {addr_str}"))?
            .collect();
        if addrs.is_empty() {
            bail!("アドレスが見つかりません: {addr_str}");
        }
        let mut last_err = None;
        let mut stream_opt = None;
        for addr in &addrs {
            log::debug!("[CSA] 接続試行: {addr}");
            match TcpStream::connect_timeout(addr, Duration::from_secs(15)) {
                Ok(s) => {
                    stream_opt = Some(s);
                    break;
                }
                Err(e) => {
                    log::debug!("[CSA] {addr} 接続失敗: {e}");
                    last_err = Some(e);
                }
            }
        }
        let stream = stream_opt.ok_or_else(|| {
            anyhow::anyhow!(
                "CSAサーバー接続失敗: {addr_str} ({}アドレス試行済み): {}",
                addrs.len(),
                last_err.map_or("unknown".to_string(), |e| e.to_string())
            )
        })?;

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
            reader: Some(reader),
            writer,
            last_activity_time: Instant::now(),
            password: String::new(),
            pending_end_reason: None,
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
        self.send_line(csa_move)
    }

    /// 指し手 + floodgate コメント（評価値・PV）を送信する。
    /// コメントは `+7776FU,'* 123 +7776FU -3334FU` のようにカンマ区切りで同一行に付加する。
    pub fn send_move_with_comment(&mut self, csa_move: &str, comment: Option<&str>) -> Result<()> {
        if let Some(c) = comment {
            let line = format!("{csa_move},{c}");
            self.send_line(&line)
        } else {
            self.send_line(csa_move)
        }
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
        if self.last_activity_time.elapsed() >= Duration::from_secs(interval_sec) {
            self.send_raw(b"\n")?;
            self.last_activity_time = Instant::now();
        }
        Ok(())
    }

    fn send_line(&mut self, line: &str) -> Result<()> {
        // パスワードをマスクしてログ出力（非パスワード行ではアロケーション不要）
        if !self.password.is_empty() && line.contains(&self.password) {
            let masked = line.replace(&self.password, "*****");
            log::debug!("[CSA] > {masked}");
        } else {
            log::debug!("[CSA] > {line}");
        }
        self.writer.write_all(line.as_bytes())?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;
        self.last_activity_time = Instant::now();
        Ok(())
    }

    fn send_raw(&mut self, data: &[u8]) -> Result<()> {
        self.writer.write_all(data)?;
        self.writer.flush()?;
        Ok(())
    }

    /// reader への可変参照を取得。start_reader_thread 後は使用不可。
    fn reader_mut(&mut self) -> Result<&mut BufReader<TcpStream>> {
        self.reader
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("reader は start_reader_thread で移動済み"))
    }

    /// サーバー受信を別スレッドに移し、共通チャネルに `Event::ServerLine` を送信する。
    /// 対局開始後に呼ぶ。以降、`recv_move` / `recv_line_*` は使用不可。
    pub fn start_reader_thread(&mut self, tx: mpsc::Sender<Event>) -> Result<()> {
        let mut reader =
            self.reader.take().ok_or_else(|| anyhow::anyhow!("reader は既に移動済み"))?;
        // 読み取りタイムアウトを短くして keep-alive のタイミングを確保
        reader.get_ref().set_read_timeout(Some(Duration::from_millis(500)))?;
        std::thread::Builder::new()
            .name("csa-server-reader".to_string())
            .spawn(move || {
                loop {
                    let mut line = String::new();
                    match reader.read_line(&mut line) {
                        Ok(0) => {
                            let _ = tx.send(Event::ServerDisconnected);
                            break;
                        }
                        Ok(_) => {
                            let trimmed = line.trim_end().to_string();
                            if !trimmed.is_empty() {
                                log::debug!("[CSA] < {trimmed}");
                                if tx.send(Event::ServerLine(trimmed)).is_err() {
                                    break;
                                }
                            }
                            // 空行: keep-alive ping — チャネルには送らない
                        }
                        Err(ref e)
                            if e.kind() == std::io::ErrorKind::WouldBlock
                                || e.kind() == std::io::ErrorKind::TimedOut =>
                        {
                            // タイムアウト: 正常、次のループへ
                        }
                        Err(_) => {
                            let _ = tx.send(Event::ServerDisconnected);
                            break;
                        }
                    }
                }
            })?;
        Ok(())
    }

    /// ブロッキング読み取り（タイムアウト付き）。start_reader_thread 前のみ使用可。
    fn recv_line_blocking(&mut self, timeout: Duration) -> Result<String> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining =
                deadline.checked_duration_since(Instant::now()).unwrap_or(Duration::ZERO);
            if remaining.is_zero() {
                bail!("サーバー応答タイムアウト");
            }
            let reader = self.reader_mut()?;
            reader.get_ref().set_read_timeout(Some(remaining.min(Duration::from_secs(5))))?;
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => bail!("サーバー切断"),
                Ok(n) if n > 0 => {
                    // 空行でもデータ受信なので activity 更新（相手の blank ping 等）
                    self.last_activity_time = Instant::now();
                    let trimmed = line.trim_end().to_string();
                    if !trimmed.is_empty() {
                        log::debug!("[CSA] < {trimmed}");
                        return Ok(trimmed);
                    }
                }
                Ok(_) => bail!("サーバー切断"),
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

    /// ノンブロッキング読み取り。データがなければ Ok(None)。start_reader_thread 前のみ。
    fn recv_line_nonblocking(&mut self) -> Result<Option<String>> {
        let reader = self.reader_mut()?;
        reader.get_ref().set_read_timeout(Some(Duration::from_millis(100)))?;
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => bail!("サーバー切断"),
            Ok(_) => {
                // 空行でもデータ受信なので activity 更新
                self.last_activity_time = Instant::now();
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
