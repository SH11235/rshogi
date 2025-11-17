use anyhow::{bail, Context, Result};
use engine_core::shogi::board::{Color, PieceType, Square};
use engine_core::shogi::Position;
use engine_core::usi::{parse_sfen, parse_usi_move};
use serde::Deserialize;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;

#[derive(Debug, Deserialize)]
struct EngineNames {
    black: String,
    white: String,
}

#[derive(Debug, Deserialize)]
struct ThinkMs {
    black: Option<u64>,
    white: Option<u64>,
}

#[derive(Debug, Deserialize, Clone)]
struct EvalLog {
    #[serde(default)]
    score_cp: Option<i32>,
    #[serde(default)]
    score_mate: Option<i32>,
    #[serde(default)]
    depth: Option<u32>,
    #[serde(default)]
    seldepth: Option<u32>,
    #[serde(default)]
    nodes: Option<u64>,
    #[serde(default)]
    time_ms: Option<u64>,
    #[serde(default)]
    nps: Option<u64>,
    #[serde(default)]
    pv: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct MetaEntry {
    #[serde(default)]
    engine_names: Option<EngineNames>,
    #[serde(default)]
    start_sfen: Option<String>,
    #[serde(default)]
    think_ms: Option<ThinkMs>,
    #[serde(default)]
    timestamp: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MoveEntry {
    game_id: u32,
    ply: u32,
    sfen_before: Option<String>,
    move_usi: String,
    #[serde(default)]
    main_eval: Option<EvalLog>,
    #[serde(default)]
    basic_eval: Option<EvalLog>,
    #[serde(default)]
    result: Option<String>,
}

pub fn convert_jsonl_to_kif(input: &Path, output: &Path) -> Result<()> {
    let reader = BufReader::new(
        File::open(input).with_context(|| format!("failed to open input {}", input.display()))?,
    );

    let mut meta: Option<MetaEntry> = None;
    let mut games: Vec<(u32, Vec<MoveEntry>)> = Vec::new();
    let mut current_game_id: Option<u32> = None;
    let mut current_moves: Vec<MoveEntry> = Vec::new();

    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(trimmed)
            .with_context(|| format!("failed to parse JSON line: {}", trimmed))?;
        if value.get("type").and_then(|v| v.as_str()) == Some("meta") {
            meta = Some(serde_json::from_value(value)?);
            continue;
        }
        let entry: MoveEntry = serde_json::from_value(value)?;
        if current_game_id != Some(entry.game_id) {
            if let Some(id) = current_game_id {
                games.push((id, std::mem::take(&mut current_moves)));
            }
            current_game_id = Some(entry.game_id);
        }
        current_moves.push(entry);
        if current_moves.last().and_then(|m| m.result.as_deref()).is_some() {
            if let Some(id) = current_game_id.take() {
                games.push((id, std::mem::take(&mut current_moves)));
            }
        }
    }

    if let Some(id) = current_game_id {
        if !current_moves.is_empty() {
            games.push((id, current_moves));
        } else if games.iter().all(|(gid, _)| *gid != id) {
            bail!("no moves found for game_id {}", id);
        }
    }

    if games.is_empty() {
        bail!("no move entries found in {}", input.display());
    }

    let mut writer = BufWriter::new(
        File::create(output).with_context(|| format!("failed to create {}", output.display()))?,
    );

    for (idx, (_, moves)) in games.iter().enumerate() {
        if idx > 0 {
            writeln!(writer)?;
        }
        export_game(&mut writer, meta.as_ref(), moves)?;
    }
    writer.flush()?;
    Ok(())
}

fn export_game<W: Write>(
    writer: &mut W,
    meta: Option<&MetaEntry>,
    moves: &[MoveEntry],
) -> Result<()> {
    let start_sfen_str = meta
        .and_then(|m| m.start_sfen.clone())
        .or_else(|| moves.first().and_then(|m| m.sfen_before.clone()))
        .unwrap_or_else(|| {
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1".to_string()
        });

    let mut pos = parse_sfen(&start_sfen_str).unwrap_or_else(|_| Position::startpos());
    let engine_names = meta
        .and_then(|m| m.engine_names.as_ref())
        .map(|names| (names.black.clone(), names.white.clone()))
        .unwrap_or_else(|| ("先手".to_string(), "後手".to_string()));
    let think_ms = meta
        .and_then(|m| m.think_ms.as_ref())
        .map(|t| (t.black.unwrap_or(0), t.white.unwrap_or(0)))
        .unwrap_or((0, 0));

    writeln!(
        writer,
        "開始日時：{}",
        meta.and_then(|m| m.timestamp.clone()).unwrap_or_else(|| "-".to_string())
    )?;
    writeln!(writer, "手合割：平手")?;
    writeln!(writer, "先手：{}", engine_names.0)?;
    writeln!(writer, "後手：{}", engine_names.1)?;
    writeln!(writer, "持ち時間：先手{}ms / 後手{}ms", think_ms.0, think_ms.1)?;
    writeln!(writer, "開始局面：{}", start_sfen_str)?;
    writeln!(writer, "手数----指手---------消費時間--")?;

    let mut last_result: Option<String> = None;

    for entry in moves {
        if entry.move_usi == "resign" || entry.move_usi == "win" {
            last_result = entry.result.clone();
            break;
        }
        for line in format_eval_comments(entry.main_eval.as_ref(), &pos) {
            writeln!(writer, "{}", line)?;
        }
        for line in format_eval_comments(entry.basic_eval.as_ref(), &pos) {
            writeln!(writer, "{}", line)?;
        }
        let mv = parse_usi_move(&entry.move_usi)?;
        let line = format_move(entry.ply, &pos, mv);
        writeln!(writer, "{}", line)?;
        pos.do_move(mv);
        if entry.result.is_some() {
            last_result = entry.result.clone();
        }
    }

    let final_ply = moves.last().map(|m| m.ply).unwrap_or(0);
    let summary = match last_result.as_deref().unwrap_or("draw") {
        "black_win" => format!("まで{}手で先手の勝ち", final_ply),
        "white_win" => format!("まで{}手で後手の勝ち", final_ply),
        _ => format!("まで{}手で引き分け", final_ply),
    };
    writeln!(writer, "\n{}", summary)?;
    Ok(())
}

fn format_move(ply: u32, pos: &Position, mv: engine_core::shogi::Move) -> String {
    let prefix = if pos.side_to_move == Color::Black {
        "▲"
    } else {
        "△"
    };
    let dest = square_label(mv.to());
    let label = if mv.is_drop() {
        format!("{}打", piece_label(mv.drop_piece_type(), false))
    } else {
        let from = mv.from().expect("normal move must have source");
        let piece = pos.piece_at(from).expect("source must have piece");
        let promoted = mv.is_promote() || piece.promoted;
        piece_label(piece.piece_type, promoted).to_string()
    };
    let from_suffix = if let Some(from) = mv.from() {
        format!("({}{})", square_file_digit(from), square_rank_digit(from))
    } else {
        String::new()
    };
    format!("{:>4} {}{}{}{}", ply, prefix, dest, label, from_suffix)
}

fn square_label(sq: Square) -> String {
    format!("{}{}", file_kanji(sq), rank_kanji(sq))
}

fn file_kanji(sq: Square) -> &'static str {
    const FILES: [&str; 10] = ["", "１", "２", "３", "４", "５", "６", "７", "８", "９"];
    let file = sq.to_string().chars().next().and_then(|c| c.to_digit(10)).unwrap_or(1);
    FILES[file as usize]
}

