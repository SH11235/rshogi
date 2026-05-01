//! SPSA run-dir 整合性の integration test (v4 仕様)。
//!
//! 単体テストでは `decide_init_action` 等の純粋関数や個別ヘルパーは検証できる
//! が、「main loop を 1 batch 回した結果として state.params / meta.json /
//! values.csv / stats.csv が正しく生成 + 後続 batch で append される」整合性は
//! 単体では保証されない。ここでは fake USI engine (`spsa_test_engine`) を相手に
//! 実際の `spsa` バイナリをサブプロセスで起動し、run-dir の最終形を検証する。
//!
//! 検証内容 (v4):
//! - fresh start (`--total-pairs 1 --batch-pairs 1`) で run-dir 配下に
//!   必要なファイルが揃う (stats_aggregate.csv は撤去)
//! - `--resume` で同 run-dir に append され、`completed_iterations=2` (= batch
//!   番号), `completed_pairs=2`, `values.csv` に header + batch0 + batch1 + batch2
//!   が残ること
//! - 既存 state でフラグなし起動は bail
//! - force-init で派生ファイルが reset

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

/// v4 CLI で SPSA を起動。`extra` で `--total-pairs` を渡さなければ既定 1 を使う。
fn run_spsa_args_with_total_pairs(
    run_dir: &Path,
    canonical: &Path,
    startpos: &Path,
    total_pairs: u32,
    extra: &[&str],
) -> std::process::Output {
    let mut args: Vec<String> = vec![
        "--run-dir".into(),
        run_dir.display().to_string(),
        "--engine-path".into(),
        FAKE_ENGINE_BIN.into(),
        "--init-from".into(),
        canonical.display().to_string(),
        "--total-pairs".into(),
        total_pairs.to_string(),
        "--batch-pairs".into(),
        "1".into(),
        "--concurrency".into(),
        "1".into(),
        "--seed".into(),
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

/// `total_pairs=1` の薄いラッパ (大半のテストはこれで足りる)。
fn run_spsa_args(
    run_dir: &Path,
    canonical: &Path,
    startpos: &Path,
    extra: &[&str],
) -> std::process::Output {
    run_spsa_args_with_total_pairs(run_dir, canonical, startpos, 1, extra)
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
    // v4: stats_aggregate.csv は撤去
    assert!(
        !run_dir.join("stats_aggregate.csv").exists(),
        "stats_aggregate.csv は v4 で撤去されているはず"
    );
    // .lock は正常終了時に Drop で削除される
    assert!(!run_dir.join(".lock").exists(), ".lock should be removed after normal exit");

    // values.csv: header + batch 0 snapshot + batch 1 = 3 行
    let values_lines = count_lines(&run_dir.join("values.csv"));
    assert_eq!(values_lines, 3, "values.csv should have header + batch0 + batch1");

    let meta: serde_json::Value =
        serde_json::from_slice(&std::fs::read(run_dir.join("meta.json")).unwrap()).unwrap();
    assert_eq!(meta["completed_iterations"], 1);
    assert_eq!(meta["format_version"], 4);
    // v4 必須フィールド
    assert_eq!(meta["total_pairs"], 1);
    assert_eq!(meta["batch_pairs"], 1);
    assert_eq!(meta["completed_pairs"], 1);
    assert!(meta["current_params_sha256"].as_str().unwrap().len() == 64);
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

    // 1 回目: fresh start (total_pairs=1)
    let out1 = run_spsa_args(&run_dir, &canonical, &startpos, &[]);
    assert!(out1.status.success(), "fresh start failed");
    let values_after_first = count_lines(&run_dir.join("values.csv"));
    assert_eq!(values_after_first, 3); // header + batch0 + batch1

    // 2 回目: --resume で total_pairs=2 に拡張 (1 batch 追加)
    let out2 = run_spsa_args_with_total_pairs(
        &run_dir,
        &canonical,
        &startpos,
        2,
        &["--resume", "--force-schedule"],
    );
    assert!(
        out2.status.success(),
        "resume failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out2.stdout),
        String::from_utf8_lossy(&out2.stderr),
    );

    // values.csv: batch2 が append されて 4 行 (header + batch0 + batch1 + batch2)
    let values_after_resume = count_lines(&run_dir.join("values.csv"));
    assert_eq!(
        values_after_resume, 4,
        "values.csv should have header + batch0 + batch1 + batch2 after resume"
    );

    let meta: serde_json::Value =
        serde_json::from_slice(&std::fs::read(run_dir.join("meta.json")).unwrap()).unwrap();
    assert_eq!(meta["completed_iterations"], 2, "completed_iterations should be 2 after resume");
    assert_eq!(meta["total_pairs"], 2);
    assert_eq!(meta["batch_pairs"], 1);
    assert_eq!(meta["completed_pairs"], 2);

    // final.params が新値で上書きされていること (sha256 が state.params と一致)
    let final_bytes = std::fs::read(run_dir.join("final.params")).unwrap();
    let state_bytes = std::fs::read(run_dir.join("state.params")).unwrap();
    assert_eq!(
        final_bytes, state_bytes,
        "final.params should mirror state.params after final batch"
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
    let stale_final = std::fs::read(run_dir.join("final.params")).unwrap();
    assert!(!stale_final.is_empty());

    // canonical を「c_end=0 で値固定」に書き換える (force-init 後の値が
    // canonical 初期値と一致することを期待する境界条件)。
    let new_body = "\
SPSA_TEST_INT,int,9,0,10,0,0.001 //changed (c_end=0 で値固定)
SPSA_TEST_FLOAT,float,2.5,0.0,3.0,0.0,0.001 //changed (c_end=0 で値固定)
";
    std::fs::write(&canonical, new_body).unwrap();

    // 2 回目: --force-init で reset
    let out2 = run_spsa_args(&run_dir, &canonical, &startpos, &["--force-init"]);
    assert!(
        out2.status.success(),
        "force-init failed:\nstderr={}",
        String::from_utf8_lossy(&out2.stderr),
    );

    // values.csv は header + batch0 + batch1 の 3 行に reset
    let values_lines = count_lines(&run_dir.join("values.csv"));
    assert_eq!(values_lines, 3, "values.csv should be reset to 3 lines after force-init");

    // 旧 final.params のバイト列が完全に置き換わっていること
    let new_final = std::fs::read(run_dir.join("final.params")).unwrap();
    assert_ne!(new_final, stale_final, "final.params should not retain stale bytes");

    // c_end=0 schedule なので 1 batch 後も値は canonical 初期値のまま。
    // B-3 以降: is_int でも `{:.6}` 固定桁で f64 を保存する。
    let final_body = String::from_utf8(new_final).unwrap();
    assert!(
        final_body.contains("SPSA_TEST_INT,int,9.000000,"),
        "final.params should keep canonical init value with c_end=0: {final_body}"
    );
    assert!(
        final_body.contains("SPSA_TEST_FLOAT,float,2.500000,"),
        "final.params should keep canonical float init value: {final_body}"
    );
}

/// v3 multi-seed CLI (`--seeds`) は v4 で hard error になることを確認。
#[test]
fn v3_seeds_flag_rejected_with_migration_hint() {
    let dir = tempfile::tempdir().unwrap();
    let canonical = dir.path().join("canonical.params");
    let startpos = dir.path().join("startpos.txt");
    let run_dir = dir.path().join("run");
    write_canonical(&canonical);
    write_startpos_file(&startpos);

    let output = Command::new(SPSA_BIN)
        .args([
            "--run-dir",
            &run_dir.display().to_string(),
            "--engine-path",
            FAKE_ENGINE_BIN,
            "--init-from",
            &canonical.display().to_string(),
            "--total-pairs",
            "1",
            "--batch-pairs",
            "1",
            "--concurrency",
            "1",
            "--seeds",
            "1,2,3",
            "--byoyomi",
            "50",
            "--threads",
            "1",
            "--hash-mb",
            "16",
            "--startpos-file",
            &startpos.display().to_string(),
        ])
        .output()
        .unwrap();
    assert!(!output.status.success(), "--seeds は v4 で hard error のはず");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("v4") && stderr.contains("docs/spsa_v4_migration.md"),
        "v4 撤去メッセージと移行ガイドへの案内が必要: {stderr}"
    );
}

/// v3 形式の meta.json を v4 で resume できる (silent migration) ことを確認。
#[test]
fn v3_meta_silent_migrates_on_resume() {
    let dir = tempfile::tempdir().unwrap();
    let canonical = dir.path().join("canonical.params");
    let startpos = dir.path().join("startpos.txt");
    let run_dir = dir.path().join("run");
    write_canonical(&canonical);
    write_startpos_file(&startpos);
    std::fs::create_dir_all(&run_dir).unwrap();

    // v4 で 1 batch 完走させて、meta を v3 形式に書き換える。
    let out1 = run_spsa_args(&run_dir, &canonical, &startpos, &[]);
    assert!(out1.status.success());

    // meta.json を読んで format_version=4 → 3 に書き換え + v4 専用フィールド削除。
    let meta_path = run_dir.join("meta.json");
    let mut v: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&meta_path).unwrap()).unwrap();
    let m = v.as_object_mut().unwrap();
    m.insert("format_version".into(), serde_json::json!(3));
    m.remove("total_pairs");
    m.remove("batch_pairs");
    m.remove("completed_pairs");
    std::fs::write(&meta_path, serde_json::to_vec_pretty(&v).unwrap()).unwrap();

    // resume を実行。silent migrate されて成功するはず (warning は stderr に出る)。
    let out2 = run_spsa_args_with_total_pairs(
        &run_dir,
        &canonical,
        &startpos,
        2,
        &["--resume", "--force-schedule"],
    );
    assert!(
        out2.status.success(),
        "v3 → v4 silent migration が失敗: stderr={}",
        String::from_utf8_lossy(&out2.stderr),
    );
    let stderr = String::from_utf8_lossy(&out2.stderr);
    assert!(
        stderr.contains("silent migrate"),
        "silent migration の warning が出ているはず: {stderr}"
    );

    // resume 後の meta は v4 形式
    let v_after: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&meta_path).unwrap()).unwrap();
    assert_eq!(v_after["format_version"], 4);
    assert_eq!(v_after["total_pairs"], 2);
    assert_eq!(v_after["batch_pairs"], 1);
}
