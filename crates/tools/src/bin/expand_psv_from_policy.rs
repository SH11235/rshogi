//! expand_psv_from_policy - ポリシーネットワークで PSV を展開
//!
//! PSV ファイルの各局面に対して dlshogi ONNX モデルのポリシー推論を行い、
//! 合法手の選択確率が閾値を超える手について次局面を生成し、新しい PSV ファイルに書き出す。
//!
//! cshogi_util の expand_psv_from_policy.py 相当。
//!
//! # 使用例
//!
//! ```bash
//! ORT_DYLIB_PATH=/path/to/libonnxruntime.so \
//! cargo run --release -p tools --features dlshogi-onnx --bin expand_psv_from_policy -- \
//!   --input data.psv \
//!   --output expanded.psv \
//!   --onnx-model model.onnx \
//!   --threshold 10.0
//! ```

use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;

/// ポリシーネットワークで PSV を展開
///
/// 各局面の合法手についてポリシー確率を計算し、
/// 閾値を超える手の次局面を新しい PSV として書き出す。
#[derive(Parser)]
#[command(
    name = "expand_psv_from_policy",
    version,
    about = "ポリシーネットワークで PSV を展開\n\n\
             各局面の合法手のうちポリシー確率が閾値を超えるものについて、\n\
             着手後の局面を新規 PSV レコードとして書き出す。"
)]
struct Cli {
    /// 入力 PSV ファイル
    #[arg(short, long)]
    input: PathBuf,

    /// 出力 PSV ファイル
    #[arg(short, long)]
    output: PathBuf,

    /// dlshogi ONNX モデルファイル
    #[arg(long)]
    onnx_model: PathBuf,

    /// ONNX 推論バッチサイズ
    #[arg(long, default_value_t = 1024)]
    batch_size: usize,

    /// GPU デバイス ID（-1 で CPU）
    #[arg(long, default_value_t = 0)]
    gpu_id: i32,

    /// TensorRT を使用する
    #[arg(long)]
    tensorrt: bool,

    /// TensorRT エンジンキャッシュディレクトリ
    #[arg(long)]
    tensorrt_cache: Option<PathBuf>,

    /// 選択確率の閾値（%）。この値を超える手の次局面を出力する
    #[arg(long, default_value_t = 10.0)]
    threshold: f32,
}

