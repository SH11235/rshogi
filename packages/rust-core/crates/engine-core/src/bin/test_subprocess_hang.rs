use std::io::Write;
use std::process::{Command, Stdio};

fn main() {
    println!("=== Testing Subprocess Hang ===");

    // Test 1: Direct execution (should work)
    println!("\nTest 1: Direct execution of has_legal_moves");
    let start = std::time::Instant::now();

    // Try warmup first
    println!("  Warming up static tables...");
    engine_core::warm_up_static_tables();
    println!("  Warmup completed in {:?}", start.elapsed());

    // Then init all tables
    println!("  Calling init_all_tables_once...");
    engine_core::init::init_all_tables_once();
    println!("  init_all_tables_once completed in {:?}", start.elapsed());

    let pos = engine_core::Position::startpos();
    println!("  Position created in {:?}", start.elapsed());

    let mut movegen = engine_core::movegen::MoveGen::new();
    println!("  MoveGen created in {:?}", start.elapsed());

    let mut moves = engine_core::shogi::MoveList::new();
    println!("  Calling generate_all...");
    movegen.generate_all(&pos, &mut moves);

    println!("  Result: {} moves generated in {:?}", moves.len(), start.elapsed());

    // Test 2: Subprocess execution
    println!("\nTest 2: Subprocess execution");
    let engine_path = "./target/release/engine-cli";
    println!("  Setting environment: SKIP_LEGAL_MOVES=0, RUST_LOG=debug");
    let mut child = Command::new(engine_path)
        .env("SKIP_LEGAL_MOVES", "0")
        .env("RUST_LOG", "debug")
        .env("SUBPROCESS_MODE", "1") // Mark as subprocess
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn engine");

    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(b"position startpos\n").unwrap();
    stdin.write_all(b"go depth 1\n").unwrap();
    stdin.write_all(b"quit\n").unwrap();
    drop(stdin);

    // Wait for completion with timeout
    println!("  Waiting for subprocess (5s timeout)...");
    let start = std::time::Instant::now();

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                println!("  Result: Process exited with {:?} in {:?}", status, start.elapsed());
                break;
            }
            Ok(None) => {
                if start.elapsed().as_secs() >= 5 {
                    println!("  Result: HANG DETECTED - killing process");
                    child.kill().ok();
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(e) => {
                println!("  Error: {}", e);
                break;
            }
        }
    }
}
