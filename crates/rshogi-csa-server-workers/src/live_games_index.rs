//! viewer 配信 API 用の R2 補助インデックス (`live-games-index/...`)。
//!
//! 進行中対局の発見手段として、`GameRoom` DO の対局開始時に
//! `live-games-index/<inv_started_ms>-<game_id>.json` を 1 件 put し、
//! 終局時に同 key を delete する。`/api/v1/games/live` ハンドラはこの prefix を
//! R2 list することで現在 active な対局一覧を返す。
//!
//! `games_index` (`games-index/`、終局時 entry) と対称設計。違いは並び順軸が
//! `started_at_ms` 降順 (= 開始が新しい順) であることと、終局後に消える点のみ。
//! `INV_BASE` と `validate_key_component` は `games_index` / `floodgate_history`
//! から再利用する (重複定義しない)。
//!
//! # 整合性モデル (best-effort, eventual)
//!
//! - 対局開始 → R2 put までの数百 ms ～ 数秒は live に出てこない
//! - 終局 → R2 delete までは live に残る (viewer の click 時に既に終局済 cfg)
//! - DO crash で put 済だが delete されない orphan が残るリスクあり
//!   (orphan sweep は Issue #551 で扱う backfill ジョブに統合する)
//!
//! pagination 中に entry が追加・削除されうるため、同じ cursor で 2 回 list を
//! 呼んでも結果集合が一致しない。viewer 側は live entry を **発見手段** として
//! 扱い、行クリック時に WS spectate 接続で実状態を確認する前提。

use rshogi_csa_server::error::StorageError;
use serde::Serialize;

use crate::floodgate_history::validate_key_component;
use crate::games_index::{ClockSpec, INV_BASE};

/// `live-games-index/` prefix。`/api/v1/games/live` ハンドラはこの prefix に対して
/// R2 list を発行する。
pub const LIVE_KEY_PREFIX: &str = "live-games-index/";

/// 1 対局 1 オブジェクトの live index key を構築する。
///
/// `started_at_ms` が `INV_BASE` を超える場合は `StorageError::Malformed` を返す
/// (write 経路で best-effort 失敗として観測ログに残す)。
pub fn live_games_index_key(started_at_ms: u64, game_id: &str) -> Result<String, StorageError> {
    let validated = validate_key_component(game_id)?;
    if started_at_ms > INV_BASE {
        return Err(StorageError::Malformed(format!(
            "started_at_ms {started_at_ms} exceeds INV_BASE {INV_BASE}"
        )));
    }
    let inv = INV_BASE - started_at_ms;
    Ok(format!("{LIVE_KEY_PREFIX}{inv:014}-{validated}.json"))
}

