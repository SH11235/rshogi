//! Workers フロントエンドが依存する core 契約の smoke テスト。
//!
//! `GameRoom` Durable Object (wasm32 target only) の `finalize_if_ended` は
//! 対局終局時に [`primary_result_code`] を呼び出し、`FinishedState::result_code`
//! と R2 棋譜の `kifu_result_code` metadata を 1 つの文字列で固定する。
//! DO 本体はホスト target ではコンパイルされないが、同じ関数は core crate に
//! 単一 source of truth として存在するため、全 `GameResult` variant に対する
//! マッピングをホスト target 上で網羅テストして回帰検知する。
//!
//! 完全な DO 統合テストは `wrangler dev` (Miniflare) 下の外部ハーネスで別途
//! 実施する (task A (`csa-observers-chat`) で WebSocket ハーネスを整備する時に
//! 合流する)。

use rshogi_csa_server::game::result::{GameResult, IllegalReason};
use rshogi_csa_server::record::kifu::primary_result_code;
use rshogi_csa_server::types::Color;

/// 全 `GameResult` variant の `primary_result_code` マッピングを網羅して固定する。
///
/// Workers DO の `finalize_if_ended` が書き出す `result_code` は本関数が唯一の
/// 情報源。variant 追加時はこの網羅テストが落ちることで、R2 に書き出される
/// コードの更新忘れを検知できる。
#[test]
fn primary_result_code_maps_every_game_result_variant() {
    assert_eq!(
        primary_result_code(&GameResult::Toryo {
            winner: Color::Black
        }),
        "#RESIGN"
    );
    assert_eq!(
        primary_result_code(&GameResult::TimeUp {
            loser: Color::White
        }),
        "#TIME_UP"
    );
    for reason in [
        IllegalReason::Generic,
        IllegalReason::Uchifuzume,
        IllegalReason::IllegalKachi,
    ] {
        assert_eq!(
            primary_result_code(&GameResult::IllegalMove {
                loser: Color::Black,
                reason,
            }),
            "#ILLEGAL_MOVE",
            "IllegalReason::{reason:?} should map to #ILLEGAL_MOVE"
        );
    }
    assert_eq!(
        primary_result_code(&GameResult::Kachi {
            winner: Color::Black
        }),
        "#JISHOGI"
    );
    assert_eq!(
        primary_result_code(&GameResult::OuteSennichite {
            loser: Color::Black
        }),
        "#OUTE_SENNICHITE"
    );
    assert_eq!(primary_result_code(&GameResult::Sennichite), "#SENNICHITE");
    assert_eq!(primary_result_code(&GameResult::MaxMoves), "#MAX_MOVES");
    assert_eq!(primary_result_code(&GameResult::Abnormal { winner: None }), "#ABNORMAL");
    assert_eq!(
        primary_result_code(&GameResult::Abnormal {
            winner: Some(Color::Black)
        }),
        "#ABNORMAL"
    );
}
