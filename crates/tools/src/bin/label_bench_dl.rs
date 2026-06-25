//! `label_bench` の jsonl に DL水匠 (標準 dlshogi ONNX) の静的評価を追記するツール。
//!
//! ground truth ラベラー (`label_bench_positions`) が出した jsonl の各行を読み、
//! `sfen` を DL水匠 value head で静的評価して `eval_dl`（既定）フィールドを追記する。
//! 後段で ground truth の `eval_deep` とクラス別に比較するための入力を作る。
//!
//! `dlshogi-onnx` は default feature なので、追加フラグ無しの default build で推論が動く。
//! `dlshogi-onnx` を外したビルド (ONNX 依存だけなら `--no-default-features --features nnue-arch`)
//! でもコンパイルは通り、実行時に再ビルド手順を案内して終了する。

use clap::Parser;
#[cfg(any(feature = "dlshogi-onnx", test))]
use rshogi_core::types::Color;
#[cfg(any(feature = "dlshogi-onnx", test))]
use serde_json::{Map, Value};

#[derive(Parser, Debug)]
#[command(
    name = "label_bench_dl",
    about = "label_bench jsonl の各局面に DL水匠 (dlshogi ONNX) の静的評価を追記する"
)]
struct Cli {
    /// 入力 jsonl（各行 JSON object、`sfen` フィールド必須）
    #[arg(long)]
    r#in: std::path::PathBuf,

    /// 出力 jsonl
    #[arg(long)]
    out: std::path::PathBuf,

    /// 追記するフィールド名
    #[arg(long, default_value = "eval_dl")]
    out_field: String,

    /// dlshogi ONNX モデルのパス
    #[arg(long)]
    dlshogi_onnx_model: std::path::PathBuf,

    /// TensorRT EP (FP16) を使う。未指定なら CUDA EP (FP32)。
    #[arg(long)]
    onnx_tensorrt: bool,

    /// TensorRT エンジンキャッシュの保存先（`--onnx-tensorrt` 時のみ有効）
    #[arg(long)]
    onnx_tensorrt_cache: Option<std::path::PathBuf>,

    /// 1 回の推論あたりの最大局面数
    #[arg(long, default_value_t = 1024)]
    onnx_batch_size: usize,

    /// CUDA device id（負値で CPU 推論）
    #[arg(long, default_value_t = 0)]
    onnx_gpu_id: i32,

    /// winrate→cp 変換スケール
    #[arg(long, default_value_t = 600.0)]
    onnx_eval_scale: f32,
}

/// 手番 (STM) 視点 cp を先手視点 cp へ変換する。
///
/// `eval_deep` / `eval_cp_black` と同じく先手視点 cp に揃える。後手番では符号反転する。
#[cfg(any(feature = "dlshogi-onnx", test))]
fn stm_cp_to_black_view(stm_cp: i32, stm: Color) -> i32 {
    if stm == Color::White { -stm_cp } else { stm_cp }
}

/// 元 JSON object の全フィールドを保持したまま `field` を `value` で挿入する。
///
/// 既に同名フィールドがあれば上書きする。
#[cfg(any(feature = "dlshogi-onnx", test))]
fn insert_field(mut obj: Map<String, Value>, field: &str, value: i32) -> Map<String, Value> {
    obj.insert(field.to_string(), Value::from(value));
    obj
}

#[cfg(not(feature = "dlshogi-onnx"))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!(
        "label_bench_dl requires the 'dlshogi-onnx' feature (on by default; this build disabled it).\n\
         Rebuild with default features: cargo build --release -p tools --bin label_bench_dl"
    )
}

