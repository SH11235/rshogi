//! LEB128（Little Endian Base 128）デコーダ
//!
//! nnue-pytorch の圧縮形式で使用される可変長整数エンコーディング。

use std::io::{self, Read};
use std::ops::DerefMut;

/// COMPRESSED_LEB128 マジック文字列
pub const LEB128_MAGIC: &[u8] = b"COMPRESSED_LEB128";

pub(crate) const MAX_COMPRESSED_SIZE: usize = 2 * 1024 * 1024 * 1024;

/// LayerStacks FT の biases / weights を LEB128 から読む。
pub(crate) fn read_layer_stacks_ft_i16<R, W, F>(
    reader: &mut R,
    biases: &mut [i16],
    weight_count: usize,
    allocate_weights: F,
) -> io::Result<W>
where
    R: Read,
    W: DerefMut<Target = [i16]>,
    F: FnOnce(usize) -> io::Result<W>,
{
    let combined_count = biases
        .len()
        .checked_add(weight_count)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "FT element count overflow"))?;
    let mut first = read_compressed_i16_block(reader, combined_count)?;
    first.decode_into(biases, "FT biases")?;

    if first.is_exhausted() {
        drop(first);
        let mut weights_block = read_compressed_i16_block(reader, weight_count)?;
        weights_block.validate_remaining_count(weight_count, "FT weights")?;
        let mut weights = allocate_weights(weight_count)?;
        weights_block.decode_into(&mut weights, "FT weights")?;
        weights_block.require_exhausted("FT weights", weight_count)?;
        Ok(weights)
    } else {
        first.validate_remaining_count(weight_count, "combined FT weights")?;
        let mut weights = allocate_weights(weight_count)?;
        first.decode_into(&mut weights, "combined FT weights")?;
        first.require_exhausted("combined FT", combined_count)?;
        Ok(weights)
    }
}

struct CompressedI16Block {
    payload: Vec<u8>,
    position: usize,
}

impl CompressedI16Block {
    fn validate_remaining_count(&self, expected: usize, name: &str) -> io::Result<()> {
        let mut position = self.position;
        for index in 0..expected {
            if position == self.payload.len() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{name} block size mismatch: got {index} values, expected {expected}"),
                ));
            }
            let (_, consumed) = decode_single_leb128(&self.payload[position..])?;
            position += consumed;
        }
        if position != self.payload.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{name} block size mismatch: expected {expected} values"),
            ));
        }
        Ok(())
    }

    fn decode_into(&mut self, destination: &mut [i16], name: &str) -> io::Result<()> {
        let expected = destination.len();
        for (index, value) in destination.iter_mut().enumerate() {
            if self.is_exhausted() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{name} block size mismatch: got {index} values, expected {expected}"),
                ));
            }
            let (decoded, consumed) = decode_single_leb128(&self.payload[self.position..])?;
            *value = decoded as i16;
            self.position += consumed;
        }
        Ok(())
    }

    fn require_exhausted(&self, name: &str, expected: usize) -> io::Result<()> {
        if self.is_exhausted() {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{name} block size mismatch: expected {expected} values"),
            ))
        }
    }

    fn is_exhausted(&self) -> bool {
        self.position == self.payload.len()
    }
}

