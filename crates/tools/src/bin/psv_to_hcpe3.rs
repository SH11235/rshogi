//! psv_to_hcpe3 - PackedSfenValue を dlshogi 学習用 hcpe3 / hcpe に変換
//!
//! YaneuraOu の PackedSfenValue（PSV, 40バイト固定長）を、dlshogi の train.py が
//! 食う形式へストリーミング変換する。PSV は局面単位（棋譜構造を持たない）ため、
//! `--format hcpe3` では各局面を「1 局面 = 1 game」の退化した hcpe3 として書く
//! （moveNum=1 / candidateNum=1 / visitNum=1、policy target は best move の one-hot）。
//! `--format hcpe` は dlshogi 同梱 psv_to_hcpe.py と同じ 38 バイト hcpe を出力する。
//!
//! 盤面の hcp（HuffmanCodedPos, 32B）は、PSV の packed sfen を SFEN 文字列・`Position`
//! 構築を経由せず `tools::packed_sfen::unpack_sfen_to_parts` → `pack_hcp_from_parts` で
//! 直接展開する（ホットパスでのヒープ割り当てを避ける）。指し手 move16 と勝敗・eval の
//! 視点変換は本ファイル内で行う（いずれも hcpe / hcpe3 形式の参照実装 cshogi の
//! `to_hcp` / `move16_from_psv` と一致）。
//! load-all を避けてチャンクストリーミングし、ピークメモリを入力件数に非依存にする。
//!
//! # 使用例
//!
//! ```bash
//! # PSV -> hcpe3（dlshogi train.py 用、既定）
//! cargo run -p tools --bin psv_to_hcpe3 -- \
//!   --input data.psv --output train.hcpe3
//!
//! # PSV -> hcpe（dlshogi test_data 用、38B）
//! cargo run -p tools --bin psv_to_hcpe3 -- \
//!   --input data.psv --output val.hcpe --format hcpe
//!
//! # 先頭 300 万件だけ変換し全コアを使う
//! cargo run -p tools --bin psv_to_hcpe3 -- \
//!   --input data.psv --output head.hcpe3 --limit 3000000 --threads 0
//! ```

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use rayon::prelude::*;
use std::fs::File;
use std::io::{BufReader, BufWriter, IsTerminal, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use rshogi_core::types::Color;
use tools::packed_sfen::{PackedSfenValue, pack_hcp_from_parts, unpack_sfen_to_parts};

/// hcpe3 レコード長（hcp[32] + moveNum:u16 + result:u8 + opponent:u8
/// + selectedMove16:u16 + eval:i16 + candidateNum:u16 + move16:u16 + visitNum:u16）
const HCPE3_SIZE: usize = 46;
/// hcpe レコード長（hcp[32] + eval:i16 + bestMove16:u16 + gameResult:u8 + dummy:u8）
const HCPE_SIZE: usize = 38;
/// 出力レコードバッファ。両形式の最大長を確保し `len` で実長を持つ。
const RECORD_BUF: usize = HCPE3_SIZE;

const IO_BUF_SIZE: usize = 1 << 20;

/// 非TTY 実行時にテキスト進捗を出す最小間隔（秒）。
const PROGRESS_LOG_SECS: u64 = 5;

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum Format {
    /// dlshogi train.py 用の hcpe3（1 局面 = 1 game の退化形, 46B）
    Hcpe3,
    /// dlshogi test_data 用の hcpe（38B）
    Hcpe,
}

#[derive(Parser)]
#[command(
    name = "psv_to_hcpe3",
    version,
    about = "PackedSfenValue を dlshogi 学習用 hcpe3 / hcpe に変換"
)]
struct Cli {
    /// 入力 PSV ファイル
    #[arg(short, long)]
    input: PathBuf,

    /// 出力ファイル（hcpe3 または hcpe）
    #[arg(short, long)]
    output: PathBuf,

