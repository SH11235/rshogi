//! SFEN string to hash value converter
//!
//! Usage: cargo run --bin sfen_hasher -- "<SFEN_STRING>"
//! Example: cargo run --bin sfen_hasher -- "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1"

use anyhow::Result;
use engine_core::opening_book::position_hasher::PositionHasher;
use std::env;

fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: {} <SFEN_STRING>", args[0]);
        eprintln!(
            "Example: {} \"lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1\"",
            args[0]
        );
        std::process::exit(1);
    }

    let sfen = &args[1];

    match PositionHasher::hash_position(sfen) {
        Ok(hash) => {
            println!("SFEN: {sfen}");
            println!("Hash: {hash:#016x}");
            println!("Hash (decimal): {hash}");
        }
        Err(e) => {
            eprintln!("Error: Failed to hash SFEN position: {e}");
            std::process::exit(1);
        }
    }

    Ok(())
}
