#[cfg(test)]
mod tests {
    use rshogi_core::position::Position;
    use rshogi_core::search::*;
    static EVAL_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    use rshogi_core::eval::material::{MaterialLevel, set_material_level};

    fn session(threads: usize, multi_pv: usize) -> AnalysisSession {
        let mut search = Search::new_with_eval_hash(4, 1);
        search.set_num_threads(threads);
        let mut position = Position::new();
        position.set_hirate();
        AnalysisSession::new(search, position, multi_pv).unwrap()
    }

    #[test]
    fn resume_keeps_completed_result_and_rejects_stale_stop() {
        let _guard = EVAL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_material_level(MaterialLevel::Lv1);
        for threads in [1, 2, 4, 30] {
            for multi_pv in [1, 3] {
                let mut session = session(threads, multi_pv);
                let first = session.prepare(AnalysisBudget::Depth(3)).unwrap();
                let result = session.run(&first, None::<fn(&SearchInfo)>).unwrap();
                assert_eq!(result.result.depth, 3);
                let pv = result.result.pv.clone();
                let second = session.prepare(AnalysisBudget::Depth(5)).unwrap();
                assert!(!first.pause());
                assert!(second.pause());
                assert!(second.pause());
                let result = session
                    .run(&second, Some(|_: &SearchInfo| panic!("paused before start emitted info")))
                    .unwrap();
                assert_eq!(result.result.depth, 3);
                assert_eq!(result.result.pv, pv);
                assert_eq!(result.result.nodes, 0);
                let third = session.prepare(AnalysisBudget::Depth(5)).unwrap();
                assert!(!second.pause());
                let mut depths = Vec::new();
                let result =
                    session.run(&third, Some(|info: &SearchInfo| depths.push(info.depth))).unwrap();
                assert_eq!(result.result.depth, 5);
                assert_eq!(result.worker_depths.len(), threads);
                assert!(result.worker_depths.iter().all(|(before, after)| after >= before));
                assert!(depths.iter().all(|&d| d > 3));
                assert_eq!(depths.first(), Some(&4));
                assert!(session.run(&third, None::<fn(&SearchInfo)>).is_err());
            }
        }
    }

    #[test]
    fn node_budget_is_shared_and_incremental() {
        let _guard = EVAL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_material_level(MaterialLevel::Lv1);
        for threads in [1, 2, 4, 30] {
            let mut session = session(threads, 3);
            let mut cumulative = 0;
            for _ in 0..3 {
                let run = session.prepare(AnalysisBudget::Nodes(1000)).unwrap();
                let finished = session.run(&run, None::<fn(&SearchInfo)>).unwrap();
                assert!(finished.result.nodes >= 1000);
                assert!(
                    finished.result.nodes < 1000 + threads as u64 * 1024,
                    "nodes={} threads={threads}",
                    finished.result.nodes
                );
                cumulative += finished.result.nodes;
                assert_eq!(finished.total_nodes, cumulative);
            }
        }
    }

    #[test]
    fn rejected_progress_weights_keep_session_resumable() {
        use rshogi_core::nnue::{
            SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS, reset_layer_stack_progress_kpabs_q16_weights,
            reset_layer_stack_progress_kpabs_weights, set_layer_stack_progress_kpabs_q16_weights,
            set_layer_stack_progress_kpabs_weights,
        };

        let _guard = EVAL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_material_level(MaterialLevel::Lv1);
        for q16 in [false, true] {
            let mut session = session(1, 1);
            let first = session.prepare(AnalysisBudget::Depth(1)).unwrap();
            session.run(&first, None::<fn(&SearchInfo)>).unwrap();

            // 入力検証に失敗しただけなら、完了済み探索を失効させない。
            let invalid = if q16 {
                set_layer_stack_progress_kpabs_q16_weights(Vec::new().into_boxed_slice())
            } else {
                set_layer_stack_progress_kpabs_weights(Vec::new().into_boxed_slice())
            };
            assert!(invalid.is_err());
            let next = session.prepare(AnalysisBudget::Depth(2)).unwrap();
            assert_eq!(session.run(&next, None::<fn(&SearchInfo)>).unwrap().result.depth, 2);

            // 有効な設定変更では、従来どおり再開を拒否する。
            if q16 {
                set_layer_stack_progress_kpabs_q16_weights(
                    vec![0; SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS].into_boxed_slice(),
                )
                .unwrap();
            } else {
                set_layer_stack_progress_kpabs_weights(
                    vec![0.0; SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS].into_boxed_slice(),
                )
                .unwrap();
            }
            assert!(matches!(
                session.prepare(AnalysisBudget::Depth(3)),
                Err(AnalysisError::EvaluationChanged)
            ));
            reset_layer_stack_progress_kpabs_q16_weights();
            reset_layer_stack_progress_kpabs_weights();
        }
    }

