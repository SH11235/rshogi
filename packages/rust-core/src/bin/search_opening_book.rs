//! Search for positions in opening book binary files
//!
//! Usage:
//!   cargo run --bin search_opening_book -- <BINARY_FILE> <SFEN_STRING>
//!   cargo run --bin search_opening_book -- <BINARY_FILE> --hash <HASH_VALUE>
//!
//! Examples:
//!   cargo run --bin search_opening_book -- converted_openings/opening_book_web.binz "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1"
//!   cargo run --bin search_opening_book -- converted_openings/opening_book_web.binz --hash 0x6327d878264056a0

use anyhow::{anyhow, Result};
use shogi_core::opening_book::position_hasher::PositionHasher;
use shogi_core::opening_book_reader::OpeningBookReader;
use std::env;
use std::fs;
use std::path::Path;

fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();

    if args.len() < 3 {
        eprintln!("Usage: {} <BINARY_FILE> <SFEN_STRING>", args[0]);
        eprintln!("       {} <BINARY_FILE> --hash <HASH_VALUE>", args[0]);
        eprintln!();
        eprintln!("Examples:");
        eprintln!("  {} converted_openings/opening_book_web.binz \"lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1\"", args[0]);
        eprintln!(
            "  {} converted_openings/opening_book_web.binz --hash 0x6327d878264056a0",
            args[0]
        );
        std::process::exit(1);
    }

    let binary_file = &args[1];

    // Check if file exists
    if !Path::new(binary_file).exists() {
        eprintln!("Error: File not found: {binary_file}");
        std::process::exit(1);
    }

    // Determine search mode
    let hash = if args.len() >= 4 && args[2] == "--hash" {
        // Hash mode
        let hash_str = &args[3];
        parse_hash(hash_str)?
    } else {
        // SFEN mode
        let sfen = &args[2];
        PositionHasher::hash_position(sfen)?
    };

    // Load binary file
    println!("Loading binary file: {binary_file}");
    let compressed_data = fs::read(binary_file)?;

    // Create reader and load data
    let mut reader = OpeningBookReader::new();
    match reader.load_data(&compressed_data) {
        Ok(msg) => println!("{msg}"),
        Err(e) => {
            eprintln!("Error loading data: {e}");
            std::process::exit(1);
        }
    }

    // Search for moves
    println!("\nSearching for hash: {hash:#016x}");
    let moves = reader.find_moves_by_hash(hash);

    if moves.is_empty() {
        println!("No moves found for this position.");
    } else {
        println!("Found {} moves:", moves.len());
        println!("\n{:<10} {:>8} {:>8}", "Move", "Eval", "Depth");
        println!("{:-<28}", "");

        for (i, book_move) in moves.iter().enumerate() {
            println!(
                "{:2}. {:<6} {:>8} {:>8}",
                i + 1,
                book_move.notation,
                book_move.evaluation,
                book_move.depth
            );
        }
    }

    Ok(())
}

fn parse_hash(hash_str: &str) -> Result<u64> {
    // Remove 0x prefix if present
    let hash_str = if hash_str.starts_with("0x") || hash_str.starts_with("0X") {
        &hash_str[2..]
    } else {
        hash_str
    };

    // Parse as hexadecimal
    u64::from_str_radix(hash_str, 16).map_err(|_| anyhow!("Invalid hash value: {}", hash_str))
}
