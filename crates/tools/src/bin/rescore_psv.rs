//! rescore_psv - PSVファイルの評価値を再評価
//!
//! PackedSfenValueのスコアを別の評価関数やエンジンで再計算する。
//! NNUEモデルによる内部評価と、外部USIエンジンによる評価の両方をサポート。
//!
//! # 使用例
//!
//! ```bash
//! # NNUE静的評価で再スコア（高速）
//! cargo run --release -p tools --bin rescore_psv -- \
//!   --input data.psv --output-dir rescored/ \
//!   --nnue path/to/nn.bin
//!
//! # 複数ファイルを処理（glob パターン）
//! cargo run --release -p tools --bin rescore_psv -- \
//!   --input "data/*.bin" --output-dir rescored/ \
//!   --nnue path/to/nn.bin
//!
//! # qsearch評価で再スコア（より正確）
//! cargo run --release -p tools --bin rescore_psv -- \
//!   --input data.psv --output-dir rescored/ \
//!   --nnue path/to/nn.bin --use-qsearch
//!
//! # qsearch leaf置換も同時に実行
//! cargo run --release -p tools --bin rescore_psv -- \
//!   --input data.psv --output-dir rescored/ \
//!   --nnue path/to/nn.bin --apply-qsearch-leaf
//!
//! # 深さ指定探索で再スコア（最も正確だが低速）
//! cargo run --release -p tools --bin rescore_psv -- \
//!   --input "data/*.bin" --output-dir rescored/ \
//!   --nnue path/to/nn.bin \
//!   --search-depth 8 \
//!   --hash-mb 256 \
//!   --threads 4
//!
//! # 外部USIエンジン（DLshogi系等）で再スコア（知識蒸留用）
//! cargo run --release -p tools --bin rescore_psv -- \
//!   --input data.psv --output-dir rescored/ \
//!   --engine /path/to/dlshogi_aoba/usi/bin/usi \
//!   --engine-nodes 1 \
//!   --usi-option "DNN_Model=/path/to/model.onnx" \
//!   --usi-option "UCT_Threads=1" \
//!   --usi-option "DNN_Batch_Size=8"
//! ```

use anyhow::{Context, Result};
use clap::Parser;
use glob::glob;
use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressState, ProgressStyle};
use rayon::prelude::*;
use std::cell::RefCell;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, IsTerminal, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use rshogi_core::nnue::init_nnue;
use rshogi_core::position::Position;
use rshogi_core::search::{LimitsType, Search};
use tools::packed_sfen::{PackedSfenValue, pack_position, unpack_sfen};
use tools::qsearch_pv::{NnueStacks, apply_pv, qsearch_with_pv_nnue};

/// 探索用スタックサイズ（64MB）
const SEARCH_STACK_SIZE: usize = 64 * 1024 * 1024;

/// PSVファイルの評価値を再評価
#[derive(Parser)]
#[command(
    name = "rescore_psv",
    version,
    about = "PSVファイルの評価値を再評価\n\n内部NNUE評価または外部USIエンジンで局面を再評価するツール"
)]
struct Cli {
    /// 入力PSVファイル（複数指定可、globパターン対応）
    /// 例: --input file1.bin --input file2.bin
    /// 例: --input "data/*.bin"
    #[arg(short, long, required = true, num_args = 1..)]
    input: Vec<String>,

    /// 出力ディレクトリ（入力ファイル名で出力）
    #[arg(short, long)]
    output_dir: PathBuf,

    /// NNUEモデルファイル（--engine未使用時に必須）
    #[arg(long)]
    nnue: Option<PathBuf>,

    /// qsearch評価を使用（デフォルトは静的評価）
    #[arg(long)]
    use_qsearch: bool,

    /// 深さ指定探索を使用（--use-qsearchと排他）
    /// 指定した深さでalpha-beta探索を実行し、その結果をスコアとして使用
    #[arg(long)]
    search_depth: Option<i32>,

    /// 置換表サイズ（MB）、--search-depth使用時のみ有効
    #[arg(long, default_value_t = 64)]
    hash_mb: usize,

    /// 探索ノード数の上限（0=無制限）、--search-depth使用時のみ有効
    /// 複雑な局面での探索時間爆発を防ぐため、100万〜1000万程度を推奨
    #[arg(long, default_value_t = 0)]
    max_nodes: u64,

    /// 1局面あたりの探索時間上限（ミリ秒、0=無制限）、--search-depth使用時のみ有効
    /// 複雑な局面での探索時間爆発を防ぐため、1000〜10000程度を推奨
    #[arg(long, default_value_t = 0)]
    max_time: i64,

    /// qsearch leaf置換も同時に適用
    #[arg(long)]
    apply_qsearch_leaf: bool,

    /// root 局面を保持したまま、ラベルだけを qsearch 葉の評価にする（局面は置換しない）。
    /// `--nnue`（葉探索用）＋ `--dlshogi-onnx-model`/`--onnx-model`（葉ラベル用）と併用する。
    /// DL 系の静的評価でラベル付けする教師生成で、葉の評価を root 局面に付与する用途。
    /// `--apply-qsearch-leaf`（局面置換）とは併用不可。
    #[arg(long)]
    qsearch_leaf_label: bool,

    /// `--qsearch-leaf-label` と併用し、同一 1 パスで葉局面に置換したレコードを
    /// 別ディレクトリにも書き出す（leaf-REPLACEMENT arm）。
    /// `--output-dir` 側は root 局面 + 葉ラベル（leaf-LABEL arm）のまま。
    /// このディレクトリには葉局面 + 葉評価（符号反転なし）を書き出す。
    /// `--apply-qsearch-leaf` → DL rescore の 2 工程と bit 一致する。
    /// `--output-dir` とは別ディレクトリ必須。
    #[arg(long)]
    qsearch_leaf_replacement_output: Option<PathBuf>,

    /// qsearchの最大深さ
    #[arg(long, default_value_t = 16)]
    max_ply: i32,

    /// 並列処理スレッド数（0=自動）
    #[arg(short, long, default_value_t = 0)]
    threads: usize,

    /// 処理するレコード数の上限（0=無制限）
    #[arg(long, default_value_t = 0)]
    limit: u64,

    /// スコアのクリップ範囲（±この値にクリップ）
    #[arg(long, default_value_t = 10000)]
    score_clip: i16,

    /// 王手局面をスキップ（出力から除外）
    #[arg(long)]
    skip_in_check: bool,

    /// 入力NNUEのFV_SCALE（nn.bin=24, nnue-pytorch形式=16）
    /// 注意: スコアはセンチポーン単位で出力されるため、通常は変換不要
    #[arg(long, default_value_t = 24)]
    source_fv_scale: i32,

    /// 出力スコアのFV_SCALE
    /// デフォルトではsource_fv_scaleと同じ（変換なし）
    /// nnue-pytorchはセンチポーン単位のスコアをそのまま使用するため、
    /// 通常は変換不要。16に変更すると1.5倍のスケーリングが適用される。
    #[arg(long, default_value_t = 24)]
    target_fv_scale: i32,

    /// 詳細出力
    #[arg(short, long)]
    verbose: bool,

    /// 処理完了後に入力ファイルを削除
    /// ディスク容量節約のため、各ファイルの処理完了後に入力を削除
    #[arg(long)]
    delete_input: bool,

    // --- 外部USIエンジンモード ---
    /// 外部USIエンジンのパス（DLshogi系等）
    /// 指定すると内部NNUEの代わりに外部エンジンで評価
    #[arg(long)]
    engine: Option<PathBuf>,

    /// エンジンの探索ノード数（--engine使用時、0=depth 1）
    #[arg(long, default_value_t = 1)]
    engine_nodes: u64,

    /// USIオプション（"Name=Value"形式、複数指定可）
    /// 例: --usi-option "DNN_Model=model.onnx" --usi-option "UCT_Threads=1"
    #[arg(long = "usi-option")]
    usi_options: Vec<String>,

    /// エンジン応答のタイムアウト（秒）
    /// DLエンジンの初回TensorRTビルド等に対応するため長めに設定
    #[arg(long, default_value_t = 600)]
    engine_timeout: u64,

    /// 並列エンジンプロセス数（--engine使用時、デフォルト1）
    /// DL系: 2-4程度（GPU VRAM制限）、NNUE系: CPUコア数まで
    #[arg(long, default_value_t = 1)]
    engine_threads: usize,

    // --- AobaZero ONNX 直接推論モード ---
    /// AobaZero ONNXモデルパス（USIを介さず直接GPU推論）
    /// dlshogi_aoba のカスタム特徴量フォーマット専用。
    /// 標準 dlshogi モデルには使用不可。
    #[arg(long)]
    onnx_model: Option<PathBuf>,

    // --- 標準 dlshogi ONNX 直接推論モード ---
    /// 標準dlshogi ONNXモデルパス（DL水匠等、57ch features2）
    /// AobaZero モデルには使用不可。
    #[arg(long)]
    dlshogi_onnx_model: Option<PathBuf>,

    /// ONNX推論バッチサイズ（--onnx-model/--dlshogi-onnx-model使用時）
    #[arg(long, default_value_t = 256)]
    onnx_batch_size: usize,

    /// ONNX推論の GPU ID（-1=CPU）
    #[arg(long, default_value_t = 0)]
    onnx_gpu_id: i32,

    /// TensorRT ExecutionProvider を使用（FP16推論、初回はエンジンコンパイルに時間がかかる）
    #[arg(long)]
    onnx_tensorrt: bool,

    /// TensorRT エンジンキャッシュの保存先ディレクトリ
    #[arg(long)]
    onnx_tensorrt_cache: Option<PathBuf>,

    /// 引き分け手数（--onnx-model使用時の手数特徴量調整、0=調整なし）
    #[arg(long, default_value_t = 0)]
    onnx_draw_ply: i32,

    /// 勝率→cp変換のスケール（--onnx-model/--dlshogi-onnx-model使用時、bullet-shogiの--scaleと合わせる）
    #[arg(long, default_value_t = 600.0)]
    onnx_eval_scale: f32,

    /// ORT profiling出力先ディレクトリ（指定するとsession.run()の内訳をJSONで出力）
    #[arg(long)]
    ort_profile: Option<PathBuf>,

    // --- ポリシー展開モード（ONNX 推論で value と同時に policy も使う） ---
    /// 展開された子局面の出力ディレクトリ（指定時のみ expand 機能が有効）
    /// ONNX モード（--onnx-model または --dlshogi-onnx-model）必須。
    /// 入力ファイル名と同名で出力（rescore 出力とは別ディレクトリに置くこと）。
    #[arg(long)]
    expand_output_dir: Option<PathBuf>,

    /// 合法手 softmax 確率がこの値（%）を超えた手を子局面として書き出す
    /// `0.0 < v <= 100.0` の有限値が必要。expand 無効時は無視。
    #[arg(long, default_value_t = 10.0)]
    expand_threshold: f32,

    /// 親局面が王手のとき expand をスキップ（rescore 側は --skip-in-check で別制御）
    #[arg(long)]
    expand_skip_parent_in_check: bool,

    /// 展開した子局面が王手のとき expand 出力をスキップ
    #[arg(long)]
    expand_skip_child_in_check: bool,
}

/// 処理中にCtrl-Cが押されたかを追跡
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// qsearchの初期alpha値
const QSEARCH_ALPHA_INIT: i32 = -30000;
/// qsearchの初期beta値
const QSEARCH_BETA_INIT: i32 = 30000;

/// ONNX モードの marker 判定結果
#[cfg(any(feature = "aobazero-onnx", feature = "dlshogi-onnx"))]
enum OnnxMarkerDecision {
    /// 完了済み（marker 一致 + bodies_match）。ファイル全体を skip
    Skip,
    /// rescore + expand 出力を truncate して最初から処理
    TruncateAndProcess,
    /// marker 不存在 + expand 無効 → 既存の record-count resume にフォールバック
    LegacyResume,
}

/// 2 つのパスが同じファイル実体を指すかを判定する。
/// パス文字列一致、canonicalize 一致、Unix の `dev + ino` 一致のいずれかで true を返す。
/// I/O エラーや非 Unix プラットフォームで dev/ino が取れないケースでは false
/// に倒れる（= 「同一と判定できなかった」。呼び出し側の文脈で追加の安全策が
/// 必要な場面ではパス一致検証と併用する）。
#[cfg(any(feature = "aobazero-onnx", feature = "dlshogi-onnx"))]
fn is_same_file(a: &std::path::Path, b: &std::path::Path) -> bool {
    if a == b {
        return true;
    }
    if let (Ok(ca), Ok(cb)) = (a.canonicalize(), b.canonicalize())
        && ca == cb
    {
        return true;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if a.exists()
            && b.exists()
            && let (Ok(ma), Ok(mb)) = (fs::metadata(a), fs::metadata(b))
            && ma.dev() == mb.dev()
            && ma.ino() == mb.ino()
        {
            return true;
        }
    }
    false
}

/// 既存出力パスが symlink でないこと、未作成・既存どちらでも入力パスと衝突
/// しないことを検証する。既存ファイルが入力の hardlink である場合も検出する
/// （Unix のみ、`MetadataExt::dev/ino` で同一 inode を判定）。
#[cfg(any(feature = "aobazero-onnx", feature = "dlshogi-onnx"))]
fn ensure_safe_output_path(
    predicted: &std::path::Path,
    canonical_input: &std::path::Path,
) -> Result<()> {
    // 既存ファイルなら symlink を拒否（truncate/append が symlink を辿ると
    // 入力破壊につながる）
    if let Ok(meta) = fs::symlink_metadata(predicted)
        && meta.file_type().is_symlink()
    {
        anyhow::bail!(
            "Output path is a symlink (refusing to truncate a symlink): {}. \
             Remove the symlink or choose a different directory.",
            predicted.display()
        );
    }
    // 出力ファイル未作成でも parent を canonicalize して予定 canonical パスを構築
    // し、入力パスと一致したらエラー
    let predicted_canonical = canonicalize_predicted_path(predicted)?;
    if predicted_canonical == canonical_input {
        anyhow::bail!(
            "Output path resolves to input file: {} -> {}",
            predicted.display(),
            canonical_input.display()
        );
    }
    // 既存出力ファイルが入力と同じ inode を指す（hardlink）ケースを検出。
    // `canonicalize` はパス解決のみで hardlink を検出できないため、Unix では
    // `MetadataExt::{dev, ino}` で同一 inode 判定する。
    #[cfg(unix)]
    if predicted.exists() {
        use std::os::unix::fs::MetadataExt;
        let pred_meta = fs::metadata(predicted)
            .with_context(|| format!("Failed to stat predicted output {}", predicted.display()))?;
        let in_meta = fs::metadata(canonical_input)
            .with_context(|| format!("Failed to stat input {}", canonical_input.display()))?;
        if pred_meta.dev() == in_meta.dev() && pred_meta.ino() == in_meta.ino() {
            anyhow::bail!(
                "Output path is a hardlink to the input file ({} ↔ {}). \
                 Refusing to truncate; choose a different directory.",
                predicted.display(),
                canonical_input.display()
            );
        }
    }
    Ok(())
}

