//! Test that init_all_tables_once prevents hangs

use std::time::{Duration, Instant};

#[test]
fn test_init_prevents_hang() {
    println!("Testing initialization...");

    // Call init multiple times - should be safe
    let start = Instant::now();
    engine_core::init::init_all_tables_once();
    let init_time = start.elapsed();
    println!("First init took: {:?}", init_time);

    // Second call should be instant
    let start = Instant::now();
    engine_core::init::init_all_tables_once();
    let second_time = start.elapsed();
    println!("Second init took: {:?}", second_time);
    assert!(second_time < Duration::from_millis(1), "Second init should be instant");

    // Now test MoveGen directly
    println!("Testing MoveGen after init...");
    let start = Instant::now();

    use engine_core::{shogi::MoveList, MoveGen, Position};
    let position = Position::startpos();
    let mut gen = MoveGen::new();
    let mut moves = MoveList::new();

    println!("Calling generate_all...");
    gen.generate_all(&position, &mut moves);

    let movegen_time = start.elapsed();
    println!("MoveGen took: {:?}", movegen_time);
    println!("Generated {} moves", moves.len());

    assert_eq!(moves.len(), 30, "Startpos should have 30 legal moves");
    assert!(movegen_time < Duration::from_secs(1), "MoveGen should complete quickly");
}

#[test]
fn test_has_legal_moves_after_init() {
    use engine_cli::engine_adapter::EngineAdapter;

    println!("Creating EngineAdapter (should call init)...");
    let start = Instant::now();
    let mut adapter = EngineAdapter::new();
    let new_time = start.elapsed();
    println!("EngineAdapter::new took: {:?}", new_time);

    println!("Setting position...");
    adapter.set_position(true, None, &[]).expect("Should set position");

    println!("Testing has_legal_moves...");
    let start = Instant::now();
    let result = adapter.has_legal_moves();
    let check_time = start.elapsed();

    println!("has_legal_moves took: {:?}", check_time);
    println!("Result: {:?}", result);

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), true);
    assert!(check_time < Duration::from_secs(1), "has_legal_moves should complete quickly");
}
