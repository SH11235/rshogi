use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const YARDSTICK: &str = env!("CARGO_BIN_EXE_yardstick_label");
const POSITIONS: &str = env!("CARGO_BIN_EXE_label_bench_positions");
#[cfg(feature = "dlshogi-onnx")]
const DL: &str = env!("CARGO_BIN_EXE_label_bench_dl");
const SCORE: &str = env!("CARGO_BIN_EXE_yardstick_score");

#[derive(Clone, Copy, Debug)]
enum Alias {
    SamePath,
    Hardlink,
    #[cfg(unix)]
    Symlink,
    #[cfg(windows)]
    Case,
}

fn aliases() -> Vec<Alias> {
    vec![
        Alias::SamePath,
        Alias::Hardlink,
        #[cfg(unix)]
        Alias::Symlink,
        #[cfg(windows)]
        Alias::Case,
    ]
}

fn output_alias(dir: &Path, resource: &Path, alias: Alias) -> PathBuf {
    match alias {
        Alias::SamePath => resource.to_owned(),
        Alias::Hardlink => {
            let output = dir.join("output.jsonl");
            fs::hard_link(resource, &output).unwrap();
            output
        }
        #[cfg(unix)]
        Alias::Symlink => {
            let output = dir.join("output.jsonl");
            std::os::unix::fs::symlink(resource, &output).unwrap();
            output
        }
        #[cfg(windows)]
        Alias::Case => {
            let filename = resource.file_name().unwrap().to_str().unwrap().to_ascii_uppercase();
            dir.join(filename)
        }
    }
}

fn resources(dir: &Path) -> [PathBuf; 5] {
    let paths = [
        "input.hcpe",
        "model.nnue",
        "progress.bin",
        "search.params",
        "model.onnx",
    ]
    .map(|name| dir.join(name));
    for (index, path) in paths.iter().enumerate() {
        fs::write(path, vec![index as u8 + 1; 38]).unwrap();
    }
    paths
}

fn assert_guard_rejects(command: &mut Command, preserved: &[&Path]) {
    let original: Vec<_> = preserved.iter().map(|path| fs::read(path).unwrap()).collect();
    let result = command.output().unwrap();
    let error = String::from_utf8_lossy(&result.stderr);
    assert!(!result.status.success(), "unexpected success: {error}");
    assert!(
        error.contains("Output path resolves to input file")
            || error.contains("Output path is a symlink")
            || error.contains("Output paths resolve to the same file"),
        "path guard did not reject before resource loading: {error}"
    );
    for (path, expected) in preserved.iter().zip(original) {
        assert_eq!(fs::read(path).unwrap(), expected, "changed {}", path.display());
    }
}

fn nnue_yardstick(input: &Path, model: &Path, coeff: &Path, params: &Path) -> Command {
    let mut command = Command::new(YARDSTICK);
    command
        .arg("--in")
        .arg(input)
        .arg("--nnue")
        .arg(model)
        .arg("--ls-progress-coeff")
        .arg(coeff)
        .arg("--spsa-params")
        .arg(params);
    command
}

#[test]
fn yardstick_rejects_every_nnue_resource_alias_without_changes() {
    for resource_index in 0..4 {
        for alias in aliases() {
            let dir = tempfile::tempdir().unwrap();
            let paths = resources(dir.path());
            let output = output_alias(dir.path(), &paths[resource_index], alias);
            let mut command = nnue_yardstick(&paths[0], &paths[1], &paths[2], &paths[3]);
            command.arg("--out").arg(output);
            assert_guard_rejects(
                &mut command,
                &paths.iter().map(PathBuf::as_path).collect::<Vec<_>>(),
            );
        }
    }
}

