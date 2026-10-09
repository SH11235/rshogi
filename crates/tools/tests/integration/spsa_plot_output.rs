use std::path::Path;
use std::process::Command;

const HEADER: &str = "iteration,games,plus_wins,minus_wins,draws,step_sum,grad_scale,a_t,c_t,avg_abs_shift,avg_abs_update,max_abs_update,total_games\n";
const ROW: &str = "1,2,1,0,1,1,0.5,1,1,0.1,0.2,0.3,2\n";

fn convert(input: &Path, output: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_spsa_stats_to_plot_csv"))
        .arg(input)
        .arg("--output-csv")
        .arg(output)
        .output()
        .unwrap()
}

#[test]
fn aliases_preserve_input_and_fail_before_conversion() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("stats.csv");
    let original = format!("{HEADER}{}", ROW.repeat(1000));
    std::fs::write(&input, &original).unwrap();
    assert!(!convert(&input, &input).status.success());
    let hard = dir.path().join("hard.csv");
    std::fs::hard_link(&input, &hard).unwrap();
    assert!(!convert(&input, &hard).status.success());
    #[cfg(unix)]
    {
        let link = dir.path().join("link.csv");
        std::os::unix::fs::symlink(&input, &link).unwrap();
        assert!(!convert(&input, &link).status.success());
    }
    assert_eq!(std::fs::read_to_string(&input).unwrap(), original);
}

#[test]
fn only_successful_conversion_replaces_existing_output() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("stats.csv");
    let output = dir.path().join("plot.csv");
    std::fs::write(&output, "previous output").unwrap();
    std::fs::write(&input, format!("{HEADER}{ROW}broken,row\n")).unwrap();
    assert!(!convert(&input, &output).status.success());
    assert_eq!(std::fs::read_to_string(&output).unwrap(), "previous output");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    std::fs::write(&input, format!("{HEADER}{ROW}")).unwrap();
    let result = convert(&input, &output);
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    let data = std::fs::read_to_string(&output).unwrap();
    assert_eq!(data.lines().count(), 2);
    assert!(data.starts_with("iteration,mode,"));
    assert_eq!(std::fs::read_to_string(&input).unwrap(), format!("{HEADER}{ROW}"));
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
}
