//! build.rs の整合性チェック (`validate_feature_combination`) の単体テスト。
//!
//! 純粋関数は `crates/rshogi-core/build/checks.rs` に切り出されており、
//! ここでは `include!` で取り込んで `&dyn Fn(&str) -> bool` lookup を渡して呼ぶ。

include!("../build/checks.rs");

/// 与えられた feature 名集合を `has_feature` lookup に変換するヘルパー。
fn lookup(features: &[&str]) -> impl Fn(&str) -> bool {
    let owned: Vec<String> = features.iter().map(|s| (*s).to_string()).collect();
    move |name: &str| owned.iter().any(|f| f == name)
}

#[test]
fn empty_features_pass() {
    let has = lookup(&[]);
    assert!(validate_feature_combination(&has).is_ok());
}

#[test]
fn names_without_mode_sentinel_pass() {
    // mode-* sentinel を含まない build (canonical atomic feature の直接指定など) は
    // 組合せ妥当性を user 責任とし、validator は Ok を返す。
    let has = lookup(&[
        "ls-arch",
        "ls-size-1536x16x32",
        "ls-ext-psqt",
        "nnue-progress-diff",
    ]);
    assert!(validate_feature_combination(&has).is_ok());
}

#[test]
fn universal_alone_ok() {
    let has = lookup(&[
        "mode-universal",
        "ls-arch",
        "ls-size-1536x16x32",
        "ls-size-768x16x32",
        "ft-halfka_hm_merged",
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
        "ft-halfka_hm_merged",
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
        "ft-halfka_hm_merged",
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
        "ft-halfka_hm_merged",
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
        "ft-halfka_hm_merged",
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
        "ft-halfka_hm_merged",
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
        "ft-halfka_hm_merged",
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
        "ft-halfka_hm_merged",
        "nnue-progress-diff",
    ]);
    let err = validate_feature_combination(&has).unwrap_err();
    assert!(err.contains("nnue-progress-diff"));
}

#[test]
fn family_multiple_sizes_ok() {
    let has = lookup(&[
        "mode-family",
        "ls-arch",
        "ls-size-1536x16x32",
        "ls-size-768x16x32",
        "ls-size-512x16x32",
        "ft-halfka_hm_merged",
    ]);
    assert!(validate_feature_combination(&has).is_ok());
}

#[test]
fn ls_arch_plus_halfkx_arch_ok() {
    let has = lookup(&[
        "mode-universal",
        "ls-arch",
        "halfkx-arch",
        "ls-size-1536x16x32",
        "ft-halfka_hm_merged",
    ]);
    assert!(validate_feature_combination(&has).is_ok());
}

#[test]
fn ls_specific_with_ft_halfkp_ok() {
    let has = lookup(&[
        "mode-specific",
        "ls-arch",
        "ls-size-1536x16x32",
        "ft-halfkp",
    ]);
    assert!(validate_feature_combination(&has).is_ok());
}

#[test]
fn ls_specific_with_ft_halfka_split_ok() {
    let has = lookup(&[
        "mode-specific",
        "ls-arch",
        "ls-size-1536x16x32",
        "ft-halfka_split",
    ]);
    assert!(validate_feature_combination(&has).is_ok());
}

#[test]
fn ls_specific_with_ft_halfka_merged_ok() {
    let has = lookup(&[
        "mode-specific",
        "ls-arch",
        "ls-size-1536x16x32",
        "ft-halfka_merged",
    ]);
    assert!(validate_feature_combination(&has).is_ok());
}

#[test]
fn ls_specific_with_ft_halfka_hm_split_ok() {
    let has = lookup(&[
        "mode-specific",
        "ls-arch",
        "ls-size-1536x16x32",
        "ft-halfka_hm_split",
    ]);
    assert!(validate_feature_combination(&has).is_ok());
}

#[test]
fn ls_specific_with_ft_halfka_hm_merged_ok() {
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
fn ls_arch_with_halfkx_arch_specific_ft_halfkp_ok() {
    // mode-specific + ls-arch + halfkx-arch + ft-halfkp + ls-size-* は許容。
    let has = lookup(&[
        "mode-specific",
        "ls-arch",
        "halfkx-arch",
        "ls-size-512x16x32",
        "ft-halfkp",
        "halfkx-activation-crelu",
    ]);
    assert!(validate_feature_combination(&has).is_ok());
}

#[test]
fn ls_only_family_with_multi_ft_ok() {
    let has = lookup(&[
        "mode-family",
        "ls-arch",
        "ls-size-1536x16x32",
        "ls-size-768x16x32",
        "ft-halfkp",
        "ft-halfka_hm_merged",
    ]);
    assert!(validate_feature_combination(&has).is_ok());
}

#[test]
fn ls_only_universal_with_all_ft_ok() {
    let has = lookup(&[
        "mode-universal",
        "ls-arch",
        "ls-size-1536x16x32",
        "ft-halfkp",
        "ft-halfka_split",
        "ft-halfka_merged",
        "ft-halfka_hm_split",
        "ft-halfka_hm_merged",
    ]);
    assert!(validate_feature_combination(&has).is_ok());
}

#[test]
fn ls_arch_without_ft_rejected() {
    let has = lookup(&["mode-specific", "ls-arch", "ls-size-1536x16x32"]);
    let err = validate_feature_combination(&has).unwrap_err();
    assert!(err.contains("ft-* を 1 個以上"));
}

#[test]
fn halfkx_specific_with_ft_halfkp_ok() {
    // HalfKX 単独 (ls-arch なし) では ft-halfkp は valid。
    let has = lookup(&[
        "mode-specific",
        "halfkx-arch",
        "ft-halfkp",
        "halfkx-activation-crelu",
    ]);
    assert!(validate_feature_combination(&has).is_ok());
}

#[test]
fn family_with_multiple_ft_ok() {
    // family mode では複数 FT 同時 OK (dispatch)。
    let has = lookup(&[
        "mode-family",
        "halfkx-arch",
        "ft-halfkp",
        "ft-halfka_hm_merged",
        "halfkx-activation-crelu",
        "halfkx-activation-screlu",
    ]);
    assert!(validate_feature_combination(&has).is_ok());
}
