use std::path::Path;
use std::process::{Command, Output};

use sha2::{Digest, Sha256};
use tools::spsa_param_mapping::{SPSA_PARAMS_FORMAT_HEADER, load_spsa_params};

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn tuner(run: &Path, params: &Path, pairs: u32, batch_pairs: u32) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_spsa"));
    command.arg("--run-dir").arg(run).arg("--init-from").arg(params).args([
        "--engine-path",
        env!("CARGO_BIN_EXE_spsa_test_engine"),
        "--total-pairs",
        &pairs.to_string(),
        "--batch-pairs",
        &batch_pairs.to_string(),
        "--concurrency",
        "1",
        "--threads",
        "1",
        "--hash-mb",
        "1",
        "--nodes",
        "1",
        "--seed",
        "1",
    ]);
    command
}

#[test]
fn legacy_fixture_is_rejected_before_engine_start_or_force_init() {
    let dir = tempfile::tempdir().unwrap();
    let params =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/spsa/legacy-step-delta.params");
    let run = dir.path().join("run");
    std::fs::create_dir(&run).unwrap();
    std::fs::write(run.join("state.params"), "preserve state").unwrap();
    std::fs::write(run.join("meta.json"), "preserve metadata").unwrap();
    let spawn_log = dir.path().join("spawns");
    let output = tuner(&run, &params, 1, 1)
        .arg("--force-init")
        .env("SPSA_TEST_ENGINE_SPAWN_LOG", &spawn_log)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("missing SPSA format declaration"), "{error}");
    assert!(!spawn_log.exists());
    assert_eq!(std::fs::read_to_string(run.join("state.params")).unwrap(), "preserve state");
    assert_eq!(std::fs::read_to_string(run.join("meta.json")).unwrap(), "preserve metadata");
}

#[test]
fn converter_defaults_match_generator_and_are_accepted_by_tuner() {
    let dir = tempfile::tempdir().unwrap();
    let generated = dir.path().join("generated.params");
    assert_success(
        &Command::new(env!("CARGO_BIN_EXE_generate_spsa_params"))
            .arg("--output")
            .arg(&generated)
            .output()
            .unwrap(),
    );
    let mapping = dir.path().join("mapping.toml");
    std::fs::write(&mapping, "").unwrap();
    let yo = dir.path().join("yo.params");
    std::fs::write(&yo, "YO_ONLY,int,5,0,10,1,0.5\n").unwrap();
    let converted = dir.path().join("converted.params");
    assert_success(
        &Command::new(env!("CARGO_BIN_EXE_yo_to_rshogi_params"))
            .arg("--yo-params")
            .arg(&yo)
            .arg("--mapping")
            .arg(&mapping)
            .arg("--output")
            .arg(&converted)
            .output()
            .unwrap(),
    );
    assert_eq!(std::fs::read(&generated).unwrap(), std::fs::read(&converted).unwrap());
    let rows = load_spsa_params(&converted, false).unwrap();
    assert!(!rows.is_empty());
    assert!(rows.iter().all(|row| row.alpha == 0.002));
    let run = dir.path().join("run");
    assert_success(&tuner(&run, &converted, 1, 1).output().unwrap());
    let state = std::fs::read_to_string(run.join("state.params")).unwrap();
    assert!(state.starts_with(SPSA_PARAMS_FORMAT_HEADER));
}

