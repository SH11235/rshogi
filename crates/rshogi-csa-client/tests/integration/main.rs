//! 結合テストを 1 つのテストバイナリにまとめる。`tests/*.rs` はファイルごとに別バイナリになり、
//! バイナリごとに依存 crate 込みのリンクが走るため。結合テストはこのディレクトリにモジュールとして追加する。

#[cfg(unix)]
mod common;

mod csa_121;
mod csa_entering_king_rule;
mod csa_reconnect_protocol;
mod csa_tcp_transport;
mod csa_ws_transport;
mod engine_info_cache;
mod engine_stderr_diagnostic;
mod jsonl_output;
mod lib_consumer;
mod rate_limit_retry_after;
mod session_events_integration;
mod session_events_unit;
