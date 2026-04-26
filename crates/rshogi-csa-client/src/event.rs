//! サーバー/エンジンの統合イベント
//!
//! usiToCsa.rb の `IO.select([$server, $engine])` に相当する仕組み。
//! サーバー受信スレッドとエンジン受信スレッドが共通チャネルにイベントを送信し、
//! メインループで統合的に処理する。

/// サーバーとエンジンの統合イベント
#[derive(Debug)]
pub enum Event {
    /// サーバーから1行受信
    ServerLine(String),
    /// サーバー切断
    ServerDisconnected,
}
