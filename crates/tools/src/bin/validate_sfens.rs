//! validate_sfens - SFEN テキストファイルの不正局面を検出・除去
//!
//! # 使用方法
//!
//! ```bash
//! # 不正局面の検出のみ（除去なし）
//! cargo run --release -p tools --bin validate_sfens -- \
//!   --input sfens.txt
//!
//! # 不正局面を除去して出力
//! cargo run --release -p tools --bin validate_sfens -- \
//!   --input sfens.txt --output sfens_clean.txt
//! ```
//!
//! # チェック項目
//!
//! 1. SFEN パースエラー（盤面・手番・手駒・手数の形式不正）
//! 2. 玉の不在（先手・後手いずれかの玉がない）
//! 3. 駒数超過（盤上＋手駒の合計が駒種ごとの上限を超過）
//! 4. 行き所のない駒（歩・香が敵陣1段目、桂が敵陣1-2段目に未成で存在）
//! 5. 二歩（同一筋に同色の未成歩が2枚以上）

use anyhow::{Context, Result};
use clap::Parser;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::PathBuf;

use rshogi_core::bitboard::{FILE_BB, RANK_BB};
use rshogi_core::position::Position;
use rshogi_core::types::{Color, PieceType};

#[derive(Parser)]
#[command(
    name = "validate_sfens",
    about = "SFEN 局面ファイルの不正局面を検出・除去"
)]
struct Cli {
    /// 入力 SFEN ファイルパス（1行1局面）
    #[arg(long)]
    input: PathBuf,

    /// 出力ファイルパス（指定時は正常局面のみ書き出し）
    #[arg(long)]
    output: Option<PathBuf>,

    /// 不正局面の詳細を表示（最大件数）
    #[arg(long, default_value_t = 100)]
    max_errors: usize,
}

/// 局面のルール違反を検出し、最初に見つかった理由を返す
fn validate_position(pos: &Position) -> Option<String> {
    // 1. 玉の存在チェック
    for color in [Color::Black, Color::White] {
        if pos.pieces_pt(PieceType::King) & pos.pieces_c(color) == Default::default() {
            let side = if color == Color::Black {
                "先手"
            } else {
                "後手"
            };
            return Some(format!("{side}の玉がない"));
        }
    }

    // 2. 駒数超過チェック（盤上＋手駒が最大枚数を超えていないか）
    // 各駒種の最大枚数: 歩18, 香4, 桂4, 銀4, 金4, 角2, 飛2, 玉2(各1)
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
        // 手駒（玉は手駒にならないので、玉以外）
        if raw_pt != PieceType::King {
            for color in [Color::Black, Color::White] {
                total += pos.hand(color).count(raw_pt);
            }
        }
        if total > max {
            return Some(format!("{name}が{total}枚（上限{max}枚）"));
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

        // 歩・香: 敵陣1段目に未成で存在してはいけない
        let dead_rank1 = if color == Color::Black {
            RANK_BB[0] // Rank1（先手から見て敵陣1段目）
        } else {
            RANK_BB[8] // Rank9（後手から見て敵陣1段目）
        };

        let pawns_on_dead = pos.pieces_pt(PieceType::Pawn) & color_bb & dead_rank1;
        if pawns_on_dead.count() > 0 {
            return Some(format!("{side}の歩が行き所のない段にある"));
        }

        let lances_on_dead = pos.pieces_pt(PieceType::Lance) & color_bb & dead_rank1;
        if lances_on_dead.count() > 0 {
            return Some(format!("{side}の香が行き所のない段にある"));
        }

        // 桂: 敵陣1-2段目に未成で存在してはいけない
        let dead_rank12 = if color == Color::Black {
            RANK_BB[0] | RANK_BB[1]
        } else {
            RANK_BB[7] | RANK_BB[8]
        };

        let knights_on_dead = pos.pieces_pt(PieceType::Knight) & color_bb & dead_rank12;
        if knights_on_dead.count() > 0 {
            return Some(format!("{side}の桂が行き所のない段にある"));
        }
    }

    // 4. 二歩チェック（同一筋に同色の未成歩が2枚以上）
    for color in [Color::Black, Color::White] {
        let color_bb = pos.pieces_c(color);
        let pawns = pos.pieces_pt(PieceType::Pawn) & color_bb;
        let side = if color == Color::Black {
            "先手"
        } else {
            "後手"
        };

        for (file_idx, file_bb) in FILE_BB.iter().enumerate() {
            let pawns_on_file = pawns & *file_bb;
            if pawns_on_file.count() >= 2 {
                return Some(format!("{side}の二歩（{file_idx}筋）"));
            }
        }
    }

    // 5. 相手の玉に王手がかかった状態（手番側でない方が王手している = 反則局面）
    let them = !pos.side_to_move();
    let their_king = pos.king_square(them);
    let checkers = pos.attackers_to_c(their_king, pos.side_to_move());
    if checkers.count() > 0 {
        return Some("手番でない側の玉に王手がかかっている".to_string());
    }

    None
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let file = std::fs::File::open(&cli.input)
        .with_context(|| format!("入力ファイルを開けません: {:?}", cli.input))?;
    let reader = BufReader::new(file);

    let mut writer = cli.output.as_ref().map(|path| {
        let f = std::fs::File::create(path).unwrap_or_else(|e| {
            panic!("出力ファイルを作成できません: {:?}: {e}", path);
        });
        BufWriter::new(f)
    });

    let mut pos = Position::new();
    let mut total = 0u64;
    let mut valid = 0u64;
    let mut invalid = 0u64;
    let mut parse_errors = 0u64;
    let mut errors_shown = 0usize;

    for (line_no, line) in reader.lines().enumerate() {
        let sfen = line.with_context(|| format!("行 {} の読み取りに失敗", line_no + 1))?;
        let sfen = sfen.trim();
        if sfen.is_empty() {
            continue;
        }
        total += 1;

        // SFEN パース
        if let Err(e) = pos.set_sfen(sfen) {
            parse_errors += 1;
            invalid += 1;
            if errors_shown < cli.max_errors {
                eprintln!("[行{}] パースエラー: {} | {}", line_no + 1, e, sfen);
                errors_shown += 1;
            }
            continue;
        }

        // ルール違反チェック
        if let Some(reason) = validate_position(&pos) {
            invalid += 1;
            if errors_shown < cli.max_errors {
                eprintln!("[行{}] {}: {}", line_no + 1, reason, sfen);
                errors_shown += 1;
            }
            continue;
        }

        valid += 1;
        if let Some(ref mut w) = writer {
            writeln!(w, "{sfen}")?;
        }
    }

    eprintln!();
    eprintln!("=== 検証結果 ===");
    eprintln!("総行数:     {total}");
    eprintln!("正常:       {valid}");
    eprintln!("不正:       {invalid} (パースエラー: {parse_errors})");
    if let Some(ref path) = cli.output {
        eprintln!("出力先:     {}", path.display());
    }

    Ok(())
}
