pub(super) struct BatchLoader {
    indices: Vec<usize>,
    batch_size: usize,
    position: usize,
    epoch: usize,
}

impl BatchLoader {
    pub(super) fn new(num_samples: usize, batch_size: usize, shuffle: bool, rng: &mut StdRng) -> Self {
        let mut indices: Vec<usize> = (0..num_samples).collect();

        if shuffle {
            indices.shuffle(rng);
        }

        BatchLoader {
            indices,
            batch_size,
            position: 0,
            epoch: 0,
        }
    }

    pub(super) fn next_batch(&mut self) -> Option<Vec<usize>> {
        if self.position >= self.indices.len() {
            return None;
        }

        let end = (self.position + self.batch_size).min(self.indices.len());
        let batch_indices: Vec<usize> = self.indices[self.position..end].to_vec();
        self.position = end;

        Some(batch_indices)
    }

    pub(super) fn reset(&mut self, shuffle: bool, rng: &mut StdRng) {
        self.position = 0;
        self.epoch += 1;

        if shuffle {
            self.indices.shuffle(rng);
        }
    }
}

// Async prefetching batch loader (indices only)
pub(super) struct AsyncBatchLoader {
    num_samples: usize,
    batch_size: usize,
    prefetch_batches: usize,
    rx: Option<Receiver<Vec<usize>>>,
    worker: Option<JoinHandle<()>>,
    epoch: usize,
}

impl AsyncBatchLoader {
    pub(super) fn new(num_samples: usize, batch_size: usize, prefetch_batches: usize) -> Self {
        Self {
            num_samples,
            batch_size,
            prefetch_batches,
            rx: None,
            worker: None,
            epoch: 0,
        }
    }

    pub(super) fn start_epoch(&mut self, shuffle: bool, seed: u64) {
        // Ensure previous worker has finished
        if let Some(h) = self.worker.take() {
            let _ = h.join();
        }
        self.epoch += 1;

        let (tx, rx) = sync_channel::<Vec<usize>>(self.prefetch_batches);
        let num_samples = self.num_samples;
        let batch_size = self.batch_size;

        let handle = std::thread::spawn(move || {
            // Prepare indices
            let mut indices: Vec<usize> = (0..num_samples).collect();
            if shuffle {
                let mut srng = StdRng::seed_from_u64(seed);
                indices.shuffle(&mut srng);
            }
            // Stream batches into the channel
            let mut pos = 0;
            while pos < indices.len() {
                let end = (pos + batch_size).min(indices.len());
                // Copy indices slice (small object)
                let batch = indices[pos..end].to_vec();
                if tx.send(batch).is_err() {
                    break; // receiver dropped
                }
                pos = end;
            }
        });

        self.rx = Some(rx);
        self.worker = Some(handle);
    }

    pub(super) fn next_batch_with_wait(&self) -> (Option<Vec<usize>>, std::time::Duration) {
        if let Some(rx) = &self.rx {
            let t0 = Instant::now();
            match rx.recv() {
                Ok(v) => (Some(v), t0.elapsed()),
                Err(_) => (None, t0.elapsed()),
            }
        } else {
            (None, std::time::Duration::ZERO)
        }
    }

    pub(super) fn finish(&mut self) {
        // Drain and join worker if any
        self.rx.take();
        if let Some(h) = self.worker.take() {
            let _ = h.join();
        }
    }
}

impl Drop for AsyncBatchLoader {
    fn drop(&mut self) {
        self.finish();
    }
}

// Streaming cache loader: reads cache file on a worker thread and sends Vec<Sample>
enum BatchMsg {
    Ok(Vec<Sample>),
    Err(String),
}

pub(super) struct StreamCacheLoader {
    path: String,
    batch_size: usize,
    prefetch_batches: usize,
    rx: Option<Receiver<BatchMsg>>,
    worker: Option<JoinHandle<()>>,
}

impl StreamCacheLoader {
    pub(super) fn new(path: String, batch_size: usize, prefetch_batches: usize) -> Self {
        Self {
            path,
            batch_size,
            prefetch_batches,
            rx: None,
            worker: None,
        }
    }

