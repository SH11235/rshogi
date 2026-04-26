//! CSA プロトコル下層 transport（TCP / WebSocket）。
//!
//! - TCP: `host:port` への TcpStream を `BufReader` / `BufWriter` で 1 行ずつ
//!   読み書きする既存実装。同一 socket を `try_clone()` で reader thread に
//!   分配する。
//! - WebSocket: `tungstenite` の sync API で `ws://` / `wss://` URL に接続
//!   し、1 line = 1 text frame の対応で扱う。`Arc<Mutex<WebSocket>>` を共有
//!   して reader thread と writer thread の双方が同じ socket を見る。
//!
//! どちらの経路でも CSA プロトコル本体（行末改行は呼び出し側で除去済み）
//! は文字列スライスで扱う。改行コードは TCP 経路では `write_line` 内部で
//! `\n` を付加し、WS 経路では text frame の境界そのものが行境界になる。

use std::io::{BufRead, BufReader, BufWriter, ErrorKind, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use tungstenite::client::IntoClientRequest;
use tungstenite::handshake::client::Request;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

use super::event::Event;

/// 接続先のスキーム解析結果。`host` 設定文字列から `from_host_port` で生成する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportTarget {
    /// `tcp://host:port` または scheme なし `host` + `port`。
    Tcp { host: String, port: u16 },
    /// `ws://host[:port]/path` または `wss://host[:port]/path`。`port` 設定は無視される。
    WebSocket { url: String },
}

impl TransportTarget {
    /// `server.host` 設定文字列に scheme が含まれていれば優先し、そうでなければ
    /// 既存の `host:port` 形式として TCP 接続先に解釈する。
    pub fn from_host_port(host: &str, port: u16) -> Self {
        if host.starts_with("ws://") || host.starts_with("wss://") {
            Self::WebSocket {
                url: host.to_owned(),
            }
        } else if let Some(rest) = host.strip_prefix("tcp://") {
            Self::Tcp {
                host: rest.to_owned(),
                port,
            }
        } else {
            Self::Tcp {
                host: host.to_owned(),
                port,
            }
        }
    }
}

/// 接続オプション。CLI / TOML から渡される設定をひとまとめにする。
#[derive(Debug, Clone, Default)]
pub struct ConnectOpts {
    /// TCP SO_KEEPALIVE を有効化する（TCP 経路でのみ参照）。
    pub tcp_keepalive: bool,
    /// WebSocket Upgrade 時の Origin ヘッダ値。`None` なら tungstenite の既定値
    /// （`url::Url::origin()`）に任せる。Cloudflare Workers の `CORS_ORIGINS`
    /// allowlist 通過のため、運用時は明示指定する想定。
    pub ws_origin: Option<String>,
}

/// CSA 行を 1 line = 1 message として扱う transport の統一インタフェース。
///
/// `start_reader_thread` を呼ぶまでは inline で `read_line_*` / `write_line` を
/// 使い、対局開始後は reader thread に reader 部分を移して main thread が
/// `write_line` のみを使う運用を想定する。
pub enum CsaTransport {
    Tcp(TcpTransport),
    WebSocket(WsTransport),
}

impl CsaTransport {
    /// 解析済みの `TransportTarget` に対して接続する。
    pub fn connect(target: &TransportTarget, opts: &ConnectOpts) -> Result<Self> {
        match target {
            TransportTarget::Tcp { host, port } => {
                Ok(Self::Tcp(TcpTransport::connect(host, *port, opts.tcp_keepalive)?))
            }
            TransportTarget::WebSocket { url } => {
                Ok(Self::WebSocket(WsTransport::connect(url, opts.ws_origin.as_deref())?))
            }
        }
    }

    /// `timeout` 内に 1 行受信する。空行（keep-alive）は呼び出し側に空文字列で
    /// 上げず、内部でログ更新だけ行ってから次行を待つ。タイムアウト時は `bail!`。
    pub fn read_line_blocking(&mut self, timeout: Duration) -> Result<String> {
        match self {
            Self::Tcp(t) => t.read_line_blocking(timeout),
            Self::WebSocket(w) => w.read_line_blocking(timeout),
        }
    }