#[test]
fn converter_keeps_confirmed_schedule_and_tuner_update_scale() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("base.params");
    std::fs::write(
        &base,
        format!("{SPSA_PARAMS_FORMAT_HEADER}\nSPSA_TEST_INT,int,4,0,10,1,0.002\n"),
    )
    .unwrap();
    let yo = dir.path().join("yo.params");
    std::fs::write(&yo, "YO_TEST,int,5,0,10,1,0.5\n").unwrap();
    let mapping = dir.path().join("mapping.toml");
    std::fs::write(
        &mapping,
        "[[mapping]]\nyo = 'YO_TEST'\nrshogi = 'SPSA_TEST_INT'\nsign_flip = false\n",
    )
    .unwrap();
    let converted = dir.path().join("converted.params");
    let mut converter = Command::new(env!("CARGO_BIN_EXE_yo_to_rshogi_params"));
    converter
        .arg("--yo-params")
        .arg(&yo)
        .arg("--base")
        .arg(&base)
        .arg("--mapping")
        .arg(&mapping)
        .arg("--output")
        .arg(&converted);
    assert_success(&converter.output().unwrap());
    let run = dir.path().join("run");
    assert_success(
        &tuner(&run, &converted, 2, 2)
            .args(["--alpha", "1", "--gamma", "1", "--a-ratio", "0"])
            .env("SPSA_TEST_ENGINE_MODE", "value_driven")
            .output()
            .unwrap(),
    );
    let state = std::fs::read_to_string(run.join("state.params")).unwrap();
    assert!(state.contains("SPSA_TEST_INT,int,5.008000,0,10,1,0.002"), "{state}");
    let stats = std::fs::read_to_string(run.join("stats.csv")).unwrap();
    let fields: Vec<_> = stats.lines().nth(1).unwrap().split(',').collect();
    assert_eq!(fields[10], "0.008000", "{stats}");

    std::fs::write(&base, "SPSA_TEST_INT,int,5,0,10,1,0.002\n").unwrap();
    let before = std::fs::read(&converted).unwrap();
    assert!(!converter.output().unwrap().status.success());
    assert_eq!(std::fs::read(&converted).unwrap(), before);
    assert_success(&converter.arg("--assume-c-end-r-end").output().unwrap());
    assert_eq!(std::fs::read(&converted).unwrap(), before);
}

#[test]
fn confirmed_unmarked_resume_keeps_hash_checks_and_adds_header_at_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let params = dir.path().join("canonical.params");
    let unmarked = "SPSA_TEST_INT,int,5,0,10,1,0.002\n";
    std::fs::write(&params, unmarked).unwrap();
    let run = dir.path().join("run");
    assert_success(&tuner(&run, &params, 1, 1).arg("--assume-c-end-r-end").output().unwrap());
    let state_path = run.join("state.params");
    let state = std::fs::read_to_string(&state_path).unwrap();
    let old_state = state.strip_prefix(&format!("{SPSA_PARAMS_FORMAT_HEADER}\n")).unwrap();
    std::fs::write(&state_path, old_state).unwrap();
    let meta_path = run.join("meta.json");
    let mut meta: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&meta_path).unwrap()).unwrap();
    meta["current_params_sha256"] = format!("{:x}", Sha256::digest(old_state.as_bytes())).into();
    std::fs::write(&meta_path, serde_json::to_vec(&meta).unwrap()).unwrap();
    let original_meta = std::fs::read(&meta_path).unwrap();
    let spawn_log = dir.path().join("spawns");
    let mut resume = tuner(&run, &params, 2, 1);
    resume
        .args(["--resume", "--force-schedule"])
        .env("SPSA_TEST_ENGINE_SPAWN_LOG", &spawn_log);
    assert!(!resume.output().unwrap().status.success());
    assert!(!spawn_log.exists());
    assert_eq!(std::fs::read_to_string(&state_path).unwrap(), old_state);
    assert_eq!(std::fs::read(&meta_path).unwrap(), original_meta);
    resume.arg("--assume-c-end-r-end");
    std::fs::write(&state_path, old_state.replace(",5.000000,", ",6.000000,")).unwrap();
    let corrupted = resume.output().unwrap();
    assert!(!corrupted.status.success());
    let error = String::from_utf8_lossy(&corrupted.stderr);
    assert!(error.contains("current_params_sha256"), "{error}");
    assert!(!spawn_log.exists());
    assert_eq!(std::fs::read(&meta_path).unwrap(), original_meta);
    std::fs::write(&state_path, old_state).unwrap();
    assert_success(&resume.output().unwrap());
    assert!(spawn_log.exists());
    let state = std::fs::read(&state_path).unwrap();
    assert!(state.starts_with(SPSA_PARAMS_FORMAT_HEADER.as_bytes()));
    let meta: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&meta_path).unwrap()).unwrap();
    assert_eq!(meta["current_params_sha256"], format!("{:x}", Sha256::digest(&state)));
    assert_eq!(meta["completed_pairs"], 2);
    assert_eq!(std::fs::read_to_string(&params).unwrap(), unmarked);
}
