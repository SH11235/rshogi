//! Bitboard ユーティリティ関数

/// 64bit整数の最上位ビット位置を返す（MSB: Most Significant Bit）
///
/// # Arguments
/// * `x` - 入力値
///
/// # Returns
/// - `x != 0`: 最上位ビットの位置（0-63）
/// - `x == 0`: 0（未定義動作を避けるため）
///
/// # Examples
/// ```ignore
/// assert_eq!(msb64(0b1000), 3);
/// assert_eq!(msb64(0x8000_0000_0000_0000), 63);
/// assert_eq!(msb64(0), 0);
/// ```
#[inline(always)]
pub fn msb64(x: u64) -> u32 {
    if x == 0 {
        0
    } else {
        63 - x.leading_zeros()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_msb64_zero() {
        assert_eq!(msb64(0), 0);
    }

    #[test]
    fn test_msb64_single_bit() {
        assert_eq!(msb64(1), 0);
        assert_eq!(msb64(2), 1);
        assert_eq!(msb64(4), 2);
        assert_eq!(msb64(8), 3);
        assert_eq!(msb64(0x80), 7);
        assert_eq!(msb64(0x8000_0000_0000_0000), 63);
    }

    #[test]
    fn test_msb64_multiple_bits() {
        assert_eq!(msb64(0b1111), 3);
        assert_eq!(msb64(0xFF), 7);
        assert_eq!(msb64(0xFFFF_FFFF_FFFF_FFFF), 63);
    }

    #[test]
    fn test_msb64_power_of_two() {
        for i in 0..64 {
            assert_eq!(msb64(1u64 << i), i);
        }
    }
}
