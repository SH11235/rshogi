use std::sync::atomic::{AtomicU64, Ordering};

pub(crate) struct BestMoveChanges {
    published_bits: AtomicU64,
    sampled_bits: AtomicU64,
}

impl BestMoveChanges {
    pub(crate) fn new() -> Self {
        Self {
            published_bits: AtomicU64::new(0),
            sampled_bits: AtomicU64::new(0),
        }
    }

    pub(crate) fn reset(&self) {
        self.published_bits.store(0, Ordering::Relaxed);
        self.sampled_bits.store(0, Ordering::Relaxed);
    }

    pub(crate) fn publish(&self, cumulative: f64) {
        self.published_bits.store(cumulative.to_bits(), Ordering::Relaxed);
    }

    pub(crate) fn cumulative(&self) -> f64 {
        f64::from_bits(self.published_bits.load(Ordering::Relaxed))
    }

    // main だけが回収する。累積値を残すため、公開の間隔によらず増分を一度だけ数えられる。
    pub(crate) fn take_delta(&self) -> f64 {
        let current = self.published_bits.load(Ordering::Relaxed);
        let previous = self.sampled_bits.swap(current, Ordering::Relaxed);
        f64::from_bits(current) - f64::from_bits(previous)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn best_move_changes_are_counted_once_between_samples() {
        let changes = BestMoveChanges::new();
        changes.publish(2.0);
        assert_eq!(changes.cumulative(), 2.0);
        assert_eq!(changes.cumulative(), 2.0);
        assert_eq!(changes.take_delta(), 2.0);
        assert_eq!(changes.take_delta(), 0.0);
        changes.publish(5.0);
        changes.publish(6.5);
        assert_eq!(changes.take_delta(), 4.5);
        assert_eq!(changes.take_delta(), 0.0);
        changes.publish(6.5);
        assert_eq!(changes.take_delta(), 0.0);
    }

    #[test]
    fn best_move_changes_reset_starts_a_new_search() {
        let changes = BestMoveChanges::new();
        changes.publish(8.0);
        assert_eq!(changes.take_delta(), 8.0);
        changes.publish(12.0);
        changes.reset();
        assert_eq!(changes.take_delta(), 0.0);
        changes.publish(1.0);
        assert_eq!(changes.take_delta(), 1.0);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn best_move_changes_concurrent_publication_preserves_total() {
        use std::sync::{Arc, Barrier};
        let changes = Arc::new(BestMoveChanges::new());
        let barrier = Arc::new(Barrier::new(2));
        let producer_changes = Arc::clone(&changes);
        let producer_barrier = Arc::clone(&barrier);
        let producer = std::thread::spawn(move || {
            producer_barrier.wait();
            for cumulative in 1..=10_000 {
                producer_changes.publish(f64::from(cumulative));
            }
        });
        barrier.wait();
        let mut total = 0.0;
        while !producer.is_finished() {
            let delta = changes.take_delta();
            assert!(delta >= 0.0);
            total += delta;
            std::thread::yield_now();
        }
        producer.join().unwrap();
        total += changes.take_delta();
        assert_eq!(total, 10_000.0);
        assert_eq!(changes.take_delta(), 0.0);
    }
}
