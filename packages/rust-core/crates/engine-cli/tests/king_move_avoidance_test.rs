//! Test to verify the engine avoids unnecessary king moves

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

#[test]
fn test_engine_avoids_king_moves() {
    // Start the engine
    let mut child = Command::new("cargo")
        .args(&["run", "--bin", "engine-cli", "--release"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("Failed to start engine");

    let mut stdin = child.stdin.take().expect("Failed to get stdin");
    let stdout = child.stdout.take().expect("Failed to get stdout");
    let reader = BufReader::new(stdout);

    // Collect output in a separate thread
    let output_handle = thread::spawn(move || {
        let mut lines = Vec::new();
        for line in reader.lines() {
            if let Ok(line) = line {
                println!("Engine: {}", line);
                lines.push(line);
            }
        }
        lines
    });

    // Send USI initialization
    writeln!(stdin, "usi").unwrap();
    thread::sleep(Duration::from_millis(500));

    writeln!(stdin, "isready").unwrap();
    thread::sleep(Duration::from_millis(500));

    // Test positions where the engine previously chose king moves
    let test_positions = vec![
        ("startpos", "Initial position"),
        ("startpos moves 7g7f 3c3d", "After 1.P-7f 2.P-3d"),
        ("startpos moves 7g7f 8c8d", "After 1.P-7f 2.P-8d"),
    ];

    for (position, desc) in test_positions {
        println!("\nTesting position: {}", desc);

        // Set position
        writeln!(stdin, "position {}", position).unwrap();
        thread::sleep(Duration::from_millis(100));

        // Start search with enough time for deep search
        writeln!(stdin, "go movetime 2000").unwrap();
        thread::sleep(Duration::from_secs(3));
    }

    // Send quit
    writeln!(stdin, "quit").unwrap();
    stdin.flush().unwrap();
    drop(stdin);

    // Wait for child to exit
    child.wait().expect("Failed to wait for child");

    // Get collected output
    let lines = output_handle.join().expect("Failed to join output thread");

    // Analyze bestmoves
    let mut king_move_count = 0;
    let mut total_moves = 0;
    let mut last_depth = 0;

    for line in &lines {
        if line.starts_with("bestmove ") {
            total_moves += 1;
            let bestmove = line.split_whitespace().nth(1).unwrap_or("");

            // Check if it's a king move (5i or 5a in the move)
            if bestmove.contains("5i") || bestmove.contains("5a") {
                king_move_count += 1;
                println!("Found king move: {}", bestmove);
            } else {
                println!("Found non-king move: {}", bestmove);
            }
        }

        // Track search depth
        if line.contains("info depth ") {
            if let Some(depth_str) = line.split("depth ").nth(1) {
                if let Some(depth) = depth_str.split_whitespace().next() {
                    if let Ok(d) = depth.parse::<u32>() {
                        last_depth = last_depth.max(d);
                    }
                }
            }
        }
    }

    println!("\nResults:");
    println!("Total moves: {}", total_moves);
    println!("King moves: {}", king_move_count);
    println!("Max search depth: {}", last_depth);

    // Assert that king moves are rare (allow at most 1 out of 3)
    assert!(
        king_move_count <= 1,
        "Too many king moves: {} out of {}",
        king_move_count,
        total_moves
    );

    // Assert that we achieved reasonable search depth
    assert!(last_depth >= 3, "Search depth too shallow: {}", last_depth);
}
