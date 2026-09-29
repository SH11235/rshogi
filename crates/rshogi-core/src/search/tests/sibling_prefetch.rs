use crate::eval::{MaterialLevel, eval_hash_enabled, set_eval_hash_enabled, set_material_level};
use crate::position::Position;
use crate::position::playout_test_support::{PERFT_MATSURI, PERFT_MIDGAME};
use crate::search::{LimitsType, Search, SearchInfo, set_tt_sibling_prefetch};

#[test]
fn sibling_prefetch_preserves_fixed_depth_results() {
    let _guard = crate::eval::material::test_support::lock_material();
    set_material_level(MaterialLevel::Lv1);
    struct RestoreSettings(bool);
    impl Drop for RestoreSettings {
        fn drop(&mut self) {
            set_eval_hash_enabled(self.0);
            set_tt_sibling_prefetch(false);
        }
    }
    let _restore = RestoreSettings(eval_hash_enabled());
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            let mut worker = crate::search::SearchWorker::new(
                std::sync::Arc::new(crate::tt::TranspositionTable::new(1)),
                std::sync::Arc::new(crate::eval::EvalHash::new(0)),
                0,
                0,
                crate::search::SearchTuneParams::default(),
            );
            for enabled in [false, true, false] {
                set_tt_sibling_prefetch(enabled);
                worker.prepare_search(&LimitsType::default());
                assert_eq!(worker.state.tt_sibling_prefetch, enabled);
                set_tt_sibling_prefetch(!enabled);
                assert_eq!(worker.state.tt_sibling_prefetch, enabled);
            }
            drop(worker);
            for use_eval_hash in [false, true] {
                set_eval_hash_enabled(use_eval_hash);
                for sfen in [
                    crate::position::SFEN_HIRATE,
                    PERFT_MATSURI,
                    PERFT_MIDGAME,
                    "k8/9/9/9/9/9/9/4r4/4K4 b - 1",
                ] {
                    let mut baseline = None;
                    for enabled in [false, true] {
                        set_tt_sibling_prefetch(enabled);
                        let mut search = Search::new_with_eval_hash(4, 1);
                        let mut pos = Position::new();
                        pos.set_sfen(sfen).unwrap();
                        let mut iterations = Vec::new();
                        let result = search.go(
                            &mut pos,
                            LimitsType {
                                depth: 4,
                                ..Default::default()
                            },
                            Some(|info: &SearchInfo| {
                                iterations.push((
                                    info.depth,
                                    info.nodes,
                                    info.score,
                                    info.pv.clone(),
                                ));
                            }),
                        );
                        assert_eq!(result.depth, 4);
                        let observed =
                            (result.nodes, result.score, result.pv, result.best_move, iterations);
                        if let Some(expected) = baseline.as_ref() {
                            assert_eq!(&observed, expected, "{sfen}, EvalHash={use_eval_hash}");
                        } else {
                            baseline = Some(observed);
                        }
                    }
                }
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
