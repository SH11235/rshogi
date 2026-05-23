//! build.rs の整合性チェック (`validate_feature_combination`) を単体テストする。
//!
//! 純粋関数は `crates/rshogi-core/build/checks.rs` に切り出されており、
//! ここでは `include!` で取り込んで run-time の `&dyn Fn(&str) -> bool` lookup を
//! 渡しテストする。実際の build script panic 発火確認は cargo build 経由の
//! shell smoke test で行うが、ロジックの単体検証はここで完結する。

include!("../build/checks.rs");

/// 与えられた feature 名集合を `has_feature` lookup に変換するヘルパー。
fn lookup(features: &[&str]) -> impl Fn(&str) -> bool {
    let owned: Vec<String> = features.iter().map(|s| (*s).to_string()).collect();
    move |name: &str| owned.iter().any(|f| f == name)
}

#[test]
fn phase1_compat_no_mode_passes() {
    // Phase 1 互換: 旧 atomic feature 直指定 (mode-* なし) は通る。
    let has = lookup(&[
        "layerstack-only",
        "layerstacks-1536x16x32",
        "nnue-psqt",
        "nnue-progress-diff",
    ]);
    assert!(validate_feature_combination(&has).is_ok());
}

#[test]
fn empty_features_pass() {
    let has = lookup(&[]);
    assert!(validate_feature_combination(&has).is_ok());
}

#[test]
fn universal_alone_ok() {
    let has = lookup(&[
        "mode-universal",
        "ls-arch",
        "ls-size-1536x16x32",
        "ls-size-768x16x32",
    ]);
    assert!(validate_feature_combination(&has).is_ok());
}

#[test]
fn universal_plus_family_rejected() {
    let has = lookup(&["mode-universal", "mode-family"]);
    let err = validate_feature_combination(&has).unwrap_err();
    assert!(err.contains("edition-universal"));
}

#[test]
fn universal_plus_specific_rejected() {
    let has = lookup(&["mode-universal", "mode-specific", "ls-size-1536x16x32"]);
    let err = validate_feature_combination(&has).unwrap_err();
    assert!(err.contains("edition-universal"));
}

#[test]
fn family_plus_specific_rejected() {
    let has = lookup(&["mode-family", "mode-specific", "ls-size-1536x16x32"]);
    let err = validate_feature_combination(&has).unwrap_err();
    // mode-universal は含まれていないので「ちょうど 1 個」エラーに落ちる。
    assert!(err.contains("must be exactly 1"));
}

#[test]
fn ls_arch_without_size_rejected() {
    let has = lookup(&["mode-family", "ls-arch"]);
    let err = validate_feature_combination(&has).unwrap_err();
    assert!(err.contains("ls-size-* を 1 個以上"));
}

#[test]
fn specific_multiple_sizes_rejected() {
    let has = lookup(&[
        "mode-specific",
        "ls-arch",
        "ls-size-1536x16x32",
        "ls-size-1536x32x32",
    ]);
    let err = validate_feature_combination(&has).unwrap_err();
    assert!(err.contains("ls-size-* を 1 個だけ"));
}

#[test]
fn specific_multiple_activations_rejected() {
    let has = lookup(&[
        "mode-specific",
        "halfkx-arch",
        "halfkx-activation-crelu",
        "halfkx-activation-screlu",
    ]);
    let err = validate_feature_combination(&has).unwrap_err();
    assert!(err.contains("halfkx-activation-*"));
}

#[test]
fn specific_multiple_ft_rejected() {
    let has = lookup(&[
        "mode-specific",
        "halfkx-arch",
        "ft-halfkp",
        "ft-halfka_hm_merged",
    ]);
    let err = validate_feature_combination(&has).unwrap_err();
    assert!(err.contains("ft-* を 1 個まで"));
}

#[test]
fn specific_single_size_ok() {
    let has = lookup(&[
        "mode-specific",
        "ls-arch",
        "ls-size-1536x16x32",
        "ls-ext-psqt",
        "nnue-progress-diff",
    ]);
    assert!(validate_feature_combination(&has).is_ok());
}

