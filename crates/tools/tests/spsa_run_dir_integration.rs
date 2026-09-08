//! golden 生成（基準 SHA 8a740655、mock は本 worktree でビルド）:
//! ```text
//! cargo build -p tools --bin spsa_test_engine
//! BASELINE=/path/to/baseline/spsa.exe
//! MOCK="$PWD/target/debug/spsa_test_engine.exe" # Linux は .exe を外す
//! GOLDEN=crates/tools/tests/golden/spsa
//! OUT="target/spsa-golden-$(date +%s)"
//! for mode in cyclic random; do
//!   random=false; [ "$mode" = random ] && random=true
//!   for concurrency in 1 3; do
//!     run="$OUT/$mode-$concurrency"
//!     SPSA_TEST_ENGINE_MODE=value_driven "$BASELINE" \
//!       --run-dir "$run" --engine-path "$MOCK" \
//!       --init-from "$GOLDEN/canonical.params" --startpos-file "$GOLDEN/startpos.txt" \
//!       --total-pairs 4 --batch-pairs 2 --concurrency "$concurrency" \
//!       --seed 1 --nodes 1000 --threads 1 --hash-mb 16 --random-startpos "$random" || exit 1
//!     mkdir -p "$GOLDEN/$mode-$concurrency"
//!     cp "$run/values.csv" "$run/stats.csv" "$run/state.params" "$GOLDEN/$mode-$concurrency/"
//!   done
//! done
//! cargo test -p tools --test spsa_run_dir_integration value_driven_matches_fixed_sha_golden
//! ```
//! 詳細・互換性範囲は docs/spsa_runbook.md の §14 を参照。
//!
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

/// SPSA を起動するヘルパ。`total_pairs` は常に `--total-pairs` として渡し、
/// 追加の CLI 引数は `extra` で付与する。
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
        stderr.contains("v4") && stderr.contains("spsa_runbook.md"),
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

/// v3 silent migrate 時、CLI の `--batch-pairs` が v3 meta から推定される値と
/// 一致しないと bail することを確認 (silent failure 防止の中核ガード)。
#[test]
fn v3_silent_migrate_bails_when_batch_pairs_mismatches_estimate() {
    let dir = tempfile::tempdir().unwrap();
    let canonical = dir.path().join("canonical.params");
    let startpos = dir.path().join("startpos.txt");
    let run_dir = dir.path().join("run");
    write_canonical(&canonical);
    write_startpos_file(&startpos);
    std::fs::create_dir_all(&run_dir).unwrap();

    // 1 batch (total_pairs=1, batch_pairs=1) で完走 → completed_iterations=1, total_games=2。
    // 推定 batch_pairs = total_games / completed_iterations / 2 = 1。
    let out1 = run_spsa_args(&run_dir, &canonical, &startpos, &[]);
    assert!(out1.status.success());

    // meta を v3 形式に書き換える (silent migrate を発動させる)。
    let meta_path = run_dir.join("meta.json");
    let mut v: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&meta_path).unwrap()).unwrap();
    let m = v.as_object_mut().unwrap();
    m.insert("format_version".into(), serde_json::json!(3));
    m.remove("total_pairs");
    m.remove("batch_pairs");
    m.remove("completed_pairs");
    std::fs::write(&meta_path, serde_json::to_vec_pretty(&v).unwrap()).unwrap();

    // CLI で --batch-pairs を **意図的に推定値と異なる 4** にして resume を試みる。
    // 推定 batch_pairs=1 vs CLI 4 → bail 必須。--force-schedule は付けない。
    let mut args: Vec<String> = vec![
        "--run-dir".into(),
        run_dir.display().to_string(),
        "--engine-path".into(),
        FAKE_ENGINE_BIN.into(),
        "--init-from".into(),
        canonical.display().to_string(),
        "--total-pairs".into(),
        "8".into(),
        "--batch-pairs".into(),
        "4".into(),
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
        "--resume".into(),
    ];
    // bail 経路では run-dir に副作用を残さないこと: 既存 stats.csv を rename
    // してから bail すると再試行時に状態が変わるので、検証 → 失敗時は何も
    // 触らない順序が必要。
    let stats_csv_before = std::fs::read(run_dir.join("stats.csv")).unwrap();
    assert!(!run_dir.join("stats.v3.csv").exists(), "事前に stats.v3.csv は無いはず");

    let out2 = Command::new(SPSA_BIN).args(&args).output().unwrap();
    assert!(
        !out2.status.success(),
        "batch_pairs 不一致では bail するはず: stderr={}",
        String::from_utf8_lossy(&out2.stderr),
    );
    // bail 後: stats.csv は元のまま。stats.v3.csv は作られていない (副作用なし)。
    let stats_csv_after = std::fs::read(run_dir.join("stats.csv")).unwrap();
    assert_eq!(
        stats_csv_before, stats_csv_after,
        "bail 経路では stats.csv が触られていないはず"
    );
    assert!(
        !run_dir.join("stats.v3.csv").exists(),
        "bail 経路では stats.v3.csv (rotate 副作用) が作られていないはず"
    );
    let stderr = String::from_utf8_lossy(&out2.stderr);
    assert!(
        stderr.contains("batch_pairs 推定値と CLI 値が不一致"),
        "推定値不一致の bail メッセージが必要: {stderr}"
    );
    assert!(stderr.contains("--force-schedule"), "force-schedule 案内が必要: {stderr}");

    // --force-schedule を付け足すと続行する (warning のみ)。
    args.push("--force-schedule".into());
    let out3 = Command::new(SPSA_BIN).args(&args).output().unwrap();
    assert!(
        out3.status.success(),
        "--force-schedule 付き resume は成功するはず: stderr={}",
        String::from_utf8_lossy(&out3.stderr),
    );
    let stderr3 = String::from_utf8_lossy(&out3.stderr);
    assert!(
        stderr3.contains("batch_pairs 推定値"),
        "force 続行時も warning は出るはず: {stderr3}"
    );
}

