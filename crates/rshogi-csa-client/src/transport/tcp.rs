//! CSA の TCP 接続と受信スレッド。

use super::line_reader::LineReader;
use crate::event::Event;
use anyhow::{Context, Result, anyhow, bail};
use std::io::{BufReader, BufWriter, ErrorKind, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// TCP 経路の transport。
pub struct TcpTransport {
    /// 対局開始前はブロッキング読み取りに使用。`start_reader_thread` 後は `None`。
    reader: Option<LineReader<BufReader<TcpStream>>>,
    writer: BufWriter<TcpStream>,
}

impl Drop for TcpTransport {
    fn drop(&mut self) {
        // reader の複製も同じソケットを参照するため、shutdown で受信待ちを解除する。
        // 切断済みの場合のエラーは無視し、reader thread は EOF または読み取りエラーで終了する。
        let _ = self.writer.get_ref().shutdown(Shutdown::Both);
    }
}

impl TcpTransport {
    pub(super) fn connect(host: &str, port: u16, tcp_keepalive: bool) -> Result<Self> {
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
            reader: Some(LineReader::new(reader)),
            writer,
        })
    }

    fn reader_mut(&mut self) -> Result<&mut LineReader<BufReader<TcpStream>>> {
        self.reader
            .as_mut()
            .ok_or_else(|| anyhow!("reader は start_reader_thread で移動済み"))
    }

    pub(super) fn read_line_blocking(&mut self, timeout: Duration) -> Result<String> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining =
                deadline.checked_duration_since(Instant::now()).unwrap_or(Duration::ZERO);
            if remaining.is_zero() {
                bail!("サーバー応答タイムアウト");
            }
            let reader = self.reader_mut()?;
            reader
                .get_ref()
                .get_ref()
                .set_read_timeout(Some(remaining.min(Duration::from_secs(5))))?;
            match reader.read_line() {
                Ok(None) => bail!("サーバー切断"),
                Ok(Some(line)) => {
                    let trimmed = line.trim_end().to_string();
                    if !trimmed.is_empty() {
                        log::debug!("[CSA] < {trimmed}");
                        return Ok(trimmed);
                    }
                }
                Err(ref e)
                    if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut =>
                {
                    continue;
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    pub(super) fn read_line_nonblocking(&mut self) -> Result<Option<String>> {
        let reader = self.reader_mut()?;
        reader.get_ref().get_ref().set_read_timeout(Some(Duration::from_millis(100)))?;
        match reader.read_line() {
            Ok(None) => bail!("サーバー切断"),
            Ok(Some(line)) => {
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

    pub(super) fn write_line(&mut self, line: &str) -> Result<()> {
        self.writer.write_all(line.as_bytes())?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;
        Ok(())
    }

    pub(super) fn write_raw(&mut self, data: &[u8]) -> Result<()> {
        self.writer.write_all(data)?;
        self.writer.flush()?;
        Ok(())
    }

    pub(super) fn start_reader_thread(&mut self, tx: mpsc::Sender<Event>) -> Result<()> {
        let mut reader = self.reader.take().ok_or_else(|| anyhow!("reader は既に移動済み"))?;
        reader.get_ref().get_ref().set_read_timeout(Some(Duration::from_millis(500)))?;
        std::thread::Builder::new().name("csa-tcp-reader".to_string()).spawn(move || {
            loop {
                match reader.read_line() {
                    Ok(None) => {
                        let _ = tx.send(Event::ServerDisconnected);
                        break;
                    }
                    Ok(Some(line)) => {
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
                    Err(e) => {
                        log::warn!("[CSA/TCP] 受信エラー: {e}");
                        let _ = tx.send(Event::ServerDisconnected);
                        break;
                    }
                }
            }
        })?;
        Ok(())
    }
}

/// TCP SO_KEEPALIVE を有効化する。
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