fn read_compressed_i16_block<R: Read>(
    reader: &mut R,
    max_values: usize,
) -> io::Result<CompressedI16Block> {
    let mut magic_buf = [0u8; 17];
    reader.read_exact(&mut magic_buf)?;
    if magic_buf != LEB128_MAGIC {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "Expected COMPRESSED_LEB128 magic"));
    }

    let mut size_buf = [0u8; 4];
    reader.read_exact(&mut size_buf)?;
    let compressed_size = u32::from_le_bytes(size_buf) as usize;
    validate_compressed_size(compressed_size)?;
    // 既存 decoder は非最短形も受け付けるため、i64 の最大符号化長を使う。
    if compressed_size > max_values.saturating_mul(10) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "compressed FT block exceeds its maximum encoded element count",
        ));
    }

    let mut payload = Vec::new();
    let mut chunk = [0; 8192];
    while payload.len() < compressed_size {
        let requested = chunk.len().min(compressed_size - payload.len());
        let read = match reader.read(&mut chunk[..requested]) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        let new_len = payload.len() + read;
        if new_len > payload.capacity() {
            let capacity = new_len.max(payload.capacity().saturating_mul(2)).min(compressed_size);
            payload
                .try_reserve_exact(capacity - payload.len())
                .map_err(|_| io::Error::from(io::ErrorKind::OutOfMemory))?;
        }
        payload.extend_from_slice(&chunk[..read]);
    }
    Ok(CompressedI16Block {
        payload,
        position: 0,
    })
}

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
pub(crate) fn decode_single_leb128(data: &[u8]) -> io::Result<(i64, usize)> {
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

/// 符号付き整数を最短形の LEB128 として追記する。
pub(crate) fn encode_signed_leb128(mut value: i64, output: &mut Vec<u8>) {
    loop {
        let byte = (value as u8) & 0x7f;
        value >>= 7;
        let done = (value == 0 && byte & 0x40 == 0) || (value == -1 && byte & 0x40 != 0);
        output.push(if done { byte } else { byte | 0x80 });
        if done {
            break;
        }
    }
}

/// LEB128圧縮ブロックを読み込み、全値をデコードして返す
///
/// count を指定せず、圧縮データ内の全値をデコードする。
/// ブロック内の要素数で形式（biases のみ / biases+weights 結合）を判別する用途に使う。
#[cfg(test)]
pub fn read_compressed_tensor_i16_all<R: Read>(reader: &mut R) -> io::Result<Vec<i16>> {
    let block = read_compressed_i16_block(reader, usize::MAX)?;
    decode_leb128_all_i16(&block.payload)
}

fn validate_compressed_size(compressed_size: usize) -> io::Result<()> {
    // 不正ファイルの巨大 alloc を防ぐ sanity 上限。HalfKaHmMerged + EffectBucket は base 特徴を NB 倍に
    // 拡張するため FT block が大きく (2x2×1024=600MB 生 / ~300MB 圧縮、3x3 系はさらに大)、
    // 旧 256MB では正当な effect bucket net を弾く。size field は u32 なので上限は 4GiB 未満。
    if compressed_size == 0 || compressed_size > MAX_COMPRESSED_SIZE {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Invalid compressed size: {compressed_size} (max: {MAX_COMPRESSED_SIZE})"),
        ))
    } else {
        Ok(())
    }
}

