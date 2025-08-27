use crate::movegen::compat::MoveGen;
use crate::movegen::v2::MoveGenerator;
use crate::shogi::{Position, moves::Move};
use std::collections::HashSet;

/// Test helper to compare move lists from old and new implementations
fn compare_move_lists(pos: &Position) {
    // Generate moves with old implementation
    let mut old_gen = MoveGen::new(pos);
    let old_moves = old_gen.generate_moves();
    
    // Generate moves with new implementation
    let new_gen = MoveGenerator::new();
    let new_moves_result = new_gen.generate_all(pos);
    
    // New implementation should succeed
    assert!(new_moves_result.is_ok(), "New MoveGen failed: {:?}", new_moves_result);
    let new_moves = new_moves_result.unwrap();
    
    // Convert to sets for comparison (order may differ)
    let old_set: HashSet<Move> = old_moves.into_iter().collect();
    let new_set: HashSet<Move> = new_moves.into_iter().collect();
    
    // Compare counts
    assert_eq!(
        old_set.len(),
        new_set.len(),
        "Move count mismatch: old={}, new={}",
        old_set.len(),
        new_set.len()
    );
    
    // Find differences
    let only_in_old: Vec<&Move> = old_set.difference(&new_set).collect();
    let only_in_new: Vec<&Move> = new_set.difference(&old_set).collect();
    
    if !only_in_old.is_empty() || !only_in_new.is_empty() {
        panic!(
            "Move lists differ!\nOnly in old ({}):\n{:?}\nOnly in new ({}):\n{:?}",
            only_in_old.len(),
            only_in_old,
            only_in_new.len(),
            only_in_new
        );
    }
}

#[test]
fn test_starting_position_compatibility() {
    let pos = Position::startpos();
    compare_move_lists(&pos);
}

#[test]
#[ignore] // Remove when implementation is more complete
fn test_various_positions_compatibility() {
    // Test various positions
    let test_sfens = [
        "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",  // Starting position
        "8l/1l+R2P3/p2pBG1pp/kps1p4/Nn1P2G2/P1P1P2PP/1PS6/1KSG3+r1/LN2+p3L w Sbgn3p 1",  // Middle game
        "4k4/9/4P4/9/9/9/9/9/4K4 b - 1",  // Endgame
    ];
    
    for sfen in &test_sfens {
        let pos = Position::from_sfen(sfen).expect("Invalid SFEN");
        compare_move_lists(&pos);
    }
}

#[test]
fn test_has_legal_moves_compatibility() {
    let pos = Position::startpos();
    
    // Check with old implementation
    let mut old_gen = MoveGen::new(&pos);
    let old_moves = old_gen.generate_moves();
    let old_has_moves = !old_moves.is_empty();
    
    // Check with new implementation
    let new_gen = MoveGenerator::new();
    let new_has_moves_result = new_gen.has_legal_moves(&pos);
    assert!(new_has_moves_result.is_ok());
    let new_has_moves = new_has_moves_result.unwrap();
    
    assert_eq!(old_has_moves, new_has_moves, "has_legal_moves mismatch");
}

/// Test helper for parallel execution
pub fn run_parallel_test<F>(test_fn: F, iterations: usize)
where
    F: Fn() + Send + Sync,
{
    use std::sync::Arc;
    use std::thread;
    
    let test_fn = Arc::new(test_fn);
    let mut handles = vec![];
    
    for _ in 0..4 {  // 4 threads
        let test_fn = Arc::clone(&test_fn);
        let handle = thread::spawn(move || {
            for _ in 0..iterations {
                test_fn();
            }
        });
        handles.push(handle);
    }
    
    for handle in handles {
        handle.join().expect("Thread panicked");
    }
}

#[test]
fn test_parallel_move_generation() {
    run_parallel_test(|| {
        let pos = Position::startpos();
        let gen = MoveGenerator::new();
        let result = gen.generate_all(&pos);
        assert!(result.is_ok());
        assert!(!result.unwrap().is_empty());
    }, 100);
}