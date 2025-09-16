use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::time::{Duration, Instant};

use clap::Parser;
use engine_core::shogi::{
    piece_constants::piece_type_to_hand_index, Color, Piece, PieceType, Position,
};
use engine_core::usi::{parse_usi_square, position_to_sfen};
use serde::Serialize;
use tools::io_detect::open_maybe_compressed_reader;

const RECORD_SIZE_YO_V1: usize = 40;
const MATE_SCORE_THRESH: i32 = 31_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StrictMode {
    FailClosed,
    AllowSkip,
    MaxErrors(u64),
}

#[derive(Parser, Debug)]
#[command(
    name = "psv2jsonl",
    about = "Convert YaneuraOu PSV (yo_v1) to JSONL for training"
)]
struct Opt {
    #[arg(short = 'i', long = "input", default_value = "-")]
    input: String,
    #[arg(short = 'o', long = "output", default_value = "-")]
    output: String,

    #[arg(long = "with-pv")]
    with_pv: bool,
    /// For yo_v1 only 1 is supported (first move). Values >1 are ignored.
    #[arg(long = "pv-max-moves", default_value_t = 1)]
    pv_max_moves: usize,

    /// Format selector: yo_v1 or auto (auto is not recommended). Default: yo_v1
    #[arg(long = "format", default_value = "yo_v1")]
    format: String,

    /// Strict mode: fail-closed | allow-skip | max-errors=N
    #[arg(long = "strict", default_value = "fail-closed")]
    strict: String,

    /// If set, coerce invalid gamePly=0 to 1 instead of error/skip (still counts as error when strict=max-errors)
    #[arg(long = "coerce-ply-min-1")]
    coerce_ply_min_1: bool,

    /// Input buffer MB
    #[arg(long = "io-buf-mb", default_value_t = 4)]
    io_buf_mb: usize,

    /// Limit number of records to process (for testing)
    #[arg(long = "limit")]
    limit: Option<u64>,

    /// Sample rate 0.0..1.0 (process only a fraction of records)
    #[arg(long = "sample-rate", default_value_t = 1.0)]
    sample_rate: f64,

    /// Metrics output: plain | json
    #[arg(long = "metrics", default_value = "plain")]
    metrics: String,
    /// Metrics interval in seconds
    #[arg(long = "metrics-interval", default_value_t = 5u64)]
    metrics_interval_sec: u64,

    /// For STDIN only: decompress kind (gz|zst)
    #[arg(long = "decompress")]
    decompress: Option<String>,
}

fn parse_strict(s: &str) -> StrictMode {
    let s = s.trim().to_ascii_lowercase();
    if s == "fail-closed" {
        StrictMode::FailClosed
    } else if s == "allow-skip" {
        StrictMode::AllowSkip
    } else if let Some(rest) = s.strip_prefix("max-errors=") {
        let n = rest.parse::<u64>().unwrap_or(0);
        StrictMode::MaxErrors(n)
    } else {
        StrictMode::FailClosed
    }
}

#[derive(Debug, Clone, Copy)]
struct YoV1Header {
    side_to_move: Color, // 0=Black,1=White
}

#[derive(Debug)]
struct DecodeResult {
    sfen: String,
    coerced_gameply: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    let opt = Opt::parse();

    // Validate format (yo_v1 only)
    let fmt = opt.format.to_ascii_lowercase();
    if fmt != "yo_v1" && fmt != "auto" {
        eprintln!("Error: --format must be yo_v1|auto");
        std::process::exit(2);
    }
    if fmt == "auto" {
        eprintln!("Warning: --format=auto is treated as yo_v1 in this version.");
    }

    let mut reader: Box<dyn BufRead> = if opt.input == "-" {
        let stdin = io::stdin();
        let stdin_lock = stdin.lock();
        let base: Box<dyn BufRead> =
            Box::new(BufReader::with_capacity(opt.io_buf_mb * 1024 * 1024, stdin_lock));
        match opt.decompress.as_deref() {
            Some("gz") => {
                use flate2::read::MultiGzDecoder;
                Box::new(BufReader::with_capacity(
                    opt.io_buf_mb * 1024 * 1024,
                    MultiGzDecoder::new(base),
                ))
            }
            Some("zst") => {
                #[cfg(feature = "zstd")]
                {
                    Box::new(BufReader::with_capacity(
                        opt.io_buf_mb * 1024 * 1024,
                        zstd::Decoder::new(base)?,
                    ))
                }
                #[cfg(not(feature = "zstd"))]
                {
                    eprintln!("Error: stdin looks compressed with zstd but binary built without 'zstd' feature");
                    std::process::exit(2);
                }
            }
            Some(other) => {
                eprintln!("Error: unsupported --decompress kind: {}", other);
                std::process::exit(2);
            }
            None => base,
        }
    } else {
        // Use magic-based detection for files (supports gz/zst)
        let buf_bytes = opt.io_buf_mb * 1024 * 1024;
        let r = open_maybe_compressed_reader(&opt.input, buf_bytes)?;
        Box::new(r)
    };

