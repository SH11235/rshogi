//! CSA形式の棋譜パーサ

use anyhow::{Context, Result, bail};
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
            bail!("insufficient hand for drop: {pt:?}");
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

fn piece_from_csa_token(token: &str) -> Result<Option<Piece>> {
    if token == "*" {
        return Ok(None);
    }
    anyhow::ensure!(token.len() == 3, "invalid CSA board token: {token}");
    let color = match token.as_bytes()[0] {
        b'+' => Color::Black,
        b'-' => Color::White,
        _ => bail!("invalid CSA board token side: {token}"),
    };
    let (ty, promoted) = piece_from_csa_code(&token[1..3])?;
    Ok(Some(Piece::new(ty, color, promoted)))
}

fn tokenize_csa_rank(payload: &str) -> Result<Vec<String>> {
    let bytes = payload.as_bytes();
    let mut i = 0usize;
    let mut tokens = Vec::with_capacity(9);

    while i < bytes.len() {
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        match bytes[i] {
            b'*' => {
                tokens.push("*".to_string());
                i += 1;
            }
            b'+' | b'-' => {
                anyhow::ensure!(i + 3 <= bytes.len(), "short CSA rank token: {payload}");
                let token = std::str::from_utf8(&bytes[i..i + 3])
                    .with_context(|| format!("invalid UTF-8 in CSA rank token: {payload}"))?;
                tokens.push(token.to_string());
                i += 3;
            }
            _ => bail!("invalid CSA rank payload: {payload}"),
        }
    }

    anyhow::ensure!(tokens.len() == 9, "CSA rank must have 9 squares: {payload}");
    Ok(tokens)
}

fn parse_csa_rank_line(pos: &mut Position, line: &str) -> Result<()> {
    anyhow::ensure!(line.len() >= 2, "invalid CSA rank line: {line}");
    let rank = csa_rank_to_y(line.as_bytes()[1] - b'0')?;
    let tokens = tokenize_csa_rank(&line[2..])?;
    for (x, token) in (1..=9).zip(tokens.iter()) {
        pos.board[rank][x] = piece_from_csa_token(token)?;
    }
    Ok(())
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
                        write!(out, "{empty}").unwrap();
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
                write!(out, "{empty}").unwrap();
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
        let tx = csa_file_to_x(bytes[3] - b'0')?;
        let ty = csa_rank_to_y(bytes[4] - b'0')?;
        let code = &mv[5..7];
        let (pty, promoted) = piece_from_csa_code(code)?;
        // validate turn
        anyhow::ensure!(
            self.side_to_move == side,
            "turn mismatch: mv={mv} side_to_move={:?}",
            self.side_to_move
        );

        // capture if any
        if let Some(dst) = self.board[ty][tx] {
            anyhow::ensure!(dst.color != side, "cannot capture own piece: {mv}");
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
            self.board[ty][tx] = Some(Piece::new(pty, side, false));
        } else {
            // normal move
            let fx = csa_file_to_x(fx)?;
            let fy = csa_rank_to_y(fy)?;
            let src = self.board[fy][fx].with_context(|| format!("no piece at source for {mv}"))?;
            anyhow::ensure!(src.color == side, "source piece color mismatch: {mv}");
            // remove src
            self.board[fy][fx] = None;
            // place with promoted flag per destination code
            self.board[ty][tx] = Some(Piece::new(pty, side, promoted));
        }

        self.side_to_move = match self.side_to_move {
            Color::Black => Color::White,
            Color::White => Color::Black,
        };
        self.ply += 1;
        Ok(())
    }
}

fn csa_file_to_x(file: u8) -> Result<usize> {
    anyhow::ensure!((1..=9).contains(&file), "bad CSA file: {file}");
    Ok((10 - file) as usize)
}

fn csa_rank_to_y(rank: u8) -> Result<usize> {
    anyhow::ensure!((1..=9).contains(&rank), "bad CSA rank: {rank}");
    Ok(rank as usize)
}

