//! `tokio::net::TcpStream` を [`rshogi_csa_server::ClientTransport`] として扱うアダプタ。
//!
//! - 受信: `BufReader` で 1 行ずつ読み、末尾の `\n` と 1 個分の `\r` を落として CRLF / LF を吸収する。
//! - 送信: 末尾に CRLF（`\r\n`）を付与する（CSA 1.2.1 仕様）。
//! - 受信タイムアウトは [`tokio::time::timeout`] で包み、期限切れを
//!   [`TransportError::Timeout`] にマップする。
//! - EOF（相手切断）は [`TransportError::Closed`]、その他の I/O 失敗は
//!   [`TransportError::Io`] に変換する。
//!
//! ホットパスでの割り当てを避けるため、行バッファを接続ごとに 1 つ保持し、
//! 使い回す（`read_line` の戻りを parse 後に `clear` する）。

use std::time::Duration;

use rshogi_csa_server::port::ClientTransport;
use rshogi_csa_server::types::{CsaLine, IpKey};
use rshogi_csa_server::{ServerError, TransportError};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};

/// 送信バッファの初期 capacity。通常の CSA 行はこの範囲に収まる。
const SEND_BUF_INIT_CAPACITY: usize = 256;

/// 送信後も保持を許す capacity の上限。巨大な行（長い `%%CHAT` の broadcast 等）で
/// 一時的に伸びた capacity を長寿命接続が持ち続けないよう、超過時は縮める。
const SEND_BUF_MAX_RETAIN: usize = 4096;

/// TCP 接続 1 本分の行 I/O アダプタ。
pub struct TcpTransport {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
    peer: IpKey,
    /// 行確定前のバイト列を累積するバッファ。
    ///
    /// `String` ではなく `Vec<u8>` で保持する理由: TCP 分割によって UTF-8 マルチバイトが
    /// 2 チャンクにまたがったときにチャンク単位で `from_utf8` すると誤った `Io` エラーに
    /// 落ちる。改行バイトで 1 行が確定した時点でだけ UTF-8 検証する。
    line_buf: Vec<u8>,
    /// 送信 1 回分（本文 + CRLF）を組み立てるバッファ。
    ///
    /// 本文と CRLF を別々の `write_all` で送ると 2 syscall / 2 セグメントに
    /// 分かれ、受信側が本文だけを先に観測しうる。1 バッファにまとめて
    /// `write_all` 1 回で送るために接続ごとに保持し、使い回す。
    send_buf: Vec<u8>,
}

impl TcpTransport {
    /// `TcpStream` をラップして新しい [`TcpTransport`] を作る。
    ///
    /// `peer` は `stream.peer_addr()?.ip().to_string()` を `IpKey::new` したもの。
    /// レート制限キーなどで使うため、呼び出し側で明示的に組み立てて渡す。
    pub fn new(stream: TcpStream, peer: IpKey) -> Self {
        let (read_half, write_half) = stream.into_split();
        Self {
            reader: BufReader::new(read_half),
            writer: write_half,
            peer,
            line_buf: Vec::with_capacity(256),
            send_buf: Vec::with_capacity(SEND_BUF_INIT_CAPACITY),
        }
    }

    /// [`TcpStream::peer_addr`] の IP 部分を [`IpKey`] にマップするヘルパ。
    ///
    /// `peer_addr()` が失敗した場合（相手が既に切断されている等）は
    /// [`ServerError::Transport`] を返す。
    pub fn peer_key(stream: &TcpStream) -> Result<IpKey, ServerError> {
        let addr = stream.peer_addr().map_err(|e| TransportError::Io(format!("peer_addr: {e}")))?;
        Ok(IpKey::new(addr.ip().to_string()))
    }
}

