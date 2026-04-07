/// .pack（GenSfen可変長対局棋譜）→ PSV（PackedSfenValue 40byte固定長）変換ツール
///
/// GenSfen の .pack 形式は対局単位の可変長バイナリ。各対局の全局面を
/// PSV レコードとして展開し、NNUE 学習用の教師データとして出力する。
///
/// # .pack フォーマット
///
/// 各対局:
///   [開始局面フラグ: u8] — 1=平手, 0=任意局面
///   0 の場合: [HuffmanCodedPos: 32byte][game_ply: u16]
///   繰り返し: [move: u16][eval: i16]
///   [終局マーカー: u16 (from==to)] [終局理由: u8]
///
/// # 使用例
///
/// ```bash
/// # 単一ファイル
/// cargo run --release -p tools --bin pack_to_psv -- \
///   --input data.pack --output train.psv
///
/// # ディレクトリ内の .pack を一括変換（ファイル単位で並列処理）
/// cargo run --release -p tools --bin pack_to_psv -- \
///   --input-dir data/suisho11a_50k_nodes --output train.psv
///
/// # サブディレクトリ再帰
/// cargo run --release -p tools --bin pack_to_psv -- \
///   --input-dir data/suisho11ab_50k_100k_nodes --output train.psv
/// ```
use std::fs::File;
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use rshogi_core::movegen::{MoveList, generate_legal_all};
use rshogi_core::position::Position;
use rshogi_core::types::Color;
use tools::common::dedup::collect_input_paths;
use tools::packed_sfen::{PackedSfenValue, cshogi_move16_to_move, pack_position};

#[derive(Parser, Debug)]
#[command(name = "pack_to_psv")]
struct Args {
    /// 入力 .pack ファイル（カンマ区切りで複数可）。--input-dir と排他
    #[arg(long)]
    input: Option<String>,

    /// 入力ディレクトリ。--pattern と組み合わせて使用。--input と排他
    #[arg(long)]
    input_dir: Option<PathBuf>,

    /// --input-dir 使用時の glob パターン
    #[arg(long, default_value = "*.pack")]
    pattern: String,

    /// 出力ファイルパス（PSV形式）
    #[arg(long)]
    output: PathBuf,

    /// 処理する最大対局数（0 = 全件）
    #[arg(long, default_value = "0")]
    max_games: u64,
}

/// .pack ファイルからバイトを読み取るカーソル
struct PackReader {
    data: Vec<u8>,
    pos: usize,
}

impl PackReader {
    fn new(data: Vec<u8>) -> Self {
        Self { data, pos: 0 }
    }

    fn eof(&self) -> bool {
        self.pos >= self.data.len()
    }

    fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    fn read_u8(&mut self) -> io::Result<u8> {
        if self.remaining() < 1 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "read_u8"));
        }
        let v = self.data[self.pos];
        self.pos += 1;
        Ok(v)
    }

    fn read_u16(&mut self) -> io::Result<u16> {
        if self.remaining() < 2 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "read_u16"));
        }
        let v = u16::from_le_bytes([self.data[self.pos], self.data[self.pos + 1]]);
        self.pos += 2;
        Ok(v)
    }

    fn read_i16(&mut self) -> io::Result<i16> {
        if self.remaining() < 2 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "read_i16"));
        }
        let v = i16::from_le_bytes([self.data[self.pos], self.data[self.pos + 1]]);
        self.pos += 2;
        Ok(v)
    }

    fn read_bytes(&mut self, n: usize) -> io::Result<&[u8]> {
        if self.remaining() < n {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "read_bytes"));
        }
        let slice = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(slice)
    }
}

/// 終局マーカーの判定: from と to が同じマス
fn is_end_marker(move16: u16) -> bool {
    let to = move16 & 0x7F;
    let from = (move16 >> 7) & 0x7F;
    to == from
}

/// .pack の game_result (0=draw, 1=black_win, 2=white_win) を
/// PSV の game_result (per-STM: 1=win, -1=loss, 0=draw) に変換
fn convert_game_result(pack_result: u8, stm: Color) -> i8 {
    match pack_result {
        0 => 0, // draw
        1 => {
            // black_win
            if stm == Color::Black { 1 } else { -1 }
        }
        2 => {
            // white_win
            if stm == Color::White { 1 } else { -1 }
        }
        _ => 0, // unknown → draw
    }
}

