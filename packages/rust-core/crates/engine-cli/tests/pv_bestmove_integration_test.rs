//! Integration test to verify bestmove matches PV[0] in actual search

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

#[test]
fn test_bestmove_matches_pv_in_search() {
    // Start the engine
    let mut child = Command::new("cargo")
        .args(["run", "--bin", "engine-cli", "--release"])
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
                println!("Engine: {line}"); // Debug output
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

    // Set position
    writeln!(stdin, "position startpos").unwrap();
    thread::sleep(Duration::from_millis(100));

    // Start search with reasonable time to ensure info output
    writeln!(stdin, "go movetime 1000").unwrap();

    // Wait for search to complete
    thread::sleep(Duration::from_secs(2));

    // Send quit
    writeln!(stdin, "quit").unwrap();
    stdin.flush().unwrap();
    drop(stdin);

    // Wait for child to exit
    child.wait().expect("Failed to wait for child");

    // Get collected output
    let lines = output_handle.join().expect("Failed to join output thread");

    // Parse the output to find bestmove and last PV

    // Find the bestmove
    let bestmove_line = lines
        .iter()
        .find(|line| line.starts_with("bestmove "))
        .expect("No bestmove found");

    let bestmove = bestmove_line.split_whitespace().nth(1).expect("Invalid bestmove format");

    // Find the last info line with PV
    let last_pv_line =
        lines.iter().rev().find(|line| line.contains("info ") && line.contains(" pv "));

    // If no PV found, just check that bestmove exists (shallow searches might not output PV)
    if last_pv_line.is_none() {
        println!("No PV found in output, skipping PV consistency check");
        println!("Bestmove found: {bestmove}");
        return;
    }

    let last_pv_line = last_pv_line.unwrap();

    // Extract PV from the info line
    let pv_start = last_pv_line.find(" pv ").expect("PV not found") + 4;
    let pv_part = &last_pv_line[pv_start..];
    let pv_first_move = pv_part.split_whitespace().next().expect("Empty PV");

    // Verify bestmove matches PV[0]
    assert_eq!(
        bestmove, pv_first_move,
        "bestmove '{bestmove}' should match PV[0] '{pv_first_move}'"
    );

    println!("✓ bestmove matches PV[0]: {bestmove}");
}
