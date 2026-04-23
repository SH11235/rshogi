//! Workers 側 x1 拡張で使う R2 キー生成ヘルパ。
//!
//! buoy 名 / game_id は任意文字列を含み得るため、R2 オブジェクトキーへ埋める前に
//! 可逆な `%XX` 形式でエスケープする。

/// オブジェクトキーに安全なエンコーディングへ変換する。
///
/// - ASCII 英数字と `-` / `_` はそのまま。
/// - それ以外は UTF-8 byte 単位で `%XX` (大文字 hex) にエスケープする。
pub fn encode_component(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for b in raw.bytes() {
        let is_safe = b.is_ascii_alphanumeric() || b == b'-' || b == b'_';
        if is_safe {
            out.push(b as char);
        } else {
            out.push('%');
            const HEX: &[u8; 16] = b"0123456789ABCDEF";
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0x0f) as usize] as char);
        }
    }
    out
}

/// buoy 保存先の R2 キー。
pub fn buoy_object_key(game_name: &str) -> String {
    format!("buoys/{}.json", encode_component(game_name))
}

/// game_id から逆引きする棋譜本体キー。
pub fn kifu_by_id_object_key(game_id: &str) -> String {
    format!("kifu-by-id/{}.csa", encode_component(game_id))
}

/// `%%FORK` で省略時に使う既定の buoy 名。
pub fn default_fork_buoy_name(source_game: &str, nth_move: Option<u32>) -> String {
    let suffix = nth_move.map_or_else(|| "final".to_owned(), |n| n.to_string());
    format!("{source_game}-fork-{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_component_preserves_safe_ascii() {
        assert_eq!(encode_component("floodgate-600_10"), "floodgate-600_10");
    }

    #[test]
    fn encode_component_escapes_slash_and_dot_and_utf8() {
        assert_eq!(encode_component("../a/b"), "%2E%2E%2Fa%2Fb");
        assert_eq!(encode_component("対局"), "%E5%AF%BE%E5%B1%80");
    }

    #[test]
    fn fork_default_name_uses_final_when_nth_missing() {
        assert_eq!(default_fork_buoy_name("20260417120000", None), "20260417120000-fork-final");
        assert_eq!(default_fork_buoy_name("20260417120000", Some(24)), "20260417120000-fork-24");
    }
}
