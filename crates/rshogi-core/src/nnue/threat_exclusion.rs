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
//! | 3  | `threat-profile-step-attacker` | occupancy 依存 slider (香角飛馬竜) を attacker から除外 (`ac==1 \|\| ac>=5`)、単発利き駒 (歩桂銀金) のみ attacker | 33,408 |
//! | 10 | `threat-profile-cross-side` | 同 side (`as==ds`) と同種 (`ac==dc`) を除外、敵味方跨ぎの異種 pair のみ残す | 96,320 |
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
//! ## arch_str
//!
//! profile 0 (default) のモデル:
//! - `Threat=216720`
//!
//! profile 1 以上のモデル:
//! - `Threat=<dims>,ThreatProfile=<id>`
//!
//! ## quantised.bin block order (Threat 部分)
//!
//! ```text
//! FT biases  (i16, LEB128)
//! FT weights (i16, LEB128)
//! [PSQT biases + weights (i32, raw)]        ← PSQT ありの場合のみ
//! [Threat profile id (u32, LE raw)]         ← ThreatProfile= が arch_str にある場合のみ
//! [Threat weights (i8, raw)]                ← Threat= が arch_str にある場合のみ
//! LayerStack per-bucket data
//! ```
//!
//! Threat weights layout: `i8[THREAT_DIMENSIONS × L1]` (feature-major、
//! 各特徴量の L1 i8 重みが連続)。
//!
//! ## profile id
//!
//! profile 0 (default) のモデルでは `ThreatProfile=` も profile id バイトも書かない。
//! profile 1 以上のモデルのみ、arch_str に `ThreatProfile=N` を含め、
//! Threat weights の直前に `profile_id` (u32, little-endian) を書き込む。
//!
//! 読み込み側の判定:
//! 1. arch_str に `ThreatProfile=` がある → u32 を読み、engine の profile id と照合
//! 2. arch_str に `ThreatProfile=` がない → 旧モデル (profile 0)。engine が profile 0 でなければエラー
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
        + cfg!(feature = "threat-profile-same-class-major-pawn") as usize
        + cfg!(feature = "threat-profile-step-attacker") as usize
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
    } else if cfg!(feature = "threat-profile-step-attacker") {
        3
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
/// - `as_`: attacker side (0=friend, 1=enemy) — `threat-profile-cross-side` のみ使用
/// - `ac`: attacker class index (0..8, ThreatClass の discriminant)
/// - `ds`: attacked side (0=friend, 1=enemy) — `threat-profile-cross-side` のみ使用
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

    // 同 side ペアと同種ペアを除外し、敵味方跨ぎの異種 pair のみ残す (profile 10)
    if cfg!(feature = "threat-profile-cross-side") && (as_ == ds || ac == dc) {
        return true;
    }

    // occupancy 依存 slider (Lance=1, Bishop=5, Rook=6, Horse=7, Dragon=8) を attacker
    // から除外し、単発利き駒 (Pawn/Knight/Silver/GoldLike) のみ attacker に残す (profile 3)
    if cfg!(feature = "threat-profile-step-attacker") && (ac == 1 || ac >= 5) {
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
            feature = "threat-profile-step-attacker",
            feature = "threat-profile-cross-side",
        )))]
        assert_eq!(THREAT_PROFILE_ID, 0);
    }

    #[test]
    fn test_is_excluded_profile_0() {
        #[cfg(not(any(
            feature = "threat-profile-same-class",
            feature = "threat-profile-same-class-major-pawn",
            feature = "threat-profile-step-attacker",
            feature = "threat-profile-cross-side",
        )))]
        {
            assert!(!is_excluded(0, 0, 0, 0));
            assert!(!is_excluded(0, 8, 1, 8));
            assert!(!is_excluded(0, 5, 0, 0));
        }
    }

    #[test]
    fn test_block_step_attacker() {
        #[cfg(feature = "threat-profile-step-attacker")]
        {
            assert_eq!(THREAT_PROFILE_ID, 3);
            // slider attacker (Lance=1, Bishop=5..Dragon=8) は除外
            assert!(is_excluded(0, 1, 0, 0)); // Lance attacker
            assert!(is_excluded(0, 8, 1, 0)); // Dragon attacker
            assert!(is_excluded(0, 6, 0, 3)); // Rook attacker
            // 単発利き駒 (Pawn=0, Knight=2, Silver=3, GoldLike=4) は残す (attacked 非依存)
            assert!(!is_excluded(0, 0, 0, 0)); // Pawn attacker
            assert!(!is_excluded(0, 2, 0, 6)); // Knight attacker → Rook attacked
            assert!(!is_excluded(0, 4, 1, 8)); // GoldLike attacker → Dragon attacked
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

    #[test]
    fn test_block_cross_side() {
        #[cfg(feature = "threat-profile-cross-side")]
        {
            assert_eq!(THREAT_PROFILE_ID, 10);
            assert!(is_excluded(0, 3, 0, 5)); // 同 side (friend→friend) → 除外
            assert!(is_excluded(1, 3, 1, 5)); // 同 side (enemy→enemy) → 除外
            assert!(is_excluded(0, 4, 1, 4)); // 同種 (ac==dc) → 除外
            assert!(!is_excluded(0, 3, 1, 5)); // cross-side 異種 (friend→enemy) → 残す
            assert!(!is_excluded(1, 5, 0, 3)); // cross-side 異種 (enemy→friend) → 残す
        }
    }
}
