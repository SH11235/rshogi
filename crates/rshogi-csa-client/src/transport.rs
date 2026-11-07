//! CSA プロトコル下層 transport（TCP / WebSocket）。
//!
//! - TCP: `host:port` への TcpStream を `BufReader` / `BufWriter` で 1 行ずつ
//!   読み書きする。同一 socket を `try_clone()` で reader thread に
//!   分配する。
//! - WebSocket: `tungstenite` の sync API で `ws://` / `wss://` URL に接続
//!   し、1 line = 1 text frame の対応で扱う。`Arc<Mutex<WebSocket>>` を共有
//!   して reader thread と writer thread の双方が同じ socket を見る。
//!
//! どちらの経路でも CSA プロトコル本体（行末改行は呼び出し側で除去済み）
//! は文字列スライスで扱う。改行コードは TCP 経路では `write_line` 内部で
//! `\n` を付加し、WS 経路では text frame の境界そのものが行境界になる。

use crate::event::Event;
use anyhow::Result;
#[cfg(not(feature = "websocket"))]
use anyhow::bail;
use std::sync::mpsc;
use std::time::Duration;

mod line_reader;
mod tcp;
#[cfg(feature = "websocket")]
mod websocket;

pub use tcp::TcpTransport;
#[cfg(feature = "websocket")]
pub use websocket::{WsReadError, WsTransport};

/// 接続先のスキーム解析結果。`host` 設定文字列から `from_host_port` で生成する。
///
/// `WebSocket` バリアントは `websocket` feature 有効時のみ存在する。feature OFF で
/// `ws://` / `wss://` URL を渡すと `from_host_port` が `Err` を返す。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportTarget {
    /// `tcp://host:port` または scheme なし `host` + `port`。
    Tcp { host: String, port: u16 },
    /// `ws://host[:port]/path` または `wss://host[:port]/path`。`port` 設定は無視される。
    #[cfg(feature = "websocket")]
    WebSocket { url: String },
}

impl TransportTarget {
    /// `server.host` 設定文字列に scheme が含まれていれば優先し、そうでなければ
    /// 既存の `host:port` 形式として TCP 接続先に解釈する。
    ///
    /// `websocket` feature OFF で `ws://` / `wss://` を渡した場合は `Err` を返す。
    pub fn from_host_port(host: &str, port: u16) -> Result<Self> {
        if host.starts_with("ws://") || host.starts_with("wss://") {
            // 片方の cfg block だけがコンパイルされる: feature ON で `WebSocket`
            // バリアントを返し、OFF では明示エラーで bail する。
            #[cfg(feature = "websocket")]
            {
                return Ok(Self::WebSocket {
                    url: host.to_owned(),
                });
            }
            #[cfg(not(feature = "websocket"))]
            {
                bail!(
                    "WebSocket scheme `{host}` was provided but the `websocket` feature is disabled"
                );
            }
        }
        if let Some(rest) = host.strip_prefix("tcp://") {
            return Ok(Self::Tcp {
                host: rest.to_owned(),
                port,
            });
        }
        Ok(Self::Tcp {
            host: host.to_owned(),
            port,
        })
    }
}

/// 接続オプション。CLI / TOML から渡される設定をひとまとめにする。
#[derive(Debug, Clone, Default)]
pub struct ConnectOpts {
    /// TCP SO_KEEPALIVE を有効化する（TCP 経路でのみ参照）。
    pub tcp_keepalive: bool,
    /// 送ったデータが確認応答されないまま経過したら接続を失敗させる時間
    /// (Linux の `TCP_USER_TIMEOUT`)。`None` なら OS 既定。TCP・WebSocket の両経路で使う。
    pub unacked_timeout: Option<Duration>,
    /// WebSocket Upgrade 時の Origin ヘッダ値。`None` なら送信しない。
    /// 指定する場合は接続先の `WS_ALLOWED_ORIGINS` に含まれる値を使う。
    pub ws_origin: Option<String>,
}

/// CSA 行を 1 line = 1 message として扱う transport の統一インタフェース。
///
/// 受信行は LF を除いて最大 64 KiB。WebSocket の frame と message は
/// 最大 1 MiB に制限し、超過した入力はエラーにする。
///
/// `start_reader_thread` を呼ぶまでは inline で `read_line_*` / `write_line` を
/// 使い、対局開始後は reader thread に reader 部分を移して main thread が
/// `write_line` のみを使う運用を想定する。
///
/// `WebSocket` バリアントは `websocket` feature 有効時のみ存在する。
pub enum CsaTransport {
    Tcp(TcpTransport),
    #[cfg(feature = "websocket")]
    WebSocket(WsTransport),
}

impl CsaTransport {
    /// 解析済みの `TransportTarget` に対して接続する。
    pub fn connect(target: &TransportTarget, opts: &ConnectOpts) -> Result<Self> {
        match target {
            TransportTarget::Tcp { host, port } => Ok(Self::Tcp(TcpTransport::connect(
                host,
                *port,
                opts.tcp_keepalive,
                opts.unacked_timeout,
            )?)),
            #[cfg(feature = "websocket")]
            TransportTarget::WebSocket { url } => Ok(Self::WebSocket(WsTransport::connect(
                url,
                opts.ws_origin.as_deref(),
                opts.unacked_timeout,
            )?)),
        }
    }

