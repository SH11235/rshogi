use super::*;
use crate::book_tree::model::prefetch_tests::extended_fixture;

fn completed(stop_value: i32) -> (tempfile::TempDir, LinesArgs, Vec<String>) {
    let dir = tempfile::tempdir().unwrap();
    let (mut tree, mut cache, keys) = extended_fixture(stop_value);
    cache.get_mut(&keys[2]).unwrap().multipv[1].cp = 25;
    tree.def.out = dir.path().to_owned();
    tree.def.rounds = 0;
    tree.def.extend_rounds = 0;
    assert!(tree.advance(&cache).unwrap().is_empty());
    assert!(tree.finished);
    assert!(tree.write_outputs().unwrap());
    fs::write(
        dir.path().join("paths.record.tsv"),
        format!("{}\t1\t5i4i 5a6a\tqueue\n", keys[0]),
    )
    .unwrap();
    let args = LinesArgs {
        dir: dir.path().to_owned(),
        out: None,
        game_out: None,
        depth: None,
        extend: None,
        stop: None,
        paths: None,
    };
    (dir, args, keys)
}

#[test]
fn saved_output_traces_stop_of_either_sign_and_cap() {
    for (cp, reason, moves, level, value) in [
        (300, Reason::Stop, 1, 2, 300),
        (-300, Reason::Stop, 1, 2, -300),
        (10, Reason::Cap, 3, 4, 10),
    ] {
        let (_dir, args, keys) = completed(cp);
        let report = Report::load(&args).unwrap();
        let trace = report
            .trace(
                &report.propagation().unwrap(),
                keys[1].clone(),
                BTreeSet::new(),
                Follow::FirstRead,
                0,
            )
            .unwrap();
        assert_eq!(
            (trace.reason, trace.moves.len(), trace.level, trace.value),
            (reason, moves, Some(level), Some(value))
        );
        assert_eq!(trace.key, keys[level]);
    }
}

#[test]
fn game_reports_moves_past_depth_with_known_single_reads() {
    let (dir, args, _) = completed(10);
    fs::remove_file(dir.path().join("tree.db")).unwrap();
    let report = Report::load(&args).unwrap();
    let (extension, game, _) = report.tables(&dir.path().join("paths.record.tsv")).unwrap();
    let mut rows = game.lines().skip(1);
    assert_eq!(rows.next().unwrap(), "1\t1\t5i4i\t0\t1\tunexpanded\t0\t0\t\t0");
    assert_eq!(rows.next().unwrap(), "1\t2\t5a6a\t0\t\tunexpanded\t0\t0\t\t0");
    assert!(rows.next().is_none());
    assert!(extension.lines().nth(1).unwrap().ends_with("\t0\t1"));
}

#[test]
fn stops_at_shallower_merge_with_its_propagated_value() {
    let (_dir, args, keys) = completed(10);
    let mut report = Report::load(&args).unwrap();
    report.levels.insert(keys[2].clone(), 1);
    let trace = report
        .trace(
            &report.propagation().unwrap(),
            keys[1].clone(),
            BTreeSet::new(),
            Follow::FirstRead,
            0,
        )
        .unwrap();
    assert_eq!(
        (trace.reason, trace.level, trace.value, trace.merges),
        (Reason::Merged, Some(1), Some(25), 1)
    );
    assert_eq!(trace.key, keys[2]);
    assert_eq!(trace.moves, ["5a4a"]);
}

#[test]
fn direct_game_move_at_stop_uses_the_stopping_read() {
    for (cp, reason) in [
        (300, Reason::Stop),
        (-300, Reason::Stop),
        (29900, Reason::Mate),
    ] {
        let (_dir, args, keys) = completed(cp);
        let report = Report::load(&args).unwrap();
        let next = child(&keys[2], "4i4h").unwrap();
        let (value, trace) = report
            .trace_game_move(
                &report.propagation().unwrap(),
                &keys[2],
                "4i4h",
                &next,
                &BTreeSet::from([keys[2].clone()]),
            )
            .unwrap();
        assert_eq!(value, Some(25));
        assert_eq!(
            (trace.reason, trace.key, trace.level, trace.value),
            (reason, keys[2].clone(), Some(2), Some(cp))
        );
    }
}

#[test]
fn unexpanded_game_move_without_a_read_leaves_value_blank() {
    let (_dir, args, keys) = completed(10);
    let report = Report::load(&args).unwrap();
    let next = child(&keys[2], "4i5i").unwrap();
    let (value, trace) = report
        .trace_game_move(
            &report.propagation().unwrap(),
            &keys[2],
            "4i5i",
            &next,
            &BTreeSet::from([keys[2].clone()]),
        )
        .unwrap();
    assert_eq!((value, trace.reason, trace.value), (None, Reason::Unexpanded, None));
}

#[test]
fn expanded_edge_with_missing_read_is_distinct_from_unexpanded() {
    let (dir, args, keys) = completed(10);
    fs::remove_file(dir.path().join("d2.jsonl")).unwrap();
    let report = Report::load(&args).unwrap();
    let trace = report
        .trace(
            &report.propagation().unwrap(),
            keys[1].clone(),
            BTreeSet::new(),
            Follow::FirstRead,
            0,
        )
        .unwrap();
    assert_eq!((trace.reason, trace.level, trace.value), (Reason::MissingRead, Some(2), None));
    assert_eq!(trace.key, keys[2]);
    assert_eq!(trace.moves, ["5a4a"]);
}