#[cfg(feature = "dlshogi-onnx")]
fn run(cli: &Cli) -> Result<()> {
    use anyhow::Context;
    use indicatif::{ProgressBar, ProgressStyle};
    use ort::ep::ExecutionProvider;
    use ort::memory::{AllocationDevice, AllocatorType, MemoryInfo, MemoryType};
    use ort::session::Session;
    use ort::value::TensorRef;
    use std::fs::{self, File};
    use std::io::{BufWriter, Read, Write};
    use std::sync::atomic::{AtomicBool, Ordering};

    use rshogi_core::movegen::{MoveList, generate_legal};
    use rshogi_core::position::Position;
    use tools::dlshogi_features::{
        FEATURES1_SIZE, FEATURES2_SIZE, INPUT1_CHANNELS, INPUT2_CHANNELS, MAX_MOVE_LABEL_NUM,
        make_input_features, make_move_label,
    };
    use tools::packed_sfen::{PackedSfenValue, pack_position, unpack_sfen};

    /// ort のエラーを anyhow に変換
    fn ort_err(e: ort::Error) -> anyhow::Error {
        anyhow::anyhow!("{e}")
    }

    /// softmax を計算し正規化する（オーバーフロー防止のため最大値を引く）
    fn softmax_normalize(logits: &[f32], out: &mut [f32]) {
        debug_assert_eq!(logits.len(), out.len());
        let max = logits.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        let mut sum = 0.0f32;
        for (o, &l) in out.iter_mut().zip(logits.iter()) {
            *o = (l - max).exp();
            sum += *o;
        }
        let inv = 1.0 / sum;
        for o in out.iter_mut() {
            *o *= inv;
        }
    }

    static INTERRUPTED: AtomicBool = AtomicBool::new(false);

    // ORT_DYLIB_PATH 検証
    match std::env::var("ORT_DYLIB_PATH") {
        Ok(path) if !path.is_empty() => {
            if !std::path::Path::new(&path).is_file() {
                anyhow::bail!(
                    "ORT_DYLIB_PATH is set to '{path}' but file does not exist.\n\
                     Download from: https://github.com/microsoft/onnxruntime/releases"
                );
            }
            eprintln!("ORT_DYLIB_PATH: {path}");
        }
        _ => {
            anyhow::bail!(
                "ORT_DYLIB_PATH environment variable is not set.\n\
                 Download from: https://github.com/microsoft/onnxruntime/releases\n\
                 Example:\n  ORT_DYLIB_PATH=/path/to/libonnxruntime.so cargo run ..."
            );
        }
    }

    // ONNX セッション初期化
    eprintln!("Loading dlshogi ONNX model: {}", cli.onnx_model.display());

    let mut builder = Session::builder()
        .map_err(ort_err)?
        .with_optimization_level(ort::session::builder::GraphOptimizationLevel::All)
        .map_err(|e| anyhow::anyhow!("ORT builder error: {e}"))?
        .with_intra_threads(1)
        .map_err(|e| anyhow::anyhow!("ORT builder error: {e}"))?;

    let mut session = if cli.gpu_id >= 0 {
        if cli.tensorrt {
            eprintln!("Using TensorRT (FP16) on GPU {}", cli.gpu_id);

            let trt_ep = ort::execution_providers::TensorRTExecutionProvider::default()
                .with_device_id(cli.gpu_id)
                .with_fp16(true)
                .with_engine_cache(cli.tensorrt_cache.is_some());
            let trt_ep = if let Some(cache_path) = &cli.tensorrt_cache {
                let cache_str = cache_path.to_str().ok_or_else(|| {
                    anyhow::anyhow!("TensorRT cache path contains non-UTF-8 characters")
                })?;
                eprintln!("TensorRT engine cache: {}", cache_path.display());
                trt_ep.with_engine_cache_path(cache_str)
            } else {
                trt_ep
            };

            let cuda_ep = ort::execution_providers::CUDAExecutionProvider::default()
                .with_device_id(cli.gpu_id)
                .build()
                .error_on_failure();
            let trt_ep = trt_ep.build().error_on_failure();

            builder
                .with_execution_providers([trt_ep, cuda_ep])
                .map_err(|e| anyhow::anyhow!("TensorRT/CUDA EP failed: {e}"))?
                .commit_from_file(&cli.onnx_model)
                .map_err(ort_err)?
        } else {
            eprintln!("Using CUDA GPU {}", cli.gpu_id);
            let cuda_ep = ort::execution_providers::CUDAExecutionProvider::default()
                .with_device_id(cli.gpu_id);
            match cuda_ep.is_available() {
                Ok(true) => eprintln!("CUDA execution provider: available"),
                Ok(false) => {
                    anyhow::bail!(
                        "CUDAExecutionProvider not available.\n\
                         Check ORT_DYLIB_PATH points to a GPU-enabled onnxruntime."
                    );
                }
                Err(e) => eprintln!("WARNING: CUDA EP check: {e}"),
            }
            let ep = cuda_ep.build().error_on_failure();
            builder
                .with_execution_providers([ep])
                .map_err(|e| anyhow::anyhow!("CUDA EP failed: {e}"))?
                .commit_from_file(&cli.onnx_model)
                .map_err(ort_err)?
        }
    } else {
        eprintln!("Using CPU");
        builder.commit_from_file(&cli.onnx_model).map_err(ort_err)?
    };

    let batch_size = cli.batch_size;
    let threshold = cli.threshold / 100.0; // % → 割合

    eprintln!("Model loaded. Batch size: {batch_size}, threshold: {:.1}%", cli.threshold);

    // 入力ファイルサイズからレコード数を計算
    let input_size = fs::metadata(&cli.input)
        .with_context(|| format!("Cannot stat {}", cli.input.display()))?
        .len();
    let total_records = input_size / PackedSfenValue::SIZE as u64;
    eprintln!("Input: {} ({total_records} records)", cli.input.display());

    // 進捗バー
    let progress = ProgressBar::new(total_records);
    progress.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} ({per_sec}) {msg}")
            .expect("valid template"),
    );

    // Ctrl+C ハンドラ
    ctrlc::set_handler(move || {
        INTERRUPTED.store(true, Ordering::SeqCst);
        eprintln!("\nInterrupted. Finishing current batch...");
    })
    .ok();

    let in_file = File::open(&cli.input)
        .with_context(|| format!("Failed to open {}", cli.input.display()))?;
    let mut reader = std::io::BufReader::new(in_file);

    let out_file = File::create(&cli.output)
        .with_context(|| format!("Failed to create {}", cli.output.display()))?;
    let mut writer = BufWriter::new(out_file);

    // 特徴量バッファ
    let mut f1_buf = vec![0.0f32; batch_size * FEATURES1_SIZE];
    let mut f2_buf = vec![0.0f32; batch_size * FEATURES2_SIZE];
    let mut buffer = [0u8; PackedSfenValue::SIZE];

    let output_mem =
        MemoryInfo::new(AllocationDevice::CPU, 0, AllocatorType::Device, MemoryType::CPUOutput)
            .map_err(ort_err)?;

    let mut total_expanded: u64 = 0;
    let mut total_processed: u64 = 0;
    let mut error_count: u64 = 0;

    // 合法手 softmax 用バッファ（再利用）
    let mut logits_buf = Vec::with_capacity(600);
    let mut probs_buf = Vec::with_capacity(600);

    loop {
        if INTERRUPTED.load(Ordering::SeqCst) {
            break;
        }

        // バッチ分読み込み
        let mut batch_records: Vec<(PackedSfenValue, String)> = Vec::with_capacity(batch_size);
        while batch_records.len() < batch_size {
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
                    error_count += 1;
                    continue;
                }
            };
            let sfen = match unpack_sfen(&psv.sfen) {
                Ok(s) => s,
                Err(_) => {
                    error_count += 1;
                    continue;
                }
            };
            batch_records.push((psv, sfen));
        }

        let actual_batch = batch_records.len();
        if actual_batch == 0 {
            break;
        }

        // 特徴量構築
        f1_buf[..actual_batch * FEATURES1_SIZE].fill(0.0);
        f2_buf[..actual_batch * FEATURES2_SIZE].fill(0.0);

        for (i, (_, sfen)) in batch_records.iter().enumerate() {
            let mut pos = Position::new();
            if pos.set_sfen(sfen).is_err() {
                error_count += 1;
                continue;
            }
            let f1 = &mut f1_buf[i * FEATURES1_SIZE..(i + 1) * FEATURES1_SIZE];
            let f2 = &mut f2_buf[i * FEATURES2_SIZE..(i + 1) * FEATURES2_SIZE];
            make_input_features(&pos, f1, f2);
        }

        // ONNX 推論
        let shape1: [usize; 4] = [actual_batch, INPUT1_CHANNELS, 9, 9];
        let input1 =
            TensorRef::<f32>::from_array_view((shape1, &f1_buf[..actual_batch * FEATURES1_SIZE]))
                .map_err(ort_err)?;

        let shape2: [usize; 4] = [actual_batch, INPUT2_CHANNELS, 9, 9];
        let input2 =
            TensorRef::<f32>::from_array_view((shape2, &f2_buf[..actual_batch * FEATURES2_SIZE]))
                .map_err(ort_err)?;

        let mut binding = session.create_binding().map_err(ort_err)?;
        binding.bind_input("input1", &input1).map_err(ort_err)?;
        binding.bind_input("input2", &input2).map_err(ort_err)?;
        binding.bind_output_to_device("output_policy", &output_mem).map_err(ort_err)?;
        binding.bind_output_to_device("output_value", &output_mem).map_err(ort_err)?;

        let outputs = session.run_binding(&binding).map_err(ort_err)?;

        // ポリシー出力を取得: shape [batch, MAX_MOVE_LABEL_NUM]
        let (_, policy_data) =
            outputs["output_policy"].try_extract_tensor::<f32>().map_err(ort_err)?;

        // 各局面について合法手の確率を計算し、閾値超えの次局面を生成
        for (i, (psv, sfen)) in batch_records.iter().enumerate() {
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

            let policy_row = &policy_data[i * MAX_MOVE_LABEL_NUM..(i + 1) * MAX_MOVE_LABEL_NUM];

            // 合法手のロジットを収集
            logits_buf.clear();
            for mv in list.iter() {
                let label = make_move_label(*mv, color);
                logits_buf.push(policy_row[label]);
            }

            // softmax
            probs_buf.resize(logits_buf.len(), 0.0);
            softmax_normalize(&logits_buf, &mut probs_buf);

            // 閾値を超える手の次局面を書き出す
            for (j, mv) in list.iter().enumerate() {
                if probs_buf[j] > threshold {
                    let gives_check = pos.gives_check(*mv);
                    pos.do_move(*mv, gives_check);

                    let packed = pack_position(&pos);
                    let new_psv = PackedSfenValue {
                        sfen: packed,
                        score: 0,
                        move16: 0,
                        game_ply: psv.game_ply.saturating_add(1),
                        game_result: 0,
                        padding: 0,
                    };
                    writer.write_all(&new_psv.to_bytes())?;
                    total_expanded += 1;

                    pos.undo_move(*mv);
                }
            }
        }

        total_processed += actual_batch as u64;
        progress.inc(actual_batch as u64);
    }

    writer.flush()?;
    progress.finish_with_message("Done");

    eprintln!("Processed: {total_processed} positions");
    eprintln!("Expanded: {total_expanded} new positions");
    if error_count > 0 {
        eprintln!("Errors: {error_count}");
    }
    eprintln!("Output: {}", cli.output.display());

    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    #[cfg(not(feature = "dlshogi-onnx"))]
    {
        let _ = cli;
        anyhow::bail!(
            "This binary requires the 'dlshogi-onnx' feature.\n\
             Rebuild with: cargo build --release -p tools --features dlshogi-onnx \
             --bin expand_psv_from_policy"
        );
    }

    #[cfg(feature = "dlshogi-onnx")]
    run(&cli)
}