#[derive(Default)]
struct FileStats {
    games: u64,
    positions: u64,
    move_errors: u64,
    file_errors: u64,
}

/// 1対局分を読み取り、PSV レコードをバッファに書き出す
fn process_game(
    reader: &mut PackReader,
    output: &mut Vec<u8>,
    stats: &mut FileStats,
) -> io::Result<()> {
    let start_flag = reader.read_u8()?;

    let mut pos = Position::new();

    match start_flag {
        1 => {
            pos.set_hirate();
        }
        0 => {
            let mut hcp = [0u8; 32];
            hcp.copy_from_slice(reader.read_bytes(32)?);
            let game_ply = reader.read_u16()?;

            let sfen = tools::packed_sfen::unpack_hcp(&hcp).map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("HuffmanCodedPos デコード失敗: {e}"),
                )
            })?;

            let sfen_with_ply = format!(
                "{} {}",
                sfen.rsplit_once(' ').map(|(prefix, _)| prefix).unwrap_or(&sfen),
                game_ply
            );

            pos.set_sfen(&sfen_with_ply).map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("SFEN パース失敗 ({sfen_with_ply}): {e}"),
                )
            })?;
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("不明な開始局面フラグ: {start_flag}"),
            ));
        }
    }

    let mut game_moves: Vec<(u16, i16)> = Vec::new();
    let pack_game_result: u8;

    loop {
        let move16 = reader.read_u16()?;

        if is_end_marker(move16) {
            pack_game_result = (move16 & 0x7F) as u8;
            // 終局理由 (u8) はフォーマット上存在するが PSV には不要のため読み飛ばす
            let _reason = reader.read_u8()?;
            break;
        }

        let eval = reader.read_i16()?;
        game_moves.push((move16, eval));
    }

    for &(move16, eval) in &game_moves {
        let stm = pos.side_to_move();
        let game_ply = pos.game_ply() as u16;
        let game_result = convert_game_result(pack_game_result, stm);

        let packed_sfen = pack_position(&pos);

        let psv = PackedSfenValue {
            sfen: packed_sfen,
            score: eval,
            move16,
            game_ply,
            game_result,
            padding: 0,
        };

        output.extend_from_slice(&psv.to_bytes());
        stats.positions += 1;

        let target_mv = cshogi_move16_to_move(move16);
        if target_mv.is_none() {
            stats.move_errors += 1;
            return Ok(());
        }

        let mut legal_moves = MoveList::new();
        generate_legal_all(&pos, &mut legal_moves);

        let mv = legal_moves
            .iter()
            .find(|m| {
                m.to() == target_mv.to()
                    && m.is_drop() == target_mv.is_drop()
                    && m.is_promote() == target_mv.is_promote()
                    && if target_mv.is_drop() {
                        m.drop_piece_type() == target_mv.drop_piece_type()
                    } else {
                        m.from() == target_mv.from()
                    }
            })
            .copied();

        let mv = match mv {
            Some(m) => m,
            None => {
                stats.move_errors += 1;
                return Ok(());
            }
        };

        let gives_check = pos.gives_check(mv);
        pos.do_move(mv, gives_check);
    }

    stats.games += 1;
    Ok(())
}

/// 1ファイル全体を処理し、PSV バイト列と統計を返す
fn process_file(path: &Path) -> io::Result<(Vec<u8>, FileStats)> {
    let mut data = Vec::new();
    File::open(path)?.read_to_end(&mut data)?;

    let mut reader = PackReader::new(data);
    let mut output = Vec::new();
    let mut stats = FileStats::default();

    while !reader.eof() {
        if let Err(e) = process_game(&mut reader, &mut output, &mut stats) {
            eprintln!("  対局パースエラー ({}): {e}", path.display());
            stats.file_errors += 1;
            break;
        }
    }

    Ok((output, stats))
}

