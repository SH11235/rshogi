use super::*;

struct Constant {
    value: f32,
    fail: bool,
}
impl Evaluator for Constant {
    fn input_format(&self) -> Option<rshogi_dl::InputFormat> {
        Some(rshogi_dl::InputFormat::Basic)
    }

    fn evaluate(&mut self, _: &Position, logits: &mut [f32]) -> Result<f32> {
        anyhow::ensure!(!self.fail, "evaluation failed");
        logits.fill(0.0);
        Ok(self.value)
    }
}

fn root() -> (Position, History) {
    let mut pos = Position::new();
    pos.set_hirate();
    let history = History::new(&pos);
    (pos, history)
}

#[test]
fn backups_change_perspective_and_restore_history() -> Result<()> {
    let (mut pos, mut history) = root();
    let original = pos.to_sfen();
    let mut search = Search::new(Config::default())?;
    let result = search.run(
        &mut pos,
        &mut history,
        &mut Constant {
            value: 0.2,
            fail: false,
        },
        Limits {
            visits: 2,
            time: None,
        },
        &AtomicBool::new(false),
    )?;
    assert_eq!(result.visits, 2);
    assert_eq!(result.evaluations, 2);
    assert!((result.value - 0.5).abs() < 1e-6);
    assert_eq!(search.edges.iter().map(|e| e.visits).sum::<u64>(), 1);
    assert!((search.edges.iter().map(|e| e.value_sum).sum::<f64>() - 0.8).abs() < 1e-6);
    assert_eq!(pos.to_sfen(), original);
    assert_eq!(history.len(), 1);
    Ok(())
}

#[test]
fn zero_visits_and_stop_need_no_inference() -> Result<()> {
    let (mut pos, mut history) = root();
    let mut search = Search::new(Config::default())?;
    for (visits, stop) in [(0, false), (100, true)] {
        let result = search.run(
            &mut pos,
            &mut history,
            &mut Constant {
                value: 0.5,
                fail: true,
            },
            Limits { visits, time: None },
            &AtomicBool::new(stop),
        )?;
        assert_eq!(result.visits, 0);
        assert_ne!(result.best_move, Move::NONE);
    }
    Ok(())
}

#[test]
fn evaluation_error_restores_position() -> Result<()> {
    let (mut pos, mut history) = root();
    let original = pos.to_sfen();
    struct FailsSecond(usize);
    impl Evaluator for FailsSecond {
        fn evaluate(&mut self, _: &Position, logits: &mut [f32]) -> Result<f32> {
            self.0 += 1;
            logits.fill(0.0);
            anyhow::ensure!(self.0 < 2, "failure");
            Ok(0.5)
        }
    }
    let mut search = Search::new(Config::default())?;
    assert!(
        search
            .run(
                &mut pos,
                &mut history,
                &mut FailsSecond(0),
                Limits {
                    visits: 10,
                    time: None
                },
                &AtomicBool::new(false)
            )
            .is_err()
    );
    assert_eq!(pos.to_sfen(), original);
    assert_eq!(history.len(), 1);
    Ok(())
}

#[test]
fn fourfold_repetition_uses_complete_history() -> Result<()> {
    let mut pos = Position::new();
    pos.set_sfen("4k4/9/9/9/9/9/9/9/4K4 b - 1")?;
    let mut history = History::new(&pos);
    for round in 0..3 {
        for usi in ["5i6i", "5a6a", "6i5i", "6a5a"] {
            let mv = pos.to_move(Move::from_usi(usi).unwrap()).unwrap();
            pos.do_move(mv, pos.gives_check(mv));
            history.push(&pos);
        }
        assert_eq!(
            history.outcome(),
            if round == 2 {
                Some(Outcome::Draw)
            } else {
                None
            }
        );
    }
    let result = Search::new(Config::default())?.run(
        &mut pos,
        &mut history,
        &mut Constant {
            value: 0.5,
            fail: true,
        },
        Limits {
            visits: 10,
            time: None,
        },
        &AtomicBool::new(false),
    )?;
    assert_eq!(result.outcome, Some(Outcome::Draw));
    assert_eq!(result.evaluations, 0);
    Ok(())
}