/// LEB128エンコードされたバイト列から全 i16 値をデコード
#[cfg(test)]
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

    fn compressed_block(values: &[i16]) -> Vec<u8> {
        assert!(values.iter().all(|value| (-64..=63).contains(value)));
        let mut block = Vec::new();
        block.extend_from_slice(LEB128_MAGIC);
        block.extend_from_slice(&(values.len() as u32).to_le_bytes());
        block.extend(values.iter().map(|value| (*value as u8) & 0x7f));
        block
    }

    fn decode_ft_with_old_loader(
        bytes: &[u8],
        bias_count: usize,
        weight_count: usize,
    ) -> (Vec<i16>, Vec<i16>) {
        let mut reader = Cursor::new(bytes);
        let first = read_compressed_tensor_i16_all(&mut reader).expect("first block");
        if first.len() == bias_count + weight_count {
            (first[..bias_count].to_vec(), first[bias_count..].to_vec())
        } else {
            assert_eq!(first.len(), bias_count);
            let weights = read_compressed_tensor_i16_all(&mut reader).expect("weights block");
            assert_eq!(weights.len(), weight_count);
            (first, weights)
        }
    }

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
    fn layer_stacks_ft_streaming_matches_old_loader() {
        let biases = [1, -2, 3];
        let weights = [4, -5, 6, -7];
        let combined =
            compressed_block(&biases.iter().chain(&weights).copied().collect::<Vec<_>>());
        let mut split = compressed_block(&biases);
        split.extend_from_slice(&compressed_block(&weights));

        for bytes in [combined, split] {
            let expected = decode_ft_with_old_loader(&bytes, biases.len(), weights.len());
            let mut actual_biases = vec![0; biases.len()];
            let actual_weights = read_layer_stacks_ft_i16(
                &mut Cursor::new(bytes),
                &mut actual_biases,
                weights.len(),
                |len| Ok(vec![0; len]),
            )
            .expect("streaming FT");
            assert_eq!((actual_biases, actual_weights), expected);
        }
    }

    #[test]
    fn layer_stacks_ft_streaming_rejects_size_mismatch() {
        let combined_too_short = compressed_block(&[1, 2, 3]);
        let mut split_too_long = compressed_block(&[1, 2]);
        split_too_long.extend_from_slice(&compressed_block(&[3, 4, 5]));

        for bytes in [combined_too_short, split_too_long] {
            let result = read_layer_stacks_ft_i16(&mut Cursor::new(bytes), &mut [0; 2], 2, |len| {
                Ok(vec![0; len])
            });
            assert!(result.is_err());
        }
    }

    #[test]
    fn layer_stacks_ft_validates_block_before_weight_allocation() {
        let mut allocated = false;
        let result = read_layer_stacks_ft_i16(
            &mut Cursor::new(vec![0; LEB128_MAGIC.len() + 4]),
            &mut [0; 2],
            2,
            |len| {
                allocated = true;
                Ok(vec![0; len])
            },
        );
        assert!(result.is_err());
        assert!(!allocated);
    }

    #[test]
    fn layer_stacks_ft_validates_values_before_weight_allocation() {
        let combined_too_short = compressed_block(&[1, 2, 3]);
        let mut split_too_long = compressed_block(&[1, 2]);
        split_too_long.extend_from_slice(&compressed_block(&[3, 4, 5]));
        let mut split_too_short = compressed_block(&[1, 2]);
        split_too_short.extend_from_slice(&compressed_block(&[3]));
        for bytes in [combined_too_short, split_too_long, split_too_short] {
            let mut allocated = false;
            let result = read_layer_stacks_ft_i16(&mut Cursor::new(bytes), &mut [0; 2], 2, |len| {
                allocated = true;
                Ok(vec![0; len])
            });
            assert!(result.is_err());
            assert!(!allocated);
        }
    }

    #[test]
    fn layer_stacks_ft_propagates_weight_allocation_failure() {
        let error = read_layer_stacks_ft_i16(
            &mut &compressed_block(&[1, 2])[..],
            &mut [0; 1],
            1,
            |_| -> io::Result<Vec<i16>> { Err(io::ErrorKind::OutOfMemory.into()) },
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::OutOfMemory);
    }

    #[test]
    fn compressed_block_reads_fragmented_payload_without_seek() {
        struct Fragmented<'a> {
            bytes: &'a [u8],
            interrupt: bool,
        }
        impl Read for Fragmented<'_> {
            fn read(&mut self, destination: &mut [u8]) -> io::Result<usize> {
                self.interrupt = !self.interrupt;
                if self.interrupt {
                    return Err(io::ErrorKind::Interrupted.into());
                }
                let len = destination.len().min(3);
                self.bytes.read(&mut destination[..len])
            }
        }
        let values = [i16::MIN as i64, i16::MAX as i64, 32_768, -32_769, 0, -1];
        let mut encoded = Vec::new();
        for value in values {
            encode_signed_leb128(value, &mut encoded);
        }
        encoded.extend_from_slice(&[0x80; 9]);
        encoded.push(0x00);
        let mut bytes = LEB128_MAGIC.to_vec();
        bytes.extend_from_slice(&(encoded.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&encoded);
        let mut reader = Fragmented {
            bytes: &bytes,
            interrupt: false,
        };
        let mut biases = [0; 2];
        let weights =
            read_layer_stacks_ft_i16(&mut reader, &mut biases, 5, |len| Ok(vec![0; len])).unwrap();
        assert_eq!(biases, [i16::MIN, i16::MAX]);
        assert_eq!(weights, [i16::MIN, i16::MAX, 0, -1, 0]);
        assert!(reader.bytes.is_empty());
    }

    #[test]
    fn compressed_block_rejects_short_payload_and_impossible_encoded_count() {
        let mut bytes = LEB128_MAGIC.to_vec();
        bytes.extend_from_slice(&(16 * 1024 * 1024u32).to_le_bytes());
        bytes.extend_from_slice(&[1, 2, 3]);
        let error = read_compressed_i16_block(&mut &bytes[..], usize::MAX).err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
        let error = read_compressed_i16_block(&mut &bytes[..], 3).err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
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

    #[test]
    fn signed_leb128_encode_decode_round_trip_uses_shortest_form() {
        for value in [
            i16::MIN as i64,
            -129,
            -128,
            -65,
            -64,
            -1,
            0,
            1,
            63,
            64,
            127,
            128,
            i16::MAX as i64,
        ] {
            let mut encoded = Vec::new();
            encode_signed_leb128(value, &mut encoded);
            let (decoded, consumed) = decode_single_leb128(&encoded).expect("decode");
            assert_eq!(decoded, value);
            assert_eq!(consumed, encoded.len());
        }
        assert_eq!(encode(-64), [0x40]);
        assert_eq!(encode(63), [0x3f]);
        assert_eq!(encode(64), [0xc0, 0x00]);
        assert_eq!(encode(-65), [0xbf, 0x7f]);

        fn encode(value: i64) -> Vec<u8> {
            let mut bytes = Vec::new();
            encode_signed_leb128(value, &mut bytes);
            bytes
        }
    }
}
