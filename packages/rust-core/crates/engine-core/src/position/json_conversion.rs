use crate::eval::material::compute_material_value;
use crate::movegen::{generate_legal, MoveList};
use crate::types::json::{
    BoardStateJson, CellJson, HandJson, HandsJson, PieceJson, ReplayResultJson,
};
use crate::types::{Color, File, Hand, Move, Piece, PieceType, Rank, Square};

use super::{Position, SFEN_HIRATE};

impl Position {
    /// 平手初期局面をJSON形式で取得する。
    pub fn initial_board_json() -> BoardStateJson {
        let mut pos = Position::new();
        pos.set_hirate();
        pos.to_board_state_json()
    }

    /// 現在の盤面をJSON形式に変換する。
    pub fn to_board_state_json(&self) -> BoardStateJson {
        let mut cells: Vec<Vec<CellJson>> = Vec::with_capacity(9);
        for rank in Rank::ALL {
            let mut row: Vec<CellJson> = Vec::with_capacity(9);
            for file in File::ALL {
                let sq = Square::new(file, rank);
                row.push(CellJson {
                    square: sq.to_usi(),
                    piece: piece_to_json(self.piece_on(sq)),
                });
            }
            cells.push(row);
        }

        BoardStateJson {
            cells,
            hands: HandsJson {
                sente: hand_to_json(self.hand[Color::Black.index()]),
                gote: hand_to_json(self.hand[Color::White.index()]),
            },
            turn: color_to_owner(self.side_to_move).to_string(),
            ply: Some(self.game_ply),
        }
    }

    /// JSON形式から局面を復元する。
    pub fn from_board_state_json(json: &BoardStateJson) -> Result<Self, String> {
        if json.cells.len() != 9 {
            return Err(format!("cells must have 9 rows, but got {}", json.cells.len()));
        }

        let mut position = Position::new();
        position.side_to_move = turn_to_color(&json.turn)?;
        position.game_ply = json.ply.unwrap_or(1);

        let mut black_king = None;
        let mut white_king = None;

        for (rank_idx, row) in json.cells.iter().enumerate() {
            if row.len() != 9 {
                return Err(format!("row {rank_idx} must have 9 cells, but got {}", row.len()));
            }

            for cell in row {
                let square = Square::from_usi(&cell.square)
                    .ok_or_else(|| format!("invalid square: {}", cell.square))?;

                if let Some(piece_json) = &cell.piece {
                    let piece = piece_from_json(piece_json)?;
                    if position.piece_on(square).is_some() {
                        return Err(format!("duplicated piece on square {}", cell.square));
                    }
                    if piece.piece_type() == PieceType::King {
                        match piece.color() {
                            Color::Black => black_king = Some(square),
                            Color::White => white_king = Some(square),
                        }
                    }
                    position.put_piece(piece, square);
                }
            }
        }

        position.king_square[Color::Black.index()] =
            black_king.ok_or_else(|| "sente king is missing in board state".to_string())?;
        position.king_square[Color::White.index()] =
            white_king.ok_or_else(|| "gote king is missing in board state".to_string())?;

        position.hand[Color::Black.index()] = hand_from_json(&json.hands.sente)?;
        position.hand[Color::White.index()] = hand_from_json(&json.hands.gote)?;

        position.compute_hash();
        position.update_blockers_and_pinners();
        position.update_check_squares();
        position.recompute_board_effects();

        let them = !position.side_to_move;
        position.state_mut().checkers =
            position.attackers_to_c(position.king_square[position.side_to_move.index()], them);
        position.state_mut().material_value = compute_material_value(&position);

        Ok(position)
    }

    /// SFENをパースし、盤面をJSON形式で返す。
    pub fn parse_sfen_to_json(sfen: &str) -> Result<BoardStateJson, String> {
        let mut pos = Position::new();
        if sfen.trim() == "startpos" {
            pos.set_sfen(SFEN_HIRATE).map_err(|e| e.to_string())?;
        } else {
            pos.set_sfen(sfen).map_err(|e| e.to_string())?;
        }
        Ok(pos.to_board_state_json())
    }

