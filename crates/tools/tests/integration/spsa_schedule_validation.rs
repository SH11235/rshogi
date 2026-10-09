use std::process::Command;

#[test]
fn invalid_cli_and_derived_schedules_fail_before_engine_start() {
    let dir = tempfile::tempdir().unwrap();
    let params = dir.path().join("input.params");
    std::fs::write(&params, "X,int,10,0,100,2,0.5\n").unwrap();
    let engine = dir.path().join("engine-must-not-be-started");
    std::fs::write(&engine, "fixture: must fail before process spawn").unwrap();
    let marker = dir.path().join("engine-started");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let script = format!(
            r#"#!/bin/sh
echo started >> '{}'
while IFS= read -r line; do
 case "$line" in
 usi) echo usiok ;;
 isready) echo readyok ;;
 go*) echo 'bestmove resign' ;;
 quit) exit 0 ;;
 esac
done
"#,
            marker.display()
        );
        std::fs::write(&engine, script).unwrap();
        std::fs::set_permissions(&engine, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut cases = Vec::new();
    for name in [
        "alpha",
        "gamma",
        "a-ratio",
        "mobility",
        "early-stop-avg-abs-update-threshold",
        "early-stop-result-variance-threshold",
    ] {
        for value in ["NaN", "inf", "-inf"] {
            cases.push((format!("--{name}={value}"), "must be finite"));
        }
    }
    for name in ["alpha", "gamma", "a-ratio", "mobility"] {
        cases.push((format!("--{name}=1e308"), "bound"));
    }
    for (index, (arg, expected)) in cases.iter().enumerate() {
        let out = Command::new(env!("CARGO_BIN_EXE_spsa"))
            .arg("--engine-path")
            .arg(&engine)
            .arg("--run-dir")
            .arg(dir.path().join(format!("run-{index}")))
            .arg("--init-from")
            .arg(&params)
            .args(["--total-pairs", "4", "--batch-pairs", "2"])
            .arg(arg)
            .output()
            .unwrap();
        assert!(!out.status.success(), "{arg}");
        let error = String::from_utf8_lossy(&out.stderr);
        assert!(
            error.contains(expected)
                || (*expected == "bound" && error.contains("schedule constants overflow")),
            "{arg}: {error}"
        );
        assert!(!error.contains("failed to spawn"), "{arg}: {error}");
        assert!(!marker.exists(), "{arg}: engine started before rejection");
    }
    #[cfg(unix)]
    {
        let normal = Command::new(env!("CARGO_BIN_EXE_spsa"))
            .arg("--engine-path")
            .arg(&engine)
            .arg("--run-dir")
            .arg(dir.path().join("normal"))
            .arg("--init-from")
            .arg(&params)
            .args(["--total-pairs", "1", "--batch-pairs", "1"])
            .output()
            .unwrap();
        assert!(normal.status.success(), "{}", String::from_utf8_lossy(&normal.stderr));
        assert!(marker.exists(), "normal control must start the same mock engine");
    }
}
