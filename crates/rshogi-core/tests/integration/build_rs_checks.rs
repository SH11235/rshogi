//! build.rs の整合性チェック (`validate_feature_combination`) の単体テスト。
//!
//! 純粋関数は `crates/rshogi-core/build/checks.rs` に切り出されており、
//! ここでは `include!` で取り込んで `&dyn Fn(&str) -> bool` lookup を渡して呼ぶ。

include!("../../build/checks.rs");

#[test]
fn dense_concat_requires_a_single_static_halfka_hm_merged_slot() {
    for (dense_concat, slot) in [
        ("layerstacks-dense-concat-1536x16x64", "layerstacks-1536x15x64"),
        ("layerstacks-dense-concat-2304x16x64", "layerstacks-2304x15x64"),
    ] {
        assert_dense_concat_feature_combination(dense_concat, slot);
    }
}

fn assert_dense_concat_feature_combination(dense_concat: &str, slot: &str) {
    let base = [
        "mode-specific",
        "layerstack-arch",
        slot,
        dense_concat,
        "ft-halfka_hm_merged",
        "nnue-progress-diff",
    ];
    assert!(validate_feature_combination(&lookup(&base)).is_ok());
    let without_layerstack = base.map(|feature| {
        if feature == "layerstack-arch" {
            ""
        } else {
            feature
        }
    });
    let error = validate_feature_combination(&lookup(&without_layerstack)).unwrap_err();
    assert!(error.contains("DenseConcat"), "{error}");
    for width in ["threat-width-256", "threat-width-512", "threat-width-768"] {
        let threat = [base.as_slice(), &["nnue-threat", width]].concat();
        let result = validate_feature_combination(&lookup(&threat));
        if width == "threat-width-256" && slot == "layerstacks-2304x15x64" {
            assert!(result.unwrap_err().contains("threat-width-256"));
        } else {
            assert!(result.is_ok(), "{slot}, {width}: {result:?}");
        }
    }

    let wrong_slot = base.map(|feature| {
        if feature == slot {
            "layerstacks-1536x15x32"
        } else {
            feature
        }
    });
    let error = validate_feature_combination(&lookup(&wrong_slot)).unwrap_err();
    assert!(error.contains(slot), "{error}");

    for incompatible in [
        "nnue-runtime-dimensions",
        "layerstacks-1536x15x32",
        "ft-halfkp",
        "nnue-effect-bucket",
        "prepacked-nnue",
        "mode-universal",
        "mode-family",
    ] {
        let features = [base.as_slice(), &[incompatible]].concat();
        let error = validate_feature_combination(&lookup(&features)).unwrap_err();
        assert!(error.contains("DenseConcat"), "{incompatible}: {error}");
    }
}

#[test]
fn dense_concat_slots_are_mutually_exclusive() {
    let features = [
        "mode-specific",
        "layerstack-arch",
        "ft-halfka_hm_merged",
        "layerstacks-dense-concat-1536x16x64",
        "layerstacks-1536x15x64",
        "layerstacks-dense-concat-2304x16x64",
        "layerstacks-2304x15x64",
    ];
    let error = validate_feature_combination(&lookup(&features)).unwrap_err();
    assert!(error.contains("DenseConcat features are mutually exclusive"), "{error}");
}

#[test]
fn dense_concat_rejects_split_skip_editions_of_the_same_slot() {
    for (dense_concat, split_skip) in [
        (
            "edition-layerstacks-dense-concat-halfka_hm_merged-1536x16x64-none",
            "edition-layerstacks-halfka_hm_merged-1536x15x64-none",
        ),
        (
            "edition-layerstacks-dense-concat-halfka_hm_merged-2304x16x64-threat512",
            "edition-layerstacks-halfka_hm_merged-2304x15x64-threat512",
        ),
    ] {
        let slot = if dense_concat.contains("1536") {
            "1536x"
        } else {
            "2304x"
        };
        let features = [
            "mode-specific",
            "layerstack-arch",
            "ft-halfka_hm_merged",
            if slot == "1536x" {
                "layerstacks-dense-concat-1536x16x64"
            } else {
                "layerstacks-dense-concat-2304x16x64"
            },
            if slot == "1536x" {
                "layerstacks-1536x15x64"
            } else {
                "layerstacks-2304x15x64"
            },
            dense_concat,
            split_skip,
        ];
        let error = validate_feature_combination(&lookup(&features)).unwrap_err();
        assert!(error.contains(split_skip), "{error}");
    }
}

