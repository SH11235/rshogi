//! hcpe（cshogi HuffmanCodedPosAndEval, 38B/レコード）→ PSV（PackedSfenValue, 40B/レコード）
//! 変換ツール。
//!
//! 外部公開の hcpe 教師/検証プール（例: dlshogi 系で標準の floodgate 検証局面）を、
//! nnue-train の `--data` / `--test-data` が読む PSV 形式へ変換する。
//!
//! 盤面は `tools::packed_sfen::unpack_hcp_to_parts` → `pack_sfen_from_parts` で
//! SFEN 文字列・`Position` 構築を経由せず直接再パックする（ホットパスでのヒープ
//! 割り当てなし）。move16 / gameResult の形式差も同モジュールの共有変換
//! （`hcpe_move16_to_psv` / `hcpe_result_to_stm`）で吸収する。
//! チャンク読み + rayon 並列で入力順を保持したまま変換し、`.partial` へ書いて
//! 成功時のみ最終パスへ rename する。
//!
//! # フィールド対応
//!
//! - 局面: `HuffmanCodedPos`（Apery/cshogi 形式）→ `PackedSfen`（YaneuraOu 形式）。
//! - eval: 手番側視点 cp（両形式で同一規約）をそのままコピー。詰み帯の数値表現は
//!   生成系により異なる（例: gensfen は PSV 詰みを ±10000 帯で保存）が、値変換は行わない。
//! - bestMove16: cshogi 形式 → **実 YaneuraOu Move16** 形式（bit14=駒打ち/bit15=成り。
//!   旧リポジトリ内部形式 (B) とは別形式）。
//! - gameResult: 絶対視点（0=draw / 1=black_win / 2=white_win）→ 手番側視点
//!   （1=win / -1=loss / 0=draw）。
//! - game_ply: hcpe には手数が無いため 1 固定。
//!
//! # 使用例
//!
//! ```bash
//! cargo run --release -p tools --bin hcpe_to_psv -- \
//!   --input "$SHOGI_DATA/validation/floodgate_hcpe_yamaoka/floodgate.hcpe" \
//!   --output "$SHOGI_DATA/validation/floodgate_hcpe_yamaoka/floodgate.psv"
//! ```
use std::fs::File;
use std::io::{BufReader, BufWriter, IsTerminal, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use anyhow::{Context, Result};
use clap::Parser;
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use rayon::prelude::*;
use tools::hcpe::{HCPE_RECORD_SIZE, HcpeRecord};
use tools::packed_sfen::{
    PackedSfenValue, hcpe_move16_to_psv, hcpe_result_to_stm, pack_sfen_from_parts,
    unpack_hcp_to_parts,
};

const IO_BUF_SIZE: usize = 1 << 20;

/// 非TTY 実行時にテキスト進捗を出す最小間隔（秒）。
const PROGRESS_LOG_SECS: u64 = 5;

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

#[derive(Parser, Debug)]
#[command(
    name = "hcpe_to_psv",
    about = "hcpe (38B/レコード) を PSV (PackedSfenValue 40B/レコード) に変換する"
)]
struct Args {
    /// 入力 hcpe ファイル（カンマ区切りで複数可）。--input-dir と排他
    #[arg(long)]
    input: Option<String>,

    /// 入力ディレクトリ。--pattern と組み合わせて使用。--input と排他
    #[arg(long)]
    input_dir: Option<PathBuf>,

    /// --input-dir 使用時の glob パターン
    #[arg(long, default_value = "*.hcpe")]
    pattern: String,

    /// 出力ファイルパス（PSV 形式）。入力順（複数ファイルはパスのソート順）を保持して連結する
    #[arg(long)]
    output: PathBuf,

    /// 並列変換のチャンクサイズ（レコード数）
    #[arg(long, default_value = "65536")]
    chunk: usize,

    /// rayon スレッド数（0 = 自動）
    #[arg(long, default_value = "0")]
    threads: usize,
}

