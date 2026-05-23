// build.rs と tests/build_rs_checks.rs から `include!` される純粋ロジック。
// 環境変数や Cargo 連携には依存させず、`has_feature` lookup を引数で受け取る。

/// Cargo feature 組合せの整合性を検証する。
///
/// 不正組合せが見つかれば `Err(message)` を返す。
/// 詳細は `docs/decisions/2026-05-24-build-edition-flavor-design.md` を参照。
fn validate_feature_combination(
    has_feature: &dyn Fn(&str) -> bool,
) -> Result<(), String> {
    let ls_arch = has_feature("ls-arch");
    let halfkx_arch = has_feature("halfkx-arch");

    let mode_universal = has_feature("mode-universal");
    let mode_family = has_feature("mode-family");
    let mode_specific = has_feature("mode-specific");
    let mode_count =
        (mode_universal as u8) + (mode_family as u8) + (mode_specific as u8);

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

    let ls_size_features: &[&str] = &[
        "ls-size-1536x16x32",
        "ls-size-1536x32x32",
        "ls-size-768x16x32",
        "ls-size-512x16x32",
    ];
    let ls_size_count = ls_size_features
        .iter()
        .filter(|f| has_feature(f))
        .count();
    if ls_arch && ls_size_count == 0 {
        return Err(
            "ls-arch を有効化するには ls-size-* を 1 個以上必要です。".to_string(),
        );
    }

    if mode_specific {
        if ls_size_count > 1 {
            return Err(format!(
                "mode-specific では ls-size-* を 1 個だけ指定してください (現在 {ls_size_count} 個有効)。"
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
        let ft_features: &[&str] = &[
            "ft-halfkp",
            "ft-halfka_split",
            "ft-halfka_merged",
            "ft-halfka_hm_split",
            "ft-halfka_hm_merged",
        ];
        let ft_count = ft_features.iter().filter(|f| has_feature(f)).count();
        if ft_count > 1 {
            return Err(format!(
                "mode-specific では ft-* を 1 個までしか指定できません (現在 {ft_count} 個有効)。"
            ));
        }

        // LS network 単独 specific build では `ft-halfka_hm_merged` のみ実装済み。
        // halfkx-arch が同時に立っていれば他 FT は HalfKX 経路にルーティング可能なため許容。
        if ls_arch && !halfkx_arch {
            for ft in &[
                "ft-halfkp",
                "ft-halfka_split",
                "ft-halfka_merged",
                "ft-halfka_hm_split",
            ] {
                if has_feature(ft) {
                    return Err(format!(
                        "LayerStack 単独 specific build (ls-arch + not(halfkx-arch)) では \
                         ft-halfka_hm_merged のみサポートします (`{ft}` 指定済み)。"
                    ));
                }
            }
        }
    }

    // nnue-progress-diff は L0=1536 系で性能向上、L0=768/512 で退行する trade-off。
    // 退行構成での誤指定を弾く。
    if has_feature("nnue-progress-diff") {
        let valid = mode_specific
            && (has_feature("ls-size-1536x16x32")
                || has_feature("ls-size-1536x32x32"));
        if !valid {
            return Err(
                "nnue-progress-diff は mode-specific + ls-size-1536x16x32 / ls-size-1536x32x32 \
                 でのみ有効です。他構成では NPS が退行します。"
                    .to_string(),
            );
        }
    }

    Ok(())
}
