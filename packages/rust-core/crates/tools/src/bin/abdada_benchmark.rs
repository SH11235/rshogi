//! Simple test to verify ABDADA flag functionality

use engine_core::search::{tt::NodeType, TranspositionTable};

fn main() {
    println!("ABDADA Flag Test");
    println!("================\n");

    // Create a transposition table
    let tt = TranspositionTable::new(1); // 1MB table
    let test_hash = 0x1234567890ABCDEF;

    // Store an entry
    println!("1. Storing entry in TT...");
    tt.store(test_hash, None, 100, 50, 5, NodeType::Exact);
    println!("   Entry stored successfully");

    // Check initial state
    println!("\n2. Checking initial ABDADA flag state...");
    let has_flag = tt.has_exact_cut(test_hash);
    println!("   Flag is set: {has_flag}");
    assert!(!has_flag, "Flag should not be set initially");

    // Set the flag
    println!("\n3. Setting ABDADA flag...");
    let set_result = tt.set_exact_cut(test_hash);
    println!("   Set operation result: {set_result}");
    assert!(set_result, "Should be able to set flag");

    // Verify flag is set
    println!("\n4. Verifying flag is set...");
    let has_flag = tt.has_exact_cut(test_hash);
    println!("   Flag is set: {has_flag}");
    assert!(has_flag, "Flag should be set after setting");

    // Clear the flag
    println!("\n5. Clearing ABDADA flag...");
    let clear_result = tt.clear_exact_cut(test_hash);
    println!("   Clear operation result: {clear_result}");
    assert!(clear_result, "Should be able to clear flag");

    // Verify flag is cleared
    println!("\n6. Verifying flag is cleared...");
    let has_flag = tt.has_exact_cut(test_hash);
    println!("   Flag is set: {has_flag}");
    assert!(!has_flag, "Flag should be cleared after clearing");

    // Test non-existent entry
    println!("\n7. Testing non-existent entry...");
    let non_existent_hash = 0xFEDCBA0987654321;
    let has_flag = tt.has_exact_cut(non_existent_hash);
    let set_result = tt.set_exact_cut(non_existent_hash);
    println!("   Non-existent entry has flag: {has_flag}");
    println!("   Attempt to set flag on non-existent: {set_result}");
    assert!(!has_flag, "Non-existent entry should not have flag");
    assert!(!set_result, "Should not be able to set flag on non-existent entry");

    println!("\n✅ All tests passed! ABDADA flag operations work correctly.");
    println!("\nNote: In parallel search, this flag helps threads avoid duplicating work");
    println!("by marking positions where beta cutoffs have been found.");
}
