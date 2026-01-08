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

/// 圧縮形式かどうかをチェックし、LEB128バッファを読み込む
///
/// nnue-pytorch形式:
/// - "COMPRESSED_LEB128" (17バイト)
/// - int32: 圧縮データのサイズ
/// - 圧縮データ（LEB128エンコードされたバイト列）
pub fn read_compressed_tensor_i16<R: Read>(reader: &mut R, count: usize) -> io::Result<Vec<i16>> {
    // まず17バイトをpeek
    let mut magic_buf = [0u8; 17];
    reader.read_exact(&mut magic_buf)?;

    if magic_buf == LEB128_MAGIC {
        // LEB128圧縮形式
        let mut size_buf = [0u8; 4];
        reader.read_exact(&mut size_buf)?;
        let compressed_size = u32::from_le_bytes(size_buf) as usize;

        // 圧縮データを読み込み
        let mut compressed_data = vec![0u8; compressed_size];
        reader.read_exact(&mut compressed_data)?;

        // LEB128デコード
        decode_leb128_array_i16(&compressed_data, count)
    } else {
        // 非圧縮形式: magic_bufは実際のデータの一部
        // i16として読み込む必要がある
        // magic_bufの17バイト = 8個の i16 + 1バイト
        // これは少し厄介なので、全体を読み直す方が簡単

        // 既に読んだ17バイトから i16 を復元
        let mut result = Vec::with_capacity(count);

        // 17バイトから8個のi16を読む
        let mut idx = 0;
        while idx + 1 < magic_buf.len() && result.len() < count {
            let val = i16::from_le_bytes([magic_buf[idx], magic_buf[idx + 1]]);
            result.push(val);
            idx += 2;
        }

        // 残りの byte がある場合は次のバイトと組み合わせる
        let leftover: Option<u8> = if idx < magic_buf.len() {
            Some(magic_buf[idx])
        } else {
            None
        };

        // 残りを読み込み
        let remaining = count - result.len();
        if remaining > 0 {
            let mut buf = [0u8; 2];

            if let Some(first_byte) = leftover {
                // 1バイト残りがある場合
                reader.read_exact(&mut buf[..1])?;
                let val = i16::from_le_bytes([first_byte, buf[0]]);
                result.push(val);
            }

            // 残りを2バイトずつ読む
            let still_remaining = count - result.len();
            for _ in 0..still_remaining {
                reader.read_exact(&mut buf)?;
                result.push(i16::from_le_bytes(buf));
            }
        }

        Ok(result)
    }
}

/// LEB128エンコードされたバイト列から i16 配列をデコード
fn decode_leb128_array_i16(data: &[u8], count: usize) -> io::Result<Vec<i16>> {
    let mut result = Vec::with_capacity(count);
    let mut pos = 0;

    for _ in 0..count {
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
    fn test_read_compressed_tensor_i16_uncompressed() {
        // 非圧縮形式のテスト: [1, 2, 3] をi16 little endianで
        let data: Vec<u8> = vec![
            0x01, 0x00, // 1
            0x02, 0x00, // 2
            0x03, 0x00, // 3
            // 17バイトに達するまでパディング
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        let mut cursor = Cursor::new(data);
        let result = read_compressed_tensor_i16(&mut cursor, 3).unwrap();
        assert_eq!(result, vec![1, 2, 3]);
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
    fn test_decode_leb128_array_count_mismatch() {
        // 要求数より少ないデータ
        let data = [0x00, 0x01]; // 2つの値
        let result = decode_leb128_array_i16(&data, 10); // 10個要求
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
