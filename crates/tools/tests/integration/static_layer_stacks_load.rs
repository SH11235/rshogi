//! LayerStacks 診断ツールが静的 net を読めることの回帰テスト。
//!
//! `crates/tools` は `rshogi-core` を `default-features = false` で引くが、同じビルドの
//! `rshogi-csa-client` / `rshogi-csa-server` が default features 付きで引くため、cargo の
//! feature 統合で `nnue-runtime-dimensions` (universal edition) が有効になる。修正前は
//! その構成で `NNUENetwork::load` が LayerStacks net を `DynamicLayerStacks` として返し、
//! 静的 net を前提にした診断ツールが「LayerStacks NNUE のみ対応」/
//! 「Expected LayerStacks network」で落ちていた。
//!
//! このテストは default features (= 統合で runtime-dimensions が有効になる構成) で走り、
//! 各ツールが静的 LayerStacks reader 経由で合成 net を読めることを確認する。

use std::process::{Command, Output};

use rshogi_core::nnue::HALFKA_HM_DIMENSIONS;
use rshogi_core::nnue::net_delta::test_utils::build_synthetic_layer_stacks;
use rshogi_core::position::SFEN_HIRATE;
use tempfile::{TempDir, tempdir};

/// 7g7f 後の局面 (SFEN_HIRATE と別特徴量になる 2 局面目)。
const SFEN_AFTER_7G7F: &str = "lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL w - 2";

/// 合成 net は L1=1536 / L2=16 / L3=32、FT は HalfKaHmMerged、活性化は CReLU。
/// 静的 net の `ArchitectureSpec::name()` はこの文字列になる。
const EXPECTED_STATIC_ARCH_NAME: &str = "LayerStacks-1536-16-32-CReLU";

/// 修正前に出ていた「静的 LS net を読めなかった」ことを示すエラー文字列。
/// 新しい context 文言と、旧文言 (退行検出用) の両方を見る。
const STATIC_LOAD_FAILURE_MARKERS: [&str; 3] = [
    "静的 LayerStacks NNUE を読み込めません",
    "LayerStacks NNUE のみ対応",
    "Expected LayerStacks network",
];

/// 合成 net と SFEN ファイルを持つ fixture。
struct Fixture {
    /// 合成 net と SFEN を置く一時 dir。drop で削除される。
    temp: TempDir,
}

impl Fixture {
    /// tools ビルド (nnue-arch) で静的に有効な LS variant は
    /// ft-halfka_hm_merged × layerstacks-1536x16x32。
    /// num_buckets = 1 なら progresskpabs routing は no-op なので progress 係数が要らない。
    fn new() -> Self {
        Self::with_buckets(1)
    }

    fn with_buckets(buckets: usize) -> Self {
        let fixture = Self {
            temp: tempdir().expect("tempdir"),
        };
        let synthetic = build_synthetic_layer_stacks(
            "HalfKaHmMerged",
            HALFKA_HM_DIMENSIONS,
            1536,
            16,
            32,
            buckets,
        );
        std::fs::write(fixture.net(), &synthetic.bytes).expect("write synthetic net");
        std::fs::write(fixture.sfens(), format!("{SFEN_HIRATE}\n{SFEN_AFTER_7G7F}\n"))
            .expect("write sfens");
        fixture
    }

    fn file(&self, name: &str) -> String {
        let path = self.temp.path().join(name);
        path.to_str().expect("temp path is valid UTF-8").to_string()
    }

    fn net(&self) -> String {
        self.file("synthetic_ls.bin")
    }

    fn sfens(&self) -> String {
        self.file("sfens.txt")
    }
}

/// `NNUE_ARCHITECTURE` は現状 USI option (process 内 global) で環境変数からは読まないため
/// tools 内では常に `Auto` だが、override が LayerStacks 系以外だと静的読み込みは
/// 正しく reject される。子プロセスが環境に左右されないよう明示的に除去して起動する。
fn run(binary: &str, args: &[&str]) -> Output {
    Command::new(binary)
        .args(args)
        .env_remove("NNUE_ARCHITECTURE")
        .output()
        .expect("run command")
}

