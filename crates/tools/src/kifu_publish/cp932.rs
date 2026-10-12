//! 棋譜ファイルの文字コード判定と CP932 (Windows の Shift_JIS) の decode。
//!
//! 大会の公式棋譜アーカイブは CP932 と UTF-8 が混在している。サーバーへは UTF-8 で
//! 送るので、UTF-8 として読めなければ CP932 として読む。

use anyhow::{Result, bail};

/// 2 byte 文字の対応表。第 1 byte `0x81..=0x9F, 0xE0..=0xFC` (60 個) × 第 2 byte
/// `0x40..=0xFC` (189 個) の順に、Unicode のコードポイント (BMP、little endian の
/// u16) を並べたもの。割り当ての無い組は `0xFFFF`。
///
/// Python の `cp932` codec から生成した:
/// `bytes([lead, trail]).decode("cp932")` を上の順に回し、1 文字に decode できた
/// ものの `ord()` を書き出す。
const TABLE: &[u8] = include_bytes!("cp932_table.bin");

const TRAIL_MIN: u8 = 0x40;
const TRAIL_MAX: u8 = 0xFC;
const TRAILS_PER_LEAD: usize = (TRAIL_MAX - TRAIL_MIN) as usize + 1;

fn lead_index(byte: u8) -> Option<usize> {
    match byte {
        0x81..=0x9F => Some(usize::from(byte - 0x81)),
        0xE0..=0xFC => Some(usize::from(byte - 0xE0) + 0x1F),
        _ => None,
    }
}

/// CP932 の byte 列を文字列にする。割り当ての無い byte があれば `None`。
pub fn decode_cp932(bytes: &[u8]) -> Option<String> {
    let mut text = String::with_capacity(bytes.len());
    let mut iter = bytes.iter().copied();
    while let Some(byte) = iter.next() {
        match byte {
            0x00..=0x7F => text.push(char::from(byte)),
            // 半角カタカナ (U+FF61..=U+FF9F)。
            0xA1..=0xDF => text.push(char::from_u32(0xFF61 + u32::from(byte - 0xA1))?),
            _ => {
                let lead = lead_index(byte)?;
                let trail = iter.next().filter(|trail| (TRAIL_MIN..=TRAIL_MAX).contains(trail))?;
                let offset = (lead * TRAILS_PER_LEAD + usize::from(trail - TRAIL_MIN)) * 2;
                let code = u16::from_le_bytes([*TABLE.get(offset)?, *TABLE.get(offset + 1)?]);
                if code == 0xFFFF {
                    return None;
                }
                text.push(char::from_u32(u32::from(code))?);
            }
        }
    }
    Some(text)
}

/// 棋譜ファイルの中身を文字列にする。UTF-8 (BOM は除く) として読めなければ CP932 として
/// 読む。どちらでもなければエラー。
pub fn decode_kifu_bytes(bytes: &[u8]) -> Result<String> {
    if let Ok(text) = std::str::from_utf8(bytes) {
        return Ok(text.strip_prefix('\u{feff}').unwrap_or(text).to_owned());
    }
    match decode_cp932(bytes) {
        Some(text) => Ok(text),
        None => bail!("UTF-8 でも Shift_JIS (CP932) でもありません"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_has_one_entry_per_lead_and_trail_pair() {
        assert_eq!(TABLE.len(), 60 * TRAILS_PER_LEAD * 2);
    }

    #[test]
    fn decodes_kifu_text() {
        // 「先手：水匠」+ 半角カナ「ｱ」+ 全角記号「＋×」+ ASCII。
        let bytes = [
            0x90, 0xE6, 0x8E, 0xE8, 0x81, 0x46, 0x90, 0x85, 0x8F, 0xA0, 0xB1, 0x81, 0x7B, 0x81,
            0x7E, b'A', b'\r', b'\n',
        ];
        assert_eq!(decode_cp932(&bytes).unwrap(), "先手：水匠ｱ＋×A\r\n");
        // NEC 特殊文字 (丸数字) と IBM 拡張漢字 (髙) も CP932 の範囲。
        assert_eq!(decode_cp932(&[0x87, 0x40, 0xFB, 0xFC]).unwrap(), "①髙");
    }

    #[test]
    fn rejects_bytes_outside_cp932() {
        // 第 2 byte が欠けている / 範囲外 / 未割り当て。
        assert_eq!(decode_cp932(&[0x90]), None);
        assert_eq!(decode_cp932(&[0x90, 0x20]), None);
        assert_eq!(decode_cp932(&[0x85, 0x40]), None);
        // 第 1 byte として使えない値。
        assert_eq!(decode_cp932(&[0x80]), None);
        assert_eq!(decode_cp932(&[0xFF, 0x40]), None);
    }

    #[test]
    fn utf8_wins_over_cp932_and_bom_is_dropped() {
        assert_eq!(decode_kifu_bytes("N+水匠\n".as_bytes()).unwrap(), "N+水匠\n");
        assert_eq!(decode_kifu_bytes(b"\xEF\xBB\xBFV2.2\n").unwrap(), "V2.2\n");
        assert_eq!(decode_kifu_bytes(&[b'N', b'+', 0x90, 0x85, 0x8F, 0xA0]).unwrap(), "N+水匠");
        assert!(decode_kifu_bytes(&[0x80, 0xFF]).is_err());
    }
}
