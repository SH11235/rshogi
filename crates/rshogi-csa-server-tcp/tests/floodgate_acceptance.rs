//! Floodgate 機能（task 15.x）の受入シナリオ統合テスト。
//!
//! 本テストは TCP listener や fake client を立てる「フル E2E」ではなく、
//! `ServerConfig` を Floodgate 全機能 ON で組み立てたときの起動経路と
//! intent 算出の **横断的整合性** を固定する smoke 統合テスト。各機能の
//! 個別テストはそれぞれの crate / モジュールで網羅済み:
//!
//! - 15.5 (Players レート永続化): `rshogi_csa_server::storage::players_yaml::tests`
//! - 15.1 (スケジューラ): `rshogi_csa_server::scheduler::tests` +
//!   `rshogi_csa_server_tcp::scheduler::tests`
//! - 15.3 (Floodgate 履歴): `rshogi_csa_server::storage::floodgate_history::tests`
//! - 15.2 (LeastDiff ペアリング): `rshogi_csa_server::matching::pairing::tests`
//! - 15.4 (駒落ち / 重複ログイン): `rshogi_csa_server_tcp::server::tests`
//!
//! 本受入は「全部入りで起動できるか」を上から見て確認する位置付けで、
//! E2E（実 TCP / 実エンジン接続）は task 20.1 負荷試験ハーネスで扱う。

use std::collections::HashMap;
use std::path::PathBuf;

use rshogi_csa_server::{FloodgateFeatureIntent, FloodgateSchedule, FloodgateWeekday};
use rshogi_csa_server_tcp::server::{DuplicateLoginPolicy, ServerConfig, prepare_runtime};

/// Floodgate 全機能を ON にした構成を組み立てるヘルパ。
///
/// 各 PR で追加された機能 (`players_yaml_path` / `floodgate_schedules` /
/// `floodgate_history_path` / `handicap_initial_sfens` / `duplicate_login_policy`)
/// を全て利用する設定を返す。`allow_floodgate_features` は呼び出し側で立てる。
fn floodgate_full_config(allow_floodgate: bool) -> ServerConfig {
    let mut cfg = ServerConfig::sensible_defaults();
    cfg.allow_floodgate_features = allow_floodgate;
    cfg.players_yaml_path = Some(PathBuf::from("/tmp/players.yaml"));
    cfg.floodgate_schedules.push(FloodgateSchedule {
        game_name: "floodgate-600-10".to_owned(),
        weekday: FloodgateWeekday::Mon,
        hour: 9,
        minute: 0,
        pairing_strategy: "least_diff".to_owned(),
    });
    cfg.floodgate_history_path = Some(PathBuf::from("/tmp/history.jsonl"));
    let mut handicap = HashMap::new();
    handicap.insert(
        "floodgate-handicap-kakkin".to_owned(),
        "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1".to_owned(),
    );
    cfg.handicap_initial_sfens = handicap;
    cfg.duplicate_login_policy = DuplicateLoginPolicy::EvictOld;
    cfg
}

/// 全 Floodgate 機能を要求する構成は、`--allow-floodgate-features` opt-in
/// が立っていれば `prepare_runtime` を通過する（受入シナリオ起動経路）。
#[test]
fn full_floodgate_stack_passes_prepare_runtime_with_optin() {
    let cfg = floodgate_full_config(true);
    prepare_runtime(&cfg).expect("full floodgate stack must start when opt-in is enabled");
}

/// 全 Floodgate 機能を要求する構成は、opt-in なしでは起動失敗する。
/// gate メッセージは要求された各機能を列挙する（運用者が何を opt-in すべきか
/// 一目で判別できるよう）。
#[test]
fn full_floodgate_stack_fails_prepare_runtime_without_optin() {
    let cfg = floodgate_full_config(false);
    let err = prepare_runtime(&cfg).expect_err("full stack must fail-fast without opt-in");
    // 各 Floodgate 機能要求が error メッセージに乗っている。
    for required in [
        "scheduler",
        "persistent_player_rates",
        "floodgate_history",
        "duplicate_login_policy",
    ] {
        assert!(err.contains(required), "error must list {required}, got: {err}");
    }
}

