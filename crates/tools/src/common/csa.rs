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

/// P+/P- 行のペイロード（先頭 "P+" / "P-" を除いた部分）をパースする。
/// `00FU` = 持ち駒追加、`76FU` = 盤上に駒配置。4文字ずつ消費。
fn parse_hand_setup(pos: &mut Position, color: Color, payload: &str) -> Result<()> {
    let bytes = payload.as_bytes();
    anyhow::ensure!(
        bytes.len().is_multiple_of(4),
        "P+/P- payload length must be multiple of 4: {payload}"
    );
    let mut i = 0;
    while i + 4 <= bytes.len() {
        let file = bytes[i] - b'0';
        let rank = bytes[i + 1] - b'0';
        let code = std::str::from_utf8(&bytes[i + 2..i + 4])
            .with_context(|| format!("invalid UTF-8 in P+/P- payload: {payload}"))?;
        let (pt, promoted) = piece_from_csa_code(code)?;
        if file == 0 && rank == 0 {
            // 持ち駒追加
            anyhow::ensure!(!promoted, "promoted piece in hand: {payload}");
            let hand = match color {
                Color::Black => &mut pos.hand_b,
                Color::White => &mut pos.hand_w,
            };
            hand.add_demoted(pt);
        } else {
            // 盤上配置
            let x = csa_file_to_x(file)?;
            let y = csa_rank_to_y(rank)?;
            pos.board[y][x] = Some(Piece::new(pt, color, promoted));
        }
        i += 4;
    }
    Ok(())
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

/// CSAの指し手と消費時間
#[derive(Clone, Debug, PartialEq)]
pub struct CsaMove {
    /// CSA形式の指し手文字列 (例: "+7776FU")
    pub mv: String,
    /// 消費時間（秒）。`,T30` のように指し手行に含まれる場合に Some
    pub time_sec: Option<u32>,
}

/// CSA特殊手
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpecialMove {
    Resign,      // %TORYO
    Win,         // %KACHI (入玉宣言勝ち)
    Draw,        // %HIKIWAKE
    Sennichite,  // %SENNICHITE
    Interrupt,   // %CHUDAN
    TimeUp,      // %TIME_UP
    IllegalMove, // %ILLEGAL_MOVE
    Jishogi,     // %JISHOGI
    MaxMoves,    // %MAX_MOVES
}

/// パース結果の指し手（通常手 or 特殊手）
#[derive(Clone, Debug, PartialEq)]
pub enum ParsedMove {
    Normal(CsaMove),
    Special(SpecialMove),
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
    let (pos, moves, info) = parse_csa_full(text)?;
    let simple_moves = moves
        .into_iter()
        .filter_map(|m| match m {
            ParsedMove::Normal(cm) => Some(cm.mv),
            ParsedMove::Special(_) => None,
        })
        .collect();
    Ok((pos, simple_moves, info))
}

/// CSA棋譜を完全パース。指し手は消費時間・特殊手を含む `ParsedMove` で返す。
pub fn parse_csa_full(text: &str) -> Result<(Position, Vec<ParsedMove>, GameInfo)> {
    let mut pos = None;
    let mut moves = Vec::new();
    let mut info = GameInfo::default();
    let mut explicit_board = false;
    for line in text.lines() {
        let raw = line.trim_end_matches('\r');
        let s = raw.trim();
        if s.is_empty() || s.starts_with('V') {
            continue;
        }
        // 消費時間行（独立した T 行）: 直前の指し手に付与
        if s.starts_with('T') && s.len() >= 2 && s.as_bytes()[1].is_ascii_digit() {
            if let Some(ParsedMove::Normal(last)) = moves.last_mut()
                && last.time_sec.is_none()
                && let Ok(sec) = s[1..].parse::<u32>()
            {
                last.time_sec = Some(sec);
            }
            continue;
        }
        // 特殊手 (%TORYO 等)
        if s.starts_with('%') {
            if let Some(sp) = parse_special_move(s) {
                moves.push(ParsedMove::Special(sp));
            }
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
        if let Some(removal) = s.strip_prefix("PI") {
            if removal.is_empty() {
                pos = Some(initial_position());
            } else {
                // PI + 駒除去形式: PI82HI22KA ...
                let mut p = initial_position();
                parse_pi_removal(&mut p, removal)?;
                pos = Some(p);
            }
            explicit_board = false;
            continue;
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
            let color = if s.starts_with("P+") {
                Color::Black
            } else {
                Color::White
            };
            let pos_ref = pos.get_or_insert_with(Position::default);
            parse_hand_setup(pos_ref, color, &s[2..])?;
            continue;
        }
        if s.starts_with('+') || s.starts_with('-') {
            if s.len() >= 7 {
                let mv = s[..7].to_string();
                // インライン消費時間: +7776FU,T30
                let time_sec = s
                    .get(7..)
                    .and_then(|rest| rest.strip_prefix(",T").and_then(|t| t.parse::<u32>().ok()));
                moves.push(ParsedMove::Normal(CsaMove { mv, time_sec }));
            }
            continue;
        }
    }
    let pos = pos.unwrap_or_else(initial_position);
    Ok((pos, moves, info))
}

fn parse_special_move(s: &str) -> Option<SpecialMove> {
    // `%+ILLEGAL_ACTION` / `%-ILLEGAL_ACTION` のような手番付き形式も考慮
    let cmd = s.trim_start_matches('%').trim_start_matches(['+', '-']);
    match cmd {
        "TORYO" => Some(SpecialMove::Resign),
        "KACHI" => Some(SpecialMove::Win),
        "HIKIWAKE" => Some(SpecialMove::Draw),
        "SENNICHITE" => Some(SpecialMove::Sennichite),
        "CHUDAN" => Some(SpecialMove::Interrupt),
        "TIME_UP" => Some(SpecialMove::TimeUp),
        "ILLEGAL_MOVE" | "ILLEGAL_ACTION" => Some(SpecialMove::IllegalMove),
        "JISHOGI" => Some(SpecialMove::Jishogi),
        "MAX_MOVES" => Some(SpecialMove::MaxMoves),
        _ => None,
    }
}

/// PI行の駒除去部分をパース。4文字ずつ `<筋><段><駒名>` を消費して平手配置から除去する。
fn parse_pi_removal(pos: &mut Position, payload: &str) -> Result<()> {
    let bytes = payload.as_bytes();
    anyhow::ensure!(
        bytes.len().is_multiple_of(4),
        "PI removal payload length must be multiple of 4: {payload}"
    );
    let mut i = 0;
    while i + 4 <= bytes.len() {
        let file = bytes[i] - b'0';
        let rank = bytes[i + 1] - b'0';
        let x = csa_file_to_x(file)?;
        let y = csa_rank_to_y(rank)?;
        // 対象マスの駒を除去
        anyhow::ensure!(pos.board[y][x].is_some(), "PI removal: no piece at {}{}", file, rank);
        pos.board[y][x] = None;
        i += 4;
    }
    Ok(())
}

// ────────────────────────────────────────────
// CSA ⇔ USI 手変換
// ────────────────────────────────────────────

/// CSA段番号(1-9) → USIアルファベット(a-i)
fn rank_to_usi(rank: u8) -> char {
    (b'a' + rank - 1) as char
}

/// USIアルファベット(a-i) → CSA段番号(1-9)
fn usi_rank_to_num(c: u8) -> Result<u8> {
    anyhow::ensure!((b'a'..=b'i').contains(&c), "invalid USI rank char: {}", c as char);
    Ok(c - b'a' + 1)
}

/// CSA形式の駒種 → USI駒打ち文字（大文字）
fn piece_type_to_usi_drop(pt: PieceType) -> Result<char> {
    use PieceType::*;
    match pt {
        Pawn => Ok('P'),
        Lance => Ok('L'),
        Knight => Ok('N'),
        Silver => Ok('S'),
        Gold => Ok('G'),
        Bishop => Ok('B'),
        Rook => Ok('R'),
        King => bail!("cannot drop king"),
    }
}

/// USI駒打ち文字（大文字）→ CSA駒種コード
fn usi_drop_to_csa_code(c: u8) -> Result<&'static str> {
    match c {
        b'P' => Ok("FU"),
        b'L' => Ok("KY"),
        b'N' => Ok("KE"),
        b'S' => Ok("GI"),
        b'G' => Ok("KI"),
        b'B' => Ok("KA"),
        b'R' => Ok("HI"),
        _ => bail!("invalid USI drop piece: {}", c as char),
    }
}

/// CSA指し手 → USI指し手に変換。
///
/// 例: `+7776FU` → `7g7f`, `+0076FU` → `P*7f`, `+8822UM` → `8h2b+`
///
/// `pos` は変換前の局面（成り判定に使用）。この関数は局面を変更しない。
pub fn csa_move_to_usi(mv: &str, pos: &Position) -> Result<String> {
    anyhow::ensure!(mv.len() >= 7, "invalid CSA move: {mv}");
    let bytes = mv.as_bytes();
    let fx = bytes[1] - b'0';
    let fy = bytes[2] - b'0';
    let tx = bytes[3] - b'0';
    let ty = bytes[4] - b'0';
    let code = &mv[5..7];
    let (dst_pt, dst_promoted) = piece_from_csa_code(code)?;

    let mut out = String::with_capacity(5);
    if fx == 0 && fy == 0 {
        // 駒打ち: P*7f
        let drop_char = piece_type_to_usi_drop(dst_pt)?;
        out.push(drop_char);
        out.push('*');
    } else {
        // 通常手: 7g7f
        out.push((b'0' + fx) as char);
        out.push(rank_to_usi(fy));
    }
    out.push((b'0' + tx) as char);
    out.push(rank_to_usi(ty));

    // 成り判定: 移動元の駒が未成りで、移動先の駒が成り駒なら成り
    if fx != 0 && fy != 0 && dst_promoted {
        let src_x = csa_file_to_x(fx)?;
        let src_y = csa_rank_to_y(fy)?;
        if let Some(src_piece) = pos.board[src_y][src_x] {
            if !src_piece.promoted {
                out.push('+');
            }
        } else {
            // 移動元に駒がない場合でもCSA指し手が成り駒を指定していれば成り
            out.push('+');
        }
    }
    Ok(out)
}

/// USI指し手 → CSA指し手に変換。
///
/// 例: `7g7f` → `+7776FU`, `P*7f` → `+0076FU`, `8h2b+` → `+8822UM`
///
/// `pos` は変換前の局面（駒種解決・手番判定に使用）。この関数は局面を変更しない。
pub fn usi_move_to_csa(mv: &str, pos: &Position) -> Result<String> {
    let bytes = mv.as_bytes();
    anyhow::ensure!(bytes.len() >= 4, "invalid USI move: {mv}");

    let side_char = match pos.side_to_move {
        Color::Black => '+',
        Color::White => '-',
    };

    let mut out = String::with_capacity(7);
    out.push(side_char);

    if bytes.len() >= 4 && bytes[1] == b'*' {
        // 駒打ち: P*7f → +0076FU
        let csa_code = usi_drop_to_csa_code(bytes[0])?;
        let to_file = bytes[2] - b'0';
        let to_rank = usi_rank_to_num(bytes[3])?;
        write!(out, "00{}{}{}", to_file, to_rank, csa_code).unwrap();
    } else {
        // 通常手: 7g7f → +7776FU
        let from_file = bytes[0] - b'0';
        let from_rank = usi_rank_to_num(bytes[1])?;
        let to_file = bytes[2] - b'0';
        let to_rank = usi_rank_to_num(bytes[3])?;
        let promote = bytes.len() >= 5 && bytes[4] == b'+';

        // 移動元の駒種を盤面から取得
        let src_x = csa_file_to_x(from_file)?;
        let src_y = csa_rank_to_y(from_rank)?;
        let src_piece = pos.board[src_y][src_x]
            .with_context(|| format!("no piece at source for USI move: {mv}"))?;

        let csa_code = if promote || src_piece.promoted {
            promoted_csa_code(src_piece.ty)?
        } else {
            base_csa_code(src_piece.ty)
        };

        write!(out, "{}{}{}{}{}", from_file, from_rank, to_file, to_rank, csa_code).unwrap();
    }
    Ok(out)
}

fn base_csa_code(pt: PieceType) -> &'static str {
    use PieceType::*;
    match pt {
        Pawn => "FU",
        Lance => "KY",
        Knight => "KE",
        Silver => "GI",
        Gold => "KI",
        Bishop => "KA",
        Rook => "HI",
        King => "OU",
    }
}

