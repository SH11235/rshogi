//! 切断時の再接続プロトコル用 I/O 非依存ロジック。
//!
//! Workers DO は WebSocket close を契機に grace 期間の対局状態保持に入る。
//! 本モジュールは grace registry のシリアライズ可能な型と、token / handle / 色
//! の照合、状態再送メッセージの組み立てなど、DO ランタイムから完全に切り離した
//! 純粋関数群を提供する。`game_room.rs` のアダプタは本モジュールの戻り値を
//! パターンマッチして I/O 経路に流す形に薄く保つ。

use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::time::Duration;

use rshogi_csa_server::types::{Color, ReconnectToken};

/// `Color` を `MoveRow.color` と整合する文字列形式 (`"black"` / `"white"`)
/// に変換する。永続化スキーマは serde 文字列形式で安定化している。
pub fn color_to_str(color: Color) -> &'static str {
    match color {
        Color::Black => "black",
        Color::White => "white",
    }
}

/// 文字列形式 (`"black"` / `"white"`) から `Color` に戻す。永続化データを
/// `CoreRoom` API に流す際に使う。`MoveRow.color` の検証経路と同様、未知の
/// 値は `Err` で返して呼び出し側に判断させる。
pub fn color_from_str(raw: &str) -> Result<Color, String> {
    match raw {
        "black" => Ok(Color::Black),
        "white" => Ok(Color::White),
        other => Err(format!("unknown color string: {other:?}")),
    }
}

/// 切断時に保持する対局スナップショット。再接続クライアントへ
/// `Reconnect_State` ブロックで再送するため必要な情報を最小限で持つ。
///
/// 残り時間は `GameRoom::clock_remaining_main_ms` と同義の本体時間 (秒読み残
/// は含まない)。再接続クライアント側で 1 手 deadline 計算に使うことはできず、
/// 表示・ログ用途に限る。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReconnectSnapshot {
    /// 切断時点の `BEGIN Position` … `END Position` ブロック (末尾改行込み)。
    pub position_section: String,
    /// 先手の本体残り時間 (ms)。
    pub black_remaining_ms: u64,
    /// 後手の本体残り時間 (ms)。
    pub white_remaining_ms: u64,
    /// 現在の手番。`"black"` / `"white"` のみ書き込まれる契約。
    pub current_turn: String,
    /// 直前に確定した最終手 (CSA トークン)。1 手も指していない時点での切断は
    /// `None`。
    pub last_move: Option<String>,
}

/// 切断対局者の grace 中エントリ。DO storage の `KEY_GRACE_REGISTRY` (1 対局
/// = 1 オブジェクト) にシリアライズして書き込む。
///
/// 二人の対局者のうち切断したのが一方だけの場合は本構造体 1 件で表現する
/// (DO instance は 1 対局専属なので 0..=1 件しか持たない)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingReconnect {
    /// 切断側の handle (LOGIN 時の照合に使う)。
    pub disconnected_handle: String,
    /// 切断側の `Color` を文字列で持つ (`"black"` / `"white"`)。
    pub disconnected_color: String,
    /// 切断側に発行された再接続トークン (Game_Summary 末尾拡張行で配布済み)。
    pub expected_token: String,
    /// grace 期間の終了時刻。`now_ms()` がこれを超えたら満了。
    pub deadline_ms: u64,
    /// 状態再送に使うスナップショット。
    pub snapshot: ReconnectSnapshot,
    /// 切断側宛の Game_Summary 文字列 (`Reconnect_Token:` 拡張行を含む完全形、
    /// `position_section` は切断時点の現在局面で再構築済み)。
    pub game_summary_for_disconnected: String,
    /// 切断時点で予約されていた turn alarm の発火時刻 (UNIX epoch ms)。
    /// 再接続成功時に新しい turn alarm を貼り直す際、本値と「再接続時刻 + 残時間
    /// budget」のうち**早い方**を採用することで、悪意あるクライアントが切断 →
    /// grace 直前再接続を繰り返して相手手番の deadline を wall-clock 上で延長
    /// する経路を防ぐ (元 deadline は壊さない)。`None` なら turn alarm 未予約
    /// 時点での切断 (例: AGREE 直後の対局未開始) で、上書きせず素直に新規予約。
    /// `#[serde(default)]` で旧 schema からの cold-start 互換も維持する。
    #[serde(default)]
    pub original_turn_alarm_epoch_ms: Option<u64>,
}

