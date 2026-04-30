//! SPSA run-dir 整合性の integration test。
//!
//! 単体テストでは `decide_init_action` 等の純粋関数や個別ヘルパーは検証できる
//! が、「main loop を 1 iter 回した結果として state.params / meta.json /
//! values.csv / stats.csv が正しく生成 + 反復ごとに append される」整合性は
//! 単体では保証されない。ここでは fake USI engine (`spsa_test_engine`) を相手に
//! 実際の `spsa` バイナリをサブプロセスで起動し、run-dir の最終形を検証する。
//!
//! 検証内容:
//! - fresh start (1 iter) で run-dir 配下に必要なファイルが揃う
//! - `--resume --iterations 1` で同 run-dir に append され、`completed_iterations=2`、
//!   `values.csv` に iter 0 / iter 1 / iter 2 の 3 行 + header が残ること
//! - 並行起動 (lock) で 2 個目が bail すること

use std::path::Path;
use std::process::Command;

const SPSA_BIN: &str = env!("CARGO_BIN_EXE_spsa");
const FAKE_ENGINE_BIN: &str = env!("CARGO_BIN_EXE_spsa_test_engine");

/// 最小限の canonical params (整数 1 個 + 浮動 1 個) を tempfile に書く。
fn write_canonical(path: &Path) {
    let body = "\
SPSA_TEST_INT,int,5,0,10,1,0.001 //test integer param
SPSA_TEST_FLOAT,float,1.5,0.0,3.0,0.5,0.001 //test float param
";
    std::fs::write(path, body).unwrap();
}

/// 1 行だけの startpos sfen。
fn write_startpos_file(path: &Path) {
    std::fs::write(path, "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1\n")
        .unwrap();
}

fn count_lines(path: &Path) -> usize {
    std::fs::read_to_string(path).unwrap_or_default().lines().count()
}

fn run_spsa_args(
    run_dir: &Path,
    canonical: &Path,
    startpos: &Path,
    extra: &[&str],
) -> std::process::Output {
    let mut args: Vec<String> = vec![
        "--run-dir".into(),
        run_dir.display().to_string(),
        "--engine-path".into(),
        FAKE_ENGINE_BIN.into(),
        "--init-from".into(),
        canonical.display().to_string(),
        "--iterations".into(),
        "1".into(),
        "--games-per-iteration".into(),
        "2".into(),
        "--concurrency".into(),
        "1".into(),
        "--seeds".into(),
        "1".into(),
        "--byoyomi".into(),
        "50".into(),
        "--threads".into(),
        "1".into(),
        "--hash-mb".into(),
        "16".into(),
        "--startpos-file".into(),
        startpos.display().to_string(),
    ];
    for s in extra {
        args.push((*s).to_string());
    }
    Command::new(SPSA_BIN)
        .args(&args)
        .output()
        .expect("failed to spawn spsa binary")
}