    let mut writer: Box<dyn Write> = if opt.output == "-" {
        let out = io::stdout();
        Box::new(BufWriter::with_capacity(1 << 20, out.lock()))
    } else {
        let f = File::create(&opt.output)?;
        Box::new(BufWriter::with_capacity(1 << 20, f))
    };

    let strict = parse_strict(&opt.strict);
    let s = opt.strict.to_ascii_lowercase();
    if !(s == "fail-closed" || s == "allow-skip" || s.starts_with("max-errors=")) {
        eprintln!("Error: --strict must be 'fail-closed' | 'allow-skip' | 'max-errors=N'");
        std::process::exit(2);
    }
    let start_time = Instant::now();
    let mut processed: u64 = 0;
    let mut success: u64 = 0;
    let mut skipped: u64 = 0;
    let mut errors: u64 = 0;
    let mut invalid_gameply: u64 = 0;
    let mut last_metrics = Instant::now();
    let metrics_interval = Duration::from_secs(opt.metrics_interval_sec.max(1));
    let mut rec_buf = [0u8; RECORD_SIZE_YO_V1];
    let mut total_bytes: u64 = 0;

    let mut warned_pvmax: bool = false;
    'outer: loop {
        match read_one_record(&mut reader, &mut rec_buf) {
            Ok(true) => {
                total_bytes += RECORD_SIZE_YO_V1 as u64;
                processed += 1;
                if opt.sample_rate < 1.0 && !sample_hit(&rec_buf, opt.sample_rate) {
                    continue;
                }

                match process_one_record(&rec_buf, opt.coerce_ply_min_1) {
                    Ok(DecodeResult {
                        sfen,
                        coerced_gameply,
                    }) => {
                        if coerced_gameply {
                            invalid_gameply += 1;
                            errors += 1;
                            log_err_json(
                                "coerced_gameply",
                                processed,
                                total_bytes,
                                "gamePly=0 -> 1",
                                Some(&sfen),
                            );
                        }
                        // score i16 LE at offset 32
                        let eval = i16::from_le_bytes([rec_buf[32], rec_buf[33]]) as i32;
                        let mut rec_out = JsonRec {
                            sfen: &sfen,
                            eval,
                            mate_boundary: None,
                            lines: None,
                        };
                        if eval.abs() >= MATE_SCORE_THRESH {
                            rec_out.mate_boundary = Some(true);
                        }
                        if opt.with_pv {
                            if opt.pv_max_moves > 1 && !warned_pvmax {
                                eprintln!(
                                    "Warning: --pv-max-moves > 1 is not supported for yo_v1; using 1"
                                );
                                warned_pvmax = true;
                            }
                            if let Some(line) = first_move_usi_from_move16(rec_buf[34], rec_buf[35])
                            {
                                rec_out.lines = Some(vec![JsonLine {
                                    score_cp: eval,
                                    multipv: 1,
                                    pv: vec![line],
                                }]);
                            }
                        }
                        serde_json::to_writer(&mut writer, &rec_out)?;
                        writer.write_all(b"\n")?;
                        success += 1;
                        if let Some(limit) = opt.limit {
                            if success >= limit {
                                break 'outer;
                            }
                        }
                    }
                    Err(RecError::InvalidGamePly {
                        game_ply,
                        sfen_stub,
                    }) => {
                        invalid_gameply += 1;
                        match strict {
                            StrictMode::FailClosed => {
                                log_err_json(
                                    "invalid_gameply",
                                    processed,
                                    total_bytes,
                                    &format!("gamePly={}", game_ply),
                                    sfen_stub.as_deref(),
                                );
                                std::process::exit(2);
                            }
                            StrictMode::AllowSkip | StrictMode::MaxErrors(_) => {
                                log_err_json(
                                    "invalid_gameply",
                                    processed,
                                    total_bytes,
                                    &format!("gamePly={}", game_ply),
                                    sfen_stub.as_deref(),
                                );
                                skipped += 1;
                                errors += 1;
                            }
                        }
                    }
                    Err(RecError::DecodeError { kind, detail }) => match strict {
                        StrictMode::FailClosed => {
                            log_err_json(kind, processed, total_bytes, &detail, None);
                            std::process::exit(2);
                        }
                        StrictMode::AllowSkip | StrictMode::MaxErrors(_) => {
                            log_err_json(kind, processed, total_bytes, &detail, None);
                            skipped += 1;
                            errors += 1;
                        }
                    },
                }

                if let StrictMode::MaxErrors(maxn) = strict {
                    if maxn > 0 && errors >= maxn {
                        eprintln!("Reached max-errors={}", maxn);
                        std::process::exit(3);
                    }
                }

                if last_metrics.elapsed() >= metrics_interval {
                    let elapsed = start_time.elapsed();
                    let elapsed_secs = elapsed.as_secs_f64();
                    let mb = total_bytes as f64 / (1024.0 * 1024.0);
                    let rps = if elapsed_secs > 0.0 {
                        processed as f64 / elapsed_secs
                    } else {
                        0.0
                    };
                    match opt.metrics.as_str() {
                        s if s.eq_ignore_ascii_case("json") => {
                            let m = serde_json::json!({
                                "kind":"metrics","processed":processed,"success":success,"skipped":skipped,
                                "errors":errors,"invalid_gameply":invalid_gameply,
                                "in_mb":mb,
                                "rps":rps,
                            });
                            eprintln!("{}", m);
                        }
                        _ => {
                            eprint!(
                                "\rprocessed={} success={} skipped={} errors={} invalid_gameply={} in_mb={:.3} rps={:.1}",
                                processed, success, skipped, errors, invalid_gameply, mb, rps
                            );
                        }
                    }
                    last_metrics = Instant::now();
                }
            }
            Ok(false) => break,
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                eprintln!("Error: {}", e);
                std::process::exit(2);
            }
            Err(e) => {
                eprintln!("Error reading input: {}", e);
                std::process::exit(2);
            }
        }
    }

    // Final newline for plain metrics line
    if opt.metrics.eq_ignore_ascii_case("plain") {
        eprintln!();
    }
    let _ = writer.flush();

    Ok(())
}