/// `PendingReconnect::match_request` の判定結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconnectMatchOutcome {
    /// handle / 色 / token が全て一致し、deadline 内。
    Accepted,
    /// handle / 色 / token のいずれかが不一致。grace 中の登録は変更しない。
    Rejected,
    /// `now_ms` が `deadline_ms` を超えている。grace は満了済みなので登録を
    /// 取り除き、切断側を敗北として確定させる経路に進む。
    Expired,
}

impl PendingReconnect {
    /// 再接続要求 (LOGIN 行 `reconnect:<game_id>+<token>`) を本エントリと照合する。
    ///
    /// `game_id` は呼び出し側で DO storage の key 検索によって既に絞り込まれて
    /// いる前提なので、本関数は handle / 色 / token / deadline のみを検査する。
    pub fn match_request(
        &self,
        handle: &str,
        color: Color,
        token: &str,
        now_ms: u64,
    ) -> ReconnectMatchOutcome {
        if now_ms > self.deadline_ms {
            return ReconnectMatchOutcome::Expired;
        }
        if self.disconnected_handle != handle
            || self.disconnected_color != color_to_str(color)
            || self.expected_token != token
        {
            return ReconnectMatchOutcome::Rejected;
        }
        ReconnectMatchOutcome::Accepted
    }
}

/// 再接続成立時にクライアントへ送出する状態再送メッセージを組み立てる。
///
/// フォーマット (TCP frontend と統一):
/// 1. `BEGIN Game_Summary` ... `END Game_Summary` (`position_section` は切断時点
///    の現在局面、`Reconnect_Token:` 拡張行を含む完全形)
/// 2. `BEGIN Reconnect_State` ... `END Reconnect_State` (現在の手番・両者残時間・
///    直前手のメタ情報)
pub fn build_resume_message(
    game_summary_for_disconnected: &str,
    snapshot: &ReconnectSnapshot,
) -> String {
    let mut out = game_summary_for_disconnected.to_owned();
    out.push_str("BEGIN Reconnect_State\n");
    let turn_char = match snapshot.current_turn.as_str() {
        "black" => '+',
        "white" => '-',
        // `current_turn` は `color_to_str` 経由でしか書き込まれないため、ここに
        // 到達するのは DO storage を外部から直接書き換えた等のスキーマ不整合
        // ケースのみ。値判定不能で勝敗が変わる経路を作らないよう、安全側に
        // 黒手番（先手）でフォールバックして再接続自体は成立させる（CSA 互換
        // クライアントは Reconnect_Token / Game_Summary 由来の手番情報を再
        // 受信するため、致命的影響は限定的）。
        _ => '+',
    };
    let _ = writeln!(out, "Current_Turn:{turn_char}");
    let _ = writeln!(out, "Black_Time_Remaining_Ms:{}", snapshot.black_remaining_ms);
    let _ = writeln!(out, "White_Time_Remaining_Ms:{}", snapshot.white_remaining_ms);
    if let Some(last) = &snapshot.last_move {
        let _ = writeln!(out, "Last_Move:{last}");
    }
    out.push_str("END Reconnect_State\n");
    out
}

/// `state.alarm()` は DO 1 instance につき 1 つしか同時にセットできない。複数
/// 種別の発火 (時間切れ / grace 満了) を扱うため、次に発火すべき種別を DO
/// storage に明示記録する。alarm ハンドラはこのタグを読んで分岐する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PendingAlarmKind {
    /// 時間切れ (turn deadline)。既存の `force_time_up` 経路を駆動する。
    TimeUp,
    /// grace 期間満了。grace registry を読み取り、切断側を `force_abnormal` で
    /// 敗北として確定させる経路を駆動する。
    GraceExpired,
}

/// 再接続 grace が有効化されている (`grace > 0`) のときのみ対局者ごとの
/// `Reconnect_Token` を発行する。`grace == Duration::ZERO` のときは
/// `(None, None)` を返し、`Game_Summary` 末尾拡張行への配布も
/// `PersistedConfig` への保存もスキップする。
///
/// production の保守的既定 (`RECONNECT_GRACE_SECONDS=0`) では本関数は常に
/// `(None, None)` を返し、Issue #591 の `LOGIN:incorrect reconnect_rejected`
/// 経路に client が到達しなくなる (`csa_client::run_one_game` の reconnect
/// skip 判定で `summary.reconnect_token == None` のため reconnect 試行自体を
/// skip する)。
pub(crate) fn issue_tokens_if_enabled(
    grace: Duration,
) -> (Option<ReconnectToken>, Option<ReconnectToken>) {
    if grace.is_zero() {
        (None, None)
    } else {
        (Some(ReconnectToken::generate()), Some(ReconnectToken::generate()))
    }
}