#[test]
fn capture_later_resource_alias_preserves_earlier_output() {
    for resource_index in 0..4 {
        let dir = tempfile::tempdir().unwrap();
        let paths = resources(dir.path());
        let first = dir.path().join("labels_d9.jsonl");
        let second = dir.path().join("labels_d15.jsonl");
        fs::write(&first, b"previous labels").unwrap();
        fs::hard_link(&paths[resource_index], &second).unwrap();
        let mut command = nnue_yardstick(&paths[0], &paths[1], &paths[2], &paths[3]);
        command
            .arg("--out")
            .arg(dir.path().join("labels.jsonl"))
            .args(["--capture-depths", "9,15"]);
        let mut preserved: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
        preserved.extend([first.as_path(), second.as_path()]);
        assert_guard_rejects(&mut command, &preserved);
    }
}

#[test]
fn capture_output_hardlinks_are_rejected_before_resource_loading() {
    let dir = tempfile::tempdir().unwrap();
    let paths = resources(dir.path());
    let first = dir.path().join("labels_d9.jsonl");
    let second = dir.path().join("labels_d15.jsonl");
    fs::write(&first, b"previous labels").unwrap();
    fs::hard_link(&first, &second).unwrap();
    let mut command = nnue_yardstick(&paths[0], &paths[1], &paths[2], &paths[3]);
    command
        .arg("--out")
        .arg(dir.path().join("labels.jsonl"))
        .args(["--capture-depths", "9,15"]);
    assert_guard_rejects(&mut command, &[&paths[0], &paths[1], &first, &second]);
}

#[test]
fn positions_rejects_input_model_and_coeff_aliases() {
    for resource_index in 0..3 {
        for alias in aliases() {
            let dir = tempfile::tempdir().unwrap();
            let paths = resources(dir.path());
            let output = output_alias(dir.path(), &paths[resource_index], alias);
            let mut command = Command::new(POSITIONS);
            command
                .arg("--in")
                .arg(&paths[0])
                .arg("--nnue")
                .arg(&paths[1])
                .arg("--ls-progress-coeff")
                .arg(&paths[2])
                .arg("--out")
                .arg(output);
            assert_guard_rejects(&mut command, &[&paths[0], &paths[1], &paths[2]]);
        }
    }
}

#[cfg(feature = "dlshogi-onnx")]
#[test]
fn onnx_labelers_reject_input_and_model_aliases_before_loading_runtime() {
    for (binary, model_flag) in [(YARDSTICK, "--onnx-model"), (DL, "--dlshogi-onnx-model")] {
        for resource_index in [0, 4] {
            for alias in aliases() {
                let dir = tempfile::tempdir().unwrap();
                let paths = resources(dir.path());
                let output = output_alias(dir.path(), &paths[resource_index], alias);
                let mut command = Command::new(binary);
                command
                    .arg("--in")
                    .arg(&paths[0])
                    .arg(model_flag)
                    .arg(&paths[4])
                    .arg("--out")
                    .arg(output);
                assert_guard_rejects(&mut command, &[&paths[0], &paths[4]]);
            }
        }
    }
}

#[test]
fn score_rejects_alias_to_any_labeled_input() {
    for resource_index in [0, 1] {
        for alias in aliases() {
            let dir = tempfile::tempdir().unwrap();
            let paths = resources(dir.path());
            let output = output_alias(dir.path(), &paths[resource_index], alias);
            let mut command = Command::new(SCORE);
            command.arg(&paths[0]).arg(&paths[1]).arg("--out").arg(output);
            assert_guard_rejects(&mut command, &[&paths[0], &paths[1]]);
        }
    }
}

#[test]
fn score_replaces_distinct_output_and_preserves_input() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("labels.jsonl");
    let output = dir.path().join("report.json");
    let record = br#"{"wdl":1.0,"eval_ref":100,"eval_label":90,"eval_band":"0-150","nyugyoku":"none","in_check":false,"mate_ref":false,"mate_label":false}"#;
    fs::write(&input, record).unwrap();
    fs::write(&output, vec![b'x'; 4096]).unwrap();
    let result = Command::new(SCORE).arg(&input).arg("--out").arg(&output).output().unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    assert_eq!(fs::read(&input).unwrap(), record);
    let report: serde_json::Value = serde_json::from_slice(&fs::read(output).unwrap()).unwrap();
    assert_eq!(report[0]["n_total"], 1);
}
