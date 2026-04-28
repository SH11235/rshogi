//! viewer 配信 API 用の R2 補助インデックス (`games-index/...`)。
//!
//! 終局時に [`crate::game_room::GameRoom::export_kifu_to_r2`] が CSA 本文を
//! `KIFU_BUCKET` に書き込んだ直後、本モジュールのキー生成 + JSON 直列化を
//! 経由して `games-index/<inv_ended_at_ms>-<game_id>.json` を 1 件追加する。
//!
//! # キー設計
//!
//! - prefix: `games-index/`
//! - 並び順軸: `ended_at_ms` (Floodgate 履歴と同じ軸。長時間対局が古いキーになる
//!   start_time 軸の問題を回避)
//! - lex 順 = 数値降順を成立させるため、`inv_ms = INV_BASE - ended_at_ms` を
//!   `{:014}` でゼロパディングして key 先頭に置く。`INV_BASE = 99_999_999_999_999`
//!   (= `10^14 - 1`) で `ended_at_ms ∈ [0, INV_BASE]` を仮定し、書式上は常に 14 桁。
//! - `<game_id>` は [`crate::floodgate_history::validate_key_component`] で
//!   ASCII 英数 + `-` `_` のみを許可する。それ以外は `StorageError::Malformed`。
//!
//! # 不変条件
//!
//! - 1 対局 = 1 オブジェクト、本文は単一 JSON entry。
//! - 並行書き込み中の pagination 整合: 新着 entry は既存 cursor より前に挿入
//!   される (cursor 位置で見落とされる) が、これは仕様として明示的に許容する。
//! - 同一 ms に同一 `game_id` の終局は DO の単一 finalize で 1 度しか起きない
//!   (room_id ごとに DO 一意) ため、key は `room_id × ended_at_ms × game_id`
//!   の積で一意。
//!
//! # ホスト側ユニットテスト
//!
//! `inv_ms` のゼロパディング、key formatter、`validate_key_component` の
//! disallowed char 系を host target で検証する。R2 アダプタ固有の経路 (list /
//! pagination) は `wrangler dev` で別途確認する。

use rshogi_csa_server::error::StorageError;
use serde::Serialize;

use crate::floodgate_history::validate_key_component;

/// `inv_ms = INV_BASE - ended_at_ms` の基準値。`10^14 - 1` を u64 で表現。
///
/// `ended_at_ms` が西暦 5138 年 (= INV_BASE 直前) を超える場合は overflow で減算が
/// 失敗するが、本コードベース運用上ここに到達することは想定しない。仮に到達した
/// 場合は `validate_inv_ms` 経由で `Err` を返し、index put は best-effort で skip
/// する経路に乗せる。
pub const INV_BASE: u64 = 99_999_999_999_999;

/// `games-index/` prefix。一覧 API はこの prefix に対して R2 list を発行する。
pub const KEY_PREFIX: &str = "games-index/";

/// 1 対局 1 オブジェクトの index key を構築する。
///
/// `ended_at_ms` が `INV_BASE` を超える場合は `StorageError::Malformed` を返す
/// (write 経路で best-effort 失敗として観測ログに残す)。
pub fn games_index_key(ended_at_ms: u64, game_id: &str) -> Result<String, StorageError> {
    let validated = validate_key_component(game_id)?;
    if ended_at_ms > INV_BASE {
        return Err(StorageError::Malformed(format!(
            "ended_at_ms {ended_at_ms} exceeds INV_BASE {INV_BASE}"
        )));
    }
    let inv = INV_BASE - ended_at_ms;
    Ok(format!("{KEY_PREFIX}{inv:014}-{validated}.json"))
}

/// games-index に書き込む 1 entry の wire format。
///
/// API contract A (Issue #542 issuecomment-4338125621) に準拠し、`result_kind` /
/// `end_reason` を **2 フィールドに分離** する。client 側 (ramu-shogi viewer)
/// は両者を合わせて UI 表示用の構造化結果に変換する。
///
/// `source` は `"kifu"` / `"floodgate"` の 2 値 enum。`"floodgate"` は
/// 「Floodgate gating が opt-in された worker (`ALLOW_FLOODGATE_FEATURES`
/// 立ち + `FLOODGATE_HISTORY_BUCKET` binding 解決可) で起きた終局」を意味し、
/// `floodgate-history/` への put 成否は反映しない。
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct GamesIndexEntry<'a> {
    pub game_id: &'a str,
    pub started_at_ms: u64,
    pub ended_at_ms: u64,
    pub black_handle: &'a str,
    pub white_handle: &'a str,
    pub result_kind: &'static str,
    pub end_reason: &'static str,
    pub moves_count: u32,
    pub clock: ClockSpec<'a>,
    pub source: &'static str,
}

