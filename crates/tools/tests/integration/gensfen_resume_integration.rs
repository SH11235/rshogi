#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_gensfen");

fn write_engine(dir: &Path, crash_on_go: bool) -> PathBuf {
    let path = dir.join("mock-engine.sh");
    let go_action = if crash_on_go {
        "exit 42"
    } else {
        "sleep 0.05; printf 'info score cp 0 nodes 1\\nbestmove 7g7f\\n'"
    };
    fs::write(
        &path,
        format!(
            "#!/bin/sh\nif [ -n \"$GENSFEN_ENGINE_MARKER\" ]; then : > \"$GENSFEN_ENGINE_MARKER\"; fi\nwhile IFS= read -r line; do\n  case \"$line\" in\n    usi) printf 'id name resume-test\\nusiok\\n' ;;\n    isready) printf 'readyok\\n' ;;\n    go*) {go_action} ;;\n    quit) break ;;\n  esac\ndone\nif [ -n \"$GENSFEN_REMOVE_ON_EXIT\" ]; then rm -f -- \"$GENSFEN_REMOVE_ON_EXIT\"; fi\n"
        ),
    )
    .unwrap();
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).unwrap();
    path
}

fn base_command(engine: &Path, out_dir: &Path) -> Command {
    command_with_concurrency(engine, out_dir, 2)
}

fn command_with_concurrency(engine: &Path, out_dir: &Path, concurrency: usize) -> Command {
    let mut command = Command::new(BIN);
    command.args([
        "--native=false",
        "--engine-path",
        engine.to_str().unwrap(),
        "--out-dir",
        out_dir.to_str().unwrap(),
        "--games",
        "4",
        "--nodes",
        "1",
        "--max-moves",
        "1",
        "--concurrency",
        &concurrency.to_string(),
        "--dedup-hash-size",
        "0",
        "--shuffle-seed",
        "42",
        "--emit-game-id-sidecar",
        out_dir.join("gensfen.game_ids.bin").to_str().unwrap(),
        "--log-info",
        "--emit-eval-file",
        "--emit-metrics",
    ]);
    command
}

fn wait_for_nonempty_checkpoint(child: &mut Child, out_dir: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        for worker in 0..2 {
            let path = out_dir.join(format!("gensfen.w{worker}.jsonl"));
            if fs::metadata(path).is_ok_and(|meta| meta.len() > 0) {
                return;
            }
        }
        assert!(child.try_wait().unwrap().is_none(), "gensfen exited before checkpoint creation");
        assert!(Instant::now() < deadline, "timed out waiting for worker checkpoint");
        thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_result_count(child: &mut Child, out_dir: &Path, count: usize) {
    let checkpoint = out_dir.join("gensfen.w0.jsonl");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if checkpoint.exists() && result_ids(&checkpoint).len() >= count {
            return;
        }
        assert!(child.try_wait().unwrap().is_none(), "gensfen exited before result checkpoint");
        assert!(Instant::now() < deadline, "timed out waiting for result checkpoint");
        thread::sleep(Duration::from_millis(10));
    }
}

fn result_ids(path: &Path) -> Vec<u32> {
    let mut ids: Vec<u32> = fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter_map(|line| {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            (value["type"] == "result").then(|| value["game_id"].as_u64().unwrap() as u32)
        })
        .collect();
    ids.sort_unstable();
    ids
}

fn result_values(path: &Path) -> Vec<serde_json::Value> {
    let mut values: Vec<_> = fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|value| value["type"] == "result")
        .collect();
    values.sort_by_key(|value| value["game_id"].as_u64().unwrap());
    values
}

fn sidecar_ids(path: &Path) -> Vec<u32> {
    let bytes = fs::read(path).unwrap();
    assert_eq!(bytes.len() % 4, 0);
    let mut ids: Vec<u32> = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|bytes| u32::from_le_bytes(*bytes))
        .collect();
    ids.sort_unstable();
    ids
}

fn assert_exact_outputs(out_dir: &Path) {
    assert_eq!(result_ids(&out_dir.join("gensfen.jsonl")), vec![1, 2, 3, 4]);
    assert_eq!(fs::metadata(out_dir.join("gensfen.psv")).unwrap().len(), 4 * 40);
    assert_eq!(sidecar_ids(&out_dir.join("gensfen.game_ids.bin")), vec![1, 2, 3, 4]);
    for (path, expected_lines) in [
        ("gensfen.info.jsonl", 4),
        ("gensfen.eval.txt", 12),
        ("gensfen.metrics.jsonl", 4),
    ] {
        assert_eq!(
            fs::read_to_string(out_dir.join(path)).unwrap().lines().count(),
            expected_lines,
            "unexpected line count in {path}"
        );
    }
}

