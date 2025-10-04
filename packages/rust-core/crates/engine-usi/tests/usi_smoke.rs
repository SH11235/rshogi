use assert_cmd::Command;

#[test]
fn profile_toggles_emit_pruning_notes() {
    let mut cmd = Command::cargo_bin("engine-usi").expect("binary available");
    let script = r#"usi
isready
setoption name EngineType value Enhanced
setoption name SearchParams.EnableIID value true
setoption name SearchParams.EnableProbCut value true
setoption name SearchParams.EnableNMP value true
setoption name SearchParams.EnableRazor value true
isready
position startpos
go depth 4
quit
"#;

    let output = cmd.write_stdin(script).assert().success().get_output().stdout.clone();
    let text = String::from_utf8_lossy(&output);

    assert!(text.contains("pruning_note=IID"), "IID note missing: {}", text);
    assert!(text.contains("pruning_note=ProbCut"), "ProbCut note missing: {}", text);
    assert!(
        !text.contains("pruning_note=NMP"),
        "NMP note should not be emitted when profile allows it: {}",
        text
    );
    // Razor may be enabled by profile; ensure no unexpected warning when allowed
}

#[test]
fn threads_hash_stop_finalize_once() {
    let mut cmd = Command::cargo_bin("engine-usi").expect("binary available");
    let script = r#"usi
isready
setoption name EngineType value EnhancedNnue
setoption name Threads value 2
setoption name USI_Hash value 128
isready
position startpos
go movetime 200
stop
quit
"#;

    let output = cmd.write_stdin(script).assert().success().get_output().stdout.clone();
    let text = String::from_utf8_lossy(&output);

    assert!(text.contains("threads_note"), "threads note missing: {}", text);
    let bestmove_count = text.match_indices("bestmove").count();
    assert_eq!(bestmove_count, 1, "bestmove emitted {} times: {}", bestmove_count, text);
}

#[test]
fn go_single_legal_move_emits_each_time() {
    let mut cmd = Command::cargo_bin("engine-usi").expect("binary available");
    let script = r#"usi
isready
usinewgame
setoption name Ponder value false
position sfen 9/9/9/9/9/9/9/3srs3/4K4 b - 1
go
isready
go
quit
"#;

    let output = cmd.write_stdin(script).assert().success().get_output().stdout.clone();
    let text = String::from_utf8_lossy(&output);

    let bestmove_hits = text.match_indices("bestmove 5i5h").count();
    assert_eq!(bestmove_hits, 2, "expected two bestmove 5i5h outputs: {}", text);
}
