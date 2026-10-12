//! 棋譜ファイルの文字コード判定と CP932 (Windows の Shift_JIS) の decode。
//!
//! 大会の公式棋譜アーカイブは CP932 と UTF-8 が混在している。サーバーへは UTF-8 で
//! 送るので、UTF-8 として読めなければ CP932 として読む。
//!
//! # 読まないもの (意図的な制限)
//!
//! 文字化けした対局者名を登録しないよう、割り当ての確かな文字だけを通す。次のものは
//! 読み取り失敗にする。
//!
//! - 1 byte の `0x80` / `0xA0` / `0xFD`〜`0xFF` (CP932 に割り当てが無い。Windows は
//!   制御文字や私用領域に写すが、棋譜の文字として意味を持たない)。
//! - 外字 (第 1 byte `0xF0`〜`0xF9`)。Unicode の私用領域 (U+E000〜U+F8FF) に写るだけで、
//!   作成者の環境の外字フォントが無いと何の文字か分からない。
//! - 対応表に割り当ての無い 2 byte の組。

use anyhow::{Result, bail};

/// 2 byte 文字の対応表。第 1 byte `0x81..=0x9F, 0xE0..=0xFC` (60 個) × 第 2 byte
/// `0x40..=0xFC` (189 個) の順に、Unicode のコードポイント (BMP、little endian の
/// u16) を並べたもの。割り当ての無い組は `0xFFFF`。
///
/// Python の `cp932` codec から生成した。再生成は、このファイルのあるディレクトリで次の
/// 1 コマンド (内容は [`tests::table_is_the_one_generated_from_python_cp932`] の SHA-256
/// で固定している):
///
/// ```text
/// python -c "f=lambda s: ord(s) if len(s)==1 and s!=chr(0xFFFD) else 0xFFFF; open('cp932_table.bin','wb').write(b''.join(f(bytes([a,b]).decode('cp932','replace')).to_bytes(2,'little') for a in [*range(0x81,0xA0),*range(0xE0,0xFD)] for b in range(0x40,0xFD)))"
/// ```
const TABLE: &[u8] = include_bytes!("cp932_table.bin");

const TRAIL_MIN: u8 = 0x40;
const TRAIL_MAX: u8 = 0xFC;
const TRAILS_PER_LEAD: usize = (TRAIL_MAX - TRAIL_MIN) as usize + 1;

/// 棋譜ファイルを読んだ文字コード。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum KifuEncoding {
    /// UTF-8。
    Utf8,
    /// CP932 (Shift_JIS)。
    Cp932,
}

impl KifuEncoding {
    /// 集計表示用の名前。
    pub fn label(self) -> &'static str {
        match self {
            Self::Utf8 => "utf-8",
            Self::Cp932 => "cp932",
        }
    }
}

fn lead_index(byte: u8) -> Option<usize> {
    match byte {
        0x81..=0x9F => Some(usize::from(byte - 0x81)),
        0xE0..=0xFC => Some(usize::from(byte - 0xE0) + 0x1F),
        // 0x80 / 0xA0 / 0xFD..=0xFF は CP932 に割り当てが無い (モジュールコメント参照)。
        _ => None,
    }
}

/// CP932 の byte 列を文字列にする。割り当ての無い byte や外字 (私用領域に写る文字) が
/// あれば `None`。
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
                // 0xFFFF は未割り当て。私用領域は外字。
                if code == 0xFFFF || (0xE000..=0xF8FF).contains(&code) {
                    return None;
                }
                text.push(char::from_u32(u32::from(code))?);
            }
        }
    }
    Some(text)
}

/// 棋譜ファイルの中身を文字列にし、読んだ文字コードと合わせて返す。UTF-8 (BOM は除く)
/// として読めなければ CP932 として読む。どちらでもなければエラー。
pub fn decode_kifu_bytes(bytes: &[u8]) -> Result<(String, KifuEncoding)> {
    if let Ok(text) = std::str::from_utf8(bytes) {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        return Ok((text.to_owned(), KifuEncoding::Utf8));
    }
    match decode_cp932(bytes) {
        Some(text) => Ok((text, KifuEncoding::Cp932)),
        None => bail!(
            "UTF-8 でも Shift_JIS (CP932) でもありません (外字・未割り当ての文字を含む場合も読みません)"
        ),
    }
}

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha256};

    use super::*;

    #[test]
    fn table_is_the_one_generated_from_python_cp932() {
        assert_eq!(TABLE.len(), 60 * TRAILS_PER_LEAD * 2);
        let digest: String =
            Sha256::digest(TABLE).iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(digest, "2b57fd992804a5b14ae70eb7911e528cce70b19363af33b9b883a7c658548ebd");
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
        // 1 byte で割り当ての無い値 (意図的に読まない)。
        for byte in [0x80, 0xA0, 0xFD, 0xFE, 0xFF] {
            assert_eq!(decode_cp932(&[byte]), None, "{byte:#04x}");
            assert_eq!(decode_cp932(&[b'N', byte, 0x40]), None, "{byte:#04x}");
        }
    }

    #[test]
    fn rejects_user_defined_characters() {
        // 外字領域 (第 1 byte 0xF0..=0xF9) は私用領域 U+E000.. に写るので読まない。
        for bytes in [[0xF0, 0x40], [0xF5, 0x80], [0xF9, 0xFC]] {
            assert_eq!(decode_cp932(&bytes), None, "{bytes:02x?}");
        }
        assert!(decode_kifu_bytes(&[b'N', b'+', 0xF0, 0x40]).is_err());
        // 対応表から私用領域を除いた残りは、どれも私用領域の外。
        for lead in (0x81..=0x9F_u8).chain(0xE0..=0xFC) {
            for trail in TRAIL_MIN..=TRAIL_MAX {
                if let Some(text) = decode_cp932(&[lead, trail]) {
                    let code = u32::from(text.chars().next().unwrap());
                    assert!(!(0xE000..=0xF8FF).contains(&code), "{lead:#04x}{trail:02x}");
                }
            }
        }
    }

    #[test]
    fn utf8_wins_over_cp932_and_bom_is_dropped() {
        assert_eq!(
            decode_kifu_bytes("N+水匠\n".as_bytes()).unwrap(),
            ("N+水匠\n".to_owned(), KifuEncoding::Utf8)
        );
        assert_eq!(
            decode_kifu_bytes(b"\xEF\xBB\xBFV2.2\n").unwrap(),
            ("V2.2\n".to_owned(), KifuEncoding::Utf8)
        );
        assert_eq!(
            decode_kifu_bytes(&[b'N', b'+', 0x90, 0x85, 0x8F, 0xA0]).unwrap(),
            ("N+水匠".to_owned(), KifuEncoding::Cp932)
        );
        assert!(decode_kifu_bytes(&[0x80, 0xFF]).is_err());
        assert_eq!(KifuEncoding::Utf8.label(), "utf-8");
        assert_eq!(KifuEncoding::Cp932.label(), "cp932");
    }
}