/// v3 silent migrate 経路で **schedule (alpha 等) が meta と不一致** で
/// `--force-schedule` 無しの bail が起きた場合も、CSV rotate (FS 副作用) が
/// 走っていないことを確認する。
///
/// batch_pairs 推定値が一致しているケースを通したあと、後続の `schedule_matches`
/// 検証で bail する経路で「副作用なし」を担保するためのガードレール。
#[test]
fn v3_silent_migrate_bails_without_csv_rotate_on_schedule_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let canonical = dir.path().join("canonical.params");
    let startpos = dir.path().join("startpos.txt");
    let run_dir = dir.path().join("run");
    write_canonical(&canonical);
    write_startpos_file(&startpos);
    std::fs::create_dir_all(&run_dir).unwrap();

    // 1 batch (推定 batch_pairs=1) を完走 → meta を v3 化。
    let out1 = run_spsa_args(&run_dir, &canonical, &startpos, &[]);
    assert!(out1.status.success());

    let meta_path = run_dir.join("meta.json");
    let mut v: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&meta_path).unwrap()).unwrap();
    let m = v.as_object_mut().unwrap();
    m.insert("format_version".into(), serde_json::json!(3));
    m.remove("total_pairs");
    m.remove("batch_pairs");
    m.remove("completed_pairs");
    // schedule.alpha を CLI のデフォルト 0.602 から **わざとズラす** ことで、
    // batch_pairs 検証は通過するが `schedule_matches` で bail させる。
    if let Some(schedule) = m.get_mut("schedule").and_then(|s| s.as_object_mut()) {
        schedule.insert("alpha".into(), serde_json::json!(0.700));
    } else {
        panic!("v3 meta に schedule オブジェクトが無い");
    }
    std::fs::write(&meta_path, serde_json::to_vec_pretty(&v).unwrap()).unwrap();

    let stats_csv_before = std::fs::read(run_dir.join("stats.csv")).unwrap();
    assert!(!run_dir.join("stats.v3.csv").exists());

    // batch_pairs=1 (推定値と一致) で resume を試みる。`--force-schedule` 無し
    // なので schedule_matches が alpha 不一致で bail するはず。
    let args: Vec<String> = vec![
        "--run-dir".into(),
        run_dir.display().to_string(),
        "--engine-path".into(),
        FAKE_ENGINE_BIN.into(),
        "--init-from".into(),
        canonical.display().to_string(),
        "--total-pairs".into(),
        "2".into(),
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
        "--resume".into(),
    ];
    let out2 = Command::new(SPSA_BIN).args(&args).output().unwrap();
    assert!(
        !out2.status.success(),
        "schedule mismatch では bail するはず: stderr={}",
        String::from_utf8_lossy(&out2.stderr),
    );
    let stderr = String::from_utf8_lossy(&out2.stderr);
    assert!(
        stderr.contains("schedule mismatch"),
        "schedule mismatch の bail メッセージが必要: {stderr}"
    );

    // bail 後: stats.csv は元のまま、stats.v3.csv は作られていない。
    let stats_csv_after = std::fs::read(run_dir.join("stats.csv")).unwrap();
    assert_eq!(
        stats_csv_before, stats_csv_after,
        "schedule mismatch bail 経路でも stats.csv が触られていないはず"
    );
    assert!(
        !run_dir.join("stats.v3.csv").exists(),
        "schedule mismatch bail 経路でも stats.v3.csv (rotate 副作用) が作られていないはず"
    );
}