    #[test]
    fn changed_evaluator_and_foreign_run_are_rejected() {
        let _guard = EVAL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_material_level(MaterialLevel::Lv1);
        let mut a = session(1, 1);
        let mut b = session(1, 1);
        let run = a.prepare(AnalysisBudget::Depth(1)).unwrap();
        let foreign = b.prepare(AnalysisBudget::Depth(1)).unwrap();
        assert!(matches!(
            a.run(&foreign, None::<fn(&SearchInfo)>),
            Err(AnalysisError::InvalidRun)
        ));
        assert!(matches!(a.prepare(AnalysisBudget::Depth(1)), Err(AnalysisError::RunPending)));
        set_material_level(MaterialLevel::Lv1);
        assert!(matches!(
            a.run(&run, None::<fn(&SearchInfo)>),
            Err(AnalysisError::EvaluationChanged)
        ));
        assert!(matches!(
            a.prepare(AnalysisBudget::Depth(1)),
            Err(AnalysisError::EvaluationChanged)
        ));
    }
    #[test]
    fn interrupted_iteration_keeps_previous_pv_and_can_resume_from_zero() {
        let _guard = EVAL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_material_level(MaterialLevel::Lv1);
        for threads in [1, 4] {
            let mut session = session(threads, 3);
            let first = session.prepare(AnalysisBudget::Depth(3)).unwrap();
            first.pause();
            assert_eq!(session.run(&first, None::<fn(&SearchInfo)>).unwrap().result.depth, 0);
            let next = session.prepare(AnalysisBudget::Depth(3)).unwrap();
            let original = session.run(&next, None::<fn(&SearchInfo)>).unwrap();
            for _ in 0..3 {
                let tiny = session.prepare(AnalysisBudget::Nodes(1)).unwrap();
                let paused = session
                    .run(&tiny, Some(|_: &SearchInfo| panic!("incomplete iteration emitted info")))
                    .unwrap();
                assert_eq!(paused.reason, AnalysisEndReason::BudgetReached);
                assert_eq!(paused.result.depth, 3);
                assert_eq!(paused.result.pv, original.result.pv);
                assert_eq!(paused.result.score, original.result.score);
            }
            let next = session.prepare(AnalysisBudget::Depth(4)).unwrap();
            assert_eq!(session.run(&next, None::<fn(&SearchInfo)>).unwrap().result.depth, 4);
        }
    }

    #[test]
    fn callback_panic_stops_helpers_and_invalidates_session() {
        let _guard = EVAL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_material_level(MaterialLevel::Lv1);
        let mut session = session(4, 1);
        let run = session.prepare(AnalysisBudget::Unlimited).unwrap();
        assert!(matches!(
            session.run(&run, Some(|_: &SearchInfo| panic!("injected callback failure"))),
            Err(AnalysisError::RunFailed)
        ));
        assert!(matches!(
            session.prepare(AnalysisBudget::Unlimited),
            Err(AnalysisError::RunFailed)
        ));
    }