fn log_err_json(
    kind: &str,
    record_index: u64,
    byte_offset: u64,
    detail: &str,
    sfen_stub: Option<&str>,
) {
    let mut obj = serde_json::json!({
        "level":"error",
        "kind": kind,
        "record_index": record_index,
        "byte_offset": byte_offset,
        "detail": detail,
    });
    if let Some(s) = sfen_stub {
        obj.as_object_mut().unwrap().insert("sfen".to_string(), serde_json::json!(s));
    }
    eprintln!("{}", obj);
}

#[derive(Debug)]
enum RecError {
    DecodeError {
        kind: &'static str,
        detail: String,
    },
    InvalidGamePly {
        game_ply: u16,
        sfen_stub: Option<String>,
    },
}

fn process_one_record(
    rec: &[u8; RECORD_SIZE_YO_V1],
    coerce_ply_min_1: bool,
) -> Result<DecodeResult, RecError> {
    // Read header fields
    let packed: &[u8; 32] = rec[..32].try_into().unwrap();
    let _score = i16::from_le_bytes([rec[32], rec[33]]) as i32;
    let game_ply = u16::from_le_bytes([rec[36], rec[37]]);
    // Read side/move from packed: side at bit0 of stream

    // Decode PackedSfen to Position
    let (mut pos, header) = decode_packed_sfen(packed).map_err(|e| RecError::DecodeError {
        kind: "decode_sfen",
        detail: e,
    })?;

    // gamePly policy
    let mut gp = game_ply;
    let mut coerced = false;
    if gp == 0 {
        if coerce_ply_min_1 {
            gp = 1;
            coerced = true;
        } else {
            // reflect side-to-move for stub
            pos.side_to_move = header.side_to_move;
            let stub = Some(position_to_sfen(&pos));
            return Err(RecError::InvalidGamePly {
                game_ply,
                sfen_stub: stub,
            });
        }
    }

    // Set side_to_move/ply to reproduce provided move count exactly
    pos.side_to_move = header.side_to_move;
    let move_count = gp as u32;
    pos.ply = ((move_count - 1) * 2
        + if pos.side_to_move == Color::White {
            1
        } else {
            0
        }) as u16;

    let sfen = position_to_sfen(&pos);
    Ok(DecodeResult {
        sfen,
        coerced_gameply: coerced,
    })
}

