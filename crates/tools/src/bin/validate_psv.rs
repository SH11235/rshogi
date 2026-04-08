//! validate_psv - PSV バイナリファイルの不正局面を検出・除去
//!
//! PackedSfenValue 形式（40バイト/レコード）の PSV ファイルを読み込み、
//! 不正局面を検出して統計を出力する。`--output` 指定時は正常レコードのみ書き出す。
//!
//! # 使用方法
//!
//! ```bash
//! # 不正局面の検出のみ（除去なし）
//! cargo run --release -p tools --bin validate_psv -- \
//!   --data /path/to/data.psv
//!
//! # ディレクトリ指定
//! cargo run --release -p tools --bin validate_psv -- \
//!   --input-dir /path/to/dir --pattern "*.bin"
//!
//! # 不正レコードを除去して出力
//! cargo run --release -p tools --bin validate_psv -- \
//!   --data /path/to/data.psv --output /path/to/clean.psv
//! ```
//!
//! # チェック項目
//!
//! 1. ファイルサイズが 40 バイトの倍数でない（末尾端数）
//! 2. PackedSfen の unpack 失敗（ハフマン符号破損等）
//! 3. SFEN パースエラー（Position::set_sfen 失敗）
//! 4. 玉の不在（先手・後手いずれかの玉がない）
//! 5. 駒数超過（盤上＋手駒の合計が駒種ごとの上限を超過）
//! 6. 行き所のない駒（歩・香が敵陣1段目、桂が敵陣1-2段目に未成で存在）
//! 7. 二歩（同一筋に同色の未成歩が2枚以上）
//! 8. 手番でない側の玉に王手がかかっている
//! 9. game_result が {-1, 0, 1} 以外

use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;

use rshogi_core::bitboard::{FILE_BB, RANK_BB};
use rshogi_core::position::Position;
use rshogi_core::types::{Color, PieceType};
use tools::common::dedup::{PSV_SIZE, SFEN_SIZE, collect_input_paths};
use tools::packed_sfen::{PackedSfenValue, unpack_sfen};

#[derive(Parser)]
#[command(
    name = "validate_psv",
    about = "PSV バイナリファイルの不正局面を検出・除去"
)]
struct Cli {
    /// PSV ファイル（カンマ区切りで複数指定可）
    #[arg(long)]
    data: Option<String>,

    /// 入力ディレクトリ。--pattern と組み合わせて使用。--data と排他
    #[arg(long)]
    input_dir: Option<PathBuf>,

    /// --input-dir 使用時の glob パターン
    #[arg(long, default_value = "*.bin")]
    pattern: String,

    /// 出力ファイルパス（指定時は正常レコードのみ書き出し）
    #[arg(long)]
    output: Option<PathBuf>,

    /// 不正レコードの詳細を表示する最大件数
    #[arg(long, default_value_t = 100)]
    max_errors: usize,
}

/// 不正理由の分類
#[derive(Default)]
struct ErrorStats {
    unpack_failed: u64,
    parse_failed: u64,
    no_king: u64,
    piece_overflow: u64,
    dead_piece: u64,
    double_pawn: u64,
    enemy_in_check: u64,
    bad_game_result: u64,
}

impl ErrorStats {
    fn total(&self) -> u64 {
        self.unpack_failed
            + self.parse_failed
            + self.no_king
            + self.piece_overflow
            + self.dead_piece
            + self.double_pawn
            + self.enemy_in_check
            + self.bad_game_result
    }
}

