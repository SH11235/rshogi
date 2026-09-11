#![cfg(unix)]
use serde_json::Value;
use std::fs;
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::Path;
use std::process::Command;

fn engine(path: &Path) {
    fs::write(
        path,
        r#"#!/bin/sh
printf 'launch
' >> "$LAUNCH_LOG"
while IFS= read -r line; do
  case "$line" in
    usi) printf 'id name output-mock
usiok
' ;;
    isready) printf 'readyok
' ;;
    go*) printf 'bestmove resign
' ;;
    quit) break ;;
  esac
done
"#,
    )
    .unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn run(engine: &Path, output: &Path, labels: &[&str], launches: &Path) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_tournament"));
    for label in labels {
        cmd.arg("--engine").arg(engine).arg(format!("--engine-label={label}"));
    }
    cmd.args([
        "--games",
        "1",
        "--concurrency",
        "1",
        "--nodes",
        "1",
        "--out-dir",
    ])
    .arg(output)
    .env("LAUNCH_LOG", launches)
    .output()
    .unwrap()
}

#[test]
fn colliding_labels_produce_six_distinct_complete_card_files() {
    assert_complete_cards(&["a", "b-vs-c", "a-vs-b", "c"], &["a", "b-vs-c", "a-vs-b", "c"]);
}

#[test]
fn sanitized_and_truncated_labels_preserve_original_metadata_and_distinct_cards() {
    let long_a = format!("{}a", "x".repeat(48));
    let long_b = format!("{}b", "x".repeat(48));
    assert_complete_cards(
        &["a/b", "a\\b", &long_a, &long_b],
        &["a-b", "a-b", &"x".repeat(48), &"x".repeat(48)],
    );
    assert_complete_cards(
        &["", "日本語", "../: *?\"<>|", "-Step_200000.nnue-"],
        &["engine", "engine", "engine", "Step_200000-nnue"],
    );
}

fn assert_complete_cards(labels: &[&str; 4], display_labels: &[&str; 4]) {
    let dir = tempfile::tempdir().unwrap();
    let mock = dir.path().join("engine.sh");
    engine(&mock);
    let output = dir.path().join("run");
    let result = run(&mock, &output, labels, &dir.path().join("launches"));
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    let jsonls = fs::read_dir(&output)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "jsonl"))
        .count();
    assert_eq!(jsonls, 6);
    let meta: Value = serde_json::from_slice(&fs::read(output.join("meta.json")).unwrap()).unwrap();
    for (i, label) in labels.iter().enumerate() {
        assert_eq!(meta["engines"][i]["label"], *label);
    }
    #[cfg(feature = "csa-replay")]
    {
        use tools::replay::{GameSource, JsonlSource};
        let source = JsonlSource::new(&output);
        let index = source.build_index().unwrap();
        assert_eq!(index.pair_files.len(), 6);
        assert_eq!(index.entries.len(), 12);
        assert!(index.warnings.is_empty());
        for entry in &index.entries {
            source.load_game(&index, entry).unwrap();
        }
    }
    for i in 0..4 {
        for j in i + 1..4 {
            let rows = fs::read_to_string(output.join(format!(
                "pair-{i}-{j}__{}-vs-{}.jsonl",
                display_labels[i], display_labels[j]
            )))
            .unwrap();
            let rows = rows
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .collect::<Vec<_>>();
            assert_eq!(rows[0]["engine_cmd"]["label_black"], labels[i]);
            assert_eq!(rows[0]["engine_cmd"]["label_white"], labels[j]);
            assert_eq!(rows.iter().filter(|row| row["type"] == "result").count(), 2);
        }
    }
}

#[test]
fn existing_run_and_symlink_directories_are_preserved_before_engine_spawn() {
    let dir = tempfile::tempdir().unwrap();
    let mock = dir.path().join("engine.sh");
    engine(&mock);
    for kind in ["populated", "symlink"] {
        let output = dir.path().join(kind);
        let target = dir.path().join(format!("{kind}-target"));
        if kind == "symlink" {
            fs::create_dir(&target).unwrap();
            symlink(&target, &output).unwrap();
        } else {
            fs::create_dir(&output).unwrap();
        }
        fs::write(output.join("meta.json"), b"original bytes").unwrap();
        let launches = dir.path().join(format!("{kind}-launches"));
        let result = run(&mock, &output, &["a", "b"], &launches);
        assert!(!result.status.success());
        assert!(!launches.exists());
        assert_eq!(fs::read(output.join("meta.json")).unwrap(), b"original bytes");
    }
}

#[test]
fn prepared_stderr_directory_is_compatible_and_old_run_cannot_restart() {
    let dir = tempfile::tempdir().unwrap();
    let mock = dir.path().join("engine.sh");
    engine(&mock);
    let output = dir.path().join("prepared");
    fs::create_dir_all(output.join("engine-stderr/base")).unwrap();
    fs::write(output.join("engine-stderr/base/real-bin"), b"wrapper setup").unwrap();
    let launches = dir.path().join("launches");
    assert!(run(&mock, &output, &["a", "b"], &launches).status.success());
    assert_eq!(fs::read(output.join("engine-stderr/base/real-bin")).unwrap(), b"wrapper setup");
    let meta = fs::read(output.join("meta.json")).unwrap();
    let card = fs::read(output.join("pair-0-1__a-vs-b.jsonl")).unwrap();
    let old_launches = fs::read(&launches).unwrap();
    assert!(!run(&mock, &output, &["a", "b"], &launches).status.success());
    assert_eq!(fs::read(output.join("meta.json")).unwrap(), meta);
    assert_eq!(fs::read(output.join("pair-0-1__a-vs-b.jsonl")).unwrap(), card);
    assert_eq!(fs::read(launches).unwrap(), old_launches);
}
