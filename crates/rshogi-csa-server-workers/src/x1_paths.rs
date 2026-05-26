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

/// buoy R2 オブジェクトキーの prefix (`buoys/`)。
///
/// `delete_buoy` 経路の delete 対象を本 prefix 配下に限定するための path
/// validation 定数 (https://github.com/SH11235/rshogi/issues/624)。`buoy_object_key`
/// と削除側の runtime ガードで共有することで、prefix 文字列のずれを回避する。
pub const BUOY_KEY_PREFIX: &str = "buoys/";

/// buoy 保存先の R2 キー。
pub fn buoy_object_key(game_name: &str) -> String {
    format!("{BUOY_KEY_PREFIX}{}.json", encode_component(game_name))
}

/// game_id から逆引きする棋譜本体キー。
pub fn kifu_by_id_object_key(game_id: &str) -> String {
    format!("kifu-by-id/{}.csa", encode_component(game_id))
}

/// game_id から逆引きする棋譜メタ (`<id>.meta.json`) キー。
///
/// `kifu_by_id_object_key` と同じ `encode_component(game_id)` を通すことで、
/// CSA 本体キーと完全に同じエンコーディング規約に揃える (https://github.com/SH11235/rshogi/issues/551 v3 §12)。
/// reader (viewer_api) と writer (game_room / backfill) で生成キーが乖離しない
/// ように、本ヘルパを必ず経由して `<game_id>.meta.json` を構築する。
pub fn kifu_by_id_meta_key(game_id: &str) -> String {
    format!("kifu-by-id/{}.meta.json", encode_component(game_id))
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
    fn buoy_object_key_always_starts_with_prefix() {
        // `delete_buoy` の runtime ガードで使う前提条件 (バックアップ経路の隣接懸念):
        // 任意入力に対して `buoy_object_key` の戻り値は `BUOY_KEY_PREFIX` で
        // 始まる。`encode_component` のエスケープ規則と相俟って、buoy 命名の
        // 不正値が `buoys/` の外に逃げ出さないことを固定する。
        for sample in [
            "name",
            "../escape",
            "..%2F../still-buoys",
            "対局",
            "",
            "/",
            "buoys/",
        ] {
            let key = buoy_object_key(sample);
            assert!(
                key.starts_with(BUOY_KEY_PREFIX),
                "buoy_object_key({sample:?}) = {key:?} did not start with {BUOY_KEY_PREFIX:?}"
            );
        }
    }

    #[test]
    fn fork_default_name_uses_final_when_nth_missing() {
        assert_eq!(default_fork_buoy_name("20260417120000", None), "20260417120000-fork-final");
        assert_eq!(default_fork_buoy_name("20260417120000", Some(24)), "20260417120000-fork-24");
    }

    #[test]
    fn kifu_by_id_meta_key_uses_meta_json_suffix() {
        // ASCII 安全な game_id はそのまま埋まる + 末尾は `.meta.json`。
        assert_eq!(
            kifu_by_id_meta_key("lobby-cross-fischer-1777391025209"),
            "kifu-by-id/lobby-cross-fischer-1777391025209.meta.json",
        );
    }

    #[test]
    fn kifu_by_id_meta_key_encodes_unsafe_chars_consistently_with_object_key() {
        // CSA 本体キーと meta キーは「encode_component を通したあと拡張子だけ
        // 異なる」という不変条件が崩れていないこと。これにより writer/reader が
        // 同一 game_id について常に対応するペアを参照できる。
        let game_id = "../weird id/対局";
        let object = kifu_by_id_object_key(game_id);
        let meta = kifu_by_id_meta_key(game_id);
        let object_stem = object.strip_suffix(".csa").expect("object key ends with .csa");
        let meta_stem = meta.strip_suffix(".meta.json").expect("meta key ends with .meta.json");
        assert_eq!(object_stem, meta_stem, "encoded stem must match between object and meta");
    }
}