impl ClientTransport for TcpTransport {
    async fn recv_line(&mut self, timeout: Duration) -> Result<CsaLine, TransportError> {
        // cancel-safe な `AsyncReadExt::read` で 1 チャンクずつバイト列を [`Self::line_buf`]
        // に累積する。`tokio::select!` で本 future がキャンセルされた場合でも、read は
        // cancel-safe（キャンセル時に読み取り済みバイトは 0）なのでバッファは不整合にならない。
        // UTF-8 検証は改行バイトで 1 行が確定した時点でだけ行う（チャンク境界で
        // マルチバイト文字が分割されても誤った I/O エラーにしない）。
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            // 1. バッファに完全な行があれば切り出して返す（UTF-8 検証はここで行う）。
            if let Some(pos) = self.line_buf.iter().position(|&b| b == b'\n') {
                let mut line_bytes: Vec<u8> = self.line_buf.drain(..=pos).collect();
                // 末尾の `\n` とその直前の `\r` を剥がす。
                line_bytes.pop(); // '\n'
                if line_bytes.last() == Some(&b'\r') {
                    line_bytes.pop();
                }
                let s = std::str::from_utf8(&line_bytes)
                    .map_err(|e| TransportError::Io(format!("utf8: {e}")))?;
                return Ok(CsaLine::new(s));
            }
            // 2. 追加読み取り。残り時間で deadline を計算する。
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Err(TransportError::Timeout);
            }
            let remaining = deadline - now;
            let mut chunk = [0u8; 256];
            let read_res = match tokio::time::timeout(remaining, self.reader.read(&mut chunk)).await
            {
                Ok(r) => r,
                Err(_) => return Err(TransportError::Timeout),
            };
            match read_res {
                Ok(0) => {
                    if self.line_buf.is_empty() {
                        return Err(TransportError::Closed);
                    }
                    // EOF だが末尾行を返す（改行無しの終端行扱い）。
                    let mut line_bytes = std::mem::take(&mut self.line_buf);
                    if line_bytes.last() == Some(&b'\r') {
                        line_bytes.pop();
                    }
                    let s = std::str::from_utf8(&line_bytes)
                        .map_err(|e| TransportError::Io(format!("utf8: {e}")))?;
                    return Ok(CsaLine::new(s));
                }
                Ok(n) => self.line_buf.extend_from_slice(&chunk[..n]),
                Err(e) => return Err(TransportError::Io(format!("read: {e}"))),
            }
        }
    }

    async fn send_line(&mut self, line: &CsaLine) -> Result<(), TransportError> {
        // CSA 1.2.1 は CR+LF を要求する。本文 + CRLF を 1 バッファに組んで
        // `write_all` 1 回で送り、アプリ層起因の分割送信を排除する。
        // なお TCP はストリームなので 1 write でも受信側の read が複数回に
        // 分かれることはあり得る（受信側は行確定・期待長まで読む前提）。
        self.send_buf.clear();
        // 巨大な行（%%CHAT broadcast 等）で伸びた capacity を接続に残さない。
        // clear 直後（= 空）に縮めるので、直前送信の内容量に関わらず安全。
        if self.send_buf.capacity() > SEND_BUF_MAX_RETAIN {
            self.send_buf.shrink_to(SEND_BUF_INIT_CAPACITY);
        }
        self.send_buf.extend_from_slice(line.as_str().as_bytes());
        self.send_buf.extend_from_slice(b"\r\n");
        self.writer
            .write_all(&self.send_buf)
            .await
            .map_err(|e| TransportError::Io(format!("write_all: {e}")))?;
        self.writer
            .flush()
            .await
            .map_err(|e| TransportError::Io(format!("flush: {e}")))?;
        Ok(())
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        self.writer
            .shutdown()
            .await
            .map_err(|e| TransportError::Io(format!("shutdown: {e}")))
    }

    fn peer_id(&self) -> IpKey {
        self.peer.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    /// ループバックで 1 対の `TcpStream` を張るヘルパ。
    /// サーバー側ストリームを `TcpTransport` にラップして返す。
    async fn loopback_pair() -> (TcpTransport, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let client_fut = TcpStream::connect(addr);
        let accept_fut = listener.accept();
        let (client_res, accept_res) = tokio::join!(client_fut, accept_fut);
        let client = client_res.unwrap();
        let (server, _) = accept_res.unwrap();
        let peer = TcpTransport::peer_key(&server).unwrap();
        (TcpTransport::new(server, peer), client)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn recv_line_returns_closed_on_eof() {
        let (mut transport, client) = loopback_pair().await;
        drop(client);
        let err = transport.recv_line(Duration::from_secs(1)).await.unwrap_err();
        assert_eq!(err, TransportError::Closed);
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn recv_line_maps_timeout() {
        let (mut transport, _client) = loopback_pair().await;
        let err = transport.recv_line(Duration::from_millis(10)).await.unwrap_err();
        assert_eq!(err, TransportError::Timeout);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn send_line_appends_crlf() {
        let (mut transport, mut client) = loopback_pair().await;
        transport.send_line(&CsaLine::new("START:g1")).await.unwrap();
        // クライアント側は生 TcpStream で読むので CRLF 込みで比較。
        // 単発 read は TCP 分割で本文だけ返る short read がありうるため、
        // 期待バイト数に達するまで読む `read_exact` を使う。
        // CRLF 欠落の退行時に read_exact がハングしないよう timeout で包む。
        use tokio::io::AsyncReadExt;
        const EXPECTED: &[u8; 10] = b"START:g1\r\n";
        let mut buf = [0u8; EXPECTED.len()];
        tokio::time::timeout(Duration::from_secs(1), client.read_exact(&mut buf))
            .await
            .expect("read_exact timed out")
            .unwrap();
        assert_eq!(&buf, EXPECTED);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn send_line_shrinks_oversized_buffer() {
        // 巨大な行で伸びた send_buf の capacity が、次の送信時に上限まで縮むこと。
        let (mut transport, _client) = loopback_pair().await;
        let big = "X".repeat(SEND_BUF_MAX_RETAIN * 2);
        transport.send_line(&CsaLine::new(&big)).await.unwrap();
        assert!(transport.send_buf.capacity() > SEND_BUF_MAX_RETAIN);
        transport.send_line(&CsaLine::new("PING")).await.unwrap();
        assert!(transport.send_buf.capacity() <= SEND_BUF_MAX_RETAIN);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn peer_id_returns_localhost_key() {
        let (transport, _client) = loopback_pair().await;
        let peer = transport.peer_id();
        // 127.0.0.1 で bind したので、peer の IP 表記は "127.0.0.1" のはず。
        assert_eq!(peer.as_str(), "127.0.0.1");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn consecutive_recv_line_reuses_buffer() {
        // 2 行を一度に送っても、1 行ずつ分解されて返ること（行バッファの再利用確認）。
        let (mut transport, mut client) = loopback_pair().await;
        client.write_all(b"LINE1\r\nLINE2\n").await.unwrap();
        let l1 = transport.recv_line(Duration::from_secs(1)).await.unwrap();
        assert_eq!(l1.as_str(), "LINE1");
        let l2 = transport.recv_line(Duration::from_secs(1)).await.unwrap();
        assert_eq!(l2.as_str(), "LINE2");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn recv_line_handles_utf8_split_across_chunks() {
        // UTF-8 マルチバイト文字が read チャンク境界でまたがっても誤検出しないこと。
        // 「あ」(U+3042) は 3 バイトの UTF-8 "\xE3\x81\x82"。
        // 1 バイト目 → 2 バイト目 → 残り 1 バイト + LF を別 write で送り、
        // 複数 read に分散するようにする。
        let (mut transport, mut client) = loopback_pair().await;
        client.write_all(b"\xE3").await.unwrap();
        client.flush().await.unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        client.write_all(b"\x81").await.unwrap();
        client.flush().await.unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        client.write_all(b"\x82X\n").await.unwrap();
        client.flush().await.unwrap();
        let line = transport.recv_line(Duration::from_secs(2)).await.unwrap();
        assert_eq!(line.as_str(), "あX");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn recv_line_strips_trailing_cr_once() {
        // `\r\n` の直前に余分な CR が来たケース。read_line は `\n` で止まるので
        // 実測は `KEEPALIVE\r\r\n`。末尾の `\n` → `\r` を 1 段だけ落とす運用仕様に
        // 合わせて残り CR は透過させる（CRLF/LF 吸収の対称性を保つ）。
        let (mut transport, mut client) = loopback_pair().await;
        client.write_all(b"KEEPALIVE\r\r\n").await.unwrap();
        let line = transport.recv_line(Duration::from_secs(1)).await.unwrap();
        assert_eq!(line.as_str(), "KEEPALIVE\r");
    }
}