#[test]
fn split_skip_editions_sharing_dense_concat_slots_match_the_manifest() {
    let manifest = include_str!("../../Cargo.toml");
    let declared: Vec<&str> = manifest
        .lines()
        .filter_map(|line| line.split_once(" = ").map(|(name, _)| name.trim()))
        .filter(|name| {
            name.starts_with("edition-layerstacks-halfka_hm_merged-1536x15x64-")
                || name.starts_with("edition-layerstacks-halfka_hm_merged-2304x15x64-")
        })
        .collect();
    let mut expected = SPLIT_SKIP_EDITIONS_SHARING_DENSE_CONCAT_SLOTS.to_vec();
    let mut actual = declared;
    expected.sort_unstable();
    actual.sort_unstable();
    assert_eq!(actual, expected);
}

/// 与えられた feature 名集合を `has_feature` lookup に変換するヘルパー。
fn lookup(features: &[&str]) -> impl Fn(&str) -> bool {
    let owned: Vec<String> = features.iter().map(|s| (*s).to_string()).collect();
    move |name: &str| owned.iter().any(|f| f == name)
}

#[test]
fn partial_threat_width_requires_supported_static_size() {
    for size in LAYERSTACK_FEATURES {
        for mode in ["mode-specific", "mode-family", "mode-universal", ""] {
            for width in ["threat-width-256", "threat-width-512", "threat-width-768"] {
                let features = [
                    mode,
                    "layerstack-arch",
                    "ft-halfka_hm_merged",
                    size,
                    "nnue-threat",
                    width,
                ];
                let supported = mode == "mode-specific"
                    && match *size {
                        "layerstacks-1536x15x64" => true,
                        "layerstacks-1024x15x64" | "layerstacks-2304x15x64" => {
                            width != "threat-width-256"
                        }
                        "layerstacks-1536x15x32" => width == "threat-width-512",
                        _ => false,
                    };
                assert_eq!(
                    validate_feature_combination(&lookup(&features)).is_ok(),
                    supported,
                    "{mode}, {size}, {width}"
                );
            }
        }
    }
}

#[test]
fn partial_threat_widths_are_mutually_exclusive() {
    for (a, b) in [
        ("threat-width-256", "threat-width-512"),
        ("threat-width-256", "threat-width-768"),
        ("threat-width-512", "threat-width-768"),
    ] {
        let features = [
            "mode-specific",
            "layerstack-arch",
            "ft-halfka_hm_merged",
            "layerstacks-1536x15x64",
            "nnue-threat",
            a,
            b,
        ];
        assert!(
            validate_feature_combination(&lookup(&features))
                .unwrap_err()
                .contains("mutually exclusive")
        );
    }
}

#[test]
fn large_ft_slots_require_dense_concat() {
    for &(ft, dense_concat, slot) in DENSE_CONCAT_SLOTS {
        if ft <= 2304 {
            continue;
        }
        let base = [
            "mode-specific",
            "layerstack-arch",
            "ft-halfka_hm_merged",
            slot,
        ];
        let error = validate_feature_combination(&lookup(&base)).unwrap_err();
        assert!(error.contains("SplitSkip") && error.contains(dense_concat), "{error}");
        let with_head = [base.as_slice(), &[dense_concat, "nnue-progress-diff"]].concat();
        assert!(validate_feature_combination(&lookup(&with_head)).is_ok());
    }
}