#[test]
fn legacy_depth_is_inferred_but_cap_is_explicit() {
    let (dir, mut args, _) = completed(10);
    fs::write(dir.path().join("registration_status.json"), r#"{"ready":true,"unresolved":{}}"#)
        .unwrap();
    assert!(Report::load(&args).is_err());
    args.extend = Some(3);
    let report = Report::load(&args).unwrap();
    assert_eq!((report.limits.depth, report.limits.cap, report.limits.stop), (1, 4, 300));
}

#[test]
fn game_pv_and_direct_repetition_use_the_ancestor_path() {
    let (dir, args, _) = completed(10);
    let (mut tree, mut cache, mut keys) = extended_fixture(10);
    keys[3] = child(&keys[2], "4i5i").unwrap();
    let mut reading = cache[&keys[1]].clone();
    reading.key.clone_from(&keys[3]);
    reading.bestmove = "4a5a".into();
    reading.multipv.truncate(2);
    reading.multipv[0].mv = "4a5a".into();
    reading.multipv[1].mv = "4a3a".into();
    reading.multipv[1].cp = 40;
    cache.insert(keys[3].clone(), reading);
    for (key, mv) in keys[1..3].iter().zip(["5a4a", "4i5i"]) {
        let read = cache.get_mut(key).unwrap();
        read.bestmove = mv.into();
        read.multipv.truncate(1);
        read.multipv[0].mv = mv.into();
    }
    tree.def.out = dir.path().to_owned();
    tree.def.rounds = 0;
    tree.def.extend_rounds = 0;
    tree.def.draw_black = Some(-30);
    tree.def.draw_white = Some(70);
    assert!(tree.advance(&cache).unwrap().is_empty());
    assert!(tree.write_outputs().unwrap());
    let rows = fs::read_to_string(dir.path().join(NODE_MOVES)).unwrap();
    assert!(
        rows.lines()
            .any(|row| row.starts_with(&keys[3]) && row.ends_with("\t4a3a\t40\t0\t1"))
    );

    let report = Report::load(&args).unwrap();
    let propagation = report.propagation().unwrap();
    let (value, trace) = report
        .trace_game_move(
            &propagation,
            &keys[0],
            "5i4i",
            &keys[1],
            &BTreeSet::from([keys[0].clone()]),
        )
        .unwrap();
    assert_eq!(value, Some(-70));
    assert_eq!(trace.reason, Reason::Repetition);
    assert_eq!(trace.key, keys[0]);
    assert_eq!(trace.moves, ["5a4a", "4i5i", "4a5a"]);
    assert_eq!(trace.value, Some(-70));

    let (value, trace) = report
        .trace_game_move(
            &propagation,
            &keys[3],
            "4a5a",
            &keys[0],
            &keys[..4].iter().cloned().collect(),
        )
        .unwrap();
    assert_eq!((value, trace.reason, trace.value), (Some(-70), Reason::Repetition, Some(-70)));
    assert!(trace.moves.is_empty());
}

#[cfg(unix)]
#[test]
fn refuses_outputs_in_the_tree_through_a_symlink() {
    let (dir, mut args, _) = completed(10);
    let outside = tempfile::tempdir().unwrap();
    let alias = outside.path().join("alias");
    std::os::unix::fs::symlink(dir.path(), &alias).unwrap();
    let inside = alias.join("game.tsv");
    args.out = Some(outside.path().join("extension.tsv"));
    args.game_out = Some(inside.clone());
    assert!(
        run(args)
            .unwrap_err()
            .to_string()
            .contains("木のディレクトリ内には出力できません")
    );
    assert!(!inside.exists());
    assert!(!outside.path().join("extension.tsv").exists());
}

#[test]
fn terminal_readings_distinguish_mate_no_legal_declaration_and_empty_read() {
    let (_dir, args, keys) = completed(10);
    let mut report = Report::load(&args).unwrap();
    for (key, bestmove, reason) in [
        ("8k/7G1/7K1/9/9/9/9/9/9 w -", "resign", Reason::Mate),
        ("8k/6G2/7K1/9/9/9/9/9/9 w -", "resign", Reason::NoLegal),
        (keys[1].as_str(), "win", Reason::Declaration),
        (keys[1].as_str(), "resign", Reason::MissingRead),
    ] {
        let mut reading = report.readings[&keys[0]].clone();
        reading.key = key.to_owned();
        reading.bestmove = bestmove.into();
        reading.multipv.clear();
        report.levels.insert(key.to_owned(), 1);
        report.readings.insert(key.to_owned(), reading);
        report.expanded.remove(key);
        let trace = report
            .trace(
                &report.propagation().unwrap(),
                key.to_owned(),
                BTreeSet::new(),
                Follow::FirstRead,
                0,
            )
            .unwrap();
        assert_eq!(trace.reason, reason);
    }
}
