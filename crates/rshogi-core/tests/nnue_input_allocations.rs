use std::alloc::System;
use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};

use rshogi_core::nnue::{FeatureTransformerLayerStacks, HalfKaHmMergedSpec};
use tracking_allocator::{AllocationGroupId, AllocationRegistry, AllocationTracker, Allocator};

#[global_allocator]
static ALLOCATOR: Allocator<System> = Allocator::system();
static MAX_REQUEST: AtomicUsize = AtomicUsize::new(0);

struct Tracker;

impl AllocationTracker for Tracker {
    fn allocated(&self, _: usize, object_size: usize, _: usize, _: AllocationGroupId) {
        MAX_REQUEST.fetch_max(object_size, Ordering::Relaxed);
    }

    fn deallocated(
        &self,
        _: usize,
        _: usize,
        _: usize,
        _: AllocationGroupId,
        _: AllocationGroupId,
    ) {
    }
}

fn bounded_error<T>(name: &str, read: impl FnOnce() -> io::Result<T>) {
    MAX_REQUEST.store(0, Ordering::Relaxed);
    AllocationRegistry::enable_tracking();
    let result = read();
    AllocationRegistry::disable_tracking();
    let maximum = MAX_REQUEST.load(Ordering::Relaxed);
    assert!(result.is_err(), "{name}");
    assert!(maximum <= 64 * 1024, "{name}: maximum allocation request = {maximum}");
    println!("{name}: maximum allocation request = {maximum} bytes");
}

fn compressed(declared: u32, payload: &[u8]) -> Vec<u8> {
    let mut bytes = b"COMPRESSED_LEB128".to_vec();
    bytes.extend_from_slice(&declared.to_le_bytes());
    bytes.extend_from_slice(payload);
    bytes
}

#[test]
fn short_nnue_inputs_do_not_allocate_declared_tensors() {
    AllocationRegistry::set_global_tracker(Tracker).unwrap();

    let bytes = compressed(16 * 1024 * 1024, &[1, 2, 3]);
    bounded_error("Read-only LEB128 / 16 MiB declaration", || {
        FeatureTransformerLayerStacks::<32, HalfKaHmMergedSpec>::read_leb128(&mut &bytes[..])
    });

    let bytes = compressed(33, &[0; 33]);
    bounded_error("Read-only LEB128 / incomplete 32-wide FT", || {
        FeatureTransformerLayerStacks::<32, HalfKaHmMergedSpec>::read_leb128(&mut &bytes[..])
    });

    #[cfg(feature = "nnue-runtime-dimensions")]
    {
        use rshogi_core::nnue::{NNUE_VERSION_HALFKA, NNUENetwork};
        let arch = b"Features=HalfKaHmMerged[73305->64x2],l2=8,l3=32";
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&NNUE_VERSION_HALFKA.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&(arch.len() as u32).to_le_bytes());
        bytes.extend_from_slice(arch);
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.resize(bytes.len() + 64 * 2, 0);
        bounded_error("HalfKX / incomplete 64-wide FT", || NNUENetwork::from_bytes(&bytes));
    }
}