/// ONNX モードの marker チェック + 必要な truncate を行い、判定結果を返す
#[cfg(any(feature = "aobazero-onnx", feature = "dlshogi-onnx"))]
#[allow(clippy::too_many_arguments)]
fn onnx_marker_decide(
    cli: &Cli,
    model_kind: &str,
    onnx_path: &std::path::Path,
    input_path: &std::path::Path,
    rescore_output_path: &std::path::Path,
    expand_output_path: Option<&std::path::Path>,
    replacement_output: Option<&std::path::Path>,
    process_count: u64,
) -> Result<OnnxMarkerDecision> {
    // 現在 run の OnnxPipelineConfig を組み立てて fingerprint を作る
    // （f1/f2 サイズや input1/input2 channels は fingerprint に含まれないが、
    //  Config を共通化するために設定する。任意値で OK）
    let expand_cfg = expand_output_path.map(|p| ExpandConfig {
        output_path: p,
        threshold: cli.expand_threshold / 100.0,
        skip_parent_in_check: cli.expand_skip_parent_in_check,
        skip_child_in_check: cli.expand_skip_child_in_check,
    });
    // dlshogi モデルは `onnx_draw_ply` を使わないため fingerprint からも 0 固定にする
    // （AobaZero 専用パラメータの誤った invalidation を避ける）
    let onnx_draw_ply_for_fp = if model_kind == "aobazero" {
        cli.onnx_draw_ply
    } else {
        0
    };
    let cfg = OnnxPipelineConfig {
        model_name: "",
        model_kind,
        onnx_path,
        input_path,
        output_path: rescore_output_path,
        process_count,
        batch_size: cli.onnx_batch_size,
        gpu_id: cli.onnx_gpu_id,
        use_tensorrt: cli.onnx_tensorrt,
        tensorrt_cache: cli.onnx_tensorrt_cache.as_deref(),
        score_clip: cli.score_clip,
        eval_scale: cli.onnx_eval_scale,
        skip_in_check: cli.skip_in_check,
        onnx_draw_ply: onnx_draw_ply_for_fp,
        f1_size: 0,
        f2_size: 0,
        input1_channels: 0,
        input2_channels: 0,
        profile_path: None,
        expand: expand_cfg,
        qsearch_leaf_label: cli.qsearch_leaf_label,
        qsearch_max_ply: cli.max_ply,
        qsearch_nnue_path: cli.nnue.as_deref(),
        replacement_output,
    };
    let current = build_run_fingerprint(&cfg)?;

    // 入出力 symlink / 重複チェック
    let canonical_input = current.input_path.clone();
    ensure_safe_output_path(rescore_output_path, &canonical_input)?;
    if let Some(p) = expand_output_path {
        ensure_safe_output_path(p, &canonical_input)?;
    }
    if let Some(p) = replacement_output {
        ensure_safe_output_path(p, &canonical_input)?;
    }

    let marker_path = marker_path_for(rescore_output_path);
    if marker_path.exists() {
        let marker = parse_marker(&marker_path)?;
        let bodies_match = rescore_output_path.exists()
            && fs::metadata(rescore_output_path)?.len() == marker.output_sizes.rescore_output_size
            && match (
                marker.fingerprint.expand,
                marker.output_sizes.expand_output_size,
                marker.fingerprint.expand_output_path.as_ref(),
            ) {
                (true, Some(size), Some(path)) => {
                    path.exists() && fs::metadata(path)?.len() == size
                }
                (false, None, None) => true,
                _ => false,
            }
            && match (
                marker.fingerprint.replacement,
                marker.output_sizes.replacement_output_size,
                marker.fingerprint.replacement_output_path.as_ref(),
            ) {
                (true, Some(size), Some(path)) => {
                    path.exists() && fs::metadata(path)?.len() == size
                }
                (false, None, None) => true,
                _ => false,
            };

        if marker.fingerprint == current && bodies_match {
            return Ok(OnnxMarkerDecision::Skip);
        }

        // 不一致時の再生成手順。
        // 破壊操作の順序は意図的:
        //   1. 旧 expand artifact 削除（失敗しうる、入力衝突も事前検証）
        //   2. 旧 replacement artifact 削除（同上）
        //   3. 現 rescore 出力 truncate
        //   4. 現 expand 出力 truncate
        //   5. 現 replacement 出力 truncate
        //   6. marker 削除（最後）
        // この順なら、どの段階で失敗しても次回実行時に marker 不一致判定が
        // 働き、続きからやり直せる。"truncate してから remove_file" の順だと、
        // 後段失敗時に truncate 済み出力 + 古い marker が残ってデータ損失に
        // なり得る。

        // 1. 旧 expand artifact 削除
        if let Some(old) = &marker.fingerprint.expand_output_path {
            // 段階的パイプライン（前回 expand 出力を次回入力に使う）対策。
            // 旧 artifact が現在 run の入力と同一ファイルなら絶対に削除しない。
            if is_same_file(old, &canonical_input) {
                anyhow::bail!(
                    "Stale expand artifact {} resolves to the current input file {}. \
                     Refusing to delete to prevent input data loss. \
                     Move the input or change --expand-output-dir.",
                    old.display(),
                    canonical_input.display()
                );
            }
            // 現在 run の expand 出力と同一なら truncate ステップで処理されるのでスキップ
            let same_as_current = expand_output_path.is_some_and(|p| is_same_file(p, old));
            if !same_as_current && old.exists() {
                fs::remove_file(old).with_context(|| {
                    format!("Failed to remove stale expand artifact {}", old.display())
                })?;
            }
        }

        // 2. 旧 replacement artifact 削除
        if let Some(old) = &marker.fingerprint.replacement_output_path {
            if is_same_file(old, &canonical_input) {
                anyhow::bail!(
                    "Stale leaf-replacement artifact {} resolves to the current input file {}. \
                     Refusing to delete to prevent input data loss. \
                     Move the input or change --qsearch-leaf-replacement-output.",
                    old.display(),
                    canonical_input.display()
                );
            }
            let same_as_current = replacement_output.is_some_and(|p| is_same_file(p, old));
            if !same_as_current && old.exists() {
                fs::remove_file(old).with_context(|| {
                    format!("Failed to remove stale leaf-replacement artifact {}", old.display())
                })?;
            }
        }

        // 3. 現 rescore 出力 truncate
        if rescore_output_path.exists() {
            File::options().write(true).open(rescore_output_path)?.set_len(0)?;
        }
        // 4. 現 expand 出力 truncate
        if let Some(p) = expand_output_path
            && p.exists()
        {
            File::options().write(true).open(p)?.set_len(0)?;
        }
        // 5. 現 replacement 出力 truncate
        if let Some(p) = replacement_output
            && p.exists()
        {
            File::options().write(true).open(p)?.set_len(0)?;
        }
        // 6. marker 削除（最後）
        fs::remove_file(&marker_path)
            .with_context(|| format!("Failed to remove stale marker {}", marker_path.display()))?;
        return Ok(OnnxMarkerDecision::TruncateAndProcess);
    }

    // marker 不存在
    // leaf-label は marker 必須にする。record 数ベースの legacy resume では、旧通常 rescore の
    // marker 無し出力（レコード数だけ一致）を完了扱いし、葉ラベルを生成せず stale 出力を温存する。
    if expand_output_path.is_some() || replacement_output.is_some() || cli.qsearch_leaf_label {
        // expand / replacement / leaf-label 有効時は marker 必須。truncate して最初から
        if rescore_output_path.exists() {
            File::options().write(true).open(rescore_output_path)?.set_len(0)?;
        }
        if let Some(p) = expand_output_path
            && p.exists()
        {
            File::options().write(true).open(p)?.set_len(0)?;
        }
        if let Some(p) = replacement_output
            && p.exists()
        {
            File::options().write(true).open(p)?.set_len(0)?;
        }
        Ok(OnnxMarkerDecision::TruncateAndProcess)
    } else {
        Ok(OnnxMarkerDecision::LegacyResume)
    }
}

/// 入力パターンをglobで展開してファイルリストを取得
fn expand_input_patterns(patterns: &[String]) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();

    for pattern in patterns {
        // まず通常のファイルとして存在するか確認
        let path = PathBuf::from(pattern);
        if path.exists() && path.is_file() {
            files.push(path);
            continue;
        }

        // globパターンとして展開
        let matches: Vec<_> = glob(pattern)
            .with_context(|| format!("Invalid glob pattern: {pattern}"))?
            .filter_map(|entry| entry.ok())
            .filter(|p| p.is_file())
            .collect();

        if matches.is_empty() {
            // ファイルが見つからない場合はエラー
            anyhow::bail!("No files found matching pattern: {pattern}");
        }

        files.extend(matches);
    }

    // 重複を除去してソート
    files.sort();
    files.dedup();

    Ok(files)
}

// ============================================================
// 進捗表示（% / 残り時間 / 完了予定時刻）
// ============================================================

/// 件数を k/M 短縮表記にする（523456 -> "523.5k", 1000000 -> "1.00M"）。
fn compact_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.2}M", n as f64 / 1.0e6)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1.0e3)
    } else {
        n.to_string()
    }
}

/// 秒速を短縮表記にする（8338.0 -> "8.3k"）。
fn compact_rate(per_sec: f64) -> String {
    if per_sec >= 1_000_000.0 {
        format!("{:.2}M", per_sec / 1.0e6)
    } else if per_sec >= 1_000.0 {
        format!("{:.1}k", per_sec / 1.0e3)
    } else {
        format!("{per_sec:.0}")
    }
}

/// Duration を `H:MM:SS`（1h 以上）または `MM:SS` に整形する。
fn fmt_hms(d: Duration) -> String {
    let s = d.as_secs();
    let (h, m, sec) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{sec:02}")
    } else {
        format!("{m:02}:{sec:02}")
    }
}

/// `now + eta` をローカル時刻 `HH:MM` にする。eta が未確定（速度 0）のときは `--:--`。
fn finish_clock(eta: Duration, per_sec: f64) -> String {
    if per_sec <= 0.0 || eta.is_zero() {
        return "--:--".to_string();
    }
    match chrono::Duration::from_std(eta) {
        Ok(d) => (chrono::Local::now() + d).format("%H:%M").to_string(),
        Err(_) => "--:--".to_string(),
    }
}

/// テンプレートに共通のカスタムキー（短縮件数・完了予定時刻）を足す。
fn with_progress_keys(style: ProgressStyle) -> ProgressStyle {
    style
        .with_key("cpos", |s: &ProgressState, w: &mut dyn std::fmt::Write| {
            let _ = write!(w, "{}", compact_count(s.pos()));
        })
        .with_key("clen", |s: &ProgressState, w: &mut dyn std::fmt::Write| {
            let _ = write!(w, "{}", compact_count(s.len().unwrap_or(0)));
        })
        .with_key("finish_at", |s: &ProgressState, w: &mut dyn std::fmt::Write| {
            let _ = write!(w, "{}", finish_clock(s.eta(), s.per_sec()));
        })
}

#[cfg(test)]
mod progress_format_tests {
    use super::{Duration, compact_count, compact_rate, fmt_hms};

    #[test]
    fn compact_count_thresholds() {
        assert_eq!(compact_count(0), "0");
        assert_eq!(compact_count(999), "999");
        assert_eq!(compact_count(1_000), "1.0k");
        assert_eq!(compact_count(523_456), "523.5k");
        assert_eq!(compact_count(1_000_000), "1.00M");
        assert_eq!(compact_count(3_840_000), "3.84M");
    }

    #[test]
    fn compact_rate_thresholds() {
        assert_eq!(compact_rate(500.0), "500");
        assert_eq!(compact_rate(8338.0), "8.3k");
        assert_eq!(compact_rate(1_500_000.0), "1.50M");
    }

    #[test]
    fn fmt_hms_minutes_and_hours() {
        assert_eq!(fmt_hms(Duration::from_secs(0)), "00:00");
        assert_eq!(fmt_hms(Duration::from_secs(90)), "01:30");
        assert_eq!(fmt_hms(Duration::from_secs(600)), "10:00");
        assert_eq!(fmt_hms(Duration::from_secs(3661)), "1:01:01");
    }
}

/// 非TTY 時に定期ログ行を間引くための状態。
struct LogThrottle {
    last: Option<Instant>,
    last_pct: f64,
}

const LOG_INTERVAL: Duration = Duration::from_secs(15);
const LOG_PCT_STEP: f64 = 5.0;

/// rescore 全体の進捗管理。glob の全ファイルを分母とした overall バーと、
/// ファイル単位バーを束ねる。TTY ではバー描画、非TTY では定期ログ行に切替える。
struct RescoreProgress {
    multi: MultiProgress,
    /// overall バーは複数ファイル時のみ（単一ファイルは per-file バーがそのまま全体）。
    overall: Option<ProgressBar>,
    total_files: usize,
    is_tty: bool,
    log: Arc<Mutex<LogThrottle>>,
}

impl RescoreProgress {
    fn new(overall_total: u64, total_files: usize) -> Self {
        let is_tty = std::io::stderr().is_terminal();
        let multi = if is_tty {
            MultiProgress::new()
        } else {
            // 非TTY ではバー描画をやめ、進捗は定期ログ行で出す（CR スパム回避）。
            MultiProgress::with_draw_target(ProgressDrawTarget::hidden())
        };
        let overall = if total_files > 1 {
            let pb = multi.add(ProgressBar::new(overall_total));
            pb.set_style(
                with_progress_keys(ProgressStyle::default_bar().template(
                    "全体 {percent:>3}% {cpos}/{clen} ({prefix}) {per_sec} 残り {eta_precise} 完了 {finish_at}",
                ).expect("valid template")),
            );
            if is_tty {
                pb.enable_steady_tick(Duration::from_millis(500));
            }
            Some(pb)
        } else {
            None
        };
        Self {
            multi,
            overall,
            total_files,
            is_tty,
            log: Arc::new(Mutex::new(LogThrottle {
                last: None,
                last_pct: 0.0,
            })),
        }
    }

    /// 完了済み / skip されたファイル 1 個分を overall に反映する（per-file バーは作らない）。
    fn skip_file(&self, n: u64) {
        if let Some(o) = &self.overall {
            o.inc(n);
        }
    }

    /// 処理するファイル 1 個分の per-file 進捗ハンドルを作る。`shard_idx` は 1 始まり。
    fn start_file(&self, label: &str, shard_idx: usize, len: u64) -> FileProgress {
        let file = self.multi.add(ProgressBar::new(len));
        if self.total_files > 1 {
            file.set_style(
                with_progress_keys(
                    ProgressStyle::default_bar()
                        .template("└ {prefix} {percent:>3}% {bar:30.cyan/blue} {cpos}/{clen}")
                        .expect("valid template"),
                )
                .progress_chars("██░"),
            );
            file.set_prefix(label.to_string());
            if let Some(o) = &self.overall {
                o.set_prefix(format!("shard {shard_idx}/{}", self.total_files));
            }
        } else {
            file.set_style(
                with_progress_keys(
                    ProgressStyle::default_bar()
                        .template("[{elapsed_precise}] {bar:30.cyan/blue} {percent:>3}% {cpos}/{clen} {per_sec} 残り {eta_precise} 完了 {finish_at}")
                        .expect("valid template"),
                )
                .progress_chars("██░"),
            );
        }
        if self.is_tty {
            file.enable_steady_tick(Duration::from_millis(500));
        }
        FileProgress {
            file,
            overall: self.overall.clone(),
            is_tty: self.is_tty,
            log: Arc::clone(&self.log),
            label: label.to_string(),
            shard_idx,
            total_files: self.total_files,
        }
    }

    /// 全ファイル完了。非TTY では overall の最終行を必ず 1 行出す。
    fn finish(&self) {
        if let Some(o) = &self.overall {
            if !self.is_tty {
                eprintln!(
                    "[rescore] 全体 完了 {}/{} ({} files) {} pos/s 所要 {}",
                    compact_count(o.position()),
                    compact_count(o.length().unwrap_or(0)),
                    self.total_files,
                    compact_rate(o.per_sec()),
                    fmt_hms(o.elapsed()),
                );
            }
            o.finish_and_clear();
        }
    }
}

/// 1 ファイル分の進捗ハンドル。`inc` で per-file と overall を同時に進める。
/// 複数スレッドから呼ぶ search/engine 用に `Clone`（内部の ProgressBar / Arc は共有）。
#[derive(Clone)]
struct FileProgress {
    file: ProgressBar,
    overall: Option<ProgressBar>,
    is_tty: bool,
    log: Arc<Mutex<LogThrottle>>,
    label: String,
    shard_idx: usize,
    total_files: usize,
}

impl FileProgress {
    /// resume で既処理だった分を起点として進める（per-file/overall とも前進）。
    /// 追記再開する ONNX 直推論モード専用。NNUE/USI モードは出力を File::create で
    /// truncate して全件を再処理するため起点補正は不要（全件 inc で 100% に到達する）。
    #[cfg(any(feature = "aobazero-onnx", feature = "dlshogi-onnx"))]
    fn advance_start(&self, n: u64) {
        if n == 0 {
            return;
        }
        self.file.inc(n);
        if let Some(o) = &self.overall {
            o.inc(n);
        }
    }

    fn inc(&self, n: u64) {
        self.file.inc(n);
        if let Some(o) = &self.overall {
            o.inc(n);
        }
        if !self.is_tty {
            self.maybe_log(false);
        }
    }

    fn set_message(&self, msg: &'static str) {
        self.file.set_message(msg);
    }

    fn finish_with_message(&self, msg: &'static str) {
        if !self.is_tty {
            self.maybe_log(true);
        }
        if self.total_files > 1 {
            // 複数ファイル時は overall を残して per-file バーだけ消す。
            self.file.finish_and_clear();
        } else {
            self.file.finish_with_message(msg);
        }
    }

    fn abandon_with_message(&self, msg: &'static str) {
        self.file.abandon_with_message(msg);
    }

    /// 非TTY 用の 1 行ログ。`force` 時は throttle を無視して必ず出す。
    fn maybe_log(&self, force: bool) {
        // overall（複数ファイル）か per-file（単一）を「全体進捗」として使う。
        let primary = self.overall.as_ref().unwrap_or(&self.file);
        let pos = primary.position();
        let len = primary.length().unwrap_or(0);
        let pct = if len > 0 {
            pos as f64 / len as f64 * 100.0
        } else {
            0.0
        };

        {
            let mut th = self.log.lock().expect("log throttle poisoned");
            if !force {
                let due_time = th.last.map(|t| t.elapsed() >= LOG_INTERVAL).unwrap_or(true);
                let due_pct = pct - th.last_pct >= LOG_PCT_STEP;
                if !due_time && !due_pct {
                    return;
                }
            }
            th.last = Some(Instant::now());
            th.last_pct = pct;
        }

        let rate = compact_rate(primary.per_sec());
        let elapsed = fmt_hms(primary.elapsed());
        let eta = fmt_hms(primary.eta());
        let clock = finish_clock(primary.eta(), primary.per_sec());
        if self.overall.is_some() {
            let fpos = self.file.position();
            let flen = self.file.length().unwrap_or(0);
            let fpct = if flen > 0 {
                fpos as f64 / flen as f64 * 100.0
            } else {
                0.0
            };
            eprintln!(
                "[rescore] 全体 {pct:.1}% {}/{} shard {}/{} ({} {fpct:.1}%) {rate} pos/s elapsed {elapsed} ETA {eta} (完了 {clock})",
                compact_count(pos),
                compact_count(len),
                self.shard_idx,
                self.total_files,
                self.label,
            );
        } else {
            eprintln!(
                "[rescore] {} {pct:.1}% {}/{} {rate} pos/s elapsed {elapsed} ETA {eta} (完了 {clock})",
                self.label,
                compact_count(pos),
                compact_count(len),
            );
        }
    }
}