fn play(pos: &mut Position, history: &mut History, text: &str) {
    let mv = pos.to_move(Move::from_usi(text).unwrap()).unwrap();
    let mut legal = MoveList::new();
    generate_legal_all(pos, &mut legal);
    assert!(legal.contains(mv), "illegal {text}");
    pos.do_move(mv, pos.gives_check(mv));
    history.push(pos);
}

#[test]
fn repetition_beyond_short_search_window_is_still_terminal() -> Result<()> {
    let mut pos = Position::new();
    pos.set_sfen("4k4/9/9/9/9/9/9/9/4K4 b - 1")?;
    let mut history = History::new(&pos);
    let cycle = [
        "5i6i", "5a6a", "6i7i", "6a7a", "7i8i", "7a8a", "8i9i", "8a9a", "9i9h", "9a9b", "9h8h",
        "9b8b", "8h7h", "8b7b", "7h6h", "7b6b", "6h5h", "6b5b", "5h5i", "5b5a",
    ];
    for _ in 0..3 {
        for text in cycle {
            play(&mut pos, &mut history, text);
        }
    }
    assert_eq!(pos.repetition_state(0), rshogi_core::types::RepetitionState::None);
    assert_eq!(history.outcome(), Some(Outcome::Draw));
    Ok(())
}

#[test]
fn continuous_check_repetition_loses_for_checking_side() -> Result<()> {
    let mut pos = Position::new();
    pos.set_sfen("4k4/5R3/9/9/9/9/9/9/K8 b - 1")?;
    let mut history = History::new(&pos);
    for _ in 0..3 {
        for text in ["4b5b", "5a4a", "5b4b", "4a5a"] {
            play(&mut pos, &mut history, text);
        }
    }
    assert_eq!(history.outcome(), Some(Outcome::Loss));
    Ok(())
}

#[test]
fn mate_is_propagated_and_root_declaration_needs_no_inference() -> Result<()> {
    let mut pos = Position::new();
    pos.set_sfen("7Pk/6R2/9/9/9/9/9/9/4K4 b G 1")?;
    let mut history = History::new(&pos);
    struct MatePolicy(usize);
    impl Evaluator for MatePolicy {
        fn evaluate(&mut self, _: &Position, logits: &mut [f32]) -> Result<f32> {
            logits.fill(-100.0);
            logits[self.0] = 100.0;
            Ok(0.5)
        }
    }
    let mate_move = pos.to_move(Move::from_usi("G*1b").unwrap()).unwrap();
    let label = make_move_label(mate_move, pos.side_to_move());
    let mut search = Search::new(Config::default())?;
    let result = search.run(
        &mut pos,
        &mut history,
        &mut MatePolicy(label),
        Limits {
            visits: 10,
            time: None,
        },
        &AtomicBool::new(false),
    )?;
    assert_eq!(result.outcome, Some(Outcome::Win));
    assert_eq!(result.best_move, mate_move);
    assert_eq!(result.visits, 2);
    assert_eq!(result.evaluations, 1);
    pos.set_sfen("+B+N4R+Pl/KBG2g2l/P+P2P+N3/6p2/2p3s2/4S2L1/G8/7p+l/7+pk b G2S4Pr2n6p 1")?;
    let mut history = History::new(&pos);
    let result = search.run(
        &mut pos,
        &mut history,
        &mut Constant {
            value: 0.5,
            fail: true,
        },
        Limits {
            visits: 10,
            time: None,
        },
        &AtomicBool::new(false),
    )?;
    assert_eq!(result.best_move, Move::WIN);
    assert_eq!(result.evaluations, 0);
    Ok(())
}