/// 既定構成（Floodgate 機能を 1 つも要求しない）は、opt-in 有無に関わらず
/// `prepare_runtime` を通過する。通常運用経路の安全網。
#[test]
fn defaults_pass_prepare_runtime_regardless_of_optin() {
    for allow in [false, true] {
        let mut cfg = ServerConfig::sensible_defaults();
        cfg.allow_floodgate_features = allow;
        prepare_runtime(&cfg).expect("defaults must always start (allow={allow})");
    }
}

/// closure 型定義を簡潔化する type alias（clippy::type_complexity 対応）。
type FeatureMutate = fn(&mut ServerConfig);

/// Floodgate 機能を 1 つだけ立てたときの intent と error メッセージの整合性を
/// 横断的に固定する。新機能を追加する PR が `floodgate_intent_from_config`
/// の更新を忘れた場合の回帰検出。
#[test]
fn each_floodgate_feature_individually_requires_optin() {
    fn add_persistent_player_rates(c: &mut ServerConfig) {
        c.players_yaml_path = Some(PathBuf::from("/tmp/p.yaml"));
    }
    fn add_scheduler(c: &mut ServerConfig) {
        c.floodgate_schedules.push(FloodgateSchedule {
            game_name: "g".to_owned(),
            weekday: FloodgateWeekday::Mon,
            hour: 0,
            minute: 0,
            pairing_strategy: "direct".to_owned(),
        });
    }
    fn add_floodgate_history(c: &mut ServerConfig) {
        c.floodgate_history_path = Some(PathBuf::from("/tmp/h.jsonl"));
    }
    fn add_duplicate_login_policy(c: &mut ServerConfig) {
        c.duplicate_login_policy = DuplicateLoginPolicy::EvictOld;
    }
    let cases: &[(&str, FeatureMutate)] = &[
        ("persistent_player_rates", add_persistent_player_rates),
        ("scheduler", add_scheduler),
        ("floodgate_history", add_floodgate_history),
        ("duplicate_login_policy", add_duplicate_login_policy),
    ];
    for (label, mutate) in cases {
        let mut cfg = ServerConfig::sensible_defaults();
        cfg.allow_floodgate_features = false;
        mutate(&mut cfg);
        let err = prepare_runtime(&cfg)
            .expect_err("feature {label} requested without opt-in must fail-fast");
        assert!(err.contains(label), "expected error to mention {label}, got: {err}");
        // opt-in を立てれば通過する。
        cfg.allow_floodgate_features = true;
        prepare_runtime(&cfg).expect("feature must start with opt-in");
    }
}

/// `FloodgateFeatureIntent` の Default は全フラグ false で、`validate_floodgate_feature_gate`
/// を通過する（gate off 時の通常起動が壊れない契約の固定）。
#[test]
fn default_floodgate_intent_does_not_request_anything() {
    let intent = FloodgateFeatureIntent::default();
    assert!(!intent.enable_scheduler);
    assert!(!intent.use_non_direct_pairing);
    assert!(!intent.enable_duplicate_login_policy);
    assert!(!intent.enable_persistent_player_rates);
    assert!(!intent.enable_floodgate_history);
    assert!(!intent.enable_reconnect_protocol);
}

/// 駒落ち初期局面マップ（task 15.4）は Floodgate gate 対象 **外**。
/// `handicap_initial_sfens` を非空にしても opt-in なしで起動を通す（駒落ち
/// 自体は買い切りの設定で、Floodgate 運用機能ではない）。
#[test]
fn handicap_initial_sfens_does_not_require_floodgate_optin() {
    let mut cfg = ServerConfig::sensible_defaults();
    cfg.allow_floodgate_features = false;
    cfg.handicap_initial_sfens.insert(
        "floodgate-handicap-kakkin".to_owned(),
        "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1".to_owned(),
    );
    prepare_runtime(&cfg).expect("handicap config alone must not require floodgate opt-in");
}
