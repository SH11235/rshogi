use std::fs;
use std::path::Path;
use std::process::Command;

fn command(input: &Path, output: &Path, mask: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_preprocess_psv"));
    command
        .arg("--input")
        .arg(input)
        .arg("--output")
        .arg(output)
        .arg("--moved-mask")
        .arg(mask)
        .arg("--threads")
        .arg("1");
    command
}

#[test]
fn all_outputs_protect_input_and_declared_resources_before_model_load() {
    for resource_index in 0..3 {
        for output_index in 0..4 {
            for hardlink in [false, true] {
                let dir = tempfile::tempdir().unwrap();
                let input = dir.path().join("input.psv");
                let nnue = dir.path().join("model.nnue");
                let coeff = dir.path().join("coeff.bin");
                let mut output = dir.path().join("output.psv");
                let mut mask = dir.path().join("mask.bits");
                let resources = [&input, &nnue, &coeff];
                for path in resources {
                    fs::write(path, b"protected bytes").unwrap();
                }
                let resource = resources[resource_index];
                let target = match output_index {
                    0 => output.clone(),
                    1 => mask.clone(),
                    2 => dir.path().join("output.psv.tmp"),
                    _ => dir.path().join("mask.bits.tmp"),
                };
                if hardlink {
                    fs::hard_link(resource, &target).unwrap();
                } else {
                    match output_index {
                        0 => output = resource.clone(),
                        1 => mask = resource.clone(),
                        _ => {
                            fs::rename(resource, &target).unwrap();
                        }
                    }
                }
                let actual_resource = if !hardlink && output_index >= 2 {
                    &target
                } else {
                    resource
                };
                let actual_input = if resource_index == 0 {
                    actual_resource
                } else {
                    &input
                };
                let mut cmd = command(actual_input, &output, &mask);
                cmd.arg("--nnue").arg(if resource_index == 1 {
                    actual_resource
                } else {
                    &nnue
                });
                cmd.arg("--ls-progress-coeff").arg(if resource_index == 2 {
                    actual_resource
                } else {
                    &coeff
                });
                let result = cmd.output().unwrap();
                let stderr = String::from_utf8_lossy(&result.stderr);
                assert!(!result.status.success(), "{stderr}");
                assert!(stderr.contains("Output path"), "{stderr}");
                assert_eq!(fs::read(actual_resource).unwrap(), b"protected bytes");
                if hardlink {
                    assert_eq!(fs::read(target).unwrap(), b"protected bytes");
                }
            }
        }
    }
}

#[cfg(unix)]
#[test]
fn staging_symlinks_do_not_touch_unrelated_or_missing_targets() {
    for missing in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input.psv");
        let output = dir.path().join("output.psv");
        let mask = dir.path().join("mask.bits");
        let staging = dir.path().join("output.psv.tmp");
        let target = dir.path().join("unrelated.bin");
        fs::write(&input, []).unwrap();
        if !missing {
            fs::write(&target, b"untouched").unwrap();
        }
        std::os::unix::fs::symlink(&target, &staging).unwrap();
        let result = command(&input, &output, &mask).output().unwrap();
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains("symlink"));
        assert!(fs::symlink_metadata(staging).unwrap().file_type().is_symlink());
        if missing {
            assert!(!target.exists());
        } else {
            assert_eq!(fs::read(target).unwrap(), b"untouched");
        }
    }
}

#[test]
fn unused_but_declared_coefficients_must_exist() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.psv");
    let output = dir.path().join("output.psv");
    fs::write(&input, []).unwrap();
    fs::write(&output, b"previous output").unwrap();
    let result = command(&input, &output, &dir.path().join("mask.bits"))
        .arg("--ls-progress-coeff")
        .arg(dir.path().join("missing.bin"))
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert_eq!(fs::read(output).unwrap(), b"previous output");
}