fn main() -> Result<()> {
    env_logger::init();
    let cli = Cli::parse();

    let use_engine = cli.engine.is_some();
    let use_onnx = cli.onnx_model.is_some();
    let use_dlshogi_onnx = cli.dlshogi_onnx_model.is_some();

    // 排他チェック
    if use_onnx && (use_dlshogi_onnx || use_engine || cli.use_qsearch || cli.search_depth.is_some())
    {
        anyhow::bail!(
            "--onnx-model is mutually exclusive with --dlshogi-onnx-model, --engine, --use-qsearch, --search-depth"
        );
    }
    if use_dlshogi_onnx && (use_engine || cli.use_qsearch || cli.search_depth.is_some()) {
        anyhow::bail!(
            "--dlshogi-onnx-model is mutually exclusive with --engine, --use-qsearch, --search-depth"
        );
    }
    if cli.onnx_tensorrt && cli.onnx_gpu_id < 0 {
        anyhow::bail!("--onnx-tensorrt requires a GPU (--onnx-gpu-id >= 0)");
    }
    if use_engine && (cli.use_qsearch || cli.search_depth.is_some()) {
        anyhow::bail!("--engine is mutually exclusive with --use-qsearch and --search-depth");
    }
    if cli.use_qsearch && cli.search_depth.is_some() {
        anyhow::bail!("--use-qsearch and --search-depth are mutually exclusive");
    }
    if !use_engine && !use_onnx && !use_dlshogi_onnx && cli.nnue.is_none() {
        anyhow::bail!(
            "--nnue is required when --engine/--onnx-model/--dlshogi-onnx-model is not specified"
        );
    }

    // --qsearch-leaf-label の前提チェック
    // root 局面を保持しつつラベルだけ葉評価にするには、葉を求める NNUE と
    // 葉を評価する ONNX ラベラーの両方が要る。AobaZero 特徴量は手数 (game_ply) を
    // 含み、葉へ進めても root の game_ply が渡って葉特徴量に混入するため dlshogi 限定。
    if cli.qsearch_leaf_label {
        if cli.nnue.is_none() {
            anyhow::bail!("--qsearch-leaf-label requires --nnue (qsearch で葉局面を求めるため)");
        }
        if !use_dlshogi_onnx {
            anyhow::bail!(
                "--qsearch-leaf-label requires --dlshogi-onnx-model \
                 (AobaZero 特徴量は game_ply を含み葉局面と不整合のため非対応)"
            );
        }
        if cli.apply_qsearch_leaf {
            anyhow::bail!(
                "--qsearch-leaf-label and --apply-qsearch-leaf are mutually exclusive \
                 (前者は局面据え置き・ラベルのみ、後者は局面置換)"
            );
        }
        if cli.expand_output_dir.is_some() {
            anyhow::bail!(
                "--qsearch-leaf-label is mutually exclusive with --expand-output-dir \
                 (policy 出力は葉局面に対応し root 局面と不整合になるため)"
            );
        }
    }

    // --qsearch-leaf-replacement-output は --qsearch-leaf-label 前提。
    // leaf-label の qsearch 葉を再利用して、葉局面置換 arm を同時生成する。
    if cli.qsearch_leaf_replacement_output.is_some() && !cli.qsearch_leaf_label {
        anyhow::bail!(
            "--qsearch-leaf-replacement-output requires --qsearch-leaf-label \
             (葉局面は leaf-label の qsearch 結果を再利用するため)"
        );
    }

    // --expand-output-dir は ONNX モード必須
    if cli.expand_output_dir.is_some() && !(use_onnx || use_dlshogi_onnx) {
        anyhow::bail!(
            "--expand-output-dir requires ONNX mode (--onnx-model or --dlshogi-onnx-model). \
             policy 出力は ONNX 推論経路でのみ取得できます。"
        );
    }
    // expand 機能の数値検証
    if cli.expand_output_dir.is_some() {
        let t = cli.expand_threshold;
        if !(t.is_finite() && 0.0 < t && t <= 100.0) {
            anyhow::bail!("--expand-threshold must be a finite value in (0.0, 100.0], got {t}");
        }
    }
    // ONNX 系の eval_scale 妥当性
    if use_onnx || use_dlshogi_onnx {
        let s = cli.onnx_eval_scale;
        if !s.is_finite() || s <= 0.0 {
            anyhow::bail!("--onnx-eval-scale must be a positive finite value, got {s}");
        }
    }
    #[cfg(not(feature = "aobazero-onnx"))]
    if use_onnx {
        anyhow::bail!(
            "--onnx-model requires the 'aobazero-onnx' feature.\n\
             Rebuild with: cargo build --release -p tools --features aobazero-onnx --bin rescore_psv"
        );
    }
    #[cfg(not(feature = "dlshogi-onnx"))]
    if use_dlshogi_onnx {
        anyhow::bail!(
            "--dlshogi-onnx-model requires the 'dlshogi-onnx' feature.\n\
             Rebuild with: cargo build --release -p tools --features dlshogi-onnx --bin rescore_psv"
        );
    }

    // --search-depth 指定時に --apply-qsearch-leaf が有効なら警告
    if cli.search_depth.is_some() && cli.apply_qsearch_leaf {
        eprintln!("Warning: --apply-qsearch-leaf is ignored when --search-depth is specified");
    }

    // 入力ファイルをglobパターンで展開
    let input_files = expand_input_patterns(&cli.input)?;
    if input_files.is_empty() {
        anyhow::bail!("No input files found matching the patterns");
    }

    eprintln!("Found {} input file(s)", input_files.len());

    // 出力ディレクトリの作成
    if !cli.output_dir.exists() {
        fs::create_dir_all(&cli.output_dir).with_context(|| {
            format!("Failed to create output directory: {}", cli.output_dir.display())
        })?;
    }
    // expand 出力ディレクトリの作成
    if let Some(d) = &cli.expand_output_dir
        && !d.exists()
    {
        fs::create_dir_all(d).with_context(|| {
            format!("Failed to create expand output directory: {}", d.display())
        })?;
    }
    // leaf-replacement 出力ディレクトリの作成
    if let Some(d) = &cli.qsearch_leaf_replacement_output
        && !d.exists()
    {
        fs::create_dir_all(d).with_context(|| {
            format!("Failed to create leaf-replacement output directory: {}", d.display())
        })?;
    }
    // 各 dir を canonicalize して衝突を検出
    let canonical_rescore_dir = cli.output_dir.canonicalize().with_context(|| {
        format!("Failed to canonicalize --output-dir {}", cli.output_dir.display())
    })?;
    let canonical_expand_dir = match &cli.expand_output_dir {
        Some(d) => Some(d.canonicalize().with_context(|| {
            format!("Failed to canonicalize --expand-output-dir {}", d.display())
        })?),
        None => None,
    };
    if let Some(ed) = &canonical_expand_dir
        && &canonical_rescore_dir == ed
    {
        anyhow::bail!("--output-dir and --expand-output-dir must point to different directories");
    }
    let canonical_replacement_dir = match &cli.qsearch_leaf_replacement_output {
        Some(d) => Some(d.canonicalize().with_context(|| {
            format!("Failed to canonicalize --qsearch-leaf-replacement-output {}", d.display())
        })?),
        None => None,
    };
    if let Some(rd) = &canonical_replacement_dir
        && &canonical_rescore_dir == rd
    {
        anyhow::bail!(
            "--output-dir and --qsearch-leaf-replacement-output must point to different directories"
        );
    }

    // NNUEモデルのロード（NNUE内部評価モード、または ONNX ラベラー併用の
    // --qsearch-leaf-label で葉探索に NNUE を使う場合）
    if (!use_engine && !use_onnx && !use_dlshogi_onnx) || cli.qsearch_leaf_label {
        let nnue = cli.nnue.as_ref().unwrap();
        if !nnue.exists() {
            anyhow::bail!("NNUE model file not found: {}", nnue.display());
        }
        init_nnue(nnue).context("Failed to load NNUE model")?;
        eprintln!("NNUE model loaded: {}", nnue.display());
    }

    // Ctrl-Cハンドラを設定
    ctrlc::set_handler(|| {
        eprintln!("\nInterrupted!");
        INTERRUPTED.store(true, Ordering::SeqCst);
    })
    .context("Failed to set Ctrl-C handler")?;

    // rayon スレッドプール設定（NNUE並列モードのみ）
    if !use_engine && !use_onnx && cli.search_depth.is_none() && cli.threads > 0 {
        rayon::ThreadPoolBuilder::new()
            .num_threads(cli.threads)
            .build_global()
            .unwrap_or_else(|e| {
                eprintln!("Warning: Failed to set thread count: {e}");
            });
    }

    // 外部USIエンジンの起動
    let engine_threads = if use_engine {
        cli.engine_threads.max(1)
    } else {
        0
    };
    let mut engines: Vec<UsiEngine> = Vec::new();
    if use_engine {
        let engine_path = cli.engine.as_ref().unwrap();
        let timeout = std::time::Duration::from_secs(cli.engine_timeout);
        for i in 0..engine_threads {
            eprintln!("--- Engine instance {}/{} ---", i + 1, engine_threads);
            engines.push(UsiEngine::new(engine_path, &cli.usi_options, timeout)?);
        }
    }

    // 処理設定の表示
    eprintln!(
        "Mode: {}",
        if use_onnx || use_dlshogi_onnx {
            let ep = if cli.onnx_tensorrt {
                "TensorRT+FP16"
            } else {
                "CUDA"
            };
            let model = if use_onnx { "AobaZero" } else { "dlshogi" };
            format!(
                "{model} ONNX direct inference (batch={}, gpu={}, ep={ep})",
                cli.onnx_batch_size, cli.onnx_gpu_id
            )
        } else if use_engine {
            format!("external USI engine (nodes={}, threads={})", cli.engine_nodes, engine_threads)
        } else if let Some(depth) = cli.search_depth {
            format!("depth {depth} search")
        } else if cli.use_qsearch {
            "qsearch evaluation".to_string()
        } else {
            "static NNUE evaluation".to_string()
        }
    );
    if cli.search_depth.is_some() {
        eprintln!("Hash size: {} MB", cli.hash_mb);
        if cli.max_nodes > 0 {
            eprintln!("Max nodes: {} (per position)", cli.max_nodes);
        } else {
            eprintln!("Max nodes: unlimited");
        }
        if cli.max_time > 0 {
            eprintln!("Max time: {} ms (per position)", cli.max_time);
        } else {
            eprintln!("Max time: unlimited");
        }
    }
    if !use_engine {
        eprintln!(
            "qsearch leaf replacement: {}",
            if cli.apply_qsearch_leaf && cli.search_depth.is_none() {
                "enabled"
            } else {
                "disabled"
            }
        );
    }
    eprintln!("Score clip: ±{}", cli.score_clip);
    eprintln!("Skip in-check positions: {}", if cli.skip_in_check { "yes" } else { "no" });
    eprintln!(
        "FV_SCALE conversion: {} -> {} (factor: {:.3})",
        cli.source_fv_scale,
        cli.target_fv_scale,
        cli.source_fv_scale as f64 / cli.target_fv_scale as f64
    );
    eprintln!("Output directory: {}", cli.output_dir.display());
    if let Some(d) = &cli.expand_output_dir {
        eprintln!(
            "Expand output directory: {} (threshold={:.2}%, skip_parent={}, skip_child={})",
            d.display(),
            cli.expand_threshold,
            cli.expand_skip_parent_in_check,
            cli.expand_skip_child_in_check,
        );
    }
    if let Some(d) = &cli.qsearch_leaf_replacement_output {
        eprintln!("Leaf-replacement output directory: {}", d.display());
    }
    if cli.delete_input {
        eprintln!("Delete input after processing: yes");
    }
    eprintln!();

    // 各ファイルを処理
    let total_files = input_files.len();
    // 全ファイル件数（--limit を各ファイルに適用）を合算し overall 進捗の分母にする。
    // metadata の stat だけで全件 load しない。skip / resume は処理中に overall へ反映する。
    let overall_total: u64 = input_files
        .iter()
        .map(|p| {
            let rc = fs::metadata(p).map(|m| m.len()).unwrap_or(0) / PackedSfenValue::SIZE as u64;
            if cli.limit > 0 && cli.limit < rc {
                cli.limit
            } else {
                rc
            }
        })
        .sum();
    let rprog = RescoreProgress::new(overall_total, total_files);
    for (file_idx, input_path) in input_files.iter().enumerate() {
        if INTERRUPTED.load(Ordering::SeqCst) {
            eprintln!("Processing interrupted");
            break;
        }

        // 出力ファイルパスを生成
        let file_name = input_path
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("Invalid input file name: {}", input_path.display()))?;
        let output_path = cli.output_dir.join(file_name);
        #[cfg(any(feature = "aobazero-onnx", feature = "dlshogi-onnx"))]
        let expand_output_path = canonical_expand_dir.as_ref().map(|d| d.join(file_name));
        #[cfg(any(feature = "aobazero-onnx", feature = "dlshogi-onnx"))]
        let replacement_output_path = canonical_replacement_dir.as_ref().map(|d| d.join(file_name));

        // 入力ファイルサイズと process_count を最初に確定（marker 判定の前提）
        let input_file_size = fs::metadata(input_path)?.len();
        let input_record_count = input_file_size / PackedSfenValue::SIZE as u64;
        let process_count = if cli.limit > 0 && cli.limit < input_record_count {
            cli.limit
        } else {
            input_record_count
        };

        // ONNX モードでは marker ベースで skip / truncate 判定
        #[cfg(any(feature = "aobazero-onnx", feature = "dlshogi-onnx"))]
        let mut use_legacy_resume_check = !(use_onnx || use_dlshogi_onnx);
        #[cfg(not(any(feature = "aobazero-onnx", feature = "dlshogi-onnx")))]
        let use_legacy_resume_check = !(use_onnx || use_dlshogi_onnx);
        #[cfg(any(feature = "aobazero-onnx", feature = "dlshogi-onnx"))]
        if use_onnx || use_dlshogi_onnx {
            let (model_kind, onnx_path): (&str, &std::path::Path) = if use_onnx {
                ("aobazero", cli.onnx_model.as_ref().unwrap().as_path())
            } else {
                ("dlshogi", cli.dlshogi_onnx_model.as_ref().unwrap().as_path())
            };
            let decision = onnx_marker_decide(
                &cli,
                model_kind,
                onnx_path,
                input_path,
                &output_path,
                expand_output_path.as_deref(),
                replacement_output_path.as_deref(),
                process_count,
            )?;
            match decision {
                OnnxMarkerDecision::Skip => {
                    eprintln!(
                        "=== [{}/{}] Skipping (marker matches): {} ===",
                        file_idx + 1,
                        total_files,
                        output_path.display()
                    );
                    rprog.skip_file(process_count);
                    continue;
                }
                OnnxMarkerDecision::TruncateAndProcess => {
                    eprintln!(
                        "=== [{}/{}] Truncated stale outputs, regenerating: {} ===",
                        file_idx + 1,
                        total_files,
                        input_path.display()
                    );
                }
                OnnxMarkerDecision::LegacyResume => {
                    use_legacy_resume_check = true;
                }
            }
        }

        // legacy resume / skip 判定（NNUE/USI 全モード、または ONNX 無 marker + expand 無効）
        if use_legacy_resume_check {
            // 入力と出力が同じパスの場合はエラー（--delete-input でデータ消失を防ぐ）
            if input_path.canonicalize().ok() == output_path.canonicalize().ok() {
                anyhow::bail!(
                    "Input and output paths are the same: {}. Use a different --output-dir.",
                    input_path.display()
                );
            }
            if output_path.exists() {
                let out_size = fs::metadata(&output_path)?.len();
                let out_records = out_size / PackedSfenValue::SIZE as u64;
                if out_records >= input_record_count && out_size % PackedSfenValue::SIZE as u64 == 0
                {
                    eprintln!(
                        "=== [{}/{}] Skipping (complete: {} records): {} ===",
                        file_idx + 1,
                        total_files,
                        out_records,
                        output_path.display()
                    );
                    rprog.skip_file(process_count);
                    continue;
                }
                if out_records > 0 {
                    eprintln!(
                        "=== [{}/{}] Resuming ({}/{} records): {} ===",
                        file_idx + 1,
                        total_files,
                        out_records,
                        input_record_count,
                        input_path.display()
                    );
                }
            }
        }

        eprintln!(
            "=== [{}/{}] Processing: {} ===",
            file_idx + 1,
            total_files,
            input_path.display()
        );

        let file_size = input_file_size;
        let record_count = input_record_count;

        if file_size % PackedSfenValue::SIZE as u64 != 0 {
            eprintln!(
                "Warning: File size ({file_size}) is not a multiple of record size ({})",
                PackedSfenValue::SIZE
            );
        }

        eprintln!("Records: {record_count}, Processing: {process_count}");

        // 必要メモリの概算と警告（入力バッファ + 出力バッファ）
        let required_memory_mb =
            (process_count as usize * PackedSfenValue::SIZE * 2) / (1024 * 1024);
        if required_memory_mb > 1024 {
            eprintln!(
                "Warning: Estimated memory usage: {} GB. Ensure sufficient RAM is available.",
                required_memory_mb / 1024
            );
        }

        // 処理実行
        let fprog = rprog.start_file(&file_name.to_string_lossy(), file_idx + 1, process_count);
        #[cfg(feature = "aobazero-onnx")]
        if use_onnx {
            process_file_with_onnx(
                &cli,
                input_path,
                &output_path,
                expand_output_path.as_deref(),
                process_count,
                &fprog,
            )?;
        }
        #[cfg(feature = "dlshogi-onnx")]
        if use_dlshogi_onnx {
            process_file_with_dlshogi_onnx(
                &cli,
                input_path,
                &output_path,
                expand_output_path.as_deref(),
                replacement_output_path.as_deref(),
                process_count,
                &fprog,
            )?;
        }
        if !use_onnx && !use_dlshogi_onnx {
            if !engines.is_empty() {
                process_file_with_engine(
                    &cli,
                    &mut engines,
                    input_path,
                    &output_path,
                    process_count,
                    &fprog,
                )?;
            } else if cli.search_depth.is_some() {
                process_file_with_search(&cli, input_path, &output_path, process_count, &fprog)?;
            } else {
                process_file(&cli, input_path, &output_path, process_count, &fprog)?;
            }
        }

        if !INTERRUPTED.load(Ordering::SeqCst) {
            eprintln!("Output: {}", output_path.display());

            // 処理完了後に入力ファイルを削除
            if cli.delete_input {
                let output_size = fs::metadata(&output_path).map(|m| m.len()).unwrap_or(0);
                if output_size > 0 {
                    fs::remove_file(input_path).with_context(|| {
                        format!("Failed to delete input file: {}", input_path.display())
                    })?;
                    eprintln!("Deleted input: {}", input_path.display());
                } else {
                    eprintln!(
                        "Warning: Output file is empty or missing, keeping input file: {}",
                        input_path.display()
                    );
                }
            }
        }
        eprintln!();
    }
    rprog.finish();

    // エンジン終了
    for mut eng in engines {
        let _ = eng.quit();
    }

    if INTERRUPTED.load(Ordering::SeqCst) {
        eprintln!("Note: Processing was interrupted, some outputs may be incomplete");
    } else {
        eprintln!("All {} file(s) processed successfully", total_files);
    }

    Ok(())
}

/// ファイルを処理
fn process_file(
    cli: &Cli,
    input_path: &PathBuf,
    output_path: &PathBuf,
    process_count: u64,
    progress: &FileProgress,
) -> Result<()> {
    // チャンクストリーミング: 読み込み → rayon 並列処理 → 書き出しをチャンク単位で繰り返す
    // 全レコードをメモリに溜めず、ピークメモリ = チャンクサイズ分のみ
    const CHUNK_SIZE: usize = 1_000_000;

    let in_file = File::open(input_path)
        .with_context(|| format!("Failed to open {}", input_path.display()))?;
    let mut reader = BufReader::with_capacity(8 * 1024 * 1024, in_file);

    let out_file = File::create(output_path)
        .with_context(|| format!("Failed to create {}", output_path.display()))?;
    let mut writer = BufWriter::with_capacity(8 * 1024 * 1024, out_file);

    let error_count = AtomicU64::new(0);
    let processed_count = AtomicU64::new(0);
    let clipped_count = AtomicU64::new(0);
    let skipped_count = AtomicU64::new(0);

    let max_ply = cli.max_ply;
    let use_qsearch = cli.use_qsearch;
    let apply_leaf = cli.apply_qsearch_leaf;
    let score_clip = cli.score_clip;
    let skip_in_check = cli.skip_in_check;
    let source_fv_scale = cli.source_fv_scale;
    let target_fv_scale = cli.target_fv_scale;
    let verbose = cli.verbose;

    progress.set_message("Processing...");
    let mut total_read = 0u64;
    let mut total_written = 0u64;

    loop {
        if INTERRUPTED.load(Ordering::SeqCst) {
            progress.abandon_with_message("Interrupted");
            break;
        }

        // チャンク読み込み
        let want = (CHUNK_SIZE as u64).min(process_count.saturating_sub(total_read)) as usize;
        let mut chunk: Vec<[u8; PackedSfenValue::SIZE]> = Vec::with_capacity(want);
        let mut buffer = [0u8; PackedSfenValue::SIZE];

        for _ in 0..want {
            match reader.read_exact(&mut buffer) {
                Ok(()) => chunk.push(buffer),
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e.into()),
            }
        }

        if chunk.is_empty() {
            break;
        }
        total_read += chunk.len() as u64;

        // rayon 並列処理 → 即座に書き出し
        let results: Vec<Option<[u8; PackedSfenValue::SIZE]>> = chunk
            .par_iter()
            .map(|record| {
                if INTERRUPTED.load(Ordering::SeqCst) {
                    return None;
                }

                thread_local! {
                    static NNUE_STACKS: RefCell<NnueStacks> = RefCell::new(NnueStacks::new());
                }

                let result = NNUE_STACKS.with(|stacks| {
                    let mut stacks = stacks.borrow_mut();
                    stacks.reset();
                    process_record(
                        record,
                        &mut stacks,
                        max_ply,
                        use_qsearch,
                        apply_leaf,
                        score_clip,
                        skip_in_check,
                        source_fv_scale,
                        target_fv_scale,
                    )
                });

                match result {
                    ProcessResult::Ok(new_record, clipped) => {
                        processed_count.fetch_add(1, Ordering::Relaxed);
                        if clipped {
                            clipped_count.fetch_add(1, Ordering::Relaxed);
                        }
                        Some(new_record)
                    }
                    ProcessResult::Skip => {
                        skipped_count.fetch_add(1, Ordering::Relaxed);
                        None
                    }
                    ProcessResult::Error(e) => {
                        error_count.fetch_add(1, Ordering::Relaxed);
                        if verbose {
                            eprintln!("Error processing record: {e}");
                        }
                        None
                    }
                }
            })
            .collect();

        // チャンク結果を即座に書き出し
        for record in results.iter().flatten() {
            writer.write_all(record)?;
            total_written += 1;
        }
        progress.inc(results.len() as u64);
    }

    writer.flush()?;
    progress.finish_with_message("Done");

    let final_errors = error_count.load(Ordering::SeqCst);
    let final_clipped = clipped_count.load(Ordering::SeqCst);
    let final_skipped = skipped_count.load(Ordering::SeqCst);
    if final_errors > 0 {
        eprintln!("Note: {final_errors} positions had errors");
    }
    if final_skipped > 0 && total_read > 0 {
        eprintln!(
            "Skipped (in check): {} ({:.2}%)",
            final_skipped,
            final_skipped as f64 / total_read as f64 * 100.0
        );
    }
    if total_read > 0 {
        eprintln!(
            "Clipped scores: {} ({:.2}%)",
            final_clipped,
            final_clipped as f64 / total_read as f64 * 100.0
        );
    }

    eprintln!("Wrote {total_written} records");

    Ok(())
}