    #[test]
    fn stop_during_search_and_added_time_budget() {
        let _guard = EVAL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_material_level(MaterialLevel::Lv1);
        let mut session = session(4, 1);
        let run = session.prepare(AnalysisBudget::Unlimited).unwrap();
        let paused = session
            .run(
                &run,
                Some(|_: &SearchInfo| {
                    run.pause();
                }),
            )
            .unwrap();
        assert_eq!(paused.reason, AnalysisEndReason::Paused);
        assert!(paused.resumable);
        let timed = session.prepare(AnalysisBudget::Time(25)).unwrap();
        let finished = session.run(&timed, None::<fn(&SearchInfo)>).unwrap();
        assert_eq!(finished.reason, AnalysisEndReason::BudgetReached);
        assert!(finished.time_ms >= 25);
        assert_eq!(finished.total_time_ms, paused.time_ms + finished.time_ms);
    }

    fn hirate_after(moves: &[&str]) -> Position {
        let mut pos = Position::new();
        pos.set_hirate();
        for usi in moves {
            let mv = pos.to_move(rshogi_core::types::Move::from_usi(usi).unwrap()).unwrap();
            pos.do_move(mv, pos.gives_check(mv));
        }
        pos
    }

    /// 深さ `depth` まで読み、最初の info の置換表の使用率（千分率）を返す。
    fn first_hashfull(session: &mut AnalysisSession, depth: i32) -> u32 {
        let run = session.prepare(AnalysisBudget::Depth(depth)).unwrap();
        let mut first = None;
        session
            .run(
                &run,
                Some(|info: &SearchInfo| {
                    first.get_or_insert(info.hashfull);
                }),
            )
            .unwrap();
        first.unwrap()
    }

    fn small_session(threads: usize) -> AnalysisSession {
        let mut search = Search::new_with_eval_hash(1, 1);
        search.set_num_threads(threads);
        AnalysisSession::new(search, hirate_after(&[]), 3).unwrap()
    }

    #[test]
    fn set_position_keeps_tables_and_invalidates_old_runs() {
        let _guard = EVAL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_material_level(MaterialLevel::Lv1);
        for threads in [1, 2] {
            let mut session = small_session(threads);
            assert!(first_hashfull(&mut session, 10) < 50);
            let old_run = session.prepare(AnalysisBudget::Depth(11)).unwrap();

            session.set_position(hirate_after(&["7g7f"]), 2).unwrap();
            let run = session.prepare(AnalysisBudget::Depth(5)).unwrap();
            assert!(run.run_id() > old_run.run_id(), "run ids continue across positions");
            assert_eq!(
                session.run(&old_run, None::<fn(&SearchInfo)>).err(),
                Some(AnalysisError::InvalidRun)
            );
            assert!(!old_run.pause(), "a stale handle must not stop the new run");
            let mut hashfull = None;
            let finished = session
                .run(
                    &run,
                    Some(|info: &SearchInfo| {
                        hashfull.get_or_insert(info.hashfull);
                    }),
                )
                .unwrap();
            // 前の局面で埋めた置換表が残っているので、最初の info から使用率が高い。
            assert!(hashfull.unwrap() >= 100, "hashfull {hashfull:?}");
            assert_eq!(finished.result.depth, 5);
            assert_eq!(finished.total_nodes, finished.result.nodes);
            assert_eq!(finished.worker_depths.len(), threads);
            assert!(finished.worker_depths.iter().all(|&(before, _)| before == 0));
        }
    }

    #[test]
    fn set_position_clears_tables_after_evaluation_change() {
        let _guard = EVAL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_material_level(MaterialLevel::Lv1);
        let mut session = small_session(1);
        first_hashfull(&mut session, 10);
        assert_eq!(
            session.set_position(hirate_after(&["7g7f"]), 0),
            Err(AnalysisError::InvalidOptions)
        );
        // 評価関数を設定し直すと世代が進み、続きも作り直しも古い置換表を使わない。
        set_material_level(MaterialLevel::Lv1);
        assert_eq!(
            session.prepare(AnalysisBudget::Depth(5)).err(),
            Some(AnalysisError::EvaluationChanged)
        );
        session.set_position(hirate_after(&["7g7f"]), 1).unwrap();
        assert!(first_hashfull(&mut session, 5) < 50);
    }
}
