use clap::{arg, Command};
use rand::prelude::*;
use rand_xoshiro::Xoshiro256PlusPlus;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};

use engine_core::movegen::MoveGenerator;
use engine_core::shogi::Position;
use engine_core::usi::{parse_sfen, position_to_sfen};
use tools::common::sfen::normalize_4t;

fn extract_sfen_line(line: &str) -> Option<String> {
    let s = line.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(idx) = s.find("sfen ") {
        let raw = &s[idx + 5..];
        // cut at first comment or trailing tokens beyond 4 fields if present
        // normalize_4t will truncate after 4 tokens
        normalize_4t(raw)
    } else {
        // maybe already a 4-token SFEN
        normalize_4t(s)
    }
}

fn parse_book_moves(
    book_path: &str,
) -> std::io::Result<std::collections::HashMap<String, Vec<String>>> {
    use std::collections::HashMap;
    let f = File::open(book_path)?;
    let rd = BufReader::new(f);
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    let mut current_sfen: Option<String> = None;
    for line in rd.lines() {
        let line = line?;
        let s = line.trim();
        if s.is_empty() || s.starts_with('#') {
            continue;
        }
        if s.starts_with("sfen ") {
            if let Some(sf) = extract_sfen_line(s) {
                current_sfen = Some(sf);
            }
            continue;
        }
        if let Some(sf) = current_sfen.as_ref() {
            // treat first token as USI move; skip if 'none'
            if let Some(tok) = s.split_whitespace().next() {
                if tok != "none" {
                    map.entry(sf.clone()).or_default().push(tok.to_string());
                }
            }
        }
    }
    Ok(map)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let app = Command::new("generate_random_sfens")
        .about("Generate random SFEN positions by playout from seed SFENs")
        .arg(
            arg!(--seeds <FILE> "Seed SFEN file (repeatable)")
                .required(true)
                .num_args(1..)
                .action(clap::ArgAction::Append),
        )
        .arg(arg!(--out <FILE> "Output file").required(true))
        .arg(arg!(--count <N> "Number of positions to generate").required(true))
        .arg(arg!(--"min-plies" <N> "Minimum random plies from seed").default_value("1"))
        .arg(arg!(--"max-plies" <N> "Maximum random plies from seed").default_value("80"))
        .arg(arg!(--seed <N> "RNG seed (u64)").required(false))
        .arg(arg!(--book <FILE> "Opening book (.db) to follow for initial plies").required(false))
        .arg(
            arg!(--"book-plies" <N> "Number of initial plies to follow book if available")
                .default_value("4"),
        )
        .arg(arg!(--jsonl "Output JSONL {\"sfen\": \"...\"} (default: SFEN lines)").required(false))
        .arg(arg!(--append "Append to output instead of overwrite").required(false))
        .get_matches();

    let seed_files: Vec<String> = app.get_many::<String>("seeds").unwrap().cloned().collect();
    let out: String = app.get_one::<String>("out").unwrap().clone();
    let total: usize = app.get_one::<String>("count").unwrap().parse()?;
    let min_plies: u32 = app.get_one::<String>("min-plies").unwrap().parse()?;
    let max_plies: u32 = app.get_one::<String>("max-plies").unwrap().parse()?;
    if min_plies == 0 || max_plies < min_plies {
        return Err("invalid plies range".into());
    }
    let jsonl = app.get_flag("jsonl");
    let append = app.get_flag("append");
    let seed: u64 = if let Some(s) = app.get_one::<String>("seed") {
        s.parse()?
    } else {
        0x00C0_FFEE_5EED_1234_u64
    };
    let book_path = app.get_one::<String>("book").cloned();
    let book_plies: u32 = app.get_one::<String>("book-plies").unwrap().parse()?;
    let book_moves = if let Some(bp) = book_path.as_ref() {
        Some(parse_book_moves(bp).map_err(|e| format!("failed to read book '{}': {}", bp, e))?)
    } else {
        None
    };

    // Load seeds
    let mut seed_positions: Vec<Position> = Vec::new();
    for f in &seed_files {
        let rf = File::open(f)?;
        let mut rd = BufReader::new(rf);
        let mut line = String::new();
        loop {
            line.clear();
            let n = rd.read_line(&mut line)?;
            if n == 0 {
                break;
            }
            if let Some(sf) = extract_sfen_line(line.trim()) {
                if let Ok(pos) = parse_sfen(&sf) {
                    seed_positions.push(pos);
                }
            }
        }
    }
    if seed_positions.is_empty() {
        return Err("no valid seeds loaded".into());
    }

    let of = if append {
        File::options().create(true).append(true).open(&out)?
    } else {
        File::create(&out)?
    };
    let mut w = BufWriter::with_capacity(1 << 20, of);

    let mut rng = Xoshiro256PlusPlus::seed_from_u64(seed);
    let movegen = MoveGenerator::new();

    for _ in 0..total {
        // Pick seed
        let idx = rng.random_range(0..seed_positions.len());
        let mut pos = seed_positions[idx].clone();
        // Random plies
        let plies = rng.random_range(min_plies..=max_plies);
        let mut played: u32 = 0;
        // Follow book if provided
        if let Some(map) = book_moves.as_ref() {
            while played < plies && played < book_plies {
                let key = position_to_sfen(&pos);
                if let Some(list) = map.get(&key) {
                    if list.is_empty() {
                        break;
                    }
                    let mv_str = &list[rng.random_range(0..list.len())];
                    if let Some(mv) = engine_core::util::usi_helpers::resolve_usi_move(&pos, mv_str)
                    {
                        let _u = pos.do_move(mv);
                        played += 1;
                        continue;
                    }
                }
                break;
            }
        }
        // Random legal moves for the rest
        while played < plies {
            match movegen.generate_all(&pos) {
                Ok(mvlist) => {
                    let mv_slice = mvlist.as_slice();
                    if mv_slice.is_empty() {
                        break;
                    }
                    let mv = mv_slice[rng.random_range(0..mv_slice.len())];
                    let _u = pos.do_move(mv);
                    played += 1;
                }
                Err(_) => break,
            }
        }
        let sfen = position_to_sfen(&pos);
        if jsonl {
            writeln!(w, "{{\"sfen\":\"{}\"}}", sfen.replace('"', "\\\""))?;
        } else {
            // follow seed file format: prefix with 'sfen '
            writeln!(w, "sfen {}", sfen)?;
        }
    }
    w.flush()?;

    Ok(())
}