/// 処理結果
enum ProcessResult {
    /// 正常に処理完了（新レコード, クリップされたか）
    Ok([u8; PackedSfenValue::SIZE], bool),
    /// スキップ（王手局面など）
    Skip,
    /// エラー
    Error(anyhow::Error),
}

/// 1レコードを処理
fn process_record(
    record: &[u8; PackedSfenValue::SIZE],
    stacks: &mut NnueStacks,
    max_ply: i32,
    use_qsearch: bool,
    apply_leaf: bool,
    score_clip: i16,
    skip_in_check: bool,
    source_fv_scale: i32,
    target_fv_scale: i32,
) -> ProcessResult {
    // PackedSfenValueを読み込み
    let psv = match PackedSfenValue::from_bytes(record) {
        Some(p) => p,
        None => return ProcessResult::Error(anyhow::anyhow!("Failed to parse PackedSfenValue")),
    };

    // PackedSfen → SFEN → Position
    let sfen = match unpack_sfen(&psv.sfen) {
        Ok(s) => s,
        Err(e) => return ProcessResult::Error(anyhow::anyhow!("Failed to unpack SFEN: {e}")),
    };

    let mut pos = Position::new();
    if let Err(e) = pos.set_sfen(&sfen) {
        return ProcessResult::Error(anyhow::anyhow!("Failed to set SFEN: {e:?}"));
    }

    // 王手局面をスキップ
    if skip_in_check && pos.in_check() {
        return ProcessResult::Skip;
    }

    // qsearch leaf置換を適用する場合
    let (final_sfen, stm_changed) = if apply_leaf && !pos.in_check() {
        let result = qsearch_with_pv_nnue(
            &mut pos,
            stacks,
            QSEARCH_ALPHA_INIT,
            QSEARCH_BETA_INIT,
            0,
            max_ply,
        );

        // PV に沿って葉局面まで進める。STM 反転有無も同時に得る。
        let stm_changed = apply_pv(&mut pos, &result.pv);
        let new_sfen = pack_position(&pos);
        (new_sfen, stm_changed)
    } else {
        (psv.sfen, false)
    };

    // NNUEで評価
    stacks.reset();
    let raw_score = if use_qsearch && !pos.in_check() {
        // qsearch評価
        let result = qsearch_with_pv_nnue(
            &mut pos,
            stacks,
            QSEARCH_ALPHA_INIT,
            QSEARCH_BETA_INIT,
            0,
            max_ply,
        );
        result.value
    } else {
        // 静的評価
        stacks.evaluate(&pos)
    };

    // STM視点で統一（エンジン評価は常にSTM視点）
    // game_resultは元のまま使用（または手番変更時に反転）
    let new_game_result = if stm_changed {
        -psv.game_result
    } else {
        psv.game_result
    };

    // FV_SCALE補正: source_fv_scale -> target_fv_scale
    // nn.bin (FV_SCALE=24) の評価値を nnue-pytorch (FV_SCALE=16) 用に変換
    // 補正式: scaled_score = raw_score * source_fv_scale / target_fv_scale
    // 例: source=24, target=16 -> factor=1.5
    let scaled_score = raw_score * source_fv_scale / target_fv_scale;

    // スコアをクリップ
    let clipped = scaled_score.abs() > score_clip as i32;
    let new_score = scaled_score.clamp(-score_clip as i32, score_clip as i32) as i16;

    // 新しいPackedSfenValueを作成
    let new_psv = PackedSfenValue {
        sfen: final_sfen,
        score: new_score,
        move16: 0, // 無効値
        game_ply: psv.game_ply,
        game_result: new_game_result,
        padding: 0,
    };

    ProcessResult::Ok(new_psv.to_bytes(), clipped)
}

/// 深さ指定探索でファイルを処理
///
/// 探索は重いため、rayon並列処理ではなく、複数のワーカースレッドが
/// それぞれ独自のSearchインスタンスを持ってチャンク単位で処理する。
fn process_file_with_search(
    cli: &Cli,
    input_path: &PathBuf,
    output_path: &PathBuf,
    process_count: u64,
    progress: &FileProgress,
) -> Result<()> {
    let search_depth = cli.search_depth.expect("search_depth should be Some");

    // 入力ファイルを読み込み
    let in_file = File::open(input_path)
        .with_context(|| format!("Failed to open {}", input_path.display()))?;
    let mut reader = BufReader::new(in_file);

    // 全レコードを読み込み
    let mut records: Vec<[u8; PackedSfenValue::SIZE]> = Vec::with_capacity(process_count as usize);
    let mut buffer = [0u8; PackedSfenValue::SIZE];

    progress.set_message("Reading...");
    for _ in 0..process_count {
        if INTERRUPTED.load(Ordering::SeqCst) {
            progress.abandon_with_message("Interrupted");
            return Ok(());
        }

        match reader.read_exact(&mut buffer) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        }

        records.push(buffer);
    }

    let actual_count = records.len();
    eprintln!("Read {actual_count} records");

    // 空ファイルガード（chunks(0)でpanicを防ぐ）
    if actual_count == 0 {
        eprintln!("Warning: No records to process, creating empty output file");
        File::create(output_path)?;
        return Ok(());
    }

    // スレッド数を決定（0なら利用可能なCPU数）
    let num_threads = if cli.threads > 0 {
        cli.threads
    } else {
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
    };
    eprintln!("Using {num_threads} worker threads for search");

    // メモリ使用量の警告（各スレッドが独自の置換表を持つ）
    let total_hash_mb = cli.hash_mb * num_threads;
    eprintln!(
        "Total hash table size: {} MB ({} MB × {} threads)",
        total_hash_mb, cli.hash_mb, num_threads
    );
    if total_hash_mb > 4096 {
        eprintln!(
            "Warning: Large memory allocation ({} GB). Consider reducing --hash-mb or --threads.",
            total_hash_mb / 1024
        );
    }

    // レコードをチャンクに分割（chunk_sizeは最低1を保証）
    let chunk_size = records.len().div_ceil(num_threads).max(1);
    let chunks: Vec<Vec<[u8; PackedSfenValue::SIZE]>> =
        records.chunks(chunk_size).map(|chunk| chunk.to_vec()).collect();

    // 設定値をキャプチャ
    let hash_mb = cli.hash_mb;
    let max_nodes = cli.max_nodes;
    let max_time = cli.max_time;
    let score_clip = cli.score_clip;
    let skip_in_check = cli.skip_in_check;
    let source_fv_scale = cli.source_fv_scale;
    let target_fv_scale = cli.target_fv_scale;
    let verbose = cli.verbose;

    // カウンタ
    let error_count = AtomicU64::new(0);
    let clipped_count = AtomicU64::new(0);
    let skipped_count = AtomicU64::new(0);

    // 結果収集用チャネル
    let (tx, rx) = mpsc::channel::<SearchProcessResult>();

    progress.set_message("Processing...");

    // ワーカースレッドを起動
    let handles: Vec<_> = chunks
        .into_iter()
        .enumerate()
        .map(|(chunk_idx, chunk)| {
            let tx = tx.clone();
            let progress = progress.clone();

            thread::Builder::new()
                .stack_size(SEARCH_STACK_SIZE)
                .spawn(move || {
                    // 各ワーカースレッドで独自のSearchインスタンスを作成
                    // 各ワーカーは1スレッドで探索（マルチスレッド探索を無効化）
                    let mut search = Search::new(hash_mb);
                    search.set_num_threads(1);

                    for (record_idx, record) in chunk.iter().enumerate() {
                        if INTERRUPTED.load(Ordering::SeqCst) {
                            break;
                        }

                        let result = process_record_with_search(
                            record,
                            &mut search,
                            search_depth,
                            max_nodes,
                            max_time,
                            score_clip,
                            skip_in_check,
                            source_fv_scale,
                            target_fv_scale,
                        );

                        let global_idx = chunk_idx * chunk_size + record_idx;
                        let send_result = SearchProcessResult {
                            index: global_idx,
                            result,
                        };

                        if tx.send(send_result).is_err() {
                            break;
                        }

                        progress.inc(1);
                    }
                })
                .expect("Failed to spawn worker thread")
        })
        .collect();

    // 送信側をドロップ（全ワーカーが終了したらチャネルがクローズされる）
    drop(tx);

    // 結果を収集（順序を保持するためにインデックス付きで受け取る）
    let mut results_with_index: Vec<(usize, ProcessResult)> = Vec::with_capacity(actual_count);
    for search_result in rx {
        results_with_index.push((search_result.index, search_result.result));
    }

    // インデックスでソート
    results_with_index.sort_by_key(|(idx, _)| *idx);

    // rx のドレインが完了した時点でワーカーは既に終了しているため、join は即座に返る
    for handle in handles {
        let _ = handle.join();
    }

    progress.finish_with_message("Done");

    // ソート済み結果から直接書き出し（中間 Vec を排除）
    eprintln!("Writing output...");
    let out_file = File::create(output_path)
        .with_context(|| format!("Failed to create {}", output_path.display()))?;
    let mut writer = BufWriter::with_capacity(8 * 1024 * 1024, out_file);

    let mut written = 0u64;
    for (_, result) in results_with_index {
        match result {
            ProcessResult::Ok(record, clipped) => {
                if clipped {
                    clipped_count.fetch_add(1, Ordering::Relaxed);
                }
                writer.write_all(&record)?;
                written += 1;
            }
            ProcessResult::Skip => {
                skipped_count.fetch_add(1, Ordering::Relaxed);
            }
            ProcessResult::Error(e) => {
                error_count.fetch_add(1, Ordering::Relaxed);
                if verbose {
                    eprintln!("Error processing record: {e}");
                }
            }
        }
    }

    writer.flush()?;

    let final_errors = error_count.load(Ordering::SeqCst);
    let final_clipped = clipped_count.load(Ordering::SeqCst);
    let final_skipped = skipped_count.load(Ordering::SeqCst);
    if final_errors > 0 {
        eprintln!("Note: {final_errors} positions had errors");
    }
    if final_skipped > 0 {
        eprintln!(
            "Skipped (in check): {final_skipped} ({:.2}%)",
            final_skipped as f64 / actual_count as f64 * 100.0
        );
    }
    eprintln!(
        "Clipped scores: {final_clipped} ({:.2}%)",
        final_clipped as f64 / actual_count as f64 * 100.0
    );
    eprintln!("Wrote {written} records");

    Ok(())
}

/// 探索結果（インデックス付き）
struct SearchProcessResult {
    index: usize,
    result: ProcessResult,
}

/// 深さ指定探索で1レコードを処理
fn process_record_with_search(
    record: &[u8; PackedSfenValue::SIZE],
    search: &mut Search,
    depth: i32,
    max_nodes: u64,
    max_time: i64,
    score_clip: i16,
    skip_in_check: bool,
    source_fv_scale: i32,
    target_fv_scale: i32,
) -> ProcessResult {
    // PackedSfenValueを読み込み
    let psv = match PackedSfenValue::from_bytes(record) {
        Some(p) => p,
        None => return ProcessResult::Error(anyhow::anyhow!("Failed to parse PackedSfenValue")),
    };

    // PackedSfen → SFEN → Position
    let sfen = match unpack_sfen(&psv.sfen) {
        Ok(s) => s,
        Err(e) => return ProcessResult::Error(anyhow::anyhow!("Failed to unpack SFEN: {e}")),
    };

    let mut pos = Position::new();
    if let Err(e) = pos.set_sfen(&sfen) {
        return ProcessResult::Error(anyhow::anyhow!("Failed to set SFEN: {e:?}"));
    }

    // 王手局面をスキップ
    if skip_in_check && pos.in_check() {
        return ProcessResult::Skip;
    }

    // 探索を実行
    let mut limits = LimitsType::default();
    limits.depth = depth;
    if max_nodes > 0 {
        limits.nodes = max_nodes;
    }
    if max_time > 0 {
        limits.movetime = max_time;
    }
    limits.set_start_time();

    let search_result = search.go(&mut pos, limits, None::<fn(&rshogi_core::search::SearchInfo)>);

    // 探索結果のスコアを取得（STM視点）
    let raw_score: i32 = search_result.score.into();

    // FV_SCALE補正
    let scaled_score = raw_score * source_fv_scale / target_fv_scale;

    // スコアをクリップ
    let clipped = scaled_score.abs() > score_clip as i32;
    let new_score = scaled_score.clamp(-score_clip as i32, score_clip as i32) as i16;

    // 新しいPackedSfenValueを作成（局面は変更しない）
    let new_psv = PackedSfenValue {
        sfen: psv.sfen,
        score: new_score,
        move16: 0, // 無効値
        game_ply: psv.game_ply,
        game_result: psv.game_result,
        padding: 0,
    };

    ProcessResult::Ok(new_psv.to_bytes(), clipped)
}

// ============================================================
// 外部USIエンジンによるリスコア
// ============================================================

/// 外部USIエンジンの管理構造体
struct UsiEngine {
    child: Child,
    stdin: BufWriter<std::process::ChildStdin>,
    stdout: BufReader<std::process::ChildStdout>,
}

impl UsiEngine {
    /// USIエンジンを起動し、初期化する
    fn new(
        engine_path: &std::path::Path,
        usi_options: &[String],
        _timeout: std::time::Duration,
    ) -> Result<Self> {
        eprintln!("Starting USI engine: {}", engine_path.display());

        let mut child = Command::new(engine_path)
            .current_dir(engine_path.parent().unwrap_or(std::path::Path::new(".")))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("Failed to start engine: {}", engine_path.display()))?;

        let stdin = BufWriter::new(child.stdin.take().expect("stdin"));
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));

        let mut engine = Self {
            child,
            stdin,
            stdout,
        };

        // USIハンドシェイク
        engine.send_command("usi")?;
        engine.wait_for("usiok")?;

        // USIオプション設定
        for opt in usi_options {
            if let Some((name, value)) = opt.split_once('=') {
                engine.send_command(&format!("setoption name {name} value {value}"))?;
            } else {
                eprintln!("Warning: invalid USI option format (expected Name=Value): {opt}");
            }
        }

        // isready/readyok（TensorRTビルド等で長時間かかる場合あり）
        eprintln!("Waiting for engine ready (TensorRT build may take a while)...");
        engine.send_command("isready")?;
        engine.wait_for("readyok")?;
        eprintln!("Engine ready.");

        // ウォームアップ: 初期局面で評価して GPU/TRT ランタイムを安定させる
        eprintln!("Warming up engine...");
        engine.send_command("usinewgame")?;
        engine.send_command(
            "position sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
        )?;
        engine.send_command("go nodes 1")?;
        engine.wait_for_bestmove()?;
        // 2回目: DLshogi系は初回goがスレッドプール初期化を含む場合がある
        engine.send_command(
            "position sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
        )?;
        engine.send_command("go nodes 1")?;
        engine.wait_for_bestmove()?;
        eprintln!("Warmup complete.");

        Ok(engine)
    }

    fn send_command(&mut self, cmd: &str) -> Result<()> {
        writeln!(self.stdin, "{cmd}")?;
        self.stdin.flush()?;
        Ok(())
    }

    fn wait_for(&mut self, expected: &str) -> Result<()> {
        let mut line = String::new();
        loop {
            line.clear();
            let n = self.stdout.read_line(&mut line)?;
            if n == 0 {
                anyhow::bail!("Engine process closed stdout while waiting for '{expected}'");
            }
            if line.trim() == expected {
                break;
            }
        }
        Ok(())
    }

    /// bestmove行まで読み飛ばす
    fn wait_for_bestmove(&mut self) -> Result<()> {
        let mut line = String::new();
        loop {
            line.clear();
            let n = self.stdout.read_line(&mut line)?;
            if n == 0 {
                anyhow::bail!("Engine process closed stdout while waiting for bestmove");
            }
            if line.trim().starts_with("bestmove") {
                break;
            }
        }
        Ok(())
    }

    /// 局面を評価し、score cp 値を返す
    fn evaluate_position(&mut self, sfen: &str, nodes: u64) -> Result<Option<i32>> {
        self.send_command(&format!("position sfen {sfen}"))?;
        if nodes > 0 {
            self.send_command(&format!("go nodes {nodes}"))?;
        } else {
            self.send_command("go depth 1")?;
        }

        let mut score: Option<i32> = None;
        let mut line = String::new();

        loop {
            line.clear();
            let n = self.stdout.read_line(&mut line)?;
            if n == 0 {
                anyhow::bail!("Engine process closed stdout during evaluation");
            }
            let trimmed = line.trim();

            // score cp / score mate を抽出（最後のinfo行のものを採用）
            if trimmed.starts_with("info") {
                if let Some(cp_idx) = trimmed.find("score cp") {
                    let rest = &trimmed[cp_idx + 9..];
                    let end_idx = rest.find(' ').unwrap_or(rest.len());
                    if let Ok(cp) = rest[..end_idx].parse::<i32>() {
                        score = Some(cp);
                    }
                } else if let Some(mate_idx) = trimmed.find("score mate") {
                    let rest = &trimmed[mate_idx + 11..];
                    let end_idx = rest.find(' ').unwrap_or(rest.len());
                    if let Ok(mate_in) = rest[..end_idx].parse::<i32>() {
                        score = Some(if mate_in > 0 { 30000 } else { -30000 });
                    }
                }
            }

            if trimmed.starts_with("bestmove") {
                if trimmed.contains("resign") && score.is_none() {
                    score = Some(-30000);
                }
                break;
            }
        }

        Ok(score)
    }

    fn quit(&mut self) -> Result<()> {
        let _ = self.send_command("quit");
        let _ = self.child.wait();
        Ok(())
    }
}