fn rank_kanji(sq: Square) -> &'static str {
    const RANKS: [&str; 9] = ["一", "二", "三", "四", "五", "六", "七", "八", "九"];
    let rank_char = sq.to_string().chars().nth(1).unwrap_or('a');
    let idx = (rank_char as u8 - b'a') as usize;
    RANKS[idx]
}

fn square_file_digit(sq: Square) -> char {
    sq.to_string().chars().next().unwrap_or('0')
}

fn square_rank_digit(sq: Square) -> char {
    let rank_char = sq.to_string().chars().nth(1).unwrap_or('a');
    let idx = (rank_char as u8 - b'a') as u32 + 1;
    char::from_digit(idx, 10).unwrap_or('0')
}

fn format_eval_comments(eval: Option<&EvalLog>, pos: &Position) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(eval) = eval {
        if let Some(mate) = eval.score_mate {
            lines.push(format_mate_line(mate));
        } else if let Some(score) = eval.score_cp {
            lines.push(format!("**評価値={:+}", score));
        }
        if let Some(text) = format_pv_text(pos, eval.pv.as_ref()) {
            lines.push(format!("**読み筋={}", text));
        }
        if let Some(depth) = eval.depth {
            lines.push(format!("**深さ={}", depth));
        }
        if let Some(seldepth) = eval.seldepth {
            lines.push(format!("**選択深さ={}", seldepth));
        }
        if let Some(nodes) = eval.nodes {
            lines.push(format!("**ノード数={}", nodes));
        }
        if let Some(time_ms) = eval.time_ms {
            lines.push(format!("**探索時間={}ms", time_ms));
        }
        if let Some(nps) = eval.nps {
            lines.push(format!("**NPS={}", nps));
        }
    }
    lines
}

fn format_mate_line(mate: i32) -> String {
    let winner = if mate > 0 {
        "先手勝ち"
    } else {
        "後手勝ち"
    };
    let turns = mate.abs();
    format!("**詰み={}:{turns}手", winner)
}

fn format_pv_text(pos: &Position, pv: Option<&Vec<String>>) -> Option<String> {
    let pv = pv?;
    if pv.is_empty() {
        return None;
    }
    let mut tmp = pos.clone();
    let mut parts = Vec::new();
    for usi in pv {
        let mv = parse_usi_move(usi).ok()?;
        parts.push(move_to_text(&tmp, mv));
        tmp.do_move(mv);
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

fn move_to_text(pos: &Position, mv: engine_core::shogi::Move) -> String {
    let prefix = if pos.side_to_move == Color::Black {
        "▲"
    } else {
        "△"
    };
    let dest = square_label(mv.to());
    let label = if mv.is_drop() {
        format!("{}打", piece_label(mv.drop_piece_type(), false))
    } else {
        let from = mv.from().expect("normal move must have source");
        let piece = pos.piece_at(from).expect("source must have piece");
        let promoted = mv.is_promote() || piece.promoted;
        piece_label(piece.piece_type, promoted).to_string()
    };
    format!("{}{}{}", prefix, dest, label)
}

fn piece_label(piece_type: PieceType, promoted: bool) -> &'static str {
    match (piece_type, promoted) {
        (PieceType::Pawn, false) => "歩",
        (PieceType::Pawn, true) => "と",
        (PieceType::Lance, false) => "香",
        (PieceType::Lance, true) => "成香",
        (PieceType::Knight, false) => "桂",
        (PieceType::Knight, true) => "成桂",
        (PieceType::Silver, false) => "銀",
        (PieceType::Silver, true) => "成銀",
        (PieceType::Gold, _) => "金",
        (PieceType::Bishop, false) => "角",
        (PieceType::Bishop, true) => "馬",
        (PieceType::Rook, false) => "飛",
        (PieceType::Rook, true) => "龍",
        (PieceType::King, _) => "玉",
    }
}
