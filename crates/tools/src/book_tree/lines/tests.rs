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
            .trace(keys[1].clone(), BTreeSet::new(), Follow::FirstRead, None, 0)
            .unwrap();
        assert_eq!(
            (trace.reason, trace.moves.len(), trace.level, trace.value),
            (reason, moves, Some(level), Some(value))
        );
        assert_eq!(trace.key, keys[level]);
    }
}

#[test]
fn game_uses_saved_rank_and_reports_unexpanded_winner() {
    let (dir, args, keys) = completed(10);
    let mut report = Report::load(&args).unwrap();
    let (_, game, _) = report.tables(&dir.path().join("paths.record.tsv")).unwrap();
    assert_eq!(game.lines().nth(1).unwrap(), "1\t1\t5i4i\t0\t1\tunexpanded\t0\t0\t\t0");
    assert_eq!(game.lines().count(), 2);

    let first = report.nodes.get_mut(&keys[1]).unwrap();
    assert_eq!(first.reading.as_ref().unwrap().multipv[0].mv, "5a4a");
    first.moves.get_mut("5a6a").unwrap().rank = 2;
    first.moves.get_mut("5a4a").unwrap().rank = 1;
    let (_, trace) = report
        .trace_game_move(&keys[0], "5i4i", &keys[1], &BTreeSet::from([keys[0].clone()]))
        .unwrap();
    assert_eq!(trace.moves.first().map(String::as_str), Some("5a4a"));
}

#[test]
fn follows_shallower_merge_and_reports_return_to_path() {
    let (_dir, args, keys) = completed(10);
    let mut report = Report::load(&args).unwrap();
    report.nodes.get_mut(&keys[2]).unwrap().level = Some(1);
    let trace = report
        .trace(keys[1].clone(), BTreeSet::new(), Follow::FirstRead, None, 0)
        .unwrap();
    assert_eq!((trace.reason, trace.merges), (Reason::Cap, 1));

    let trace = report
        .trace(keys[1].clone(), BTreeSet::from([keys[2].clone()]), Follow::FirstRead, None, 0)
        .unwrap();
    assert_eq!(
        (trace.reason, trace.moves.as_slice(), trace.key, trace.value, trace.merges),
        (
            Reason::Repetition,
            ["5a4a".to_owned()].as_slice(),
            keys[2].clone(),
            Some(-17),
            0
        )
    );
}

#[test]
fn expanded_edge_with_missing_read_is_distinct_from_unexpanded() {
    let (dir, args, keys) = completed(10);
    fs::remove_file(dir.path().join("d2.jsonl")).unwrap();
    let report = Report::load(&args).unwrap();
    let trace = report
        .trace(keys[1].clone(), BTreeSet::new(), Follow::FirstRead, None, 0)
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
fn extension_detects_a_cycle_without_a_supplied_path() {
    let (_dir, args, keys) = completed(10);
    let mut report = Report::load(&args).unwrap();
    let return_key = child(&keys[2], "4i5i").unwrap();
    let mut return_reading = report.nodes[&keys[3]].reading.clone().unwrap();
    return_reading.key.clone_from(&return_key);
    return_reading.bestmove = "4a5a".into();
    return_reading.multipv[0].mv = "4a5a".into();
    report.nodes.insert(
        return_key,
        Node {
            level: Some(3),
            reading: Some(return_reading),
            moves: BTreeMap::new(),
        },
    );
    let reading = report.nodes.get_mut(&keys[2]).unwrap().reading.as_mut().unwrap();
    reading.bestmove = "4i5i".into();
    reading.multipv[0].mv = "4i5i".into();
    let trace = report
        .trace(keys[1].clone(), BTreeSet::new(), Follow::FirstRead, None, 0)
        .unwrap();
    assert_eq!(trace.reason, Reason::Repetition);
    assert_eq!(trace.key, keys[1]);
    assert_eq!(trace.moves, ["5a4a", "4i5i", "4a5a", "5i4i"]);
    assert_eq!(trace.value, Some(17));
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
        let mut reading = report.nodes[&keys[0]].reading.clone().unwrap();
        reading.key = key.to_owned();
        reading.bestmove = bestmove.into();
        reading.multipv.clear();
        report.nodes.insert(
            key.to_owned(),
            Node {
                level: Some(1),
                reading: Some(reading),
                moves: BTreeMap::new(),
            },
        );
        let trace = report
            .trace(key.to_owned(), BTreeSet::new(), Follow::FirstRead, None, 0)
            .unwrap();
        assert_eq!(trace.reason, reason);
    }
}
