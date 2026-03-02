//! LEB128（Little Endian Base 128）デコーダ
//!
//! nnue-pytorch の圧縮形式で使用される可変長整数エンコーディング。

use std::io::{self, Read};

/// COMPRESSED_LEB128 マジック文字列
pub const LEB128_MAGIC: &[u8] = b"COMPRESSED_LEB128";

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

/// バイトスライスからLEB128値を1つデコード
///
/// 戻り値: (デコードされた値, 消費したバイト数)
fn decode_single_leb128(data: &[u8]) -> io::Result<(i64, usize)> {
    let mut result: i64 = 0;
    let mut shift = 0;
    let mut pos = 0;

    loop {
        if pos >= data.len() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Unexpected end of LEB128 data",
            ));
        }

        let b = data[pos];
        pos += 1;

        result |= ((b & 0x7f) as i64) << shift;
        shift += 7;

        if b & 0x80 == 0 {
            // 符号拡張
            if shift < 64 && (b & 0x40) != 0 {
                result |= !0i64 << shift;
            }
            break;
        }

        if shift >= 64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "LEB128 overflow: value too large",
            ));
        }
    }

    Ok((result, pos))
}

/// LEB128圧縮ブロックを読み込み、全値をデコードして返す
///
/// count を指定せず、圧縮データ内の全値をデコードする。
/// ブロック内の要素数で形式（biases のみ / biases+weights 結合）を判別する用途に使う。
pub fn read_compressed_tensor_i16_all<R: Read>(reader: &mut R) -> io::Result<Vec<i16>> {
    let mut magic_buf = [0u8; 17];
    reader.read_exact(&mut magic_buf)?;

    if magic_buf != LEB128_MAGIC {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "Expected COMPRESSED_LEB128 magic"));
    }

    let mut size_buf = [0u8; 4];
    reader.read_exact(&mut size_buf)?;
    let compressed_size = u32::from_le_bytes(size_buf) as usize;

    const MAX_COMPRESSED_SIZE: usize = 256 * 1024 * 1024;
    if compressed_size == 0 || compressed_size > MAX_COMPRESSED_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Invalid compressed size: {compressed_size} (max: {MAX_COMPRESSED_SIZE})"),
        ));
    }

    let mut compressed_data = vec![0u8; compressed_size];
    reader.read_exact(&mut compressed_data)?;

    decode_leb128_all_i16(&compressed_data)
}

/// LEB128エンコードされたバイト列から全 i16 値をデコード
fn decode_leb128_all_i16(data: &[u8]) -> io::Result<Vec<i16>> {
    let mut result = Vec::new();
    let mut pos = 0;
    while pos < data.len() {
        let (val, consumed) = decode_single_leb128(&data[pos..])?;
        result.push(val as i16);
        pos += consumed;
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_decode_single_leb128_positive() {
        // 0 → 0x00
        let (val, consumed) = decode_single_leb128(&[0x00]).unwrap();
        assert_eq!(val, 0);
        assert_eq!(consumed, 1);

        // 1 → 0x01
        let (val, consumed) = decode_single_leb128(&[0x01]).unwrap();
        assert_eq!(val, 1);
        assert_eq!(consumed, 1);

        // 63 → 0x3F
        let (val, consumed) = decode_single_leb128(&[0x3F]).unwrap();
        assert_eq!(val, 63);
        assert_eq!(consumed, 1);

        // 64 → 0xC0 0x00
        let (val, consumed) = decode_single_leb128(&[0xC0, 0x00]).unwrap();
        assert_eq!(val, 64);
        assert_eq!(consumed, 2);

        // 127 → 0xFF 0x00
        let (val, consumed) = decode_single_leb128(&[0xFF, 0x00]).unwrap();
        assert_eq!(val, 127);
        assert_eq!(consumed, 2);

        // 128 → 0x80 0x01
        let (val, consumed) = decode_single_leb128(&[0x80, 0x01]).unwrap();
        assert_eq!(val, 128);
        assert_eq!(consumed, 2);
    }

    #[test]
    fn test_decode_single_leb128_negative() {
        // -1 → 0x7F
        let (val, _) = decode_single_leb128(&[0x7F]).unwrap();
        assert_eq!(val, -1);

        // -64 → 0x40
        let (val, _) = decode_single_leb128(&[0x40]).unwrap();
        assert_eq!(val, -64);

        // -65 → 0xBF 0x7F
        let (val, _) = decode_single_leb128(&[0xBF, 0x7F]).unwrap();
        assert_eq!(val, -65);

        // -128 → 0x80 0x7F
        let (val, _) = decode_single_leb128(&[0x80, 0x7F]).unwrap();
        assert_eq!(val, -128);
    }

    #[test]
    fn test_read_compressed_tensor_i16_all() {
        // LEB128 圧縮形式: [1, -1, 127] をエンコード
        // 1 → 0x01, -1 → 0x7F, 127 → 0xFF 0x00
        let compressed = vec![0x01, 0x7F, 0xFF, 0x00];
        let mut data = Vec::new();
        data.extend_from_slice(b"COMPRESSED_LEB128");
        data.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
        data.extend_from_slice(&compressed);

        let mut cursor = Cursor::new(data);
        let result = read_compressed_tensor_i16_all(&mut cursor).unwrap();
        assert_eq!(result, vec![1, -1, 127]);
    }

    #[test]
    fn test_decode_single_leb128_early_eof() {
        // 継続ビットが立っているが次のバイトがない
        let result = decode_single_leb128(&[0x80]); // 継続ビットが立っているが終端
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Unexpected end"));

        // 空のデータ
        let result = decode_single_leb128(&[]);
        assert!(result.is_err());
    }

    #[test]
    fn test_decode_single_leb128_large_values() {
        // 多バイトエンコーディング（正常系）
        // 300 = 0xAC 0x02
        let (val, consumed) = decode_single_leb128(&[0xAC, 0x02]).unwrap();
        assert_eq!(val, 300);
        assert_eq!(consumed, 2);

        // 16384 = 0x80 0x80 0x01
        let (val, consumed) = decode_single_leb128(&[0x80, 0x80, 0x01]).unwrap();
        assert_eq!(val, 16384);
        assert_eq!(consumed, 3);
    }

    #[test]
    fn test_read_compressed_tensor_i16_all_invalid_magic() {
        let data = vec![0x00; 21]; // マジックが一致しない
        let mut cursor = Cursor::new(data);
        let result = read_compressed_tensor_i16_all(&mut cursor);
        assert!(result.is_err());
    }

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
    fn test_read_signed_leb128_i16_range() {
        // i16の範囲内の値が正しく読み込まれることを確認
        // i16::MAX = 32767 = 0xFF 0xFF 0x01
        let (val, _) = decode_single_leb128(&[0xFF, 0xFF, 0x01]).unwrap();
        assert_eq!(val, 32767);
        assert_eq!(val as i16, i16::MAX);

        // i16::MIN = -32768 = 0x80 0x80 0x7E
        let (val, _) = decode_single_leb128(&[0x80, 0x80, 0x7E]).unwrap();
        assert_eq!(val, -32768);
        assert_eq!(val as i16, i16::MIN);
    }
}
