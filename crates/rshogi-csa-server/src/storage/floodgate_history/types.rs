//! Floodgate 履歴 entry の値オブジェクト。
//!
//! ランタイム非依存の純データ型のみを置き、tokio や I/O 系 crate は読み込まない。
//! TCP/Workers いずれの crate からも安全に import できる。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::types::{Color, GameId, GameName, PlayerName};

/// Floodgate 履歴 1 件分のエントリ。`persist_kifu` 経由で終局確定時に
/// `FloodgateHistoryStorage::append` に渡される。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FloodgateHistoryEntry {
    /// 対局識別子（サーバ発行）。
    pub game_id: String,
    /// マッチが帰属する `game_name`（Floodgate スケジュールの分類軸と一致）。
    pub game_name: String,
    /// 先手プレイヤ名。
    pub black: String,
    /// 後手プレイヤ名。
    pub white: String,
    /// 対局開始時刻（UTC、RFC3339）。
    pub start_time: String,
    /// 対局終了時刻（UTC、RFC3339）。
    pub end_time: String,
    /// 終局理由コード（`#RESIGN` / `#TIME_UP` / `#ILLEGAL_MOVE` 等）。
    pub result_code: String,
    /// 勝者の色。引き分け（千日手・最大手数）や勝敗不確定の `#ABNORMAL` では
    /// `None`。シリアライズ時は `Black` / `White` 文字列。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub winner: Option<HistoryColor>,
}

/// `Color` を JSON スキーマ用に文字列シリアライズする小 enum。core の
/// `Color` は serde 派生していないので独立させる（serde を core 全体に拡げる
/// より隔離する方が依存範囲が読みやすい）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum HistoryColor {
    Black,
    White,
}

impl From<Color> for HistoryColor {
    fn from(c: Color) -> Self {
        match c {
            Color::Black => Self::Black,
            Color::White => Self::White,
        }
    }
}

impl FloodgateHistoryEntry {
    /// 業務型から構築するヘルパ。`persist_kifu` 経路から呼ばれる。
    pub fn new(
        game_id: &GameId,
        game_name: &GameName,
        black: &PlayerName,
        white: &PlayerName,
        start_time: DateTime<Utc>,
        end_time: DateTime<Utc>,
        result_code: &str,
        winner: Option<Color>,
    ) -> Self {
        Self {
            game_id: game_id.as_str().to_owned(),
            game_name: game_name.as_str().to_owned(),
            black: black.as_str().to_owned(),
            white: white.as_str().to_owned(),
            start_time: start_time.to_rfc3339(),
            end_time: end_time.to_rfc3339(),
            result_code: result_code.to_owned(),
            winner: winner.map(HistoryColor::from),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(game_id: &str, winner: Option<HistoryColor>) -> FloodgateHistoryEntry {
        FloodgateHistoryEntry {
            game_id: game_id.to_owned(),
            game_name: "floodgate-600-10".to_owned(),
            black: "alice".to_owned(),
            white: "bob".to_owned(),
            start_time: "2026-04-26T12:00:00+00:00".to_owned(),
            end_time: "2026-04-26T12:30:00+00:00".to_owned(),
            result_code: "#RESIGN".to_owned(),
            winner,
        }
    }

    #[test]
    fn entry_round_trips_through_json() {
        let e = entry("g1", Some(HistoryColor::Black));
        let s = serde_json::to_string(&e).unwrap();
        let parsed: FloodgateHistoryEntry = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed, e);
    }

    #[test]
    fn entry_omits_winner_when_none() {
        let e = entry("g1", None);
        let s = serde_json::to_string(&e).unwrap();
        // 引き分け（千日手 / 最大手数）では `winner` フィールドが出力に出ない。
        assert!(!s.contains("\"winner\""), "winner must be omitted: {s}");
    }
}
