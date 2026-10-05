//! CSA の WebSocket 接続と受信スレッド。

use std::collections::VecDeque;
use std::io::ErrorKind;
use std::net::TcpStream;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use tungstenite::client::IntoClientRequest;
use tungstenite::error::CapacityError;
use tungstenite::handshake::client::Request;
use tungstenite::protocol::WebSocketConfig;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

use super::line_reader::MAX_CSA_LINE_BYTES;
use crate::event::Event;

const MAX_WS_MESSAGE_BYTES: usize = 1024 * 1024;

/// WebSocket 受信の失敗。
///
/// 原因を `source` に載せないのは、文言に原因を含めたまま載せると `{:#}` 表示で
/// 同じ文言が 2 回並ぶため。
#[derive(Debug, thiserror::Error)]
#[error("WebSocket read error: {0}")]
pub struct WsReadError(tungstenite::Error);

impl WsReadError {
    /// frame / message のサイズ上限超過で拒否された場合に `(size, max_size)` を返す。
    pub fn exceeded_size(&self) -> Option<(usize, usize)> {
        match &self.0 {
            tungstenite::Error::Capacity(CapacityError::MessageTooLong { size, max_size }) => {
                Some((*size, *max_size))
            }
            _ => None,
        }
    }
}

/// WebSocket 経路の transport。`websocket` feature 有効時のみ提供。
pub struct WsTransport {
    ws: Arc<Mutex<WebSocket<MaybeTlsStream<TcpStream>>>>,
    /// `start_reader_thread` 後は reader が thread 内で動作する。inline 操作禁止フラグ。
    reader_moved: bool,
    /// CSA サーバ実装は `Game_Summary` のように `\n` 区切りの複数行を 1 つの
    /// text frame で送ってくる（TCP 互換のため文字列全体を `send_with_str` する）。
    /// inline モードではここに 1 frame 分を行ごとに分割して push し、
    /// `read_line_*` が 1 行ずつ pop する。空行 (CSA keep-alive) は捨てる。
    pending_lines: VecDeque<String>,
}

