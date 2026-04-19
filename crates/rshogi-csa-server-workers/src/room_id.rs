//! `room_id` の入力バリデーション（純粋関数）。
//!
//! Cloudflare の `id_from_name` は任意文字列を受け付けるが、ログ / メトリクス
//! の可観測性と将来の KV / R2 キーとの混在時の安全性（制御文字・パス区切り
//! による意図しない階層化）を考慮し、Workers 側で受付文字種を制約する。
//!
//! Phase 2 では最小限のホワイトリスト（ASCII 英数字 + `-` + `_`、長さ 1〜128）
//! を採用する。運用で命名規約が広がれば上限と文字種を緩める。本モジュールは
//! worker ランタイムに依存しないので、ホスト target で単体テストできる。

/// `room_id` として受け付けられる最大長（バイト）。
pub const ROOM_ID_MAX_LEN: usize = 128;

/// `room_id` のバリデーション。OK なら `true`。
pub fn is_valid_room_id(s: &str) -> bool {
    if s.is_empty() || s.len() > ROOM_ID_MAX_LEN {
        return false;
    }
    s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_ascii_alnum_and_hyphen_underscore() {
        assert!(is_valid_room_id("room1"));
        assert!(is_valid_room_id("Room_A-1"));
        assert!(is_valid_room_id("abc123"));
    }

    #[test]
    fn rejects_empty_and_overlong() {
        assert!(!is_valid_room_id(""));
        let too_long = "a".repeat(ROOM_ID_MAX_LEN + 1);
        assert!(!is_valid_room_id(&too_long));
    }

    #[test]
    fn rejects_control_and_non_ascii() {
        assert!(!is_valid_room_id("room id"));
        assert!(!is_valid_room_id("room/1"));
        assert!(!is_valid_room_id("room.1"));
        assert!(!is_valid_room_id("部屋"));
        assert!(!is_valid_room_id("room\n1"));
    }

    #[test]
    fn accepts_max_length() {
        let max = "a".repeat(ROOM_ID_MAX_LEN);
        assert!(is_valid_room_id(&max));
    }
}