#[test]
fn fresh_start_produces_full_run_dir_layout() {
    let dir = tempfile::tempdir().unwrap();
    let canonical = dir.path().join("canonical.params");
    let startpos = dir.path().join("startpos.txt");
    let run_dir = dir.path().join("run");
    write_canonical(&canonical);
    write_startpos_file(&startpos);

    let output = run_spsa_args(&run_dir, &canonical, &startpos, &[]);
    assert!(
        output.status.success(),
        "spsa fresh start failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    assert!(run_dir.join("state.params").exists(), "state.params missing");
    assert!(
        run_dir.join("final.params").exists(),
        "final.params missing (正常完了で生成されるはず)"
    );
    assert!(run_dir.join("meta.json").exists(), "meta.json missing");
    assert!(run_dir.join("values.csv").exists(), "values.csv missing");
    assert!(run_dir.join("stats.csv").exists(), "stats.csv missing");
    // .lock は正常終了時に Drop で削除される
    assert!(!run_dir.join(".lock").exists(), ".lock should be removed after normal exit");

    // values.csv: header + iter 0 snapshot + iter 1 = 3 行
    let values_lines = count_lines(&run_dir.join("values.csv"));
    assert_eq!(values_lines, 3, "values.csv should have header + iter0 + iter1");

    let meta: serde_json::Value =
        serde_json::from_slice(&std::fs::read(run_dir.join("meta.json")).unwrap()).unwrap();
    assert_eq!(meta["completed_iterations"], 1);
    assert_eq!(meta["format_version"], 4);
    // current_params_sha256 は v4 で必須
    assert!(meta["current_params_sha256"].as_str().unwrap().len() == 64);
    // init_mode = "fresh-init-from"
    assert_eq!(meta["init_mode"], "fresh-init-from");
}

#[test]
fn resume_appends_to_existing_run_dir() {
    let dir = tempfile::tempdir().unwrap();
    let canonical = dir.path().join("canonical.params");
    let startpos = dir.path().join("startpos.txt");
    let run_dir = dir.path().join("run");
    write_canonical(&canonical);
    write_startpos_file(&startpos);

    // 1 回目: fresh start
    let out1 = run_spsa_args(&run_dir, &canonical, &startpos, &[]);
    assert!(out1.status.success(), "fresh start failed");
    let values_after_first = count_lines(&run_dir.join("values.csv"));
    assert_eq!(values_after_first, 3); // header + iter0 + iter1

    // 2 回目: --resume で 1 iter 追加
    let out2 = run_spsa_args(&run_dir, &canonical, &startpos, &["--resume"]);
    assert!(
        out2.status.success(),
        "resume failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out2.stdout),
        String::from_utf8_lossy(&out2.stderr),
    );

    // values.csv: iter2 が append されて 4 行
    let values_after_resume = count_lines(&run_dir.join("values.csv"));
    assert_eq!(
        values_after_resume, 4,
        "values.csv should have header + iter0 + iter1 + iter2 after resume"
    );

    let meta: serde_json::Value =
        serde_json::from_slice(&std::fs::read(run_dir.join("meta.json")).unwrap()).unwrap();
    assert_eq!(meta["completed_iterations"], 2, "completed_iterations should be 2 after resume");

    // final.params が新値で上書きされていること (sha256 が state.params と一致)
    let final_bytes = std::fs::read(run_dir.join("final.params")).unwrap();
    let state_bytes = std::fs::read(run_dir.join("state.params")).unwrap();
    assert_eq!(
        final_bytes, state_bytes,
        "final.params should mirror state.params after final iter"
    );
}

#[test]
fn bail_on_existing_state_without_flags() {
    let dir = tempfile::tempdir().unwrap();
    let canonical = dir.path().join("canonical.params");
    let startpos = dir.path().join("startpos.txt");
    let run_dir = dir.path().join("run");
    write_canonical(&canonical);
    write_startpos_file(&startpos);

    // 1 回目: fresh start で state.params を作る
    let out1 = run_spsa_args(&run_dir, &canonical, &startpos, &[]);
    assert!(out1.status.success());

    // 2 回目: --init-from を渡しているが --resume も --force-init もなし → bail
    let out2 = run_spsa_args(&run_dir, &canonical, &startpos, &[]);
    assert!(!out2.status.success(), "should bail when state exists but no flag");
    let stderr = String::from_utf8_lossy(&out2.stderr);
    assert!(
        stderr.contains("--resume") && stderr.contains("--force-init"),
        "stderr should suggest --resume / --force-init: {stderr}"
    );
}

#[test]
fn force_init_resets_run_dir_and_clears_stale_final_params() {
    let dir = tempfile::tempdir().unwrap();
    let canonical = dir.path().join("canonical.params");
    let startpos = dir.path().join("startpos.txt");
    let run_dir = dir.path().join("run");
    write_canonical(&canonical);
    write_startpos_file(&startpos);

    // 1 回目: fresh start で final.params を残す
    let out1 = run_spsa_args(&run_dir, &canonical, &startpos, &[]);
    assert!(out1.status.success());
    let stale_final_size = std::fs::metadata(run_dir.join("final.params")).unwrap().len();
    assert!(stale_final_size > 0);

    // canonical を別内容で書き換え (旧 final.params の値とは異なる)
    let new_body = "\
SPSA_TEST_INT,int,9,0,10,1,0.001 //changed
SPSA_TEST_FLOAT,float,2.5,0.0,3.0,0.5,0.001 //changed
";
    std::fs::write(&canonical, new_body).unwrap();

    // 2 回目: --force-init で reset
    let out2 = run_spsa_args(&run_dir, &canonical, &startpos, &["--force-init"]);
    assert!(
        out2.status.success(),
        "force-init failed:\nstderr={}",
        String::from_utf8_lossy(&out2.stderr),
    );

    // values.csv は header + iter0 + iter1 の 3 行に reset (旧 4 行ではない)
    let values_lines = count_lines(&run_dir.join("values.csv"));
    assert_eq!(values_lines, 3, "values.csv should be reset to 3 lines after force-init");

    // final.params も新 canonical 値ベース (旧値の残留がない)
    let final_body = std::fs::read_to_string(run_dir.join("final.params")).unwrap();
    assert!(
        final_body.contains("SPSA_TEST_INT,int,9")
            || final_body.contains("SPSA_TEST_INT,int,8")
            || final_body.contains("SPSA_TEST_INT,int,10"),
        "final.params should reflect new canonical (close to 9 after 1 iter): {final_body}"
    );
}
