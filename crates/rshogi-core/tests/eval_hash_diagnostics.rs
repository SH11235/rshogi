#![cfg(feature = "diagnostics")]

use rshogi_core::eval::{EvalHash, eval_hash_stats, reset_eval_hash_stats};

// 統計はプロセス全体で共有されるため、他の probe を行うテストとは別バイナリで検証する。
#[test]
fn uncacheable_key_counts_as_a_miss() {
    let hash = EvalHash::new(1);
    hash.store(1, 42);
    reset_eval_hash_stats();

    assert_eq!(hash.probe(1), Some(42));
    assert_eq!(hash.probe(1 | (1 << 16)), None);
    let stats = eval_hash_stats();
    assert_eq!(stats.probes, 2);
    assert_eq!(stats.hits, 1);
    assert_eq!(stats.hit_rate(), 0.5);
}