#[test]
fn large_partial_threat_widths_require_their_dense_concat_edition() {
    for &(ft, dense_concat, slot) in DENSE_CONCAT_SLOTS {
        for (width, required_ft) in [("threat-width-2816", 8192), ("threat-width-3072", 6144)] {
            let base = [
                "mode-specific",
                "layerstack-arch",
                "ft-halfka_hm_merged",
                slot,
                "nnue-threat",
                width,
            ];
            assert!(validate_feature_combination(&lookup(&base)).is_err());
            let with_head = [base.as_slice(), &[dense_concat]].concat();
            let result = validate_feature_combination(&lookup(&with_head));
            if ft == required_ft {
                assert!(result.is_ok(), "{width}: {result:?}");
            } else {
                assert!(result.unwrap_err().contains(width));
            }
            for other in [
                "threat-width-256",
                "threat-width-512",
                "threat-width-768",
                "threat-width-2816",
                "threat-width-3072",
            ] {
                if other != width {
                    let both = [with_head.as_slice(), &[other]].concat();
                    assert!(
                        validate_feature_combination(&lookup(&both))
                            .unwrap_err()
                            .contains("mutually exclusive")
                    );
                }
            }
        }
    }
}

#[test]
fn empty_features_pass() {
    let has = lookup(&[]);
    assert!(validate_feature_combination(&has).is_ok());
}

#[test]
fn unrecognized_feature_names_ignored() {
    // バリデータは自身が参照する既知 feature 名以外を無視する
    // (mode sentinel も無いので緩和されて Ok)。
    let has = lookup(&[
        "some-unrecognized-feature",
        "layerstacks-1536x15x32",
        "nnue-psqt",
    ]);
    assert!(validate_feature_combination(&has).is_ok());
}

#[test]
fn atomic_features_without_mode_pass() {
    // mode sentinel 未指定 (atomic feature 直指定) の build は check を緩和して Ok。
    let has = lookup(&[
        "layerstack-arch",
        "layerstacks-1536x15x32",
        "ft-halfka_hm_merged",
        "nnue-psqt",
        "nnue-progress-diff",
    ]);
    assert!(validate_feature_combination(&has).is_ok());
}

#[test]
fn universal_alone_ok() {
    let has = lookup(&[
        "mode-universal",
        "layerstack-arch",
        "layerstacks-1536x15x32",
        "layerstacks-768x15x32",
        "ft-halfka_hm_merged",
    ]);
    assert!(validate_feature_combination(&has).is_ok());
}

#[test]
fn runtime_dimension_universal_needs_no_static_slots() {
    let has = lookup(&[
        "mode-universal",
        "layerstack-arch",
        "halfkx-arch",
        "nnue-runtime-dimensions",
        "nnue-psqt",
        "nnue-threat",
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
    let has = lookup(&["mode-universal", "mode-specific", "layerstacks-1536x15x32"]);
    let err = validate_feature_combination(&has).unwrap_err();
    assert!(err.contains("edition-universal"));
}

#[test]
fn family_plus_specific_rejected() {
    let has = lookup(&["mode-family", "mode-specific", "layerstacks-1536x15x32"]);
    let err = validate_feature_combination(&has).unwrap_err();
    assert!(err.contains("must be exactly 1"));
}

#[test]
fn ls_arch_without_size_rejected() {
    let has = lookup(&["mode-family", "layerstack-arch"]);
    let err = validate_feature_combination(&has).unwrap_err();
    assert!(err.contains("layerstacks-* を 1 個以上"));
}

#[test]
fn atomic_ls_arch_without_size_rejected() {
    let has = lookup(&[
        "layerstack-arch",
        "halfkx-arch",
        "ft-halfkp",
        "halfkx-activation-crelu",
    ]);
    let err = validate_feature_combination(&has).unwrap_err();
    assert!(err.contains("layerstacks-* を 1 個以上"));
}

#[test]
fn atomic_ls_arch_without_ft_rejected() {
    let has = lookup(&["layerstack-arch", "layerstacks-1536x15x32"]);
    let err = validate_feature_combination(&has).unwrap_err();
    assert!(err.contains("ft-* を 1 個以上"));
}

#[test]
fn atomic_halfkx_without_ls_arch_ok() {
    let has = lookup(&["halfkx-arch", "ft-halfkp", "halfkx-activation-crelu"]);
    assert!(validate_feature_combination(&has).is_ok());
}

#[test]
fn specific_multiple_sizes_rejected() {
    let has = lookup(&[
        "mode-specific",
        "layerstack-arch",
        "layerstacks-1536x15x32",
        "layerstacks-1536x31x32",
        "ft-halfka_hm_merged",
    ]);
    let err = validate_feature_combination(&has).unwrap_err();
    assert!(err.contains("layerstacks-* を 1 個だけ"));
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
        "layerstack-arch",
        "layerstacks-1536x15x32",
        "nnue-psqt",
        "ft-halfka_hm_merged",
        "nnue-progress-diff",
    ]);
    assert!(validate_feature_combination(&has).is_ok());
}