    /// 受信データがなければ `Ok(None)`（keep-alive チェック用）。
    pub fn read_line_nonblocking(&mut self) -> Result<Option<String>> {
        match self {
            Self::Tcp(t) => t.read_line_nonblocking(),
            Self::WebSocket(w) => w.read_line_nonblocking(),
        }
    }

    /// 1 行送信する。改行コードは transport 側で適切に付加する。
    pub fn write_line(&mut self, line: &str) -> Result<()> {
        match self {
            Self::Tcp(t) => t.write_line(line),
            Self::WebSocket(w) => w.write_line(line),
        }
    }

    /// CSA の空行 keep-alive。TCP では `\n` 単独、WS では空 text frame を送る。
    pub fn write_keepalive(&mut self) -> Result<()> {
        match self {
            Self::Tcp(t) => t.write_raw(b"\n"),
            Self::WebSocket(w) => w.write_line(""),
        }
    }

    /// 受信ループを別スレッドで起動する。同 transport インスタンスでの
    /// `read_line_*` 呼び出しは以降不可（reader が thread に移動するため）。
    pub fn start_reader_thread(&mut self, tx: mpsc::Sender<Event>) -> Result<()> {
        match self {
            Self::Tcp(t) => t.start_reader_thread(tx),
            Self::WebSocket(w) => w.start_reader_thread(tx),
        }
    }
}

/// TCP 経路の transport。
pub struct TcpTransport {
    /// 対局開始前はブロッキング読み取りに使用。`start_reader_thread` 後は `None`。
    reader: Option<BufReader<TcpStream>>,
    writer: BufWriter<TcpStream>,
}

impl TcpTransport {
    fn connect(host: &str, port: u16, tcp_keepalive: bool) -> Result<Self> {
        let addr_str = format!("{host}:{port}");
        log::info!("[CSA/TCP] 接続中: {addr_str}");
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
            log::debug!("[CSA/TCP] 接続試行: {addr}");
            match TcpStream::connect_timeout(addr, Duration::from_secs(15)) {
                Ok(s) => {
                    stream_opt = Some(s);
                    break;
                }
                Err(e) => {
                    log::debug!("[CSA/TCP] {addr} 接続失敗: {e}");
                    last_err = Some(e);
                }
            }
        }
        let stream = stream_opt.ok_or_else(|| {
            anyhow!(
                "CSAサーバー接続失敗: {addr_str} ({}アドレス試行済み): {}",
                addrs.len(),
                last_err.map_or("unknown".to_string(), |e| e.to_string())
            )
        })?;

        if tcp_keepalive {
            set_tcp_keepalive(&stream)?;
        }
        let _ = stream.set_nodelay(true);
        stream.set_read_timeout(Some(Duration::from_secs(30)))?;

        let reader = BufReader::new(stream.try_clone()?);
        let writer = BufWriter::new(stream);

