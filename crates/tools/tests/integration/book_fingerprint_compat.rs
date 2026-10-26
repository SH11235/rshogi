use std::path::{Path, PathBuf};
use std::process::Command;

const START: &str = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";

fn engine() -> PathBuf {
    static ENGINE: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    ENGINE
        .get_or_init(|| {
            let dir = tempfile::tempdir().unwrap();
            let status = Command::new("rustc")
                .arg("--edition=2024")
                .arg(
                    Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("tests/fixtures/book_fingerprint_engine.rs"),
                )
                .arg("-o")
                .arg(dir.path().join(format!("engine{}", std::env::consts::EXE_SUFFIX)))
                .output()
                .unwrap();
            assert!(status.status.success(), "{}", String::from_utf8_lossy(&status.stderr));
            dir
        })
        .path()
        .join(format!("engine{}", std::env::consts::EXE_SUFFIX))
}

fn run(executable: &str, dir: &Path, options: &[String], resume: bool) -> std::process::Output {
    let mut command = Command::new(executable);
    command
        .arg("--book")
        .arg(dir.join("input.db"))
        .arg("--out")
        .arg(dir.join("out.db"))
        .arg("--engine")
        .arg(engine())
        .arg("--journal")
        .arg(dir.join("journal.jsonl"))
        .arg("--go")
        .arg("nodes 1")
        .env("BOOK_FINGERPRINT_GO_LOG", dir.join("go.log"));
    for option in options {
        command.arg("--engine-option").arg(option);
    }
    if resume {
        command.arg("--resume");
    }
    command.output().unwrap()
}

fn count(dir: &Path) -> usize {
    std::fs::read_to_string(dir.join("go.log")).unwrap().lines().count()
}

#[test]
fn valid_resource_options_work_for_fresh_and_resume_runs() {
    for executable in [
        env!("CARGO_BIN_EXE_book_rescore"),
        env!("CARGO_BIN_EXE_book_extend"),
    ] {
        for kind in ["disabled", "book", "relative"] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(
                dir.path().join("input.db"),
                format!("#YANEURAOU-DB2016 1.00\nsfen {START}\n7g7f none 0 0 1\n"),
            )
            .unwrap();
            let models = dir.path().join("models");
            std::fs::create_dir(&models).unwrap();
            let model = models.join("model.bin");
            std::fs::write(&model, b"model").unwrap();
            let books = dir.path().join("books");
            std::fs::create_dir(&books).unwrap();
            let book = books.join("opening.db");
            std::fs::write(&book, b"book-a").unwrap();
            let options = match kind {
                "disabled" => vec![
                    format!("EvalFile={}", model.display()),
                    "BookFile=no_book".into(),
                    "BookDir=missing".into(),
                    "BookExploreFile=<empty>".into(),
                    "SPSAParamsFile=<auto>".into(),
                ],
                "book" => vec![
                    format!("EvalFile={}", model.display()),
                    format!("BookDir={}", books.display()),
                    "BookFile=opening.db".into(),
                ],
                _ => vec![
                    format!("EvalDir={}", models.display()),
                    "EvalFile=model.bin".into(),
                    "BookFile=no_book".into(),
                    "CustomFile=false".into(),
                ],
            };
            let first = run(executable, dir.path(), &options, false);
            assert!(first.status.success(), "{kind}: {}", String::from_utf8_lossy(&first.stderr));
            let before = count(dir.path());
            if kind == "book" {
                std::fs::write(&book, b"book-b").unwrap();
            }
            let second = run(executable, dir.path(), &options, true);
            assert!(second.status.success(), "{kind}: {}", String::from_utf8_lossy(&second.stderr));
            if kind == "disabled" {
                assert_eq!(before, count(dir.path()));
            } else {
                assert!(count(dir.path()) > before);
            }
        }
    }
}

#[test]
fn rescore_protects_the_book_resource_resolved_from_book_dir() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("input.db"), "#YANEURAOU-DB2016 1.00\n").unwrap();
    let book = dir.path().join("opening.db");
    std::fs::write(&book, b"preserve").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_book_rescore"))
        .arg("--book")
        .arg(dir.path().join("input.db"))
        .arg("--out")
        .arg(&book)
        .arg("--engine")
        .arg(engine())
        .arg("--journal")
        .arg(dir.path().join("journal.jsonl"))
        .arg("--engine-option")
        .arg(format!("BookDir={}", dir.path().display()))
        .arg("--engine-option")
        .arg("BookFile=opening.db")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(std::fs::read(&book).unwrap(), b"preserve");
}

#[test]
fn rescore_protects_existing_custom_file_candidates_through_hardlinks() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("input.db"), "#YANEURAOU-DB2016 1.00\n").unwrap();
    let resource = dir.path().join("resource.bin");
    let alias = dir.path().join("output.db");
    std::fs::write(&resource, b"preserve").unwrap();
    std::fs::hard_link(&resource, &alias).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_book_rescore"))
        .arg("--book")
        .arg(dir.path().join("input.db"))
        .arg("--out")
        .arg(&alias)
        .arg("--engine")
        .arg(engine())
        .arg("--journal")
        .arg(dir.path().join("journal.jsonl"))
        .arg("--engine-option")
        .arg(format!("CustomFile={}", resource.display()))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(std::fs::read(&resource).unwrap(), b"preserve");
    assert_eq!(std::fs::read(&alias).unwrap(), b"preserve");
}

