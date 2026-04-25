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

/// 終局確定回数の counter。`result_code` ラベルで `primary_result_code` の
/// 既知値（`#RESIGN` / `#TIME_UP` / `#ILLEGAL_MOVE` / `#JISHOGI` /
/// `#OUTE_SENNICHITE` / `#SENNICHITE` / `#MAX_MOVES` / `#ABNORMAL`）に加えて、
/// AGREE 不成立 / REJECT / 進行中失敗で対局が破棄された場合の合成値
/// `#ABORTED` を分類する。**`csa_games_total` と総和が常に一致する不変条件**
/// を維持するため、`drive_game` の RAII ガード Drop で 1 件ずつ確実に増分する。
/// 既知値の網羅は `result_code_label_is_in_known_allowlist` テストが固定する。
pub const GAMES_FINISHED_TOTAL: &str = "csa_games_finished_total";

/// AGREE 不成立 / REJECT / その他 `drive_game` 内 Err での合成終局コード。
/// CSA プロトコルの通知コードではなく、メトリクス用の合成ラベルとして使う。
pub const RESULT_CODE_ABORTED: &str = "#ABORTED";

/// 時間切れ確定回数の counter。
///
/// **不変条件**: `csa_time_up_total == sum(csa_games_finished_total{result_code="#TIME_UP"})`。
/// 運用 SLO ダッシュボードで時間切れ件数だけを単一クエリで参照したい用途のため
/// 二重カウントしているが、両者で齟齬が出ると alerting が壊れるので必ず両方を
/// 同じ箇所（[`record_game_finished`] 相当）で増分する。
pub const TIME_UP_TOTAL: &str = "csa_time_up_total";

/// 指し手レイテンシ histogram (秒)。`drive_game` で 1 手の handle_line を呼んで
/// から `dispatch` で全宛先への broadcast 送出が完了するまでの区間を計測する。
///
/// bucket は [`MOVE_LATENCY_BUCKETS_SECONDS`] で固定する（運用 SLO の P50 / P95
/// / P99 を 1ms〜5s レンジで観測できるよう、Prometheus 慣行の指数分布に近い
/// 8 区間）。`metrics-exporter-prometheus` の既定は **summary**（rolling quantile）
/// で `_bucket` 系列を出さないため、明示の bucket 設定がないと
/// `histogram_quantile(...)` クエリと複数インスタンス aggregation が動かない。
pub const MOVE_LATENCY_SECONDS: &str = "csa_move_latency_seconds";

/// `csa_move_latency_seconds` の histogram bucket 境界。1ms / 5ms / 10ms / 50ms
/// / 100ms / 500ms / 1s / 5s の 8 区間で運用上の P50 / P95 / P99 SLO を網羅する。
/// 実運用レイテンシ分布が分かったタイミングで TCP 負荷試験 (task 20.1) で再調整する。
pub const MOVE_LATENCY_BUCKETS_SECONDS: &[f64] = &[0.001, 0.005, 0.01, 0.05, 0.1, 0.5, 1.0, 5.0];

