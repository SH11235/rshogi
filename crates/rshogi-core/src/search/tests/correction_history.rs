//! correction value / correction history の tune param で、(ss-6) を使わない式を再現できること。
use crate::position::Position;
use crate::search::{LimitsType, Search, SearchInfo, SearchTuneParams};

fn on_large_stack(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}

/// (ss-2)/(ss-4) だけを参照し、前の手が無いときは `8 * cnt` を足す式 (YaneuraOu と同じ形) の値。
pub(super) fn correction_params_ss2_ss4_only() -> SearchTuneParams {
    SearchTuneParams {
        correction_value_pcv_weight: 9536,
        correction_value_micv_weight: 8494,
        correction_value_nonpawn_weight: 10132,
        correction_value_cnt_weight: 7156,
        correction_value_cnt_ss6_weight: 0,
        correction_value_cnt_no_move: 8 * 7156,
        correction_history_non_pawn_weight: 165,
        correction_history_minor_piece_mult: 156,
        correction_history_cont_ss2_weight: 137,
        correction_history_cont_ss4_weight: 64,
        correction_history_cont_ss6_weight: 0,
        ..SearchTuneParams::default()
    }
}

const SFENS: [&str; 3] = [
    "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
    "ln1gk2nl/1rs1g2b1/pppppp1pp/6p2/9/2P1P4/PP1P1PPPP/1B2G2R1/LNS1KGSNL b - 1",
    "ln1gkg1nl/1rs3sb1/p1pppp1pp/1p4p2/9/2PP5/PPS1PPPPP/1BG4R1/LN2KGSNL w - 1",
];

const DEPTH: i32 = 16;

/// 深さ固定・1 スレッドで探索し、(nodes, bestmove) を返す。
fn search(sfen: &str, params: SearchTuneParams) -> (u64, String) {
    let mut pos = Position::new();
    pos.set_sfen(sfen).unwrap();
    let mut search = Search::new(16);
    search.set_search_tune_params(params);
    let result = search.go(
        &mut pos,
        LimitsType {
            depth: DEPTH,
            ..Default::default()
        },
        None::<fn(&SearchInfo)>,
    );
    (result.nodes, result.best_move.to_usi())
}

/// material 評価 Lv1 で、(ss-2)/(ss-4) だけの式を `correction_value` に直書きしていた探索が返す値。
/// use-lazy-evaluate は TT の eval を再利用するため 2 局面目の nodes が変わる。
fn expected_ss2_ss4_only() -> Vec<(u64, String)> {
    let second = if cfg!(feature = "use-lazy-evaluate") {
        645_120
    } else {
        798_790
    };
    vec![
        (869_122, "3i4h".to_string()),
        (second, "8h2b+".to_string()),
        (418_329, "1c1d".to_string()),
    ]
}

#[test]
fn ss2_ss4_only_params_reproduce_hardcoded_formula() {
    let _guard = crate::eval::material::test_support::lock_material();
    crate::eval::set_material_level(crate::eval::MaterialLevel::Lv1);
    on_large_stack(|| {
        let base = correction_params_ss2_ss4_only();
        let got: Vec<_> = SFENS.iter().map(|sfen| search(sfen, base)).collect();
        assert_eq!(got, expected_ss2_ss4_only());

        // 新しい 3 param がそれぞれ単独で探索に効く (参照・更新・定数項の経路が生きている)。
        let toggles = [
            SearchTuneParams {
                correction_value_cnt_ss6_weight: 6307,
                ..base
            },
            SearchTuneParams {
                correction_history_cont_ss6_weight: 35,
                ..base
            },
            SearchTuneParams {
                correction_value_cnt_no_move: 80695,
                ..base
            },
        ];
        for params in toggles {
            assert_ne!(search(SFENS[0], params), got[0]);
        }
    });
}