fn first_move_usi_from_move16(lo: u8, hi: u8) -> Option<String> {
    let m = u16::from_le_bytes([lo, hi]);
    let drop = (m & 0x4000) != 0;
    let promote = (m & 0x8000) != 0;
    let to_idx = (m & 0x7f) as u8; // 0..80
    if to_idx > 80 {
        return None;
    }
    let to_usi = sqidx_to_usi(to_idx);
    if drop {
        let pt_code = ((m >> 7) & 0x7f) as u8; // 1..7
        let piece_char = match pt_code {
            1 => 'P',
            2 => 'L',
            3 => 'N',
            4 => 'S',
            5 => 'B',
            6 => 'R',
            7 => 'G',
            _ => return None,
        };
        Some(format!("{}*{}", piece_char, to_usi))
    } else {
        let from_idx = ((m >> 7) & 0x7f) as u8;
        if from_idx > 80 {
            return None;
        }
        let from_usi = sqidx_to_usi(from_idx);
        if promote {
            Some(format!("{}{}+", from_usi, to_usi))
        } else {
            Some(format!("{}{}", from_usi, to_usi))
        }
    }
}

fn sqidx_to_usi(idx: u8) -> String {
    let f = idx / 9; // 0..8 -> '1'..'9'
    let r = idx % 9; // 0..8 -> 'a'..'i'
    let file_ch = (b'1' + f) as char;
    let rank_ch = (b'a' + r) as char;
    format!("{}{}", file_ch, rank_ch)
}

// Read exactly one fixed-size record; detect truncated EOF (fail-closed)
fn read_one_record<R: Read>(r: &mut R, buf: &mut [u8; RECORD_SIZE_YO_V1]) -> io::Result<bool> {
    let mut off = 0usize;
    while off < RECORD_SIZE_YO_V1 {
        let n = r.read(&mut buf[off..])?;
        if n == 0 {
            return if off == 0 {
                Ok(false) // clean EOF
            } else {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!("truncated record: got {} bytes (need {})", off, RECORD_SIZE_YO_V1),
                ))
            };
        }
        off += n;
    }
    Ok(true)
}

// JSON output structs with deterministic field ordering
#[derive(Serialize)]
struct JsonRec<'a> {
    sfen: &'a str,
    eval: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    mate_boundary: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    lines: Option<Vec<JsonLine>>, // only with --with-pv (yo_v1 supports only first move)
}

#[derive(Serialize)]
struct JsonLine {
    score_cp: i32,
    multipv: u8,
    pv: Vec<String>,
}

// ---- PackedSfen decoder (yo_v1) ----

struct BitReader<'a> {
    data: &'a [u8; 32],
    cursor: usize, // bit cursor 0..=256
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8; 32]) -> Self {
        Self { data, cursor: 0 }
    }
    fn read_one(&mut self) -> Result<u8, String> {
        if self.cursor >= 256 {
            return Err("bit cursor overflow".to_string());
        }
        let b = (self.data[self.cursor / 8] >> (self.cursor & 7)) & 1;
        self.cursor += 1;
        Ok(b)
    }
    fn read_n(&mut self, n: usize) -> Result<u32, String> {
        let mut v = 0u32;
        for i in 0..n {
            v |= (self.read_one()? as u32) << i;
        }
        Ok(v)
    }
}