#[test]
fn hard_kill_resume_preserves_worker_checkpoints_and_completes_missing_ids() {
    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("run");
    let engine = write_engine(dir.path(), false);
    let mut child = base_command(&engine, &out_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_for_nonempty_checkpoint(&mut child, &out_dir);
    child.kill().unwrap();
    child.wait().unwrap();

    let checkpoint_bytes: u64 = (0..2)
        .map(|worker| out_dir.join(format!("gensfen.w{worker}.psv")))
        .filter_map(|path| fs::metadata(path).ok().map(|meta| meta.len()))
        .sum();
    assert!(checkpoint_bytes > 0);

    let status = base_command(&engine, &out_dir)
        .args(["--resume", "--force-unlock"])
        .status()
        .unwrap();
    assert!(status.success());
    assert_exact_outputs(&out_dir);
    for worker in 0..2 {
        assert!(!out_dir.join(format!("gensfen.w{worker}.jsonl")).exists());
        assert!(!out_dir.join(format!("gensfen.w{worker}.psv")).exists());
    }
}

#[test]
fn resume_rejects_each_missing_referenced_worker_output_without_truncating_psv() {
    for missing_suffix in [
        "psv",
        "game_ids.bin",
        "info.jsonl",
        "eval.txt",
        "metrics.jsonl",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let out_dir = dir.path().join(format!("missing-{missing_suffix}"));
        let engine = write_engine(dir.path(), false);
        let mut child = command_with_concurrency(&engine, &out_dir, 1)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        wait_for_result_count(&mut child, &out_dir, 1);
        child.kill().unwrap();
        child.wait().unwrap();

        let psv = out_dir.join("gensfen.w0.psv");
        let psv_before = fs::read(&psv).unwrap();
        fs::remove_file(out_dir.join(format!("gensfen.w0.{missing_suffix}"))).unwrap();
        let status = command_with_concurrency(&engine, &out_dir, 1)
            .args(["--resume", "--force-unlock"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(!status.success(), "missing {missing_suffix} was accepted");
        if missing_suffix != "psv" {
            assert_eq!(
                fs::read(&psv).unwrap(),
                psv_before,
                "PSV was modified for {missing_suffix}"
            );
        }
    }
}

#[test]
fn successful_worker_exit_rejects_each_missing_output_before_staging() {
    for suffix in [
        "jsonl",
        "psv",
        "game_ids.bin",
        "info.jsonl",
        "eval.txt",
        "metrics.jsonl",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let out_dir = dir.path().join(format!("missing-after-success-{suffix}"));
        let engine = write_engine(dir.path(), false);
        let missing = out_dir.join(format!("gensfen.w0.{suffix}"));
        let status = command_with_concurrency(&engine, &out_dir, 1)
            .env("GENSFEN_REMOVE_ON_EXIT", &missing)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(!status.success(), "missing {suffix} was accepted");
        assert!(!out_dir.join(".gensfen.finalization.json").exists());
        assert!(!out_dir.join("gensfen.finalized.json").exists());
    }
}

#[test]
fn output_training_data_rejects_staging_and_worker_path_collisions_before_creation() {
    let dir = tempfile::tempdir().unwrap();
    let engine = write_engine(dir.path(), false);
    for relative in [".gensfen.jsonl.merge.tmp", "gensfen.w0.psv"] {
        let out_dir = dir.path().join(relative.replace('.', "_"));
        let training = out_dir.join(relative);
        let status = command_with_concurrency(&engine, &out_dir, 1)
            .args(["--output-training-data", training.to_str().unwrap()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(!status.success(), "collision {relative} was accepted");
        assert!(!out_dir.exists(), "collision validation created output directory");
    }
}

#[test]
fn output_collision_detection_resolves_symlinks() {
    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("symlink-collision");
    fs::create_dir(&out_dir).unwrap();
    let worker_path = out_dir.join("gensfen.w0.psv");
    fs::write(&worker_path, b"preserve").unwrap();
    let training_alias = out_dir.join("training-alias.psv");
    symlink(&worker_path, &training_alias).unwrap();
    let engine = write_engine(dir.path(), false);

    let status = command_with_concurrency(&engine, &out_dir, 1)
        .args(["--output-training-data", training_alias.to_str().unwrap()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(!status.success());
    assert_eq!(fs::read(&worker_path).unwrap(), b"preserve");
    assert!(!out_dir.join(".gensfen.lock").exists());
}

#[test]
fn resume_rolls_back_to_last_fsync_interval_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("fsync-boundary");
    let engine = write_engine(dir.path(), false);
    let status = command_with_concurrency(&engine, &out_dir, 1)
        .args(["--fsync-interval-games", "2"])
        .env("RSHOGI_GENSFEN_FAULT", "before_result:4")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(!status.success());
    assert_eq!(result_ids(&out_dir.join("gensfen.jsonl")), vec![1, 2]);
    assert_eq!(fs::metadata(out_dir.join("gensfen.psv")).unwrap().len(), 2 * 40);

    assert!(
        command_with_concurrency(&engine, &out_dir, 1)
            .args(["--resume", "--fsync-interval-games", "2"])
            .status()
            .unwrap()
            .success()
    );
    assert_exact_outputs(&out_dir);
    for relative in [
        "gensfen.w0.jsonl",
        "gensfen.w0.psv",
        "gensfen.w0.game_ids.bin",
        "gensfen.w0.info.jsonl",
        "gensfen.w0.eval.txt",
        "gensfen.w0.metrics.jsonl",
        ".gensfen.finalization.json",
        ".gensfen.finalization.json.tmp",
        ".gensfen.lock",
    ] {
        assert!(!out_dir.join(relative).exists(), "{relative} remains after resume");
    }
}

#[test]
fn worker_crash_returns_nonzero_exit_status() {
    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("run");
    let engine = write_engine(dir.path(), true);
    let status: ExitStatus = base_command(&engine, &out_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(!status.success());
}

#[test]
fn non_resume_rejects_existing_training_output_and_merge_temp() {
    let dir = tempfile::tempdir().unwrap();
    let engine = write_engine(dir.path(), false);

    let training_dir = dir.path().join("existing-training");
    fs::create_dir(&training_dir).unwrap();
    let training_path = training_dir.join("gensfen.psv");
    fs::write(&training_path, b"keep").unwrap();
    assert!(!base_command(&engine, &training_dir).status().unwrap().success());
    assert_eq!(fs::read(&training_path).unwrap(), b"keep");

    let merge_dir = dir.path().join("existing-merge");
    fs::create_dir(&merge_dir).unwrap();
    let merge_path = merge_dir.join(".gensfen.jsonl.merge.tmp");
    fs::write(&merge_path, b"incomplete").unwrap();
    assert!(!base_command(&engine, &merge_dir).status().unwrap().success());
    assert_eq!(fs::read(&merge_path).unwrap(), b"incomplete");
}

#[test]
fn non_resume_rejects_dangling_worker_temp_with_actionable_error() {
    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("dangling-worker-temp");
    fs::create_dir(&out_dir).unwrap();
    let worker_temp = out_dir.join("gensfen.w99.psv");
    symlink(dir.path().join("missing-worker-temp"), &worker_temp).unwrap();
    let engine = write_engine(dir.path(), false);
    let engine_marker = dir.path().join("engine-started");

    let output = command_with_concurrency(&engine, &out_dir, 1)
        .env("GENSFEN_ENGINE_MARKER", &engine_marker)
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("already exists and is non-empty; use --resume or move it aside")
    );
    assert!(!engine_marker.exists());
    assert!(fs::symlink_metadata(&worker_temp).unwrap().file_type().is_symlink());
}

fn internal_paths() -> [&'static str; 17] {
    [
        "gensfen.w0.jsonl",
        "gensfen.w0.psv",
        "gensfen.w0.game_ids.bin",
        "gensfen.w0.info.jsonl",
        "gensfen.w0.eval.txt",
        "gensfen.w0.metrics.jsonl",
        ".gensfen.jsonl.merge.tmp",
        ".gensfen.psv.merge.tmp",
        ".gensfen.game_ids.bin.merge.tmp",
        ".gensfen.info.jsonl.merge.tmp",
        ".gensfen.eval.txt.merge.tmp",
        ".gensfen.metrics.jsonl.merge.tmp",
        ".gensfen.finalization.json",
        ".gensfen.finalization.json.tmp",
        "gensfen.finalized.json",
        "gensfen.finalized.json.tmp",
        ".gensfen.lock",
    ]
}

#[test]
fn every_internal_path_rejects_symlink_without_touching_target() {
    for relative in internal_paths() {
        let dir = tempfile::tempdir().unwrap();
        let out_dir = dir.path().join("run");
        fs::create_dir(&out_dir).unwrap();
        let victim = dir.path().join("victim");
        fs::write(&victim, b"preserve").unwrap();
        symlink(&victim, out_dir.join(relative)).unwrap();
        let engine = write_engine(dir.path(), false);
        let engine_marker = dir.path().join("engine-started");

        let status = command_with_concurrency(&engine, &out_dir, 1)
            .env("GENSFEN_ENGINE_MARKER", &engine_marker)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();

        assert!(!status.success(), "accepted symlink at {relative}");
        assert_eq!(fs::read(&victim).unwrap(), b"preserve", "modified target of {relative}");
        assert!(!engine_marker.exists(), "started engine before rejecting {relative}");
        assert!(fs::symlink_metadata(out_dir.join(relative)).unwrap().file_type().is_symlink());
    }
}

#[test]
fn every_internal_path_detects_dangling_symlink() {
    for relative in internal_paths() {
        let dir = tempfile::tempdir().unwrap();
        let out_dir = dir.path().join("run");
        fs::create_dir(&out_dir).unwrap();
        let internal = out_dir.join(relative);
        let missing_target = dir.path().join("missing-target");
        symlink(&missing_target, &internal).unwrap();
        let engine = write_engine(dir.path(), false);

        let status = command_with_concurrency(&engine, &out_dir, 1)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();

        assert!(!status.success(), "accepted dangling symlink at {relative}");
        assert!(fs::symlink_metadata(&internal).unwrap().file_type().is_symlink());
        assert!(!missing_target.exists(), "created target of dangling symlink at {relative}");
    }
}

#[test]
fn resume_rejects_dangling_worker_jsonl_without_creating_its_target() {
    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("run");
    let engine = write_engine(dir.path(), false);
    let mut child = command_with_concurrency(&engine, &out_dir, 1)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_for_nonempty_checkpoint(&mut child, &out_dir);
    child.kill().unwrap();
    child.wait().unwrap();

    let worker_jsonl = out_dir.join("gensfen.w0.jsonl");
    fs::remove_file(&worker_jsonl).unwrap();
    let missing_target = dir.path().join("missing-worker-jsonl");
    symlink(&missing_target, &worker_jsonl).unwrap();

    let status = command_with_concurrency(&engine, &out_dir, 1)
        .args(["--resume", "--force-unlock"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(!status.success());
    assert!(!missing_target.exists());
    assert!(fs::symlink_metadata(&worker_jsonl).unwrap().file_type().is_symlink());
}

#[test]
fn every_internal_path_rejects_directories_and_special_files() {
    for relative in internal_paths() {
        for kind in ["directory", "special file"] {
            let dir = tempfile::tempdir().unwrap();
            let out_dir = dir.path().join("run");
            fs::create_dir(&out_dir).unwrap();
            let internal = out_dir.join(relative);
            if kind == "directory" {
                fs::create_dir(&internal).unwrap();
            } else {
                rustix::fs::mkfifoat(
                    rustix::fs::CWD,
                    &internal,
                    rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
                )
                .unwrap();
            }
            let engine = write_engine(dir.path(), false);

            let status = command_with_concurrency(&engine, &out_dir, 1)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap();

            assert!(!status.success(), "accepted {kind} at {relative}");
            assert!(fs::symlink_metadata(&internal).is_ok(), "removed {kind} at {relative}");
        }
    }
}

#[test]
fn non_resume_rejects_every_existing_final_path_before_side_effects() {
    for relative in [
        "gensfen.jsonl",
        "gensfen.psv",
        "gensfen.game_ids.bin",
        "gensfen.info.jsonl",
        "gensfen.eval.txt",
        "gensfen.metrics.jsonl",
    ] {
        for existing_kind in ["file", "directory", "symlink"] {
            let dir = tempfile::tempdir().unwrap();
            let out_dir = dir.path().join("run");
            fs::create_dir(&out_dir).unwrap();
            let existing = out_dir.join(relative);
            if existing_kind == "file" {
                fs::write(&existing, b"preserve").unwrap();
            } else if existing_kind == "directory" {
                fs::create_dir(&existing).unwrap();
            } else {
                symlink(out_dir.join("missing-target"), &existing).unwrap();
            }
            let engine = write_engine(dir.path(), false);
            let status = command_with_concurrency(&engine, &out_dir, 1)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap();
            assert!(!status.success(), "accepted existing {existing_kind} at {relative}");
            if existing_kind == "file" {
                assert_eq!(fs::read(&existing).unwrap(), b"preserve");
            } else if existing_kind == "directory" {
                assert!(existing.is_dir());
            } else {
                assert!(fs::symlink_metadata(&existing).unwrap().file_type().is_symlink());
            }
            assert!(!out_dir.join(".gensfen.lock").exists());
            assert!(!out_dir.join("gensfen.finalized.json").exists());
        }
    }
}

#[test]
fn uncommitted_worker_tails_are_not_finalized() {
    for fault in ["before_result", "result_partial", "sidecar_partial"] {
        let dir = tempfile::tempdir().unwrap();
        let out_dir = dir.path().join(fault);
        let engine = write_engine(dir.path(), false);
        let status = base_command(&engine, &out_dir)
            .env("RSHOGI_GENSFEN_FAULT", fault)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(!status.success(), "fault {fault} unexpectedly succeeded");
        assert!(result_ids(&out_dir.join("gensfen.jsonl")).is_empty());
        assert_eq!(fs::metadata(out_dir.join("gensfen.psv")).unwrap().len(), 0);
        assert_eq!(fs::metadata(out_dir.join("gensfen.game_ids.bin")).unwrap().len(), 0);
        assert_eq!(fs::metadata(out_dir.join("gensfen.info.jsonl")).unwrap().len(), 0);
        assert_eq!(fs::metadata(out_dir.join("gensfen.eval.txt")).unwrap().len(), 0);
        assert_eq!(fs::metadata(out_dir.join("gensfen.metrics.jsonl")).unwrap().len(), 0);

        assert!(base_command(&engine, &out_dir).arg("--resume").status().unwrap().success());
        assert_exact_outputs(&out_dir);
    }
}

#[test]
fn every_finalization_rename_is_idempotent_after_abort() {
    let baseline_dir = tempfile::tempdir().unwrap();
    let baseline_out = baseline_dir.path().join("baseline");
    let baseline_engine = write_engine(baseline_dir.path(), false);
    assert!(
        command_with_concurrency(&baseline_engine, &baseline_out, 1)
            .status()
            .unwrap()
            .success()
    );
    let expected_aux: Vec<Vec<u8>> = [
        "gensfen.info.jsonl",
        "gensfen.eval.txt",
        "gensfen.metrics.jsonl",
    ]
    .map(|path| fs::read(baseline_out.join(path)).unwrap())
    .into();

    for rename_index in 1..=6 {
        let dir = tempfile::tempdir().unwrap();
        let out_dir = dir.path().join(format!("rename-{rename_index}"));
        let engine = write_engine(dir.path(), false);
        let status = command_with_concurrency(&engine, &out_dir, 1)
            .env("RSHOGI_GENSFEN_FAULT", format!("after_final_rename_{rename_index}"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(!status.success(), "rename fault {rename_index} unexpectedly succeeded");
        assert!(out_dir.join(".gensfen.finalization.json").exists());

        assert!(
            command_with_concurrency(&engine, &out_dir, 1)
                .args(["--resume", "--force-unlock"])
                .status()
                .unwrap()
                .success()
        );
        assert_exact_outputs(&out_dir);
        for (path, expected) in [
            "gensfen.info.jsonl",
            "gensfen.eval.txt",
            "gensfen.metrics.jsonl",
        ]
        .into_iter()
        .zip(&expected_aux)
        {
            assert_eq!(
                &fs::read(out_dir.join(path)).unwrap(),
                expected,
                "content mismatch in {path}"
            );
        }
        assert!(!out_dir.join(".gensfen.finalization.json").exists());
    }
}

#[test]
fn finalization_journal_atomic_rename_faults_are_recoverable() {
    for fault in ["before_journal_rename", "after_journal_rename"] {
        let dir = tempfile::tempdir().unwrap();
        let out_dir = dir.path().join(fault);
        let engine = write_engine(dir.path(), false);
        let status = command_with_concurrency(&engine, &out_dir, 1)
            .env("RSHOGI_GENSFEN_FAULT", fault)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(!status.success(), "journal fault {fault} unexpectedly succeeded");

        assert!(
            command_with_concurrency(&engine, &out_dir, 1)
                .args(["--resume", "--force-unlock"])
                .status()
                .unwrap()
                .success()
        );
        assert_exact_outputs(&out_dir);
        assert!(!out_dir.join(".gensfen.finalization.json").exists());
        assert!(!out_dir.join(".gensfen.finalization.json.tmp").exists());
    }
}

#[test]
fn partial_finalization_journal_temporary_file_is_regenerated_on_resume() {
    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("partial-journal");
    let engine = write_engine(dir.path(), false);
    let status = command_with_concurrency(&engine, &out_dir, 1)
        .env("RSHOGI_GENSFEN_FAULT", "before_journal_rename")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(!status.success());

    let journal_tmp = out_dir.join(".gensfen.finalization.json.tmp");
    fs::write(&journal_tmp, b"{\"schema\":1,\"outputs\":[").unwrap();
    assert!(!out_dir.join(".gensfen.finalization.json").exists());

    assert!(
        command_with_concurrency(&engine, &out_dir, 1)
            .arg("--resume")
            .status()
            .unwrap()
            .success()
    );
    assert_exact_outputs(&out_dir);
    assert!(!journal_tmp.exists());
}

#[test]
fn merge_staging_without_journal_is_regenerated_on_resume() {
    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("before-journal");
    let engine = write_engine(dir.path(), false);
    let status = command_with_concurrency(&engine, &out_dir, 1)
        .env("RSHOGI_GENSFEN_FAULT", "before_journal_write")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(!status.success());
    assert!(out_dir.join(".gensfen.jsonl.merge.tmp").exists());
    assert!(!out_dir.join(".gensfen.finalization.json").exists());
    assert!(!out_dir.join(".gensfen.finalization.json.tmp").exists());

    assert!(
        command_with_concurrency(&engine, &out_dir, 1)
            .arg("--resume")
            .status()
            .unwrap()
            .success()
    );
    assert_exact_outputs(&out_dir);
    assert!(!out_dir.join(".gensfen.jsonl.merge.tmp").exists());
}

#[test]
fn finalization_cleanup_crash_discards_already_committed_worker_temps() {
    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("cleanup");
    let engine = write_engine(dir.path(), false);
    let status = base_command(&engine, &out_dir)
        .env("RSHOGI_GENSFEN_FAULT", "after_worker_temp_delete_1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(!status.success());
    assert!(out_dir.join(".gensfen.finalization.json").exists());
    assert!(out_dir.join("gensfen.jsonl").exists());
    assert!((0..2).any(|worker| out_dir.join(format!("gensfen.w{worker}.jsonl")).exists()));

    assert!(
        base_command(&engine, &out_dir)
            .args(["--resume", "--force-unlock"])
            .status()
            .unwrap()
            .success()
    );
    assert_exact_outputs(&out_dir);
    assert!(!out_dir.join(".gensfen.finalization.json").exists());
    for worker in 0..2 {
        assert!(!out_dir.join(format!("gensfen.w{worker}.jsonl")).exists());
    }
}

#[test]
fn fully_completed_resume_does_not_start_engine_workers() {
    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("complete");
    let engine = write_engine(dir.path(), false);
    assert!(base_command(&engine, &out_dir).status().unwrap().success());
    let marker = dir.path().join("engine-started");

    assert!(
        base_command(&engine, &out_dir)
            .arg("--resume")
            .env("GENSFEN_ENGINE_MARKER", &marker)
            .status()
            .unwrap()
            .success()
    );
    assert!(!marker.exists());
    assert_exact_outputs(&out_dir);
}

#[test]
fn resume_rejects_shortened_or_mismatched_final_teacher_outputs() {
    for damaged in ["psv", "sidecar"] {
        let dir = tempfile::tempdir().unwrap();
        let out_dir = dir.path().join(damaged);
        let engine = write_engine(dir.path(), false);
        assert!(base_command(&engine, &out_dir).status().unwrap().success());
        let path = if damaged == "psv" {
            out_dir.join("gensfen.psv")
        } else {
            out_dir.join("gensfen.game_ids.bin")
        };
        let file = fs::OpenOptions::new().write(true).open(path).unwrap();
        file.set_len(file.metadata().unwrap().len() - 1).unwrap();
        assert!(!base_command(&engine, &out_dir).arg("--resume").status().unwrap().success());
    }
}

#[test]
fn resume_rejects_same_length_final_content_corruption_without_rebaselining_hash() {
    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("same-length-corruption");
    let engine = write_engine(dir.path(), false);
    assert!(base_command(&engine, &out_dir).status().unwrap().success());
    let state_path = out_dir.join("gensfen.finalized.json");
    let state_before = fs::read(&state_path).unwrap();
    let psv_path = out_dir.join("gensfen.psv");
    let mut psv = fs::read(&psv_path).unwrap();
    psv[0] ^= 0x80;
    fs::write(&psv_path, &psv).unwrap();

    let status = base_command(&engine, &out_dir)
        .arg("--resume")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(!status.success());
    assert_eq!(fs::read(&state_path).unwrap(), state_before);
    assert_eq!(fs::read(&psv_path).unwrap(), psv);
    assert!(!out_dir.join(".gensfen.finalization.json").exists());
}

#[test]
fn resume_rejects_replaced_path_valued_usi_option_content() {
    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("option-model");
    let engine = write_engine(dir.path(), false);
    let model = dir.path().join("eval.bin");
    fs::write(&model, b"model-a").unwrap();
    let option = format!("EvalFile={}", model.display());
    assert!(
        base_command(&engine, &out_dir)
            .args(["--usi-option", &option])
            .status()
            .unwrap()
            .success()
    );
    fs::write(&model, b"model-b").unwrap();
    assert!(
        !base_command(&engine, &out_dir)
            .args(["--usi-option", &option, "--resume"])
            .status()
            .unwrap()
            .success()
    );
}

#[test]
fn nonexistent_path_valued_usi_option_runs_and_value_change_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("book-sentinel");
    let engine = write_engine(dir.path(), false);
    assert!(
        base_command(&engine, &out_dir)
            .args(["--usi-option", "BookFile=no_book"])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        !base_command(&engine, &out_dir)
            .args(["--usi-option", "BookFile=other_sentinel", "--resume"])
            .status()
            .unwrap()
            .success()
    );
}

#[test]
fn nth_fault_preserves_committed_prefix_and_resume_continues_from_it() {
    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("nth-fault");
    let engine = write_engine(dir.path(), false);
    let status = command_with_concurrency(&engine, &out_dir, 1)
        .env("RSHOGI_GENSFEN_FAULT", "before_result:2")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(!status.success());
    assert_eq!(result_ids(&out_dir.join("gensfen.jsonl")), vec![1]);
    assert_eq!(fs::metadata(out_dir.join("gensfen.psv")).unwrap().len(), 40);

    assert!(
        command_with_concurrency(&engine, &out_dir, 1)
            .arg("--resume")
            .status()
            .unwrap()
            .success()
    );
    assert_exact_outputs(&out_dir);
}

#[test]
fn uninterrupted_and_hard_kill_resume_outputs_match() {
    let dir = tempfile::tempdir().unwrap();
    let engine = write_engine(dir.path(), false);
    let full_dir = dir.path().join("full");
    let resumed_dir = dir.path().join("resumed");
    assert!(command_with_concurrency(&engine, &full_dir, 1).status().unwrap().success());

    let mut child = command_with_concurrency(&engine, &resumed_dir, 1)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_for_nonempty_checkpoint(&mut child, &resumed_dir);
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(
        command_with_concurrency(&engine, &resumed_dir, 1)
            .args(["--resume", "--force-unlock"])
            .status()
            .unwrap()
            .success()
    );

    assert_eq!(
        fs::read(full_dir.join("gensfen.psv")).unwrap(),
        fs::read(resumed_dir.join("gensfen.psv")).unwrap()
    );
    assert_eq!(
        fs::read(full_dir.join("gensfen.game_ids.bin")).unwrap(),
        fs::read(resumed_dir.join("gensfen.game_ids.bin")).unwrap()
    );
    assert_eq!(
        result_values(&full_dir.join("gensfen.jsonl")),
        result_values(&resumed_dir.join("gensfen.jsonl"))
    );
}