/// エンジン処理結果（インデックス付き）
struct EngineProcessResult {
    index: usize,
    score: Option<i32>,
    psv: PackedSfenValue,
}

/// 外部USIエンジンでファイルを処理（複数エンジン並列対応）
fn process_file_with_engine(
    cli: &Cli,
    engines: &mut [UsiEngine],
    input_path: &PathBuf,
    output_path: &PathBuf,
    process_count: u64,
    progress: &FileProgress,
) -> Result<()> {
    let num_engines = engines.len();

    // 入力ファイルを読み込み
    let in_file = File::open(input_path)
        .with_context(|| format!("Failed to open {}", input_path.display()))?;
    let mut reader = BufReader::new(in_file);

    // 全レコードを読み込み（SFEN展開・フィルタリング含む）
    progress.set_message("Reading...");
    let mut records: Vec<(usize, PackedSfenValue, String)> = Vec::new(); // (global_index, psv, sfen)
    let mut buffer = [0u8; PackedSfenValue::SIZE];
    let mut skipped_count: u64 = 0;
    let mut read_errors: u64 = 0;

    for global_idx in 0..process_count as usize {
        if INTERRUPTED.load(Ordering::SeqCst) {
            break;
        }
        match reader.read_exact(&mut buffer) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        }
        let psv = match PackedSfenValue::from_bytes(&buffer) {
            Some(p) => p,
            None => {
                read_errors += 1;
                progress.inc(1);
                continue;
            }
        };
        let sfen = match unpack_sfen(&psv.sfen) {
            Ok(s) => s,
            Err(_) => {
                read_errors += 1;
                progress.inc(1);
                continue;
            }
        };
        if cli.skip_in_check {
            let mut pos = Position::new();
            if pos.set_sfen(&sfen).is_ok() && pos.in_check() {
                skipped_count += 1;
                progress.inc(1);
                continue;
            }
        }
        records.push((global_idx, psv, sfen));
    }

    let actual_count = records.len();
    eprintln!(
        "Read {} records ({} skipped, {} errors)",
        actual_count, skipped_count, read_errors
    );

    if actual_count == 0 {
        progress.finish_with_message("Done (empty)");
        File::create(output_path)?;
        return Ok(());
    }

    // レコードをチャンクに分割
    let chunk_size = actual_count.div_ceil(num_engines).max(1);
    let chunks: Vec<Vec<(usize, PackedSfenValue, String)>> =
        records.chunks(chunk_size).map(|c| c.to_vec()).collect();

    eprintln!("Using {} engine process(es), chunk_size={}", chunks.len(), chunk_size);

    // 各チャンクをワーカースレッドで処理
    let score_clip = cli.score_clip;
    let engine_nodes = cli.engine_nodes;
    let verbose = cli.verbose;

    let error_count = AtomicU64::new(read_errors);
    let clipped_count = AtomicU64::new(0);

    let (tx, rx) = mpsc::channel::<EngineProcessResult>();

    progress.set_message("Processing...");

    std::thread::scope(|s| {
        let mut handles = Vec::new();

        for (engine, chunk) in engines.iter_mut().zip(chunks) {
            let tx = tx.clone();
            let progress = progress.clone();
            let error_count = &error_count;
            let _clipped_count = &clipped_count;

            handles.push(s.spawn(move || {
                // usinewgame でリセット
                let _ = engine.send_command("usinewgame");

                for (global_idx, psv, sfen) in &chunk {
                    if INTERRUPTED.load(Ordering::SeqCst) {
                        break;
                    }

                    match engine.evaluate_position(sfen, engine_nodes) {
                        Ok(score) => {
                            if score.is_none() {
                                error_count.fetch_add(1, Ordering::Relaxed);
                                if verbose {
                                    eprintln!("No score returned for: {sfen}");
                                }
                            }
                            let _ = tx.send(EngineProcessResult {
                                index: *global_idx,
                                score,
                                psv: *psv,
                            });
                        }
                        Err(e) => {
                            error_count.fetch_add(1, Ordering::Relaxed);
                            if verbose {
                                eprintln!("Engine error for {sfen}: {e}");
                            }
                            // スコアなしで送信（エンジン死亡時はループを抜ける）
                            let _ = tx.send(EngineProcessResult {
                                index: *global_idx,
                                score: None,
                                psv: *psv,
                            });
                            break;
                        }
                    }

                    progress.inc(1);
                }
            }));
        }

        drop(tx); // 全ワーカーの送信側をドロップ

        // 結果を収集
        let mut results: Vec<EngineProcessResult> = rx.into_iter().collect();
        results.sort_by_key(|r| r.index);

        // 出力レコードを構築
        let mut processed_records: Vec<[u8; PackedSfenValue::SIZE]> =
            Vec::with_capacity(results.len());

        for r in &results {
            if let Some(raw_score) = r.score {
                let clipped = raw_score.abs() > score_clip as i32;
                let new_score = raw_score.clamp(-score_clip as i32, score_clip as i32) as i16;
                if clipped {
                    clipped_count.fetch_add(1, Ordering::Relaxed);
                }
                let new_psv = PackedSfenValue {
                    sfen: r.psv.sfen,
                    score: new_score,
                    move16: 0,
                    game_ply: r.psv.game_ply,
                    game_result: r.psv.game_result,
                    padding: 0,
                };
                processed_records.push(new_psv.to_bytes());
            }
        }

        // ワーカースレッド完了待ち
        for h in handles {
            let _ = h.join();
        }

        progress.finish_with_message("Done");

        let final_errors = error_count.load(Ordering::SeqCst);
        let final_clipped = clipped_count.load(Ordering::SeqCst);
        let total = actual_count as u64 + skipped_count + read_errors;
        if final_errors > 0 {
            eprintln!("Note: {final_errors} positions had errors");
        }
        if skipped_count > 0 {
            eprintln!(
                "Skipped (in check): {skipped_count} ({:.2}%)",
                skipped_count as f64 / total as f64 * 100.0
            );
        }
        if total > 0 {
            eprintln!(
                "Clipped scores: {final_clipped} ({:.2}%)",
                final_clipped as f64 / total as f64 * 100.0
            );
        }

        // 出力ファイルに書き込み
        eprintln!("Writing output...");
        let out_file = File::create(output_path)
            .with_context(|| format!("Failed to create {}", output_path.display()))
            .unwrap();
        let mut writer = BufWriter::new(out_file);

        for record in &processed_records {
            writer.write_all(record).unwrap();
        }

        writer.flush().unwrap();
        eprintln!("Wrote {} records", processed_records.len());
    });

    Ok(())
}

// ============================================================
// ONNX 直接推論モード (共通パイプライン)
// ============================================================

/// ort のエラーを anyhow に変換 (ort::Error は Send+Sync を満たさないため)
#[cfg(any(feature = "aobazero-onnx", feature = "dlshogi-onnx"))]
fn onnx_ort_err(e: ort::Error) -> anyhow::Error {
    anyhow::anyhow!("ONNX Runtime error: {e}")
}

/// ONNX 直接推論パイプラインの共通設定
///
/// 全フィールドが `Copy` のため、関数内で destructure して既存ローカル変数名と
/// 互換に展開できる。
#[cfg(any(feature = "aobazero-onnx", feature = "dlshogi-onnx"))]
#[derive(Clone, Copy)]
struct OnnxPipelineConfig<'a> {
    model_name: &'a str,
    model_kind: &'a str,
    onnx_path: &'a std::path::Path,
    input_path: &'a std::path::Path,
    output_path: &'a std::path::Path,
    process_count: u64,
    batch_size: usize,
    gpu_id: i32,
    use_tensorrt: bool,
    tensorrt_cache: Option<&'a std::path::Path>,
    score_clip: i16,
    eval_scale: f32,
    skip_in_check: bool,
    onnx_draw_ply: i32,
    f1_size: usize,
    f2_size: usize,
    input1_channels: usize,
    input2_channels: usize,
    profile_path: Option<&'a std::path::Path>,
    expand: Option<ExpandConfig<'a>>,
    /// root 局面据え置き・ラベルのみ葉評価モード（`--qsearch-leaf-label`）。
    /// 真のとき各局面を NNUE qsearch で葉まで進めてから特徴量を構築し、
    /// 出力 sfen は root のまま・STM 変化時はスコアを符号反転する。
    qsearch_leaf_label: bool,
    /// qsearch の最大深さ（`qsearch_leaf_label` 時のみ使用）
    qsearch_max_ply: i32,
    /// 葉探索用 NNUE モデルのパス（`qsearch_leaf_label` 時のみ Some）。
    /// 葉 PV ＝ ONNX が評価する葉局面が NNUE に依存するため、NNUE 差し替えを
    /// marker で検知できるよう fingerprint に path/size/mtime を含める。
    qsearch_nnue_path: Option<&'a std::path::Path>,
    /// leaf-REPLACEMENT arm の出力パス（`--qsearch-leaf-replacement-output`）。
    /// `qsearch_leaf_label` 併用時のみ Some。葉局面に置換したレコード
    /// （葉 sfen + 葉評価・符号反転なし）をこのパスに書き出す。leaf-LABEL arm
    /// （`output_path`）とは 1:1 lockstep で同数のレコードを書く。
    replacement_output: Option<&'a std::path::Path>,
}

/// ポリシー展開機能の設定（`OnnxPipelineConfig::expand` が `Some` のときのみ動作）
#[cfg(any(feature = "aobazero-onnx", feature = "dlshogi-onnx"))]
#[derive(Clone, Copy)]
struct ExpandConfig<'a> {
    /// 展開した子局面の出力ファイルパス（入力ファイル名を `--expand-output-dir` に置いたもの）
    output_path: &'a std::path::Path,
    /// 合法手 softmax 確率の閾値（割合: `0.0 < v <= 1.0`、CLI の `%` から内部変換済み）
    threshold: f32,
    /// 親局面が王手なら expand 出力をスキップ
    skip_parent_in_check: bool,
    /// 展開した子局面が王手なら expand 出力をスキップ
    skip_child_in_check: bool,
}

/// 完了マーカーのフォーマットバージョン
#[cfg(any(feature = "aobazero-onnx", feature = "dlshogi-onnx"))]
const MARKER_VERSION: u32 = 1;

/// 完了マーカーの fingerprint 部（出力内容を一意に決める実行設定）
#[cfg(any(feature = "aobazero-onnx", feature = "dlshogi-onnx"))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct RunFingerprint {
    version: u32,
    mode: String,
    model_kind: String,
    model_path: PathBuf,
    model_size: u64,
    model_mtime_ns: u128,
    input_path: PathBuf,
    input_size: u64,
    input_mtime_ns: u128,
    process_count: u64,
    skip_in_check: bool,
    score_clip: i16,
    eval_scale_bits: u32,
    onnx_draw_ply: i32,
    // 出力内容を変える要素: root 据え置き・葉ラベルモードと葉探索深さ。
    // モード差で resume / marker が誤って一致しないよう fingerprint に含める。
    // max_ply は葉ラベル時のみ意味を持つので Option（expand_* と同じ扱い）。
    qsearch_leaf_label: bool,
    qsearch_max_ply: Option<i32>,
    // 葉探索用 NNUE（`--nnue`）。葉ラベルモードでは葉局面＝出力が NNUE に依存するため、
    // path/size/mtime を fingerprint に含める。qsearch_max_ply と同じく leaf_label 時のみ Some。
    qsearch_nnue_path: Option<PathBuf>,
    qsearch_nnue_size: Option<u64>,
    qsearch_nnue_mtime_ns: Option<u128>,
    expand: bool,
    expand_threshold_bits: Option<u32>,
    expand_skip_parent_in_check: Option<bool>,
    expand_skip_child_in_check: Option<bool>,
    expand_output_path: Option<PathBuf>,
    // leaf-REPLACEMENT arm（`--qsearch-leaf-replacement-output`）。expand_* と同じ
    // Option パターン。出力内容を変えるため fingerprint に含める。
    replacement: bool,
    replacement_output_path: Option<PathBuf>,
}

/// 完了マーカーの出力サイズ情報（fingerprint とは分離）
#[cfg(any(feature = "aobazero-onnx", feature = "dlshogi-onnx"))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct OutputSizes {
    rescore_output_size: u64,
    expand_output_size: Option<u64>,
    replacement_output_size: Option<u64>,
}

/// 完了マーカー全体（fingerprint + output sizes）
#[cfg(any(feature = "aobazero-onnx", feature = "dlshogi-onnx"))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct DoneMarker {
    fingerprint: RunFingerprint,
    output_sizes: OutputSizes,
}

/// `<rescore_output>.done` を返す
#[cfg(any(feature = "aobazero-onnx", feature = "dlshogi-onnx"))]
fn marker_path_for(rescore_output: &std::path::Path) -> PathBuf {
    let mut s = rescore_output.as_os_str().to_owned();
    s.push(".done");
    PathBuf::from(s)
}

/// ファイルの `(len, modified_unix_ns)` を取得
#[cfg(any(feature = "aobazero-onnx", feature = "dlshogi-onnx"))]
fn file_size_mtime_ns(path: &std::path::Path) -> Result<(u64, u128)> {
    let m = fs::metadata(path).with_context(|| format!("Failed to stat {}", path.display()))?;
    let mtime = m
        .modified()
        .with_context(|| format!("Failed to read mtime: {}", path.display()))?;
    let dur = mtime
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .with_context(|| format!("Invalid mtime (before UNIX epoch): {}", path.display()))?;
    Ok((m.len(), dur.as_nanos()))
}

/// `DoneMarker` をテキスト key=value 形式にシリアライズ
#[cfg(any(feature = "aobazero-onnx", feature = "dlshogi-onnx"))]
fn serialize_marker(marker: &DoneMarker) -> String {
    let f = &marker.fingerprint;
    let mut out = String::new();
    use std::fmt::Write;
    let _ = writeln!(out, "version={}", f.version);
    let _ = writeln!(out, "mode={}", f.mode);
    let _ = writeln!(out, "model_kind={}", f.model_kind);
    let _ = writeln!(out, "model_path={}", f.model_path.display());
    let _ = writeln!(out, "model_size={}", f.model_size);
    let _ = writeln!(out, "model_mtime_ns={}", f.model_mtime_ns);
    let _ = writeln!(out, "input_path={}", f.input_path.display());
    let _ = writeln!(out, "input_size={}", f.input_size);
    let _ = writeln!(out, "input_mtime_ns={}", f.input_mtime_ns);
    let _ = writeln!(out, "process_count={}", f.process_count);
    let _ = writeln!(out, "skip_in_check={}", f.skip_in_check);
    let _ = writeln!(out, "score_clip={}", f.score_clip);
    let _ = writeln!(out, "eval_scale_bits=0x{:08x}", f.eval_scale_bits);
    let _ = writeln!(out, "onnx_draw_ply={}", f.onnx_draw_ply);
    let _ = writeln!(out, "qsearch_leaf_label={}", f.qsearch_leaf_label);
    if f.qsearch_leaf_label {
        let _ = writeln!(
            out,
            "qsearch_max_ply={}",
            f.qsearch_max_ply.expect("qsearch_leaf_label=true requires max_ply")
        );
        let _ = writeln!(
            out,
            "qsearch_nnue_path={}",
            f.qsearch_nnue_path
                .as_ref()
                .expect("qsearch_leaf_label=true requires nnue path")
                .display()
        );
        let _ = writeln!(
            out,
            "qsearch_nnue_size={}",
            f.qsearch_nnue_size.expect("qsearch_leaf_label=true requires nnue size")
        );
        let _ = writeln!(
            out,
            "qsearch_nnue_mtime_ns={}",
            f.qsearch_nnue_mtime_ns.expect("qsearch_leaf_label=true requires nnue mtime")
        );
    }
    let _ = writeln!(out, "expand={}", f.expand);
    if f.expand {
        let _ = writeln!(
            out,
            "expand_threshold_bits=0x{:08x}",
            f.expand_threshold_bits.expect("expand=true requires threshold")
        );
        let _ = writeln!(
            out,
            "expand_skip_parent_in_check={}",
            f.expand_skip_parent_in_check.expect("expand=true requires skip_parent")
        );
        let _ = writeln!(
            out,
            "expand_skip_child_in_check={}",
            f.expand_skip_child_in_check.expect("expand=true requires skip_child")
        );
        let _ = writeln!(
            out,
            "expand_output_path={}",
            f.expand_output_path.as_ref().expect("expand=true requires path").display()
        );
    }
    let _ = writeln!(out, "replacement={}", f.replacement);
    if f.replacement {
        let _ = writeln!(
            out,
            "replacement_output_path={}",
            f.replacement_output_path
                .as_ref()
                .expect("replacement=true requires path")
                .display()
        );
    }
    let _ = writeln!(out, "rescore_output_size={}", marker.output_sizes.rescore_output_size);
    if f.expand {
        let _ = writeln!(
            out,
            "expand_output_size={}",
            marker
                .output_sizes
                .expand_output_size
                .expect("expand=true requires expand_output_size")
        );
    }
    if f.replacement {
        let _ = writeln!(
            out,
            "replacement_output_size={}",
            marker
                .output_sizes
                .replacement_output_size
                .expect("replacement=true requires replacement_output_size")
        );
    }
    out
}

