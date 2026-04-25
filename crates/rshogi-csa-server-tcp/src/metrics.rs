//! Prometheus 互換メトリクスの名前と意味の集約点。
//!
//! 配置箇所（accept ループ・対局タスク・指し手ハンドラ）は本モジュールの
//! 定数を経由して `metrics::counter!` / `gauge!` / `histogram!` を呼ぶ。
//! 名前と意味の単一ソースを保ち、後で系列の追加や rename をしたときに
//! 配信側（Prometheus / Grafana / アラート）と整合を保ちやすくするため。
//!
//! recorder の install / uninstall は [`init_prometheus_exporter`] が担当する。
//! 未 install 状態（`--metrics-bind` 未指定で起動した場合）でも `metrics`
//! crate の facade は NoOp で動作するため、本モジュールの記録ポイントは
//! 起動オプションに無関係に呼んで良い。
//!
//! # 命名規約
//!
//! - prefix `csa_` で本サーバ系列を分離
//! - 単位を suffix で明示（`_seconds`, `_total`）
//! - counter は `_total`、gauge は名詞 / 形容詞、histogram は単位付き
//! - ラベルは ASCII 小文字 snake_case、値は安定セット（既知列挙）に限る

use std::net::SocketAddr;

/// 同時接続数。`accept_loop` で増減する gauge。
pub const CONNECTIONS_ACTIVE: &str = "csa_connections_active";

/// 累計接続数。`accept_loop` で接続を受理した回数の counter。
pub const CONNECTIONS_TOTAL: &str = "csa_connections_total";

/// 進行中対局数。`drive_game` の RAII ガード成立時に増、Drop で減らす gauge。
pub const GAMES_ACTIVE: &str = "csa_games_active";

/// 累計対局数。`drive_game` 開始時の counter。
pub const GAMES_TOTAL: &str = "csa_games_total";

/// 終局確定回数の counter。`result_code` ラベルで `#RESIGN` / `#TIME_UP` /
/// `#ILLEGAL_MOVE` / `#JISHOGI` / `#OUTE_SENNICHITE` / `#SENNICHITE` /
/// `#MAX_MOVES` / `#ABNORMAL` を分類する。
pub const GAMES_FINISHED_TOTAL: &str = "csa_games_finished_total";

/// 時間切れ確定回数の counter。`GAMES_FINISHED_TOTAL{result_code="#TIME_UP"}` と
/// 一致するが、運用 SLO ダッシュボードでよく見るため独立カウンタとして保持する。
pub const TIME_UP_TOTAL: &str = "csa_time_up_total";

/// 指し手レイテンシ histogram (秒)。`drive_game` で 1 手の handle_line を呼ぶ
/// 区間を計測する（受信→解釈→broadcast 配信まで）。histogram の bucket は
/// `metrics-exporter-prometheus` の既定（指数分布）に従う。
pub const MOVE_LATENCY_SECONDS: &str = "csa_move_latency_seconds";

/// `metrics-exporter-prometheus` の Prometheus exporter を install し、
/// 系列の `# HELP` / `# TYPE` を事前登録する。
///
/// 別 thread で multi-threaded Tokio runtime を立てて HTTP listener を bind
/// するため、本クレートが採用している `current_thread` + `LocalSet` 設計には
/// 影響しない。listener は process 終了時に runtime ごと drop される。
///
/// `--metrics-bind` 未指定時は本関数を呼ばず、recorder は install されない。
/// その場合 `metrics::counter!` 等は NoOp で動き、計測点を呼ぶオーバーヘッドは
/// 数 ns 程度に収まる（NoOp recorder の atomic 1 回分）。
///
/// `describe_*` で事前登録しておくと、起動直後の `/metrics` 応答にも HELP/TYPE
/// 行が含まれ、Prometheus 側のアラートクエリが「系列が一度も観測されていない」
/// 初期状態でも展開できる。観測値が来てから初めて系列が現れるレース条件を防ぐ
/// ための運用標準。
pub fn init_prometheus_exporter(addr: SocketAddr) -> Result<(), MetricsError> {
    use metrics_exporter_prometheus::PrometheusBuilder;
    PrometheusBuilder::new()
        .with_http_listener(addr)
        .install()
        .map_err(|e| MetricsError::Install(e.to_string()))?;

    metrics::describe_counter!(
        CONNECTIONS_TOTAL,
        "Total number of CSA TCP connections accepted since process start"
    );
    metrics::describe_gauge!(
        CONNECTIONS_ACTIVE,
        "Currently open CSA TCP connections (incremented on accept, decremented on task drop)"
    );
    metrics::describe_counter!(
        GAMES_TOTAL,
        "Total number of CSA games started since process start"
    );
    metrics::describe_gauge!(
        GAMES_ACTIVE,
        "Currently in-progress CSA games (covers play loop and kifu/00LIST persistence epilogue)"
    );
    metrics::describe_counter!(
        GAMES_FINISHED_TOTAL,
        "Total number of CSA games finished, partitioned by `result_code` label"
    );
    metrics::describe_counter!(
        TIME_UP_TOTAL,
        "Total number of games ended by time-up (subset of csa_games_finished_total{result_code=\"#TIME_UP\"})"
    );
    metrics::describe_histogram!(
        MOVE_LATENCY_SECONDS,
        metrics::Unit::Seconds,
        "Server-side latency from move arrival to broadcast completion (per accepted move)"
    );

    // 起動直後の `/metrics` でも主要系列がゼロ値で見えるようにラベル無し
    // counter / gauge を一度だけ touch する。これがないと exporter は「値が
    // 一度も記録されていない系列」を出力に含めず、Prometheus の `rate(...)` や
    // `absent(...)` 系クエリが series 不在エラーになる。
    //
    // `GAMES_FINISHED_TOTAL{result_code=..}` や `MOVE_LATENCY_SECONDS` は
    // ラベル別 / histogram で「事前にラベル全列挙はできない」性質のため、
    // 最初の発火を待つ。
    metrics::counter!(CONNECTIONS_TOTAL).absolute(0);
    metrics::gauge!(CONNECTIONS_ACTIVE).set(0.0);
    metrics::counter!(GAMES_TOTAL).absolute(0);
    metrics::gauge!(GAMES_ACTIVE).set(0.0);
    metrics::counter!(TIME_UP_TOTAL).absolute(0);
    Ok(())
}

/// `init_prometheus_exporter` の失敗。
#[derive(Debug, thiserror::Error)]
pub enum MetricsError {
    /// recorder install / HTTP listener bind 失敗。文字列はそのまま運用ログに出る。
    #[error("failed to install Prometheus recorder: {0}")]
    Install(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// メトリクス名は外部から観測される運用契約。意図せぬ rename を CI で止める
    /// ため、定数の文字列値を完全一致で固定する。新しい系列を追加する場合は
    /// 本テストも更新する（exporter のラベル付与方式と Prometheus 命名規約に
    /// 準拠していることを併せて確認する）。
    #[test]
    fn metric_names_are_byte_stable() {
        assert_eq!(CONNECTIONS_ACTIVE, "csa_connections_active");
        assert_eq!(CONNECTIONS_TOTAL, "csa_connections_total");
        assert_eq!(GAMES_ACTIVE, "csa_games_active");
        assert_eq!(GAMES_TOTAL, "csa_games_total");
        assert_eq!(GAMES_FINISHED_TOTAL, "csa_games_finished_total");
        assert_eq!(TIME_UP_TOTAL, "csa_time_up_total");
        assert_eq!(MOVE_LATENCY_SECONDS, "csa_move_latency_seconds");
    }
}