#[test]
fn full_arena_returns_completed_work_and_restores_root() -> Result<()> {
    let (mut pos, mut history) = root();
    let original = pos.to_sfen();
    let mut search = Search::new(Config::default())?;
    search.node_limit = 1;
    let result = search.run(
        &mut pos,
        &mut history,
        &mut Constant {
            value: 0.5,
            fail: false,
        },
        Limits {
            visits: 1000,
            time: None,
        },
        &AtomicBool::new(false),
    )?;
    assert!(result.memory_full);
    assert_eq!(result.visits, 1);
    assert_ne!(result.best_move, Move::NONE);
    assert_eq!(pos.to_sfen(), original);
    Ok(())
}

#[test]
fn try_rule_returns_the_winning_king_move_even_without_visits() -> Result<()> {
    let mut pos = Position::new();
    pos.set_sfen("3K5/9/9/9/9/9/9/9/4k4 b 2r2b4g4s4n4l18p 1")?;
    let mut history = History::new(&pos);
    let config = Config {
        entering_king_rule: EnteringKingRule::TryRule,
        ..Config::default()
    };
    let mut search = Search::new(config)?;
    for visits in [0, 16] {
        let result = search.run(
            &mut pos,
            &mut history,
            &mut Constant {
                value: 0.5,
                fail: true,
            },
            Limits { visits, time: None },
            &AtomicBool::new(false),
        )?;
        assert_eq!(result.best_move.to_usi(), "6a5a");
        assert_eq!(result.outcome, Some(Outcome::Win));
        assert_eq!(result.evaluations, 0);
    }
    Ok(())
}

#[test]
fn try_rule_win_is_also_recognized_inside_the_tree() -> Result<()> {
    let mut pos = Position::new();
    pos.set_sfen("9/9/9/9/9/9/9/3k5/K8 b - 1")?;
    let mut history = History::new(&pos);
    let config = Config {
        entering_king_rule: EnteringKingRule::TryRule,
        ..Config::default()
    };
    let mut search = Search::new(config)?;
    let result = search.run(
        &mut pos,
        &mut history,
        &mut Constant {
            value: 0.5,
            fail: false,
        },
        Limits {
            visits: 100,
            time: None,
        },
        &AtomicBool::new(false),
    )?;
    assert_eq!(result.outcome, Some(Outcome::Loss));
    assert_eq!(result.evaluations, 1);
    for edge in &search.edges {
        let child = &search.nodes[edge.child.unwrap()];
        assert_eq!(child.outcome, Some(Outcome::Win));
        assert_eq!(child.declaration_move.to_usi(), "6h5i");
    }
    Ok(())
}

#[test]
fn root_draw_keeps_a_legal_response_for_external_runners() -> Result<()> {
    let (mut pos, mut history) = root();
    play(&mut pos, &mut history, "7g7f");
    let config = Config {
        max_game_ply: 1,
        ..Config::default()
    };
    let mut search = Search::new(config)?;
    let result = search.run(
        &mut pos,
        &mut history,
        &mut Constant {
            value: 0.5,
            fail: true,
        },
        Limits {
            visits: 0,
            time: None,
        },
        &AtomicBool::new(false),
    )?;
    let mut legal = MoveList::new();
    generate_legal_all(&pos, &mut legal);
    assert_eq!(result.outcome, Some(Outcome::Draw));
    assert!(legal.contains(result.best_move));
    assert_eq!(result.evaluations, 0);
    Ok(())
}

