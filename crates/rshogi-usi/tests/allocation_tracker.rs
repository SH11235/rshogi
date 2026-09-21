#![cfg(feature = "allocation-stats")]

#[path = "../src/allocation_stats.rs"]
mod diagnostic;

use rshogi_core::allocation_stats::{Phase, Scope, snapshot};
use std::hint::black_box;

#[test]
fn callbacks_count_zeroed_growth_and_preserve_alignment() {
    diagnostic::init().unwrap();
    let before = snapshot();
    {
        let _scope = Scope::enter(Phase::Tree);
        let mut bytes = black_box(vec![0_u8; 128]);
        assert!(bytes.iter().all(|byte| *byte == 0));
        bytes[0] = 42;
        bytes.reserve_exact(128);
        assert_eq!(bytes[0], 42);
        assert_eq!(bytes.capacity(), 256);
        black_box(&bytes);

        #[repr(align(64))]
        struct Aligned([u8; 64]);
        let aligned = black_box(Box::new(Aligned([7; 64])));
        assert_eq!(aligned.as_ref() as *const Aligned as usize % 64, 0);
        assert_eq!(black_box(&aligned.0)[63], 7);
    }
    let after = snapshot();
    let tree = Phase::Tree as usize;
    // zeroed + growth + aligned の3確保。growth の旧領域を含めて3解放。
    assert_eq!(after[tree][0] - before[tree][0], 3);
    assert_eq!(after[tree][1] - before[tree][1], 3);
    assert_eq!(after[tree][2] - before[tree][2], 128 + 256 + 64);
    diagnostic::report(before);
}