#[test]
fn progress_diff_with_512_rejected() {
    let has = lookup(&[
        "mode-specific",
        "ls-arch",
        "ls-size-512x16x32",
        "nnue-progress-diff",
    ]);
    let err = validate_feature_combination(&has).unwrap_err();
    assert!(err.contains("nnue-progress-diff"));
}

#[test]
fn progress_diff_with_768_rejected() {
    let has = lookup(&[
        "mode-specific",
        "ls-arch",
        "ls-size-768x16x32",
        "nnue-progress-diff",
    ]);
    let err = validate_feature_combination(&has).unwrap_err();
    assert!(err.contains("nnue-progress-diff"));
}

#[test]
fn progress_diff_with_1536x32x32_ok() {
    let has = lookup(&[
        "mode-specific",
        "ls-arch",
        "ls-size-1536x32x32",
        "nnue-progress-diff",
    ]);
    assert!(validate_feature_combination(&has).is_ok());
}

#[test]
fn progress_diff_in_family_rejected() {
    let has = lookup(&[
        "mode-family",
        "ls-arch",
        "ls-size-1536x16x32",
        "nnue-progress-diff",
    ]);
    let err = validate_feature_combination(&has).unwrap_err();
    assert!(err.contains("nnue-progress-diff"));
}

#[test]
fn progress_diff_in_universal_rejected() {
    let has = lookup(&[
        "mode-universal",
        "ls-arch",
        "ls-size-1536x16x32",
        "nnue-progress-diff",
    ]);
    let err = validate_feature_combination(&has).unwrap_err();
    assert!(err.contains("nnue-progress-diff"));
}

#[test]
fn family_multiple_sizes_ok() {
    // family mode では複数 size 同時 OK (dispatch する用途)。
    let has = lookup(&[
        "mode-family",
        "ls-arch",
        "ls-size-1536x16x32",
        "ls-size-768x16x32",
        "ls-size-512x16x32",
    ]);
    assert!(validate_feature_combination(&has).is_ok());
}

#[test]
fn ls_arch_plus_halfkx_arch_rejected_phase1() {
    // Phase 1: ls-arch は旧 layerstack-only 意味論 (HalfKX 経路除去) を保持しているため
    // halfkx-arch との同時指定は不整合。Phase 2 で意味論再定義後に解禁予定。
    let has = lookup(&[
        "mode-universal",
        "ls-arch",
        "halfkx-arch",
        "ls-size-1536x16x32",
    ]);
    let err = validate_feature_combination(&has).unwrap_err();
    assert!(err.contains("ls-arch") && err.contains("halfkx-arch"));
}

#[test]
fn ls_arch_with_ft_halfkp_rejected() {
    // ADR「LS は halfka_hm_merged だけ通る」: LS で他 FT を立てると reject。
    let has = lookup(&[
        "mode-specific",
        "ls-arch",
        "ls-size-1536x16x32",
        "ft-halfkp",
    ]);
    let err = validate_feature_combination(&has).unwrap_err();
    assert!(err.contains("ft-halfka_hm_merged のみ"));
}

#[test]
fn ls_arch_with_ft_halfka_split_rejected() {
    let has = lookup(&[
        "mode-specific",
        "ls-arch",
        "ls-size-1536x16x32",
        "ft-halfka_split",
    ]);
    let err = validate_feature_combination(&has).unwrap_err();
    assert!(err.contains("ft-halfka_hm_merged のみ"));
}

#[test]
fn ls_arch_with_ft_halfka_hm_merged_ok() {
    // LS + 唯一サポートされている FT variant の組合せは pass。
    let has = lookup(&[
        "mode-specific",
        "ls-arch",
        "ls-size-1536x16x32",
        "ft-halfka_hm_merged",
        "nnue-progress-diff",
    ]);
    assert!(validate_feature_combination(&has).is_ok());
}

#[test]
fn halfkx_arch_with_ft_halfkp_ok() {
    // HalfKX 側では ft-halfkp は valid (Phase 1 の LS 制約は ls-arch 限定)。
    let has = lookup(&[
        "mode-specific",
        "halfkx-arch",
        "ft-halfkp",
        "halfkx-activation-crelu",
    ]);
    assert!(validate_feature_combination(&has).is_ok());
}
