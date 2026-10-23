use std::fs;
use std::process::Command;

#[test]
fn rescore_rejects_later_input_alias_before_loading_model_or_writing_outputs() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first.psv");
    let second = dir.path().join("second.psv");
    let model = dir.path().join("model.nnue");
    let output_dir = dir.path().join("out");
    fs::create_dir(&output_dir).unwrap();
    fs::write(&first, [1; 40]).unwrap();
    fs::write(&second, [2; 40]).unwrap();
    fs::write(&model, b"model placeholder").unwrap();
    let first_output = output_dir.join("first.psv");
    fs::write(&first_output, b"existing output").unwrap();
    fs::hard_link(&first, output_dir.join("second.psv")).unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_rescore_psv"))
        .arg("--input").arg(&first).arg(&second)
        .arg("--nnue").arg(&model)
        .arg("--output-dir").arg(&output_dir)
        .output().unwrap();
    let error = String::from_utf8_lossy(&result.stderr);
    assert!(!result.status.success());
    assert!(error.contains("Output path resolves to input file"), "{error}");
    assert_eq!(fs::read(first).unwrap(), [1; 40]);
    assert_eq!(fs::read(second).unwrap(), [2; 40]);
    assert_eq!(fs::read(model).unwrap(), b"model placeholder");
    assert_eq!(fs::read(first_output).unwrap(), b"existing output");
}
