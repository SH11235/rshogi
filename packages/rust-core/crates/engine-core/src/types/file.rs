//! 筋（File）

/// 筋（1筋〜9筋）
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum File {
    File1 = 0,
    File2 = 1,
    File3 = 2,
    File4 = 3,
    File5 = 4,
    File6 = 5,
    File7 = 6,
    File8 = 7,
    File9 = 8,
}

impl File {
    /// 筋の数
    pub const NUM: usize = 9;

    /// 全ての筋
    pub const ALL: [File; 9] = [
        File::File1,
        File::File2,
        File::File3,
        File::File4,
        File::File5,
        File::File6,
        File::File7,
        File::File8,
        File::File9,
    ];

    /// u8からFileに変換（0-8）
    #[inline]
    pub const fn from_u8(n: u8) -> Option<File> {
        if n < 9 {
            // SAFETY: n < 9 なので有効なFile値
            Some(unsafe { std::mem::transmute::<u8, File>(n) })
        } else {
            None
        }
    }

    /// インデックスとして使用
    #[inline]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// USI形式の文字（'1'-'9'）に変換
    #[inline]
    pub const fn to_usi_char(self) -> char {
        (b'1' + self as u8) as char
    }

    /// USI形式の文字からFileに変換
    #[inline]
    pub const fn from_usi_char(c: char) -> Option<File> {
        let n = (c as u8).wrapping_sub(b'1');
        File::from_u8(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_file_from_u8() {
        assert_eq!(File::from_u8(0), Some(File::File1));
        assert_eq!(File::from_u8(8), Some(File::File9));
        assert_eq!(File::from_u8(9), None);
    }

    #[test]
    fn test_file_index() {
        assert_eq!(File::File1.index(), 0);
        assert_eq!(File::File9.index(), 8);
    }

    #[test]
    fn test_file_usi() {
        assert_eq!(File::File1.to_usi_char(), '1');
        assert_eq!(File::File9.to_usi_char(), '9');
        assert_eq!(File::from_usi_char('1'), Some(File::File1));
        assert_eq!(File::from_usi_char('9'), Some(File::File9));
        assert_eq!(File::from_usi_char('0'), None);
    }
}
