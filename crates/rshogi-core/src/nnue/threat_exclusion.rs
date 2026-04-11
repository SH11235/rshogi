//! Threat pair 除外 profile
//!
//! Cargo feature flag で選択された profile に基づき、除外する pair を定義する。
//! rshogi と bullet-shogi で同一の除外ロジックを維持すること。
//!
//! ## Profile 一覧
//!
//! | id | feature flag | 除外内容 |
//! |----|-------------|---------|
//! | 0 | (default) | なし (Baseline) |
//! | 1 | `threat-profile-same-class` | 同種ペア全除外 |
//! | 2 | `threat-profile-same-class-major-pawn` | 同種 + 大駒→歩除外 |
//! | 10 | `threat-profile-cross-side` | cross-side 異種ペアのみ |
//!
//! ## 制約: profile は STM/NSTM 対称であること
//!
//! bullet-shogi の `SparseInputType::map_features` は `f(stm_idx, nstm_idx)` で
//! STM/NSTM ペアを同時に列挙する設計のため、STM perspective で active な pair は
//! NSTM perspective でも active である必要がある。
//! enemy→friend のみ (enemy-only) のような非対称 profile は学習に使えない。
//!
//! 仕様: `docs/threat_spec.md` Exclusion profiles セクション

// 相互排他チェック: 複数 profile を同時選択すると compile error
const _PROFILE_EXCLUSIVITY: () = {
    let count = cfg!(feature = "threat-profile-same-class") as usize
        + cfg!(feature = "threat-profile-same-class-major-pawn") as usize
        + cfg!(feature = "threat-profile-cross-side") as usize;
    assert!(count <= 1, "Multiple threat profiles selected. Choose at most one.");
};

/// Threat profile ID
///
/// quantised.bin に書き込まれる profile 識別子。
/// engine と model の profile が一致しなければ読み込みエラー。
pub const THREAT_PROFILE_ID: u32 = {
    if cfg!(feature = "threat-profile-cross-side") {
        10
    } else if cfg!(feature = "threat-profile-same-class-major-pawn") {
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
/// - `as_`: attacker side (0=friend, 1=enemy)
/// - `ac`: attacker class index (0..8, ThreatClass の discriminant)
/// - `ds`: attacked side (0=friend, 1=enemy)
/// - `dc`: attacked class index (0..8)
///
/// # ThreatClass index
/// 0=Pawn, 1=Lance, 2=Knight, 3=Silver, 4=GoldLike,
/// 5=Bishop, 6=Rook, 7=Horse, 8=Dragon
pub const fn is_excluded(as_: usize, ac: usize, ds: usize, dc: usize) -> bool {
    // Cross-side profile: cross-side 異種ペアのみ含める
    // same-side (味方→味方, 敵→敵) または同種ペアは全て除外
    if cfg!(feature = "threat-profile-cross-side") {
        return as_ == ds || ac == dc;
    }

    // 同種ペア全除外 (profile 1+)
    if cfg!(any(
        feature = "threat-profile-same-class",
        feature = "threat-profile-same-class-major-pawn",
    )) && ac == dc
    {
        return true;
    }

    // 大駒 attacker → Pawn attacked (profile 2)
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
            feature = "threat-profile-cross-side",
        )))]
        assert_eq!(THREAT_PROFILE_ID, 0);
    }

    #[test]
    fn test_is_excluded_profile_0() {
        #[cfg(not(any(
            feature = "threat-profile-same-class",
            feature = "threat-profile-same-class-major-pawn",
            feature = "threat-profile-cross-side",
        )))]
        {
            assert!(!is_excluded(0, 0, 0, 0));
            assert!(!is_excluded(0, 8, 1, 8));
            assert!(!is_excluded(0, 5, 0, 0));
        }
    }

    #[test]
    fn test_cross_side_profile() {
        #[cfg(feature = "threat-profile-cross-side")]
        {
            // cross-side 異種: 含める
            assert!(!is_excluded(1, 2, 0, 5)); // enemy→friend 異種
            assert!(!is_excluded(0, 0, 1, 5)); // friend→enemy 異種
            // same-side: 除外
            assert!(is_excluded(0, 2, 0, 5)); // friend→friend
            assert!(is_excluded(1, 2, 1, 5)); // enemy→enemy
            // same-class (cross-side でも): 除外
            assert!(is_excluded(1, 5, 0, 5)); // enemy_Bishop → friend_Bishop
        }
    }

    #[test]
    fn test_block_a_same_class() {
        #[cfg(feature = "threat-profile-same-class")]
        {
            assert!(is_excluded(0, 0, 0, 0));
            assert!(is_excluded(0, 8, 0, 8));
            assert!(!is_excluded(0, 0, 0, 1));
        }
    }

    #[test]
    fn test_block_c_major_to_pawn() {
        #[cfg(feature = "threat-profile-same-class-major-pawn")]
        {
            assert!(is_excluded(0, 0, 0, 0)); // same-class
            assert!(is_excluded(0, 5, 0, 0)); // Bishop→Pawn
            assert!(!is_excluded(0, 3, 0, 0)); // Silver→Pawn (not major)
        }
    }
}