/// 1ファイルをストリーミング処理（max_games 制限あり用）
fn process_file_streaming(
    path: &Path,
    writer: &mut BufWriter<File>,
    max_games: u64,
    current_games: &mut u64,
) -> io::Result<FileStats> {
    eprintln!("Reading: {}", path.display());

    let mut data = Vec::new();
    File::open(path)?.read_to_end(&mut data)?;

    let mut reader = PackReader::new(data);
    let mut output = Vec::new();
    let mut stats = FileStats::default();
    let start = std::time::Instant::now();

    while !reader.eof() {
        if max_games > 0 && *current_games >= max_games {
            break;
        }

        output.clear();
        if let Err(e) = process_game(&mut reader, &mut output, &mut stats) {
            eprintln!("  対局パースエラー (game {}): {e}", *current_games + stats.file_errors + 1);
            stats.file_errors += 1;
            break;
        }

        if !output.is_empty() {
            writer.write_all(&output)?;
        }

        // stats.games が増えた場合のみカウント（move_errors で早期終了した対局は数えない）
        *current_games = stats.games;

        if stats.games.is_multiple_of(10000) && stats.games > 0 {
            let elapsed = start.elapsed().as_secs_f64();
            eprintln!("  {} games, {} positions, {:.1} sec", stats.games, stats.positions, elapsed,);
        }
    }

    Ok(stats)
}

fn main() -> io::Result<()> {
    let args = Args::parse();

    let paths = collect_input_paths(args.input.as_deref(), args.input_dir.as_ref(), &args.pattern)?;
    if paths.is_empty() {
        eprintln!("入力ファイルが見つかりません");
        return Ok(());
    }

    // 入出力の衝突チェック
    let output_canonical = args.output.canonicalize().ok();
    for p in &paths {
        if let Ok(c) = p.canonicalize()
            && Some(&c) == output_canonical.as_ref()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("出力ファイルが入力ファイルと同一です: {}", p.display()),
            ));
        }
    }

    let start = std::time::Instant::now();

    let out_file = File::create(&args.output)?;
    let mut writer = BufWriter::with_capacity(8 * 1024 * 1024, out_file);

    let mut total_games = 0u64;
    let mut total_positions = 0u64;
    let mut total_move_errors = 0u64;
    let mut total_file_errors = 0u64;

    if args.max_games > 0 {
        // max_games 指定時: ストリーミング逐次処理（早期終了とメモリ制限のため）
        eprintln!(
            "Processing {} files (max_games={}, sequential mode)...",
            paths.len(),
            args.max_games
        );
        let mut current_games = 0u64;
        for path in &paths {
            if current_games >= args.max_games {
                break;
            }
            let stats =
                process_file_streaming(path, &mut writer, args.max_games, &mut current_games)?;
            total_games += stats.games;
            total_positions += stats.positions;
            total_move_errors += stats.move_errors;
            total_file_errors += stats.file_errors;
        }
    } else {
        // 全件処理: ファイル単位で rayon 並列
        eprintln!("Processing {} files (parallel mode)...", paths.len());
        let progress = ProgressBar::new(paths.len() as u64);
        progress.set_style(
            ProgressStyle::default_bar()
                .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} files")
                .expect("valid template"),
        );

        let results: Vec<io::Result<(Vec<u8>, FileStats)>> =
            paths.par_iter().map(|path| process_file(path)).collect();

        for result in results {
            let (output, stats) = result?;
            if !output.is_empty() {
                writer.write_all(&output)?;
            }
            total_games += stats.games;
            total_positions += stats.positions;
            total_move_errors += stats.move_errors;
            total_file_errors += stats.file_errors;
            progress.inc(1);
        }
        progress.finish();
    }

    writer.flush()?;

    let elapsed = start.elapsed().as_secs_f64();
    println!("=== Pack → PSV Summary ===");
    println!("Input files:     {}", paths.len());
    println!("Games:           {total_games}");
    println!("Positions:       {total_positions}");
    if total_move_errors > 0 {
        println!("Move errors:     {total_move_errors}");
    }
    if total_file_errors > 0 {
        println!("File errors:     {total_file_errors}");
    }
    println!("Output file:     {}", args.output.display());
    println!(
        "Output size:     {:.1} MB",
        (total_positions * PackedSfenValue::SIZE as u64) as f64 / (1024.0 * 1024.0)
    );
    println!("Elapsed:         {:.1} sec", elapsed);

    Ok(())
}