#[derive(Default)]
struct Stats {
    converted: u64,
    /// hcp デコード失敗（Huffman 破損・玉重複・在庫超過）
    decode_errors: u64,
    /// bestMove16 が不正な駒打ち駒種
    move_errors: u64,
    /// bestMove16 == 0（終局直前レコード等、指し手なし）。壊れた指し手とは区別する
    no_bestmove: u64,
    /// gameResult が 0/1/2 以外
    result_errors: u64,
}

impl Stats {
    fn merge(&mut self, other: &Stats) {
        self.converted += other.converted;
        self.decode_errors += other.decode_errors;
        self.move_errors += other.move_errors;
        self.no_bestmove += other.no_bestmove;
        self.result_errors += other.result_errors;
    }
}

enum ConvResult {
    Psv([u8; PackedSfenValue::SIZE]),
    DecodeError,
    MoveError,
    NoBestmove,
    ResultError,
}

/// hcpe 1 レコードを PSV 1 レコードへ変換する。壊れたレコードは種別を返して skip する。
fn convert_record(bytes: &[u8; HCPE_RECORD_SIZE]) -> ConvResult {
    let HcpeRecord {
        hcp,
        eval,
        best_move16,
        game_result,
        ..
    } = HcpeRecord::from_bytes(bytes);

    let Ok(parts) = unpack_hcp_to_parts(&hcp) else {
        return ConvResult::DecodeError;
    };

    if best_move16 == 0 {
        return ConvResult::NoBestmove;
    }
    let move16 = hcpe_move16_to_psv(best_move16);
    if move16 == 0 {
        return ConvResult::MoveError;
    }

    let Some(psv_result) = hcpe_result_to_stm(game_result, parts.side_to_move) else {
        return ConvResult::ResultError;
    };

    let psv = PackedSfenValue {
        sfen: pack_sfen_from_parts(&parts),
        score: eval,
        move16,
        game_ply: 1,
        game_result: psv_result,
        padding: 0,
    };
    ConvResult::Psv(psv.to_bytes())
}