/// exit 0 かつ「静的 LS net を読めなかった」痕跡がないことを確認し、stdout を返す。
fn assert_static_layer_stacks_success(label: &str, output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "{label}: status={}\nstdout={stdout}\nstderr={stderr}",
        output.status
    );
    for marker in STATIC_LOAD_FAILURE_MARKERS {
        assert!(
            !stdout.contains(marker) && !stderr.contains(marker),
            "{label}: 静的 LayerStacks net の読み込みに失敗した ('{marker}')\n\
             stdout={stdout}\nstderr={stderr}"
        );
    }
    stdout
}

#[test]
fn nnue_saturation_loads_static_layer_stacks() {
    let fixture = Fixture::new();
    let output = run(
        env!("CARGO_BIN_EXE_nnue_saturation"),
        &[
            "--nnue",
            &fixture.net(),
            "--sfens",
            &fixture.sfens(),
            "--progress-buckets",
            "1",
            "--count",
            "2",
        ],
    );
    let stdout = assert_static_layer_stacks_success("nnue_saturation", &output);
    assert!(stdout.contains("\"ft_rate\""), "飽和率 JSON が出ていない:\n{stdout}");
    assert!(stdout.contains("\"positions\": 2"), "2 局面を集計していない:\n{stdout}");
}

#[test]
fn eval_sfens_loads_static_layer_stacks() {
    let fixture = Fixture::new();
    let output = run(
        env!("CARGO_BIN_EXE_eval_sfens"),
        &[
            "--nnue",
            &fixture.net(),
            "--sfens",
            &fixture.sfens(),
            "--progress-buckets",
            "1",
            "--count",
            "2",
        ],
    );
    let stdout = assert_static_layer_stacks_success("eval_sfens", &output);
    assert!(
        stdout.contains("sfen\tbucket\traw\tscore\tscore_cp"),
        "評価テーブルのヘッダ行が無い:\n{stdout}"
    );
    assert!(stdout.contains(SFEN_HIRATE), "平手局面の行が無い:\n{stdout}");
}

#[test]
fn verify_nnue_accumulator_loads_static_layer_stacks() {
    let fixture = Fixture::new();
    let output = run(
        env!("CARGO_BIN_EXE_verify_nnue_accumulator"),
        &[
            "--nnue-file",
            &fixture.net(),
            "--ls-bucket-mode",
            "progresskpabs",
            "--ls-progress-buckets",
            "1",
            "--moves",
            "2",
        ],
    );
    let stdout = assert_static_layer_stacks_success("verify_nnue_accumulator", &output);
    assert!(
        stdout.contains("ALL PASSED"),
        "refresh/update 一致テストが通っていない:\n{stdout}"
    );
}

#[test]
fn bench_nnue_eval_layer_stack_mode_loads_static_layer_stacks() {
    let fixture = Fixture::new();
    let output = run(
        env!("CARGO_BIN_EXE_bench_nnue_eval"),
        &[
            "--nnue-file",
            &fixture.net(),
            "--mode",
            "layer-stack-eval",
            "--warmup",
            "1",
            "--iterations",
            "1",
            "--ls-bucket-mode",
            "progresskpabs",
            "--ls-progress-buckets",
            "1",
        ],
    );
    let stdout = assert_static_layer_stacks_success("bench_nnue_eval", &output);
    // arch ラベルは静的 net の spec 名。build 構成で変わらないこと。
    assert!(
        stdout.contains(&format!("Architecture: {EXPECTED_STATIC_ARCH_NAME}")),
        "arch ラベルが静的 spec 名になっていない:\n{stdout}"
    );
}

