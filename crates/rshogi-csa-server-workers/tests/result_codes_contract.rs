use rshogi_csa_server::{
    game::result::{GameResult, IllegalReason},
    record::kifu::primary_result_code,
    types::Color,
};
use rshogi_csa_server_workers::games_index::classify_result;
use serde::Serialize;

#[derive(Serialize)]
struct ResultCodesContract {
    variants: Vec<ResultVariantRow>,
    csa_outcome_codes: Vec<&'static str>,
    result_kinds: Vec<&'static str>,
}

#[derive(Serialize)]
struct ResultVariantRow {
    variant: &'static str,
    csa_code: &'static str,
    end_reason: &'static str,
}

#[test]
fn result_codes_contract_matches_committed_manifest() {
    let generated = build_contract_json();
    let committed = include_str!("../contracts/result-codes.json");

    assert_eq!(generated, committed);
    for reason in [
        IllegalReason::Generic,
        IllegalReason::Uchifuzume,
        IllegalReason::IllegalKachi,
    ] {
        assert_eq!(
            primary_result_code(&GameResult::IllegalMove {
                loser: Color::Black,
                reason
            }),
            "#ILLEGAL_MOVE"
        );
    }
    assert_eq!(
        primary_result_code(&GameResult::Abnormal {
            winner: Some(Color::Black)
        }),
        "#ABNORMAL"
    );
}

fn build_contract_json() -> String {
    let contract = ResultCodesContract {
        variants: variant_rows(),
        csa_outcome_codes: csa_outcome_codes(),
        result_kinds: result_kinds(),
    };

    let mut json = serde_json::to_string_pretty(&contract).expect("contract serializes to JSON");
    json.push('\n');
    json
}

fn variant_rows() -> Vec<ResultVariantRow> {
    [
        GameResult::Toryo {
            winner: Color::Black,
        },
        GameResult::TimeUp {
            loser: Color::Black,
        },
        GameResult::IllegalMove {
            loser: Color::Black,
            reason: IllegalReason::Generic,
        },
        GameResult::Kachi {
            winner: Color::Black,
        },
        GameResult::OuteSennichite {
            loser: Color::Black,
        },
        GameResult::Sennichite,
        GameResult::MaxMoves,
        GameResult::Abnormal { winner: None },
    ]
    .iter()
    .map(result_variant_row)
    .collect()
}

fn result_variant_row(result: &GameResult) -> ResultVariantRow {
    let variant = match result {
        GameResult::Toryo { .. } => "Toryo",
        GameResult::TimeUp { .. } => "TimeUp",
        GameResult::IllegalMove { .. } => "IllegalMove",
        GameResult::Kachi { .. } => "Kachi",
        GameResult::OuteSennichite { .. } => "OuteSennichite",
        GameResult::Sennichite => "Sennichite",
        GameResult::MaxMoves => "MaxMoves",
        GameResult::Abnormal { .. } => "Abnormal",
    };
    let (_, end_reason) = classify_result(result);

    ResultVariantRow {
        variant,
        csa_code: primary_result_code(result),
        end_reason,
    }
}

fn csa_outcome_codes() -> Vec<&'static str> {
    let mut codes = Vec::new();
    for result in outcome_representative_results() {
        let reason_code = primary_result_code(&result);
        for (_, lines) in result.server_messages().sends {
            for line in lines {
                if line != reason_code && !codes.contains(&line.as_str()) {
                    codes.push(outcome_code_name(&line));
                }
            }
        }
    }
    codes
}

fn outcome_representative_results() -> Vec<GameResult> {
    vec![
        GameResult::Toryo {
            winner: Color::Black,
        },
        GameResult::TimeUp {
            loser: Color::Black,
        },
        GameResult::IllegalMove {
            loser: Color::Black,
            reason: IllegalReason::Generic,
        },
        GameResult::Kachi {
            winner: Color::Black,
        },
        GameResult::OuteSennichite {
            loser: Color::Black,
        },
        GameResult::Sennichite,
        GameResult::MaxMoves,
        GameResult::Abnormal {
            winner: Some(Color::Black),
        },
        GameResult::Abnormal { winner: None },
    ]
}

fn outcome_code_name(code: &str) -> &'static str {
    match code {
        "#WIN" => "#WIN",
        "#LOSE" => "#LOSE",
        "#DRAW" => "#DRAW",
        "#CENSORED" => "#CENSORED",
        other => panic!("unknown CSA outcome code: {other}"),
    }
}

fn result_kinds() -> Vec<&'static str> {
    let mut kinds = Vec::new();
    for result in result_kind_representative_results() {
        let (kind, _) = classify_result(&result);
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }
    kinds
}

fn result_kind_representative_results() -> Vec<GameResult> {
    // 先頭 4 件で committed JSON の順序 (WIN_BLACK, WIN_WHITE, DRAW, ABORT) を固定し、
    // 続けて全 variant(両色を含む outcome 代表値)を回す。これにより、いずれかの
    // variant が将来新しい result_kind を生むようになれば集合が増えて byte 不一致で
    // 検知できる(4 代表値だけだと非代表 variant 経由の新 kind を取りこぼす)。
    let mut results = vec![
        GameResult::Toryo {
            winner: Color::Black,
        },
        GameResult::Toryo {
            winner: Color::White,
        },
        GameResult::Sennichite,
        GameResult::Abnormal { winner: None },
    ];
    results.extend(outcome_representative_results());
    results
}