fn write_results(
    results: &[ConvResult],
    writer: &mut BufWriter<File>,
    stats: &mut Stats,
) -> Result<()> {
    for result in results {
        match result {
            ConvResult::Psv(bytes) => {
                writer.write_all(bytes)?;
                stats.converted += 1;
            }
            ConvResult::DecodeError => stats.decode_errors += 1,
            ConvResult::MoveError => stats.move_errors += 1,
            ConvResult::NoBestmove => stats.no_bestmove += 1,
            ConvResult::ResultError => stats.result_errors += 1,
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let args = Args::parse();

    if args.chunk == 0 {
        anyhow::bail!("--chunk は 1 以上を指定してください");
    }

    let paths = tools::common::dedup::collect_input_paths(
        args.input.as_deref(),
        args.input_dir.as_ref(),
        &args.pattern,
    )?;
    if paths.is_empty() {
        anyhow::bail!("入力ファイルが見つかりません");
    }

    // 一時ファイルに書き、正常完了時のみ最終パスへ rename する（中断時の途中書き PSV は
    // バイト長がほぼ確実に 40 の倍数になり、下流の整合チェックをすり抜けるため）。
    let tmp_output = {
        let mut s = args.output.clone().into_os_string();
        s.push(".partial");
        PathBuf::from(s)
    };

    // 入力と出力（一時ファイル含む）が同一実体だと File::create が読み取り前に入力を
    // truncate してしまうため拒否する。パス比較 (canonicalize) では hardlink
    // (別パス・同一 inode) を検出できないので same-file の Handle (unix: dev/ino、
    // Windows: volume serial + file index) で判定する。Handle はファイルを開いたまま
    // 保持するため、入力分を Vec に溜めず 1 件ずつ比較して即 drop する
    // (入力が多数でも fd を占有しない)。
    let file_id = |p: &std::path::Path| same_file::Handle::from_path(p).ok();
    let out_id = file_id(&args.output);
    let tmp_id = file_id(&tmp_output);
    let mut total_records = 0u64;
    for p in &paths {
        let id =
            file_id(p).with_context(|| format!("入力のメタデータ取得に失敗: {}", p.display()))?;
        if out_id.as_ref() == Some(&id) {
            anyhow::bail!("入力と出力が同一ファイルです: {}", p.display());
        }
        if tmp_id.as_ref() == Some(&id) {
            anyhow::bail!("一時ファイル {} が入力と同一です", tmp_output.display());
        }
        let len = std::fs::metadata(p)?.len();
        if len % HCPE_RECORD_SIZE as u64 != 0 {
            anyhow::bail!(
                "{} のサイズ {len} が hcpe レコード長 {HCPE_RECORD_SIZE} の倍数ではありません",
                p.display()
            );
        }
        total_records += len / HCPE_RECORD_SIZE as u64;
    }
    // File::create の前に Handle を閉じる（same-file は共有フラグ付きで開くが、
    // 明示的に手放しておく方が安全）。
    drop(out_id);
    drop(tmp_id);

    if args.threads > 0 {
        rayon::ThreadPoolBuilder::new()
            .num_threads(args.threads)
            .build_global()
            .context("rayon スレッドプールの構築に失敗")?;
    }

    ctrlc::set_handler(|| {
        eprintln!("\n中断シグナルを受信しました。処理を終了します...");
        INTERRUPTED.store(true, Ordering::SeqCst);
    })
    .context("Ctrl-C ハンドラの設定に失敗")?;

    let out_file = File::create(&tmp_output)
        .with_context(|| format!("{} を作成できません", tmp_output.display()))?;
    let mut writer = BufWriter::with_capacity(IO_BUF_SIZE, out_file);

    let is_tty = std::io::stderr().is_terminal();
    let progress = ProgressBar::new(total_records);
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

    let mut stats = Stats::default();
    let mut chunk: Vec<[u8; HCPE_RECORD_SIZE]> = Vec::with_capacity(args.chunk);
    let mut buffer = [0u8; HCPE_RECORD_SIZE];
    let mut interrupted = false;
    let start = Instant::now();
    let mut last_report = start;

    'files: for path in &paths {
        eprintln!("Reading: {}", path.display());
        let in_file =
            File::open(path).with_context(|| format!("{} を開けません", path.display()))?;
        let mut reader = BufReader::with_capacity(IO_BUF_SIZE, in_file);

        loop {
            if INTERRUPTED.load(Ordering::Acquire) {
                interrupted = true;
                break 'files;
            }

            chunk.clear();
            for _ in 0..args.chunk {
                match reader.read_exact(&mut buffer) {
                    Ok(()) => chunk.push(buffer),
                    Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                    Err(e) => return Err(e.into()),
                }
            }
            if chunk.is_empty() {
                break;
            }

            let results: Vec<ConvResult> = chunk.par_iter().map(convert_record).collect();
            let mut chunk_stats = Stats::default();
            write_results(&results, &mut writer, &mut chunk_stats)?;
            stats.merge(&chunk_stats);
            progress.inc(results.len() as u64);

            if !is_tty {
                let now = Instant::now();
                if now.duration_since(last_report).as_secs() >= PROGRESS_LOG_SECS {
                    last_report = now;
                    let done = progress.position();
                    let secs = start.elapsed().as_secs_f64();
                    let rate = done as f64 / secs.max(1e-9);
                    eprintln!(
                        "進捗: {done}/{total_records} レコード ({rate:.0} rec/s, 経過 {secs:.0}s)"
                    );
                }
            }
        }
    }

    writer.flush()?;
    drop(writer);

    if interrupted {
        progress.abandon_with_message("中断");
        let _ = std::fs::remove_file(&tmp_output);
        eprintln!("完了前に中断されました。出力は書き込まれていません。");
        return Ok(());
    }
    progress.finish();

    std::fs::rename(&tmp_output, &args.output).with_context(|| {
        format!("{} → {} の rename に失敗", tmp_output.display(), args.output.display())
    })?;

    let elapsed = start.elapsed().as_secs_f64();
    println!("=== hcpe → PSV Summary ===");
    println!("Input files:     {}", paths.len());
    println!("Converted:       {}", stats.converted);
    println!("Decode errors:   {}", stats.decode_errors);
    println!("Move errors:     {}", stats.move_errors);
    println!("No bestmove:     {}", stats.no_bestmove);
    println!("Result errors:   {}", stats.result_errors);
    println!("Output file:     {}", args.output.display());
    println!("Elapsed:         {elapsed:.1} sec");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rshogi_core::position::Position;
    use tools::packed_sfen::{pack_position, pack_position_hcp};

    fn make_hcpe_record(
        pos: &Position,
        eval: i16,
        best_move16: u16,
        game_result: u8,
    ) -> [u8; HCPE_RECORD_SIZE] {
        let mut rec = [0u8; HCPE_RECORD_SIZE];
        rec[0..32].copy_from_slice(&pack_position_hcp(pos));
        rec[32..34].copy_from_slice(&eval.to_le_bytes());
        rec[34..36].copy_from_slice(&best_move16.to_le_bytes());
        rec[36] = game_result;
        rec
    }

    #[test]
    fn convert_record_roundtrips_position_eval_move_result() {
        let mut pos = Position::new();
        pos.set_hirate();
        // 平手初期局面の ▲7六歩: hcpe move16 = to(59) | from(60)<<7
        let hcpe_move = 59u16 | (60 << 7);

        let rec = make_hcpe_record(&pos, -123, hcpe_move, 2);
        let ConvResult::Psv(bytes) = convert_record(&rec) else {
            panic!("convert should succeed");
        };
        let psv = PackedSfenValue::from_bytes(&bytes).unwrap();

        assert_eq!(psv.sfen, pack_position(&pos));
        assert_eq!(psv.score, -123);
        // 通常手は YO PSV 形式でも同一ビット
        assert_eq!(psv.move16, hcpe_move);
        assert_eq!(psv.game_ply, 1);
        // white_win で手番=先手 → 手番側視点 loss
        assert_eq!(psv.game_result, -1);
    }

    #[test]
    fn convert_record_emits_true_yaneuraou_move16_for_drop_and_promote() {
        let mut pos = Position::new();
        pos.set_sfen("9/9/9/9/4k4/9/9/9/4K4 b P 1").expect("set_sfen");

        // 歩打ち 5五: hcpe = to(40) | 81<<7 → YO PSV = to | 1<<7 | bit14
        let rec = make_hcpe_record(&pos, 10, 40 | (81 << 7), 1);
        let ConvResult::Psv(bytes) = convert_record(&rec) else {
            panic!("drop convert should succeed");
        };
        let psv = PackedSfenValue::from_bytes(&bytes).unwrap();
        assert_eq!(psv.move16, 40 | (1 << 7) | 0x4000, "歩打ちは bit14 + 駒種1");

        // 成り手 2三→2二: hcpe bit14 → YO PSV bit15
        let rec = make_hcpe_record(&pos, 10, 10 | (11 << 7) | 0x4000, 1);
        let ConvResult::Psv(bytes) = convert_record(&rec) else {
            panic!("promote convert should succeed");
        };
        let psv = PackedSfenValue::from_bytes(&bytes).unwrap();
        assert_eq!(psv.move16, 10 | (11 << 7) | 0x8000, "成りは bit15");
    }

    #[test]
    fn convert_record_classifies_broken_records() {
        let mut pos = Position::new();
        pos.set_hirate();

        // bestMove16 == 0 → NoBestmove（壊れた指し手とは別カウント）
        let rec = make_hcpe_record(&pos, 0, 0, 1);
        assert!(matches!(convert_record(&rec), ConvResult::NoBestmove));

        // 不正な駒打ち駒種 (from=88) → MoveError
        let rec = make_hcpe_record(&pos, 0, 40 | (88 << 7), 1);
        assert!(matches!(convert_record(&rec), ConvResult::MoveError));

        // gameResult 不正 → ResultError
        let rec = make_hcpe_record(&pos, 0, 59 | (60 << 7), 3);
        assert!(matches!(convert_record(&rec), ConvResult::ResultError));

        // hcp 全ゼロ（玉重複）→ DecodeError
        let rec = [0u8; HCPE_RECORD_SIZE];
        assert!(matches!(convert_record(&rec), ConvResult::DecodeError));
    }
}