/// 子の出力はファイルへ流し、親は独立した期限で監視する。
struct PoolTest {
    dir: tempfile::TempDir,
}

impl PoolTest {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }
    fn path(&self, name: &str) -> std::path::PathBuf {
        self.dir.path().join(name)
    }
    fn run(
        &self,
        mode: &str,
        total: u32,
        batch: u32,
        concurrency: usize,
        extra: &[&str],
        env: &[(&str, &Path)],
    ) -> std::process::Output {
        use std::process::Stdio;
        use std::time::{Duration, Instant};
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/spsa");
        let stdout = std::fs::File::create(self.path("stdout")).unwrap();
        let stderr = std::fs::File::create(self.path("stderr")).unwrap();
        let mut command = Command::new(SPSA_BIN);
        command
            .args(["--run-dir"])
            .arg(self.path("run"))
            .arg("--engine-path")
            .arg(FAKE_ENGINE_BIN)
            .arg("--init-from")
            .arg(fixtures.join("canonical.params"))
            .arg("--startpos-file")
            .arg(fixtures.join("startpos.txt"))
            .args([
                "--total-pairs",
                &total.to_string(),
                "--batch-pairs",
                &batch.to_string(),
                "--concurrency",
                &concurrency.to_string(),
                "--seed",
                "1",
                "--nodes",
                "1000",
                "--threads",
                "1",
                "--hash-mb",
                "16",
            ])
            .args(extra)
            .env("SPSA_TEST_ENGINE_MODE", mode)
            .env("SPSA_TEST_ENGINE_SPAWN_LOG", self.path("spawns"))
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
        for (name, value) in env {
            command.env(name, value);
        }
        let mut child = TestChild {
            child: command.spawn().unwrap(),
            spawn_log: self.path("spawns"),
            mocks: Vec::new(),
        };
        let deadline = Instant::now() + Duration::from_secs(45);
        let status = loop {
            child.record_mocks();
            if let Some(status) = child.child.try_wait().unwrap() {
                // 最後の poll 後に spawn log へ現れた個体も記録する。
                child.record_mocks();
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "spsa parent deadline exceeded: {}",
                std::fs::read_to_string(self.path("stderr")).unwrap()
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        let system = sysinfo::System::new_all();
        for mock in &child.mocks {
            assert!(
                system.process(mock.pid).is_none_or(|process| !mock.matches(process)),
                "engine survived parent exit: {}",
                mock.pid
            );
        }
        std::process::Output {
            status,
            stdout: std::fs::read(self.path("stdout")).unwrap(),
            stderr: std::fs::read(self.path("stderr")).unwrap(),
        }
    }
    fn bytes(&self, file: &str) -> Vec<u8> {
        std::fs::read(self.path("run").join(file)).unwrap()
    }
    fn meta(&self) -> serde_json::Value {
        serde_json::from_slice(&self.bytes("meta.json")).unwrap()
    }
}

struct TestChild {
    child: std::process::Child,
    spawn_log: std::path::PathBuf,
    mocks: Vec<MockIdentity>,
}

struct MockIdentity {
    pid: sysinfo::Pid,
    start_time: u64,
    exe_name: std::ffi::OsString,
}

impl MockIdentity {
    fn matches(&self, process: &sysinfo::Process) -> bool {
        process.pid() == self.pid
            && process.start_time() == self.start_time
            && process.exe().and_then(Path::file_name) == Some(self.exe_name.as_os_str())
    }
}

impl TestChild {
    fn record_mocks(&mut self) {
        // この run 専用の spawn log が所有関係の根拠。PPID は再親付けで変わるため使わない。
        // 親の待機中から個体情報を保持し、PID 再利用時に別個体を回収しない。
        let pids: Vec<_> = std::fs::read_to_string(&self.spawn_log)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| line.parse::<usize>().ok().map(sysinfo::Pid::from))
            .filter(|pid| !self.mocks.iter().any(|mock| mock.pid == *pid))
            .collect();
        if pids.is_empty() {
            return;
        }
        let mut system = sysinfo::System::new();
        system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&pids), true);
        for pid in pids {
            if let Some(process) = system.process(pid)
                && let Some(exe) = process.exe().filter(|exe| is_mock_engine(exe))
                && let Some(exe_name) = exe.file_name()
            {
                self.mocks.push(MockIdentity {
                    pid,
                    start_time: process.start_time(),
                    exe_name: exe_name.to_owned(),
                });
            }
        }
    }
}