    pub(super) fn start_epoch(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        // Join any previous worker
        if let Some(h) = self.worker.take() {
            let _ = h.join();
        }
        let path = self.path.clone();
        let batch_size = self.batch_size;
        let (tx, rx) = sync_channel::<BatchMsg>(self.prefetch_batches.max(1));

        let handle = std::thread::spawn(move || {
            // Use shared nnfc_v1 reader
            let (reader, header) = match tools::nnfc_v1::open_payload_reader(&path) {
                Ok(v) => v,
                Err(e) => {
                    let _ = tx.send(BatchMsg::Err(format!("Failed to open cache {}: {}", path, e)));
                    return;
                }
            };
            if header.feature_set_id != tools::nnfc_v1::FEATURE_SET_ID_HALF {
                let _ = tx.send(BatchMsg::Err(format!(
                    "Unsupported feature_set_id: 0x{:08x} (file {})",
                    header.feature_set_id, path
                )));
                return;
            }
            let num_samples = header.num_samples;
            let flags_mask = header.flags_mask;
            let mut r = reader; // BufReader<Box<dyn Read>>

            let mut loaded: u64 = 0;
            let mut batch = Vec::with_capacity(batch_size);
            let mut unknown_flag_samples: u64 = 0;
            let mut unknown_flag_bits_accum: u32 = 0;

            while loaded < num_samples {
                // Read one sample
                // n_features
                let mut nb = [0u8; 4];
                if let Err(e) = r.read_exact(&mut nb) {
                    let _ = tx.send(BatchMsg::Err(format!(
                        "Read error at sample {} in {}: {}",
                        loaded, path, e
                    )));
                    return;
                }
                let n_features = u32::from_le_bytes(nb) as usize;
                const MAX_FEATURES_PER_SAMPLE: usize = SHOGI_BOARD_SIZE * FE_END;
                if n_features > MAX_FEATURES_PER_SAMPLE {
                    let _ = tx.send(BatchMsg::Err(format!(
                        "n_features={} exceeds sane limit {} in {}",
                        n_features, MAX_FEATURES_PER_SAMPLE, path
                    )));
                    return;
                }
                let mut features: Vec<u32> = vec![0u32; n_features];
                #[cfg(target_endian = "little")]
                {
                    use bytemuck::cast_slice_mut;
                    if let Err(e) = r.read_exact(cast_slice_mut::<u32, u8>(&mut features)) {
                        let _ = tx.send(BatchMsg::Err(format!(
                            "Read features failed at {}: {}",
                            loaded, e
                        )));
                        return;
                    }
                }
                #[cfg(target_endian = "big")]
                {
                    let mut buf = vec![0u8; n_features * 4];
                    if let Err(e) = r.read_exact(&mut buf) {
                        let _ = tx.send(BatchMsg::Err(format!(
                            "Read features failed at {}: {}",
                            loaded, e
                        )));
                        return;
                    }
                    for (dst, chunk) in features.iter_mut().zip(buf.chunks_exact(4)) {
                        *dst = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                    }
                }

                // label
                let mut lb = [0u8; 4];
                if let Err(e) = r.read_exact(&mut lb) {
                    let _ =
                        tx.send(BatchMsg::Err(format!("Read label failed at {}: {}", loaded, e)));
                    return;
                }
                let label = f32::from_le_bytes(lb);

                // meta
                let mut gapb = [0u8; 2];
                if let Err(e) = r.read_exact(&mut gapb) {
                    let _ = tx.send(BatchMsg::Err(format!("Read gap failed at {}: {}", loaded, e)));
                    return;
                }
                let gap = u16::from_le_bytes(gapb);

                let mut depth = [0u8; 1];
                if let Err(e) = r.read_exact(&mut depth) {
                    let _ =
                        tx.send(BatchMsg::Err(format!("Read depth failed at {}: {}", loaded, e)));
                    return;
                }
                let depth = depth[0];

                let mut seldepth = [0u8; 1];
                if let Err(e) = r.read_exact(&mut seldepth) {
                    let _ = tx
                        .send(BatchMsg::Err(format!("Read seldepth failed at {}: {}", loaded, e)));
                    return;
                }
                let seldepth = seldepth[0];

                let mut flags = [0u8; 1];
                if let Err(e) = r.read_exact(&mut flags) {
                    let _ =
                        tx.send(BatchMsg::Err(format!("Read flags failed at {}: {}", loaded, e)));
                    return;
                }
                let flags = flags[0];
                let unknown = (flags as u32) & !flags_mask;
                if unknown != 0 {
                    unknown_flag_samples += 1;
                    unknown_flag_bits_accum |= unknown;
                }

                // weight policy
                let mut weight = 1.0f32;
                let base_gap = (gap as f32 / GAP_WEIGHT_DIVISOR).clamp(BASELINE_MIN_EPS, 1.0);
                weight *= base_gap;
                let both_exact = (flags & fc_flags::BOTH_EXACT) != 0;
                weight *= if both_exact {
                    1.0
                } else {
                    NON_EXACT_BOUND_WEIGHT
                };
                if (flags & fc_flags::MATE_BOUNDARY) != 0 {
                    weight *= 0.5;
                }
                if seldepth < depth.saturating_add(SELECTIVE_DEPTH_MARGIN as u8) {
                    weight *= SELECTIVE_DEPTH_WEIGHT;
                }

                batch.push(Sample {
                    features,
                    label,
                    weight,
                    cp: None,
                    phase: None,
                });
                loaded += 1;

                if batch.len() >= batch_size {
                    if tx.send(BatchMsg::Ok(std::mem::take(&mut batch))).is_err() {
                        break;
                    }
                    batch = Vec::with_capacity(batch_size);
                }

                // Progress log is omitted in worker to avoid log interleaving with training side
            }

            if !batch.is_empty() {
                let _ = tx.send(BatchMsg::Ok(batch));
            }

            if unknown_flag_samples > 0 {
                eprintln!(
                    "Warning: {} samples contained unknown flag bits (mask=0x{:08x}, seen=0x{:08x})",
                    unknown_flag_samples, flags_mask, unknown_flag_bits_accum
                );
            }
        });

        self.rx = Some(rx);
        self.worker = Some(handle);
        Ok(())
    }

    pub(super) fn next_batch_with_wait(&self) -> (Option<Result<Vec<Sample>, String>>, std::time::Duration) {
        if let Some(rx) = &self.rx {
            let t0 = Instant::now();
            match rx.recv() {
                Ok(BatchMsg::Ok(v)) => (Some(Ok(v)), t0.elapsed()),
                Ok(BatchMsg::Err(msg)) => (Some(Err(msg)), t0.elapsed()),
                Err(_) => (None, t0.elapsed()),
            }
        } else {
            (None, std::time::Duration::ZERO)
        }
    }

    pub(super) fn finish(&mut self) {
        self.rx.take();
        if let Some(h) = self.worker.take() {
            let _ = h.join();
        }
    }
}

impl Drop for StreamCacheLoader {
    fn drop(&mut self) {
        self.finish();
    }
}