fn decode_packed_sfen(data: &[u8; 32]) -> Result<(Position, YoV1Header), String> {
    let mut r = BitReader::new(data);
    let stm_bit = r.read_one().map_err(|e| format!("read stm: {e}"))?;
    let side = if stm_bit == 0 {
        Color::Black
    } else {
        Color::White
    };
    let bk = r.read_n(7).map_err(|e| format!("read bk: {e}"))? as u8;
    let wk = r.read_n(7).map_err(|e| format!("read wk: {e}"))? as u8;
    if bk > 80 || wk > 80 {
        return Err("king square out of range".into());
    }

    // Initialize empty Position and seed kings
    let mut pos = Position::empty();
    // Place kings
    let bk_sq = parse_usi_square(&sqidx_to_usi(bk)).map_err(|e| format!("king parse {}", e))?;
    let wk_sq = parse_usi_square(&sqidx_to_usi(wk)).map_err(|e| format!("king parse {}", e))?;
    pos.board.put_piece(bk_sq, Piece::new(PieceType::King, Color::Black));
    pos.board.put_piece(wk_sq, Piece::new(PieceType::King, Color::White));

    // Board tokens (81 squares, skipping kings)
    for idx in 0u8..=80u8 {
        if idx == bk || idx == wk {
            continue;
        }
        let (piece_opt, consumed) = read_board_piece(&mut r)?;
        let _ = consumed; // always consumed via read_board_piece
        if let Some(pc) = piece_opt {
            // Map square
            let sq =
                parse_usi_square(&sqidx_to_usi(idx)).map_err(|e| format!("square parse {}", e))?;
            pos.board.put_piece(sq, pc);
        }
    }

    // Hands / piecebox until cursor==256
    while r.cursor < 256 {
        let (pc, is_piecebox) = read_hand_or_piecebox(&mut r)?;
        if is_piecebox {
            continue;
        }
        let color_idx = pc.color as usize;
        let hand_idx = piece_type_to_hand_index(pc.piece_type).map_err(|e| e.to_string())? as usize;
        pos.hands[color_idx][hand_idx] = pos.hands[color_idx][hand_idx].saturating_add(1);
    }
    if r.cursor != 256 {
        return Err("packed stream not 256 bits".into());
    }

    let header = YoV1Header { side_to_move: side };
    Ok((pos, header))
}

#[inline]
fn sample_hit(buf: &[u8], rate: f64) -> bool {
    // Deterministic FNV-like hash over first up-to-16 bytes
    let mut x: u64 = 0xcbf29ce484222325;
    let n = buf.len().min(16);
    for &b in &buf[..n] {
        x ^= b as u64;
        x = x.wrapping_mul(0x100000001b3);
    }
    ((x & 0xffff_ffff) as f64 / 4294967295.0) < rate
}

// Returns (piece or None if empty, consumed)
fn read_board_piece(r: &mut BitReader<'_>) -> Result<(Option<Piece>, usize), String> {
    // Huffman table for board (NO, P, L, N, S, B, R, G)
    // 出典: YaneuraOu packedSfenValue (packedSfen.cpp の yo_v1 テーブル)
    const CODES: [u32; 8] = [0x00, 0x01, 0x03, 0x0b, 0x07, 0x1f, 0x3f, 0x0f];
    const BITS: [usize; 8] = [1, 2, 4, 4, 4, 6, 6, 5];
    let mut code: u32 = 0;
    let mut bits: usize = 0;
    loop {
        // read one bit
        let b = r.read_one().map_err(|e| format!("board token: {e}"))?;
        code |= (b as u32) << bits;
        bits += 1;
        // Match against table
        for (i, (&c, &w)) in CODES.iter().zip(BITS.iter()).enumerate() {
            if code == c && bits == w {
                if i == 0 {
                    return Ok((None, bits));
                }
                // Decode piece type
                let pt = match i {
                    1 => PieceType::Pawn,
                    2 => PieceType::Lance,
                    3 => PieceType::Knight,
                    4 => PieceType::Silver,
                    5 => PieceType::Bishop,
                    6 => PieceType::Rook,
                    7 => PieceType::Gold,
                    _ => unreachable!(),
                };
                // Promotion bit for non-Gold
                let mut promoted = false;
                if pt != PieceType::Gold {
                    promoted = r.read_one().map_err(|e| format!("board promote: {e}"))? != 0;
                }
                // Color bit
                let color = if r.read_one().map_err(|e| format!("board color: {e}"))? == 0 {
                    Color::Black
                } else {
                    Color::White
                };
                let mut piece = Piece::new(pt, color);
                piece.promoted = promoted;
                return Ok((Some(piece), bits));
            }
        }
        if bits > 6 {
            return Err("invalid board token".into());
        }
    }
}