/// live-games-index に書き込む 1 entry の wire format。
///
/// `started_at_ms` は **常に `play_started_at_ms`** (= `mark_play_started`
/// 観測時刻)。`matched_at_ms` への fallback は意図的に不採用 (live put は
/// `mark_play_started` 経路でしか発火しない契約)。
///
/// `source` は `"kifu"` / `"floodgate"` の 2 値。判定は
/// [`crate::games_index::resolve_index_source`] 共通 helper に集約しており、
/// 本モジュールでは事前に解決済みの値を受け取る。
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct LiveGamesIndexEntry<'a> {
    pub game_id: &'a str,
    pub started_at_ms: u64,
    pub black_handle: &'a str,
    pub white_handle: &'a str,
    pub clock: ClockSpec<'a>,
    pub source: &'static str,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_games_index_key_pads_inv_ms_to_14_digits() {
        // started_at_ms = 1 → inv = INV_BASE - 1 = 99_999_999_999_998 (14 桁)
        let key = live_games_index_key(1, "g1").unwrap();
        assert_eq!(key, "live-games-index/99999999999998-g1.json");
    }

    #[test]
    fn live_games_index_key_inv_ms_zero_pads_for_recent_timestamp() {
        // 2026-04-29 頃の epoch ms (13 桁) でも inv は 14 桁ゼロパディングで揃う。
        let started = 1_777_391_025_209_u64;
        let key = live_games_index_key(started, "lobby-cross-fischer-1777391025209").unwrap();
        // INV_BASE - 1_777_391_025_209 = 98_222_608_974_790 (14 桁)
        assert_eq!(key, "live-games-index/98222608974790-lobby-cross-fischer-1777391025209.json");
    }

    #[test]
    fn live_games_index_key_lex_order_matches_descending_started_at() {
        // 早い開始 → 古い → inv が大きい → key が lex 後ろ。
        // 遅い開始 → 新しい → inv が小さい → key が lex 前。
        let early = live_games_index_key(1_000_000_000_000, "g1").unwrap();
        let late = live_games_index_key(2_000_000_000_000, "g2").unwrap();
        assert!(late < early, "late {late} should sort before early {early}");
    }

    #[test]
    fn live_games_index_key_inv_zero_when_started_at_eq_inv_base() {
        let key = live_games_index_key(INV_BASE, "g1").unwrap();
        assert_eq!(key, "live-games-index/00000000000000-g1.json");
    }

    #[test]
    fn live_games_index_key_rejects_overflowing_started_at() {
        let err = live_games_index_key(INV_BASE + 1, "g1").unwrap_err();
        assert!(matches!(err, StorageError::Malformed(_)), "got: {err:?}");
    }

    #[test]
    fn live_games_index_key_rejects_game_id_with_slash() {
        // `/` は R2 の階層区切り。validate_key_component 経由で弾かれる。
        let err = live_games_index_key(1_000, "g1/evil").unwrap_err();
        assert!(matches!(err, StorageError::Malformed(_)), "got: {err:?}");
    }

    #[test]
    fn live_games_index_key_rejects_empty_game_id() {
        let err = live_games_index_key(1_000, "").unwrap_err();
        assert!(matches!(err, StorageError::Malformed(_)), "got: {err:?}");
    }

    #[test]
    fn live_games_index_key_rejects_disallowed_punctuation() {
        // `.` / 空白 / `?` 等 ASCII でも英数 + `-` `_` 以外は拒否。
        for bad in ["g.1", "g 1", "g?1", "g+1", "g/1"] {
            let err = live_games_index_key(1_000, bad).unwrap_err();
            assert!(
                matches!(err, StorageError::Malformed(_)),
                "input={bad:?} expected Malformed, got: {err:?}",
            );
        }
    }

    #[test]
    fn live_games_index_key_accepts_underscore_and_dash() {
        let key = live_games_index_key(1_000, "g_1-abc").unwrap();
        assert!(key.ends_with("-g_1-abc.json"), "got: {key}");
    }

    #[test]
    fn live_entry_serializes_with_expected_fields() {
        let entry = LiveGamesIndexEntry {
            game_id: "g1",
            started_at_ms: 1_777_391_025_209,
            black_handle: "alice",
            white_handle: "bob",
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
        assert!(json.contains("\"game_id\":\"g1\""), "json={json}");
        assert!(json.contains("\"started_at_ms\":1777391025209"), "json={json}");
        assert!(json.contains("\"black_handle\":\"alice\""), "json={json}");
        assert!(json.contains("\"white_handle\":\"bob\""), "json={json}");
        assert!(json.contains("\"source\":\"kifu\""), "json={json}");
        // 未使用 clock field は省略される。
        assert!(!json.contains("byoyomi_sec"), "json={json}");
        assert!(!json.contains("total_ms"), "json={json}");
        // ended_at_ms / result_kind / moves_count は live entry の wire には出ない。
        assert!(!json.contains("ended_at_ms"), "json={json}");
        assert!(!json.contains("result_kind"), "json={json}");
        assert!(!json.contains("moves_count"), "json={json}");
    }

    #[test]
    fn live_key_prefix_matches_expected_string() {
        // viewer_api ハンドラと R2 list でしか参照されない契約値を固定。
        assert_eq!(LIVE_KEY_PREFIX, "live-games-index/");
    }

    // `source` の env 切替判定は `games_index::resolve_index_source` (wasm32
    // 限定) に集約済み。ホスト target からは純粋関数
    // `games_index::classify_index_source_from_inputs` のテスト
    // (`games_index::tests`) で網羅する。
}