    /// 棋譜を厳密に適用し、不正手で停止する。
    ///
    /// # Arguments
    /// * `sfen` - 開始局面のSFEN
    /// * `moves` - 適用する棋譜
    /// * `pass_rights` - パス権の初期値（先手, 後手）。棋譜に"pass"が含まれる場合は必須
    pub fn replay_moves_strict(
        sfen: &str,
        moves: &[String],
        pass_rights: Option<(u8, u8)>,
    ) -> Result<ReplayResultJson, String> {
        let mut position = Position::new();
        if sfen.trim() == "startpos" {
            position.set_sfen(SFEN_HIRATE).map_err(|e| e.to_string())?;
        } else {
            position.set_sfen(sfen).map_err(|e| e.to_string())?;
        }

        // パス権が指定された場合は有効化
        if let Some((black, white)) = pass_rights {
            position.enable_pass_rights(black, white);
        }

        let mut applied: Vec<String> = Vec::with_capacity(moves.len());
        let mut error: Option<String> = None;

        for mv in moves {
            let parsed = Move::from_usi(mv).ok_or_else(|| format!("failed to parse move: {mv}"))?;
            let parsed_raw = parsed.raw();

            let mut list = MoveList::new();
            generate_legal(&position, &mut list);
            let is_legal = list.iter().any(|candidate| candidate.raw() == parsed_raw);
            if !is_legal {
                error = Some(format!("illegal move: {mv}"));
                break;
            }

            let gives_check = position.gives_check(parsed);
            position.do_move(parsed, gives_check);
            applied.push(parsed.to_usi());
        }

        let last_ply = if applied.is_empty() {
            -1
        } else {
            (applied.len().min(i32::MAX as usize) as i32) - 1
        };
        let board = position.to_board_state_json();

        Ok(ReplayResultJson {
            applied,
            last_ply,
            board,
            error,
        })
    }
}

fn color_to_owner(color: Color) -> &'static str {
    match color {
        Color::Black => "sente",
        Color::White => "gote",
    }
}

fn turn_to_color(turn: &str) -> Result<Color, String> {
    match turn {
        "sente" => Ok(Color::Black),
        "gote" => Ok(Color::White),
        _ => Err(format!("invalid turn: {turn}")),
    }
}

fn piece_to_json(pc: Piece) -> Option<PieceJson> {
    if pc.is_none() {
        return None;
    }

    let (piece_type, promoted) = match pc.piece_type() {
        PieceType::ProPawn => ("P".to_string(), Some(true)),
        PieceType::ProLance => ("L".to_string(), Some(true)),
        PieceType::ProKnight => ("N".to_string(), Some(true)),
        PieceType::ProSilver => ("S".to_string(), Some(true)),
        PieceType::Horse => ("B".to_string(), Some(true)),
        PieceType::Dragon => ("R".to_string(), Some(true)),
        other => (piece_type_to_string(other), None),
    };

    Some(PieceJson {
        owner: color_to_owner(pc.color()).to_string(),
        piece_type,
        promoted,
    })
}

fn piece_from_json(json: &PieceJson) -> Result<Piece, String> {
    let color = turn_to_color(&json.owner)?;
    let base = string_to_piece_type(&json.piece_type)?;
    let promoted = json.promoted.unwrap_or(false);
    let piece_type = if promoted {
        base.promote()
            .ok_or_else(|| format!("piece {} cannot be promoted", json.piece_type))?
    } else {
        base
    };

    Ok(Piece::new(color, piece_type))
}

fn piece_type_to_string(pt: PieceType) -> String {
    match pt {
        PieceType::Pawn => "P",
        PieceType::Lance => "L",
        PieceType::Knight => "N",
        PieceType::Silver => "S",
        PieceType::Bishop => "B",
        PieceType::Rook => "R",
        PieceType::Gold => "G",
        PieceType::King => "K",
        PieceType::ProPawn => "P",
        PieceType::ProLance => "L",
        PieceType::ProKnight => "N",
        PieceType::ProSilver => "S",
        PieceType::Horse => "B",
        PieceType::Dragon => "R",
    }
    .to_string()
}