#[test]
fn raw_cache_preserves_fixed_visit_search_and_history_terminals() -> Result<()> {
    use rshogi_dl::cache::CachedEvaluator;
    let (mut pos, mut history) = root();
    let mut search = Search::new(Config::default())?;
    let limits = Limits {
        visits: 128,
        time: None,
    };
    let stop = AtomicBool::new(false);
    let baseline = search.run(
        &mut pos,
        &mut history,
        &mut Constant {
            value: 0.3,
            fail: false,
        },
        limits,
        &stop,
    )?;
    let mut cached = CachedEvaluator::new(
        Constant {
            value: 0.3,
            fail: false,
        },
        4,
    )?;
    for _ in 0..2 {
        let result = search.run(&mut pos, &mut history, &mut cached, limits, &stop)?;
        assert_eq!(result.best_move, baseline.best_move);
        assert_eq!(result.pv, baseline.pv);
        assert_eq!(result.value.to_bits(), baseline.value.to_bits());
        assert_eq!(result.visits, baseline.visits);
        assert_eq!(result.evaluations, baseline.evaluations);
        assert_eq!(result.hashfull, baseline.hashfull);
    }
    assert!(cached.stats().hits > 0);
    let mut terminal = Search::new(Config {
        max_game_ply: 1,
        ..Config::default()
    })?;
    play(&mut pos, &mut history, "7g7f");
    cached.reset_stats();
    let result = terminal.run(&mut pos, &mut history, &mut cached, limits, &stop)?;
    assert_eq!(result.outcome, Some(Outcome::Draw));
    assert_eq!(cached.stats().requests, 0);
    Ok(())
}

struct BatchedConstant<'a> {
    format: rshogi_dl::InputFormat,
    capacity: Option<usize>,
    scalar_calls: usize,
    batches: Vec<usize>,
    fail_call: Option<usize>,
    invalid_call: Option<usize>,
    stop_after_call: Option<usize>,
    stop: Option<&'a AtomicBool>,
    preferred: Option<(usize, usize)>,
}

fn batched_constant<'a>(capacity: Option<usize>) -> BatchedConstant<'a> {
    BatchedConstant {
        format: rshogi_dl::InputFormat::StmTokensV1,
        capacity,
        scalar_calls: 0,
        batches: Vec::new(),
        fail_call: None,
        invalid_call: None,
        stop_after_call: None,
        stop: None,
        preferred: None,
    }
}

impl BatchedConstant<'_> {
    fn policy(&self, logits: &mut [f32]) {
        if let Some((first, second)) = self.preferred {
            logits.fill(-100.0);
            logits[first] = 0.1;
            logits[second] = 0.0;
        } else {
            logits.fill(0.0);
        }
    }
}

impl Evaluator for BatchedConstant<'_> {
    fn input_format(&self) -> Option<rshogi_dl::InputFormat> {
        Some(self.format)
    }

    fn encoded_batch_capacity(&self) -> Option<usize> {
        self.capacity
    }

    fn evaluate(&mut self, _: &Position, logits: &mut [f32]) -> Result<f32> {
        self.scalar_calls += 1;
        self.policy(logits);
        Ok(0.3)
    }

    fn evaluate_encoded_batch(
        &mut self,
        inputs: rshogi_dl::EncodedBatch<'_>,
        values: &mut [f32],
        logits: &mut [f32],
    ) -> Result<()> {
        assert_eq!(inputs.format(), self.format);
        assert!(inputs.len() <= self.capacity.expect("batch support was declared"));
        assert_eq!(values.len(), inputs.len());
        assert_eq!(logits.len(), inputs.len() * MAX_MOVE_LABEL_NUM);
        self.batches.push(inputs.len());
        ensure!(self.fail_call != Some(self.batches.len()), "batch failure");
        values.fill(0.3);
        for row in logits.chunks_exact_mut(MAX_MOVE_LABEL_NUM) {
            self.policy(row);
        }
        if self.invalid_call == Some(self.batches.len()) {
            *logits.last_mut().expect("batch is not empty") = f32::NAN;
        }
        if self.stop_after_call == Some(self.batches.len()) {
            self.stop.expect("stop flag was provided").store(true, Ordering::Relaxed);
        }
        Ok(())
    }
}

fn same_result(want: &SearchResult, got: &SearchResult) {
    assert_eq!(want.best_move, got.best_move);
    assert_eq!(want.value.to_bits(), got.value.to_bits());
    assert_eq!(want.visits, got.visits);
    assert_eq!(want.evaluations, got.evaluations);
    assert_eq!(want.outcome, got.outcome);
    assert_eq!(want.pv, got.pv);
    assert_eq!(want.hashfull, got.hashfull);
    assert_eq!(want.memory_full, got.memory_full);
}