// Returns (piece, is_piecebox)
fn read_hand_or_piecebox(r: &mut BitReader<'_>) -> Result<(Piece, bool), String> {
    // Hand codes: board codes >>1 (except NO). Bits-1
    // 出典: YaneuraOu packedSfenValue (packedSfen.cpp の yo_v1 テーブル)
    const BOARD_CODES: [u32; 8] = [0x00, 0x01, 0x03, 0x0b, 0x07, 0x1f, 0x3f, 0x0f];
    const BOARD_BITS: [usize; 8] = [1, 2, 4, 4, 4, 6, 6, 5];
    const PIECEBOX_CODES: [u32; 8] = [0x00, 0x02, 0x09, 0x0d, 0x0b, 0x2f, 0x3f, 0x1b];
    const PIECEBOX_BITS: [usize; 8] = [1, 2, 4, 4, 4, 6, 6, 5];
    let mut code: u32 = 0;
    let mut bits: usize = 0;
    loop {
        let b = r.read_one().map_err(|e| format!("hand token: {e}"))?;
        code |= (b as u32) << bits;
        bits += 1;

        // Try piecebox first
        for pr in 1..=7 {
            if code == PIECEBOX_CODES[pr] && bits == PIECEBOX_BITS[pr] {
                // Consume optional color bit for non-Gold (written as 0)
                if pr != 7 {
                    let _ = r.read_one().map_err(|e| format!("piecebox color: {e}"))?;
                }
                let pt = match pr {
                    1 => PieceType::Pawn,
                    2 => PieceType::Lance,
                    3 => PieceType::Knight,
                    4 => PieceType::Silver,
                    5 => PieceType::Bishop,
                    6 => PieceType::Rook,
                    7 => PieceType::Gold,
                    _ => unreachable!(),
                };
                let mut pc = Piece::new(pt, Color::Black);
                pc.promoted = true; // Mark as piecebox sentinel
                return Ok((pc, true));
            }
        }

        // Try hand codes
        for pr in 1..=7 {
            if code == (BOARD_CODES[pr] >> 1) && bits == (BOARD_BITS[pr] - 1) {
                // Promotion bit always 0 for hand (except Gold which has no promotion bit)
                if pr != 7 {
                    let _p = r.read_one().map_err(|e| format!("hand promote: {e}"))?;
                }
                // Color bit
                let color = if r.read_one().map_err(|e| format!("hand color: {e}"))? == 0 {
                    Color::Black
                } else {
                    Color::White
                };
                let pt = match pr {
                    1 => PieceType::Pawn,
                    2 => PieceType::Lance,
                    3 => PieceType::Knight,
                    4 => PieceType::Silver,
                    5 => PieceType::Bishop,
                    6 => PieceType::Rook,
                    7 => PieceType::Gold,
                    _ => unreachable!(),
                };
                return Ok((Piece::new(pt, color), false));
            }
        }

        if bits > 6 {
            return Err("invalid hand/piecebox token".into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_cmd::Command;
    use engine_core::usi::parse_usi_square;
    use predicates::prelude::*;
    use std::fs::File;
    use std::io::Read;
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn fixture_path(name: &str) -> PathBuf {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let root = manifest
            .parent()
            .expect("crate dir has parent")
            .parent()
            .expect("workspace root");
        root.join("docs/tools/psv2jsonl/fixtures").join(name)
    }

    #[test]
    fn sqidx_roundtrip() -> TestResult {
        for idx in 0u8..=80 {
            let usi = sqidx_to_usi(idx);
            let square = parse_usi_square(&usi)?;
            assert_eq!(square.to_string(), usi);
        }
        Ok(())
    }

    #[test]
    fn pv_warning_emitted() -> TestResult {
        let input = fixture_path("tiny.psv");
        let mut cmd = Command::cargo_bin("psv2jsonl")?;
        let assert = cmd
            .arg("-i")
            .arg(&input)
            .arg("-o")
            .arg("-")
            .arg("--with-pv")
            .arg("--pv-max-moves")
            .arg("2")
            .assert()
            .success()
            .stderr(predicate::str::contains("Warning: --pv-max-moves > 1"));
        // Ensure stdout is not empty (processing occurred)
        assert.stdout(predicate::str::is_match(r"\S").unwrap());
        Ok(())
    }

    #[test]
    fn limit_outputs_single_record() -> TestResult {
        let input = fixture_path("tiny.psv");
        let output_file = NamedTempFile::new()?;
        Command::cargo_bin("psv2jsonl")?
            .arg("-i")
            .arg(&input)
            .arg("-o")
            .arg(output_file.path())
            .arg("--limit")
            .arg("1")
            .assert()
            .success();

        let mut buf = String::new();
        File::open(output_file.path())?.read_to_string(&mut buf)?;
        let lines: Vec<_> = buf.lines().collect();
        assert_eq!(lines.len(), 1, "expected exactly one JSONL line");
        Ok(())
    }

    #[test]
    fn truncated_input_fails_closed() -> TestResult {
        let input = fixture_path("tiny_bad.psv");
        Command::cargo_bin("psv2jsonl")?
            .arg("-i")
            .arg(&input)
            .arg("-o")
            .arg("-")
            .assert()
            .failure()
            .code(2);
        Ok(())
    }
}