    /// 出力形式
    #[arg(long, value_enum, default_value_t = Format::Hcpe3)]
    format: Format,

    /// 処理するレコード数の上限（0=無制限）
    #[arg(long, default_value_t = 0)]
    limit: usize,

    /// スレッド数（0=全コア）
    #[arg(long, default_value_t = 0)]
    threads: usize,

    /// チャンクサイズ（レコード数）。ピークメモリはこの値に比例し、入力件数に依存しない。
    #[arg(long, default_value_t = 200_000)]
    chunk: usize,

    /// evalfix の係数 a（有限の正数）。指定すると eval を `round_ties_even(score × 756.0865/a)` で
    /// 焼き込み ±32767 でクランプする（python `psv_to_hcpe_flat.py --evalfix_a` と bit 一致）。
    /// 未指定なら生 score を書く。0・負・非有限値はエラー。
    #[arg(long)]
    evalfix_a: Option<f64>,

    /// 詳細出力（変換できなかったレコードを逐次ログ）
    #[arg(short, long)]
    verbose: bool,
}

/// 1 レコードの変換結果。出力は `data[..len]`。
enum ConvResult {
    Record { data: [u8; RECORD_BUF], len: usize },
    Error(String),
}

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// game_result（手番側視点の ±1/0）を cshogi gameResult（0:DRAW / 1:BLACK_WIN / 2:WHITE_WIN）へ。
///
/// 手番側勝ち(`1`)なら勝者 = 手番側、手番側負け(`-1`)なら勝者 = 相手側になる。
/// cshogi の `to_hcp` 系と同じく BLACK=0 / WHITE=1 を `+1` した値が勝者の色を表す。
#[inline]
fn game_result_byte(game_result: i8, side_to_move: Color) -> u8 {
    let stm = side_to_move.index() as u8;
    match game_result {
        1 => stm + 1,
        -1 => 2 - stm,
        _ => 0,
    }
}

/// YaneuraOu PSV の move16 を hcpe / hcpe3 が要求する move16 へ意味的に復号する。
///
/// YaneuraOu PSV の move16 は bit14=駒打ちフラグ（from フィールド=駒種, 歩=1..飛=7）、
/// bit15=成りフラグ。hcpe 形式は駒打ちを `from = 81 + (駒種 - 1)` で表し、成りを bit14 で
/// 表すため、両フラグを見て変換する。実 YaneuraOu の手で形式の参照実装 cshogi
/// `move16_from_psv` と一致することを確認済み。
#[inline]
fn move16_psv_to_hcpe(yo_move16: u16) -> u16 {
    let to = yo_move16 & 0x7f;
    let from_field = (yo_move16 >> 7) & 0x7f;
    if yo_move16 & 0x4000 != 0 {
        // 駒打ち: from フィールドは駒種(1 始まり) → from = 81 + (駒種 - 1) = 80 + from_field
        to | ((80 + from_field) << 7)
    } else {
        // 盤上の手: 成りは YaneuraOu の bit15 → hcpe の bit14 へ移す
        let promote = if yo_move16 & 0x8000 != 0 { 0x4000 } else { 0 };
        to | (from_field << 7) | promote
    }
}

/// dlshogi 固定 decode 定数 = 1/0.0013226。evalfix bake のスケール分子。
const EVAL_DECODE_CONST: f64 = 756.0864962951762;

/// evalfix: 生 score に `756.0865/a` を掛けて焼き込む。round-half-even（python `round` 互換）で丸め、
/// ±32767 にクランプする。`eval_scale=None` なら生 score をそのまま返す。
#[inline]
fn baked_eval(score: i16, eval_scale: Option<f64>) -> i16 {
    match eval_scale {
        Some(s) => (f64::from(score) * s).round_ties_even().clamp(-32767.0, 32767.0) as i16,
        None => score,
    }
}

