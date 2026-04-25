//! `init_prometheus_exporter` の起動契約 integration test。
//!
//! 「`describe_*` 登録 + ラベル無し主要 5 系列のゼロ初期化により、起動直後の
//! `/metrics` に HELP/TYPE と初期 0 値が出る」契約は外部 dashboard / アラート
//! クエリの前提なので、unit test (`metric_names_are_byte_stable` など) では
//! 担保できない。本 integration test で実 HTTP listener を立てて応答を検証する。
//!
//! `metrics::set_global_recorder` はプロセス内で 1 回しか呼べないため、本ファイル
//! には **1 テストだけ** 置く。`cargo test` は integration test ファイルごとに
//! 別プロセスを spawn するため、同ファイル内で複数の install を試みない限り
//! 衝突しない。
//!
//! HTTP クライアント依存（`reqwest` 等）は導入せず、`tokio::net::TcpStream` で
//! 直接 HTTP/1.0 リクエストを送る。dev-dependency を増やさない方針。

use std::net::SocketAddr;
use std::time::Duration;

use rshogi_csa_server_tcp::metrics::init_prometheus_exporter;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// 空き port を `127.0.0.1` で予約してそのアドレスを返す。
///
/// 一度 bind した listener は drop するので、`init_prometheus_exporter` が
/// 同じ port に再 bind できる（同プロセス内なら TIME_WAIT もほぼ無視できる）。
/// テストの並列実行（別プロセス）でも port 衝突しないよう毎回エフェメラルで取る。
fn ephemeral_loopback_addr() -> SocketAddr {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    let addr = listener.local_addr().expect("local_addr");
    drop(listener);
    addr
}

/// HTTP/1.0 GET `/metrics` を送って body を返す。exporter が runtime spawn 中で
/// まだ listening していない race を吸収するため、短いリトライ待ちを入れる。
async fn fetch_metrics(addr: SocketAddr) -> String {
    let request = format!("GET /metrics HTTP/1.0\r\nHost: {addr}\r\n\r\n");
    for _ in 0..100 {
        match TcpStream::connect(addr).await {
            Ok(mut stream) => {
                if stream.write_all(request.as_bytes()).await.is_err() {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    continue;
                }
                let mut buf = Vec::new();
                if stream.read_to_end(&mut buf).await.is_err() {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    continue;
                }
                let raw = String::from_utf8_lossy(&buf).into_owned();
                if let Some(body_start) = raw.find("\r\n\r\n") {
                    let body = raw[body_start + 4..].to_owned();
                    if !body.is_empty() {
                        return body;
                    }
                }
            }
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    }
    panic!("metrics endpoint at {addr} did not return a non-empty body within 2s");
}

/// 起動直後の `/metrics` 応答契約:
///
/// - 主要 5 系列（counter 3 / gauge 2）が `# HELP` / `# TYPE` 行付きで現れる
/// - ラベル無しの値が初期 `0` で出る（Prometheus の `rate(...)` / `absent(...)`
///   クエリが起動直後でも展開できる）
///
/// histogram (`csa_move_latency_seconds`) は意図的に事前 record しないため、
/// 値発火前は出力に出ない（観測値の汚染を避ける契約）。本テストでは含めない。
#[tokio::test(flavor = "current_thread")]
async fn exporter_serves_help_type_and_zero_init_for_main_series() {
    let addr = ephemeral_loopback_addr();
    init_prometheus_exporter(addr).expect("install exporter must succeed on a free loopback port");

    let body = fetch_metrics(addr).await;

    let main_series = [
        "csa_connections_total",
        "csa_connections_active",
        "csa_games_total",
        "csa_games_active",
        "csa_time_up_total",
    ];
    for series in main_series {
        assert!(
            body.contains(&format!("# HELP {series} ")),
            "missing # HELP line for {series} in /metrics body:\n{body}"
        );
        assert!(
            body.contains(&format!("# TYPE {series} ")),
            "missing # TYPE line for {series} in /metrics body:\n{body}"
        );
        // ラベル無しの初期値 `<series> 0` が単独行として出ること。`{label="..."} 0`
        // ではなく裸の `series 0` 行を要求することで、ゼロ初期化が確実に効いて
        // いる（NoOp recorder ではない）ことを併せて確認する。
        let zero_line = format!("\n{series} 0\n");
        assert!(
            body.contains(&zero_line),
            "missing initial zero value `{zero_line:?}` for {series} in /metrics body:\n{body}"
        );
    }
}
