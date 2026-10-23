//! build.rs の結果を変える feature の判定 (`result_features`) の単体テスト。

include!("../../build/result_features.rs");

fn enabled(features: &'static [&'static str]) -> impl Fn(&str) -> bool {
    move |name: &str| features.contains(&name)
}

const MANIFEST: &str = r#"
[package]
name = "x"

[features]
# comment
default = ["edition-universal"]
search-stats = []
use-lazy-evaluate = []
brand-new-feature = []
edition-universal = [
  "mode-universal",
  "nnue-runtime-dimensions",
]
mode-universal = []
nnue-runtime-dimensions = []
layerstacks-1536x16x32 = []
threat-profile-same-class = []

[dependencies]
serde = "1"
"#;

#[test]
fn neutral_features_are_dropped_and_unknown_ones_count() {
    let all = enabled(&[
        "default",
        "search-stats",
        "use-lazy-evaluate",
        "brand-new-feature",
        "edition-universal",
        "mode-universal",
        "nnue-runtime-dimensions",
        "layerstacks-1536x16x32",
        "threat-profile-same-class",
        "serde",
    ]);
    assert_eq!(
        result_features(MANIFEST, &all),
        [
            "brand-new-feature",
            "threat-profile-same-class",
            "use-lazy-evaluate"
        ]
    );
    assert!(result_features(MANIFEST, &enabled(&["default", "edition-universal"])).is_empty());
}

#[test]
fn real_manifest_lists_every_feature_and_default_build_is_neutral() {
    let manifest =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).unwrap();
    let names = manifest_features(&manifest);
    for name in [
        "default",
        "use-lazy-evaluate",
        "search-pass-rules",
        "nnue-progress-diff",
        "edition-universal",
    ] {
        assert!(names.iter().any(|n| n == name), "{name}");
    }
    assert!(!names.iter().any(|n| n.starts_with('"') || n.contains(' ')));
    // 既定の universal edition が引き込む feature は結果を変えない側に入る。
    let universal = [
        "default",
        "edition-universal",
        "mode-universal",
        "nnue-runtime-dimensions",
        "halfkx-arch",
        "nnue-psqt",
        "nnue-threat",
    ];
    let has = move |name: &str| universal.contains(&name);
    assert!(result_features(&manifest, &has).is_empty());
    let specific = |name: &str| {
        [
            "edition-layerstacks-halfka_hm_merged-1536x16x32-none",
            "mode-specific",
            "layerstack-arch",
            "ft-halfka_hm_merged",
            "layerstacks-1536x16x32",
            "nnue-progress-diff",
            "use-lazy-evaluate",
        ]
        .contains(&name)
    };
    assert_eq!(
        result_features(&manifest, &specific),
        ["nnue-progress-diff", "use-lazy-evaluate"]
    );
}
