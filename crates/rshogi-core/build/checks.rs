// build.rs と tests/build_rs_checks.rs から `include!` される純粋ロジック。
// 環境変数や Cargo 連携には依存させず、`has_feature` lookup を引数で受け取る。

/// Cargo feature 組合せの整合性を検証する。
///
/// 不正組合せが見つかれば `Err(message)` を返す。
/// 詳細は `docs/decisions/2026-05-24-build-edition-flavor-design.md` を参照。
fn validate_feature_combination(
    has_feature: &dyn Fn(&str) -> bool,
) -> Result<(), String> {
    let layerstack_arch = has_feature("layerstack-arch");
    let runtime_dimensions = has_feature("nnue-runtime-dimensions");

    let mode_universal = has_feature("mode-universal");
    let mode_family = has_feature("mode-family");
    let mode_specific = has_feature("mode-specific");
    let mode_count =
        (mode_universal as u8) + (mode_family as u8) + (mode_specific as u8);

    let layerstacks_features: &[&str] = &[
        "layerstacks-1536x16x32",
        "layerstacks-1536x32x32",
        "layerstacks-768x16x32",
        "layerstacks-768x8x32",
        "layerstacks-512x16x32",
        "layerstacks-1024x16x32",
        "layerstacks-3072x16x32",
        "layerstacks-2304x16x64",
        "layerstacks-3072x16x64",
        "layerstacks-1536x16x64",
    ];
    let layerstacks_count = layerstacks_features
        .iter()
        .filter(|f| has_feature(f))
        .count();

    let ft_features: &[&str] = &[
        "ft-halfkp",
        "ft-halfka_split",
        "ft-halfka_merged",
        "ft-halfka_hm_split",
        "ft-halfka_hm_merged",
    ];
    let ft_count = ft_features.iter().filter(|f| has_feature(f)).count();

    if has_feature("nnue-effect-bucket") {
        if !mode_specific {
            return Err(
                "nnue-effect-bucket は mode-specific の effect bucket 専用 edition でのみ有効です。\
                 universal / family / atomic feature 直指定では使えません。"
                    .to_string(),
            );
        }
        if has_feature("nnue-threat") || has_feature("nnue-psqt") {
            return Err(
                "nnue-effect-bucket は LayerStacks の base index を effect bucket 用に差し替えるため、\
                 nnue-threat / nnue-psqt と同時指定できません。"
                    .to_string(),
            );
        }
        if ft_count != 1 || !has_feature("ft-halfka_hm_merged") {
            return Err(format!(
                "nnue-effect-bucket では ft-* は ft-halfka_hm_merged だけを指定してください \
                 (現在 {ft_count} 個有効)。"
            ));
        }

        let effect_bucket_configs: &[&str] = &[
            "effect-bucket-2x2-kingfixed",
            "effect-bucket-2x2-kingbucketed",
            "effect-bucket-3x3-kingfixed",
            "effect-bucket-3x3-kingbucketed",
        ];
        let effect_bucket_config_count = effect_bucket_configs.iter().filter(|f| has_feature(f)).count();
        if effect_bucket_config_count != 1 {
            return Err(format!(
                "nnue-effect-bucket では effect-bucket-* を 1 個だけ指定してください (現在 {effect_bucket_config_count} 個有効)。"
            ));
        }
        if !has_feature("layerstack-arch") {
            return Err(
                "nnue-effect-bucket は layerstack-arch と一緒に指定してください。"
                    .to_string(),
            );
        }
    }

    // mode sentinel 未指定の build は旧 atomic feature 直指定の従来運用と見なし、
    // 以降の mode 依存 check を緩和する (組合せ妥当性は user 責任)。
    if mode_count == 0 {
        return Ok(());
    }

    if mode_universal && (mode_family || mode_specific) {
        return Err(
            "edition-universal は他 edition (family / specific) と同時指定できません。\
             preset edition を 1 つだけ有効化してください。"
                .to_string(),
        );
    }
    if mode_count != 1 {
        return Err(format!(
            "mode-* features must be exactly 1; got {mode_count} \
             (universal={mode_universal}, family={mode_family}, specific={mode_specific})."
        ));
    }

    if layerstack_arch && layerstacks_count == 0 && !runtime_dimensions {
        return Err(
            "layerstack-arch を有効化するには layerstacks-* を 1 個以上必要です。".to_string(),
        );
    }

    if layerstack_arch && ft_count == 0 && !runtime_dimensions {
        return Err("layerstack-arch を有効化するには ft-* を 1 個以上必要です。".to_string());
    }

    if mode_specific {
        if layerstacks_count > 1 {
            return Err(format!(
                "mode-specific では layerstacks-* を 1 個だけ指定してください (現在 {layerstacks_count} 個有効)。"
            ));
        }
        let activations: &[&str] = &[
            "halfkx-activation-crelu",
            "halfkx-activation-screlu",
            "halfkx-activation-pairwise",
        ];
        let activation_count =
            activations.iter().filter(|f| has_feature(f)).count();
        if activation_count > 1 {
            return Err(format!(
                "mode-specific では halfkx-activation-* を 1 個までしか指定できません (現在 {activation_count} 個有効)。"
            ));
        }
        if ft_count > 1 {
            return Err(format!(
                "mode-specific では ft-* を 1 個までしか指定できません (現在 {ft_count} 個有効)。"
            ));
        }
    }

    // nnue-progress-diff は L0=1536 系 / L0=3072 で性能向上 (実測確認済み)、
    // L0=768/512 で退行する trade-off。退行構成での誤指定を弾く。
    if has_feature("nnue-progress-diff") {
        let valid = mode_specific
            && (has_feature("layerstacks-1536x16x32")
                || has_feature("layerstacks-1536x32x32")
                || has_feature("layerstacks-3072x16x32")
                || has_feature("layerstacks-2304x16x64")
                || has_feature("layerstacks-3072x16x64")
                || has_feature("layerstacks-1536x16x64"));
        if !valid {
            return Err(
                "nnue-progress-diff は mode-specific + layerstacks-1536x16x32 / layerstacks-1536x32x32 \
                 / layerstacks-3072x16x32 / layerstacks-2304x16x64 / layerstacks-3072x16x64 / layerstacks-1536x16x64 でのみ有効です。他構成では NPS が退行します。"
                    .to_string(),
            );
        }
    }

    Ok(())
}