#[test]
fn progress_diff_with_512_rejected() {
    let has = lookup(&[
        "mode-specific",
        "layerstack-arch",
        "layerstacks-512x15x32",
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
        "layerstack-arch",
        "layerstacks-768x15x32",
        "ft-halfka_hm_merged",
        "nnue-progress-diff",
    ]);
    let err = validate_feature_combination(&has).unwrap_err();
    assert!(err.contains("nnue-progress-diff"));
}

#[test]
fn progress_diff_with_1024_rejected() {
    let has = lookup(&[
        "mode-specific",
        "layerstack-arch",
        "layerstacks-1024x15x32",
        "ft-halfka_hm_merged",
        "nnue-progress-diff",
    ]);
    let err = validate_feature_combination(&has).unwrap_err();
    assert!(err.contains("nnue-progress-diff"));
}

#[test]
fn progress_diff_with_1536x31x32_ok() {
    let has = lookup(&[
        "mode-specific",
        "layerstack-arch",
        "layerstacks-1536x31x32",
        "ft-halfka_hm_merged",
        "nnue-progress-diff",
    ]);
    assert!(validate_feature_combination(&has).is_ok());
}

#[test]
fn progress_diff_with_3072_ok() {
    let has = lookup(&[
        "mode-specific",
        "layerstack-arch",
        "layerstacks-3072x15x32",
        "ft-halfka_hm_merged",
        "nnue-progress-diff",
    ]);
    assert!(validate_feature_combination(&has).is_ok());
}

#[test]
fn progress_diff_with_2304_15_64_ok() {
    let has = lookup(&[
        "mode-specific",
        "layerstack-arch",
        "layerstacks-2304x15x64",
        "ft-halfka_hm_merged",
        "nnue-progress-diff",
    ]);
    assert!(validate_feature_combination(&has).is_ok());
}

#[test]
fn progress_diff_with_3072_15_64_ok() {
    let has = lookup(&[
        "mode-specific",
        "layerstack-arch",
        "layerstacks-3072x15x64",
        "ft-halfka_hm_merged",
        "nnue-progress-diff",
    ]);
    assert!(validate_feature_combination(&has).is_ok());
}

#[test]
fn progress_diff_with_1536_15_64_ok() {
    let has = lookup(&[
        "mode-specific",
        "layerstack-arch",
        "layerstacks-1536x15x64",
        "ft-halfka_hm_merged",
        "nnue-progress-diff",
    ]);
    assert!(validate_feature_combination(&has).is_ok());
}

#[test]
fn progress_diff_in_family_rejected() {
    let has = lookup(&[
        "mode-family",
        "layerstack-arch",
        "layerstacks-1536x15x32",
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
        "layerstack-arch",
        "layerstacks-1536x15x32",
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
        "layerstack-arch",
        "layerstacks-1536x15x32",
        "layerstacks-768x15x32",
        "layerstacks-512x15x32",
        "ft-halfka_hm_merged",
    ]);
    assert!(validate_feature_combination(&has).is_ok());
}