/// 局面のルール違反を検出し、最初に見つかった理由を返す
fn validate_position(pos: &Position) -> Option<(&'static str, String)> {
    // 1. 玉の存在チェック
    for color in [Color::Black, Color::White] {
        if pos.pieces_pt(PieceType::King) & pos.pieces_c(color) == Default::default() {
            let side = if color == Color::Black {
                "先手"
            } else {
                "後手"
            };
            return Some(("no_king", format!("{side}の玉がない")));
        }
    }

    // 2. 駒数超過チェック
    let piece_limits: &[(PieceType, Option<PieceType>, u32, &str)] = &[
        (PieceType::Pawn, Some(PieceType::ProPawn), 18, "歩"),
        (PieceType::Lance, Some(PieceType::ProLance), 4, "香"),
        (PieceType::Knight, Some(PieceType::ProKnight), 4, "桂"),
        (PieceType::Silver, Some(PieceType::ProSilver), 4, "銀"),
        (PieceType::Gold, None, 4, "金"),
        (PieceType::Bishop, Some(PieceType::Horse), 2, "角"),
        (PieceType::Rook, Some(PieceType::Dragon), 2, "飛"),
        (PieceType::King, None, 2, "玉"),
    ];

    for &(raw_pt, promoted_pt, max, name) in piece_limits {
        let mut total = pos.pieces_pt(raw_pt).count();
        if let Some(ppt) = promoted_pt {
            total += pos.pieces_pt(ppt).count();
        }
        if raw_pt != PieceType::King {
            for color in [Color::Black, Color::White] {
                total += pos.hand(color).count(raw_pt);
            }
        }
        if total > max {
            return Some(("piece_overflow", format!("{name}が{total}枚（上限{max}枚）")));
        }
    }

    // 3. 行き所のない駒チェック
    for color in [Color::Black, Color::White] {
        let color_bb = pos.pieces_c(color);
        let side = if color == Color::Black {
            "先手"
        } else {
            "後手"
        };

        let dead_rank1 = if color == Color::Black {
            RANK_BB[0]
        } else {
            RANK_BB[8]
        };

        if (pos.pieces_pt(PieceType::Pawn) & color_bb & dead_rank1).count() > 0 {
            return Some(("dead_piece", format!("{side}の歩が行き所のない段にある")));
        }
        if (pos.pieces_pt(PieceType::Lance) & color_bb & dead_rank1).count() > 0 {
            return Some(("dead_piece", format!("{side}の香が行き所のない段にある")));
        }

        let dead_rank12 = if color == Color::Black {
            RANK_BB[0] | RANK_BB[1]
        } else {
            RANK_BB[7] | RANK_BB[8]
        };

        if (pos.pieces_pt(PieceType::Knight) & color_bb & dead_rank12).count() > 0 {
            return Some(("dead_piece", format!("{side}の桂が行き所のない段にある")));
        }
    }

    // 4. 二歩チェック
    for color in [Color::Black, Color::White] {
        let pawns = pos.pieces_pt(PieceType::Pawn) & pos.pieces_c(color);
        let side = if color == Color::Black {
            "先手"
        } else {
            "後手"
        };

        for (file_idx, file_bb) in FILE_BB.iter().enumerate() {
            if (pawns & *file_bb).count() >= 2 {
                return Some(("double_pawn", format!("{side}の二歩（{file_idx}筋）")));
            }
        }
    }

    // 5. 相手の玉に王手がかかった状態
    let them = !pos.side_to_move();
    let their_king = pos.king_square(them);
    let checkers = pos.attackers_to_c(their_king, pos.side_to_move());
    if checkers.count() > 0 {
        return Some(("enemy_in_check", "手番でない側の玉に王手がかかっている".to_string()));
    }

    None
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let paths = collect_input_paths(cli.data.as_deref(), cli.input_dir.as_ref(), &cli.pattern)
        .context("入力パスの収集に失敗")?;

    if paths.is_empty() {
        eprintln!("入力ファイルが見つかりません");
        return Ok(());
    }

    eprintln!("{} ファイルを処理します", paths.len());

    let mut writer = cli.output.as_ref().map(|path| {
        let f = File::create(path).unwrap_or_else(|e| {
            panic!("出力ファイルを作成できません: {:?}: {e}", path);
        });
        BufWriter::new(f)
    });

    let mut pos = Position::new();
    let mut total_records = 0u64;
    let mut valid_records = 0u64;
    let mut trailing_bytes_total = 0u64;
    let mut errors = ErrorStats::default();
    let mut errors_shown = 0usize;
    let mut buf = [0u8; PSV_SIZE];

    let start = std::time::Instant::now();

    for path in &paths {
        // ファイルサイズチェック
        let file_size = std::fs::metadata(path)
            .with_context(|| format!("ファイル情報の取得に失敗: {}", path.display()))?
            .len();
        let trailing = file_size % PSV_SIZE as u64;
        if trailing != 0 {
            eprintln!(
                "Warning: {} のサイズが40バイトの倍数ではない（末尾 {} バイト余り）",
                path.display(),
                trailing
            );
            trailing_bytes_total += trailing;
        }

        eprintln!("Reading: {}", path.display());
        let file = File::open(path)
            .with_context(|| format!("ファイルを開けません: {}", path.display()))?;
        let mut reader = BufReader::with_capacity(1 << 20, file);

        loop {
            match reader.read_exact(&mut buf) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e).context("読み込みエラー"),
            }

            total_records += 1;

            // PSV メタデータチェック
            let psv = PackedSfenValue::from_bytes(&buf).unwrap();
            if !(-1..=1).contains(&psv.game_result) {
                errors.bad_game_result += 1;
                if errors_shown < cli.max_errors {
                    eprintln!("[レコード#{total_records}] game_result が不正: {}", psv.game_result);
                    errors_shown += 1;
                }
                continue;
            }

            // PackedSfen → SFEN 文字列
            let sfen_bytes: &[u8; SFEN_SIZE] = buf[..SFEN_SIZE].try_into().unwrap();
            let sfen = match unpack_sfen(sfen_bytes) {
                Ok(s) => s,
                Err(e) => {
                    errors.unpack_failed += 1;
                    if errors_shown < cli.max_errors {
                        eprintln!("[レコード#{total_records}] unpack 失敗: {e}");
                        errors_shown += 1;
                    }
                    continue;
                }
            };

            // SFEN → Position
            if let Err(e) = pos.set_sfen(&sfen) {
                errors.parse_failed += 1;
                if errors_shown < cli.max_errors {
                    eprintln!("[レコード#{total_records}] パースエラー: {e} | {sfen}");
                    errors_shown += 1;
                }
                continue;
            }

            // ルール違反チェック
            if let Some((category, reason)) = validate_position(&pos) {
                match category {
                    "no_king" => errors.no_king += 1,
                    "piece_overflow" => errors.piece_overflow += 1,
                    "dead_piece" => errors.dead_piece += 1,
                    "double_pawn" => errors.double_pawn += 1,
                    "enemy_in_check" => errors.enemy_in_check += 1,
                    _ => {}
                }
                if errors_shown < cli.max_errors {
                    eprintln!("[レコード#{total_records}] {reason} | {sfen}");
                    errors_shown += 1;
                }
                continue;
            }

            valid_records += 1;
            if let Some(ref mut w) = writer {
                w.write_all(&buf)?;
            }

            if total_records.is_multiple_of(10_000_000) {
                let elapsed = start.elapsed().as_secs_f64();
                eprintln!("  {} M レコード処理, {:.1} sec", total_records / 1_000_000, elapsed,);
            }
        }
    }

    let elapsed = start.elapsed().as_secs_f64();
    let invalid = errors.total();

    eprintln!();
    println!("=== PSV 検証結果 ===");
    println!("総レコード数:       {total_records}");
    println!(
        "正常:               {valid_records} ({:.2}%)",
        100.0 * valid_records as f64 / total_records.max(1) as f64
    );
    println!(
        "不正:               {invalid} ({:.2}%)",
        100.0 * invalid as f64 / total_records.max(1) as f64
    );

    if invalid > 0 {
        println!();
        println!("--- 不正の内訳 ---");
        if errors.unpack_failed > 0 {
            println!("  unpack 失敗:      {}", errors.unpack_failed);
        }
        if errors.parse_failed > 0 {
            println!("  パースエラー:     {}", errors.parse_failed);
        }
        if errors.no_king > 0 {
            println!("  玉不在:           {}", errors.no_king);
        }
        if errors.piece_overflow > 0 {
            println!("  駒数超過:         {}", errors.piece_overflow);
        }
        if errors.dead_piece > 0 {
            println!("  行き所のない駒:   {}", errors.dead_piece);
        }
        if errors.double_pawn > 0 {
            println!("  二歩:             {}", errors.double_pawn);
        }
        if errors.enemy_in_check > 0 {
            println!("  相手玉に王手:     {}", errors.enemy_in_check);
        }
        if errors.bad_game_result > 0 {
            println!("  game_result 不正: {}", errors.bad_game_result);
        }
    }

    if trailing_bytes_total > 0 {
        println!();
        println!("末尾端数バイト:     {trailing_bytes_total}");
    }

    println!();
    println!("処理時間:           {elapsed:.1} sec");
    if let Some(ref path) = cli.output {
        println!("出力先:             {}", path.display());
        println!("出力レコード数:     {valid_records}");
    }

    Ok(())
}