fn build_hcpe3(eval: i16, hcp: &[u8; 32], move16: u16, result: u8) -> ConvResult {
    let mut data = [0u8; RECORD_BUF];
    data[0..32].copy_from_slice(hcp);
    data[32..34].copy_from_slice(&1u16.to_le_bytes()); // moveNum
    data[34] = result;
    data[35] = 0; // opponent
    data[36..38].copy_from_slice(&move16.to_le_bytes()); // selectedMove16
    data[38..40].copy_from_slice(&eval.to_le_bytes()); // eval
    data[40..42].copy_from_slice(&1u16.to_le_bytes()); // candidateNum
    data[42..44].copy_from_slice(&move16.to_le_bytes()); // move16（= selectedMove16）
    data[44..46].copy_from_slice(&1u16.to_le_bytes()); // visitNum
    ConvResult::Record {
        data,
        len: HCPE3_SIZE,
    }
}

fn build_hcpe(eval: i16, hcp: &[u8; 32], move16: u16, result: u8) -> ConvResult {
    let mut data = [0u8; RECORD_BUF];
    data[0..32].copy_from_slice(hcp);
    data[32..34].copy_from_slice(&eval.to_le_bytes()); // eval
    data[34..36].copy_from_slice(&move16.to_le_bytes()); // bestMove16
    data[36] = result; // gameResult
    data[37] = 0; // dummy
    ConvResult::Record {
        data,
        len: HCPE_SIZE,
    }
}

fn convert(
    record: &[u8; PackedSfenValue::SIZE],
    format: Format,
    eval_scale: Option<f64>,
) -> ConvResult {
    let psv = match PackedSfenValue::from_bytes(record) {
        Some(v) => v,
        None => return ConvResult::Error("PackedSfenValue のパースに失敗".to_string()),
    };
    // game_result は手番側視点の -1/0/1 のみが正当。範囲外は破損レコードとして
    // skip+count に乗せる（不正値をサイレントに DRAW へ写さない）。
    if !matches!(psv.game_result, -1..=1) {
        return ConvResult::Error(format!("不正な game_result: {}", psv.game_result));
    }
    // ホットパス: String 生成も Position 構築も挟まず packed → hcp を直接展開する
    // (CLAUDE.md「ホットパスでのヒープ割り当て禁止」。完全に stack 上で完結)。
    let parts = match unpack_sfen_to_parts(&psv.sfen) {
        Ok(p) => p,
        Err(e) => return ConvResult::Error(format!("SFEN の展開に失敗: {e}")),
    };

    let hcp = pack_hcp_from_parts(&parts);
    let move16 = move16_psv_to_hcpe(psv.move16);
    let result = game_result_byte(psv.game_result, parts.side_to_move);
    let eval = baked_eval(psv.score, eval_scale);

    match format {
        Format::Hcpe3 => build_hcpe3(eval, &hcp, move16, result),
        Format::Hcpe => build_hcpe(eval, &hcp, move16, result),
    }
}

/// 並列処理結果を入力順のまま書き出す。戻り値: (書き出し件数, エラー件数)。
fn write_results(
    results: &[ConvResult],
    writer: &mut BufWriter<File>,
    verbose: bool,
) -> Result<(u64, u64)> {
    let mut written = 0u64;
    let mut errors = 0u64;
    for result in results {
        match result {
            ConvResult::Record { data, len } => {
                writer.write_all(&data[..*len])?;
                written += 1;
            }
            ConvResult::Error(e) => {
                errors += 1;
                if verbose {
                    eprintln!("レコード変換エラー: {e}");
                }
            }
        }
    }
    Ok((written, errors))
}