impl Drop for TestChild {
    fn drop(&mut self) {
        self.record_mocks();
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.record_mocks();
        // engine は Unix では別 process group。親だけでなく記録した子も回収する。
        let system = sysinfo::System::new_all();
        for mock in &self.mocks {
            if let Some(process) = system.process(mock.pid)
                && mock.matches(process)
            {
                process.kill();
            }
        }
    }
}

fn assert_success(output: &std::process::Output) {
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
}

#[test]
fn recorded_mock_identity_survives_parent_exit_and_drop_reclaims_it() {
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    let dir = tempfile::tempdir().unwrap();
    let spawn_log = dir.path().join("spawns");
    let mut parent = TestChild {
        child: Command::new(FAKE_ENGINE_BIN).stdin(Stdio::piped()).spawn().unwrap(),
        spawn_log: spawn_log.clone(),
        mocks: Vec::new(),
    };
    // 別 PPID の mock を run 専用ログで関連付け、現在の PPID に依存しない検査を再現する。
    // mock 自身もガードで所有し、assert が失敗しても残存させない。
    let mut engine = TestChild {
        child: Command::new(FAKE_ENGINE_BIN)
            .stdin(Stdio::piped())
            .env("SPSA_TEST_ENGINE_SPAWN_LOG", &spawn_log)
            .spawn()
            .unwrap(),
        spawn_log: dir.path().join("unused"),
        mocks: Vec::new(),
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while parent.mocks.is_empty() {
        assert!(parent.child.try_wait().unwrap().is_none());
        parent.record_mocks();
        assert!(Instant::now() < deadline, "mock identity not recorded");
        std::thread::sleep(Duration::from_millis(10));
    }
    parent.child.kill().unwrap();
    parent.child.wait().unwrap();
    let system = sysinfo::System::new_all();
    let process = system.process(sysinfo::Pid::from_u32(engine.child.id())).unwrap();
    let identity = &mut parent.mocks[0];
    assert!(identity.matches(process));
    identity.start_time += 1;
    assert!(!identity.matches(process), "reused PID must not match");
    identity.start_time -= 1;
    let exe_name = identity.exe_name.clone();
    identity.exe_name = "different-engine".into();
    assert!(!identity.matches(process));
    identity.exe_name = exe_name;
    let pid = identity.pid;
    identity.pid = sysinfo::Pid::from_u32(parent.child.id());
    assert!(!identity.matches(process));
    identity.pid = pid;
    drop(parent);
    let deadline = Instant::now() + Duration::from_secs(5);
    while engine.child.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "recorded mock was not reclaimed");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn is_mock_engine(path: &Path) -> bool {
    path.file_name().zip(Path::new(FAKE_ENGINE_BIN).file_name()).is_some_and(
        |(actual, expected)| {
            actual.to_string_lossy().eq_ignore_ascii_case(&expected.to_string_lossy())
        },
    )
}

#[test]
fn fatal_error_cancels_before_engine_drop_and_next_go() {
    let test = PoolTest::new();
    let output = test.run(
        "cancel_script",
        2,
        2,
        2,
        &["--engine-retries", "0", "--nodes-timeout-ms", "20000"],
        &[
            ("SPSA_TEST_ENGINE_SCRIPT_GATE", &test.path("gate")),
            ("SPSA_TEST_ENGINE_PROTOCOL_LOG", &test.path("protocol")),
        ],
    );
    assert!(!output.status.success());
    assert!(test.path("gate.waiting").exists());
    assert!(test.path("gate.quit").exists());
    let log = std::fs::read_to_string(test.path("protocol")).unwrap();
    assert_eq!(log.lines().filter(|line| line.contains(" go ")).count(), 2, "{log}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("batch=1 game_id="), "{stderr}");
    let final_error = stderr.rsplit_once("Error:").expect("final error missing").1;
    assert!(final_error.contains("engine exited unexpectedly"), "{stderr}");
    assert!(!final_error.contains("worker cancelled"), "{stderr}");
    assert_eq!(test.meta()["completed_pairs"], 0);
}

#[test]
fn active_parameters_are_resent_in_protocol_order_each_game() {
    let test = PoolTest::new();
    assert_success(&test.run(
        "value_driven",
        4,
        2,
        1,
        &[],
        &[("SPSA_TEST_ENGINE_PROTOCOL_LOG", &test.path("protocol"))],
    ));
    let log = std::fs::read_to_string(test.path("protocol")).unwrap();
    let mut engines = std::collections::BTreeMap::<&str, Vec<&str>>::new();
    for line in log.lines() {
        let (pid, command) = line.split_once(' ').unwrap();
        engines.entry(pid).or_default().push(command);
    }
    assert_eq!(engines.len(), 2);
    // seed=1 の batch 1 では INT の flip が +1 なので、初回 INT が大きい個体が plus。
    let plus_pid = *engines
        .iter()
        .max_by_key(|(_, commands)| {
            commands
                .iter()
                .find_map(|command| {
                    command
                        .strip_prefix("setoption name SPSA_TEST_INT value ")
                        .map(|value| value.parse::<i64>().unwrap())
                })
                .unwrap()
        })
        .unwrap()
        .0;
    let minus_pid = *engines.keys().find(|pid| **pid != plus_pid).unwrap();
    // 個体別の初期化を除外した共有ログを、その到着順のまま比較する。
    let mut initialized = std::collections::BTreeSet::new();
    let mut preparation = Vec::new();
    for line in log.lines() {
        let (pid, command) = line.split_once(' ').unwrap();
        if !initialized.contains(pid) {
            if command == "usinewgame" {
                initialized.insert(pid);
            }
            continue;
        }
        let kind = if command.starts_with("setoption name SPSA_TEST_INT ") {
            "int"
        } else if command.starts_with("setoption name SPSA_TEST_FLOAT ") {
            "float"
        } else if matches!(command, "isready" | "usinewgame") {
            command
        } else {
            continue;
        };
        preparation.push((pid, kind));
    }
    let expected = [
        (plus_pid, "int"),
        (plus_pid, "float"),
        (plus_pid, "isready"),
        (minus_pid, "int"),
        (minus_pid, "float"),
        (minus_pid, "isready"),
        (plus_pid, "usinewgame"),
        (plus_pid, "isready"),
        (minus_pid, "usinewgame"),
        (minus_pid, "isready"),
    ];
    assert_eq!(preparation.len(), 8 * expected.len());
    for (game, commands) in preparation.chunks_exact(expected.len()).enumerate() {
        assert_eq!(commands, expected, "game={game}\n{log}");
    }
    let mut vectors = Vec::new();
    for commands in engines.values() {
        let mut games = Vec::new();
        // initialize が送る最初の usinewgame は game task の準備と別。
        let initial = commands.iter().position(|command| *command == "usinewgame").unwrap();
        assert_eq!(commands[initial - 1], "isready");
        for (index, command) in commands.iter().enumerate().skip(initial + 1) {
            if *command != "usinewgame" {
                continue;
            }
            assert_eq!(commands[index - 1], "isready");
            assert_eq!(commands[index + 1], "isready");
            let int = commands[index - 3]
                .strip_prefix("setoption name SPSA_TEST_INT value ")
                .unwrap()
                .parse::<i64>()
                .unwrap();
            let float = commands[index - 2]
                .strip_prefix("setoption name SPSA_TEST_FLOAT value ")
                .unwrap()
                .parse::<f64>()
                .unwrap();
            games.push((int, float));
        }
        assert_eq!(games.len(), 8);
        for batch in games.chunks_exact(4) {
            assert!(batch.iter().all(|v| *v == batch[0]));
        }
        vectors.push(games);
    }
    // seed=1 の batch 1: θ=(5,1.5)、摂動=(+c,+c/2)。差分値の送信を検出する。
    vectors.sort_by_key(|games| games[0].0);
    assert_eq!(vectors[0][0].0, 4);
    assert_eq!(vectors[1][0].0, 6);
    assert_eq!(vectors[0][4].0, 4);
    assert_eq!(vectors[1][4].0, 6);
    let first_shift = 0.5 * 4_f64.powf(0.101);
    let second_shift = first_shift / 3_f64.powf(0.101);
    for (side, sign) in [(0, -1.0), (1, 1.0)] {
        assert!((vectors[side][0].1 - (1.5 + sign * first_shift)).abs() < 1e-6);
        assert!((vectors[side][4].1 - (1.503464 - sign * second_shift)).abs() < 1e-6);
    }
    assert!((vectors[0][0].1 + vectors[1][0].1 - 3.0).abs() < 1e-6);
    // batch 2 では更新済み θ と逆符号の FLOAT 摂動が両側へ反映される。
    assert!(vectors[0][0].1 < 1.5 && vectors[1][0].1 > 1.5);
    assert!(vectors[0][4].1 > 1.503464 && vectors[1][4].1 < 1.503464);
    assert!((vectors[0][4].1 + vectors[1][4].1 - 2.0 * 1.503464).abs() < 2e-6);
}

#[test]
fn initialization_failure_retries_without_cancelling_run() {
    let test = PoolTest::new();
    let output = test.run(
        "initialize_fail_once",
        1,
        1,
        1,
        &["--engine-retries", "1"],
        &[("SPSA_TEST_ENGINE_SCRIPT_GATE", &test.path("gate"))],
    );
    assert_success(&output);
    assert_eq!(test.meta()["completed_pairs"], 1);
    assert_eq!(count_lines(&test.path("spawns")), 3);
    assert_eq!(
        String::from_utf8_lossy(&output.stderr)
            .lines()
            .filter(|line| line.starts_with("retry "))
            .count(),
        1
    );
}

#[test]
fn initialization_failure_cancels_before_engine_drop_and_next_go() {
    let test = PoolTest::new();
    let output = test.run(
        "initialize_cancel_script",
        2,
        2,
        2,
        &["--engine-retries", "0", "--nodes-timeout-ms", "20000"],
        &[
            ("SPSA_TEST_ENGINE_SCRIPT_GATE", &test.path("gate")),
            ("SPSA_TEST_ENGINE_PROTOCOL_LOG", &test.path("protocol")),
        ],
    );
    assert!(!output.status.success());
    assert!(test.path("gate.waiting").exists());
    assert!(test.path("gate.quit").exists());
    let log = std::fs::read_to_string(test.path("protocol")).unwrap();
    assert_eq!(log.lines().filter(|line| line.contains(" go ")).count(), 1, "{log}");
    assert_eq!(test.meta()["completed_pairs"], 0);
    assert!(!test.path("run/final.params").exists());
}

#[test]
fn persistent_pool_spawns_four_engines_across_three_batches() {
    let test = PoolTest::new();
    assert_success(&test.run("resign", 3, 1, 2, &[], &[]));
    assert_eq!(count_lines(&test.path("spawns")), 4);
    assert!(!test.path("run/.lock").exists());
}

#[test]
fn value_driven_matches_fixed_sha_golden() {
    for (mode, random) in [("cyclic", "false"), ("random", "true")] {
        for concurrency in [1, 3] {
            let test = PoolTest::new();
            assert_success(&test.run(
                "value_driven",
                4,
                2,
                concurrency,
                &["--random-startpos", random],
                &[],
            ));
            let golden = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("tests/golden/spsa/{mode}-{concurrency}"));
            for file in ["values.csv", "stats.csv", "state.params"] {
                assert_eq!(
                    test.bytes(file),
                    std::fs::read(golden.join(file)).unwrap(),
                    "{mode} concurrency={concurrency} {file}"
                );
            }
        }
    }
}

#[test]
fn failed_once_retries_same_game_and_completes() {
    let test = PoolTest::new();
    let output = test.run(
        "value_driven",
        4,
        2,
        1,
        &["--engine-retries", "1", "--random-startpos", "false"],
        &[("SPSA_TEST_ENGINE_FAIL_ONCE_MARKER", &test.path("failed"))],
    );
    assert_success(&output);
    assert_eq!(
        String::from_utf8_lossy(&output.stderr)
            .lines()
            .filter(|line| line.starts_with("retry "))
            .count(),
        1
    );
    assert_eq!(test.meta()["completed_pairs"], 4);
    let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/spsa/cyclic-1");
    for file in ["values.csv", "stats.csv", "state.params"] {
        assert_eq!(test.bytes(file), std::fs::read(golden.join(file)).unwrap());
    }
}

#[test]
fn first_batch_failure_preserves_zero_checkpoint() {
    let test = PoolTest::new();
    let output = test.run(
        "hang_on_go",
        3,
        1,
        2,
        &[
            "--nodes-timeout-ms",
            "300",
            "--timeout-margin-ms",
            "100",
            "--engine-retries",
            "1",
        ],
        &[],
    );
    assert!(!output.status.success());
    assert_eq!(test.meta()["completed_pairs"], 0);
    assert_eq!(test.meta()["format_version"], 4);
    assert!(!test.path("run/final.params").exists());
    assert!(!test.path("run/.lock").exists());
    assert_eq!(count_lines(&test.path("run/values.csv")), 2);
    assert_eq!(count_lines(&test.path("run/stats.csv")), 1);
}

#[test]
fn second_batch_failure_resumes_without_duplicate_rows() {
    let reference = PoolTest::new();
    assert_success(&reference.run(
        "value_driven",
        4,
        1,
        1,
        &[
            "--early-stop-avg-abs-update-threshold",
            "100",
            "--early-stop-result-variance-threshold",
            "100",
            "--early-stop-patience",
            "1",
        ],
        &[],
    ));
    let test = PoolTest::new();
    // 各 engine の 2 回目の go は batch 2。sleep ではなくプロトコルの進行で注入する。
    let output = test.run(
        "value_driven",
        4,
        1,
        1,
        &["--engine-retries", "0"],
        &[
            ("SPSA_TEST_ENGINE_FAIL_ONCE_MARKER", &test.path("failed")),
            ("SPSA_TEST_ENGINE_FAIL_AFTER_GO", Path::new("1")),
        ],
    );
    assert!(!output.status.success());
    assert_eq!(test.meta()["completed_pairs"], 1);
    assert_eq!(test.meta()["current_params_sha256"], reference.meta()["current_params_sha256"]);
    for file in ["state.params", "values.csv", "stats.csv"] {
        assert_eq!(test.bytes(file), reference.bytes(file));
    }
    assert!(!test.path("run/final.params").exists());
    assert_success(&test.run("value_driven", 4, 1, 1, &["--resume"], &[]));
    assert_eq!(test.meta()["completed_pairs"], 4);
    assert_eq!(test.meta()["completed_iterations"], 4);
    assert_eq!(count_lines(&test.path("run/values.csv")), 6);
    assert_eq!(count_lines(&test.path("run/stats.csv")), 5);
}

#[test]
fn watchdog_rejects_silence_stop_bestmove_and_info_flood() {
    for mode in ["hang_on_go", "stop_then_bestmove", "info_flood"] {
        let test = PoolTest::new();
        let output = test.run(
            mode,
            1,
            1,
            1,
            &[
                "--nodes-timeout-ms",
                "300",
                "--timeout-margin-ms",
                "100",
                "--engine-retries",
                "1",
            ],
            &[],
        );
        assert!(!output.status.success(), "{mode}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        let retries: Vec<_> = stderr.lines().filter(|line| line.starts_with("retry ")).collect();
        assert_eq!(retries.len(), 1, "{stderr}");
        assert!(retries[0].contains("watchdog"), "{stderr}");
        assert_eq!(test.meta()["completed_pairs"], 0);
        assert_eq!(count_lines(&test.path("run/stats.csv")), 1);
    }
}

#[test]
fn ignored_quit_is_killed_before_parent_returns() {
    let test = PoolTest::new();
    assert_success(&test.run("ignore_quit", 1, 1, 2, &[], &[]));
    assert_eq!(test.meta()["completed_pairs"], 1);
    assert!(!test.path("run/.lock").exists());
}

#[test]
fn value_driven_same_seed_is_bit_identical() {
    let a = PoolTest::new();
    let b = PoolTest::new();
    assert_success(&a.run("value_driven", 5, 2, 3, &[], &[]));
    assert_success(&b.run("value_driven", 5, 2, 3, &[], &[]));
    assert_eq!(a.bytes("values.csv"), b.bytes("values.csv"));
    assert_eq!(a.meta()["completed_pairs"], 5);
}