/// grace 登録時に次回 alarm が表す意味と、alarm 本体を grace deadline に
/// 上書きするかを決める。
///
/// 既存 turn alarm が grace deadline 以前に発火するなら、alarm 本体はそのまま
/// `TimeUp` として扱う。そうでなければ tag/body ともに `GraceExpired` に揃える。
pub(crate) fn classify_alarm_after_enter_grace(
    existing_alarm_epoch_ms: Option<i64>,
    grace_deadline_ms: u64,
) -> (PendingAlarmKind, bool) {
    match existing_alarm_epoch_ms.and_then(|epoch_ms| u64::try_from(epoch_ms).ok()) {
        Some(epoch_ms) if epoch_ms <= grace_deadline_ms => (PendingAlarmKind::TimeUp, false),
        _ => (PendingAlarmKind::GraceExpired, true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_snapshot() -> ReconnectSnapshot {
        ReconnectSnapshot {
            position_section: "BEGIN Position\nP1...\nEND Position\n".to_owned(),
            black_remaining_ms: 599_500,
            white_remaining_ms: 600_000,
            current_turn: "white".to_owned(),
            last_move: Some("+7776FU".to_owned()),
        }
    }

    fn sample_pending(deadline_ms: u64) -> PendingReconnect {
        PendingReconnect {
            disconnected_handle: "alice".to_owned(),
            disconnected_color: "black".to_owned(),
            expected_token: "abcd".to_owned(),
            deadline_ms,
            snapshot: sample_snapshot(),
            game_summary_for_disconnected:
                "BEGIN Game_Summary\nGame_ID:g1\nReconnect_Token:abcd\nEND Game_Summary\n"
                    .to_owned(),
            original_turn_alarm_epoch_ms: None,
        }
    }

    #[test]
    fn match_request_accepts_when_all_fields_match_and_within_deadline() {
        let p = sample_pending(1_000);
        assert_eq!(
            p.match_request("alice", Color::Black, "abcd", 500),
            ReconnectMatchOutcome::Accepted
        );
    }

    #[test]
    fn match_request_rejects_handle_mismatch() {
        let p = sample_pending(1_000);
        assert_eq!(
            p.match_request("bob", Color::Black, "abcd", 500),
            ReconnectMatchOutcome::Rejected
        );
    }

    #[test]
    fn match_request_rejects_color_mismatch() {
        let p = sample_pending(1_000);
        assert_eq!(
            p.match_request("alice", Color::White, "abcd", 500),
            ReconnectMatchOutcome::Rejected
        );
    }

    #[test]
    fn match_request_rejects_token_mismatch() {
        let p = sample_pending(1_000);
        assert_eq!(
            p.match_request("alice", Color::Black, "wrong", 500),
            ReconnectMatchOutcome::Rejected
        );
    }

    #[test]
    fn match_request_returns_expired_when_now_passes_deadline() {
        let p = sample_pending(1_000);
        assert_eq!(
            p.match_request("alice", Color::Black, "abcd", 1_001),
            ReconnectMatchOutcome::Expired
        );
    }

    #[test]
    fn match_request_treats_deadline_boundary_as_in_window() {
        let p = sample_pending(1_000);
        // now_ms == deadline_ms はまだ猶予内 (`>` で判定)。
        assert_eq!(
            p.match_request("alice", Color::Black, "abcd", 1_000),
            ReconnectMatchOutcome::Accepted
        );
    }

    #[test]
    fn pending_reconnect_round_trips_through_serde_json() {
        let original = sample_pending(12_345);
        let s = serde_json::to_string(&original).expect("serialize");
        let restored: PendingReconnect = serde_json::from_str(&s).expect("deserialize");
        assert_eq!(restored, original);
    }

    #[test]
    fn build_resume_message_appends_reconnect_state_block() {
        let snap = sample_snapshot();
        let summary = "BEGIN Game_Summary\nGame_ID:g1\nReconnect_Token:abcd\nEND Game_Summary\n";
        let out = build_resume_message(summary, &snap);
        let end_summary = out.find("END Game_Summary\n").expect("END Game_Summary");
        let begin_state = out.find("BEGIN Reconnect_State\n").expect("BEGIN Reconnect_State");
        let end_state = out.find("END Reconnect_State\n").expect("END Reconnect_State");
        assert!(end_summary < begin_state);
        assert!(begin_state < end_state);
        assert!(out.contains("\nCurrent_Turn:-\n"));
        assert!(out.contains("\nBlack_Time_Remaining_Ms:599500\n"));
        assert!(out.contains("\nWhite_Time_Remaining_Ms:600000\n"));
        assert!(out.contains("\nLast_Move:+7776FU\n"));
    }

    #[test]
    fn build_resume_message_emits_plus_for_black_turn() {
        let mut snap = sample_snapshot();
        snap.current_turn = "black".to_owned();
        let out = build_resume_message("BEGIN Game_Summary\nEND Game_Summary\n", &snap);
        assert!(out.contains("\nCurrent_Turn:+\n"));
    }

    #[test]
    fn build_resume_message_omits_last_move_line_when_none() {
        let mut snap = sample_snapshot();
        snap.last_move = None;
        let out = build_resume_message("BEGIN Game_Summary\nEND Game_Summary\n", &snap);
        assert!(!out.contains("Last_Move:"), "must omit Last_Move when no move played: {out}");
    }

    #[test]
    fn pending_alarm_kind_serde_round_trip() {
        let s = serde_json::to_string(&PendingAlarmKind::TimeUp).expect("serialize TimeUp");
        let restored: PendingAlarmKind = serde_json::from_str(&s).expect("deserialize TimeUp");
        assert_eq!(restored, PendingAlarmKind::TimeUp);
        let s =
            serde_json::to_string(&PendingAlarmKind::GraceExpired).expect("serialize GraceExpired");
        let restored: PendingAlarmKind =
            serde_json::from_str(&s).expect("deserialize GraceExpired");
        assert_eq!(restored, PendingAlarmKind::GraceExpired);
    }

    #[test]
    fn color_str_round_trip() {
        assert_eq!(color_to_str(Color::Black), "black");
        assert_eq!(color_to_str(Color::White), "white");
        assert_eq!(color_from_str("black").unwrap(), Color::Black);
        assert_eq!(color_from_str("white").unwrap(), Color::White);
        assert!(color_from_str("rainbow").is_err());
    }

    /// production の保守的既定 (`grace=0`) では token を発行しない。
    /// `Reconnect_Token:` 拡張行も `PersistedConfig` への保存も skip される。
    #[test]
    fn issue_tokens_if_enabled_returns_none_when_grace_is_zero() {
        let (black, white) = issue_tokens_if_enabled(Duration::ZERO);
        assert!(black.is_none(), "black token must be None when grace=0");
        assert!(white.is_none(), "white token must be None when grace=0");
    }

    /// `grace > 0` の構成では両対局者向けに token を発行する。
    #[test]
    fn issue_tokens_if_enabled_returns_some_when_grace_is_positive() {
        let (black, white) = issue_tokens_if_enabled(Duration::from_secs(30));
        assert!(black.is_some(), "black token must be Some when grace>0");
        assert!(white.is_some(), "white token must be Some when grace>0");
    }

    /// `grace > 0` で発行した 2 つの token は一意でなければならない (相手色の
    /// token を盗用すれば対局を奪える脆弱性を防ぐため、`ReconnectToken::generate`
    /// は 32 文字 hex の十分なエントロピを持つ実装を契約する)。
    #[test]
    fn issue_tokens_if_enabled_yields_distinct_tokens() {
        let (black, white) = issue_tokens_if_enabled(Duration::from_secs(30));
        let black = black.expect("grace>0 で black token は Some");
        let white = white.expect("grace>0 で white token は Some");
        assert_ne!(black.as_str(), white.as_str(), "黒/白 token は互いに異なるべき (盗用防止)");
    }

    #[test]
    fn classify_alarm_after_enter_grace_keeps_earlier_turn_alarm_as_time_up() {
        let (kind, should_set_grace) = classify_alarm_after_enter_grace(Some(1_000), 2_000);
        assert_eq!(kind, PendingAlarmKind::TimeUp);
        assert!(!should_set_grace);
    }

    #[test]
    fn classify_alarm_after_enter_grace_uses_grace_when_deadline_is_earlier() {
        let (kind, should_set_grace) = classify_alarm_after_enter_grace(Some(3_000), 2_000);
        assert_eq!(kind, PendingAlarmKind::GraceExpired);
        assert!(should_set_grace);
    }

    #[test]
    fn classify_alarm_after_enter_grace_uses_grace_when_no_alarm_exists() {
        let (kind, should_set_grace) = classify_alarm_after_enter_grace(None, 2_000);
        assert_eq!(kind, PendingAlarmKind::GraceExpired);
        assert!(should_set_grace);
    }
}