#[test]
fn path_only_engine_uses_its_binary_and_adjacent_params_for_reuse_and_protection() {
    for executable in [
        env!("CARGO_BIN_EXE_book_rescore"),
        env!("CARGO_BIN_EXE_book_extend"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        let work = dir.path().join("work");
        std::fs::create_dir(&bin).unwrap();
        std::fs::create_dir(&work).unwrap();
        let name = "rshogi-fingerprint-path-engine";
        let program = bin.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
        std::fs::copy(engine(), &program).unwrap();
        let params = bin.join("spsa.params");
        std::fs::write(&params, b"params-a").unwrap();
        let model = dir.path().join("model.bin");
        std::fs::write(&model, b"model").unwrap();
        let input = work.join("input.db");
        std::fs::write(&input, format!("#YANEURAOU-DB2016 1.00\nsfen {START}\n7g7f none 0 0 1\n"))
            .unwrap();
        let log = work.join("go.log");
        let mut directories = vec![bin];
        if let Some(paths) = std::env::var_os("PATH") {
            directories.extend(std::env::split_paths(&paths));
        }
        let paths = std::env::join_paths(directories).unwrap();
        let execute = |resume: bool, out: &Path| {
            let mut command = Command::new(executable);
            command
                .current_dir(&work)
                .env("PATH", &paths)
                .env("BOOK_FINGERPRINT_GO_LOG", &log)
                .arg("--book")
                .arg(&input)
                .arg("--out")
                .arg(out)
                .arg("--engine")
                .arg(name)
                .arg("--journal")
                .arg(work.join("journal.jsonl"))
                .arg("--engine-option")
                .arg(format!("EvalFile={}", model.display()))
                .arg("--engine-option")
                .arg("BookFile=no_book")
                .arg("--go")
                .arg("nodes 1");
            if resume {
                command.arg("--resume");
            }
            command.output().unwrap()
        };
        let out = work.join("out.db");
        let first = execute(false, &out);
        assert!(first.status.success(), "{}", String::from_utf8_lossy(&first.stderr));
        let initial = count(&work);
        let unchanged = execute(true, &out);
        assert!(unchanged.status.success(), "{}", String::from_utf8_lossy(&unchanged.stderr));
        assert_eq!(initial, count(&work));
        use std::io::Write;
        std::fs::OpenOptions::new()
            .append(true)
            .open(&program)
            .unwrap()
            .write_all(b"content-change")
            .unwrap();
        let changed_binary = execute(true, &out);
        assert!(
            changed_binary.status.success(),
            "{}",
            String::from_utf8_lossy(&changed_binary.stderr)
        );
        assert!(count(&work) > initial);
        let after_binary = count(&work);
        std::fs::write(&params, b"params-b").unwrap();
        let changed_params = execute(true, &out);
        assert!(
            changed_params.status.success(),
            "{}",
            String::from_utf8_lossy(&changed_params.stderr)
        );
        assert!(count(&work) > after_binary);
        if executable == env!("CARGO_BIN_EXE_book_rescore") {
            for resource in [&program, &params] {
                let alias = work.join("alias.db");
                std::fs::hard_link(resource, &alias).unwrap();
                let saved = std::fs::read(resource).unwrap();
                let rejected = execute(false, &alias);
                assert!(!rejected.status.success());
                assert_eq!(std::fs::read(resource).unwrap(), saved);
                std::fs::remove_file(alias).unwrap();
            }
        }
    }
}

#[test]
#[cfg(windows)]
fn bare_non_exe_name_does_not_switch_to_the_appended_exe_after_resolution() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    let work = dir.path().join("work");
    std::fs::create_dir(&bin).unwrap();
    std::fs::create_dir(&work).unwrap();
    let plain = bin.join("fingerprint-name.custom");
    std::fs::copy(engine(), &plain).unwrap();
    let suffixed = bin.join("fingerprint-name.custom.exe");
    std::fs::copy(engine(), &suffixed).unwrap();
    let model = work.join("model.bin");
    std::fs::write(&model, b"model").unwrap();
    let input = work.join("input.db");
    std::fs::write(&input, format!("#YANEURAOU-DB2016 1.00\nsfen {START}\n7g7f none 0 0 1\n"))
        .unwrap();
    let log = work.join("executable.log");
    let output = Command::new(env!("CARGO_BIN_EXE_book_rescore"))
        .current_dir(&work)
        .env("PATH", &bin)
        .env("BOOK_FINGERPRINT_EXE_LOG", &log)
        .arg("--engine")
        .arg("fingerprint-name.custom")
        .arg("--book")
        .arg(input)
        .arg("--out")
        .arg(work.join("out.db"))
        .arg("--journal")
        .arg(work.join("journal.jsonl"))
        .arg("--engine-option")
        .arg(format!("EvalFile={}", model.display()))
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    for executable in std::fs::read_to_string(log).unwrap().lines() {
        assert!(same_file::is_same_file(executable, &plain).unwrap());
        assert!(!same_file::is_same_file(executable, &suffixed).unwrap());
    }
}