/// マーカーファイルをパース（key=value テキスト形式、行頭空白・コメントは未対応）
#[cfg(any(feature = "aobazero-onnx", feature = "dlshogi-onnx"))]
fn parse_marker(path: &std::path::Path) -> Result<DoneMarker> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("Failed to read marker {}", path.display()))?;
    let mut map: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for (lineno, raw) in text.lines().enumerate() {
        if raw.is_empty() {
            continue;
        }
        let (k, v) = raw.split_once('=').ok_or_else(|| {
            anyhow::anyhow!("Marker {} line {}: missing '='", path.display(), lineno + 1)
        })?;
        map.insert(k.to_string(), v.to_string());
    }
    let get = |k: &str| -> Result<&String> {
        map.get(k)
            .ok_or_else(|| anyhow::anyhow!("Marker {} missing key {k}", path.display()))
    };
    let parse_hex_u32 = |s: &str| -> Result<u32> {
        let stripped = s
            .strip_prefix("0x")
            .ok_or_else(|| anyhow::anyhow!("expected '0x' prefix, got: {s}"))?;
        u32::from_str_radix(stripped, 16).context("invalid hex u32")
    };
    let parse_bool = |s: &str| -> Result<bool> {
        match s {
            "true" => Ok(true),
            "false" => Ok(false),
            other => anyhow::bail!("invalid bool: {other}"),
        }
    };

    let version: u32 = get("version")?.parse().context("invalid version")?;
    if version != MARKER_VERSION {
        anyhow::bail!("Unsupported marker version: {version} (expected {MARKER_VERSION})");
    }
    let expand = parse_bool(get("expand")?)?;
    // 後方互換: 旧 marker には無いキー。欠落時は false（葉ラベル未使用）扱い。
    let qsearch_leaf_label = match map.get("qsearch_leaf_label") {
        Some(v) => parse_bool(v)?,
        None => false,
    };
    // 後方互換: 旧 marker には無いキー。欠落時は false（replacement arm 未使用）扱い。
    let replacement = match map.get("replacement") {
        Some(v) => parse_bool(v)?,
        None => false,
    };
    let fingerprint = RunFingerprint {
        version,
        mode: get("mode")?.clone(),
        model_kind: get("model_kind")?.clone(),
        model_path: PathBuf::from(get("model_path")?),
        model_size: get("model_size")?.parse().context("invalid model_size")?,
        model_mtime_ns: get("model_mtime_ns")?.parse().context("invalid model_mtime_ns")?,
        input_path: PathBuf::from(get("input_path")?),
        input_size: get("input_size")?.parse().context("invalid input_size")?,
        input_mtime_ns: get("input_mtime_ns")?.parse().context("invalid input_mtime_ns")?,
        process_count: get("process_count")?.parse().context("invalid process_count")?,
        skip_in_check: parse_bool(get("skip_in_check")?)?,
        score_clip: get("score_clip")?.parse().context("invalid score_clip")?,
        eval_scale_bits: parse_hex_u32(get("eval_scale_bits")?)?,
        onnx_draw_ply: get("onnx_draw_ply")?.parse().context("invalid onnx_draw_ply")?,
        qsearch_leaf_label,
        qsearch_max_ply: if qsearch_leaf_label {
            Some(get("qsearch_max_ply")?.parse().context("invalid qsearch_max_ply")?)
        } else {
            None
        },
        // 葉探索 NNUE のメタは leaf-label モードより後に追加したキーのため、leaf_label=true でも
        // キー欠落（葉探索 NNUE 未追跡の旧 leaf-label marker）を None として許容する。現 fingerprint は
        // Some(nnue) になるので不一致 → marker 不一致経路で再生成され、hard parse error にはしない。
        qsearch_nnue_path: map.get("qsearch_nnue_path").map(PathBuf::from),
        qsearch_nnue_size: map
            .get("qsearch_nnue_size")
            .map(|v| v.parse().context("invalid qsearch_nnue_size"))
            .transpose()?,
        qsearch_nnue_mtime_ns: map
            .get("qsearch_nnue_mtime_ns")
            .map(|v| v.parse().context("invalid qsearch_nnue_mtime_ns"))
            .transpose()?,
        expand,
        expand_threshold_bits: if expand {
            Some(parse_hex_u32(get("expand_threshold_bits")?)?)
        } else {
            None
        },
        expand_skip_parent_in_check: if expand {
            Some(parse_bool(get("expand_skip_parent_in_check")?)?)
        } else {
            None
        },
        expand_skip_child_in_check: if expand {
            Some(parse_bool(get("expand_skip_child_in_check")?)?)
        } else {
            None
        },
        expand_output_path: if expand {
            Some(PathBuf::from(get("expand_output_path")?))
        } else {
            None
        },
        replacement,
        replacement_output_path: if replacement {
            Some(PathBuf::from(get("replacement_output_path")?))
        } else {
            None
        },
    };
    let output_sizes = OutputSizes {
        rescore_output_size: get("rescore_output_size")?
            .parse()
            .context("invalid rescore_output_size")?,
        expand_output_size: if expand {
            Some(get("expand_output_size")?.parse().context("invalid expand_output_size")?)
        } else {
            None
        },
        replacement_output_size: if replacement {
            Some(
                get("replacement_output_size")?
                    .parse()
                    .context("invalid replacement_output_size")?,
            )
        } else {
            None
        },
    };
    Ok(DoneMarker {
        fingerprint,
        output_sizes,
    })
}

/// マーカーを atomic に書き出す（tmp に書く → sync_all → rename）。
/// 他の出力パスと同様、tmp / final が symlink の場合は拒否する
/// （symlink を follow して任意ファイルを上書きするのを防ぐ）。
#[cfg(any(feature = "aobazero-onnx", feature = "dlshogi-onnx"))]
fn write_marker_atomic(rescore_output: &std::path::Path, marker: &DoneMarker) -> Result<()> {
    let final_path = marker_path_for(rescore_output);
    let mut tmp_os = final_path.as_os_str().to_owned();
    tmp_os.push(".tmp");
    let tmp_path = PathBuf::from(tmp_os);

    // symlink 拒否: final / tmp どちらも symlink 経由の書き込みをブロック。
    // tmp に前回クラッシュの残骸 (通常ファイル) があれば事前削除する。
    for p in [&final_path, &tmp_path] {
        if let Ok(meta) = fs::symlink_metadata(p)
            && meta.file_type().is_symlink()
        {
            anyhow::bail!(
                "Marker path is a symlink (refusing to write): {}. \
                 Remove the symlink or choose a different directory.",
                p.display()
            );
        }
    }
    if tmp_path.exists() {
        fs::remove_file(&tmp_path)
            .with_context(|| format!("Failed to remove stale marker tmp {}", tmp_path.display()))?;
    }

    let body = serialize_marker(marker);
    {
        // `create_new(true)` により、競合で tmp を別プロセスが作っていた場合は
        // 失敗させる（上の symlink チェックと TOCTOU 窓を狭める）。
        let mut f = File::options()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
            .with_context(|| format!("Failed to create marker tmp {}", tmp_path.display()))?;
        f.write_all(body.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp_path, &final_path).with_context(|| {
        format!("Failed to rename marker {} → {}", tmp_path.display(), final_path.display())
    })?;
    Ok(())
}

/// マーカーの `key=value\n` テキスト形式で安全に round-trip できないパスを拒否する。
///
/// 具体的には以下のいずれかを含むパスを起動時にエラーにする:
/// - 非 UTF-8 バイト列（`Path::to_str` が `None`、`Path::display` が lossy になる）
/// - `=`（key/value セパレータと衝突）
/// - `\n` / `\r`（レコードセパレータと衝突）
///
/// 現実的には `=` を含むパスだけが稀に発生する（例:
/// `v1.0=alpha/model.onnx`）。非 UTF-8 と改行はほぼ起こらないが、同じ
/// 検証枠で拾っておく。遭遇時はパスをリネームすることで回避可能。
#[cfg(any(feature = "aobazero-onnx", feature = "dlshogi-onnx"))]
fn ensure_marker_safe_path(kind: &str, path: &std::path::Path) -> Result<()> {
    let s = path.to_str().ok_or_else(|| {
        anyhow::anyhow!(
            "{kind} path contains non-UTF-8 characters which cannot be recorded in the \
             completion marker: {}. Rename the path to ASCII / UTF-8 only characters.",
            path.display()
        )
    })?;
    if let Some(bad) = s.chars().find(|c| *c == '=' || *c == '\n' || *c == '\r') {
        let name = match bad {
            '=' => "'='",
            '\n' => "LF (newline)",
            '\r' => "CR",
            _ => unreachable!(),
        };
        anyhow::bail!(
            "{kind} path contains {name} which is not supported by the completion marker \
             format (key=value text): {}. Rename the path to avoid these characters.",
            path.display()
        );
    }
    Ok(())
}

/// `OnnxPipelineConfig` から `RunFingerprint` を構築
#[cfg(any(feature = "aobazero-onnx", feature = "dlshogi-onnx"))]
fn build_run_fingerprint(config: &OnnxPipelineConfig<'_>) -> Result<RunFingerprint> {
    let model_path = config
        .onnx_path
        .canonicalize()
        .with_context(|| format!("Failed to canonicalize {}", config.onnx_path.display()))?;
    ensure_marker_safe_path("--onnx-model / --dlshogi-onnx-model", &model_path)?;
    let (model_size, model_mtime_ns) = file_size_mtime_ns(&model_path)?;
    let input_path = config
        .input_path
        .canonicalize()
        .with_context(|| format!("Failed to canonicalize {}", config.input_path.display()))?;
    ensure_marker_safe_path("input", &input_path)?;
    let (input_size, input_mtime_ns) = file_size_mtime_ns(&input_path)?;
    let expand_output_path = match config.expand {
        Some(e) => {
            // 出力ファイルが未作成でも canonical_dir + file_name で予定パスを構築する
            // ため、parent dir のみ canonicalize する
            let p = canonicalize_predicted_path(e.output_path)?;
            ensure_marker_safe_path("--expand-output-dir (entry)", &p)?;
            Some(p)
        }
        None => None,
    };
    let replacement_output_path = match config.replacement_output {
        Some(rp) => {
            let p = canonicalize_predicted_path(rp)?;
            ensure_marker_safe_path("--qsearch-leaf-replacement-output (entry)", &p)?;
            Some(p)
        }
        None => None,
    };
    // 葉探索用 NNUE のメタを fingerprint へ（葉ラベル時のみ）。NNUE を差し替えると葉局面＝
    // 出力が変わるのに、含めないと marker が一致して stale な葉ラベルを再利用してしまう。
    let (qsearch_nnue_path, qsearch_nnue_size, qsearch_nnue_mtime_ns) = if config.qsearch_leaf_label
    {
        let p = config
            .qsearch_nnue_path
            .context("--qsearch-leaf-label requires --nnue path for fingerprint")?;
        let cp = p
            .canonicalize()
            .with_context(|| format!("Failed to canonicalize --nnue {}", p.display()))?;
        ensure_marker_safe_path("--nnue (qsearch leaf)", &cp)?;
        let (size, mtime) = file_size_mtime_ns(&cp)?;
        (Some(cp), Some(size), Some(mtime))
    } else {
        (None, None, None)
    };
    Ok(RunFingerprint {
        version: MARKER_VERSION,
        mode: "onnx".to_string(),
        model_kind: config.model_kind.to_string(),
        model_path,
        model_size,
        model_mtime_ns,
        input_path,
        input_size,
        input_mtime_ns,
        process_count: config.process_count,
        skip_in_check: config.skip_in_check,
        score_clip: config.score_clip,
        eval_scale_bits: config.eval_scale.to_bits(),
        onnx_draw_ply: config.onnx_draw_ply,
        qsearch_leaf_label: config.qsearch_leaf_label,
        qsearch_max_ply: config.qsearch_leaf_label.then_some(config.qsearch_max_ply),
        qsearch_nnue_path,
        qsearch_nnue_size,
        qsearch_nnue_mtime_ns,
        expand: config.expand.is_some(),
        expand_threshold_bits: config.expand.map(|e| e.threshold.to_bits()),
        expand_skip_parent_in_check: config.expand.map(|e| e.skip_parent_in_check),
        expand_skip_child_in_check: config.expand.map(|e| e.skip_child_in_check),
        expand_output_path,
        replacement: config.replacement_output.is_some(),
        replacement_output_path,
    })
}

/// 未作成出力ファイルの予定 canonical パスを構築する。
/// parent dir を canonicalize して file_name と join する。
#[cfg(any(feature = "aobazero-onnx", feature = "dlshogi-onnx"))]
fn canonicalize_predicted_path(path: &std::path::Path) -> Result<PathBuf> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Path has no parent directory: {}", path.display()))?;
    let file_name = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("Path has no file name: {}", path.display()))?;
    let canonical_parent = parent
        .canonicalize()
        .with_context(|| format!("Failed to canonicalize parent {}", parent.display()))?;
    Ok(canonical_parent.join(file_name))
}

