//! 38 byte HCPE レコードの little-endian codec。局面・指し手・勝敗の妥当性は呼び出し側で検証する。

/// HCPE 1 レコードのバイト長。
pub const HCPE_RECORD_SIZE: usize = 38;

/// cshogi `HuffmanCodedPosAndEval` のフィールドを byte を失わず保持する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HcpeRecord {
    /// Huffman 符号化された局面。
    pub hcp: [u8; 32],
    /// 手番側視点の符号付き評価値。
    pub eval: i16,
    /// cshogi move16 の生ビット列。0 は指し手なし。
    pub best_move16: u16,
    /// 生の勝敗 byte。0=引分、1=先手勝ち、2=後手勝ち。
    pub game_result: u8,
    /// 未使用の dummy byte。新規生成時は 0、既存レコードの更新時は保持する。
    pub padding: u8,
}

impl HcpeRecord {
    /// 固定長の HCPE byte 列を読み取る。値の正規化・意味検証は行わない。
    pub fn from_bytes(bytes: &[u8; HCPE_RECORD_SIZE]) -> Self {
        let mut hcp = [0; 32];
        hcp.copy_from_slice(&bytes[..32]);
        Self {
            hcp,
            eval: i16::from_le_bytes([bytes[32], bytes[33]]),
            best_move16: u16::from_le_bytes([bytes[34], bytes[35]]),
            game_result: bytes[36],
            padding: bytes[37],
        }
    }

    /// padding を含む全フィールドを固定長 HCPE byte 列へ書き出す。
    pub fn to_bytes(self) -> [u8; HCPE_RECORD_SIZE] {
        let mut bytes = [0; HCPE_RECORD_SIZE];
        bytes[..32].copy_from_slice(&self.hcp);
        bytes[32..34].copy_from_slice(&self.eval.to_le_bytes());
        bytes[34..36].copy_from_slice(&self.best_move16.to_le_bytes());
        bytes[36] = self.game_result;
        bytes[37] = self.padding;
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOUNDARIES: &[u8; 114] = include_bytes!("../tests/fixtures/hcpe-layout-boundaries.hcpe");

    #[test]
    fn fixed_external_layout_decodes_and_encodes_exact_bytes() {
        let expected = [
            HcpeRecord {
                hcp: std::array::from_fn(|i| i as u8),
                eval: i16::MIN,
                best_move16: 0x1234,
                game_result: 0,
                padding: 0xa5,
            },
            HcpeRecord {
                hcp: std::array::from_fn(|i| 31 - i as u8),
                eval: i16::MAX,
                best_move16: 0xffff,
                game_result: 2,
                padding: 0x5a,
            },
            HcpeRecord {
                hcp: [0xff; 32],
                eval: -1,
                best_move16: 0,
                game_result: 1,
                padding: 0xff,
            },
        ];
        for (bytes, expected) in BOUNDARIES.as_chunks::<HCPE_RECORD_SIZE>().0.iter().zip(expected) {
            assert_eq!(HcpeRecord::from_bytes(bytes), expected);
            assert_eq!(expected.to_bytes().as_slice(), bytes);
        }
    }

    #[test]
    fn eval_update_preserves_all_other_bytes() {
        for bytes in BOUNDARIES.as_chunks::<HCPE_RECORD_SIZE>().0 {
            let mut record = HcpeRecord::from_bytes(bytes);
            record.eval = -1234;
            let updated = record.to_bytes();
            assert_eq!(&updated[..32], &bytes[..32]);
            assert_eq!(&updated[32..34], &[0x2e, 0xfb]);
            assert_eq!(&updated[34..], &bytes[34..]);
        }
    }

    #[test]
    fn cshogi_oracle_records_preserve_every_byte() {
        let oracle = include_bytes!("../tests/fixtures/psv_to_hcpe3_yaneuraou_sample.hcpe");
        assert_eq!(oracle.len() % HCPE_RECORD_SIZE, 0);
        for bytes in oracle.as_chunks::<HCPE_RECORD_SIZE>().0 {
            assert_eq!(HcpeRecord::from_bytes(bytes).to_bytes().as_slice(), bytes);
        }
    }
}