        Ok(Self {
            reader: Some(reader),
            writer,
        })
    }

    fn reader_mut(&mut self) -> Result<&mut BufReader<TcpStream>> {
        self.reader
            .as_mut()
            .ok_or_else(|| anyhow!("reader は start_reader_thread で移動済み"))
    }

    fn read_line_blocking(&mut self, timeout: Duration) -> Result<String> {
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
                    let trimmed = line.trim_end().to_string();
                    if !trimmed.is_empty() {
                        log::debug!("[CSA] < {trimmed}");
                        return Ok(trimmed);
                    }
                }
                Ok(_) => bail!("サーバー切断"),
                Err(ref e)
                    if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut =>
                {
                    continue;
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    fn read_line_nonblocking(&mut self) -> Result<Option<String>> {
        let reader = self.reader_mut()?;
        reader.get_ref().set_read_timeout(Some(Duration::from_millis(100)))?;
        let mut line = String::new();
        match reader.read_line(&mut line) {
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
            Err(ref e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {
                Ok(None)
            }
            Err(e) => Err(e.into()),
        }
    }

    fn write_line(&mut self, line: &str) -> Result<()> {
        self.writer.write_all(line.as_bytes())?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;
        Ok(())
    }

    fn write_raw(&mut self, data: &[u8]) -> Result<()> {
        self.writer.write_all(data)?;
        self.writer.flush()?;
        Ok(())
    }

    fn start_reader_thread(&mut self, tx: mpsc::Sender<Event>) -> Result<()> {
        let mut reader = self.reader.take().ok_or_else(|| anyhow!("reader は既に移動済み"))?;
        reader.get_ref().set_read_timeout(Some(Duration::from_millis(500)))?;
        std::thread::Builder::new().name("csa-tcp-reader".to_string()).spawn(move || {
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
                    }
                    Err(ref e)
                        if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut =>
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
}

/// WebSocket 経路の transport。
pub struct WsTransport {
    ws: Arc<Mutex<WebSocket<MaybeTlsStream<TcpStream>>>>,
    /// `start_reader_thread` 後は reader が thread 内で動作する。inline 操作禁止フラグ。
    reader_moved: bool,
}

impl WsTransport {
    fn connect(url: &str, origin: Option<&str>) -> Result<Self> {
        log::info!("[CSA/WS] 接続中: {url}");
        let mut request: Request = url
            .into_client_request()
            .with_context(|| format!("WebSocket URL のパース失敗: {url}"))?;
        if let Some(origin_value) = origin {
            let header_value = origin_value
                .parse()
                .with_context(|| format!("Origin ヘッダ値が不正: {origin_value}"))?;
            request.headers_mut().insert("Origin", header_value);
        }

        let (ws, response) = tungstenite::connect(request)
            .with_context(|| format!("WebSocket Upgrade 失敗: {url}"))?;
        log::info!("[CSA/WS] 接続成功: status={}", response.status());

        // 内部 TcpStream に短い read_timeout を設定し、reader thread でも main
        // thread でも read_message が long-block しないようにする。
        if let Some(stream) = stream_of_ws(&ws) {
            let _ = stream.set_nodelay(true);
            stream.set_read_timeout(Some(Duration::from_millis(100)))?;
        }

        Ok(Self {
            ws: Arc::new(Mutex::new(ws)),
            reader_moved: false,
        })
    }

    fn ensure_inline(&self) -> Result<()> {
        if self.reader_moved {
            bail!("WS reader は start_reader_thread で thread に移動済み");
        }
        Ok(())
    }

    fn read_line_blocking(&mut self, timeout: Duration) -> Result<String> {
        self.ensure_inline()?;
        let deadline = Instant::now() + timeout;
        loop {
            let remaining =
                deadline.checked_duration_since(Instant::now()).unwrap_or(Duration::ZERO);
            if remaining.is_zero() {
                bail!("サーバー応答タイムアウト");
            }
            match self.try_read_one_message()? {
                Some(line) => {
                    if !line.is_empty() {
                        log::debug!("[CSA] < {line}");
                        return Ok(line);
                    }
                    // 空 text frame は keep-alive 扱いで読み飛ばす。
                }
                None => {
                    // 50ms 単位でリトライしつつ deadline まで待つ（内部 TcpStream の
                    // read_timeout が 100ms なので、その半分でリトライ間隔を取る）。
                    std::thread::sleep(Duration::from_millis(50).min(remaining));
                }
            }
        }
    }

    fn read_line_nonblocking(&mut self) -> Result<Option<String>> {
        self.ensure_inline()?;
        match self.try_read_one_message()? {
            Some(line) if !line.is_empty() => {
                log::debug!("[CSA] < {line}");
                Ok(Some(line))
            }
            _ => Ok(None),
        }
    }

    /// `WebSocket::read` を 1 回だけ非ブロッキングで試行する。
    /// `Ok(Some(line))`: text frame を 1 つ受信した（空文字含む）。
    /// `Ok(None)`: WouldBlock / Pong / Ping をハンドリング後にデータなし。
    /// `Err(_)`: 切断 / プロトコル違反など回復不能。
    fn try_read_one_message(&mut self) -> Result<Option<String>> {
        let mut guard = self.ws.lock().map_err(|_| anyhow!("WS lock poisoned"))?;
        match guard.read() {
            Ok(Message::Text(payload)) => Ok(Some(payload.to_string())),
            Ok(Message::Binary(_)) => {
                log::warn!("[CSA/WS] 想定外の binary frame を破棄");
                Ok(None)
            }
            Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_)) => Ok(None),
            Ok(Message::Close(frame)) => {
                log::info!("[CSA/WS] サーバーから Close frame 受信: {frame:?}");
                bail!("サーバー切断");
            }
            Err(tungstenite::Error::Io(e))
                if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut =>
            {
                Ok(None)
            }
            Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                bail!("サーバー切断");
            }
            Err(e) => Err(anyhow!("WebSocket read error: {e}")),
        }
    }

    fn write_line(&mut self, line: &str) -> Result<()> {
        let mut guard = self.ws.lock().map_err(|_| anyhow!("WS lock poisoned"))?;
        guard
            .send(Message::Text(line.to_owned().into()))
            .with_context(|| "WebSocket text frame 送信失敗")?;
        Ok(())
    }

    fn start_reader_thread(&mut self, tx: mpsc::Sender<Event>) -> Result<()> {
        if self.reader_moved {
            bail!("WS reader は既に thread に移動済み");
        }
        self.reader_moved = true;
        let ws = Arc::clone(&self.ws);
        std::thread::Builder::new().name("csa-ws-reader".to_string()).spawn(move || {
            loop {
                let next = {
                    let mut guard = match ws.lock() {
                        Ok(g) => g,
                        Err(_) => {
                            let _ = tx.send(Event::ServerDisconnected);
                            break;
                        }
                    };
                    guard.read()
                };
                match next {
                    Ok(Message::Text(payload)) => {
                        let line = payload.to_string();
                        if !line.is_empty() {
                            log::debug!("[CSA] < {line}");
                            if tx.send(Event::ServerLine(line)).is_err() {
                                break;
                            }
                        }
                    }
                    Ok(Message::Binary(_)) => {
                        log::warn!("[CSA/WS] 想定外の binary frame を破棄");
                    }
                    Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_)) => {}
                    Ok(Message::Close(_)) => {
                        let _ = tx.send(Event::ServerDisconnected);
                        break;
                    }
                    Err(tungstenite::Error::Io(e))
                        if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut =>
                    {
                        // 短 timeout で抜けて lock を release し、writer に
                        // 進行のチャンスを与える。
                        std::thread::sleep(Duration::from_millis(20));
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
}

/// `tungstenite::WebSocket` 内部の `TcpStream` に到達して read_timeout を設定する
/// ためのヘルパ。`MaybeTlsStream` の variant に応じて適切な参照を返す。
fn stream_of_ws(ws: &WebSocket<MaybeTlsStream<TcpStream>>) -> Option<&TcpStream> {
    match ws.get_ref() {
        MaybeTlsStream::Plain(s) => Some(s),
        MaybeTlsStream::Rustls(s) => Some(s.get_ref()),
        _ => None,
    }
}

/// TCP SO_KEEPALIVE を有効化する（既存実装と同等）。
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_parses_tcp_default() {
        let t = TransportTarget::from_host_port("wdoor.c.u-tokyo.ac.jp", 4081);
        assert_eq!(
            t,
            TransportTarget::Tcp {
                host: "wdoor.c.u-tokyo.ac.jp".to_owned(),
                port: 4081
            }
        );
    }

    #[test]
    fn target_parses_explicit_tcp_scheme() {
        let t = TransportTarget::from_host_port("tcp://floodgate.example", 4081);
        assert_eq!(
            t,
            TransportTarget::Tcp {
                host: "floodgate.example".to_owned(),
                port: 4081
            }
        );
    }

    #[test]
    fn target_parses_ws_and_wss() {
        let t = TransportTarget::from_host_port("ws://localhost:8787/ws/room1", 0);
        assert_eq!(
            t,
            TransportTarget::WebSocket {
                url: "ws://localhost:8787/ws/room1".to_owned()
            }
        );

        let t = TransportTarget::from_host_port(
            "wss://rshogi-csa-server-workers-staging.example.workers.dev/ws/room1",
            0,
        );
        assert_eq!(
            t,
            TransportTarget::WebSocket {
                url: "wss://rshogi-csa-server-workers-staging.example.workers.dev/ws/room1"
                    .to_owned()
            }
        );
    }
}