/// ストリーミング読み込み + rayon 並列特徴量構築 + ゼロコピー GPU 推論
///
/// AobaZero / 標準 dlshogi の両方で共通のパイプライン処理。
/// `build_features` クロージャで特徴量構築の差異を吸収する。
#[cfg(any(feature = "aobazero-onnx", feature = "dlshogi-onnx"))]
fn process_file_with_onnx_pipeline<F>(
    config: &OnnxPipelineConfig<'_>,
    build_features: F,
    progress: &FileProgress,
) -> Result<()>
where
    F: Fn(&Position, &mut [f32], &mut [f32], &PackedSfenValue) + Send + Sync,
{
    let OnnxPipelineConfig {
        model_name,
        model_kind: _,
        onnx_path,
        input_path,
        output_path,
        process_count,
        batch_size,
        gpu_id,
        use_tensorrt,
        tensorrt_cache,
        score_clip,
        eval_scale,
        skip_in_check,
        onnx_draw_ply: _,
        f1_size,
        f2_size,
        input1_channels,
        input2_channels,
        profile_path,
        expand,
        qsearch_leaf_label,
        qsearch_max_ply,
        // fingerprint 用。pipeline 本体では使わず build_run_fingerprint(config) で参照する。
        qsearch_nnue_path: _,
        replacement_output,
    } = *config;
    use ort::ep::ExecutionProvider;
    use ort::memory::{AllocationDevice, AllocatorType, MemoryInfo, MemoryType};
    use ort::session::Session;
    use ort::value::TensorRef;
    use rshogi_core::movegen::{MoveList, generate_legal};
    use tools::dlshogi_features::{MAX_MOVE_LABEL_NUM, make_move_label};

    /// 合法手のロジットを softmax 正規化して `out` に書き込む
    fn softmax_normalize(logits: &[f32], out: &mut [f32]) {
        debug_assert_eq!(logits.len(), out.len());
        let max = logits.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        let mut sum = 0.0f32;
        for (o, &l) in out.iter_mut().zip(logits.iter()) {
            *o = (l - max).exp();
            sum += *o;
        }
        let inv = if sum > 0.0 { 1.0 / sum } else { 0.0 };
        for o in out.iter_mut() {
            *o *= inv;
        }
    }

    // ORT_DYLIB_PATH 事前検証（load-dynamic feature）
    //
    // ort クレートは load-dynamic feature 有効時、ORT_DYLIB_PATH 環境変数で指定された
    // ライブラリを dlopen する。未設定時はシステムパスから "libonnxruntime.so" を探すが、
    // 見つからない場合にハングする（エラーではなくブロックする）。
    //
    // ort の default features には download-binaries（ビルド時に CPU 版ランタイムを
    // 自動ダウンロード）が含まれるが、load-dynamic とは #[cfg] で排他のため共存不可。
    // また download-binaries で取得されるのは CPU 版のみで GPU 版は提供されない。
    //
    // したがって load-dynamic を使い、ランタイムはユーザーが自前で用意する設計とする。
    // GPU 推論がデフォルト。暗黙の CPU フォールバックは CUDA EP チェックで防止する。
    //
    // ORT_DYLIB_PATH は GPU/CPU どちらのモードでも必須。
    // 未設定時に ort がシステムパスから探すとハングするため。
    match std::env::var("ORT_DYLIB_PATH") {
        Ok(path) if !path.is_empty() => {
            if !std::path::Path::new(&path).is_file() {
                anyhow::bail!(
                    "ORT_DYLIB_PATH is set to '{path}' but the file does not exist \
                     (or is not a regular file).\n\
                     Download ONNX Runtime from:\n  \
                     https://github.com/microsoft/onnxruntime/releases"
                );
            }
            eprintln!("ORT_DYLIB_PATH: {path}");
        }
        _ => {
            let mode_hint = if gpu_id >= 0 {
                "GPU inference requires a GPU-enabled ONNX Runtime (onnxruntime-linux-x64-gpu-*)."
            } else {
                "CPU inference requires an ONNX Runtime library (GPU or CPU build)."
            };
            anyhow::bail!(
                "ORT_DYLIB_PATH environment variable is not set.\n\
                 {mode_hint}\n\
                 Download from: https://github.com/microsoft/onnxruntime/releases\n\
                 Example:\n  \
                 ORT_DYLIB_PATH=/path/to/libonnxruntime.so cargo run ..."
            );
        }
    }

    // ONNX Runtime セッション初期化
    eprintln!("Loading {model_name} ONNX model: {}", onnx_path.display());

    let builder = Session::builder()
        .map_err(onnx_ort_err)?
        .with_optimization_level(ort::session::builder::GraphOptimizationLevel::All)
        .map_err(|e| anyhow::anyhow!("ORT builder error: {e}"))?
        .with_intra_threads(1)
        .map_err(|e| anyhow::anyhow!("ORT builder error: {e}"))?;

    let mut builder = if let Some(path) = profile_path {
        eprintln!("ORT profiling enabled: {}", path.display());
        builder
            .with_profiling(path)
            .map_err(|e| anyhow::anyhow!("ORT profiling error: {e}"))?
    } else {
        builder
    };

    let mut session = if gpu_id >= 0 {
        if use_tensorrt {
            eprintln!("Using TensorRT (FP16) on GPU {gpu_id}");

            let trt_ep = ort::execution_providers::TensorRTExecutionProvider::default()
                .with_device_id(gpu_id)
                .with_fp16(true)
                .with_engine_cache(tensorrt_cache.is_some());
            let trt_ep = if let Some(cache_path) = tensorrt_cache {
                let cache_str = cache_path.to_str().ok_or_else(|| {
                    anyhow::anyhow!(
                        "TensorRT cache path contains non-UTF-8 characters: {}",
                        cache_path.display()
                    )
                })?;
                eprintln!("TensorRT engine cache: {}", cache_path.display());
                trt_ep.with_engine_cache_path(cache_str)
            } else {
                eprintln!("TensorRT engine cache: disabled (use --onnx-tensorrt-cache to enable)");
                trt_ep
            };

            match trt_ep.is_available() {
                Ok(true) => eprintln!("TensorRT execution provider: available"),
                Ok(false) => {
                    anyhow::bail!(
                        "TensorRTExecutionProvider is NOT available.\n\
                         Ensure TensorRT (libnvinfer.so.10) is in LD_LIBRARY_PATH.\n\
                         To use CUDA EP instead, omit --onnx-tensorrt."
                    );
                }
                Err(e) => {
                    eprintln!("WARNING: Failed to check TensorRT EP availability: {e}");
                }
            }

            // TensorRT EP + CUDA EP をフォールバックとして登録
            let cuda_ep = ort::execution_providers::CUDAExecutionProvider::default()
                .with_device_id(gpu_id)
                .build()
                .error_on_failure();
            let trt_ep = trt_ep.build().error_on_failure();

            builder
                .with_execution_providers([trt_ep, cuda_ep])
                .map_err(|e| anyhow::anyhow!("TensorRT/CUDA EP registration failed: {e}"))?
                .commit_from_file(onnx_path)
                .map_err(onnx_ort_err)?
        } else {
            eprintln!("Using CUDA GPU {gpu_id}");

            let cuda_ep =
                ort::execution_providers::CUDAExecutionProvider::default().with_device_id(gpu_id);
            match cuda_ep.is_available() {
                Ok(true) => eprintln!("CUDA execution provider: available"),
                Ok(false) => {
                    anyhow::bail!(
                        "CUDAExecutionProvider is NOT available in the loaded ONNX Runtime library.\n\
                         The library may be a CPU-only build.\n\
                         Check ORT_DYLIB_PATH points to a GPU-enabled onnxruntime.\n\
                         To use CPU inference instead, omit --onnx-gpu-id."
                    );
                }
                Err(e) => {
                    eprintln!("WARNING: Failed to check CUDA EP availability: {e}");
                }
            }

            let ep = cuda_ep.build().error_on_failure();
            builder
                .with_execution_providers([ep])
                .map_err(|e| {
                    anyhow::anyhow!(
                        "CUDA EP registration failed (is onnxruntime-gpu installed?): {e}"
                    )
                })?
                .commit_from_file(onnx_path)
                .map_err(onnx_ort_err)?
        }
    } else {
        eprintln!("Using CPU");
        builder.commit_from_file(onnx_path).map_err(onnx_ort_err)?
    };

    eprintln!("{model_name} ONNX model loaded. Batch size: {batch_size}");

    progress.set_message("Processing...");

    // ストリーミング読み込み + バッチ処理
    let in_file = File::open(input_path)
        .with_context(|| format!("Failed to open {}", input_path.display()))?;
    let mut reader = BufReader::new(in_file);

    // レジューム対応: 出力ファイルに既存レコードがあればスキップして追記
    let resume_count = if output_path.exists() {
        let out_size = fs::metadata(output_path)?.len();
        let records = out_size / PackedSfenValue::SIZE as u64;
        // 不完全レコードがあれば切り捨て
        let clean_size = records * PackedSfenValue::SIZE as u64;
        if out_size != clean_size {
            let f = File::options().write(true).open(output_path)?;
            f.set_len(clean_size)?;
        }
        records
    } else {
        0
    };

    let out_file = File::options()
        .create(true)
        .append(true)
        .open(output_path)
        .with_context(|| format!("Failed to open {}", output_path.display()))?;
    let mut writer = BufWriter::new(out_file);

    // expand 出力 writer（expand 有効時のみ）。main 側で truncate(0) 済みなので
    // append open で常に先頭から書く。
    let mut expand_writer: Option<BufWriter<File>> = if let Some(e) = expand {
        let f =
            File::options().create(true).append(true).open(e.output_path).with_context(|| {
                format!("Failed to open expand output {}", e.output_path.display())
            })?;
        Some(BufWriter::new(f))
    } else {
        None
    };

    // leaf-REPLACEMENT 出力 writer（replacement 有効時のみ）。expand と同様 main 側で
    // truncate(0) 済みなので append open で常に先頭から書く。leaf-LABEL 出力（writer）と
    // 同一ループで 1:1 lockstep に書くため、レコード数は両者で一致する。
    let mut replacement_writer: Option<BufWriter<File>> = if let Some(rp) = replacement_output {
        let f =
            File::options().create(true).append(true).open(rp).with_context(|| {
                format!("Failed to open leaf-replacement output {}", rp.display())
            })?;
        Some(BufWriter::new(f))
    } else {
        None
    };

    // 既存レコード分の入力をスキップ（expand 無効 + marker 不存在の legacy
    // resume パス。main 側で truncate(0) 済みなら resume_count == 0 になり no-op）
    let mut remaining = process_count;
    if resume_count > 0 {
        let skip = resume_count.min(remaining);
        let mut skip_buf = [0u8; PackedSfenValue::SIZE];
        let mut skipped = 0u64;
        for _ in 0..skip {
            if reader.read_exact(&mut skip_buf).is_err() {
                break;
            }
            remaining -= 1;
            skipped += 1;
        }
        // 既処理分を進捗の起点として反映（per-file/overall とも前進）。
        progress.advance_start(skipped);
        eprintln!("Resuming: skipped {skipped} already-processed records");
    }

    let mut skipped_count: u64 = 0;
    let mut error_count: u64 = 0;
    let mut clipped_count: u64 = 0;
    let mut total_processed: u64 = 0;
    let mut total_expanded: u64 = 0;
    // expand 用 softmax バッファ（バッチ・局面間で再利用）
    let mut logits_buf: Vec<f32> = Vec::with_capacity(600);
    let mut probs_buf: Vec<f32> = Vec::with_capacity(600);

    // MemoryInfo はバッチサイズに依存しないのでループ外で1回だけ作成
    let output_mem =
        MemoryInfo::new(AllocationDevice::CPU, 0, AllocatorType::Device, MemoryType::CPUOutput)
            .map_err(onnx_ort_err)?;

    // フェーズ別 wall time 計測（RESCORE_PHASE_TIMING=1 のとき末尾で出力）。read/build は
    // producer、run/write は consumer の別スレッドで計測するため、オーバーラップ分は
    // 各フェーズの合算が wall を超える。供給コストと GPU コストの絶対値切り分けに使う。
    let phase_timing = std::env::var_os("RESCORE_PHASE_TIMING").is_some();
    let (mut t_run, mut t_write) = (0u128, 0u128);

    // 推論バッチ 1 個分の CPU 成果物（読み込み済み records + 構築済み特徴量 + フラグ）。
    // producer が埋め、consumer（主スレッド）が GPU 推論・書き出しに使う。f1/f2 等は
    // slot プールで再利用し、ピークメモリは入力件数に非依存（PIPELINE_SLOTS 個ぶん）。
    struct PreparedBatch {
        f1: Vec<f32>,
        f2: Vec<f32>,
        records: Vec<(PackedSfenValue, String)>,
        stm_flags: Vec<bool>,
        in_checks: Vec<bool>,
        leaf_sfens: Vec<[u8; 32]>,
        actual_batch: usize,
        errors: u64,
    }

    let want_leaf_sfens = replacement_output.is_some();

    // 供給（read+build）と GPU 推論（session.run）を別スレッドにして直列実行をオーバーラップ。
    // 王手 probe を build へ移したこととあわせ、GPU が CPU 前処理を待つアイドルを潰す。
    // - producer: ストリーム読み込み（直列 I/O）+ rayon 並列特徴量構築
    // - consumer（主スレッド）: ORT セッションで推論し結果を書き出し
    // 決定性: from_bytes/unpack を直列段階に残しバッチ構成を不変に保つため、出力はオーバーラップ
    // 前の直列ループ実装と bit 一致。slot は free/ready の 2 本のチャネルで循環し、同時生存は
    // PIPELINE_SLOTS 個。
    const PIPELINE_SLOTS: usize = 2;
    let (t_read, t_build) = thread::scope(|scope| -> Result<(u128, u128)> {
        let (free_tx, free_rx) = mpsc::channel::<PreparedBatch>();
        let (ready_tx, ready_rx) = mpsc::channel::<PreparedBatch>();
        for _ in 0..PIPELINE_SLOTS {
            free_tx
                .send(PreparedBatch {
                    f1: vec![0.0f32; batch_size * f1_size],
                    f2: vec![0.0f32; batch_size * f2_size],
                    records: Vec::with_capacity(batch_size),
                    stm_flags: vec![false; batch_size],
                    in_checks: vec![false; batch_size],
                    leaf_sfens: if want_leaf_sfens {
                        vec![[0u8; 32]; batch_size]
                    } else {
                        Vec::new()
                    },
                    actual_batch: 0,
                    errors: 0,
                })
                .expect("initial slot send must succeed");
        }

        // ---- producer: ストリーム読み込み + 並列特徴量構築 ----
        let producer = scope.spawn(move || -> Result<(u128, u128)> {
            let mut reader = reader;
            let mut remaining = remaining;
            let mut buffer = [0u8; PackedSfenValue::SIZE];
            let (mut t_read, mut t_build) = (0u128, 0u128);
            loop {
                // 空き slot を取得。consumer が終了して free 側が切れたら producer も終了。
                let mut slot = match free_rx.recv() {
                    Ok(s) => s,
                    Err(_) => return Ok((t_read, t_build)),
                };
                let phase_t = Instant::now();
                // バッチ分のレコードをストリーム読み込み。
                // 注: 旧実装は `--skip-in-check` でこの段階で親をドロップしていたが、
                // expand 機能（ポリシー推論）と独立に動かすため、推論は常に実行し、
                // 王手フラグだけを記録して rescore 書き出し / expand 書き出しの個別判定に使う。
                slot.records.clear();
                let mut errs: u64 = 0;
                while slot.records.len() < batch_size && remaining > 0 {
                    if INTERRUPTED.load(Ordering::SeqCst) {
                        remaining = 0;
                        break;
                    }
                    remaining -= 1;
                    match reader.read_exact(&mut buffer) {
                        Ok(()) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                            remaining = 0;
                            break;
                        }
                        Err(e) => return Err(e.into()),
                    }
                    let psv = match PackedSfenValue::from_bytes(&buffer) {
                        Some(p) => p,
                        None => {
                            errs += 1;
                            continue;
                        }
                    };
                    let sfen = match unpack_sfen(&psv.sfen) {
                        Ok(s) => s,
                        Err(_) => {
                            errs += 1;
                            continue;
                        }
                    };
                    slot.records.push((psv, sfen));
                }
                let actual_batch = slot.records.len();
                if phase_timing {
                    t_read += phase_t.elapsed().as_nanos();
                }
                if actual_batch == 0 {
                    // EOF / 中断: actual_batch=0 を sentinel として送り producer 終了。
                    slot.actual_batch = 0;
                    slot.errors = errs;
                    let _ = ready_tx.send(slot);
                    return Ok((t_read, t_build));
                }

                let phase_t = Instant::now();
                // rayon 並列で特徴量構築
                let batch_errors = AtomicU64::new(0);
                {
                    let PreparedBatch {
                        f1,
                        f2,
                        records,
                        stm_flags,
                        in_checks,
                        leaf_sfens,
                        ..
                    } = &mut slot;
                    f1[..actual_batch * f1_size].fill(0.0);
                    f2[..actual_batch * f2_size].fill(0.0);
                    let f1_slices: Vec<&mut [f32]> =
                        f1[..actual_batch * f1_size].chunks_mut(f1_size).collect();
                    let f2_slices: Vec<&mut [f32]> =
                        f2[..actual_batch * f2_size].chunks_mut(f2_size).collect();

                    // 葉で STM が反転したかの記録領域を当バッチ分だけ reset（非モード時は全 false）。
                    let stm_flags = &mut stm_flags[..actual_batch];
                    stm_flags.fill(false);
                    // root 王手フラグの記録領域も当バッチ分だけ reset（set_sfen 失敗時は false 据え置き）。
                    let in_checks = &mut in_checks[..actual_batch];
                    in_checks.fill(false);
                    // leaf-REPLACEMENT 有効時のみ葉局面の packed sfen を書き込む領域を当バッチ分に絞る。
                    // 非有効時は空スライス（pack を行わないため zip しない）。
                    let leaf_sfens_slice: &mut [[u8; 32]] = if want_leaf_sfens {
                        &mut leaf_sfens[..actual_batch]
                    } else {
                        &mut []
                    };

                    // 1 局面分の特徴量構築（必要なら葉まで進めて葉 sfen を packed_leaf に書く）。
                    let build_one =
                        |f1: &mut [f32],
                         f2: &mut [f32],
                         psv: &PackedSfenValue,
                         sfen: &str,
                         stm_flag: &mut bool,
                         in_check_slot: &mut bool,
                         packed_leaf: Option<&mut [u8; 32]>| {
                            let mut pos = Position::new();
                            if pos.set_sfen(sfen).is_err() {
                                batch_errors.fetch_add(1, Ordering::Relaxed);
                                return;
                            }
                            // 王手フラグは直列 read の probe ではなくここで求める（同じ set_sfen 後の
                            // root 局面から 1 回だけ）。書き出し時の skip_in_check / expand 判定に使う。
                            let root_in_check = pos.in_check();
                            *in_check_slot = root_in_check;
                            // root 局面据え置き・ラベルのみ葉評価: NNUE qsearch で葉まで進めてから
                            // 特徴量を構築する（DL は葉局面を評価）。王手 root は葉探索せず原局面のまま。
                            if qsearch_leaf_label && !root_in_check {
                                thread_local! {
                                    static NNUE_STACKS: RefCell<NnueStacks> =
                                        RefCell::new(NnueStacks::new());
                                }
                                NNUE_STACKS.with(|stacks| {
                                    let mut stacks = stacks.borrow_mut();
                                    stacks.reset();
                                    let result = qsearch_with_pv_nnue(
                                        &mut pos,
                                        &mut stacks,
                                        QSEARCH_ALPHA_INIT,
                                        QSEARCH_BETA_INIT,
                                        0,
                                        qsearch_max_ply,
                                    );
                                    *stm_flag = apply_pv(&mut pos, &result.pv);
                                });
                            }
                            // 葉まで進めた pos を pack（王手 root は原局面のまま）。replacement arm 用。
                            if let Some(slot) = packed_leaf {
                                *slot = pack_position(&pos);
                            }
                            build_features(&pos, f1, f2, psv);
                        };

                    if want_leaf_sfens {
                        f1_slices
                            .into_par_iter()
                            .zip(f2_slices)
                            .zip(records.par_iter())
                            .zip(stm_flags.par_iter_mut())
                            .zip(in_checks.par_iter_mut())
                            .zip(leaf_sfens_slice.par_iter_mut())
                            .for_each(|(((((f1, f2), (psv, sfen)), stm_flag), in_check), leaf)| {
                                build_one(f1, f2, psv, sfen, stm_flag, in_check, Some(leaf));
                            });
                    } else {
                        f1_slices
                            .into_par_iter()
                            .zip(f2_slices)
                            .zip(records.par_iter())
                            .zip(stm_flags.par_iter_mut())
                            .zip(in_checks.par_iter_mut())
                            .for_each(|((((f1, f2), (psv, sfen)), stm_flag), in_check)| {
                                build_one(f1, f2, psv, sfen, stm_flag, in_check, None);
                            });
                    }
                }

                slot.actual_batch = actual_batch;
                slot.errors = errs + batch_errors.load(Ordering::Relaxed);
                if phase_timing {
                    t_build += phase_t.elapsed().as_nanos();
                }
                // ready へ送る。consumer 側が終了して受け口が切れたら producer も終了。
                if ready_tx.send(slot).is_err() {
                    return Ok((t_read, t_build));
                }
            }
        });

        // ---- consumer（主スレッド）: GPU 推論 + 書き出し ----
        while let Ok(batch) = ready_rx.recv() {
            if batch.actual_batch == 0 {
                // producer からの EOF / 中断 sentinel。
                error_count += batch.errors;
                break;
            }
            let actual_batch = batch.actual_batch;
            error_count += batch.errors;
            let mut phase_t = Instant::now();
            {
                // IoBinding で推論（Python の run_with_iobinding に対応）
                // session.run() より ORT 内部のメモリ管理が効率的
                //
                // 最適化検証で得られた知見:
                // - create_binding() のループ外化（再利用）は逆効果（4.6〜36% 悪化）。
                //   rebind 時に ORT 内部で前回バインドのクリーンアップコストが発生するため、
                //   毎回新規作成の方が速い。
                // - output_policy のバインド省略も逆効果（10% 悪化）。
                //   ORT が未バインド出力の処理にオーバーヘッドを生じる。
                // - ボトルネックは cudaMemcpyAsync（CPU→GPU 転送）で全体の 96.1%（nsys 計測）。
                //   転送量削減（FP16 化等）以外での大幅改善は困難。
                let shape1: [usize; 4] = [actual_batch, input1_channels, 9, 9];
                let input1 = TensorRef::<f32>::from_array_view((
                    shape1,
                    &batch.f1[..actual_batch * f1_size],
                ))
                .map_err(onnx_ort_err)?;

                let shape2: [usize; 4] = [actual_batch, input2_channels, 9, 9];
                let input2 = TensorRef::<f32>::from_array_view((
                    shape2,
                    &batch.f2[..actual_batch * f2_size],
                ))
                .map_err(onnx_ort_err)?;

                let mut binding = session.create_binding().map_err(onnx_ort_err)?;
                binding.bind_input("input1", &input1).map_err(onnx_ort_err)?;
                binding.bind_input("input2", &input2).map_err(onnx_ort_err)?;
                // output_policy: スコアリングには不使用だが、省略すると ORT 内部処理で
                // オーバーヘッドが増加するため全出力をバインドする
                binding
                    .bind_output_to_device("output_policy", &output_mem)
                    .map_err(onnx_ort_err)?;
                binding
                    .bind_output_to_device("output_value", &output_mem)
                    .map_err(onnx_ort_err)?;

                let outputs = session.run_binding(&binding).map_err(onnx_ort_err)?;

                let (_, values) =
                    outputs["output_value"].try_extract_tensor::<f32>().map_err(onnx_ort_err)?;
                if phase_timing {
                    t_run += phase_t.elapsed().as_nanos();
                    phase_t = Instant::now();
                }

                // rescore 書き出し（テンソルから直接読み取り、to_vec() コピーを排除）
                // skip_in_check が真かつ親が王手の場合は書き出しを抑制（推論結果は破棄）。
                for (i, (psv, _sfen)) in batch.records.iter().enumerate() {
                    if skip_in_check && batch.in_checks[i] {
                        skipped_count += 1;
                        continue;
                    }
                    let winrate = values[i];
                    let clamped = winrate.clamp(0.001, 0.999);
                    let logit = (clamped / (1.0 - clamped)).ln();
                    // qsearch-leaf-label モードで葉の STM が root と異なる場合、推論値は葉の
                    // 手番視点なので root 視点へ符号反転する。
                    let leaf_score = logit * eval_scale;
                    let signed_score = if batch.stm_flags[i] {
                        -leaf_score
                    } else {
                        leaf_score
                    };
                    let raw_score = signed_score as i32;
                    let clipped = raw_score.abs() > score_clip as i32;
                    let new_score = raw_score.clamp(-(score_clip as i32), score_clip as i32) as i16;
                    if clipped {
                        clipped_count += 1;
                    }
                    // leaf-LABEL arm: 出力 sfen は常に root の `psv.sfen`（局面は置換しない）。
                    // 葉評価はラベルのみに反映（符号反転は signed_score に適用済み）。
                    let new_psv = PackedSfenValue {
                        sfen: psv.sfen,
                        score: new_score,
                        move16: 0,
                        game_ply: psv.game_ply,
                        game_result: psv.game_result,
                        padding: 0,
                    };
                    writer.write_all(&new_psv.to_bytes())?;

                    // leaf-REPLACEMENT arm（有効時のみ、leaf-LABEL と 1:1 lockstep）。
                    // `--apply-qsearch-leaf` → DL rescore の 2 工程と bit 一致させる:
                    // - sfen は葉局面の packed sfen
                    // - score は葉評価（符号反転なし＝葉手番視点）に clip 適用
                    // - game_result は STM 反転時のみ符号反転
                    if let Some(rw) = replacement_writer.as_mut() {
                        let leaf_raw = leaf_score as i32;
                        let leaf_clipped_score =
                            leaf_raw.clamp(-(score_clip as i32), score_clip as i32) as i16;
                        let leaf_game_result = if batch.stm_flags[i] {
                            -psv.game_result
                        } else {
                            psv.game_result
                        };
                        let replacement_psv = PackedSfenValue {
                            sfen: batch.leaf_sfens[i],
                            score: leaf_clipped_score,
                            move16: 0,
                            game_ply: psv.game_ply,
                            game_result: leaf_game_result,
                            padding: 0,
                        };
                        rw.write_all(&replacement_psv.to_bytes())?;
                    }
                }

                // expand 機能（policy ベースの子局面生成）
                if let (Some(expand_cfg), Some(ew)) = (expand, expand_writer.as_mut()) {
                    let (policy_shape, policy_data) = outputs["output_policy"]
                        .try_extract_tensor::<f32>()
                        .map_err(onnx_ort_err)?;
                    let expected_len = actual_batch * MAX_MOVE_LABEL_NUM;
                    if policy_data.len() != expected_len {
                        anyhow::bail!(
                            "Policy output shape mismatch: expected [{actual_batch}, \
                             {MAX_MOVE_LABEL_NUM}] ({expected_len} elements), got shape {:?} \
                             ({} elements). Is the ONNX model a compatible policy network?",
                            policy_shape,
                            policy_data.len()
                        );
                    }

                    for (i, (psv, sfen)) in batch.records.iter().enumerate() {
                        if expand_cfg.skip_parent_in_check && batch.in_checks[i] {
                            continue;
                        }

                        let mut pos = Position::new();
                        if pos.set_sfen(sfen).is_err() {
                            continue;
                        }
                        let color = pos.side_to_move();

                        let mut list = MoveList::new();
                        generate_legal(&pos, &mut list);
                        if list.is_empty() {
                            continue;
                        }

                        let policy_row =
                            &policy_data[i * MAX_MOVE_LABEL_NUM..(i + 1) * MAX_MOVE_LABEL_NUM];

                        logits_buf.clear();
                        for mv in list.iter() {
                            let label = make_move_label(*mv, color);
                            logits_buf.push(policy_row[label]);
                        }
                        probs_buf.resize(logits_buf.len(), 0.0);
                        softmax_normalize(&logits_buf, &mut probs_buf);

                        for (j, mv) in list.iter().enumerate() {
                            if probs_buf[j] > expand_cfg.threshold {
                                let gives_check = pos.gives_check(*mv);
                                pos.do_move(*mv, gives_check);

                                let child_in_check = pos.in_check();
                                if !(expand_cfg.skip_child_in_check && child_in_check) {
                                    let packed = pack_position(&pos);
                                    let child = PackedSfenValue {
                                        sfen: packed,
                                        score: 0,
                                        move16: 0,
                                        game_ply: psv.game_ply.saturating_add(1),
                                        game_result: 0,
                                        padding: 0,
                                    };
                                    ew.write_all(&child.to_bytes())?;
                                    total_expanded += 1;
                                }

                                pos.undo_move(*mv);
                            }
                        }
                    }
                }

                if phase_timing {
                    t_write += phase_t.elapsed().as_nanos();
                }
            }

            total_processed += actual_batch as u64;
            progress.inc(actual_batch as u64);
            // slot を再利用に戻す。producer が既に終了していて受け口が切れていても無視。
            let _ = free_tx.send(batch);
        }

        // consumer 終了。free_tx を drop して producer の free_rx.recv() を解除し join。
        // producer の read エラーはここで `?` 相当で伝播する（join 結果が Err）。
        // consumer が途中の `?` で早期 return した場合も、scope クロージャの unwind で
        // free_tx が drop され producer の recv が解除されるためデッドロックしない。
        drop(free_tx);
        producer.join().expect("producer thread panicked")
    })?;

    if phase_timing {
        let ms = |ns: u128| ns as f64 / 1.0e6;
        let total = (t_read + t_build + t_run + t_write).max(1) as f64;
        let pct = |ns: u128| ns as f64 / total * 100.0;
        eprintln!(
            "[phase timing] read={:.0}ms ({:.1}%)  build={:.0}ms ({:.1}%)  \
             run={:.0}ms ({:.1}%)  write={:.0}ms ({:.1}%)",
            ms(t_read),
            pct(t_read),
            ms(t_build),
            pct(t_build),
            ms(t_run),
            pct(t_run),
            ms(t_write),
            pct(t_write)
        );
    }

    if profile_path.is_some() {
        match session.end_profiling() {
            Ok(path) => eprintln!("ORT profile saved: {path}"),
            Err(e) => eprintln!("ORT profile error: {e}"),
        }
    }

    // 出力ファイル本体を flush + sync して、マーカー書き出し前にクラッシュしても
    // 書き出し済みバイト列が確実にディスクに着地している状態を作る。
    // sync_all() は内部の File に対して実行（BufWriter::flush + into_inner で取り出し）。
    writer.flush()?;
    let rescore_inner = writer
        .into_inner()
        .map_err(|e| anyhow::anyhow!("rescore writer into_inner error: {}", e.error()))?;
    rescore_inner.sync_all()?;
    drop(rescore_inner);

    if let Some(mut ew) = expand_writer.take() {
        ew.flush()?;
        let inner = ew
            .into_inner()
            .map_err(|e| anyhow::anyhow!("expand writer into_inner error: {}", e.error()))?;
        inner.sync_all()?;
    }

    if let Some(mut rw) = replacement_writer.take() {
        rw.flush()?;
        let inner = rw
            .into_inner()
            .map_err(|e| anyhow::anyhow!("replacement writer into_inner error: {}", e.error()))?;
        inner.sync_all()?;
    }

    progress.finish_with_message("Done");

    // 統計情報
    let rescore_written = total_processed.saturating_sub(skipped_count);
    let total = total_processed + error_count;
    if error_count > 0 {
        // 破損/パース不能でスキップしたレコードは推論バッチに入らず進捗に計上されないため、
        // それらがあると進捗表示が 100% に届かないことがある（rescore 出力は有効分のみで正常）。
        eprintln!(
            "Note: {error_count} 件のレコードでエラー（破損 / パース不能）。スキップ分は \
             進捗に計上されないため、進捗表示が 100% に届かないことがある（rescore 出力は正常）。"
        );
    }
    if skipped_count > 0 && total > 0 {
        eprintln!(
            "Skipped (in check): {skipped_count} ({:.2}%)",
            skipped_count as f64 / total as f64 * 100.0
        );
    }
    if rescore_written > 0 {
        eprintln!(
            "Clipped scores: {clipped_count} ({:.2}%)",
            clipped_count as f64 / rescore_written as f64 * 100.0
        );
    }
    eprintln!("Rescored: {rescore_written} positions");
    if let Some(e) = expand {
        let avg = if rescore_written > 0 {
            total_expanded as f64 / rescore_written as f64
        } else {
            0.0
        };
        eprintln!(
            "Expanded: {total_expanded} positions (threshold={:.2}%, avg {:.2} per parent)",
            e.threshold * 100.0,
            avg
        );
    }

    // 完了マーカー書き出し（atomic rename）。
    // INTERRUPTED が立っている場合（Ctrl-C で残ファイル分を打ち切った場合）も
    // 書き出した分は完了扱いとはせず、marker を作らない。
    if !INTERRUPTED.load(Ordering::SeqCst) {
        let fingerprint = build_run_fingerprint(config)?;
        let rescore_size = fs::metadata(output_path)?.len();
        let expand_size = match expand {
            Some(e) => Some(fs::metadata(e.output_path)?.len()),
            None => None,
        };
        let replacement_size = match replacement_output {
            Some(rp) => Some(fs::metadata(rp)?.len()),
            None => None,
        };
        let marker = DoneMarker {
            fingerprint,
            output_sizes: OutputSizes {
                rescore_output_size: rescore_size,
                expand_output_size: expand_size,
                replacement_output_size: replacement_size,
            },
        };
        write_marker_atomic(output_path, &marker)?;
        eprintln!("Marker written: {}", marker_path_for(output_path).display());
    }

    Ok(())
}