/// `clock` field の wire 形状。`kind` は `wrangler.toml::CLOCK_KIND` と同一の
/// snake_case 値域 (`countdown` / `countdown_msec` / `fischer` / `stopwatch`)。
///
/// 各時計方式で意味のある field は限定されるため、未使用 field は `None` で省略する
/// (serde の `skip_serializing_if`)。
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ClockSpec<'a> {
    pub kind: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_sec: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub byoyomi_sec: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub byoyomi_ms: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub increment_sec: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_ms: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_min: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub byoyomi_min: Option<u32>,
}

impl<'a> ClockSpec<'a> {
    /// `rshogi_csa_server::ClockSpec` から wire 表現を構築する。各時計方式の
    /// 専用フィールドのみを埋め、それ以外は `None` で省略する。
    pub fn from_server(spec: &'a rshogi_csa_server::ClockSpec) -> Self {
        match spec {
            rshogi_csa_server::ClockSpec::Countdown {
                total_time_sec,
                byoyomi_sec,
            } => Self {
                kind: "countdown",
                total_sec: Some(*total_time_sec),
                byoyomi_sec: Some(*byoyomi_sec),
                byoyomi_ms: None,
                increment_sec: None,
                total_ms: None,
                total_min: None,
                byoyomi_min: None,
            },
            rshogi_csa_server::ClockSpec::CountdownMsec {
                total_time_ms,
                byoyomi_ms,
            } => Self {
                kind: "countdown_msec",
                total_sec: None,
                byoyomi_sec: None,
                byoyomi_ms: Some(*byoyomi_ms),
                increment_sec: None,
                total_ms: Some(*total_time_ms),
                total_min: None,
                byoyomi_min: None,
            },
            rshogi_csa_server::ClockSpec::Fischer {
                total_time_sec,
                increment_sec,
            } => Self {
                kind: "fischer",
                total_sec: Some(*total_time_sec),
                byoyomi_sec: None,
                byoyomi_ms: None,
                increment_sec: Some(*increment_sec),
                total_ms: None,
                total_min: None,
                byoyomi_min: None,
            },
            rshogi_csa_server::ClockSpec::StopWatch {
                total_time_min,
                byoyomi_min,
            } => Self {
                kind: "stopwatch",
                total_sec: None,
                byoyomi_sec: None,
                byoyomi_ms: None,
                increment_sec: None,
                total_ms: None,
                total_min: Some(*total_time_min),
                byoyomi_min: Some(*byoyomi_min),
            },
        }
    }
}

