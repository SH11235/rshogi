/// 共通のエンジンプロファイル定義。
///
/// - selfplay_eval_targets（targets.json 再評価ツール）
/// - debug_position_multipv（engine-usi を用いた MultiPV 解析ツール）
///
/// から参照され、同じプリセット名（base/short/rootfull/gates）に対して
/// 一貫した SearchParams / Root オプション / 環境変数を適用できるようにする。

#[derive(Clone)]
pub struct EngineProfilePreset {
    pub name: &'static str,
    pub search_params: &'static [(&'static str, &'static str)],
    pub root_options: &'static [(&'static str, &'static str)],
    pub env: &'static [(&'static str, &'static str)],
}

/// selfplay_eval_targets の DEFAULT_PROFILES と同等のプリセット。
pub const ENGINE_PROFILE_PRESETS: &[EngineProfilePreset] = &[
    EngineProfilePreset {
        name: "base",
        search_params: &[("RootBeamForceFullCount", "0")],
        root_options: &[],
        env: &[],
    },
    // 短TC（例: 1000ms）を想定したプロファイル。
    // - RootSeeGate を有効化し、静かな手のうち XSEE が大きく悪いものをルートで間引く。
    // - Quiet SEE Guard / capture futility は少し強め寄りの設定を想定（環境変数で制御）。
    EngineProfilePreset {
        name: "short",
        search_params: &[("RootBeamForceFullCount", "0")],
        root_options: &[("RootSeeGate", "true"), ("RootSeeGate.XSEE", "150")],
        env: &[
            ("SHOGI_QUIET_SEE_GUARD", "1"),
            ("SHOGI_CAPTURE_FUT_SCALE", "120"),
        ],
    },
    EngineProfilePreset {
        name: "rootfull",
        search_params: &[("RootBeamForceFullCount", "4")],
        root_options: &[],
        env: &[],
    },
    EngineProfilePreset {
        name: "gates",
        search_params: &[("RootBeamForceFullCount", "0")],
        root_options: &[("RootSeeGate.XSEE", "0")],
        env: &[("SHOGI_QUIET_SEE_GUARD", "0")],
    },
];

/// プロファイル名からプリセット定義を取得する。
pub fn find_profile(name: &str) -> Option<&'static EngineProfilePreset> {
    ENGINE_PROFILE_PRESETS.iter().find(|p| p.name == name)
}