#[test]
fn batch_one_and_unsupported_evaluators_keep_the_scalar_search_exact() -> Result<()> {
    let (mut pos, mut history) = root();
    let original = pos.to_sfen();
    let stop = AtomicBool::new(false);
    let limits = Limits {
        visits: 128,
        time: None,
    };
    let mut search = Search::new(Config::default())?;
    let mut evaluator = batched_constant(Some(8));
    let baseline = search.run(&mut pos, &mut history, &mut evaluator, limits, &stop)?;
    assert!(evaluator.batches.is_empty());
    assert_eq!(evaluator.scalar_calls, 128);
    assert_eq!(search.batch_workspace_bytes(), 0);
    for capacity in [None, Some(1)] {
        let mut search = Search::new(Config {
            batch_size: 8,
            ..Config::default()
        })?;
        let mut evaluator = batched_constant(capacity);
        let result = search.run(&mut pos, &mut history, &mut evaluator, limits, &stop)?;
        same_result(&baseline, &result);
        assert_eq!(evaluator.scalar_calls, 128);
        assert!(evaluator.batches.is_empty());
        assert_eq!(search.batch_workspace_bytes(), 0);
    }
    assert_eq!(pos.to_sfen(), original);
    assert_eq!(history.len(), 1);
    for batch_size in [0, 1025] {
        assert!(
            Search::new(Config {
                batch_size,
                ..Config::default()
            })
            .is_err()
        );
    }
    Ok(())
}

#[test]
fn batched_visits_are_completed_exactly_and_partial_batches_dispatch() -> Result<()> {
    for visits in [1, 3, 4, 5, 6, 9] {
        let (mut pos, mut history) = root();
        let original = pos.to_sfen();
        let mut search = Search::new(Config {
            batch_size: 4,
            tree_memory_mb: 1,
            ..Config::default()
        })?;
        let mut evaluator = batched_constant(Some(4));
        let result = search.run(
            &mut pos,
            &mut history,
            &mut evaluator,
            Limits { visits, time: None },
            &AtomicBool::new(false),
        )?;
        assert_eq!(result.visits, visits);
        assert_eq!(result.evaluations, visits);
        assert_eq!(evaluator.scalar_calls, 0);
        assert_eq!(evaluator.batches[0], 1);
        assert_eq!(evaluator.batches.iter().sum::<usize>() as u64, visits);
        assert!(evaluator.batches.iter().all(|&count| (1..=4).contains(&count)));
        assert!(search.batch.as_ref().unwrap().is_idle());
        assert!(
            search.batch_workspace_bytes()
                > (search.node_limit + search.edge_limit) * size_of::<u32>()
        );
        assert_eq!(pos.to_sfen(), original);
        assert_eq!(history.len(), 1);
        if visits == 6 {
            assert_eq!(evaluator.batches, vec![1, 4, 1]);
        }
    }
    Ok(())
}

#[test]
fn blocked_children_flush_without_repeating_the_same_pending_expansion() -> Result<()> {
    let (mut pos, mut history) = root();
    let mut search = Search::new(Config {
        batch_size: 64,
        tree_memory_mb: 1,
        ..Config::default()
    })?;
    let mut legal = MoveList::new();
    generate_legal_all(&pos, &mut legal);
    let mut evaluator = batched_constant(Some(64));
    let result = search.run(
        &mut pos,
        &mut history,
        &mut evaluator,
        Limits {
            visits: legal.len() as u64 + 3,
            time: None,
        },
        &AtomicBool::new(false),
    )?;
    assert_eq!(evaluator.batches[0], 1);
    assert_eq!(evaluator.batches[1], legal.len());
    assert_eq!(evaluator.batches[2], 2);
    assert_eq!(result.visits, legal.len() as u64 + 3);
    assert!(search.batch.as_ref().unwrap().is_idle());
    Ok(())
}

