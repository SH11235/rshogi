use std::fs;
use std::process::Command;

fn assert_failure(output: std::process::Output) {
    assert!(
        !output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn dedup_and_jsonl_conversion_preserve_hardlinked_inputs() {
    for binary in [
        env!("CARGO_BIN_EXE_psv_dedup"),
        env!("CARGO_BIN_EXE_psv_dedup_bloom"),
        env!("CARGO_BIN_EXE_jsonl_to_psv"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input.bin");
        let output = dir.path().join("output.bin");
        let bytes = [7; 40];
        fs::write(&input, bytes).unwrap();
        fs::hard_link(&input, &output).unwrap();
        assert_failure(
            Command::new(binary)
                .arg("--input")
                .arg(&input)
                .arg("--output")
                .arg(&output)
                .output()
                .unwrap(),
        );
        assert_eq!(fs::read(input).unwrap(), bytes);
    }
}

#[test]
fn bloom_rejects_nan_before_creating_output() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.bin");
    let output = dir.path().join("output.bin");
    fs::write(&input, [7; 40]).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_psv_dedup_bloom"))
        .arg("--input")
        .arg(input)
        .arg("--output")
        .arg(&output)
        .args(["--fpr", "NaN"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&result.stderr).contains("--fpr"));
    assert_failure(result);
    assert!(!output.exists());
}

#[test]
fn rescore_hcpe_protects_resources_from_every_output_suffix() {
    for suffix in [
        "",
        ".tmp",
        ".meta",
        ".meta.tmp",
        ".tmp.meta",
        ".tmp.meta.tmp",
    ] {
        for resource_kind in ["input", "nnue", "coeff", "params"] {
            let dir = tempfile::tempdir().unwrap();
            let input = dir.path().join("source.hcpe");
            let model = dir.path().join("model.bin");
            let coeff = dir.path().join("coeff.bin");
            let params = dir.path().join("params.txt");
            for path in [&input, &model, &coeff, &params] {
                fs::write(path, [7; 38]).unwrap();
            }
            let output_dir = dir.path().join("out");
            fs::create_dir(&output_dir).unwrap();
            let target = output_dir.join(format!("source.hcpe{suffix}"));
            let resource = match resource_kind {
                "input" => &input,
                "nnue" => &model,
                "coeff" => &coeff,
                _ => &params,
            };
            fs::hard_link(resource, &target).unwrap();
            let result = Command::new(env!("CARGO_BIN_EXE_rescore_hcpe"))
                .arg("--in")
                .arg(&input)
                .arg("--out-dir")
                .arg(&output_dir)
                .arg("--nnue")
                .arg(&model)
                .arg("--ls-progress-coeff")
                .arg(&coeff)
                .arg("--spsa-params")
                .arg(&params)
                .output()
                .unwrap();
            let stderr = String::from_utf8_lossy(&result.stderr);
            assert!(stderr.contains("Output path"), "{suffix} {resource_kind}: {stderr}");
            assert_failure(result);
            for path in [&input, &model, &coeff, &params] {
                assert_eq!(fs::read(path).unwrap(), [7; 38]);
            }
        }
    }
}

#[test]
fn partition_dedup_rejects_output_inside_partitions_before_writing() {
    let dir = tempfile::tempdir().unwrap();
    let input_dir = dir.path().join("input");
    fs::create_dir(&input_dir).unwrap();
    let partition = input_dir.join("partition_00000.bin");
    fs::write(&partition, [7; 40]).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_psv_dedup_partition"))
        .args(["--dedup-only", "--force"])
        .arg("--temp-dir")
        .arg(dir.path())
        .arg("--output")
        .arg(&partition)
        .output()
        .unwrap();
    assert_failure(result);
    assert_eq!(fs::read(partition).unwrap(), [7; 40]);
}

#[test]
fn split_preserves_input_when_existing_shard_is_hardlinked() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.bin");
    let shard = dir.path().join("train_000.bin");
    fs::write(&input, [7; 80]).unwrap();
    fs::hard_link(&input, &shard).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_split_psv"))
        .arg("--input")
        .arg(&input)
        .arg("--output-prefix")
        .arg(dir.path().join("train"))
        .args(["--records-per-file", "1"])
        .output()
        .unwrap();
    assert_failure(result);
    assert_eq!(fs::read(input).unwrap(), [7; 80]);
}

#[test]
fn score_fix_preview_handles_full_i16_difference() {
    let dir = tempfile::tempdir().unwrap();
    let original = dir.path().join("original.psv");
    let preprocessed = dir.path().join("preprocessed.psv");
    let mut bytes = [0; 40];
    bytes[32..34].copy_from_slice(&i16::MIN.to_le_bytes());
    fs::write(&original, bytes).unwrap();
    bytes[32..34].copy_from_slice(&i16::MAX.to_le_bytes());
    fs::write(&preprocessed, bytes).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_fix_scores"))
        .arg("--original")
        .arg(original)
        .arg("--preprocessed")
        .arg(preprocessed)
        .output()
        .unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    assert!(String::from_utf8_lossy(&result.stderr).contains("65535"));
}

#[test]
fn dedup_tools_reject_incomplete_records_before_changing_output() {
    for binary in [
        env!("CARGO_BIN_EXE_psv_dedup"),
        env!("CARGO_BIN_EXE_psv_dedup_bloom"),
        env!("CARGO_BIN_EXE_psv_dedup_check"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input.bin");
        let output = dir.path().join("output.bin");
        fs::write(&input, [7; 41]).unwrap();
        fs::write(&output, b"existing output").unwrap();
        let mut command = Command::new(binary);
        if binary == env!("CARGO_BIN_EXE_psv_dedup_check") {
            command.arg("--data").arg(&input);
        } else {
            command.arg("--input").arg(&input).arg("--output").arg(&output);
        }
        let result = command.output().unwrap();
        assert!(String::from_utf8_lossy(&result.stderr).contains("multiple of 40"));
        assert_failure(result);
        assert_eq!(fs::read(input).unwrap(), [7; 41]);
        assert_eq!(fs::read(output).unwrap(), b"existing output");
    }
}

#[test]
fn bloom_rejects_incomplete_reference_before_changing_output() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.bin");
    let reference = dir.path().join("reference.bin");
    let output = dir.path().join("output.bin");
    fs::write(&input, [1; 40]).unwrap();
    fs::write(&reference, [7; 41]).unwrap();
    fs::write(&output, b"existing output").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_psv_dedup_bloom"))
        .arg("--input")
        .arg(input)
        .arg("--reference")
        .arg(&reference)
        .arg("--output")
        .arg(&output)
        .output()
        .unwrap();
    assert_failure(result);
    assert_eq!(fs::read(reference).unwrap(), [7; 41]);
    assert_eq!(fs::read(output).unwrap(), b"existing output");
}

#[test]
fn partition_dedup_preserves_input_inside_stale_reference_directory() {
    let dir = tempfile::tempdir().unwrap();
    let reference_dir = dir.path().join("ref");
    fs::create_dir(&reference_dir).unwrap();
    let input = reference_dir.join("source.bin");
    fs::write(&input, [7; 40]).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_psv_dedup_partition"))
        .arg("--input")
        .arg(&input)
        .arg("--temp-dir")
        .arg(dir.path())
        .arg("--output")
        .arg(dir.path().join("out.bin"))
        .args(["--partitions", "1", "--force"])
        .output()
        .unwrap();
    assert_failure(result);
    assert_eq!(fs::read(input).unwrap(), [7; 40]);
}

#[test]
fn jsonl_conversion_handles_many_inputs_and_protects_hardlinks() {
    let dir = tempfile::tempdir().unwrap();
    let inputs = dir.path().join("inputs");
    fs::create_dir(&inputs).unwrap();
    for index in 0..160 {
        fs::write(inputs.join(format!("{index:05}.jsonl")), b"\n").unwrap();
    }
    let run = |output: &std::path::Path| {
        #[cfg(target_os = "linux")]
        let mut command = {
            let mut command = Command::new("bash");
            command.args([
                "-c",
                "ulimit -n 128 && exec \"$@\"",
                "limited-jsonl-conversion",
            ]);
            command.arg(env!("CARGO_BIN_EXE_jsonl_to_psv"));
            command
        };
        #[cfg(not(target_os = "linux"))]
        let mut command = Command::new(env!("CARGO_BIN_EXE_jsonl_to_psv"));
        command
            .arg("--input-dir")
            .arg(&inputs)
            .arg("--output")
            .arg(output)
            .output()
            .unwrap()
    };
    let output = dir.path().join("output.psv");
    let result = run(&output);
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    assert!(
        String::from_utf8_lossy(&result.stdout)
            .lines()
            .any(|line| line.starts_with("Files:") && line.trim_end().ends_with("160"))
    );
    assert!(fs::read(output).unwrap().is_empty());
    let alias = dir.path().join("alias.psv");
    let source = inputs.join("00000.jsonl");
    fs::hard_link(&source, &alias).unwrap();
    let rejected = run(&alias);
    assert!(
        String::from_utf8_lossy(&rejected.stderr).contains("Output path resolves to input file")
    );
    assert_failure(rejected);
    assert_eq!(fs::read(source).unwrap(), b"\n");
}