#[test]
fn ls_arch_plus_halfkx_arch_ok() {
    let has = lookup(&[
        "mode-universal",
        "layerstack-arch",
        "halfkx-arch",
        "layerstacks-1536x15x32",
        "ft-halfka_hm_merged",
    ]);
    assert!(validate_feature_combination(&has).is_ok());
}

#[test]
fn ls_specific_with_ft_halfkp_ok() {
    let has = lookup(&[
        "mode-specific",
        "layerstack-arch",
        "layerstacks-1536x15x32",
        "ft-halfkp",
    ]);
    assert!(validate_feature_combination(&has).is_ok());
}

#[test]
fn ls_specific_with_ft_halfka_split_ok() {
    let has = lookup(&[
        "mode-specific",
        "layerstack-arch",
        "layerstacks-1536x15x32",
        "ft-halfka_split",
    ]);
    assert!(validate_feature_combination(&has).is_ok());
}

#[test]
fn ls_specific_with_ft_halfka_merged_ok() {
    let has = lookup(&[
        "mode-specific",
        "layerstack-arch",
        "layerstacks-1536x15x32",
        "ft-halfka_merged",
    ]);
    assert!(validate_feature_combination(&has).is_ok());
}

#[test]
fn ls_specific_with_ft_halfka_hm_split_ok() {
    let has = lookup(&[
        "mode-specific",
        "layerstack-arch",
        "layerstacks-1536x15x32",
        "ft-halfka_hm_split",
    ]);
    assert!(validate_feature_combination(&has).is_ok());
}

#[test]
fn ls_specific_with_ft_halfka_hm_merged_ok() {
    let has = lookup(&[
        "mode-specific",
        "layerstack-arch",
        "layerstacks-1536x15x32",
        "ft-halfka_hm_merged",
        "nnue-progress-diff",
    ]);
    assert!(validate_feature_combination(&has).is_ok());
}

#[test]
fn halfka_effect_bucket_specific_ok() {
    let has = lookup(&[
        "mode-specific",
        "layerstack-arch",
        "layerstacks-512x15x32",
        "ft-halfka_hm_merged",
        "nnue-effect-bucket",
        "effect-bucket-2x2-kingfixed",
    ]);
    assert!(validate_feature_combination(&has).is_ok());
}

#[test]
fn halfka_effect_bucket_1024_3x3_kingfixed_specific_ok() {
    let has = lookup(&[
        "mode-specific",
        "layerstack-arch",
        "layerstacks-1024x15x32",
        "ft-halfka_hm_merged",
        "nnue-effect-bucket",
        "effect-bucket-3x3-kingfixed",
    ]);
    assert!(validate_feature_combination(&has).is_ok());
}

#[test]
fn halfka_effect_bucket_without_mode_rejected() {
    let has = lookup(&[
        "layerstack-arch",
        "layerstacks-512x15x32",
        "ft-halfka_hm_merged",
        "nnue-effect-bucket",
        "effect-bucket-2x2-kingfixed",
    ]);
    let err = validate_feature_combination(&has).unwrap_err();
    assert!(err.contains("mode-specific"));
}

#[test]
fn halfka_effect_bucket_in_universal_rejected() {
    let has = lookup(&[
        "mode-universal",
        "layerstack-arch",
        "layerstacks-512x15x32",
        "layerstacks-1024x15x32",
        "ft-halfkp",
        "ft-halfka_hm_merged",
        "nnue-effect-bucket",
        "effect-bucket-2x2-kingfixed",
    ]);
    let err = validate_feature_combination(&has).unwrap_err();
    assert!(err.contains("mode-specific"));
}

#[test]
fn halfka_effect_bucket_with_extra_ft_rejected() {
    let has = lookup(&[
        "mode-specific",
        "layerstack-arch",
        "layerstacks-512x15x32",
        "ft-halfka_hm_merged",
        "ft-halfka_split",
        "nnue-effect-bucket",
        "effect-bucket-2x2-kingfixed",
    ]);
    let err = validate_feature_combination(&has).unwrap_err();
    assert!(err.contains("ft-halfka_hm_merged だけ"));
}