fn main() -> Result<()> {
    env_logger::init();
    let cli = Cli::parse();

    if !cli.input.exists() {
        anyhow::bail!("入力ファイルが見つかりません: {}", cli.input.display());
    }
    if cli.chunk == 0 {
        anyhow::bail!("--chunk は 1 以上を指定してください");
    }
    // evalfix のスケールは `EVAL_DECODE_CONST / a` を掛ける係数のため、a は有限の正数のみ許可する。
    // 0/負/NaN/inf を素通しすると全 eval が 0 や符号反転で静かに壊れるので、出力作成前に弾く。
    if let Some(a) = cli.evalfix_a
        && (!a.is_finite() || a <= 0.0)
    {
        anyhow::bail!("--evalfix-a は有限の正数を指定してください（指定値: {a}）");
    }

    // 入出力が同一パスならデータ消失を防ぐためエラーにする
    let in_canonical = cli
        .input
        .canonicalize()
        .with_context(|| format!("入力パスの正規化に失敗: {}", cli.input.display()))?;
    if cli.output.exists() {
        let out_canonical = cli
            .output
            .canonicalize()
            .with_context(|| format!("出力パスの正規化に失敗: {}", cli.output.display()))?;
        if in_canonical == out_canonical {
            anyhow::bail!("入力と出力が同一ファイルです: {}", in_canonical.display());
        }
    }

    if cli.threads > 0 {
        rayon::ThreadPoolBuilder::new()
            .num_threads(cli.threads)
            .build_global()
            .context("Failed to build rayon thread pool")?;
    }

    ctrlc::set_handler(|| {
        eprintln!("\n中断シグナルを受信しました。処理を終了します...");
        INTERRUPTED.store(true, Ordering::SeqCst);
    })
    .context("Ctrl-C ハンドラの設定に失敗")?;

    let file_size = std::fs::metadata(&cli.input)?.len();
    let estimated_records = file_size / PackedSfenValue::SIZE as u64;
    let total = if cli.limit > 0 {
        estimated_records.min(cli.limit as u64)
    } else {
        estimated_records
    };
    eprintln!(
        "入力: {} ({} バイト, 約 {} レコード), format={:?}",
        cli.input.display(),
        file_size,
        estimated_records,
        cli.format
    );

    // 非TTY(background / redirect)では indicatif の progress bar はログに何も描かれず、
    // 大量変換が「無反応 = ハング」に見えてしまう。TTY なら従来通り bar を描き、非TTY では
    // bar を隠して代わりに周期テキスト進捗(下のループ)を出す。
    let is_tty = std::io::stderr().is_terminal();
    let progress = ProgressBar::new(total);
    if is_tty {
        progress.set_style(
            ProgressStyle::default_bar()
                .template(
                    "[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} ({per_sec}) ETA: {eta}",
                )
                .expect("valid template"),
        );
    } else {
        progress.set_draw_target(ProgressDrawTarget::hidden());
        eprintln!("(非TTY: 進捗を {PROGRESS_LOG_SECS} 秒ごとにテキスト出力します)");
    }

    let in_file =
        File::open(&cli.input).with_context(|| format!("{} を開けません", cli.input.display()))?;
    let mut reader = BufReader::with_capacity(IO_BUF_SIZE, in_file);

    // 一時ファイルに書き、正常完了時のみ最終パスへ rename する（中断時の破損出力を防ぐ）。
    // `--output foo.tmp` でも最終パスと衝突しないよう、拡張子置換ではなくサフィックス付与する。
    let tmp_output = {
        let mut s = cli.output.clone().into_os_string();
        s.push(".partial");
        PathBuf::from(s)
    };
    // 入力が偶然 `<output>.partial` と同一ファイルだと、書き込み開始で入力を truncate して
    // しまうため拒否する（`tmp_output` が存在＝入力と同じ実体ならここで検出できる）。
    if tmp_output.exists() {
        let tmp_canonical = tmp_output
            .canonicalize()
            .with_context(|| format!("一時パスの正規化に失敗: {}", tmp_output.display()))?;
        if tmp_canonical == in_canonical {
            anyhow::bail!("一時ファイル {} が入力と同一です", tmp_output.display());
        }
    }
    let out_file = File::create(&tmp_output)
        .with_context(|| format!("{} を作成できません", tmp_output.display()))?;
    let mut writer = BufWriter::with_capacity(IO_BUF_SIZE, out_file);
    // 処理中は一時ファイルに書き、完了時に最終パスへ rename する。実行中に最終パスが
    // 存在しなくても異常ではない（途中経過は下記 .partial を見る）。
    eprintln!(
        "出力(処理中): {} → 完了時に {} へ rename",
        tmp_output.display(),
        cli.output.display()
    );

    let format = cli.format;
    let eval_scale = cli.evalfix_a.map(|a| EVAL_DECODE_CONST / a);
    let verbose = cli.verbose;
    let limit = cli.limit;
    let mut remaining = if limit > 0 { limit } else { usize::MAX };
    let mut chunk: Vec<[u8; PackedSfenValue::SIZE]> = Vec::with_capacity(cli.chunk);
    let mut buffer = [0u8; PackedSfenValue::SIZE];
    let mut total_written = 0u64;
    let mut total_errors = 0u64;
    let mut interrupted = false;
    let mut reached_eof = false;
    let start = Instant::now();
    let mut last_report = start;

    while remaining > 0 {
        if INTERRUPTED.load(Ordering::Acquire) {
            interrupted = true;
            break;
        }

        chunk.clear();
        let chunk_target = remaining.min(cli.chunk);
        for _ in 0..chunk_target {
            match reader.read_exact(&mut buffer) {
                Ok(()) => chunk.push(buffer),
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    reached_eof = true;
                    break;
                }
                Err(e) => return Err(e.into()),
            }
        }
        if chunk.is_empty() {
            break;
        }
        remaining -= chunk.len();

        let results: Vec<ConvResult> =
            chunk.par_iter().map(|record| convert(record, format, eval_scale)).collect();

        let (written, errors) = write_results(&results, &mut writer, verbose)?;
        total_written += written;
        total_errors += errors;
        progress.inc(results.len() as u64);

        // 非TTY では progress bar が出ないため、一定間隔でテキスト進捗を出す。
        if !is_tty {
            let now = Instant::now();
            if now.duration_since(last_report).as_secs() >= PROGRESS_LOG_SECS {
                last_report = now;
                let done = total_written + total_errors;
                let secs = start.elapsed().as_secs_f64();
                let rate = done as f64 / secs.max(1e-9);
                let eta = if total > done && rate > 0.0 {
                    (total - done) as f64 / rate
                } else {
                    0.0
                };
                eprintln!(
                    "進捗: {done}/{total} レコード ({rate:.0} rec/s, 経過 {secs:.0}s, ETA {eta:.0}s)"
                );
            }
        }
    }

    writer.flush()?;
    drop(writer);

    if interrupted {
        progress.abandon_with_message("中断");
        // 中断時は不完全な一時ファイルを削除する
        let _ = std::fs::remove_file(&tmp_output);
        eprintln!("完了前に中断されました。出力は書き込まれていません。");
        return Ok(());
    }

    // EOF まで読み切った場合、末尾の半端なバイト（レコード長未満）は破損とみなしてカウントする。
    // `--limit` で途中終了したときは末尾まで到達していないため対象外。
    let trailing_bytes = file_size % PackedSfenValue::SIZE as u64;
    if reached_eof && trailing_bytes != 0 {
        total_errors += 1;
        if verbose {
            eprintln!("エラー: 末尾 {trailing_bytes} バイトは完全な PSV レコードではありません");
        }
    }

    std::fs::rename(&tmp_output, &cli.output).with_context(|| {
        format!("{} -> {} のリネームに失敗", tmp_output.display(), cli.output.display())
    })?;
    // エラーレコードがあると推定 total と実 pos がずれるため、実処理件数で長さを確定して完了させる。
    progress.set_length(total_written + total_errors);
    progress.finish_with_message("完了");

    eprintln!(
        "書き出し: {} レコード ({:?}) -> {}",
        total_written,
        format,
        cli.output.display()
    );
    if total_errors > 0 {
        eprintln!("注意: {total_errors} レコードを変換できずスキップしました");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn game_result_mapping_matches_oracle() {
        // 手番側勝ち(1): 勝者 = 手番側
        assert_eq!(game_result_byte(1, Color::Black), 1); // BLACK_WIN
        assert_eq!(game_result_byte(1, Color::White), 2); // WHITE_WIN
        // 手番側負け(-1): 勝者 = 相手側
        assert_eq!(game_result_byte(-1, Color::Black), 2); // WHITE_WIN
        assert_eq!(game_result_byte(-1, Color::White), 1); // BLACK_WIN
        // 引き分け(0): DRAW
        assert_eq!(game_result_byte(0, Color::Black), 0);
        assert_eq!(game_result_byte(0, Color::White), 0);
    }

    #[test]
    fn move16_psv_to_hcpe_matches_oracle() {
        // 実 YaneuraOu PSV move16（bit14=駒打ち, bit15=成り）を hcpe 形式へ。
        // 期待値は cshogi `move16_from_psv` で生成（参照実装）。
        assert_eq!(move16_psv_to_hcpe(0x0000), 0x0000); // none
        assert_eq!(move16_psv_to_hcpe(0x078e), 0x078e); // 通常手 2g2f
        assert_eq!(move16_psv_to_hcpe(0x40a5), 0x28a5); // 歩打ち P*5b（bit14, from=1）
        assert_eq!(move16_psv_to_hcpe(0x4380), 0x2b80); // 金打ち G*1a（bit14, from=7）
        assert_eq!(move16_psv_to_hcpe(0x9f46), 0x5f46); // 成り 7i8h+（bit15 → hcpe bit14）
    }

    #[test]
    fn baked_eval_round_half_even_and_clamp() {
        // evalfix 未指定なら生 score を素通し。
        assert_eq!(baked_eval(0, None), 0);
        assert_eq!(baked_eval(12345, None), 12345);
        assert_eq!(baked_eval(-30000, None), -30000);

        // python `round`（round-half-even / 偶数丸め）と一致すること。
        // scale=2.5 で .5 ちょうどに乗る値を選ぶ。
        assert_eq!(baked_eval(1, Some(2.5)), 2); // 2.5 → 偶数側 2
        assert_eq!(baked_eval(3, Some(2.5)), 8); // 7.5 → 偶数側 8
        assert_eq!(baked_eval(1, Some(0.5)), 0); // 0.5 → 偶数側 0
        assert_eq!(baked_eval(3, Some(0.5)), 2); // 1.5 → 偶数側 2

        // ±32767 クランプ。
        assert_eq!(baked_eval(32767, Some(2.0)), 32767); // 65534 → クランプ
        assert_eq!(baked_eval(-30000, Some(2.0)), -32767); // -60000 → クランプ
    }

    #[test]
    fn baked_eval_matches_python_golden() {
        // 本番スケール（EVAL_DECODE_CONST / a, a=1141.38…）での焼き込み結果が python 参照
        // `psv_to_hcpe_flat.py`（`round` = round-half-even, ±32767 クランプ）の値と一致すること。
        // EVAL_DECODE_CONST や式を誤変更すると golden が崩れて落ちる（決定性 + 定数の回帰検出）。
        let scale = Some(EVAL_DECODE_CONST / 1_141.381_354_386_831);
        let golden: [(i16, i16); 10] = [
            (0, 0),
            (1, 1),
            (2, 1),
            (100, 66),
            (1000, 662),
            (-1000, -662),
            (12345, 8178),
            (-12345, -8178),
            (32767, 21706),
            (-32767, -21706),
        ];
        for (score, expected) in golden {
            assert_eq!(baked_eval(score, scale), expected, "score={score}");
        }
    }
}
