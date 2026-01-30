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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_read_signed_leb128_stream() {
        // ストリームからの読み込みテスト
        let data = vec![0x00, 0x7F, 0x80, 0x01]; // 0, -1, 128
        let mut cursor = Cursor::new(data);

        let val = read_signed_leb128(&mut cursor).unwrap();
        assert_eq!(val, 0);

        let val = read_signed_leb128(&mut cursor).unwrap();
        assert_eq!(val, -1);

        let val = read_signed_leb128(&mut cursor).unwrap();
        assert_eq!(val, 128);
    }

    #[test]
    fn test_read_signed_leb128_positive() {
        // 0 → 0x00
        let mut cursor = Cursor::new(vec![0x00]);
        let val = read_signed_leb128(&mut cursor).unwrap();
        assert_eq!(val, 0);

        // 1 → 0x01
        let mut cursor = Cursor::new(vec![0x01]);
        let val = read_signed_leb128(&mut cursor).unwrap();
        assert_eq!(val, 1);

        // 63 → 0x3F
        let mut cursor = Cursor::new(vec![0x3F]);
        let val = read_signed_leb128(&mut cursor).unwrap();
        assert_eq!(val, 63);

        // 64 → 0xC0 0x00
        let mut cursor = Cursor::new(vec![0xC0, 0x00]);
        let val = read_signed_leb128(&mut cursor).unwrap();
        assert_eq!(val, 64);

        // 127 → 0xFF 0x00
        let mut cursor = Cursor::new(vec![0xFF, 0x00]);
        let val = read_signed_leb128(&mut cursor).unwrap();
        assert_eq!(val, 127);

        // 128 → 0x80 0x01
        let mut cursor = Cursor::new(vec![0x80, 0x01]);
        let val = read_signed_leb128(&mut cursor).unwrap();
        assert_eq!(val, 128);
    }

    #[test]
    fn test_read_signed_leb128_negative() {
        // -1 → 0x7F
        let mut cursor = Cursor::new(vec![0x7F]);
        let val = read_signed_leb128(&mut cursor).unwrap();
        assert_eq!(val, -1);

        // -64 → 0x40
        let mut cursor = Cursor::new(vec![0x40]);
        let val = read_signed_leb128(&mut cursor).unwrap();
        assert_eq!(val, -64);

        // -65 → 0xBF 0x7F
        let mut cursor = Cursor::new(vec![0xBF, 0x7F]);
        let val = read_signed_leb128(&mut cursor).unwrap();
        assert_eq!(val, -65);

        // -128 → 0x80 0x7F
        let mut cursor = Cursor::new(vec![0x80, 0x7F]);
        let val = read_signed_leb128(&mut cursor).unwrap();
        assert_eq!(val, -128);
    }

    #[test]
    fn test_read_signed_leb128_i16_range() {
        // i16の範囲内の値が正しく読み込まれることを確認
        // i16::MAX = 32767 = 0xFF 0xFF 0x01
        let mut cursor = Cursor::new(vec![0xFF, 0xFF, 0x01]);
        let val = read_signed_leb128(&mut cursor).unwrap();
        assert_eq!(val, 32767);
        assert_eq!(val as i16, i16::MAX);

        // i16::MIN = -32768 = 0x80 0x80 0x7E
        let mut cursor = Cursor::new(vec![0x80, 0x80, 0x7E]);
        let val = read_signed_leb128(&mut cursor).unwrap();
        assert_eq!(val, -32768);
        assert_eq!(val as i16, i16::MIN);
    }
}
