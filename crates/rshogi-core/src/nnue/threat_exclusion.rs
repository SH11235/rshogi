//! Threat pair 除外 profile
//!
//! pair 単位で Threat 特徴量を除外する仕組み。除外する pair セットを Cargo
//! feature flag でビルド時に選択する。除外された pair は index 空間からも
//! 重み行列からも物理的に削除され、推論オーバーヘッドはゼロ
//! (`pair_base` テーブルの内容が変わるだけ)。
//!
//! # クロスリポジトリ契約
//!
//! 除外集合と profile id は学習側 (bullet-shogi) と推論側 (rshogi) で**完全に
//! 一致**させる必要がある。手動同期に加え、Golden Forward cross-validation
//! テスト ([`verify-nnue` skill]) で実装一致を検証している。
//!
//! [`verify-nnue` skill]: https://github.com/SH11235/rshogi/tree/main/.claude/skills/verify-nnue
//!
//! # Profile 一覧 (実装済み)
//!
//! | id | feature flag | 除外内容 | THREAT_DIMENSIONS |
//! |----|-------------|---------|------------------:|
//! | 0  | (default) | なし (Baseline) | 216,720 |
//! | 1  | `threat-profile-same-class` | 同種ペア全除外 (9 クラス × 4 side = 36 pair) | 192,640 |
//! | 2  | `threat-profile-same-class-major-pawn` | profile 1 + 大駒 attacker → Pawn attacked (4 大駒 × 4 side = 16 pair 追加) | 173,568 |
//!
//! # 設計: pair_base の sentinel
//!
//! 除外 pair は `pair_base[i]` に sentinel `EXCLUDED_PAIR_BASE` を入れて
//! 累積和計算時にスキップする。`pair_base` の連続性を維持する代わりに次元を
//! 詰める方式で、index 計算側 ([`super::threat_features::pair_base`]) は
//! sentinel をチェックして `None` を返す。
//!
//! 各 profile の `is_excluded` 関数で除外条件を const 評価し、
//! `build_pair_base` でテーブルを生成する。
//!
//! # arch_str / quantised.bin file format
//!
//! profile 0 (default) のモデル:
//! - arch_str: `Threat=216720`
//! - file: Threat weights を直接書く
//!
//! profile 1 以上のモデル:
//! - arch_str: `Threat=<dims>,ThreatProfile=<id>`
//! - file: Threat weights の直前に `profile_id` (u32 LE) を書く
//! - 読み込み時に engine の profile id と照合し、不一致ならエラー
//!
//! # 制約: profile は STM/NSTM 対称であること
//!
//! bullet-shogi の `SparseInputType::map_features` は `f(stm_idx, nstm_idx)` で
//! STM/NSTM ペアを同時に列挙する設計のため、STM perspective で active な pair は
//! NSTM perspective でも active である必要がある。
//! enemy→friend のみ (enemy-only) のような非対称 profile は学習に使えない。

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
/// - `as_`: attacker side (0=friend, 1=enemy) — 現在未使用 (reserved)
/// - `ac`: attacker class index (0..8, ThreatClass の discriminant)
/// - `ds`: attacked side (0=friend, 1=enemy) — 現在未使用 (reserved)
/// - `dc`: attacked class index (0..8)
///
/// # ThreatClass index
/// 0=Pawn, 1=Lance, 2=Knight, 3=Silver, 4=GoldLike,
/// 5=Bishop, 6=Rook, 7=Horse, 8=Dragon
pub const fn is_excluded(as_: usize, ac: usize, ds: usize, dc: usize) -> bool {
    let _ = (as_, ds);
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
