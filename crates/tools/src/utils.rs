//! ユーティリティ関数

/// 探索スレッドのスタックサイズ（64MB）
/// engine-usiと同じ値を使用
pub const SEARCH_STACK_SIZE: usize = 64 * 1024 * 1024;

/// 数値を3桁区切りでフォーマット
pub fn format_number(n: u64) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let mut result = String::new();

    for (i, &b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i).is_multiple_of(3) {
            result.push(',');
        }
        result.push(b as char);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_number() {
        assert_eq!(format_number(1234567), "1,234,567");
        assert_eq!(format_number(123), "123");
        assert_eq!(format_number(0), "0");
    }
}
