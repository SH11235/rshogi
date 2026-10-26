use std::path::Path;
use std::process::Command;

fn row(name: &str, value: i32) -> String {
    format!("{name},int,{value},-2147483648,2147483647,1,0.002\n")
}

fn mapping(path: &Path, rshogi_name: &str, flip: bool) {
    std::fs::write(
        path,
        format!("[[mapping]]\nyo = \"YO_TEST\"\nrshogi = \"{rshogi_name}\"\nsign_flip = {flip}\n"),
    )
    .unwrap();
}

#[test]
fn unrepresentable_sign_flip_fails_without_panicking_or_replacing_output() {
    let dir = tempfile::tempdir().unwrap();
    let rshogi_name = rshogi_core::search::SearchTuneParams::option_specs()[0].usi_name;
    let map = dir.path().join("mapping.toml");
    let yo = dir.path().join("yo.params");
    let rshogi = dir.path().join("rshogi.params");
    let output = dir.path().join("out.params");
    mapping(&map, rshogi_name, true);
    std::fs::write(&yo, row("YO_TEST", i32::MIN)).unwrap();
    std::fs::write(&rshogi, row(rshogi_name, i32::MIN)).unwrap();
    std::fs::write(&output, b"preserve").unwrap();
    for executable in [
        env!("CARGO_BIN_EXE_yo_to_rshogi_params"),
        env!("CARGO_BIN_EXE_rshogi_to_yo_params"),
        env!("CARGO_BIN_EXE_check_param_mapping"),
    ] {
        let mut command = Command::new(executable);
        command.arg("--mapping").arg(&map);
        if executable == env!("CARGO_BIN_EXE_yo_to_rshogi_params") {
            command.arg("--yo-params").arg(&yo).arg("--output").arg(&output);
        } else if executable == env!("CARGO_BIN_EXE_rshogi_to_yo_params") {
            command.arg("--rshogi-params").arg(&rshogi).arg("--output").arg(&output);
        } else {
            command.arg("--yo-params").arg(&yo).arg("--rshogi-params").arg(&rshogi);
        }
        let result = command.output().unwrap();
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(!result.status.success(), "{executable}");
        assert!(stderr.contains("sign-flipped value does not fit i32"), "{stderr}");
        assert!(!stderr.contains("panicked"), "{stderr}");
        assert_eq!(std::fs::read(&output).unwrap(), b"preserve");
    }
}

#[test]
fn automatic_ranges_and_mapping_candidates_accept_i32_extremes() {
    let dir = tempfile::tempdir().unwrap();
    let rshogi_name = rshogi_core::search::SearchTuneParams::option_specs()[0].usi_name;
    let map = dir.path().join("mapping.toml");
    let yo = dir.path().join("yo.params");
    let rshogi = dir.path().join("rshogi.params");
    let output = dir.path().join("out.params");
    mapping(&map, rshogi_name, false);
    for value in [i32::MIN, i32::MAX] {
        std::fs::write(&rshogi, row(rshogi_name, value)).unwrap();
        let result = Command::new(env!("CARGO_BIN_EXE_rshogi_to_yo_params"))
            .arg("--mapping")
            .arg(&map)
            .arg("--rshogi-params")
            .arg(&rshogi)
            .arg("--output")
            .arg(&output)
            .output()
            .unwrap();
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
        let rows = tools::spsa_param_mapping::load_params(&output).unwrap();
        assert_eq!(rows[0].value, value);
        assert!(rows[0].min <= value && value <= rows[0].max);
        assert!(rows[0].step.is_finite() && rows[0].step >= 1.0);
    }
    std::fs::write(&rshogi, row(rshogi_name, i32::MIN)).unwrap();
    std::fs::write(&yo, row("YO_TEST", i32::MIN)).unwrap();
    let built = dir.path().join("built.toml");
    let result = Command::new(env!("CARGO_BIN_EXE_build_param_mapping"))
        .arg("--yo-params")
        .arg(&yo)
        .arg("--rshogi-params")
        .arg(&rshogi)
        .arg("--output")
        .arg(&built)
        .output()
        .unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    let table = tools::spsa_param_mapping::MappingTable::load(&built).unwrap();
    assert_eq!(table.mappings.len(), 1);
    assert!(!table.mappings[0].sign_flip);
}