#[test]
fn full_batch_validation_and_errors_cancel_all_reservations_before_reuse() -> Result<()> {
    let (mut pos, mut history) = root();
    let original = pos.to_sfen();
    let mut search = Search::new(Config {
        batch_size: 4,
        tree_memory_mb: 1,
        ..Config::default()
    })?;
    let stop = AtomicBool::new(false);
    let limits = Limits {
        visits: 16,
        time: None,
    };
    for invalid in [false, true] {
        let mut evaluator = batched_constant(Some(4));
        if invalid {
            evaluator.invalid_call = Some(2);
        } else {
            evaluator.fail_call = Some(2);
        }
        assert!(search.run(&mut pos, &mut history, &mut evaluator, limits, &stop).is_err());
        assert_eq!(search.nodes[0].visits, 1);
        assert_eq!(search.nodes.iter().filter(|node| node.expanded).count(), 1);
        assert!(search.batch.as_ref().unwrap().is_idle());
        assert_eq!(pos.to_sfen(), original);
        assert_eq!(history.len(), 1);
        let result =
            search.run(&mut pos, &mut history, &mut batched_constant(Some(4)), limits, &stop)?;
        assert_eq!(result.visits, 16);
        assert!(search.batch.as_ref().unwrap().is_idle());
    }
    Ok(())
}

#[test]
fn stop_during_forward_commits_the_valid_completed_batch_and_restores_root() -> Result<()> {
    let (mut pos, mut history) = root();
    let original = pos.to_sfen();
    let stop = AtomicBool::new(false);
    let mut search = Search::new(Config {
        batch_size: 4,
        tree_memory_mb: 1,
        ..Config::default()
    })?;
    let mut evaluator = batched_constant(Some(4));
    evaluator.stop = Some(&stop);
    evaluator.stop_after_call = Some(2);
    let result = search.run(
        &mut pos,
        &mut history,
        &mut evaluator,
        Limits {
            visits: 100,
            time: None,
        },
        &stop,
    )?;
    assert_eq!(evaluator.batches, vec![1, 4]);
    assert_eq!(result.visits, 5);
    assert!(search.batch.as_ref().unwrap().is_idle());
    assert_eq!(pos.to_sfen(), original);
    assert_eq!(history.len(), 1);
    stop.store(false, Ordering::Relaxed);
    let result = search.run(
        &mut pos,
        &mut history,
        &mut batched_constant(Some(4)),
        Limits {
            visits: 3,
            time: None,
        },
        &stop,
    )?;
    assert_eq!(result.visits, 3);
    Ok(())
}

#[test]
fn pending_edge_reservations_respect_arena_limit_and_commit_a_partial_batch() -> Result<()> {
    let (mut pos, mut history) = root();
    let original = pos.to_sfen();
    let mut search = Search::new(Config {
        batch_size: 4,
        tree_memory_mb: 1,
        ..Config::default()
    })?;
    search.edge_limit = 80;
    let mut evaluator = batched_constant(Some(4));
    let result = search.run(
        &mut pos,
        &mut history,
        &mut evaluator,
        Limits {
            visits: 100,
            time: None,
        },
        &AtomicBool::new(false),
    )?;
    assert!(result.memory_full);
    assert_eq!(result.visits, 2);
    assert_eq!(evaluator.batches, vec![1, 1]);
    assert!(search.edges.len() <= search.edge_limit);
    assert!(search.batch.as_ref().unwrap().is_idle());
    assert_eq!(pos.to_sfen(), original);
    assert_eq!(history.len(), 1);
    Ok(())
}