#[test]
fn halfka_effect_bucket_with_threat_rejected() {
    let has = lookup(&[
        "mode-specific",
        "layerstack-arch",
        "layerstacks-512x15x32",
        "ft-halfka_hm_merged",
        "nnue-threat",
        "nnue-effect-bucket",
        "effect-bucket-2x2-kingfixed",
    ]);
    let err = validate_feature_combination(&has).unwrap_err();
    assert!(err.contains("nnue-threat"));
}

#[test]
fn halfka_effect_bucket_with_psqt_rejected() {
    let has = lookup(&[
        "mode-specific",
        "layerstack-arch",
        "layerstacks-512x15x32",
        "ft-halfka_hm_merged",
        "nnue-psqt",
        "nnue-effect-bucket",
        "effect-bucket-2x2-kingfixed",
    ]);
    let err = validate_feature_combination(&has).unwrap_err();
    assert!(err.contains("nnue-psqt"));
}

#[test]
fn ls_arch_with_halfkx_arch_specific_ft_halfkp_ok() {
    // mode-specific + layerstack-arch + halfkx-arch + ft-halfkp + layerstacks-* は許容。
    let has = lookup(&[
        "mode-specific",
        "layerstack-arch",
        "halfkx-arch",
        "layerstacks-512x15x32",
        "ft-halfkp",
        "halfkx-activation-crelu",
    ]);
    assert!(validate_feature_combination(&has).is_ok());
}

#[test]
fn ls_only_family_with_multi_ft_ok() {
    let has = lookup(&[
        "mode-family",
        "layerstack-arch",
        "layerstacks-1536x15x32",
        "layerstacks-768x15x32",
        "ft-halfkp",
        "ft-halfka_hm_merged",
    ]);
    assert!(validate_feature_combination(&has).is_ok());
}

#[test]
fn ls_only_universal_with_all_ft_ok() {
    let has = lookup(&[
        "mode-universal",
        "layerstack-arch",
        "layerstacks-1536x15x32",
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
    let has = lookup(&["mode-specific", "layerstack-arch", "layerstacks-1536x15x32"]);
    let err = validate_feature_combination(&has).unwrap_err();
    assert!(err.contains("ft-* を 1 個以上"));
}

#[test]
fn halfkx_specific_with_ft_halfkp_ok() {
    // HalfKX 単独 (layerstack-arch なし) では ft-halfkp は valid。
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

#[test]
fn fused_1536_requires_a_single_measured_architecture() {
    let required = [
        "layerstack-arch",
        "ft-halfka_hm_merged",
        "layerstacks-1536x15x32",
    ];
    assert!(use_avx512bw_fused_1536(&lookup(&required)));
    let mut specific = required.to_vec();
    specific.push("mode-specific");
    assert!(use_avx512bw_fused_1536(&lookup(&specific)));
    for feature in [
        "mode-family",
        "mode-universal",
        "nnue-runtime-dimensions",
        "nnue-threat",
        "nnue-psqt",
        "nnue-effect-bucket",
        "ft-halfkp",
        "ft-halfka_split",
        "ft-halfka_merged",
        "ft-halfka_hm_split",
        "layerstacks-1536x31x32",
        "layerstacks-768x15x32",
        "layerstacks-768x7x32",
        "layerstacks-512x15x32",
        "layerstacks-1024x15x32",
        "layerstacks-3072x15x32",
        "layerstacks-1024x15x64",
        "layerstacks-2304x15x64",
        "layerstacks-3072x15x64",
        "layerstacks-1536x15x64",
    ] {
        let mut features = required.to_vec();
        features.push(feature);
        assert!(!use_avx512bw_fused_1536(&lookup(&features)), "{feature}");
    }
    for missing in 0..required.len() {
        let features: Vec<_> = required
            .iter()
            .enumerate()
            .filter_map(|(i, f)| (i != missing).then_some(*f))
            .collect();
        assert!(!use_avx512bw_fused_1536(&lookup(&features)));
    }
}
