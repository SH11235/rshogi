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
    fn add_demoted(&mut self, pt: PieceType) -> Result<()> {
        let slot = match pt {
            PieceType::Pawn => &mut self.p,
            PieceType::Lance => &mut self.l,
            PieceType::Knight => &mut self.n,
            PieceType::Silver => &mut self.s,
            PieceType::Gold => &mut self.g,
            PieceType::Bishop => &mut self.b,
            PieceType::Rook => &mut self.r,
            PieceType::King => return Ok(()),
        };
        *slot = slot.checked_add(1).with_context(|| format!("hand count exceeds 255: {pt:?}"))?;
        Ok(())
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

fn piece_from_sfen_char(c: char) -> Result<(PieceType, Color)> {
    use PieceType::*;
    let ty = match c.to_ascii_lowercase() {
        'p' => Pawn,
        'l' => Lance,
        'n' => Knight,
        's' => Silver,
        'g' => Gold,
        'b' => Bishop,
        'r' => Rook,
        'k' => King,
        _ => bail!("unknown SFEN piece: {c}"),
    };
    let color = if c.is_ascii_uppercase() {
        Color::Black
    } else {
        Color::White
    };
    Ok((ty, color))
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
    /// SFEN 文字列（`<盤面> <手番> <持ち駒> [手数]`）から局面を作る。
    ///
    /// 手数は省略可能で、省略時は 1 とする。
    /// 持ち駒の反復表記も合算し、先後・駒種ごとの合計が255枚を超える場合はエラーを返す。
    pub fn from_sfen(sfen: &str) -> Result<Self> {
        let mut fields = sfen.split_whitespace();
        let board = fields.next().context("SFEN が空です")?;
        let side = fields.next().with_context(|| format!("SFEN に手番がありません: {sfen}"))?;
        let hands = fields.next().with_context(|| format!("SFEN に持ち駒がありません: {sfen}"))?;
        let mut pos = Position::default();

        let ranks: Vec<&str> = board.split('/').collect();
        anyhow::ensure!(ranks.len() == 9, "SFEN の段数が 9 ではありません: {sfen}");
        for (y, rank) in (1..=9).zip(ranks) {
            let mut x = 1usize;
            let mut promoted = false;
            for c in rank.chars() {
                if c == '+' {
                    anyhow::ensure!(!promoted, "SFEN の成り指定が重複しています: {sfen}");
                    promoted = true;
                    continue;
                }
                if let Some(empty) = c.to_digit(10) {
                    anyhow::ensure!(!promoted && empty >= 1, "SFEN の空マス指定が不正です: {sfen}");
                    x += empty as usize;
                    continue;
                }
                anyhow::ensure!(x <= 9, "SFEN の段が 9 マスを超えています: {sfen}");
                let (ty, color) = piece_from_sfen_char(c)?;
                anyhow::ensure!(
                    !promoted || !matches!(ty, PieceType::Gold | PieceType::King),
                    "成れない駒に成り指定があります: {sfen}"
                );
                pos.board[y][x] = Some(Piece::new(ty, color, promoted));
                promoted = false;
                x += 1;
            }
            anyhow::ensure!(x == 10 && !promoted, "SFEN の段が 9 マスではありません: {sfen}");
        }

        pos.side_to_move = match side {
            "b" => Color::Black,
            "w" => Color::White,
            _ => bail!("SFEN の手番が不正です: {sfen}"),
        };

        if hands != "-" {
            // 1 種類の持ち駒は最大でも歩の 18 枚。
            const MAX_HAND_COUNT: u32 = 18;
            let mut count: Option<u32> = None;
            for c in hands.chars() {
                if let Some(d) = c.to_digit(10) {
                    let next = count.unwrap_or(0) * 10 + d;
                    anyhow::ensure!(
                        (1..=MAX_HAND_COUNT).contains(&next),
                        "SFEN の持ち駒の枚数が不正です: {sfen}"
                    );
                    count = Some(next);
                    continue;
                }
                let (ty, color) = piece_from_sfen_char(c)?;
                anyhow::ensure!(ty != PieceType::King, "SFEN の持ち駒に玉があります: {sfen}");
                let hand = match color {
                    Color::Black => &mut pos.hand_b,
                    Color::White => &mut pos.hand_w,
                };
                for _ in 0..count.take().unwrap_or(1) {
                    hand.add_demoted(ty)?;
                }
            }
            anyhow::ensure!(count.is_none(), "SFEN の持ち駒が枚数で終わっています: {sfen}");
        }

        if let Some(ply) = fields.next() {
            pos.ply = ply.parse().with_context(|| format!("SFEN の手数が不正です: {sfen}"))?;
        }
        Ok(pos)
    }

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

    /// CSA形式の盤面文字列を生成する（P1-P9 + P+/P- + 手番行）。
    pub fn to_csa_board(&self) -> String {
        let mut out = String::new();
        // P1-P9
        for y in 1..=9 {
            write!(out, "P{y}").unwrap();
            // CSA の P1 行は 9筋(左) → 1筋(右) の順。内部 x=1 が9筋。
            for x in 1..=9 {
                if let Some(pc) = self.board[y][x] {
                    let side = match pc.color {
                        Color::Black => '+',
                        Color::White => '-',
                    };
                    let code = if pc.promoted {
                        promoted_csa_code_static(pc.ty)
                    } else {
                        base_csa_code(pc.ty)
                    };
                    write!(out, "{side}{code}").unwrap();
                } else {
                    out.push_str(" * ");
                }
            }
            out.push('\n');
        }
        // 持ち駒
        let mut write_hand = |prefix: &str, h: &Hand| {
            let pieces: &[(PieceType, u8)] = &[
                (PieceType::Rook, h.r),
                (PieceType::Bishop, h.b),
                (PieceType::Gold, h.g),
                (PieceType::Silver, h.s),
                (PieceType::Knight, h.n),
                (PieceType::Lance, h.l),
                (PieceType::Pawn, h.p),
            ];
            let mut hand_str = String::new();
            for &(pt, count) in pieces {
                for _ in 0..count {
                    write!(hand_str, "00{}", base_csa_code(pt)).unwrap();
                }
            }
            if !hand_str.is_empty() {
                writeln!(out, "{prefix}{hand_str}").unwrap();
            }
        };
        write_hand("P+", &self.hand_b);
        write_hand("P-", &self.hand_w);
        // 手番
        match self.side_to_move {
            Color::Black => out.push('+'),
            Color::White => out.push('-'),
        }
        out
    }

    /// CSA指し手（`+7776FU`、`-0055KA`など）を局面に適用する。
    ///
    /// 先頭7 byteを解釈し、それ以降の消費時間やコメントは無視する。
    /// 将棋の完全な合法性検査は行わない。
    ///
    /// # Errors
    /// 構文・座標・手番・移動元・持ち駒が不正、持ち駒が255枚を超える、または手数が
    /// `u32::MAX`を超える場合はエラーを返し、局面を変更しない。
    pub fn apply_csa_move(&mut self, mv: &str) -> Result<()> {
        validate_csa_move(mv)?;
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
        anyhow::ensure!(
            self.side_to_move == side,
            "turn mismatch: mv={mv} side_to_move={:?}",
            self.side_to_move
        );
        let next_ply = self.ply.checked_add(1).context("CSA ply exceeds u32::MAX")?;
        let mut next_hand = match side {
            Color::Black => self.hand_b,
            Color::White => self.hand_w,
        };
        if let Some(dst) = self.board[ty][tx] {
            anyhow::ensure!(dst.color != side, "cannot capture own piece: {mv}");
            next_hand.add_demoted(dst.ty)?;
        }

        let source = if fx == 0 && fy == 0 {
            next_hand.take_one(pty)?;
            None
        } else {
            let fx = csa_file_to_x(fx)?;
            let fy = csa_rank_to_y(fy)?;
            let src = self.board[fy][fx].with_context(|| format!("no piece at source for {mv}"))?;
            anyhow::ensure!(src.color == side, "source piece color mismatch: {mv}");
            Some((fx, fy))
        };

        if let Some((fx, fy)) = source {
            self.board[fy][fx] = None;
        }
        self.board[ty][tx] = Some(Piece::new(pty, side, source.is_some() && promoted));
        match side {
            Color::Black => self.hand_b = next_hand,
            Color::White => self.hand_w = next_hand,
        }
        self.side_to_move = match self.side_to_move {
            Color::Black => Color::White,
            Color::White => Color::Black,
        };
        self.ply = next_ply;
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
        // 数字以外の byte でも underflow させず、後段の範囲チェックで弾く。
        let file = bytes[i].wrapping_sub(b'0');
        let rank = bytes[i + 1].wrapping_sub(b'0');
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
            hand.add_demoted(pt)?;
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

/// `parse_csa_full_with_evals` の返り値。
pub type ParsedCsaWithEvals = (Position, Vec<ParsedMove>, GameInfo, Vec<Option<i32>>);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EvalCommentStyle {
    /// `'*` は次の通常手、`'**` は直前の通常手に帰属する標準解釈。
    #[default]
    Standard,
    /// 旧形式のrshogi-csa-server向け。inline T付き通常手直後の`'*`を前手へ帰属する。
    LegacyServerPost,
}

/// `'summary:` 行が示す勝敗。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SummaryResult {
    /// 先手の勝ち。
    BlackWin,
    /// 後手の勝ち。
    WhiteWin,
    /// 引き分け。
    Draw,
}

/// shogi-server (wdoor) 形式の `'summary:<理由>:<先手名> <結果>:<後手名> <結果>` 行。
///
/// 終局コマンド（`%TORYO` など）と違い、勝敗を手番に頼らず明示する。wdoor の棋譜は
/// 反則負けや連続王手の千日手を `%` コマンドなしで終えるため、勝敗はこの行でしか
/// 分からないことがある。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CsaSummary {
    /// 終局理由（`toryo` / `time up` / `illegal move` など、wdoor の表記のまま）。
    pub reason: String,
    /// 勝敗。
    pub result: SummaryResult,
}

impl CsaSummary {
    /// `'summary:` より後ろの部分を解釈する。勝敗が読めなければ `None`。
    ///
    /// 対局者名は見ない（`<名前>+<hash>` 形式で書かれることがある）。先手・後手は
    /// 並び順で決まる。
    pub fn parse(rest: &str) -> Option<Self> {
        let (reason, players) = rest.trim().split_once(':')?;
        let (black, white) = players.rsplit_once(':')?;
        let word = |part: &str| part.trim().rsplit(' ').next().map(str::to_owned);
        let result = match (word(black)?.as_str(), word(white)?.as_str()) {
            ("win", "lose") => SummaryResult::BlackWin,
            ("lose", "win") => SummaryResult::WhiteWin,
            ("draw", "draw") => SummaryResult::Draw,
            _ => return None,
        };
        Some(Self {
            reason: reason.trim().to_owned(),
            result,
        })
    }

    /// 終局理由を終局コマンド相当の [`SpecialMove`] に直す。未知の理由は `None`。
    ///
    /// 反則系（`illegal move` / `oute_kaihimore` / `oute_sennichite` など）は
    /// [`SpecialMove::IllegalMove`]、`abnormal` は [`SpecialMove::Interrupt`] にまとめる。
    pub fn special_move(&self) -> Option<SpecialMove> {
        Some(match self.reason.as_str() {
            "toryo" => SpecialMove::Resign,
            "kachi" => SpecialMove::Win,
            "time up" => SpecialMove::TimeUp,
            "illegal move" | "uchifuzume" | "oute_kaihimore" | "outori" | "illegal kachi"
            | "oute_sennichite" => SpecialMove::IllegalMove,
            "sennichite" => SpecialMove::Sennichite,
            "jishogi" => SpecialMove::Jishogi,
            "max_moves" => SpecialMove::MaxMoves,
            "abnormal" => SpecialMove::Interrupt,
            _ => return None,
        })
    }
}

/// CSA棋譜から抽出した対局メタデータ
#[derive(Clone, Debug, Default)]
pub struct GameInfo {
    pub black_name: Option<String>,
    pub white_name: Option<String>,
    pub black_rating: Option<f64>,
    pub white_rating: Option<f64>,
    /// `'summary:` 行（あれば最後のもの）。勝敗の判定では終局コマンドより優先する。
    pub summary: Option<CsaSummary>,
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
///
/// 持ち駒の設定は先後・駒種ごとに255枚まで受理し、超過時はエラーを返す。
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

/// CSA棋譜を完全パース。既存APIとの互換性のため評価コメントは返さない。
pub fn parse_csa_full(text: &str) -> Result<(Position, Vec<ParsedMove>, GameInfo)> {
    let (pos, moves, info, _) = parse_csa_full_with_evals(text)?;
    Ok((pos, moves, info))
}

/// CSA棋譜を完全パースし、通常手と同じ順序の先手視点評価値も返す。
///
/// 旧版 csa_client の `'*` は直後の通常手、wdoor と現行 csa_client の `'**` は直前の通常手に対応する。
pub fn parse_csa_full_with_evals(text: &str) -> Result<ParsedCsaWithEvals> {
    parse_csa_full_with_evals_style(text, EvalCommentStyle::Standard)
}

/// 評価コメント形式を明示してCSA棋譜を完全パースする。
pub fn parse_csa_full_with_evals_style(
    text: &str,
    eval_style: EvalCommentStyle,
) -> Result<ParsedCsaWithEvals> {
    let mut pos = None;
    let mut moves = Vec::new();
    let mut evals = Vec::new();
    let mut info = GameInfo::default();
    let mut explicit_board = false;
    let mut pending_eval_cp_black = None;
    let mut follows_inline_timed_move = false;
    for line in text.lines() {
        let comment_follows_inline_timed_move = follows_inline_timed_move;
        follows_inline_timed_move = false;
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
        // wdoor / shogi-server: 直前の通常手を探索した先手視点評価値。
        if let Some(rest) = s.strip_prefix("'**") {
            if let (Some(cp), Some(last)) = (parse_leading_eval_cp(rest), evals.last_mut()) {
                *last = Some(cp);
            }
            continue;
        }
        // rshogi csa_client: 直後の通常手を探索した先手視点評価値。
        if let Some(rest) = s.strip_prefix("'*") {
            let cp = parse_leading_eval_cp(rest);
            if eval_style == EvalCommentStyle::LegacyServerPost && comment_follows_inline_timed_move
            {
                if let Some(last) = evals.last_mut() {
                    *last = cp;
                }
            } else {
                pending_eval_cp_black = cp;
            }
            continue;
        }
        if let Some(rest) = s.strip_prefix("'summary:") {
            if let Some(summary) = CsaSummary::parse(rest) {
                info.summary = Some(summary);
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
        if is_csa_move_line(s) {
            let mv = s[..7].to_string();
            // インライン消費時間: +7776FU,T30 / +7776FU,T30,'コメント
            let time_sec = s
                .get(7..)
                .and_then(|rest| rest.strip_prefix(",T"))
                .and_then(|t| t.split(',').next())
                .and_then(|t| t.parse::<u32>().ok());
            moves.push(ParsedMove::Normal(CsaMove { mv, time_sec }));
            evals.push(pending_eval_cp_black.take());
            follows_inline_timed_move = time_sec.is_some();
            continue;
        }
    }
    let pos = pos.unwrap_or_else(initial_position);
    Ok((pos, moves, info, evals))
}

fn parse_leading_eval_cp(rest: &str) -> Option<i32> {
    rest.split_whitespace().next()?.parse().ok()
}

fn is_csa_move_line(s: &str) -> bool {
    let bytes = s.as_bytes();
    bytes.len() >= 7
        && matches!(bytes[0], b'+' | b'-')
        && bytes[1..5].iter().all(u8::is_ascii_digit)
        && bytes[5..7].iter().all(u8::is_ascii_uppercase)
}

fn validate_csa_move(mv: &str) -> Result<()> {
    anyhow::ensure!(is_csa_move_line(mv), "invalid CSA move format: {mv}");
    let bytes = mv.as_bytes();
    anyhow::ensure!(
        &bytes[1..3] == b"00" || bytes[1..3].iter().all(|byte| (b'1'..=b'9').contains(byte)),
        "invalid CSA move source: {mv}"
    );
    anyhow::ensure!(
        bytes[3..5].iter().all(|byte| (b'1'..=b'9').contains(byte)),
        "invalid CSA move destination: {mv}"
    );
    Ok(())
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
        // 数字以外の byte でも underflow させず、後段の範囲チェックで弾く。
        let file = bytes[i].wrapping_sub(b'0');
        let rank = bytes[i + 1].wrapping_sub(b'0');
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

/// USIの筋(1-9) → 数値
fn usi_file_to_num(c: u8) -> Result<u8> {
    anyhow::ensure!((b'1'..=b'9').contains(&c), "invalid USI file char: {}", c as char);
    Ok(c - b'0')
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
/// 先頭7 byteを解釈し、それ以降の消費時間やコメントは無視する。
///
/// # Errors
/// 指し手の符号・座標・駒種が不正な場合はエラーを返す。
pub fn csa_move_to_usi(mv: &str, pos: &Position) -> Result<String> {
    validate_csa_move(mv)?;
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
        let to_file = usi_file_to_num(bytes[2])?;
        let to_rank = usi_rank_to_num(bytes[3])?;
        write!(out, "00{}{}{}", to_file, to_rank, csa_code).unwrap();
    } else {
        // 通常手: 7g7f → +7776FU
        let from_file = usi_file_to_num(bytes[0])?;
        let from_rank = usi_rank_to_num(bytes[1])?;
        let to_file = usi_file_to_num(bytes[2])?;
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

/// Result を返さない版（盤面出力用。Gold/King は成れないため base code にフォールバック）
fn promoted_csa_code_static(pt: PieceType) -> &'static str {
    use PieceType::*;
    match pt {
        Pawn => "TO",
        Lance => "NY",
        Knight => "NK",
        Silver => "NG",
        Bishop => "UM",
        Rook => "RY",
        Gold | King => base_csa_code(pt),
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
        let (_, moves, _, _evals) = parse_csa_full_with_evals(text).unwrap();
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
    fn test_parse_time_inline_followed_by_comment() {
        let text = "PI\n+7776FU,T5,'* 30 -3334FU, +2726FU\n-3334FU,'* -20\n";
        let (_, moves, _, _evals) = parse_csa_full_with_evals(text).unwrap();
        let times: Vec<Option<u32>> = moves
            .iter()
            .map(|m| match m {
                ParsedMove::Normal(cm) => cm.time_sec,
                ParsedMove::Special(_) => None,
            })
            .collect();
        assert_eq!(times, vec![Some(5), None]);
    }

    #[test]
    fn test_parse_time_standalone_t_line() {
        let text = "PI\n+7776FU\nT5\n-3334FU\nT10\n";
        let (_, moves, _, _evals) = parse_csa_full_with_evals(text).unwrap();
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
    fn test_parse_eval_comments_for_both_record_styles() {
        let text = concat!(
            "V2.2\r\nPI\r\n",
            "'* 45 -3334FU\r\n",
            "+7776FU\r\nT5\r\n",
            "-3334FU,T10\r\n",
            "'** -30 +2726FU\r\n",
            "%TORYO\r\n",
        );
        let (_, moves, _, evals) = parse_csa_full_with_evals(text).unwrap();
        let normal: Vec<_> = moves
            .iter()
            .filter_map(|m| match m {
                ParsedMove::Normal(cm) => Some(cm),
                ParsedMove::Special(_) => None,
            })
            .collect();
        assert_eq!(normal.len(), 2);
        assert_eq!(evals[0], Some(45));
        assert_eq!(normal[0].time_sec, Some(5));
        assert_eq!(evals[1], Some(-30));
        assert_eq!(normal[1].time_sec, Some(10));
    }

    #[test]
    fn test_last_eval_comment_wins_and_invalid_pending_is_cleared() {
        let text = "PI\n'* 5\n'* invalid\n+7776FU\n'* 10\n-3334FU\n'** invalid\n'** 20 pv\n";
        let (_, _moves, _, evals) = parse_csa_full_with_evals(text).unwrap();
        assert_eq!(evals[0], None);
        assert_eq!(evals[1], Some(20));
    }

    #[test]
    fn test_legacy_server_single_star_after_inline_timed_move_belongs_to_previous_move() {
        let text = "PI\n+7776FU,T1\n'* 100\n-3334FU,T1\n";
        let (_, _, _, evals) =
            parse_csa_full_with_evals_style(text, EvalCommentStyle::LegacyServerPost).unwrap();
        assert_eq!(evals, vec![Some(100), None]);
    }

    #[test]
    fn test_standard_single_star_after_inline_move_remains_pre_move() {
        let text = "$GAME_ID:external\nPI\n+7776FU,T1\n'* 100\n-3334FU,T1\n";
        let (_, _, _, evals) = parse_csa_full_with_evals(text).unwrap();
        assert_eq!(evals, vec![None, Some(100)]);
    }

    #[test]
    fn test_malformed_signed_line_is_not_a_normal_move() {
        let text = "PI\n'* 10\n+abcdef\n+7776FU\n";
        let (_, moves, _, evals) = parse_csa_full_with_evals(text).unwrap();
        assert_eq!(moves.len(), 1);
        let ParsedMove::Normal(cm) = &moves[0] else {
            panic!("expected normal move");
        };
        assert_eq!(cm.mv, "+7776FU");
        assert_eq!(evals, vec![Some(10)]);
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
    fn test_from_sfen_roundtrip() {
        for sfen in [
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
            "ln1g1g1nl/1ks2r3/1pppp1bpp/p3spp2/9/P1P1SP1PP/1P1PP1P2/1BK1GR3/LNSG3NL w - 20",
            "4k4/9/9/9/9/9/9/9/4K4 w RB2G2S2N2L9Prb2g2s2n2l9p 101",
            "+P+L+N+S3k1/9/9/9/9/9/9/9/+p+l+n+s+B+R1K1 b 12P 7",
        ] {
            assert_eq!(Position::from_sfen(sfen).unwrap().to_sfen(), sfen);
        }
        // 手数省略時は 1。
        assert_eq!(Position::from_sfen("4k4/9/9/9/9/9/9/9/4K4 b -").unwrap().ply, 1);
    }

    #[test]
    fn test_from_sfen_rejects_bad_hand_counts() {
        for hands in [
            "256P",
            "19P",
            "0P",
            "4294967296P",
            "99999999999999999999P",
            "1",
            "2",
        ] {
            let sfen = format!("4k4/9/9/9/9/9/9/9/4K4 b {hands} 1");
            assert!(Position::from_sfen(&sfen).is_err(), "should reject: {sfen}");
        }
        let pos = Position::from_sfen("4k4/9/9/9/9/9/9/9/4K4 b 18P2r 1").unwrap();
        assert_eq!(pos.to_sfen(), "4k4/9/9/9/9/9/9/9/4K4 b 18P2r 1");
    }

    #[test]
    fn test_usi_move_to_csa_rejects_garbage_without_panicking() {
        let pos = initial_position();
        for mv in [
            "(3c3d)", "/g7f", "7g/f", "P*/f", " g7f", "7g7", "", "resign", "abcd", "7x7f",
        ] {
            assert!(usi_move_to_csa(mv, &pos).is_err(), "should reject: {mv:?}");
        }
        assert_eq!(usi_move_to_csa("7g7f", &pos).unwrap(), "+7776FU");
    }

    #[test]
    fn test_parse_summary_line() {
        let parse = |tail: &str| {
            let text = format!("V2\nN+A\nN-B\nPI\n+\n+7776FU\nT1\n{tail}");
            parse_csa_full(&text).unwrap().2.summary
        };
        let s = parse("%TORYO\n'summary:toryo:A win:B lose\n").unwrap();
        assert_eq!((s.reason.as_str(), s.result), ("toryo", SummaryResult::BlackWin));
        assert_eq!(s.special_move(), Some(SpecialMove::Resign));
        // 名前に `+hash` や空白が入っても、勝敗は並び順と末尾の語で決まる。
        let s = parse("'summary:time up:A+0123 lose:B x win\n").unwrap();
        assert_eq!((s.reason.as_str(), s.result), ("time up", SummaryResult::WhiteWin));
        assert_eq!(s.special_move(), Some(SpecialMove::TimeUp));
        let s = parse("'summary:sennichite:A draw:B draw\n").unwrap();
        assert_eq!(s.result, SummaryResult::Draw);
        for (reason, expected) in [
            ("illegal move", Some(SpecialMove::IllegalMove)),
            ("oute_kaihimore", Some(SpecialMove::IllegalMove)),
            ("oute_sennichite", Some(SpecialMove::IllegalMove)),
            ("kachi", Some(SpecialMove::Win)),
            ("max_moves", Some(SpecialMove::MaxMoves)),
            ("abnormal", Some(SpecialMove::Interrupt)),
            ("something new", None),
        ] {
            let s = parse(&format!("'summary:{reason}:A win:B lose\n")).unwrap();
            assert_eq!(s.special_move(), expected, "{reason}");
        }
        // 勝敗が読めない行・summary のない棋譜は None。
        assert_eq!(parse("'summary:toryo:A win\n"), None);
        assert_eq!(parse("'summary:abnormal:A win:B win\n"), None);
        assert_eq!(parse("%TORYO\n"), None);
    }

    #[test]
    fn test_from_sfen_rejects_malformed() {
        for sfen in [
            "",
            "4k4/9/9/9/9/9/9/9 b - 1",
            "4k5/9/9/9/9/9/9/9/4K4 b - 1",
            "4k4/9/9/9/9/9/9/9/4K4 x - 1",
            "4k4/9/9/9/9/9/9/9/4K4 b 2 1",
            "4+k4/9/9/9/9/9/9/9/4K4 b - 1",
            "4k4/9/9/9/9/9/9/9/4K4 b K 1",
        ] {
            assert!(Position::from_sfen(sfen).is_err(), "should reject: {sfen:?}");
        }
    }

    #[test]
    fn test_to_csa_board_roundtrip() {
        // 平手初期局面 → to_csa_board → parse_csa → to_sfen で一致確認
        let pos = initial_position();
        let csa_board = pos.to_csa_board();
        let (parsed, _, _) = parse_csa(&csa_board).unwrap();
        assert_eq!(parsed.to_sfen(), pos.to_sfen());
    }

    #[test]
    fn test_to_csa_board_with_hand() {
        // 持ち駒ありの局面
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
P+00FU00FU
P-00KA
+
";
        let (pos, _, _) = parse_csa(text).unwrap();
        let csa_board = pos.to_csa_board();
        let (reparsed, _, _) = parse_csa(&csa_board).unwrap();
        assert_eq!(reparsed.to_sfen(), pos.to_sfen());
        assert_eq!(reparsed.hand_b.p, 2);
        assert_eq!(reparsed.hand_w.b, 1);
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

    #[test]
    fn move_notations_match_fixed_vectors() {
        let pos = initial_position();
        for (csa, usi) in [("+7776FU", "7g7f"), ("+8822UM", "8h2b+")] {
            assert_eq!(csa_move_to_usi(csa, &pos).unwrap(), usi);
            assert_eq!(usi_move_to_csa(usi, &pos).unwrap(), csa);
        }
        assert_eq!(csa_move_to_usi("-3334FU", &pos).unwrap(), "3c3d");
        let (pos, _, _) = parse_csa("P+55OU\nP-51OU\nP+00FU\n+\n").unwrap();
        assert_eq!(csa_move_to_usi("+0076FU", &pos).unwrap(), "P*7f");
        assert_eq!(usi_move_to_csa("P*7f", &pos).unwrap(), "+0076FU");
    }

    #[test]
    fn non_digit_square_in_setup_lines_is_an_error_not_a_panic() {
        // b'0' 未満の byte (空白や `*`) を含んでも減算 underflow で panic しない。
        for text in ["P+ *FU\n", "P-5 OU\n", "PI *HI\n", "PI8 HI\n"] {
            assert!(parse_csa(text).is_err(), "text={text:?}");
        }
    }

    #[test]
    fn csa_move_apis_reject_malformed_tokens_without_mutating_position() {
        let mut invalid: Vec<String> = [
            "", "+", "+7776F", "+ 776FU", "+/776FU", "+7\n76FU", "+7776F?", "+7776ZZ", "x7776FU",
            "+0776FU", "+7076FU", "+7706FU", "+7770FU", "+777AFU", "+0055OU",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        for index in 0..7 {
            let mut token = "+7776FU".to_owned();
            token.replace_range(index..index + 1, "あ");
            invalid.push(token);
        }
        let mut pos = initial_position();
        let before = pos.to_sfen();
        for token in invalid {
            assert!(csa_move_to_usi(&token, &pos).is_err(), "token={token:?}");
            assert!(pos.apply_csa_move(&token).is_err(), "token={token:?}");
            assert_eq!(pos.to_sfen(), before, "token={token:?}");
        }
    }

    #[test]
    fn apply_csa_move_errors_preserve_position() {
        let mut pos = initial_position();
        let before = pos.to_sfen();
        for token in [
            "+5553FU", "+3353FU", "+0053HI", "+0055FU", "+7777FU", "-3334FU",
        ] {
            assert!(pos.apply_csa_move(token).is_err(), "token={token}");
            assert_eq!(pos.to_sfen(), before, "token={token}");
        }
    }

    #[test]
    fn csa_and_sfen_hand_setup_check_cumulative_capacity() {
        for (code, sfen_piece) in [
            ("FU", 'P'),
            ("KY", 'L'),
            ("KE", 'N'),
            ("GI", 'S'),
            ("KI", 'G'),
            ("KA", 'B'),
            ("HI", 'R'),
        ] {
            for side in ['+', '-'] {
                let piece = if side == '+' {
                    sfen_piece
                } else {
                    sfen_piece.to_ascii_lowercase()
                };
                let unit = format!("00{code}");
                let setup =
                    format!("P{side}{}\nP{side}{}\n+\n", unit.repeat(128), unit.repeat(127));
                let (pos, _, _) = parse_csa(&setup).unwrap();
                assert_eq!(pos.to_sfen(), format!("9/9/9/9/9/9/9/9/9 b 255{piece} 1"));
                assert!(parse_csa(&format!("{setup}P{side}{unit}\n")).is_err());
                assert!(parse_csa(&format!("P{side}{}\n+\n", unit.repeat(256))).is_err());

                let sfen = format!("9/9/9/9/9/9/9/9/9 b {} 1", piece.to_string().repeat(255));
                let pos = Position::from_sfen(&sfen).unwrap();
                assert_eq!(pos.to_sfen(), format!("9/9/9/9/9/9/9/9/9 b 255{piece} 1"));
                let overflow = format!("9/9/9/9/9/9/9/9/9 b {} 1", piece.to_string().repeat(256));
                assert!(Position::from_sfen(&overflow).is_err());
            }
        }
    }

    #[test]
    fn apply_csa_capture_checks_hand_capacity_without_partial_updates() {
        for (code, sfen_piece) in [
            ("FU", 'P'),
            ("KY", 'L'),
            ("KE", 'N'),
            ("GI", 'S'),
            ("KI", 'G'),
            ("KA", 'B'),
            ("HI", 'R'),
        ] {
            for (side, opponent, target, mv) in
                [('+', '-', "54", "+5554FU"), ('-', '+', "56", "-5556FU")]
            {
                let piece = if side == '+' {
                    sfen_piece
                } else {
                    sfen_piece.to_ascii_lowercase()
                };
                for count in [254, 255] {
                    let setup = format!(
                        "P{side}55FU\nP{opponent}{target}{code}\nP{side}{}\n{side}\n",
                        format!("00{code}").repeat(count)
                    );
                    let (mut pos, _, _) = parse_csa(&setup).unwrap();
                    let before = pos.to_sfen();
                    let result = pos.apply_csa_move(mv);
                    if count == 254 {
                        result.unwrap();
                        assert_eq!(
                            pos.to_sfen().split_whitespace().nth(2).unwrap(),
                            format!("255{piece}")
                        );
                        assert_eq!(pos.ply, 2);
                    } else {
                        assert!(result.is_err());
                        assert_eq!(pos.to_sfen(), before);
                    }
                }
            }
        }
    }

    #[test]
    fn apply_csa_move_checks_ply_before_mutating_position() {
        let mut pos = initial_position();
        pos.ply = u32::MAX - 1;
        pos.apply_csa_move("+7776FU").unwrap();
        assert_eq!(pos.ply, u32::MAX);
        let before = pos.to_sfen();
        assert!(pos.apply_csa_move("-3334FU").is_err());
        assert_eq!(pos.to_sfen(), before);
    }

    #[test]
    fn csa_move_suffixes_preserve_apply_and_conversion() {
        let mut white = initial_position();
        white.side_to_move = Color::White;
        let drop = Position::from_sfen("4k4/9/9/9/9/9/9/9/4K4 b B 1").unwrap();
        for (pos, token, usi) in [
            (initial_position(), "+7776FU", "7g7f"),
            (initial_position(), "+8822UM", "8h2b+"),
            (white, "-3334FU", "3c3d"),
            (drop, "+0055KA", "B*5e"),
        ] {
            let mut expected = pos.clone();
            expected.apply_csa_move(token).unwrap();
            for suffix in ["", ",T30", ",T30,'日本語コメント", ",日本語", "extra"] {
                let full_token = format!("{token}{suffix}");
                assert_eq!(csa_move_to_usi(&full_token, &pos).unwrap(), usi);
                let mut actual = pos.clone();
                actual.apply_csa_move(&full_token).unwrap();
                assert_eq!(actual.to_sfen(), expected.to_sfen());
            }
        }
    }
}