impl WsTransport {
    pub(super) fn connect(url: &str, origin: Option<&str>) -> Result<Self> {
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

        let (ws, response) = tungstenite::client::connect_with_config(
            request,
            Some(
                WebSocketConfig::default()
                    .max_message_size(Some(MAX_WS_MESSAGE_BYTES))
                    .max_frame_size(Some(MAX_WS_MESSAGE_BYTES)),
            ),
            3,
        )
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
            pending_lines: VecDeque::new(),
        })
    }

    fn ensure_inline(&self) -> Result<()> {
        if self.reader_moved {
            bail!("WS reader は start_reader_thread で thread に移動済み");
        }
        Ok(())
    }

    pub(super) fn read_line_blocking(&mut self, timeout: Duration) -> Result<String> {
        self.ensure_inline()?;
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(line) = self.pop_pending_line() {
                log::debug!("[CSA] < {line}");
                return Ok(line);
            }
            let remaining =
                deadline.checked_duration_since(Instant::now()).unwrap_or(Duration::ZERO);
            if remaining.is_zero() {
                bail!("サーバー応答タイムアウト");
            }
            match self.try_read_one_frame()? {
                FrameOutcome::Text(payload) => self.enqueue_frame(&payload)?,
                FrameOutcome::None => {
                    // 50ms 単位でリトライしつつ deadline まで待つ（内部 TcpStream の
                    // read_timeout が 100ms なので、その半分でリトライ間隔を取る）。
                    std::thread::sleep(Duration::from_millis(50).min(remaining));
                }
            }
        }
    }

    pub(super) fn read_line_nonblocking(&mut self) -> Result<Option<String>> {
        self.ensure_inline()?;
        if let Some(line) = self.pop_pending_line() {
            log::debug!("[CSA] < {line}");
            return Ok(Some(line));
        }
        match self.try_read_one_frame()? {
            FrameOutcome::Text(payload) => {
                self.enqueue_frame(&payload)?;
                Ok(self.pop_pending_line().inspect(|line| {
                    log::debug!("[CSA] < {line}");
                }))
            }
            FrameOutcome::None => Ok(None),
        }
    }

    /// 受信した 1 frame の text を `\n` で分割して `pending_lines` に積む。
    /// 末尾の空行は CSA の keep-alive 慣習に従って捨てる（`split` の最後の要素は
    /// `\n` 終端時に必ず空文字になるため）。中間に来る空行も同様に捨てる。
    fn enqueue_frame(&mut self, payload: &str) -> Result<()> {
        self.pending_lines.extend(frame_lines(payload)?);
        Ok(())
    }

    fn pop_pending_line(&mut self) -> Option<String> {
        self.pending_lines.pop_front()
    }

    /// `WebSocket::read` を 1 回だけ非ブロッキングで試行し、得られた text frame
    /// を生のまま返す（`\n` 分割は呼び出し側）。
    fn try_read_one_frame(&mut self) -> Result<FrameOutcome> {
        let mut guard = self.ws.lock().map_err(|_| anyhow!("WS lock poisoned"))?;
        match guard.read() {
            Ok(Message::Text(payload)) => Ok(FrameOutcome::Text(payload.to_string())),
            Ok(Message::Binary(_)) => {
                log::warn!("[CSA/WS] 想定外の binary frame を破棄");
                Ok(FrameOutcome::None)
            }
            Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_)) => Ok(FrameOutcome::None),
            Ok(Message::Close(frame)) => {
                log::info!("[CSA/WS] サーバーから Close frame 受信: {frame:?}");
                bail!("サーバー切断");
            }
            Err(tungstenite::Error::Io(e))
                if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut =>
            {
                Ok(FrameOutcome::None)
            }
            Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                bail!("サーバー切断");
            }
            Err(e) => Err(WsReadError(e).into()),
        }
    }

    pub(super) fn write_line(&mut self, line: &str) -> Result<()> {
        let mut guard = self.ws.lock().map_err(|_| anyhow!("WS lock poisoned"))?;
        guard
            .send(Message::Text(line.to_owned().into()))
            .with_context(|| "WebSocket text frame 送信失敗")?;
        Ok(())
    }

    pub(super) fn start_reader_thread(&mut self, tx: mpsc::Sender<Event>) -> Result<()> {
        if self.reader_moved {
            bail!("WS reader は既に thread に移動済み");
        }
        self.reader_moved = true;
        let ws = Arc::clone(&self.ws);
        // 既に inline で受信して queue に溜まっている行も先に流してから loop に入る。
        let pending = std::mem::take(&mut self.pending_lines);
        std::thread::Builder::new().name("csa-ws-reader".to_string()).spawn(move || {
            for line in pending {
                log::debug!("[CSA] < {line}");
                if tx.send(Event::ServerLine(line)).is_err() {
                    return;
                }
            }
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
                        let lines = match frame_lines(payload.as_str()) {
                            Ok(lines) => lines,
                            Err(error) => {
                                log::warn!("[CSA/WS] 受信行が不正: {error}");
                                let _ = tx.send(Event::ServerDisconnected);
                                break;
                            }
                        };
                        for line in lines {
                            log::debug!("[CSA] < {line}");
                            if tx.send(Event::ServerLine(line)).is_err() {
                                return;
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

/// frame 全体を検証してから行を返し、不正な後続行による部分配送を防ぐ。
fn frame_lines(payload: &str) -> Result<impl Iterator<Item = String> + '_> {
    if payload.len() > MAX_WS_MESSAGE_BYTES {
        bail!("WebSocket message exceeds 1 MiB");
    }
    if payload.split('\n').any(|line| line.len() > MAX_CSA_LINE_BYTES) {
        bail!("CSA line exceeds 64 KiB");
    }
    Ok(payload
        .split('\n')
        .map(|line| line.trim_end_matches('\r'))
        .filter(|line| !line.is_empty())
        .map(str::to_owned))
}

/// `WsTransport::try_read_one_frame` の戻り値。
enum FrameOutcome {
    /// text frame を 1 つ受信した（複数行を含み得る）。
    Text(String),
    /// データなし（WouldBlock / Ping / Pong / Binary 廃棄）。
    None,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_read_error_keeps_single_line_message_and_reports_exceeded_size() {
        let cause = tungstenite::Error::Capacity(CapacityError::MessageTooLong {
            size: MAX_WS_MESSAGE_BYTES + 1,
            max_size: MAX_WS_MESSAGE_BYTES,
        });
        let expected = format!("WebSocket read error: {cause}");
        let error = anyhow::Error::from(WsReadError(cause));

        assert_eq!(error.to_string(), expected);
        assert_eq!(format!("{error:#}"), expected);
        assert_eq!(
            error.downcast_ref::<WsReadError>().and_then(WsReadError::exceeded_size),
            Some((MAX_WS_MESSAGE_BYTES + 1, MAX_WS_MESSAGE_BYTES))
        );
    }

    #[test]
    fn ws_read_error_reports_no_exceeded_size_for_other_causes() {
        let error = WsReadError(tungstenite::Error::Io(ErrorKind::ConnectionReset.into()));
        assert_eq!(error.exceeded_size(), None);
    }
}