/// `GameResult` から `(result_kind, end_reason)` を導出する。
///
/// API contract A の値域:
/// - `result_kind ∈ {"WIN_BLACK", "WIN_WHITE", "DRAW", "ABORT"}`
/// - `end_reason ∈ {"RESIGN", "TIME_UP", "ILLEGAL", "JISHOGI", "OUTE_SENNICHITE",
///   "SENNICHITE", "MAX_MOVES", "ABNORMAL"}`
///
/// `Abnormal { winner: None }` だけは結果が不確定なので `result_kind = "ABORT"`
/// で表現する。それ以外は勝者または引き分けが確定しているため `WIN_*` / `DRAW`
/// に分類する。
pub fn classify_result(
    result: &rshogi_csa_server::game::result::GameResult,
) -> (&'static str, &'static str) {
    use rshogi_csa_server::game::result::GameResult;
    use rshogi_csa_server::types::Color;

    fn winner_kind(c: Color) -> &'static str {
        match c {
            Color::Black => "WIN_BLACK",
            Color::White => "WIN_WHITE",
        }
    }

    match result {
        GameResult::Toryo { winner } => (winner_kind(*winner), "RESIGN"),
        GameResult::TimeUp { loser } => (winner_kind(loser.opposite()), "TIME_UP"),
        GameResult::IllegalMove { loser, .. } => (winner_kind(loser.opposite()), "ILLEGAL"),
        GameResult::Kachi { winner } => (winner_kind(*winner), "JISHOGI"),
        GameResult::OuteSennichite { loser } => (winner_kind(loser.opposite()), "OUTE_SENNICHITE"),
        GameResult::Sennichite => ("DRAW", "SENNICHITE"),
        GameResult::MaxMoves => ("DRAW", "MAX_MOVES"),
        GameResult::Abnormal { winner } => match winner {
            Some(c) => (winner_kind(*c), "ABNORMAL"),
            None => ("ABORT", "ABNORMAL"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rshogi_csa_server::game::result::{GameResult, IllegalReason};
    use rshogi_csa_server::types::Color;

    #[test]
    fn games_index_key_pads_inv_ms_to_14_digits() {
        // ended_at_ms = 1 → inv = INV_BASE - 1 = 99_999_999_999_998 (14 桁)
        let key = games_index_key(1, "g1").unwrap();
        assert_eq!(key, "games-index/99999999999998-g1.json");
    }

    #[test]
    fn games_index_key_inv_ms_zero_pads_for_recent_timestamp() {
        // 2026-04-29 頃の epoch ms (13 桁) でも inv は 14 桁ゼロパディングで揃う。
        let ended = 1_777_392_877_244_u64;
        let key = games_index_key(ended, "lobby-cross-fischer-1777391025209").unwrap();
        // INV_BASE - 1_777_392_877_244 = 98_222_607_122_755 (14 桁)
        assert_eq!(key, "games-index/98222607122755-lobby-cross-fischer-1777391025209.json");
    }

    #[test]
    fn games_index_key_lex_order_matches_descending_ended_at() {
        // 早い終局のほうが古い → inv が大きい → key が lex 後ろ。
        // 遅い終局のほうが新しい → inv が小さい → key が lex 前。
        let early = games_index_key(1_000_000_000_000, "g1").unwrap();
        let late = games_index_key(2_000_000_000_000, "g2").unwrap();
        assert!(late < early, "late {late} should sort before early {early}");
    }

    #[test]
    fn games_index_key_inv_zero_when_ended_at_eq_inv_base() {
        let key = games_index_key(INV_BASE, "g1").unwrap();
        assert_eq!(key, "games-index/00000000000000-g1.json");
    }

    #[test]
    fn games_index_key_rejects_overflowing_ended_at() {
        let err = games_index_key(INV_BASE + 1, "g1").unwrap_err();
        assert!(matches!(err, StorageError::Malformed(_)), "got: {err:?}");
    }

    #[test]
    fn games_index_key_rejects_game_id_with_slash() {
        // `/` は R2 の階層区切り。validate_key_component 経由で弾かれる。
        let err = games_index_key(1_000, "g1/evil").unwrap_err();
        assert!(matches!(err, StorageError::Malformed(_)), "got: {err:?}");
    }

    #[test]
    fn games_index_key_rejects_empty_game_id() {
        let err = games_index_key(1_000, "").unwrap_err();
        assert!(matches!(err, StorageError::Malformed(_)), "got: {err:?}");
    }

    #[test]
    fn games_index_key_rejects_non_ascii_game_id() {
        let err = games_index_key(1_000, "g\u{3042}").unwrap_err();
        assert!(matches!(err, StorageError::Malformed(_)), "got: {err:?}");
    }

    #[test]
    fn games_index_key_rejects_disallowed_punctuation() {
        // `.` / 空白 / `?` 等 ASCII でも英数 + `-` `_` 以外は拒否。
        for bad in ["g.1", "g 1", "g?1", "g+1", "g/1"] {
            let err = games_index_key(1_000, bad).unwrap_err();
            assert!(
                matches!(err, StorageError::Malformed(_)),
                "input={bad:?} expected Malformed, got: {err:?}",
            );
        }
    }

    #[test]
    fn games_index_key_accepts_underscore_and_dash() {
        let key = games_index_key(1_000, "g_1-abc").unwrap();
        assert!(key.ends_with("-g_1-abc.json"), "got: {key}");
    }

    #[test]
    fn classify_result_maps_toryo_to_winner_and_resign() {
        let r = GameResult::Toryo {
            winner: Color::Black,
        };
        assert_eq!(classify_result(&r), ("WIN_BLACK", "RESIGN"));

        let r = GameResult::Toryo {
            winner: Color::White,
        };
        assert_eq!(classify_result(&r), ("WIN_WHITE", "RESIGN"));
    }

    #[test]
    fn classify_result_maps_time_up_to_opponent_winner() {
        // 黒が時間切れ → 白勝ち
        let r = GameResult::TimeUp {
            loser: Color::Black,
        };
        assert_eq!(classify_result(&r), ("WIN_WHITE", "TIME_UP"));
    }

    #[test]
    fn classify_result_maps_illegal_move_to_opponent_winner() {
        let r = GameResult::IllegalMove {
            loser: Color::White,
            reason: IllegalReason::Generic,
        };
        assert_eq!(classify_result(&r), ("WIN_BLACK", "ILLEGAL"));
    }

    #[test]
    fn classify_result_maps_kachi_to_winner_and_jishogi() {
        let r = GameResult::Kachi {
            winner: Color::Black,
        };
        assert_eq!(classify_result(&r), ("WIN_BLACK", "JISHOGI"));
    }

    #[test]
    fn classify_result_maps_oute_sennichite_to_opponent_winner() {
        let r = GameResult::OuteSennichite {
            loser: Color::Black,
        };
        assert_eq!(classify_result(&r), ("WIN_WHITE", "OUTE_SENNICHITE"));
    }

    #[test]
    fn classify_result_maps_sennichite_to_draw() {
        assert_eq!(classify_result(&GameResult::Sennichite), ("DRAW", "SENNICHITE"));
    }

    #[test]
    fn classify_result_maps_max_moves_to_draw() {
        assert_eq!(classify_result(&GameResult::MaxMoves), ("DRAW", "MAX_MOVES"));
    }

    #[test]
    fn classify_result_maps_abnormal_with_winner_to_winner_kind() {
        let r = GameResult::Abnormal {
            winner: Some(Color::Black),
        };
        assert_eq!(classify_result(&r), ("WIN_BLACK", "ABNORMAL"));
    }

    #[test]
    fn classify_result_maps_abnormal_without_winner_to_abort() {
        let r = GameResult::Abnormal { winner: None };
        assert_eq!(classify_result(&r), ("ABORT", "ABNORMAL"));
    }

    #[test]
    fn clock_spec_from_server_countdown_emits_only_seconds() {
        let spec = rshogi_csa_server::ClockSpec::Countdown {
            total_time_sec: 600,
            byoyomi_sec: 10,
        };
        let wire = ClockSpec::from_server(&spec);
        assert_eq!(wire.kind, "countdown");
        assert_eq!(wire.total_sec, Some(600));
        assert_eq!(wire.byoyomi_sec, Some(10));
        assert_eq!(wire.byoyomi_ms, None);
        assert_eq!(wire.increment_sec, None);
    }

    #[test]
    fn clock_spec_from_server_fischer_emits_increment() {
        let spec = rshogi_csa_server::ClockSpec::Fischer {
            total_time_sec: 300,
            increment_sec: 5,
        };
        let wire = ClockSpec::from_server(&spec);
        assert_eq!(wire.kind, "fischer");
        assert_eq!(wire.total_sec, Some(300));
        assert_eq!(wire.increment_sec, Some(5));
        assert_eq!(wire.byoyomi_sec, None);
    }

    #[test]
    fn clock_spec_from_server_countdown_msec_emits_ms_fields() {
        let spec = rshogi_csa_server::ClockSpec::CountdownMsec {
            total_time_ms: 60_000,
            byoyomi_ms: 100,
        };
        let wire = ClockSpec::from_server(&spec);
        assert_eq!(wire.kind, "countdown_msec");
        assert_eq!(wire.total_ms, Some(60_000));
        assert_eq!(wire.byoyomi_ms, Some(100));
        assert_eq!(wire.total_sec, None);
        assert_eq!(wire.byoyomi_sec, None);
    }

    #[test]
    fn clock_spec_from_server_stopwatch_emits_minute_fields() {
        let spec = rshogi_csa_server::ClockSpec::StopWatch {
            total_time_min: 15,
            byoyomi_min: 2,
        };
        let wire = ClockSpec::from_server(&spec);
        assert_eq!(wire.kind, "stopwatch");
        assert_eq!(wire.total_min, Some(15));
        assert_eq!(wire.byoyomi_min, Some(2));
        assert_eq!(wire.total_sec, None);
    }

    #[test]
    fn entry_serializes_with_split_result_fields() {
        let entry = GamesIndexEntry {
            game_id: "g1",
            started_at_ms: 1_777_391_025_209,
            ended_at_ms: 1_777_392_877_244,
            black_handle: "alice",
            white_handle: "bob",
            result_kind: "WIN_BLACK",
            end_reason: "RESIGN",
            moves_count: 142,
            clock: ClockSpec {
                kind: "fischer",
                total_sec: Some(300),
                byoyomi_sec: None,
                byoyomi_ms: None,
                increment_sec: Some(5),
                total_ms: None,
                total_min: None,
                byoyomi_min: None,
            },
            source: "kifu",
        };
        let json = serde_json::to_string(&entry).unwrap();
        // result_kind / end_reason が独立 field として直列化されることを固定。
        assert!(json.contains("\"result_kind\":\"WIN_BLACK\""), "json={json}");
        assert!(json.contains("\"end_reason\":\"RESIGN\""), "json={json}");
        assert!(json.contains("\"source\":\"kifu\""), "json={json}");
        // 未使用 clock field は省略される。
        assert!(!json.contains("byoyomi_sec"), "json={json}");
        assert!(!json.contains("total_ms"), "json={json}");
    }
}