    /// `timeout` 内に 1 行受信する。空行（keep-alive）は呼び出し側に空文字列で
    /// 上げず、内部でログ更新だけ行ってから次行を待つ。タイムアウト時は `bail!`。
    pub fn read_line_blocking(&mut self, timeout: Duration) -> Result<String> {
        match self {
            Self::Tcp(t) => t.read_line_blocking(timeout),
            #[cfg(feature = "websocket")]
            Self::WebSocket(w) => w.read_line_blocking(timeout),
        }
    }

    /// 受信データがなければ `Ok(None)`（keep-alive チェック用）。
    pub fn read_line_nonblocking(&mut self) -> Result<Option<String>> {
        match self {
            Self::Tcp(t) => t.read_line_nonblocking(),
            #[cfg(feature = "websocket")]
            Self::WebSocket(w) => w.read_line_nonblocking(),
        }
    }

    /// 1 行送信する。改行コードは transport 側で適切に付加する。
    pub fn write_line(&mut self, line: &str) -> Result<()> {
        match self {
            Self::Tcp(t) => t.write_line(line),
            #[cfg(feature = "websocket")]
            Self::WebSocket(w) => w.write_line(line),
        }
    }

    /// CSA の空行 keep-alive。TCP では `\n` 単独、WS では空 text frame を送る。
    pub fn write_keepalive(&mut self) -> Result<()> {
        match self {
            Self::Tcp(t) => t.write_raw(b"\n"),
            #[cfg(feature = "websocket")]
            Self::WebSocket(w) => w.write_line(""),
        }
    }

    /// 受信ループを別スレッドで起動する。同 transport インスタンスでの
    /// `read_line_*` 呼び出しは以降不可（reader が thread に移動するため）。
    pub fn start_reader_thread(&mut self, tx: mpsc::Sender<Event>) -> Result<()> {
        match self {
            Self::Tcp(t) => t.start_reader_thread(tx),
            #[cfg(feature = "websocket")]
            Self::WebSocket(w) => w.start_reader_thread(tx),
        }
    }
}

/// 経路の途中でパケットが捨てられると、keep-alive の書き込みはカーネルの送信バッファに
/// 入って成功し続け、再送をあきらめるまで (Linux 既定の `tcp_retries2=15` で約 15 分)
/// 切断に気づけない。確認応答の来ない時間に上限を掛けて、読み書きを `ETIMEDOUT` で失敗させる。
#[cfg(any(target_os = "linux", target_os = "android"))]
pub(crate) fn set_unacked_timeout(stream: &std::net::TcpStream, timeout: Duration) {
    use std::os::unix::io::AsRawFd;
    let ms = libc::c_uint::try_from(timeout.as_millis()).unwrap_or(libc::c_uint::MAX);
    // SAFETY: fd は有効なソケット。ms は有効なポインタで、長さは c_uint。
    let ret = unsafe {
        libc::setsockopt(
            stream.as_raw_fd(),
            libc::IPPROTO_TCP,
            libc::TCP_USER_TIMEOUT,
            &ms as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_uint>() as libc::socklen_t,
        )
    };
    if ret != 0 {
        log::warn!("TCP_USER_TIMEOUT 設定失敗: {}", std::io::Error::last_os_error());
    }
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
pub(crate) fn set_unacked_timeout(_stream: &std::net::TcpStream, _timeout: Duration) {}

#[cfg(test)]
mod tests {

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn unacked_timeout_is_set_on_the_socket() {
        use std::os::unix::io::AsRawFd;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let stream = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        set_unacked_timeout(&stream, Duration::from_millis(1500));
        let mut ms: libc::c_uint = 0;
        let mut len = std::mem::size_of::<libc::c_uint>() as libc::socklen_t;
        // SAFETY: fd は有効なソケット。ms と len は有効なポインタ。
        let ret = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::IPPROTO_TCP,
                libc::TCP_USER_TIMEOUT,
                &mut ms as *mut _ as *mut libc::c_void,
                &mut len,
            )
        };
        assert_eq!(ret, 0);
        assert_eq!(ms, 1500);
    }
    use super::*;

    #[test]
    fn target_parses_tcp_default() {
        let t = TransportTarget::from_host_port("wdoor.c.u-tokyo.ac.jp", 4081).unwrap();
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
        let t = TransportTarget::from_host_port("tcp://floodgate.example", 4081).unwrap();
        assert_eq!(
            t,
            TransportTarget::Tcp {
                host: "floodgate.example".to_owned(),
                port: 4081
            }
        );
    }

    #[cfg(feature = "websocket")]
    #[test]
    fn target_parses_ws_and_wss() {
        let t = TransportTarget::from_host_port("ws://localhost:8787/ws/room1", 0).unwrap();
        assert_eq!(
            t,
            TransportTarget::WebSocket {
                url: "ws://localhost:8787/ws/room1".to_owned()
            }
        );

        let t = TransportTarget::from_host_port(
            "wss://rshogi-csa-server-workers-staging.example.workers.dev/ws/room1",
            0,
        )
        .unwrap();
        assert_eq!(
            t,
            TransportTarget::WebSocket {
                url: "wss://rshogi-csa-server-workers-staging.example.workers.dev/ws/room1"
                    .to_owned()
            }
        );
    }

    #[cfg(not(feature = "websocket"))]
    #[test]
    fn target_rejects_ws_without_feature() {
        let err = TransportTarget::from_host_port("ws://localhost:8787/ws/room1", 0).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("websocket"), "expected websocket-related error, got: {msg}");
    }
}