#[test]
fn terminal_after_pending_neural_leaf_is_backed_up_once_in_selection_order() -> Result<()> {
    let mut pos = Position::new();
    pos.set_sfen("7Pk/6R2/9/9/9/9/9/9/4K4 b G 1")?;
    let mut history = History::new(&pos);
    let original = pos.to_sfen();
    let first = pos.to_move(Move::from_usi("5i6i").unwrap()).unwrap();
    let mate = pos.to_move(Move::from_usi("G*1b").unwrap()).unwrap();
    let mut evaluator = batched_constant(Some(4));
    evaluator.preferred = Some((
        make_move_label(first, pos.side_to_move()),
        make_move_label(mate, pos.side_to_move()),
    ));
    let mut search = Search::new(Config {
        batch_size: 4,
        ..Config::default()
    })?;
    let result = search.run(
        &mut pos,
        &mut history,
        &mut evaluator,
        Limits {
            visits: 100,
            time: None,
        },
        &AtomicBool::new(false),
    )?;
    assert_eq!(result.outcome, Some(Outcome::Win));
    assert_eq!(result.best_move, mate);
    assert_eq!(result.visits, 3);
    assert_eq!(result.evaluations, 2);
    assert_eq!(evaluator.batches, vec![1, 1]);
    assert_eq!(
        search
            .nodes
            .iter()
            .filter(|node| node.outcome == Some(Outcome::Loss))
            .map(|node| node.visits)
            .sum::<u64>(),
        1
    );
    assert!(search.batch.as_ref().unwrap().is_idle());
    assert_eq!(pos.to_sfen(), original);
    assert_eq!(history.len(), 1);
    Ok(())
}

#[test]
fn batch_workspace_is_rebuilt_for_format_and_capacity_changes() -> Result<()> {
    let (mut pos, mut history) = root();
    let mut search = Search::new(Config {
        batch_size: 8,
        tree_memory_mb: 1,
        ..Config::default()
    })?;
    let mut evaluator = batched_constant(Some(4));
    let limits = Limits {
        visits: 3,
        time: None,
    };
    let stop = AtomicBool::new(false);
    search.run(&mut pos, &mut history, &mut evaluator, limits, &stop)?;
    let first_bytes = search.batch_workspace_bytes();
    assert!(search.batch.as_ref().unwrap().matches(rshogi_dl::InputFormat::StmTokensV1, 4));
    evaluator.format = rshogi_dl::InputFormat::Basic;
    evaluator.capacity = Some(2);
    search.run(&mut pos, &mut history, &mut evaluator, limits, &stop)?;
    assert!(search.batch.as_ref().unwrap().matches(rshogi_dl::InputFormat::Basic, 2));
    assert!(search.batch_workspace_bytes() > first_bytes);
    assert!(search.batch.as_ref().unwrap().is_idle());
    Ok(())
}

#[test]
fn same_neural_input_with_repetition_history_bypasses_batched_inference() -> Result<()> {
    let mut pos = Position::new();
    pos.set_sfen("4k4/9/9/9/9/9/9/9/4K4 b - 1")?;
    let input = rshogi_dl::tokens::encode(&pos)?;
    let mut history = History::new(&pos);
    let mut evaluator = batched_constant(Some(4));
    let mut search = Search::new(Config {
        batch_size: 4,
        ..Config::default()
    })?;
    let stop = AtomicBool::new(false);
    let limits = Limits {
        visits: 4,
        time: None,
    };
    let first = search.run(&mut pos, &mut history, &mut evaluator, limits, &stop)?;
    assert_eq!(first.visits, 4);
    for _ in 0..3 {
        for usi in ["5i6i", "5a6a", "6i5i", "6a5a"] {
            play(&mut pos, &mut history, usi);
        }
    }
    assert_eq!(input, rshogi_dl::tokens::encode(&pos)?);
    evaluator.batches.clear();
    evaluator.scalar_calls = 0;
    let result = search.run(&mut pos, &mut history, &mut evaluator, limits, &stop)?;
    assert_eq!(result.outcome, Some(Outcome::Draw));
    assert_eq!(result.visits, 0);
    assert_eq!(result.evaluations, 0);
    assert!(evaluator.batches.is_empty());
    assert_eq!(evaluator.scalar_calls, 0);
    assert_eq!(history.len(), 13);
    Ok(())
}
