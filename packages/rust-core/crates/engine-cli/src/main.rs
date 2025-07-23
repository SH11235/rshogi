// USI (Universal Shogi Interface) adapter

mod usi;

use anyhow::Result;
use clap::Parser;
use std::io::{self, BufRead};
use usi::{parse_usi_command, send_response, UsiCommand, UsiResponse};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Enable debug logging
    #[arg(short, long)]
    debug: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();

    // Initialize logging
    if args.debug {
        env_logger::init_from_env(
            env_logger::Env::default().filter_or(env_logger::DEFAULT_FILTER_ENV, "debug"),
        );
    } else {
        env_logger::init_from_env(
            env_logger::Env::default().filter_or(env_logger::DEFAULT_FILTER_ENV, "info"),
        );
    }

    log::info!("USI Engine starting...");

    // TODO: Implement USI protocol
    println!("id name ShogiEngine");
    println!("id author ShogiEngine Team");
    println!("usiok");

    Ok(())
}