fn append_hand(out: &mut String, h: Hand, black: bool) {
    let mut push = |c: char, n: u8| {
        if n == 0 {
            return;
        }
        if n == 1 {
            out.push(c);
        } else {
            out.push_str(&format!("{n}{c}"));
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

/// CSA棋譜から抽出した対局メタデータ
#[derive(Clone, Debug, Default)]
pub struct GameInfo {
    pub black_name: Option<String>,
    pub white_name: Option<String>,
    pub black_rating: Option<f64>,
    pub white_rating: Option<f64>,
}

impl GameInfo {
    /// 両対局者のレーティングが `min` 以上か判定。レーティング不明の場合は false。
    pub fn both_ratings_at_least(&self, min: f64) -> bool {
        match (self.black_rating, self.white_rating) {
            (Some(br), Some(wr)) => br >= min && wr >= min,
            _ => false,
        }
    }
}

/// Parse a CSA text and return initial position, list of move tokens, and game metadata.
pub fn parse_csa(text: &str) -> Result<(Position, Vec<String>, GameInfo)> {
    let mut pos = None;
    let mut moves = Vec::new();
    let mut info = GameInfo::default();
    let mut explicit_board = false;
    for line in text.lines() {
        let raw = line.trim_end_matches('\r');
        let s = raw.trim();
        if s.is_empty() || s.starts_with('%') || s.starts_with('V') || s.starts_with('T') {
            continue;
        }
        // Player names
        if let Some(name) = s.strip_prefix("N+") {
            info.black_name = Some(name.to_string());
            continue;
        }
        if let Some(name) = s.strip_prefix("N-") {
            info.white_name = Some(name.to_string());
            continue;
        }
        if s.starts_with('N') {
            continue;
        }
        // Rating comments (floodgate format: 'black_rate:<player_id>:<rating>)
        if let Some(rest) = s.strip_prefix("'black_rate:") {
            if let Some(v) = parse_rate_value(rest) {
                info.black_rating = Some(v);
            }
            continue;
        }
        if let Some(rest) = s.strip_prefix("'white_rate:") {
            if let Some(v) = parse_rate_value(rest) {
                info.white_rating = Some(v);
            }
            continue;
        }
        // Skip other comments and headers
        if s.starts_with('\'') || s.starts_with('$') {
            continue;
        }
        if s == "PI" {
            pos = Some(initial_position());
            explicit_board = false;
            continue;
        }
        if s.starts_with("PI") {
            bail!("unsupported CSA initial position shorthand: {s}");
        }
        if s.starts_with('P') && s.len() >= 2 && s.as_bytes()[1].is_ascii_digit() {
            let pos_ref = if explicit_board {
                pos.get_or_insert_with(Position::default)
            } else {
                explicit_board = true;
                pos.insert(Position::default())
            };
            parse_csa_rank_line(pos_ref, raw)?;
            continue;
        }
        if s == "+" || s == "-" {
            let pos_ref = pos.get_or_insert_with(initial_position);
            pos_ref.side_to_move = if s == "+" { Color::Black } else { Color::White };
            continue;
        }
        if s.starts_with("P+") || s.starts_with("P-") {
            bail!("unsupported CSA hand/setup line: {s}");
        }
        if s.starts_with('+') || s.starts_with('-') {
            if s.len() >= 7 {
                moves.push(s[..7].to_string());
            }
            continue;
        }
    }
    let pos = pos.unwrap_or_else(initial_position);
    Ok((pos, moves, info))
}

/// `<player_id>:<rating>` 形式からレーティング値を抽出
fn parse_rate_value(s: &str) -> Option<f64> {
    let val_str = s.rsplit(':').next()?;
    val_str.parse::<f64>().ok()
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
        let (mut pos, moves, _info) = parse_csa(text).unwrap();
        assert_eq!(moves.len(), 2);
        for m in &moves {
            pos.apply_csa_move(m).unwrap();
        }
        assert_eq!(pos.side_to_move, Color::Black);
        assert_eq!(pos.ply, 3);
        assert_eq!(
            pos.board[6][csa_file_to_x(7).unwrap()],
            Some(Piece::new(PieceType::Pawn, Color::Black, false))
        );
        assert_eq!(pos.board[7][csa_file_to_x(7).unwrap()], None);
        assert_eq!(
            pos.board[4][csa_file_to_x(3).unwrap()],
            Some(Piece::new(PieceType::Pawn, Color::White, false))
        );
        assert_eq!(pos.board[3][csa_file_to_x(3).unwrap()], None);
    }

    #[test]
    fn test_apply_csa_move_uses_csa_file_coordinates() {
        let text = "V2.2\nPI\n+8822UM\n";
        let (mut pos, moves, _info) = parse_csa(text).unwrap();
        pos.apply_csa_move(&moves[0]).unwrap();

        assert_eq!(pos.hand_b.b, 1, "白角を取って先手の角持ちになるはず");
        assert_eq!(pos.hand_b.r, 0, "飛車を取ってはいけない");
        assert_eq!(
            pos.board[2][csa_file_to_x(2).unwrap()],
            Some(Piece::new(PieceType::Bishop, Color::Black, true))
        );
        assert_eq!(
            pos.board[2][csa_file_to_x(8).unwrap()],
            Some(Piece::new(PieceType::Rook, Color::White, false))
        );
    }

    #[test]
    fn test_parse_floodgate_ratings() {
        let text = "\
V2
N+EngineA
N-EngineB
'black_rate:EngineA+abc123:4166.0
'white_rate:EngineB+def456:4156.0
PI
+7776FU
-3334FU
";
        let (_pos, moves, info) = parse_csa(text).unwrap();
        assert_eq!(moves.len(), 2);
        assert_eq!(info.black_name.as_deref(), Some("EngineA"));
        assert_eq!(info.white_name.as_deref(), Some("EngineB"));
        assert!((info.black_rating.unwrap() - 4166.0).abs() < 0.01);
        assert!((info.white_rating.unwrap() - 4156.0).abs() < 0.01);
        assert!(info.both_ratings_at_least(4000.0));
        assert!(!info.both_ratings_at_least(4200.0));
    }

    #[test]
    fn test_parse_p1p9_format() {
        // P1..P9 形式を明示盤面として解釈する
        let text = "\
V2
N+PlayerA
N-PlayerB
'black_rate:PlayerA+hash:3500.0
'white_rate:PlayerB+hash:3200.0
P1-KY-KE-GI-KI-OU-KI-GI-KE-KY
P2 * -HI *  *  *  *  * -KA *
P3-FU-FU-FU-FU-FU-FU-FU-FU-FU
P4 *  *  *  *  *  *  *  *  *
P5 *  *  *  *  *  *  *  *  *
P6 *  *  *  *  *  *  *  *  *
P7+FU+FU+FU+FU+FU+FU+FU+FU+FU
P8 * +KA *  *  *  *  * +HI *
P9+KY+KE+GI+KI+OU+KI+GI+KE+KY
+
+7776FU
";
        let (pos, moves, info) = parse_csa(text).unwrap();
        assert_eq!(moves.len(), 1);
        assert!((info.black_rating.unwrap() - 3500.0).abs() < 0.01);
        assert_eq!(
            pos.to_sfen(),
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1"
        );
    }

    #[test]
    fn test_parse_p1p9_white_to_move() {
        let text = "\
V2
P1-KY-KE-GI-KI-OU-KI-GI-KE-KY
P2 * -HI *  *  *  *  * -KA *
P3-FU-FU-FU-FU-FU-FU-FU-FU-FU
P4 *  *  *  *  *  *  *  *  *
P5 *  *  *  *  *  *  *  *  *
P6 *  *  *  *  *  *  *  *  *
P7+FU+FU+FU+FU+FU+FU+FU+FU+FU
P8 * +KA *  *  *  *  * +HI *
P9+KY+KE+GI+KI+OU+KI+GI+KE+KY
-
";
        let (pos, moves, _info) = parse_csa(text).unwrap();
        assert!(moves.is_empty());
        assert_eq!(
            pos.to_sfen(),
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL w - 1"
        );
    }

    #[test]
    fn test_parse_csa_rejects_unsupported_pplus_setup() {
        let text = "\
V2
P+00HI
";
        let err = parse_csa(text).expect_err("P+ setup line is unsupported");
        assert!(err.to_string().contains("unsupported CSA hand/setup line"));
    }
}
