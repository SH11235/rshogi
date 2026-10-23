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
        .arg("--input")
        .arg(&first)
        .arg(&second)
        .arg("--nnue")
        .arg(&model)
        .arg("--output-dir")
        .arg(&output_dir)
        .output()
        .unwrap();
    let error = String::from_utf8_lossy(&result.stderr);
    assert!(!result.status.success());
    assert!(error.contains("Output path resolves to input file"), "{error}");
    assert_eq!(fs::read(first).unwrap(), [1; 40]);
    assert_eq!(fs::read(second).unwrap(), [2; 40]);
    assert_eq!(fs::read(model).unwrap(), b"model placeholder");
    assert_eq!(fs::read(first_output).unwrap(), b"existing output");
}

#[cfg(windows)]
#[test]
fn rescore_case_only_later_outputs_leave_first_existing_output_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let input_dir_a = dir.path().join("a");
    let input_dir_b = dir.path().join("b");
    let output_dir = dir.path().join("out");
    for path in [&input_dir_a, &input_dir_b, &output_dir] {
        fs::create_dir(path).unwrap();
    }
    let first = dir.path().join("first.psv");
    let second = input_dir_a.join("New.psv");
    let third = input_dir_b.join("new.psv");
    let model = dir.path().join("model.nnue");
    for path in [&first, &second, &third, &model] {
        fs::write(path, [7; 40]).unwrap();
    }
    let existing = output_dir.join("first.psv");
    fs::write(&existing, b"previous output").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_rescore_psv"))
        .arg("--input")
        .arg(&first)
        .arg(&second)
        .arg(&third)
        .arg("--nnue")
        .arg(&model)
        .arg("--output-dir")
        .arg(&output_dir)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("differ only in case"));
    assert_eq!(fs::read(existing).unwrap(), b"previous output");
    for path in [&first, &second, &third, &model] {
        assert_eq!(fs::read(path).unwrap(), [7; 40]);
    }
    assert!(!output_dir.join("New.psv").exists());
    assert!(!output_dir.join("new.psv").exists());
}

#[test]
fn rescore_rejects_ambiguous_resource_whitespace_before_engine_start_or_output_write() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.psv");
    let engine = dir.path().join("engine");
    let resource = dir.path().join("tune params");
    let output_dir = dir.path().join("out");
    fs::create_dir(&output_dir).unwrap();
    for path in [&input, &engine, &resource] {
        fs::write(path, [7; 40]).unwrap();
    }
    let output = output_dir.join("input.psv");
    fs::hard_link(&resource, &output).unwrap();
    for separator in ["  ", "\t", "\n"] {
        let result = Command::new(env!("CARGO_BIN_EXE_rescore_psv"))
            .arg("--input")
            .arg(&input)
            .arg("--engine")
            .arg(&engine)
            .arg("--output-dir")
            .arg(&output_dir)
            .arg("--usi-option")
            .arg(format!("SPSAParamsFile=tune{separator}params"))
            .output()
            .unwrap();
        let error = String::from_utf8_lossy(&result.stderr);
        assert!(!result.status.success());
        assert!(error.contains("Ambiguous whitespace in USI resource option"), "{error}");
        for path in [&input, &engine, &resource, &output] {
            assert_eq!(fs::read(path).unwrap(), [7; 40]);
        }
    }
}

#[test]
fn rescore_rejects_reserved_usi_tokens_before_engine_start_or_output_write() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.psv");
    let engine = dir.path().join("engine");
    let resource = dir.path().join("spsa.params");
    let output_dir = dir.path().join("out");
    fs::create_dir(&output_dir).unwrap();
    for path in [&input, &engine, &resource] {
        fs::write(path, [7; 40]).unwrap();
    }
    let output = output_dir.join("input.psv");
    fs::hard_link(&resource, &output).unwrap();
    for option in [
        "SPSAParamsFile=value",
        "SPSAParamsFile=tune value params",
        "SPSAParamsFile=tune name params",
        "SPSAParamsFile value=tune params",
        "SPSAParamsFile name=tune params",
        "BookDir=books VALUE other",
        "Unrecognized=NAME",
    ] {
        let result = Command::new(env!("CARGO_BIN_EXE_rescore_psv"))
            .arg("--input")
            .arg(&input)
            .arg("--engine")
            .arg(&engine)
            .arg("--output-dir")
            .arg(&output_dir)
            .arg("--usi-option")
            .arg(option)
            .output()
            .unwrap();
        let error = String::from_utf8_lossy(&result.stderr);
        assert!(!result.status.success());
        assert!(error.contains("Reserved USI token in option name or value"), "{error}");
        for path in [&input, &engine, &resource, &output] {
            assert_eq!(fs::read(path).unwrap(), [7; 40]);
        }
    }
}
