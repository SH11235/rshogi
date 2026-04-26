//! UNIX エポック ミリ秒 → CSA V2 棋譜向け文字列の整形ヘルパ（純粋関数）。
//!
//! `chrono::DateTime::from_timestamp` ベースで、システム時計に依存しない。
//! wasm32 では std 時計が使えないので、`worker::Date::now()` で取った
//! `u64` エポック ms を本モジュールに渡して整形する設計。

/// `YYYY/MM/DD HH:MM:SS`（UTC）に整形する。CSA V2 の `$START_TIME:` /
/// `$END_TIME:` 書式に直接流し込める。
pub fn format_csa_datetime(epoch_ms: u64) -> String {
    let dt = to_utc(epoch_ms);
    dt.format("%Y/%m/%d %H:%M:%S").to_string()
}

/// `YYYY/MM/DD`（UTC）に整形する。R2 キーの日付パス（TCP 版
/// `FileKifuStorage` と互換）に使う。
pub fn format_date_path(epoch_ms: u64) -> String {
    let dt = to_utc(epoch_ms);
    dt.format("%Y/%m/%d").to_string()
}

/// RFC3339（UTC、秒単位、`Z` サフィックス）に整形する。`FloodgateHistoryEntry`
/// の `start_time` / `end_time` 契約に揃えるための共通ヘルパで、`R2FloodgateHistoryStorage`
/// の `entry_key` が `DateTime::parse_from_rfc3339` で読み戻せる書式と一致させる。
pub fn format_rfc3339_utc(epoch_ms: u64) -> String {
    to_utc(epoch_ms).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn to_utc(epoch_ms: u64) -> chrono::DateTime<chrono::Utc> {
    let secs = (epoch_ms / 1000).min(i64::MAX as u64) as i64;
    chrono::DateTime::<chrono::Utc>::from_timestamp(secs, 0)
        .unwrap_or_else(|| chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_csa_datetime_epoch_zero() {
        assert_eq!(format_csa_datetime(0), "1970/01/01 00:00:00");
    }

    #[test]
    fn format_csa_datetime_known_point() {
        // 2024-01-15 09:30:45 UTC = 1705311045 seconds → 1705311045000 ms
        assert_eq!(format_csa_datetime(1_705_311_045_000), "2024/01/15 09:30:45");
    }

    #[test]
    fn format_csa_datetime_drops_sub_second() {
        // 末尾 ms は捨てて秒単位で整形する。
        assert_eq!(format_csa_datetime(1_705_311_045_999), "2024/01/15 09:30:45");
    }

    #[test]
    fn format_date_path_epoch_zero() {
        assert_eq!(format_date_path(0), "1970/01/01");
    }

    #[test]
    fn format_date_path_known_point() {
        assert_eq!(format_date_path(1_705_311_045_000), "2024/01/15");
    }

    #[test]
    fn format_date_path_roundtrips_day_boundary_utc() {
        // 2024-01-15 23:59:59 → 2024/01/15
        assert_eq!(format_date_path(1_705_363_199_000), "2024/01/15");
        // 2024-01-16 00:00:00 → 2024/01/16
        assert_eq!(format_date_path(1_705_363_200_000), "2024/01/16");
    }

    #[test]
    fn format_rfc3339_utc_known_point() {
        // 2024-01-15 09:30:45 UTC = 1_705_311_045_000 ms
        assert_eq!(format_rfc3339_utc(1_705_311_045_000), "2024-01-15T09:30:45Z");
    }

    #[test]
    fn format_rfc3339_utc_drops_sub_second() {
        assert_eq!(format_rfc3339_utc(1_705_311_045_999), "2024-01-15T09:30:45Z");
    }
}
