use anyhow::{bail, Context, Result};
use std::fmt::Write as _;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Color {
    Black,
    White,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PieceType {
    Pawn,
    Lance,
    Knight,
    Silver,
    Gold,
    Bishop,
    Rook,
    King,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Piece {
    pub ty: PieceType,
    pub color: Color,
    pub promoted: bool,
}

impl Piece {
    pub fn new(ty: PieceType, color: Color, promoted: bool) -> Self {
        Self {
            ty,
            color,
            promoted,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Position {
    /// board[y][x] with x:1..=9 (file), y:1..=9 (rank) using 1-based indexing for CSA convenience
    board: [[Option<Piece>; 10]; 10],
    hand_b: Hand,
    hand_w: Hand,
    pub side_to_move: Color,
    pub ply: u32,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Hand {
    pub p: u8,
    pub l: u8,
    pub n: u8,
    pub s: u8,
    pub g: u8,
    pub b: u8,
    pub r: u8,
}

impl Hand {
    fn add_demoted(&mut self, pt: PieceType) {
        match pt {
            PieceType::Pawn => self.p += 1,
            PieceType::Lance => self.l += 1,
            PieceType::Knight => self.n += 1,
            PieceType::Silver => self.s += 1,
            PieceType::Gold => self.g += 1,
            PieceType::Bishop => self.b += 1,
            PieceType::Rook => self.r += 1,
            PieceType::King => {}
        }
    }
    fn take_one(&mut self, pt: PieceType) -> Result<()> {
        if matches!(pt, PieceType::King) {
            bail!("cannot drop king");
        }
        let slot = match pt {
            PieceType::Pawn => &mut self.p,
            PieceType::Lance => &mut self.l,
            PieceType::Knight => &mut self.n,
            PieceType::Silver => &mut self.s,
            PieceType::Gold => &mut self.g,
            PieceType::Bishop => &mut self.b,
            PieceType::Rook => &mut self.r,
            PieceType::King => unreachable!("handled above"),
        };
        if *slot == 0 {
            bail!("insufficient hand for drop: {:?}", pt);
        }
        *slot -= 1;
        Ok(())
    }
}

impl Default for Position {
    fn default() -> Self {
        Self {
            board: [[None; 10]; 10],
            hand_b: Hand::default(),
            hand_w: Hand::default(),
            side_to_move: Color::Black,
            ply: 1,
        }
    }
}

/// Build the standard initial position.
pub fn initial_position() -> Position {
    let mut pos = Position::default();
    // Rank 1 (top, white side)
    let top = [
        PieceType::Lance,
        PieceType::Knight,
        PieceType::Silver,
        PieceType::Gold,
        PieceType::King,
        PieceType::Gold,
        PieceType::Silver,
        PieceType::Knight,
        PieceType::Lance,
    ];
    for (x, ty) in (1..=9).zip(top.iter().copied()) {
        pos.board[1][x] = Some(Piece::new(ty, Color::White, false));
    }
    // Rank 2: 2nd file = rook, 8th file = bishop (SFEN: "1r5b1")
    pos.board[2][2] = Some(Piece::new(PieceType::Rook, Color::White, false));
    pos.board[2][8] = Some(Piece::new(PieceType::Bishop, Color::White, false));
    // Rank 3: pawns
    for x in 1..=9 {
        pos.board[3][x] = Some(Piece::new(PieceType::Pawn, Color::White, false));
    }
    // Rank 7: black pawns
    for x in 1..=9 {
        pos.board[7][x] = Some(Piece::new(PieceType::Pawn, Color::Black, false));
    }
    // Rank 8: black bishop/rook (SFEN: "1B5R1")
    pos.board[8][2] = Some(Piece::new(PieceType::Bishop, Color::Black, false));
    pos.board[8][8] = Some(Piece::new(PieceType::Rook, Color::Black, false));
    // Rank 9: black back rank
    let bot = [
        PieceType::Lance,
        PieceType::Knight,
        PieceType::Silver,
        PieceType::Gold,
        PieceType::King,
        PieceType::Gold,
        PieceType::Silver,
        PieceType::Knight,
        PieceType::Lance,
    ];
    for (x, ty) in (1..=9).zip(bot.iter().copied()) {
        pos.board[9][x] = Some(Piece::new(ty, Color::Black, false));
    }
    pos
}

/// CSA piece code at destination -> (PieceType, promoted)
fn piece_from_csa_code(code: &str) -> Result<(PieceType, bool)> {
    use PieceType::*;
    let (ty, promoted) = match code {
        "FU" => (Pawn, false),
        "KY" => (Lance, false),
        "KE" => (Knight, false),
        "GI" => (Silver, false),
        "KI" => (Gold, false),
        "KA" => (Bishop, false),
        "HI" => (Rook, false),
        "OU" => (King, false),
        "TO" => (Pawn, true),
        "NY" => (Lance, true),
        "NK" => (Knight, true),
        "NG" => (Silver, true),
        "UM" => (Bishop, true),
        "RY" => (Rook, true),
        _ => bail!("unknown CSA piece code: {code}"),
    };
    Ok((ty, promoted))
}

fn usi_rank_letter(n: u8) -> Result<char> {
    // 1..=9 -> 'a'..'i'
    if !(1..=9).contains(&n) {
        bail!("invalid rank number: {n}");
    }
    Ok((b'a' + (n - 1)) as char)
}

fn sfen_piece_char(p: Piece) -> char {
    use PieceType::*;
    let base = match p.ty {
        Pawn => 'p',
        Lance => 'l',
        Knight => 'n',
        Silver => 's',
        Gold => 'g',
        Bishop => 'b',
        Rook => 'r',
        King => 'k',
    };

    match p.color {
        Color::Black => base.to_ascii_uppercase(),
        Color::White => base,
    }
}

impl Position {
    pub fn to_sfen(&self) -> String {
        // board strings from rank 1 (top) to 9 (bottom)
        let mut out = String::new();
        for y in 1..=9 {
            let mut empty = 0u8;
            for x in 1..=9 {
                if let Some(pc) = self.board[y][x] {
                    if empty > 0 {
                        write!(out, "{}", empty).unwrap();
                        empty = 0;
                    }
                    if pc.promoted {
                        out.push('+');
                    }
                    out.push(sfen_piece_char(pc));
                } else {
                    empty += 1;
                }
            }
            if empty > 0 {
                write!(out, "{}", empty).unwrap();
            }
            if y != 9 {
                out.push('/');
            }
        }
        // side to move
        out.push(' ');
        out.push(match self.side_to_move {
            Color::Black => 'b',
            Color::White => 'w',
        });
        out.push(' ');
        // hands
        let mut hands = String::new();
        // Black (uppercase in SFEN)
        append_hand(&mut hands, self.hand_b, true);
        // White (lowercase)
        append_hand(&mut hands, self.hand_w, false);
        if hands.is_empty() {
            out.push('-');
        } else {
            out.push_str(&hands);
        }
        write!(out, " {}", self.ply).unwrap();
        out
    }

    /// Apply a CSA move like "+7776FU" or "-0055KA".
    pub fn apply_csa_move(&mut self, mv: &str) -> Result<()> {
        anyhow::ensure!(mv.len() >= 7, "invalid CSA move format: {mv}");
        let bytes = mv.as_bytes();
        let side = match bytes[0] {
            b'+' => Color::Black,
            b'-' => Color::White,
            _ => bail!("bad side: {mv}"),
        };
        let fx = bytes[1] - b'0';
        let fy = bytes[2] - b'0';
        let tx = bytes[3] - b'0';
        let ty = bytes[4] - b'0';
        let code = &mv[5..7];
        let (pty, promoted) = piece_from_csa_code(code)?;
        // validate turn
        anyhow::ensure!(
            self.side_to_move == side,
            "turn mismatch: mv={mv} side_to_move={:?}",
            self.side_to_move
        );

        // capture if any
        if let Some(dst) = self.board[ty as usize][tx as usize] {
            // captured piece moves to hand of mover, demoted
            let demoted_ty = dst.ty; // dst.promoted doesn't matter; demote
            match side {
                Color::Black => self.hand_b.add_demoted(demoted_ty),
                Color::White => self.hand_w.add_demoted(demoted_ty),
            }
        }

        if fx == 0 && fy == 0 {
            // drop
            match side {
                Color::Black => self.hand_b.take_one(pty)?,
                Color::White => self.hand_w.take_one(pty)?,
            }
            self.board[ty as usize][tx as usize] = Some(Piece::new(pty, side, false));
        } else {
            // normal move
            let _ = self.board[fy as usize][fx as usize]
                .with_context(|| format!("no piece at source: {fx}{fy} for {mv}"))?;
            // remove src
            self.board[fy as usize][fx as usize] = None;
            // place with promoted flag per destination code
            self.board[ty as usize][tx as usize] = Some(Piece::new(pty, side, promoted));
        }

        self.side_to_move = match self.side_to_move {
            Color::Black => Color::White,
            Color::White => Color::Black,
        };
        self.ply += 1;
        Ok(())
    }
}

fn append_hand(out: &mut String, h: Hand, black: bool) {
    let mut push = |c: char, n: u8| {
        if n == 0 {
            return;
        }
        if n == 1 {
            out.push(c);
        } else {
            out.push_str(&format!("{}{}", n, c));
        }
    };
    if black {
        push('R', h.r);
        push('B', h.b);
        push('G', h.g);
        push('S', h.s);
        push('N', h.n);
        push('L', h.l);
        push('P', h.p);
    } else {
        push('r', h.r);
        push('b', h.b);
        push('g', h.g);
        push('s', h.s);
        push('n', h.n);
        push('l', h.l);
        push('p', h.p);
    }
}

/// Parse a CSA text and return initial position and list of move tokens ("+7776FU" ...)
pub fn parse_csa(text: &str) -> Result<(Position, Vec<String>)> {
    // Start with either PI (initial) or explicit P1..P9 layout. We implement PI and ignore explicit layouts for now.
    let mut pos = None;
    let mut moves = Vec::new();
    for line in text.lines() {
        let s = line.trim();
        if s.is_empty()
            || s.starts_with('%')
            || s.starts_with('N')
            || s.starts_with('V')
            || s.starts_with('T')
        {
            continue;
        }
        if s == "PI" {
            pos = Some(initial_position());
            continue;
        }
        if s.starts_with('+') || s.starts_with('-') {
            if s.len() >= 7 {
                moves.push(s[..7].to_string());
            }
            continue;
        }
        // Other headers are ignored for now.
    }
    let pos = pos.unwrap_or_else(initial_position);
    Ok((pos, moves))
}

/// Convert CSA numeric square like "77" into USI like "7g" (helper if needed elsewhere)
pub fn csa_sq_to_usi_sq(src: &str) -> Result<String> {
    anyhow::ensure!(src.len() == 2, "bad sq: {src}");
    let bytes = src.as_bytes();
    let file = bytes[0] - b'0';
    let rank = bytes[1] - b'0';
    let r = usi_rank_letter(rank)?;
    Ok(format!("{}{}", file, r))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_initial_sfen() {
        let pos = initial_position();
        let sfen = pos.to_sfen();
        assert_eq!(sfen, "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1");
    }
    #[test]
    fn test_parse_and_apply_two_pawn_moves() {
        let text = "V2.2\nPI\n+7776FU\n-3334FU\n";
        let (mut pos, moves) = parse_csa(text).unwrap();
        assert_eq!(moves.len(), 2);
        for m in &moves {
            pos.apply_csa_move(m).unwrap();
        }
        let s = pos.to_sfen();
        // After 2 plies, side to move back to Black, ply=3
        assert!(s.ends_with(" b - 3"), "{s}");
        // 7g pawn moved to 7f, 3c pawn to 3d
        // 下段2段は不変であることだけ確認（8,9段）
        assert!(s.contains("1B5R1/LNSGKGSNL")); // sanity of bottom two ranks
    }
}
