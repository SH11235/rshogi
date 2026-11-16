use super::*;

struct Constant {
    value: f32,
    fail: bool,
}
impl Evaluator for Constant {
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
