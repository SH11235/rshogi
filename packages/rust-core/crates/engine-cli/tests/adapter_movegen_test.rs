//! Test MoveGen through EngineAdapter

use engine_cli::engine_adapter::EngineAdapter;

#[test]
fn test_has_legal_moves_through_adapter() {
    println!("Creating EngineAdapter...");
    let mut adapter = EngineAdapter::new();

    println!("Setting initial position...");
    adapter.set_position(true, None, &[]).expect("Should set position");

    println!("Calling has_legal_moves...");
    let result = adapter.has_legal_moves();

    println!("Result: {:?}", result);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), true);
}

#[test]
fn test_generate_emergency_move() {
    println!("Creating EngineAdapter...");
    let mut adapter = EngineAdapter::new();

    println!("Setting initial position...");
    adapter.set_position(true, None, &[]).expect("Should set position");

    println!("Calling generate_emergency_move...");
    let result = adapter.generate_emergency_move();

    println!("Result: {:?}", result);
    assert!(result.is_ok());
    let move_str = result.unwrap();
    println!("Emergency move: {}", move_str);
}