#[cfg(feature = "dlshogi-onnx")]
fn main() -> anyhow::Result<()> {
    use std::io::{BufRead, BufReader, BufWriter, Write};

    use anyhow::Context;
    use rshogi_core::position::Position;
    use tools::onnx_value::{OnnxValueConfig, OnnxValueEvaluator};

    let cli = Cli::parse();

    if cli.r#in == cli.out {
        anyhow::bail!("--in and --out must differ (got the same path)");
    }
    // 字面が違っても同一ファイル（`./x` と `x`、symlink 等）を指す場合、出力 create が
    // 入力を truncate して壊すため、解決後パスでも弾く（label_bench_positions と同じ方針）。
    if let (Ok(a), Ok(b)) = (std::fs::canonicalize(&cli.r#in), std::fs::canonicalize(&cli.out))
        && a == b
    {
        anyhow::bail!("--in and --out resolve to the same file");
    }
    // canonicalize は hardlink（別 path・同一 inode）を検出できない。出力 create が入力を
    // truncate する事故を防ぐため、dev/ino でも同一性を弾く。
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Ok(im), Ok(om)) = (std::fs::metadata(&cli.r#in), std::fs::metadata(&cli.out))
            && im.dev() == om.dev()
            && im.ino() == om.ino()
        {
            anyhow::bail!("--in and --out are the same file (same dev/ino)");
        }
    }
    if cli.out.exists()
        && std::fs::symlink_metadata(&cli.out)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
    {
        anyhow::bail!("--out points to a symlink, refusing to write: {}", cli.out.display());
    }
    if cli.onnx_batch_size == 0 {
        anyhow::bail!("--onnx-batch-size must be > 0");
    }
    if cli.onnx_tensorrt && cli.onnx_gpu_id < 0 {
        anyhow::bail!("--onnx-tensorrt requires a GPU (--onnx-gpu-id >= 0)");
    }
    if !cli.onnx_eval_scale.is_finite() || cli.onnx_eval_scale <= 0.0 {
        anyhow::bail!(
            "--onnx-eval-scale must be a positive finite value, got {}",
            cli.onnx_eval_scale
        );
    }

    let cfg = OnnxValueConfig {
        model_path: cli.dlshogi_onnx_model.clone(),
        gpu_id: cli.onnx_gpu_id,
        use_tensorrt: cli.onnx_tensorrt,
        tensorrt_cache: cli.onnx_tensorrt_cache.clone(),
        eval_scale: cli.onnx_eval_scale,
        batch_size: cli.onnx_batch_size,
    };
    let mut evaluator = OnnxValueEvaluator::new(&cfg)?;

    let in_file = std::fs::File::open(&cli.r#in)
        .with_context(|| format!("failed to open {}", cli.r#in.display()))?;
    let reader = BufReader::new(in_file);
    let out_file = std::fs::File::create(&cli.out)
        .with_context(|| format!("failed to create {}", cli.out.display()))?;
    let mut writer = BufWriter::new(out_file);

    // バッチ単位の streaming 処理。全件 load せず batch_size 行ずつ溜めて推論する。
    let batch_size = cli.onnx_batch_size;
    let mut objs: Vec<Map<String, Value>> = Vec::with_capacity(batch_size);
    let mut positions: Vec<Position> = Vec::with_capacity(batch_size);
    let mut parse_errors: u64 = 0;
    let mut sfen_errors: u64 = 0;
    let mut non_object: u64 = 0;
    let mut written: u64 = 0;

    let flush_batch = |objs: &mut Vec<Map<String, Value>>,
                       positions: &[Position],
                       evaluator: &mut OnnxValueEvaluator,
                       writer: &mut BufWriter<std::fs::File>,
                       written: &mut u64|
     -> anyhow::Result<()> {
        if positions.is_empty() {
            return Ok(());
        }
        let stm_cps = evaluator.evaluate(positions)?;
        // 手番は SFEN が正なので Position から取る（JSON の `stm` 欠落/不一致に依存しない）。
        for ((obj, pos), &stm_cp) in objs.iter().zip(positions.iter()).zip(stm_cps.iter()) {
            let black_cp = stm_cp_to_black_view(stm_cp, pos.side_to_move());
            let merged = insert_field(obj.clone(), &cli.out_field, black_cp);
            let line = serde_json::to_string(&Value::Object(merged))?;
            writeln!(writer, "{line}")?;
            *written += 1;
        }
        objs.clear();
        Ok(())
    };

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => {
                parse_errors += 1;
                continue;
            }
        };
        let obj = match value {
            Value::Object(m) => m,
            _ => {
                non_object += 1;
                continue;
            }
        };
        let sfen = match obj.get("sfen").and_then(Value::as_str) {
            Some(s) => s,
            None => {
                sfen_errors += 1;
                continue;
            }
        };
        let mut pos = Position::new();
        if pos.set_sfen(sfen).is_err() {
            sfen_errors += 1;
            continue;
        }

        objs.push(obj);
        positions.push(pos);

        if objs.len() >= batch_size {
            flush_batch(&mut objs, &positions, &mut evaluator, &mut writer, &mut written)?;
            positions.clear();
        }
    }
    flush_batch(&mut objs, &positions, &mut evaluator, &mut writer, &mut written)?;
    writer.flush()?;

    eprintln!(
        "Done. written={written} parse_errors={parse_errors} sfen_errors={sfen_errors} \
         non_object={non_object}"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stm_cp_to_black_view_white_negates() {
        // 先手番: そのまま
        assert_eq!(stm_cp_to_black_view(120, Color::Black), 120);
        assert_eq!(stm_cp_to_black_view(-80, Color::Black), -80);
        // 後手番: 符号反転（STM が有利 = 先手視点では不利）
        assert_eq!(stm_cp_to_black_view(120, Color::White), -120);
        assert_eq!(stm_cp_to_black_view(-80, Color::White), 80);
        // 0 はどちらでも 0
        assert_eq!(stm_cp_to_black_view(0, Color::White), 0);
    }

    #[test]
    fn test_insert_field_preserves_and_adds() {
        let mut obj = Map::new();
        obj.insert("sfen".to_string(), Value::from("startpos"));
        obj.insert("ply".to_string(), Value::from(32));
        obj.insert("eval_cp_black".to_string(), Value::from(123));

        let merged = insert_field(obj, "eval_dl", -87);

        // 既存フィールドは保持
        assert_eq!(merged.get("sfen").and_then(Value::as_str), Some("startpos"));
        assert_eq!(merged.get("ply").and_then(Value::as_i64), Some(32));
        assert_eq!(merged.get("eval_cp_black").and_then(Value::as_i64), Some(123));
        // out-field が追加されている
        assert_eq!(merged.get("eval_dl").and_then(Value::as_i64), Some(-87));
    }

    #[test]
    fn test_insert_field_overwrites_existing() {
        let mut obj = Map::new();
        obj.insert("eval_dl".to_string(), Value::from(1));
        let merged = insert_field(obj, "eval_dl", 999);
        assert_eq!(merged.get("eval_dl").and_then(Value::as_i64), Some(999));
    }
}
