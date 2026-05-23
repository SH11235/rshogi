// build.rs と tests/build_rs_checks.rs から `include!` される純粋ロジック。
// std::env や Cargo 連携には依存させず、`has_feature` lookup を引数で受け取る。

/// Phase 1 整合性チェック (Edition 軸 ADR)。
///
/// `has_feature(name)` は feature 名を受け取り「有効か」を返す lookup。
/// 不正組合せが見つかれば `Err(message)` を返し、build.rs はその文字列で panic する。
///
/// チェック内容と趣旨は [`docs/decisions/2026-05-24-build-edition-flavor-design.md`]
/// 「`build.rs` 整合性チェック」セクションを参照。
///
/// # check 順序と Phase 1 互換
///
/// 1. ADR 由来の「アーキテクチャ整合性」check (`ls-arch` × `halfkx-arch` 同時指定、
///    LS × 未実装 FT) は mode sentinel の有無に関わらず常に適用する。これらは
///    feature の論理矛盾そのものなので、Phase 1 互換モード下でも fail-fast すべき。
///    旧 build script は新 atomic feature (`halfkx-arch` / `ft-*`) を立てないため
///    互換性は保たれる。
/// 2. mode sentinel (`mode-*`) が 0 個の build は旧 atomic feature 直指定の従来運用
///    と見なし、それ以降の mode 依存 check (size/activation/ft 重複、progress-diff
///    の L0=1536 制約等) を緩和する。
/// 3. mode sentinel がちょうど 1 個の build は preset edition 指定相当と見なし、
///    全 check を厳格に適用する。
fn validate_feature_combination(
    has_feature: &dyn Fn(&str) -> bool,
) -> Result<(), String> {
    let ls_arch = has_feature("ls-arch");
    let halfkx_arch = has_feature("halfkx-arch");

    // [Phase 1 限定] ls-arch (旧 layerstack-only 意味論で HalfKX 経路除去) と
    // halfkx-arch の同時指定は意味論衝突。mode の有無に関わらず reject する。
    // Phase 2 で `ls-arch` の意味論を include-only に再定義した段階で削除予定。
    if ls_arch && halfkx_arch {
        return Err(
            "Phase 1 では ls-arch と halfkx-arch の同時指定は未サポートです。\
             `ls-arch` は現状旧 `layerstack-only` 意味論 (HalfKX 経路を除去) を継承して \
             いるため、`halfkx-arch` と組合せると build 構成が不整合になります。\
             Phase 2 で `edition-universal` 経由で本対応予定。"
                .to_string(),
        );
    }

    // [ADR Phase 1] LS network 上では `ft-halfka_hm_merged` (= 旧 HalfKA_hm) のみ
    // 実装済み。他 4 variant (`ft-halfkp` / `ft-halfka_split` / `ft-halfka_merged`
    // / `ft-halfka_hm_split`) は LS network 側 FT generic 化 (Issue #734) 完了後に
    // サポート予定のため、`ls-arch` 有効時に立てれば mode に関わらず reject する。
    if ls_arch {
        let invalid_ls_ft: &[&str] = &[
            "ft-halfkp",
            "ft-halfka_split",
            "ft-halfka_merged",
            "ft-halfka_hm_split",
        ];
        for ft in invalid_ls_ft {
            if has_feature(ft) {
                return Err(format!(
                    "Phase 1 では LayerStack (ls-arch) network は ft-halfka_hm_merged のみ \
                     サポートします (`{ft}` 指定済み)。\
                     他 FT variant は SH11235/rshogi#734 で LS 側 FT generic 化後に対応予定。"
                ));
            }
        }
    }

    let mode_universal = has_feature("mode-universal");
    let mode_family = has_feature("mode-family");
    let mode_specific = has_feature("mode-specific");
    let mode_count =
        (mode_universal as u8) + (mode_family as u8) + (mode_specific as u8);

    // Phase 1 互換: mode sentinel が 1 個も立っていない build は
    // 旧 atomic feature 直指定の従来運用と見なし、以降の整合性チェックを緩和する。
    // 上記アーキテクチャ整合性 check は通過済みなので、論理矛盾のある build は
    // ここに到達できない。
    if mode_count == 0 {
        return Ok(());
    }

    // mode-universal と family / specific の同時指定は禁止。
    if mode_universal && (mode_family || mode_specific) {
        return Err(
            "edition-universal は他 edition (family / specific) との同時指定不可です。\
             preset edition を 1 つだけ有効化してください。"
                .to_string(),
        );
    }

    // mode-* がちょうど 1 個有効でなければエラー (universal + 他 を弾いた後の二重指定保険)。
    if mode_count != 1 {
        return Err(format!(
            "mode-* features must be exactly 1; got {mode_count} \
             (universal={mode_universal}, family={mode_family}, specific={mode_specific}). \
             edition-* preset を 1 つだけ有効化してください。"
        ));
    }

    // ls-arch を立てるなら ls-size-* が 1 個以上必要。
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
    }

    // nnue-progress-diff は L0=1536 specific Edition でのみ valid。
    // L0=768 / 512 で有効化すると memory `feature_nnue_progress_diff` 記録の通り
    // -2〜6% 退行するため、universal / family / 他 size specific では弾く。
    if has_feature("nnue-progress-diff") {
        let valid_for_progress_diff = mode_specific
            && (has_feature("ls-size-1536x16x32")
                || has_feature("ls-size-1536x32x32"));
        if !valid_for_progress_diff {
            return Err(
                "nnue-progress-diff は mode-specific + ls-size-1536x16x32 / ls-size-1536x32x32 \
                 でのみ有効です。L0=768 / 512 や universal / family では NPS が退行するため \
                 build を停止します。"
                    .to_string(),
            );
        }
    }

    Ok(())
}
