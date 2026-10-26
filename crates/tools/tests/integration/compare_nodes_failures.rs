use std::process::Command;

#[test]
fn missing_engine_makes_run_fail_in_both_processing_modes() {
    for reuse in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_compare_nodes"));
        command
            .args(["--sfen", "startpos", "--workers", "1"])
            .arg("--config")
            .arg(dir.path().join("missing.toml"))
            .arg("--engine-a")
            .arg(dir.path().join("missing-engine"))
            .arg("--engine-b")
            .arg(dir.path().join("missing-engine"))
            .arg("--output-base")
            .arg(dir.path().join("out"));
        if reuse {
            command.arg("--reuse-engine");
        }
        let output = command.output().unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("0/1"));
    }
}