fn string_to_piece_type(value: &str) -> Result<PieceType, String> {
    match value.to_ascii_uppercase().as_str() {
        "P" => Ok(PieceType::Pawn),
        "L" => Ok(PieceType::Lance),
        "N" => Ok(PieceType::Knight),
        "S" => Ok(PieceType::Silver),
        "B" => Ok(PieceType::Bishop),
        "R" => Ok(PieceType::Rook),
        "G" => Ok(PieceType::Gold),
        "K" => Ok(PieceType::King),
        other => Err(format!("unknown piece type: {other}")),
    }
}

fn hand_to_json(hand: Hand) -> HandJson {
    let pawn = hand.count(PieceType::Pawn);
    let lance = hand.count(PieceType::Lance);
    let knight = hand.count(PieceType::Knight);
    let silver = hand.count(PieceType::Silver);
    let gold = hand.count(PieceType::Gold);
    let bishop = hand.count(PieceType::Bishop);
    let rook = hand.count(PieceType::Rook);

    HandJson {
        pawn: if pawn > 0 { Some(pawn) } else { None },
        lance: if lance > 0 { Some(lance) } else { None },
        knight: if knight > 0 { Some(knight) } else { None },
        silver: if silver > 0 { Some(silver) } else { None },
        gold: if gold > 0 { Some(gold) } else { None },
        bishop: if bishop > 0 { Some(bishop) } else { None },
        rook: if rook > 0 { Some(rook) } else { None },
    }
}

fn hand_from_json(json: &HandJson) -> Result<Hand, String> {
    let mut hand = Hand::EMPTY;

    let pieces = [
        (PieceType::Pawn, json.pawn.unwrap_or(0)),
        (PieceType::Lance, json.lance.unwrap_or(0)),
        (PieceType::Knight, json.knight.unwrap_or(0)),
        (PieceType::Silver, json.silver.unwrap_or(0)),
        (PieceType::Gold, json.gold.unwrap_or(0)),
        (PieceType::Bishop, json.bishop.unwrap_or(0)),
        (PieceType::Rook, json.rook.unwrap_or(0)),
    ];

    for (pt, count) in pieces {
        if count > hand_max(pt) {
            return Err(format!("hand count for {:?} exceeds limit: {}", pt, count));
        }
        hand = hand.set(pt, count);
    }

    Ok(hand)
}

const fn hand_max(pt: PieceType) -> u32 {
    match pt {
        PieceType::Pawn => 18,
        PieceType::Lance | PieceType::Knight | PieceType::Silver | PieceType::Gold => 4,
        PieceType::Bishop | PieceType::Rook => 2,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initial_board_json() {
        let board = Position::initial_board_json();
        assert_eq!(board.turn, "sente");
        assert_eq!(board.cells.len(), 9);

        let rook_cell = &board.cells[7][1];
        assert_eq!(rook_cell.square, "2h");
        let piece = rook_cell.piece.as_ref().expect("rook should exist");
        assert_eq!(piece.owner, "sente");
        assert_eq!(piece.piece_type, "R");
        assert_eq!(piece.promoted, None);

        let bishop_cell = &board.cells[7][7];
        assert_eq!(bishop_cell.square, "8h");
        let piece = bishop_cell.piece.as_ref().expect("bishop should exist");
        assert_eq!(piece.owner, "sente");
        assert_eq!(piece.piece_type, "B");
        assert_eq!(piece.promoted, None);
    }

    #[test]
    fn test_sfen_roundtrip() {
        let sfen = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
        let json = Position::parse_sfen_to_json(sfen).unwrap();

        let pos = Position::from_board_state_json(&json).unwrap();
        assert_eq!(pos.to_sfen(), sfen);
    }

    #[test]
    fn test_replay_moves_strict_accepts_usi_without_piece_info() {
        let moves = vec!["7g7f".to_string()];
        let result = Position::replay_moves_strict("startpos", &moves).unwrap();
        assert_eq!(result.applied, vec!["7g7f".to_string()]);
        assert!(result.error.is_none());
        assert_eq!(result.last_ply, 0);
    }
}
