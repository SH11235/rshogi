//! LEB128（Little Endian Base 128）デコーダ
//!
//! nnue-pytorch の圧縮形式で使用される可変長整数エンコーディング。

use std::io::{self, Read};

/// 符号付きLEB128を読み込み
///
/// 各バイトの下位7ビットがデータ、最上位ビットが継続フラグ。
/// 継続フラグが0になるまで読み込む。
pub fn read_signed_leb128<R: Read>(reader: &mut R) -> io::Result<i64> {
    let mut result: i64 = 0;
    let mut shift = 0;
    let mut byte = [0u8; 1];

    loop {
        reader.read_exact(&mut byte)?;
        let b = byte[0];

        // 下位7ビットを結果に追加
        result |= ((b & 0x7f) as i64) << shift;
        shift += 7;

        // 継続フラグが0なら終了
        if b & 0x80 == 0 {
            // 符号拡張（最後のバイトの6ビット目が符号ビット）
            if shift < 64 && (b & 0x40) != 0 {
                result |= !0i64 << shift;
            }
            break;
        }

        // 最大9バイト（64bit）を超えるとエラー
        if shift >= 64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "LEB128 overflow: value too large",
            ));
        }
    }

    Ok(result)
}

/// 符号なしLEB128を読み込み
#[allow(dead_code)]
pub fn read_unsigned_leb128<R: Read>(reader: &mut R) -> io::Result<u64> {
    let mut result: u64 = 0;
    let mut shift = 0;
    let mut byte = [0u8; 1];

    loop {
        reader.read_exact(&mut byte)?;
        let b = byte[0];

        result |= ((b & 0x7f) as u64) << shift;
        shift += 7;

        if b & 0x80 == 0 {
            break;
        }

        if shift >= 64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "LEB128 overflow: value too large",
            ));
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_read_signed_leb128_positive() {
        // 0 → 0x00
        let mut cursor = Cursor::new(vec![0x00]);
        assert_eq!(read_signed_leb128(&mut cursor).unwrap(), 0);

        // 1 → 0x01
        let mut cursor = Cursor::new(vec![0x01]);
        assert_eq!(read_signed_leb128(&mut cursor).unwrap(), 1);

        // 63 → 0x3F
        let mut cursor = Cursor::new(vec![0x3F]);
        assert_eq!(read_signed_leb128(&mut cursor).unwrap(), 63);

        // 64 → 0xC0 0x00
        let mut cursor = Cursor::new(vec![0xC0, 0x00]);
        assert_eq!(read_signed_leb128(&mut cursor).unwrap(), 64);

        // 127 → 0xFF 0x00
        let mut cursor = Cursor::new(vec![0xFF, 0x00]);
        assert_eq!(read_signed_leb128(&mut cursor).unwrap(), 127);

        // 128 → 0x80 0x01
        let mut cursor = Cursor::new(vec![0x80, 0x01]);
        assert_eq!(read_signed_leb128(&mut cursor).unwrap(), 128);
    }

    #[test]
    fn test_read_signed_leb128_negative() {
        // -1 → 0x7F
        let mut cursor = Cursor::new(vec![0x7F]);
        assert_eq!(read_signed_leb128(&mut cursor).unwrap(), -1);

        // -64 → 0x40
        let mut cursor = Cursor::new(vec![0x40]);
        assert_eq!(read_signed_leb128(&mut cursor).unwrap(), -64);

        // -65 → 0xBF 0x7F
        let mut cursor = Cursor::new(vec![0xBF, 0x7F]);
        assert_eq!(read_signed_leb128(&mut cursor).unwrap(), -65);

        // -128 → 0x80 0x7F
        let mut cursor = Cursor::new(vec![0x80, 0x7F]);
        assert_eq!(read_signed_leb128(&mut cursor).unwrap(), -128);
    }

    #[test]
    fn test_read_unsigned_leb128() {
        // 0 → 0x00
        let mut cursor = Cursor::new(vec![0x00]);
        assert_eq!(read_unsigned_leb128(&mut cursor).unwrap(), 0);

        // 127 → 0x7F
        let mut cursor = Cursor::new(vec![0x7F]);
        assert_eq!(read_unsigned_leb128(&mut cursor).unwrap(), 127);

        // 128 → 0x80 0x01
        let mut cursor = Cursor::new(vec![0x80, 0x01]);
        assert_eq!(read_unsigned_leb128(&mut cursor).unwrap(), 128);

        // 16383 → 0xFF 0x7F
        let mut cursor = Cursor::new(vec![0xFF, 0x7F]);
        assert_eq!(read_unsigned_leb128(&mut cursor).unwrap(), 16383);
    }
}
