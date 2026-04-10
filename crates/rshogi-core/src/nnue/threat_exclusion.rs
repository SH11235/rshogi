//! Threat pair 除外 profile
//!
//! Cargo feature flag で選択された profile に基づき、除外する pair を定義する。
//! rshogi と bullet-shogi で同一のロジックを手動同期すること。
//!
//! ## Profile 一覧
//!
//! | id | feature flag | 除外内容 |
//! |----|-------------|---------|
//! | 0 | (default) | なし (Baseline) |
//! | 1 | `threat-profile-same-class` | 同種ペア全除外 |
//! | 2 | `threat-profile-same-class-major-pawn` | 同種 + 大駒→歩除外 |
//!
//! 仕様: `docs/threat_spec.md` Exclusion profiles セクション

// 相互排他チェック: 複数 profile を同時選択すると compile error
const _PROFILE_EXCLUSIVITY: () = {
    let count = cfg!(feature = "threat-profile-same-class") as usize
        + cfg!(feature = "threat-profile-same-class-major-pawn") as usize;
    assert!(count <= 1, "Multiple threat profiles selected. Choose at most one.");
};

/// Threat profile ID
///
/// quantised.bin に書き込まれる profile 識別子。
/// engine と model の profile が一致しなければ読み込みエラー。
pub const THREAT_PROFILE_ID: u32 = {
    if cfg!(feature = "threat-profile-same-class-major-pawn") {
        2
    } else if cfg!(feature = "threat-profile-same-class") {
        1
    } else {
        0
    }
};

/// pair を除外すべきかどうか判定する
///
/// `build_pair_base` (const fn) から呼ばれるため、引数は usize。
///
/// # 引数
/// - `_as`: attacker side (0=friend, 1=enemy)
/// - `ac`: attacker class index (0..8, ThreatClass の discriminant)
/// - `_ds`: attacked side (0=friend, 1=enemy)
/// - `dc`: attacked class index (0..8)
///
/// # ThreatClass index
/// 0=Pawn, 1=Lance, 2=Knight, 3=Silver, 4=GoldLike,
/// 5=Bishop, 6=Rook, 7=Horse, 8=Dragon
pub const fn is_excluded(_as: usize, ac: usize, _ds: usize, dc: usize) -> bool {
    // Block A (profile >= 1): 同種ペア全除外
    // 除外条件: attacker_class == attacked_class (全 side 組合せ)
    if cfg!(any(
        feature = "threat-profile-same-class",
        feature = "threat-profile-same-class-major-pawn",
    )) && ac == dc
    {
        return true;
    }

    // Block C (profile >= 2): 大駒 attacker → Pawn attacked
    // 除外条件: ac ∈ {Bishop(5), Rook(6), Horse(7), Dragon(8)} && dc == Pawn(0)
    if cfg!(feature = "threat-profile-same-class-major-pawn") && ac >= 5 && dc == 0 {
        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_profile_id_default() {
        #[cfg(not(any(
            feature = "threat-profile-same-class",
            feature = "threat-profile-same-class-major-pawn",
        )))]
        assert_eq!(THREAT_PROFILE_ID, 0);
    }

    #[test]
    fn test_is_excluded_profile_0() {
        #[cfg(not(any(
            feature = "threat-profile-same-class",
            feature = "threat-profile-same-class-major-pawn",
        )))]
        {
            assert!(!is_excluded(0, 0, 0, 0));
            assert!(!is_excluded(0, 8, 1, 8));
            assert!(!is_excluded(0, 5, 0, 0));
        }
    }

    #[test]
    fn test_block_a_same_class() {
        #[cfg(feature = "threat-profile-same-class")]
        {
            assert!(is_excluded(0, 0, 0, 0));
            assert!(is_excluded(0, 0, 1, 0));
            assert!(is_excluded(1, 0, 0, 0));
            assert!(is_excluded(1, 0, 1, 0));
            assert!(is_excluded(0, 8, 0, 8));
            assert!(is_excluded(0, 4, 1, 4));
            assert!(!is_excluded(0, 0, 0, 1));
            assert!(!is_excluded(0, 5, 0, 0));
        }
    }

    #[test]
    fn test_block_c_major_to_pawn() {
        #[cfg(feature = "threat-profile-same-class-major-pawn")]
        {
            assert!(is_excluded(0, 0, 0, 0));
            assert!(is_excluded(0, 5, 0, 0));
            assert!(is_excluded(0, 6, 1, 0));
            assert!(is_excluded(1, 7, 0, 0));
            assert!(is_excluded(1, 8, 1, 0));
            assert!(!is_excluded(0, 5, 0, 1));
            assert!(!is_excluded(0, 3, 0, 0));
        }
    }
}