// ============================================================
// AobaZero ONNX 直接推論モード
// ============================================================

#[cfg(feature = "aobazero-onnx")]
fn process_file_with_onnx(
    cli: &Cli,
    input_path: &std::path::Path,
    output_path: &std::path::Path,
    expand_output_path: Option<&std::path::Path>,
    process_count: u64,
    progress: &FileProgress,
) -> Result<()> {
    use tools::aobazero_features::{
        FEATURES1_SIZE, FEATURES2_SIZE, INPUT1_CHANNELS, INPUT2_CHANNELS, make_input_features,
    };

    let draw_ply = cli.onnx_draw_ply;
    let expand = expand_output_path.map(|p| ExpandConfig {
        output_path: p,
        threshold: cli.expand_threshold / 100.0,
        skip_parent_in_check: cli.expand_skip_parent_in_check,
        skip_child_in_check: cli.expand_skip_child_in_check,
    });
    let config = OnnxPipelineConfig {
        model_name: "AobaZero",
        model_kind: "aobazero",
        onnx_path: cli.onnx_model.as_ref().unwrap(),
        input_path,
        output_path,
        process_count,
        batch_size: cli.onnx_batch_size,
        gpu_id: cli.onnx_gpu_id,
        use_tensorrt: cli.onnx_tensorrt,
        tensorrt_cache: cli.onnx_tensorrt_cache.as_deref(),
        score_clip: cli.score_clip,
        eval_scale: cli.onnx_eval_scale,
        skip_in_check: cli.skip_in_check,
        onnx_draw_ply: cli.onnx_draw_ply,
        f1_size: FEATURES1_SIZE,
        f2_size: FEATURES2_SIZE,
        input1_channels: INPUT1_CHANNELS,
        input2_channels: INPUT2_CHANNELS,
        profile_path: cli.ort_profile.as_deref(),
        expand,
        qsearch_leaf_label: cli.qsearch_leaf_label,
        qsearch_max_ply: cli.max_ply,
        // leaf-label は dlshogi 限定のため AobaZero では葉探索 NNUE / replacement arm を持たない。
        qsearch_nnue_path: None,
        replacement_output: None,
    };
    process_file_with_onnx_pipeline(
        &config,
        move |pos, f1, f2, psv| {
            make_input_features(pos, f1, f2, psv.game_ply as i32, draw_ply);
        },
        progress,
    )
}

// ============================================================
// 標準 dlshogi ONNX 直接推論モード
// ============================================================

#[cfg(feature = "dlshogi-onnx")]
fn process_file_with_dlshogi_onnx(
    cli: &Cli,
    input_path: &std::path::Path,
    output_path: &std::path::Path,
    expand_output_path: Option<&std::path::Path>,
    replacement_output_path: Option<&std::path::Path>,
    process_count: u64,
    progress: &FileProgress,
) -> Result<()> {
    use tools::dlshogi_features::{
        FEATURES1_SIZE, FEATURES2_SIZE, INPUT1_CHANNELS, INPUT2_CHANNELS, make_input_features,
    };

    let expand = expand_output_path.map(|p| ExpandConfig {
        output_path: p,
        threshold: cli.expand_threshold / 100.0,
        skip_parent_in_check: cli.expand_skip_parent_in_check,
        skip_child_in_check: cli.expand_skip_child_in_check,
    });
    let config = OnnxPipelineConfig {
        model_name: "dlshogi",
        model_kind: "dlshogi",
        onnx_path: cli.dlshogi_onnx_model.as_ref().unwrap(),
        input_path,
        output_path,
        process_count,
        batch_size: cli.onnx_batch_size,
        gpu_id: cli.onnx_gpu_id,
        use_tensorrt: cli.onnx_tensorrt,
        tensorrt_cache: cli.onnx_tensorrt_cache.as_deref(),
        score_clip: cli.score_clip,
        eval_scale: cli.onnx_eval_scale,
        skip_in_check: cli.skip_in_check,
        // dlshogi モデルでは `onnx_draw_ply` を使わない。fingerprint の過剰
        // invalidation を避けるため 0 固定（`onnx_marker_decide` 側でも同じ扱い）。
        onnx_draw_ply: 0,
        f1_size: FEATURES1_SIZE,
        f2_size: FEATURES2_SIZE,
        input1_channels: INPUT1_CHANNELS,
        input2_channels: INPUT2_CHANNELS,
        profile_path: cli.ort_profile.as_deref(),
        expand,
        qsearch_leaf_label: cli.qsearch_leaf_label,
        qsearch_max_ply: cli.max_ply,
        qsearch_nnue_path: cli.nnue.as_deref(),
        replacement_output: replacement_output_path,
    };
    process_file_with_onnx_pipeline(
        &config,
        |pos, f1, f2, _psv| {
            make_input_features(pos, f1, f2);
        },
        progress,
    )
}

#[cfg(all(test, any(feature = "aobazero-onnx", feature = "dlshogi-onnx")))]
mod marker_tests {
    use super::*;
    use std::io::Write;

    /// 必須キーを満たす最小 marker テキスト（expand=false）。`extra` に qsearch 系の追加行を差し込む。
    fn base_marker(extra: &str) -> String {
        format!(
            "version={MARKER_VERSION}\n\
             mode=onnx\n\
             model_kind=dlshogi\n\
             model_path=/tmp/model.onnx\n\
             model_size=100\n\
             model_mtime_ns=123\n\
             input_path=/tmp/in.psv\n\
             input_size=200\n\
             input_mtime_ns=456\n\
             process_count=10\n\
             skip_in_check=false\n\
             score_clip=10000\n\
             eval_scale_bits=0x44160000\n\
             onnx_draw_ply=0\n\
             {extra}\
             expand=false\n\
             rescore_output_size=400\n"
        )
    }

    fn parse_text(text: &str) -> DoneMarker {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.done");
        std::fs::File::create(&path).unwrap().write_all(text.as_bytes()).unwrap();
        parse_marker(&path).unwrap()
    }

    /// leaf-label=true の marker に必要な qsearch 系キー（max_ply + 葉探索 NNUE のメタ）。
    const LEAF_LABEL_KEYS: &str = "qsearch_leaf_label=true\n\
         qsearch_max_ply=20\n\
         qsearch_nnue_path=/tmp/nn.bin\n\
         qsearch_nnue_size=42\n\
         qsearch_nnue_mtime_ns=789\n";

    #[test]
    fn old_marker_without_qsearch_keys_defaults_to_false() {
        // 旧 marker（qsearch_leaf_label 行なし）は false / None 扱いで後方互換
        let m = parse_text(&base_marker(""));
        assert!(!m.fingerprint.qsearch_leaf_label);
        assert_eq!(m.fingerprint.qsearch_max_ply, None);
        assert_eq!(m.fingerprint.qsearch_nnue_path, None);
        assert_eq!(m.fingerprint.qsearch_nnue_size, None);
        assert_eq!(m.fingerprint.qsearch_nnue_mtime_ns, None);
    }

    #[test]
    fn new_marker_leaf_label_true_roundtrips() {
        let m = parse_text(&base_marker(LEAF_LABEL_KEYS));
        assert!(m.fingerprint.qsearch_leaf_label);
        assert_eq!(m.fingerprint.qsearch_max_ply, Some(20));
        // 葉探索 NNUE のメタも fingerprint に取り込まれる（差し替え検知用）。
        assert_eq!(m.fingerprint.qsearch_nnue_path, Some(PathBuf::from("/tmp/nn.bin")));
        assert_eq!(m.fingerprint.qsearch_nnue_size, Some(42));
        assert_eq!(m.fingerprint.qsearch_nnue_mtime_ns, Some(789));
        // serialize → parse で一致（max_ply / nnue 行は leaf_label=true 時のみ出力）
        assert_eq!(m, parse_text(&serialize_marker(&m)));
    }

    #[test]
    fn leaf_label_false_has_no_max_ply() {
        let m = parse_text(&base_marker("qsearch_leaf_label=false\n"));
        assert!(!m.fingerprint.qsearch_leaf_label);
        assert_eq!(m.fingerprint.qsearch_max_ply, None);
        assert_eq!(m.fingerprint.qsearch_nnue_path, None);
    }

    #[test]
    fn old_leaf_label_marker_without_nnue_keys_parses_as_none() {
        // 葉探索 NNUE キーを持たない旧 leaf-label marker は parse error にせず None として読む。
        // 現 fingerprint は Some(nnue) のため不一致になり、marker 不一致経路で再生成される。
        let m = parse_text(&base_marker("qsearch_leaf_label=true\nqsearch_max_ply=20\n"));
        assert!(m.fingerprint.qsearch_leaf_label);
        assert_eq!(m.fingerprint.qsearch_max_ply, Some(20));
        assert_eq!(m.fingerprint.qsearch_nnue_path, None);
        assert_eq!(m.fingerprint.qsearch_nnue_size, None);
        assert_eq!(m.fingerprint.qsearch_nnue_mtime_ns, None);
    }

    #[test]
    fn old_marker_without_replacement_key_defaults_to_false() {
        // 旧 marker（replacement 行なし）は false / None 扱いで後方互換
        let m = parse_text(&base_marker(""));
        assert!(!m.fingerprint.replacement);
        assert_eq!(m.fingerprint.replacement_output_path, None);
        assert_eq!(m.output_sizes.replacement_output_size, None);
    }

    #[test]
    fn replacement_false_has_no_path_or_size() {
        let m = parse_text(&base_marker("replacement=false\n"));
        assert!(!m.fingerprint.replacement);
        assert_eq!(m.fingerprint.replacement_output_path, None);
        assert_eq!(m.output_sizes.replacement_output_size, None);
    }

    #[test]
    fn replacement_true_roundtrips() {
        let m = parse_text(&base_marker(
            "qsearch_leaf_label=true\nqsearch_max_ply=16\n\
             qsearch_nnue_path=/tmp/nn.bin\n\
             qsearch_nnue_size=42\n\
             qsearch_nnue_mtime_ns=789\n\
             replacement=true\n\
             replacement_output_path=/tmp/repl/in.psv\n\
             replacement_output_size=320\n",
        ));
        assert!(m.fingerprint.replacement);
        assert_eq!(m.fingerprint.replacement_output_path, Some(PathBuf::from("/tmp/repl/in.psv")));
        assert_eq!(m.output_sizes.replacement_output_size, Some(320));
        // serialize → parse で一致（replacement_* 行は replacement=true 時のみ出力）
        assert_eq!(m, parse_text(&serialize_marker(&m)));
    }
}