#[test]
fn diagnostic_cli_rejects_missing_coefficients_with_its_own_flags() {
    let fixture = Fixture::with_buckets(8);
    for binary in [
        env!("CARGO_BIN_EXE_eval_sfens"),
        env!("CARGO_BIN_EXE_nnue_saturation"),
    ] {
        let output = run(
            binary,
            &[
                "--nnue",
                &fixture.net(),
                "--sfens",
                &fixture.sfens(),
                "--progress-buckets",
                "8",
                "--count",
                "1",
            ],
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{stderr}");
        assert!(stderr.contains("requires progress coefficients"), "{stderr}");
        assert!(stderr.contains("--progress-coeff"), "{stderr}");
        assert!(!stderr.contains("--ls-progress-coeff"), "{stderr}");
    }
}

#[test]
fn single_bucket_does_not_ignore_invalid_coefficients() {
    let fixture = Fixture::new();
    let coefficients = fixture.file("invalid-coeff.bin");
    std::fs::write(&coefficients, [0_u8; 3]).unwrap();
    let output = run(
        env!("CARGO_BIN_EXE_eval_sfens"),
        &[
            "--nnue",
            &fixture.net(),
            "--sfens",
            &fixture.sfens(),
            "--progress-buckets",
            "1",
            "--progress-coeff",
            &coefficients,
        ],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(stderr.contains("progress coeff size mismatch"), "{stderr}");
    let output = run(
        env!("CARGO_BIN_EXE_verify_nnue_accumulator"),
        &[
            "--nnue-file",
            &fixture.net(),
            "--ls-bucket-mode",
            "progresskpabsq16",
            "--ls-progress-buckets",
            "1",
            "--ls-progress-coeff",
            &coefficients,
        ],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(stderr.contains("Invalid progress coeff size"), "{stderr}");
}

#[test]
fn q16_single_bucket_runs_without_coefficients() {
    let fixture = Fixture::new();
    let (net, sfens) = (fixture.net(), fixture.sfens());
    let args = [
        "--nnue",
        &net,
        "--sfens",
        &sfens,
        "--bucket-mode",
        "progresskpabsq16",
        "--progress-buckets",
        "1",
    ];
    for binary in [
        env!("CARGO_BIN_EXE_eval_sfens"),
        env!("CARGO_BIN_EXE_nnue_saturation"),
    ] {
        assert_static_layer_stacks_success(binary, &run(binary, &args));
    }
}

#[test]
fn q16_routing_runs_on_diagnostic_tools() {
    use rshogi_core::nnue::{
        SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS, compute_progresskpabs_q16_sum,
        load_progress_coeff_kpabs_q16_from_bytes, progress_q16_sum_to_bucket,
    };
    use rshogi_core::position::Position;

    let fixture = Fixture::with_buckets(8);
    // 合成 net では評価値に差が出にくいので、bucket が局面ごとに変わる大きめの係数にする。
    let bytes: Vec<u8> = (0..SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS)
        .flat_map(|i| (((i * 7919) % 2001) as f64 / 1000.0 - 1.0).to_le_bytes())
        .collect();
    let coefficients = fixture.file("progress.bin");
    std::fs::write(&coefficients, &bytes).unwrap();
    let q16 = load_progress_coeff_kpabs_q16_from_bytes(&bytes).unwrap();

    let output = run(
        env!("CARGO_BIN_EXE_eval_sfens"),
        &[
            "--nnue",
            &fixture.net(),
            "--sfens",
            &fixture.sfens(),
            "--bucket-mode",
            "progresskpabsq16",
            "--progress-buckets",
            "8",
            "--progress-coeff",
            &coefficients,
        ],
    );
    let stdout = assert_static_layer_stacks_success("eval_sfens q16", &output);
    let mut rows = 0;
    for line in stdout.lines().skip(1) {
        let mut columns = line.split('\t');
        let sfen = columns.next().unwrap();
        let bucket: usize = columns.next().unwrap().parse().unwrap();
        let mut pos = Position::new();
        pos.set_sfen(sfen).unwrap();
        assert_eq!(
            bucket,
            progress_q16_sum_to_bucket(compute_progresskpabs_q16_sum(&pos, &q16), 8)
        );
        rows += 1;
    }
    assert_eq!(rows, 2, "{stdout}");

    let output = run(
        env!("CARGO_BIN_EXE_nnue_saturation"),
        &[
            "--nnue",
            &fixture.net(),
            "--sfens",
            &fixture.sfens(),
            "--bucket-mode",
            "progresskpabsq16",
            "--progress-buckets",
            "8",
            "--progress-coeff",
            &coefficients,
        ],
    );
    let stdout = assert_static_layer_stacks_success("nnue_saturation q16", &output);
    assert!(stdout.contains("\"bucket_mode\": \"progresskpabsq16\""), "{stdout}");

    let output = run(
        env!("CARGO_BIN_EXE_verify_nnue_accumulator"),
        &[
            "--nnue-file",
            &fixture.net(),
            "--ls-bucket-mode",
            "progresskpabsq16",
            "--ls-progress-buckets",
            "8",
            "--ls-progress-coeff",
            &coefficients,
        ],
    );
    assert_static_layer_stacks_success("verify_nnue_accumulator q16", &output);
}