fn promoted_csa_code(pt: PieceType) -> Result<&'static str> {
    use PieceType::*;
    match pt {
        Pawn => Ok("TO"),
        Lance => Ok("NY"),
        Knight => Ok("NK"),
        Silver => Ok("NG"),
        Bishop => Ok("UM"),
        Rook => Ok("RY"),
        _ => bail!("piece {:?} cannot promote", pt),
    }
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
    fn test_parse_hand_setup() {
        // P+ で先手持ち駒（歩2枚・金1枚）と盤上配置を同時に設定
        let text = "\
P1-KY-KE-GI-KI-OU-KI-GI-KE-KY
P2 * -HI *  *  *  *  * -KA *
P3-FU-FU-FU-FU-FU-FU-FU-FU-FU
P4 *  *  *  *  *  *  *  *  *
P5 *  *  *  *  *  *  *  *  *
P6 *  *  *  *  *  *  *  *  *
P7+FU+FU+FU+FU+FU+FU+FU+FU+FU
P8 * +KA *  *  *  *  * +HI *
P9+KY+KE+GI+KI+OU+KI+GI+KE+KY
P+00FU00FU00KI
P-00KA
+
";
        let (pos, _, _) = parse_csa(text).unwrap();
        assert_eq!(pos.hand_b.p, 2);
        assert_eq!(pos.hand_b.g, 1);
        assert_eq!(pos.hand_w.b, 1);
        assert_eq!(pos.side_to_move, Color::Black);
    }

    #[test]
    fn test_parse_time_inline() {
        let text = "PI\n+7776FU,T5\n-3334FU,T10\n%TORYO\n";
        let (_, moves, _) = parse_csa_full(text).unwrap();
        assert_eq!(moves.len(), 3);
        match &moves[0] {
            ParsedMove::Normal(cm) => {
                assert_eq!(cm.mv, "+7776FU");
                assert_eq!(cm.time_sec, Some(5));
            }
            _ => panic!("expected normal move"),
        }
        match &moves[1] {
            ParsedMove::Normal(cm) => {
                assert_eq!(cm.mv, "-3334FU");
                assert_eq!(cm.time_sec, Some(10));
            }
            _ => panic!("expected normal move"),
        }
        assert_eq!(moves[2], ParsedMove::Special(SpecialMove::Resign));
    }

    #[test]
    fn test_parse_time_standalone_t_line() {
        let text = "PI\n+7776FU\nT5\n-3334FU\nT10\n";
        let (_, moves, _) = parse_csa_full(text).unwrap();
        assert_eq!(moves.len(), 2);
        match &moves[0] {
            ParsedMove::Normal(cm) => {
                assert_eq!(cm.time_sec, Some(5));
            }
            _ => panic!("expected normal move"),
        }
        match &moves[1] {
            ParsedMove::Normal(cm) => {
                assert_eq!(cm.time_sec, Some(10));
            }
            _ => panic!("expected normal move"),
        }
    }

    #[test]
    fn test_parse_special_moves() {
        let text = "PI\n+7776FU\n%KACHI\n";
        let (_, moves, _) = parse_csa_full(text).unwrap();
        assert_eq!(moves.len(), 2);
        assert_eq!(moves[1], ParsedMove::Special(SpecialMove::Win));
    }

    #[test]
    fn test_parse_pi_removal() {
        // 二枚落ち: 8二飛・2二角を除去
        let text = "PI82HI22KA\n+\n";
        let (pos, _, _) = parse_csa(text).unwrap();
        let sfen = pos.to_sfen();
        assert_eq!(sfen, "lnsgkgsnl/9/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1");
    }

    #[test]
    fn test_csa_to_usi_normal() {
        let pos = initial_position();
        assert_eq!(csa_move_to_usi("+7776FU", &pos).unwrap(), "7g7f");
        assert_eq!(csa_move_to_usi("-3334FU", &pos).unwrap(), "3c3d");
    }

    #[test]
    fn test_csa_to_usi_promote() {
        let pos = initial_position();
        // 8八角 → 2二角成
        assert_eq!(csa_move_to_usi("+8822UM", &pos).unwrap(), "8h2b+");
    }

    #[test]
    fn test_csa_to_usi_drop() {
        let pos = initial_position();
        assert_eq!(csa_move_to_usi("+0055FU", &pos).unwrap(), "P*5e");
    }

    #[test]
    fn test_usi_to_csa_normal() {
        let pos = initial_position();
        assert_eq!(usi_move_to_csa("7g7f", &pos).unwrap(), "+7776FU");
    }

    #[test]
    fn test_usi_to_csa_promote() {
        let pos = initial_position();
        assert_eq!(usi_move_to_csa("8h2b+", &pos).unwrap(), "+8822UM");
    }

    #[test]
    fn test_usi_to_csa_drop() {
        // 先手持ち駒ありの局面を作る
        let text = "P+55OU\nP-51OU\nP+00FU\n+\n";
        let (pos, _, _) = parse_csa(text).unwrap();
        assert_eq!(usi_move_to_csa("P*7f", &pos).unwrap(), "+0076FU");
    }

    #[test]
    fn test_csa_usi_roundtrip() {
        let pos = initial_position();
        // 通常手のラウンドトリップ
        let csa = "+7776FU";
        let usi = csa_move_to_usi(csa, &pos).unwrap();
        let back = usi_move_to_csa(&usi, &pos).unwrap();
        assert_eq!(back, csa);
    }

    #[test]
    fn test_parse_hand_setup_board_placement() {
        // P+ で盤上に駒を配置
        let text = "\
P+55OU
P-51OU
+
";
        let (pos, _, _) = parse_csa(text).unwrap();
        let x5 = csa_file_to_x(5).unwrap();
        assert_eq!(pos.board[5][x5], Some(Piece::new(PieceType::King, Color::Black, false)));
        assert_eq!(pos.board[1][x5], Some(Piece::new(PieceType::King, Color::White, false)));
    }
}
