use assert_cmd::Command;
use regex::Regex;

fn run_script(script: &str) -> String {
    let mut cmd = Command::cargo_bin("engine-usi").expect("binary available");
    let output = cmd
        .write_stdin(script)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    String::from_utf8_lossy(&output).to_string()
}

#[test]
fn qa_profile_emits_root_guard_metrics() {
    let script = r#"usi
isready
setoption name LogProfile value QA
setoption name RootSafeScan.LogDetail value Full
position startpos moves 7g7f 3c3d 2g2f
go depth 1
quit
"#;
    let output = run_script(script);

    let switch_line = Regex::new(r"root_escape\.switch_count=\d+ guard_skip_count=\d+ safe_zero_count=\d+")
        .unwrap();
    let verify_line =
        Regex::new(r"root_verify\.candidates=\d+ fail_count=\d+ opp_mate_in_one_hits=\d+").unwrap();
    let rescue_line = Regex::new(r"finalize_rescue\.invocations=\d+").unwrap();
    let pv_line = Regex::new(r"pv_stability\.iterations=\d+").unwrap();
    let safe_scan_line = Regex::new(r"safe_scan budget_ms=\d+ .*checked=").unwrap();
    let instant_summary =
        Regex::new(r"instant_mate summary enabled=\d checked=\d forced=\d").unwrap();
    let mate_gate_summary =
        Regex::new(r"mate_gate summary checked=\d blocked=\d instant_override=\d reason=").unwrap();
    let post_verify_summary =
        Regex::new(r"post_verify summary enabled=\d checked=\d reject=\d skip_reason=").unwrap();

    assert!(
        switch_line.is_match(&output),
        "missing root_escape metrics line: {output}"
    );
    assert!(
        verify_line.is_match(&output),
        "missing root_verify metrics line: {output}"
    );
    assert!(
        rescue_line.is_match(&output),
        "missing finalize_rescue metrics line: {output}"
    );
    assert!(
        pv_line.is_match(&output),
        "missing pv_stability metrics line: {output}"
    );
    assert!(
        safe_scan_line.is_match(&output),
        "missing safe_scan summary: {output}"
    );
    assert!(
        instant_summary.is_match(&output),
        "missing instant_mate summary: {output}"
    );
    assert!(
        mate_gate_summary.is_match(&output),
        "missing mate_gate summary: {output}"
    );
    assert!(
        post_verify_summary.is_match(&output),
        "missing post_verify summary: {output}"
    );
}