/// `metrics-exporter-prometheus` の Prometheus exporter を install し、
/// 系列の `# HELP` / `# TYPE` と `csa_move_latency_seconds` の histogram bucket
/// を確定させる。
///
/// 別スレッド + 専用 Tokio runtime で HTTP listener を bind するため、本クレート
/// が採用している `current_thread` + `LocalSet` 設計には影響しない。listener は
/// process 終了時に runtime ごと drop される。
///
/// `--metrics-bind` 未指定時は本関数を呼ばず、recorder は install されない。
/// その場合 `metrics::counter!` 等は NoOp で動き、計測点を呼ぶオーバーヘッドは
/// 数 ns 程度に収まる（NoOp recorder の atomic 1 回分）。
///
/// 順序は **bucket 設定 → `install` → `describe_*` → 主要系列のゼロ初期化**。
/// bucket 設定は install 前に渡す必要がある（PrometheusBuilder の builder
/// pattern が install 時点の状態を fix するため）。`describe_*` は install 後
/// に呼ぶ必要がある（global recorder 未 install の状態で describe しても
/// `# HELP` 行が exporter 出力に register されない、`metrics-exporter-prometheus`
/// 0.18 の挙動）。ゼロ初期化も recorder install 後でしか効かない（NoOp recorder
/// には書き込めない）ので最後。
pub fn init_prometheus_exporter(addr: SocketAddr) -> Result<(), MetricsError> {
    use metrics_exporter_prometheus::{Matcher, PrometheusBuilder};

    PrometheusBuilder::new()
        .with_http_listener(addr)
        // `set_buckets_for_metric` で histogram 化を強制する。デフォルトの
        // summary (rolling quantile) 出力では `_bucket` 系列が無く
        // `histogram_quantile()` も複数インスタンス aggregation も使えないため、
        // 本サーバの SLO 観点には合わない。
        .set_buckets_for_metric(
            Matcher::Full(MOVE_LATENCY_SECONDS.to_owned()),
            MOVE_LATENCY_BUCKETS_SECONDS,
        )
        .map_err(MetricsError::Install)?
        .install()
        .map_err(MetricsError::Install)?;

    metrics::describe_counter!(
        CONNECTIONS_TOTAL,
        "Total number of CSA TCP connections accepted since process start"
    );
    metrics::describe_gauge!(
        CONNECTIONS_ACTIVE,
        "Currently open CSA TCP connections (incremented inside ConnectionActiveGuard, decremented on task drop)"
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
        "Total number of CSA games finished, partitioned by `result_code` label. \
         Invariant: sum equals csa_games_total. Aborted games (AGREE failure / REJECT / \
         transport error) use the synthetic label value `#ABORTED`."
    );
    metrics::describe_counter!(
        TIME_UP_TOTAL,
        "Total number of games ended by time-up. Invariant: equals \
         sum(csa_games_finished_total{result_code=\"#TIME_UP\"})."
    );
    metrics::describe_histogram!(
        MOVE_LATENCY_SECONDS,
        metrics::Unit::Seconds,
        "Server-side latency from move arrival to broadcast dispatch completion \
         (per accepted move). Bucket boundaries are explicitly set so the metric is \
         exposed as a Prometheus histogram (not summary), enabling histogram_quantile() \
         and cross-instance aggregation."
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
///
/// `metrics-exporter-prometheus` の `BuildError` を `#[from]` で保持し、source
/// chain を tracing / on-call で辿れるようにする。`set_buckets_for_metric` と
/// `install` のどちらでも同じ型のエラーが返るため variant を 1 つに統一する。
#[derive(Debug, thiserror::Error)]
pub enum MetricsError {
    /// recorder install / bucket 設定 / HTTP listener bind の失敗。
    #[error("failed to install Prometheus recorder")]
    Install(#[from] metrics_exporter_prometheus::BuildError),
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
        assert_eq!(RESULT_CODE_ABORTED, "#ABORTED");
    }

    /// `csa_games_finished_total{result_code}` ラベルに乗る値は外部 (Prometheus
    /// dashboards / alert rules) から観測される運用契約のため、`primary_result_code`
    /// が返し得る値と合成 `#ABORTED` の合算が **既知 allowlist に閉じている**こと
    /// を CI で固定する。新しい `GameResult` variant を `core` に追加する PR は
    /// `primary_result_code` の更新を強制され、その時点で本テストが落ちて漏れに
    /// 気付ける。
    #[test]
    fn result_code_label_values_are_in_known_allowlist() {
        use rshogi_csa_server::game::result::{GameResult, IllegalReason};
        use rshogi_csa_server::record::kifu::primary_result_code;
        use rshogi_csa_server::types::Color;

        // 既知 allowlist。dashboard / alert で参照される値はこれだけに限る。
        // 新規 result_code を増やしたい場合は、本配列・`primary_result_code`・
        // 関連 docstring・運用ダッシュボード設定の 4 点を同時に更新する契約。
        const KNOWN_RESULT_CODES: &[&str] = &[
            "#RESIGN",
            "#TIME_UP",
            "#ILLEGAL_MOVE",
            "#JISHOGI",
            "#OUTE_SENNICHITE",
            "#SENNICHITE",
            "#MAX_MOVES",
            "#ABNORMAL",
            RESULT_CODE_ABORTED,
        ];

        // `GameResult` の全 variant を列挙し、`primary_result_code` の戻り値が
        // allowlist に含まれることを確認する。網羅は Rust の non_exhaustive な
        // match で担保するため、ここでも match を使い、新 variant 追加時に
        // コンパイラが警告するようにする。
        let cases: &[GameResult] = &[
            GameResult::Toryo {
                winner: Color::Black,
            },
            GameResult::TimeUp {
                loser: Color::White,
            },
            GameResult::IllegalMove {
                loser: Color::Black,
                reason: IllegalReason::Generic,
            },
            GameResult::IllegalMove {
                loser: Color::Black,
                reason: IllegalReason::Uchifuzume,
            },
            GameResult::IllegalMove {
                loser: Color::Black,
                reason: IllegalReason::IllegalKachi,
            },
            GameResult::Kachi {
                winner: Color::Black,
            },
            GameResult::OuteSennichite {
                loser: Color::Black,
            },
            GameResult::Sennichite,
            GameResult::MaxMoves,
            GameResult::Abnormal { winner: None },
            GameResult::Abnormal {
                winner: Some(Color::Black),
            },
        ];
        for r in cases {
            let code = primary_result_code(r);
            assert!(
                KNOWN_RESULT_CODES.contains(&code),
                "primary_result_code({r:?}) = {code:?} is outside the metric label allowlist; \
                 update KNOWN_RESULT_CODES + dashboard / alert wiring before introducing it"
            );
        }
        // `#ABORTED` は `DriveGuard` Drop の合成ラベルとしてのみ使う。`primary_result_code`
        // が返さないことを片向きに固定する（合成を非合成と取り違える事故を防ぐ）。
        for r in cases {
            assert_ne!(
                primary_result_code(r),
                RESULT_CODE_ABORTED,
                "primary_result_code must never collide with the synthetic `{RESULT_CODE_ABORTED}` label"
            );
        }
    }

    /// `MOVE_LATENCY_BUCKETS_SECONDS` は単調増加でなければならない（Prometheus
    /// histogram の bucket 順序契約）。8 区間で 1ms〜5s レンジを 1 桁刻み中心で
    /// カバーする運用 SLO 設計を固定する。
    #[test]
    fn move_latency_buckets_are_monotonic_and_cover_slo_range() {
        let buckets = MOVE_LATENCY_BUCKETS_SECONDS;
        assert_eq!(buckets.len(), 8);
        for w in buckets.windows(2) {
            assert!(w[0] < w[1], "buckets must be strictly increasing: {buckets:?}");
        }
        assert!(*buckets.first().unwrap() <= 0.001 + f64::EPSILON);
        assert!(*buckets.last().unwrap() >= 5.0 - f64::EPSILON);
    }
}
