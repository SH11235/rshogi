//! Test to measure Position struct size

use engine_core::shogi::{CowPosition, Position};
use std::mem;

fn main() {
    println!("Struct Size Analysis");
    println!("====================");
    
    println!("Position size: {} bytes", mem::size_of::<Position>());
    println!("CowPosition size: {} bytes", mem::size_of::<CowPosition>());
    
    // Create a sample position to check actual allocation
    let sfen = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
    let pos = Position::from_sfen(sfen).unwrap();
    
    println!("\nHistory vector:");
    println!("  Length: {}", pos.history.len());
    println!("  Capacity: {}", pos.history.capacity());
    println!("  Size on heap: {} bytes", pos.history.capacity() * mem::size_of::<u64>());
    
    // Test alignment
    println!("\nAlignment:");
    println!("  Position alignment: {} bytes", mem::align_of::<Position>());
    println!("  CowPosition alignment: {} bytes", mem::align_of::<CowPosition>());
}