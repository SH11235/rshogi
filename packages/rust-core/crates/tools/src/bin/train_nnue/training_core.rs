pub fn train_model(
    network: &mut Network,
    train_samples: &mut [Sample],
    validation_samples: &Option<Vec<Sample>>,
    config: &Config,
    rng: &mut StdRng,
    ctx: &mut TrainContext,
) -> Result<(), Box<dyn std::error::Error>> {
    let network = match network {
        Network::Single(inner) => inner,
        Network::Classic(_) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "Classic アーキの学習ループは実装中です",
            )
            .into());
        }
    };
    let n_samples = train_samples.len();
    let n_batches = n_samples.div_ceil(config.batch_size);
    let mut adam_state = if config.optimizer == "adam" {
        Some(SingleAdamState::new(network))
    } else {
        None
    };

    let mut total_batches = 0;

    for epoch in 0..config.epochs {
        let epoch_start = Instant::now();
        let mut last_lr_base = config.learning_rate;

        // Shuffle training data
        if config.shuffle {
            train_samples.shuffle(rng);
        }

        let mut total_loss = 0.0;
        let mut total_weight = 0.0;

        // Training
        let mut last_report = Instant::now();
        let mut samples_since = 0usize;
        let mut batches_since = 0usize;
        let mut zero_weight_batches = 0usize;
        // 直近のバッチloss（throughput構造化ログ用）。最初は未定義。
        let mut last_loss_for_log: Option<f32> = None;
        for batch_idx in 0..n_batches {
            // consume last_loss_for_log to avoid unused-assignment lint across iterations
            let _ = last_loss_for_log;
            let start = batch_idx * config.batch_size;
            let end = (start + config.batch_size).min(n_samples);
            let batch = &train_samples[start..end];

            let batch_indices: Vec<usize> = (0..batch.len()).collect();
            // LR scheduling per spec #11
            let lr_base = lr_base_for(epoch, ctx.global_step, config, ctx.plateau.as_ref());
            // 先にバッチ重みを集計し、ゼロなら計算自体をスキップ
            let batch_weight: f32 = batch.iter().map(|s| s.weight).sum();
            if batch_weight > 0.0 {
                let loss = train_batch_by_indices(
                    network,
                    batch,
                    &batch_indices,
                    config,
                    &mut adam_state,
                    lr_base,
                );
                total_loss += loss * batch_weight;
                total_weight += batch_weight;
                last_loss_for_log = Some(loss);
            } else {
                last_loss_for_log = None;
            }
            last_lr_base = lr_base;

            total_batches += 1;
            ctx.global_step += 1;
            samples_since += batch.len();
            batches_since += 1;
            if batch_weight == 0.0 {
                zero_weight_batches += 1;
            }

            // Periodic throughput report
            if last_report.elapsed().as_secs_f32() >= config.throughput_interval_sec
                && batches_since > 0
            {
                let secs = last_report.elapsed().as_secs_f32().max(MIN_ELAPSED_TIME as f32);
                let sps = samples_since as f32 / secs;
                let bps = batches_since as f32 / secs;
                let avg_bs = samples_since as f32 / batches_since as f32;
                if ctx.structured.as_ref().map(|lg| lg.to_stdout).unwrap_or(false) {
                    eprintln!(
                        "[throughput] mode=inmem epoch={} batches={} sps={:.0} bps={:.2} avg_batch={:.1}",
                        epoch + 1,
                        batch_idx + 1,
                        sps,
                        bps,
                        avg_bs
                    );
                } else {
                    println!(
                        "[throughput] mode=inmem epoch={} batches={} sps={:.0} bps={:.2} avg_batch={:.1}",
                        epoch + 1,
                        batch_idx + 1,
                        sps,
                        bps,
                        avg_bs
                    );
                }
                if let Some(ref lg) = ctx.structured {
                    let mut rec = serde_json::json!({
                        "ts": chrono::Utc::now().to_rfc3339(),
                        "phase": "train",
                        "global_step": ctx.global_step as i64,
                        "epoch": (epoch + 1) as i64,
                        "lr": lr_base as f64,
                        "examples_sec": sps as f64,
                        "loader_ratio": 0.0f64,
                        "wall_time": secs as f64,
                    });
                    if let Some(ls) = last_loss_for_log {
                        rec.as_object_mut()
                            .unwrap()
                            .insert("train_loss".into(), serde_json::json!(ls as f64));
                    }
                    lg.write_json(&rec);
                }
                last_report = Instant::now();
                samples_since = 0;
                batches_since = 0;
            }

            // Save checkpoint if requested
            if let Some(interval) = ctx.save_every {
                if total_batches % interval == 0 {
                    let checkpoint_path =
                        ctx.out_dir.join(format!("checkpoint_batch_{}.fp32.bin", total_batches));
                    save_single_network(network, &checkpoint_path)?;
                    if ctx.structured.as_ref().map(|lg| lg.to_stdout).unwrap_or(false) {
                        eprintln!("Saved checkpoint: {}", checkpoint_path.display());
                    } else {
                        println!("Saved checkpoint: {}", checkpoint_path.display());
                    }
                }
            }
        }

        let avg_loss = if total_weight > 0.0 {
            total_loss / total_weight
        } else {
            0.0
        };

        // Validation/metrics
        let mut val_loss = None;
        let mut val_auc: Option<f64> = None;
        let mut val_ece: Option<f64> = None;
        let mut val_wsum: Option<f64> = None;
        if let Some(val_samples) = validation_samples {
            let mut scratch = SingleForwardScratch::new(network.acc_dim);
            let vl = compute_validation_loss_single(network, val_samples, config);
            val_loss = Some(vl);
            val_auc = compute_val_auc_single(network, val_samples, config);
            if ctx.dash.val_is_jsonl && config.label_type == "wdl" {
                // Build bins and write CSV/PNG
                let mut cps = Vec::with_capacity(val_samples.len());
                let mut probs = Vec::with_capacity(val_samples.len());
                let mut labels = Vec::with_capacity(val_samples.len());
                let mut wts = Vec::with_capacity(val_samples.len());
                for s in val_samples.iter() {
                    if let Some(cp) = s.cp {
                        let out = network.forward_with_scratch(&s.features, &mut scratch);
                        let p = 1.0 / (1.0 + (-out).exp());
                        cps.push(cp);
                        probs.push(p);
                        labels.push(s.label);
                        wts.push(s.weight);
                    }
                }
                if !cps.is_empty() {
                    let bins = calibration_bins(
                        &cps,
                        &probs,
                        &labels,
                        &wts,
                        config.cp_clip,
                        ctx.dash.calib_bins_n,
                    );
                    val_ece = ece_from_bins(&bins);
                    if ctx.dash.emit {
                        // Write calibration CSV
                        let mut w = csv::Writer::from_path(
                            ctx.out_dir.join(format!("calibration_epoch_{}.csv", epoch + 1)),
                        )?;
                        w.write_record([
                            "bin_left",
                            "bin_right",
                            "bin_center",
                            "count",
                            "weighted_count",
                            "mean_pred",
                            "mean_label",
                        ])?;
                        for b in &bins {
                            w.write_record([
                                b.left.to_string(),
                                b.right.to_string(),
                                format!("{:.1}", b.center),
                                b.count.to_string(),
                                format!("{:.3}", b.weighted_count),
                                format!("{:.6}", b.mean_pred),
                                format!("{:.6}", b.mean_label),
                            ])?;
                        }
                        w.flush()?;
                        if ctx.dash.do_plots {
                            let points: Vec<(i32, i32, f32, f64, f64)> = bins
                                .iter()
                                .map(|b| (b.left, b.right, b.center, b.mean_pred, b.mean_label))
                                .collect();
                            if let Err(e) = tools::plot::plot_calibration_png(
                                ctx.out_dir.join(format!("calibration_epoch_{}.png", epoch + 1)),
                                &points,
                            ) {
                                eprintln!("plot_calibration_png failed: {}", e);
                            }
                        }
                    }
                }
            }
            // Phase metrics (JSONL only)
            if ctx.dash.val_is_jsonl && ctx.dash.emit {
                // buckets per phase
                let mut probs_buckets: [(Vec<f32>, Vec<f32>, Vec<f32>); 3] = Default::default();
                let mut cp_buckets: [(Vec<f32>, Vec<f32>, Vec<f32>); 3] = Default::default();
                #[inline]
                fn idx_of(phase: GamePhase) -> usize {
                    match phase {
                        GamePhase::Opening => 0,
                        GamePhase::MiddleGame => 1,
                        GamePhase::EndGame => 2,
                    }
                }
                match config.label_type.as_str() {
                    "wdl" => {
                        for s in val_samples.iter() {
                            if let Some(ph) = s.phase {
                                let out = network.forward_with_scratch(&s.features, &mut scratch);
                                let p = 1.0 / (1.0 + (-out).exp());
                                let b = &mut probs_buckets[idx_of(ph)];
                                b.0.push(p);
                                b.1.push(s.label);
                                b.2.push(s.weight);
                            }
                        }
                    }
                    "cp" => {
                        for s in val_samples.iter() {
                            if let Some(ph) = s.phase {
                                let out = network.forward_with_scratch(&s.features, &mut scratch);
                                let b = &mut cp_buckets[idx_of(ph)];
                                b.0.push(out);
                                b.1.push(s.label);
                                b.2.push(s.weight);
                            }
                        }
                    }
                    _ => {}
                }
                let mut wpm = csv::WriterBuilder::new().has_headers(false).from_writer(
                    std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(ctx.out_dir.join("phase_metrics.csv"))?,
                );
                let phases = [
                    GamePhase::Opening,
                    GamePhase::MiddleGame,
                    GamePhase::EndGame,
                ];
                for (i, ph) in phases.iter().enumerate() {
                    match config.label_type.as_str() {
                        "wdl" => {
                            let (ref probs, ref labels, ref wts) = probs_buckets[i];
                            if !probs.is_empty() {
                                let cnt = probs.len();
                                let wsum: f64 = wts.iter().map(|&x| x as f64).sum();
                                if let Some(m) = binary_metrics(probs, labels, wts) {
                                    wpm.write_record([
                                        (epoch + 1).to_string(),
                                        format!("{:?}", ph),
                                        cnt.to_string(),
                                        format!("{:.3}", wsum),
                                        format!("{:.6}", m.logloss),
                                        format!("{:.6}", m.brier),
                                        format!("{:.6}", m.accuracy),
                                        String::new(),
                                        String::new(),
                                    ])?;
                                }
                            }
                        }
                        "cp" => {
                            let (ref preds, ref labels, ref wts) = cp_buckets[i];
                            if !preds.is_empty() {
                                let cnt = preds.len();
                                let wsum: f64 = wts.iter().map(|&x| x as f64).sum();
                                if let Some(r) = regression_metrics(preds, labels, wts) {
                                    wpm.write_record([
                                        (epoch + 1).to_string(),
                                        format!("{:?}", ph),
                                        cnt.to_string(),
                                        format!("{:.3}", wsum),
                                        String::new(),
                                        String::new(),
                                        String::new(),
                                        format!("{:.6}", r.mae),
                                        format!("{:.6}", r.mse),
                                    ])?;
                                }
                            }
                        }
                        _ => {}
                    }
                }
                wpm.flush()?;
            }
            val_wsum = Some(val_samples.iter().map(|s| s.weight as f64).sum());
        }

        let epoch_secs = epoch_start.elapsed().as_secs_f32().max(MIN_ELAPSED_TIME as f32);
        let epoch_sps = (n_samples as f32) / epoch_secs;
        // Update best trackers
        if let Some(vl) = val_loss {
            if vl < *ctx.trackers.best_val_loss {
                *ctx.trackers.best_val_loss = vl;
                *ctx.trackers.best_network = Some(Network::Single(network.clone()));
                *ctx.trackers.best_epoch = Some(epoch + 1);
            }
            *ctx.trackers.last_val_loss = Some(vl);
            // Plateau update (epoch end)
            if let Some(ref mut p) = ctx.plateau {
                if !vl.is_finite() {
                    if ctx.structured.as_ref().map(|lg| lg.to_stdout).unwrap_or(false) {
                        eprintln!("Warning: val_loss is not finite; skipping plateau update");
                    } else {
                        println!("Warning: val_loss is not finite; skipping plateau update");
                    }
                } else if let Some(new_mult) = p.update(vl) {
                    if ctx.structured.as_ref().map(|lg| lg.to_stdout).unwrap_or(false) {
                        eprintln!(
                            "LR plateau: epoch {} → lr *= {:.3} (multiplier now {:.6}, best={:.6}, cur={:.6})",
                            epoch + 1,
                            p.gamma,
                            new_mult,
                            p.best,
                            vl
                        );
                    } else {
                        println!(
                            "LR plateau: epoch {} → lr *= {:.3} (multiplier now {:.6}, best={:.6}, cur={:.6})",
                            epoch + 1,
                            p.gamma,
                            new_mult,
                            p.best,
                            vl
                        );
                    }
                }
            }
        }
        // Emit metrics.csv
        if ctx.dash.emit {
            let mut w = csv::WriterBuilder::new().has_headers(false).from_writer(
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(ctx.out_dir.join("metrics.csv"))?,
            );
            w.write_record([
                (epoch + 1).to_string(),
                format!("{:.6}", avg_loss),
                val_loss.map(|v| format!("{:.6}", v)).unwrap_or_else(|| "".into()),
                val_auc.map(|v| format!("{:.6}", v)).unwrap_or_else(|| "".into()),
                val_ece.map(|v| format!("{:.6}", v)).unwrap_or_else(|| "".into()),
                format!("{:.3}", epoch_secs),
                format!("{:.3}", total_weight),
                val_wsum.map(|v| format!("{:.3}", v)).unwrap_or_else(|| "".into()),
                if Some(epoch + 1) == *ctx.trackers.best_epoch {
                    "1".into()
                } else {
                    "0".into()
                },
            ])?;
            w.flush()?;
        }
        // Structured per-epoch logs (train/val)
        if let Some(ref lg) = ctx.structured {
            let mut rec_train = serde_json::json!({
                "ts": chrono::Utc::now().to_rfc3339(),
                "phase": "train",
                "global_step": ctx.global_step as i64,
                "epoch": (epoch + 1) as i64,
                "lr": last_lr_base as f64,
                "train_loss": avg_loss as f64,
                "examples_sec": epoch_sps as f64,
                "loader_ratio": 0.0f64,
                "wall_time": epoch_secs as f64,
            });
            // Bake training_config (Spec#12)
            if let Some(obj) = ctx.training_config_json.clone() {
                rec_train.as_object_mut().unwrap().insert("training_config".into(), obj);
            }
            lg.write_json(&rec_train);
            if let Some(vl) = val_loss {
                let mut rec_val = serde_json::json!({
                    "ts": chrono::Utc::now().to_rfc3339(),
                    "phase": "val",
                    "global_step": ctx.global_step as i64,
                    "epoch": (epoch + 1) as i64,
                    "val_loss": vl as f64,
                    "wall_time": epoch_secs as f64,
                });
                if let Some(obj) = ctx.training_config_json.clone() {
                    rec_val.as_object_mut().unwrap().insert("training_config".into(), obj);
                }
                if let Some(a) = val_auc {
                    rec_val
                        .as_object_mut()
                        .unwrap()
                        .insert("val_auc".to_string(), serde_json::json!(a));
                }
                if let Some(e) = val_ece {
                    rec_val
                        .as_object_mut()
                        .unwrap()
                        .insert("val_ece".to_string(), serde_json::json!(e));
                }
                lg.write_json(&rec_val);
            }
        }
        // Console log summary
        if ctx.structured.as_ref().map(|lg| lg.to_stdout).unwrap_or(false) {
            eprintln!(
                "Epoch {}/{}: train_loss={:.4} val_loss={} time={:.2}s sps={:.0}",
                epoch + 1,
                config.epochs,
                avg_loss,
                val_loss.map(|v| format!("{:.4}", v)).unwrap_or_else(|| "NA".into()),
                epoch_secs,
                epoch_sps
            );
        } else {
            println!(
                "Epoch {}/{}: train_loss={:.4} val_loss={} time={:.2}s sps={:.0}",
                epoch + 1,
                config.epochs,
                avg_loss,
                val_loss.map(|v| format!("{:.4}", v)).unwrap_or_else(|| "NA".into()),
                epoch_secs,
                epoch_sps
            );
        }
        print_zero_weight_debug(epoch, zero_weight_batches, &ctx.structured);
    }

    Ok(())
}

pub fn train_model_stream_cache(
    network: &mut Network,
    cache_path: &str,
    validation_samples: &Option<Vec<Sample>>,
    config: &Config,
    _rng: &mut StdRng,
    ctx: &mut TrainContext,
    weighting: &wcfg::WeightingConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let network = match network {
        Network::Single(inner) => inner,
        Network::Classic(_) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "Classic アーキの学習ループは実装中です",
            )
            .into());
        }
    };
    // Use ctx fields directly in this function to avoid borrow confusion
    // If prefetch=0, run synchronous streaming in the training thread (no background worker)
    if config.prefetch_batches == 0 {
        let mut adam_state = if config.optimizer == "adam" {
            Some(SingleAdamState::new(network))
        } else {
            None
        };
        // Open and parse header via helper
        for epoch in 0..config.epochs {
            let epoch_start = Instant::now();
            let (mut r, num_samples, flags_mask) = open_cache_payload_reader(cache_path)?;

            // Epoch loop
            let mut total_loss = 0.0f32;
            let mut total_weight = 0.0f32;
            let mut batch_count = 0usize;
            let mut total_samples_epoch = 0usize;

            let mut last_report = Instant::now();
            let mut samples_since = 0usize;
            let mut batches_since = 0usize;
            let mut read_ns_since: u128 = 0;
            let mut read_ns_epoch: u128 = 0;

            let mut loaded: u64 = 0;
            let mut last_lr_base = config.learning_rate;
            let mut last_loss_for_log: Option<f32> = None;
            let mut zero_weight_batches: usize = 0;
            while loaded < num_samples {
                // Read up to batch_size samples synchronously
                let mut batch = Vec::with_capacity(config.batch_size);
                let t_read0 = Instant::now();
                for _ in 0..config.batch_size {
                    if loaded >= num_samples {
                        break;
                    }
                    // n_features
                    let mut nb = [0u8; 4];
                    if let Err(e) = r.read_exact(&mut nb) {
                        return Err(format!("Read error at sample {}: {}", loaded, e).into());
                    }
                    let n_features = u32::from_le_bytes(nb) as usize;
                    const MAX_FEATURES_PER_SAMPLE: usize = SHOGI_BOARD_SIZE * FE_END;
                    if n_features > MAX_FEATURES_PER_SAMPLE {
                        return Err("n_features exceeds sane limit".into());
                    }
                    let mut features: Vec<u32> = vec![0u32; n_features];
                    #[cfg(target_endian = "little")]
                    {
                        use bytemuck::cast_slice_mut;
                        if let Err(e) = r.read_exact(cast_slice_mut::<u32, u8>(&mut features)) {
                            return Err(format!("Read features failed at {}: {}", loaded, e).into());
                        }
                    }
                    #[cfg(target_endian = "big")]
                    {
                        let mut buf = vec![0u8; n_features * 4];
                        if let Err(e) = r.read_exact(&mut buf) {
                            return Err(format!("Read features failed at {}: {}", loaded, e).into());
                        }
                        for (dst, chunk) in features.iter_mut().zip(buf.chunks_exact(4)) {
                            *dst = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                        }
                    }
                    let mut lb = [0u8; 4];
                    if let Err(e) = r.read_exact(&mut lb) {
                        return Err(format!("Read label failed at {}: {}", loaded, e).into());
                    }
                    let label = f32::from_le_bytes(lb);
                    let mut gapb = [0u8; 2];
                    if let Err(e) = r.read_exact(&mut gapb) {
                        return Err(format!("Read gap failed at {}: {}", loaded, e).into());
                    }
                    let gap = u16::from_le_bytes(gapb);
                    let mut depth = [0u8; 1];
                    if let Err(e) = r.read_exact(&mut depth) {
                        return Err(format!("Read depth failed at {}: {}", loaded, e).into());
                    }
                    let depth = depth[0];
                    let mut seldepth = [0u8; 1];
                    if let Err(e) = r.read_exact(&mut seldepth) {
                        return Err(format!("Read seldepth failed at {}: {}", loaded, e).into());
                    }
                    let seldepth = seldepth[0];
                    let mut flags = [0u8; 1];
                    if let Err(e) = r.read_exact(&mut flags) {
                        return Err(format!("Read flags failed at {}: {}", loaded, e).into());
                    }
                    let flags = flags[0];
                    let _unknown = (flags as u32) & !flags_mask; // ignore warn in sync path
                    let mut weight = 1.0f32;
                    let base_gap = (gap as f32 / GAP_WEIGHT_DIVISOR).clamp(BASELINE_MIN_EPS, 1.0);
                    weight *= base_gap;
                    let both_exact = (flags & fc_flags::BOTH_EXACT) != 0;
                    weight *= if both_exact {
                        1.0
                    } else {
                        NON_EXACT_BOUND_WEIGHT
                    };
                    let mate_ring = (flags & fc_flags::MATE_BOUNDARY) != 0;
                    if mate_ring {
                        weight *= 0.5;
                    }
                    if seldepth < depth.saturating_add(SELECTIVE_DEPTH_MARGIN as u8) {
                        weight *= SELECTIVE_DEPTH_WEIGHT;
                    }
                    // Apply Spec#12 coefficients (phase unknown in cache → not applied)
                    weight = wcfg::apply_weighting(
                        weight,
                        weighting,
                        Some(both_exact),
                        Some(gap as i32),
                        None,
                        Some(mate_ring),
                    );

                    batch.push(Sample {
                        features,
                        label,
                        weight,
                        cp: None,
                        phase: None,
                    });
                    loaded += 1;
                }
                let t_read1 = Instant::now();
                let read_ns = t_read1.duration_since(t_read0).as_nanos();
                read_ns_since += read_ns;
                read_ns_epoch += read_ns;

                if batch.is_empty() {
                    break;
                }

                // consume last_loss_for_log to avoid unused-assignment lint across iterations
                let _ = last_loss_for_log;
                let indices: Vec<usize> = (0..batch.len()).collect();
                // LR scheduling
                let lr_base = lr_base_for(epoch, ctx.global_step, config, ctx.plateau.as_ref());
                // 先にバッチ重みを集計し、ゼロなら計算自体をスキップ
                let batch_weight: f32 = batch.iter().map(|s| s.weight).sum();
                if batch_weight > 0.0 {
                    let loss = train_batch_by_indices(
                        network,
                        &batch,
                        &indices,
                        config,
                        &mut adam_state,
                        lr_base,
                    );
                    total_loss += loss * batch_weight;
                    total_weight += batch_weight;
                    last_loss_for_log = Some(loss);
                } else {
                    zero_weight_batches += 1;
                    last_loss_for_log = None;
                }
                last_lr_base = lr_base;

                total_samples_epoch += batch.len();
                batch_count += 1;
                batches_since += 1;
                samples_since += batch.len();

                // Define completed-batch semantics: increment before logging
                ctx.global_step += 1;
                if last_report.elapsed().as_secs_f32() >= config.throughput_interval_sec
                    && batches_since > 0
                {
                    let secs = last_report.elapsed().as_secs_f32().max(MIN_ELAPSED_TIME as f32);
                    let sps = samples_since as f32 / secs;
                    let bps = batches_since as f32 / secs;
                    let avg_bs = samples_since as f32 / batches_since as f32;
                    let loader_ratio = ((read_ns_since as f64)
                        / (secs as f64 * NANOSECONDS_PER_SECOND))
                        .clamp(0.0, 1.0)
                        * PERCENTAGE_DIVISOR as f64;
                    if ctx.structured.as_ref().map(|lg| lg.to_stdout).unwrap_or(false) {
                        eprintln!(
                            "[throughput] mode=stream-sync epoch={} batches={} sps={:.0} bps={:.2} avg_batch={:.1} loader_ratio={:.1}%",
                            epoch + 1, batch_count, sps, bps, avg_bs, loader_ratio
                        );
                    } else {
                        println!(
                            "[throughput] mode=stream-sync epoch={} batches={} sps={:.0} bps={:.2} avg_batch={:.1} loader_ratio={:.1}%",
                            epoch + 1, batch_count, sps, bps, avg_bs, loader_ratio
                        );
                    }
                    if let Some(ref lg) = ctx.structured {
                        let mut rec = serde_json::json!({
                            "ts": chrono::Utc::now().to_rfc3339(),
                            "phase": "train",
                            "global_step": ctx.global_step as i64,
                            "epoch": (epoch + 1) as i64,
                            "lr": lr_base as f64,
                            "examples_sec": sps as f64,
                            "loader_ratio": loader_ratio/100.0,
                            "wall_time": secs as f64,
                        });
                        if let Some(ls) = last_loss_for_log {
                            rec.as_object_mut()
                                .unwrap()
                                .insert("train_loss".into(), serde_json::json!(ls as f64));
                        }
                        lg.write_json(&rec);
                    }
                    last_report = Instant::now();
                    samples_since = 0;
                    batches_since = 0;
                    read_ns_since = 0;
                }
                // already incremented above to represent completed batches
            }

            let avg_loss = if total_weight > 0.0 {
                total_loss / total_weight
            } else {
                0.0
            };
            let has_val = validation_samples.is_some();
            let (val_loss, val_auc, val_ece, val_wsum): (
                f32,
                Option<f64>,
                Option<f64>,
                Option<f64>,
            ) = if let Some(val_samples) = validation_samples {
                let vl = compute_validation_loss_single(network, val_samples, config);
                let (auc, ece) =
                    compute_val_auc_and_ece_single(network, val_samples, config, &ctx.dash);
                let wsum = Some(val_samples.iter().map(|s| s.weight as f64).sum());
                (vl, auc, ece, wsum)
            } else {
                (0.0, None, None, None)
            };
            let epoch_secs = epoch_start.elapsed().as_secs_f32().max(MIN_ELAPSED_TIME as f32);
            let loader_ratio_epoch = ((read_ns_epoch as f64)
                / (epoch_secs as f64 * NANOSECONDS_PER_SECOND))
                .clamp(0.0, 1.0)
                * PERCENTAGE_DIVISOR as f64;
            let epoch_sps = (total_samples_epoch as f32) / epoch_secs;
            // Update best trackers (only when validation is present)
            if has_val {
                if val_loss < *ctx.trackers.best_val_loss {
                    *ctx.trackers.best_val_loss = val_loss;
                    *ctx.trackers.best_network = Some(Network::Single(network.clone()));
                    *ctx.trackers.best_epoch = Some(epoch + 1);
                }
                *ctx.trackers.last_val_loss = Some(val_loss);
                // Plateau update (epoch end)
                if let Some(ref mut p) = ctx.plateau {
                    if !val_loss.is_finite() {
                        if ctx.structured.as_ref().map(|lg| lg.to_stdout).unwrap_or(false) {
                            eprintln!("Warning: val_loss is not finite; skipping plateau update");
                        } else {
                            println!("Warning: val_loss is not finite; skipping plateau update");
                        }
                    } else if let Some(new_mult) = p.update(val_loss) {
                        if ctx.structured.as_ref().map(|lg| lg.to_stdout).unwrap_or(false) {
                            eprintln!(
                                "LR plateau: epoch {} → lr *= {:.3} (multiplier now {:.6}, best={:.6}, cur={:.6})",
                                epoch + 1,
                                p.gamma,
                                new_mult,
                                p.best,
                                val_loss
                            );
                        } else {
                            println!(
                                "LR plateau: epoch {} → lr *= {:.3} (multiplier now {:.6}, best={:.6}, cur={:.6})",
                                epoch + 1,
                                p.gamma,
                                new_mult,
                                p.best,
                                val_loss
                            );
                        }
                    }
                }
            }
            if ctx.structured.as_ref().map(|lg| lg.to_stdout).unwrap_or(false) {
                eprintln!(
                    "Epoch {}/{}: train_loss={:.4} val_loss={:.4} batches={} time={:.2}s sps={:.0} loader_ratio={:.1}%",
                    epoch + 1, config.epochs, avg_loss, val_loss, batch_count, epoch_secs, epoch_sps, loader_ratio_epoch
                );
            } else {
                println!(
                    "Epoch {}/{}: train_loss={:.4} val_loss={:.4} batches={} time={:.2}s sps={:.0} loader_ratio={:.1}%",
                    epoch + 1, config.epochs, avg_loss, val_loss, batch_count, epoch_secs, epoch_sps, loader_ratio_epoch
                );
            }
            // zero-weight debug: print once per epoch after summary (handled below)
            if let Some(ref lg) = ctx.structured {
                let mut rec_train = serde_json::json!({
                    "ts": chrono::Utc::now().to_rfc3339(),
                    "phase": "train",
                    "global_step": ctx.global_step as i64,
                    "epoch": (epoch + 1) as i64,
                    "lr": last_lr_base as f64,
                    "train_loss": avg_loss as f64,
                    "examples_sec": epoch_sps as f64,
                    "loader_ratio": (loader_ratio_epoch/100.0) ,
                    "wall_time": epoch_secs as f64,
                });
                if let Some(obj) = ctx.training_config_json.clone() {
                    rec_train.as_object_mut().unwrap().insert("training_config".into(), obj);
                }
                lg.write_json(&rec_train);
                let mut rec_val = serde_json::json!({
                    "ts": chrono::Utc::now().to_rfc3339(),
                    "phase": "val",
                    "global_step": ctx.global_step as i64,
                    "epoch": (epoch + 1) as i64,
                    "wall_time": epoch_secs as f64,
                });
                if let Some(obj) = ctx.training_config_json.clone() {
                    rec_val.as_object_mut().unwrap().insert("training_config".into(), obj);
                }
                if has_val {
                    rec_val
                        .as_object_mut()
                        .unwrap()
                        .insert("val_loss".to_string(), serde_json::json!(val_loss as f64));
                }
                if let Some(a) = val_auc {
                    rec_val
                        .as_object_mut()
                        .unwrap()
                        .insert("val_auc".to_string(), serde_json::json!(a));
                }
                if let Some(e) = val_ece {
                    rec_val
                        .as_object_mut()
                        .unwrap()
                        .insert("val_ece".to_string(), serde_json::json!(e));
                }
                lg.write_json(&rec_val);
            }

            // Emit metrics.csv (sync stream-cache path)
            if ctx.dash.emit {
                let mut w = csv::WriterBuilder::new().has_headers(false).from_writer(
                    std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(ctx.out_dir.join("metrics.csv"))?,
                );
                let val_loss_str = if has_val {
                    format!("{:.6}", val_loss)
                } else {
                    String::new()
                };
                w.write_record([
                    (epoch + 1).to_string(),
                    format!("{:.6}", avg_loss),
                    val_loss_str,
                    val_auc.map(|v| format!("{:.6}", v)).unwrap_or_else(|| "".into()),
                    val_ece.map(|v| format!("{:.6}", v)).unwrap_or_else(|| "".into()),
                    format!("{:.3}", epoch_secs),
                    format!("{:.3}", total_weight),
                    val_wsum.map(|v| format!("{:.3}", v)).unwrap_or_else(|| "".into()),
                    if Some(epoch + 1) == *ctx.trackers.best_epoch {
                        "1".into()
                    } else {
                        "0".into()
                    },
                ])?;
                w.flush()?;
            }

            // print once per epoch
            print_zero_weight_debug(epoch, zero_weight_batches, &ctx.structured);
        }

        return Ok(());
    }

    // Async streaming loader path
    // Optionally cap prefetch by bytes (rough estimate)
    let mut effective_prefetch = config.prefetch_batches.max(1);
    if let Some(bytes_cap) = config.prefetch_bytes.filter(|&b| b > 0) {
        // Estimate per-sample bytes: header/meta (~32B) + 4B * estimated_features
        let est_sample_bytes =
            32usize.saturating_add(4usize.saturating_mul(config.estimated_features_per_sample));
        let est_batch_bytes = config.batch_size.saturating_mul(est_sample_bytes);
        if est_batch_bytes > 0 {
            let max_batches = (bytes_cap / est_batch_bytes).max(1);
            if effective_prefetch > max_batches {
                if ctx.structured.as_ref().map(|lg| lg.to_stdout).unwrap_or(false) {
                    eprintln!(
                        "Capping prefetch-batches from {} to {} by --prefetch-bytes={} (~{} bytes/batch; est_feats/sample={})",
                        effective_prefetch, max_batches, bytes_cap, est_batch_bytes, config.estimated_features_per_sample
                    );
                } else {
                    println!(
                        "Capping prefetch-batches from {} to {} by --prefetch-bytes={} (~{} bytes/batch; est_feats/sample={})",
                        effective_prefetch, max_batches, bytes_cap, est_batch_bytes, config.estimated_features_per_sample
                    );
                }
                effective_prefetch = max_batches;
            }
        }
    }
    let mut loader = StreamCacheLoader::new(
        cache_path.to_string(),
        config.batch_size,
        effective_prefetch,
        weighting.clone(),
    );
    let mut adam_state = if config.optimizer == "adam" {
        Some(SingleAdamState::new(network))
    } else {
        None
    };

    let mut total_batches = 0usize;

    for epoch in 0..config.epochs {
        let epoch_start = Instant::now();
        loader.start_epoch()?;

        let mut total_loss = 0.0f32;
        let mut total_weight = 0.0f32;
        let mut batch_count = 0usize;
        let mut total_samples_epoch = 0usize;

        let mut last_report = Instant::now();
        let mut samples_since = 0usize;
        let mut batches_since = 0usize;
        let mut wait_ns_since: u128 = 0;
        let mut wait_ns_epoch: u128 = 0;
        let mut last_loss_for_log: Option<f32> = None;
        let mut zero_weight_batches: usize = 0;

        let mut last_lr_base = config.learning_rate;
        loop {
            let (maybe_batch, wait_dur) = loader.next_batch_with_wait();
            // consume last_loss_for_log to avoid unused-assignment lint across iterations
            let _ = last_loss_for_log;
            let Some(batch_res) = maybe_batch else { break };
            let batch = match batch_res {
                Ok(b) => b,
                Err(msg) => return Err(msg.into()),
            };
            let indices: Vec<usize> = (0..batch.len()).collect();
            // LR scheduling
            let lr_base = lr_base_for(epoch, ctx.global_step, config, ctx.plateau.as_ref());
            // 先にバッチ重みを集計し、ゼロなら計算自体をスキップ
            let batch_weight: f32 = batch.iter().map(|s| s.weight).sum();
            if batch_weight > 0.0 {
                let loss = train_batch_by_indices(
                    network,
                    &batch,
                    &indices,
                    config,
                    &mut adam_state,
                    lr_base,
                );
                total_loss += loss * batch_weight;
                total_weight += batch_weight;
                last_loss_for_log = Some(loss);
            } else {
                zero_weight_batches += 1;
                last_loss_for_log = None;
            }
            last_lr_base = lr_base;

            total_samples_epoch += batch.len();
            batch_count += 1;
            total_batches += 1;
            samples_since += batch.len();
            batches_since += 1;
            wait_ns_since += wait_dur.as_nanos();
            wait_ns_epoch += wait_dur.as_nanos();

            ctx.global_step += 1;
            if last_report.elapsed().as_secs_f32() >= config.throughput_interval_sec
                && batches_since > 0
            {
                let secs = last_report.elapsed().as_secs_f32().max(MIN_ELAPSED_TIME as f32);
                let sps = samples_since as f32 / secs;
                let bps = batches_since as f32 / secs;
                let avg_bs = samples_since as f32 / batches_since as f32;
                let wait_secs = (wait_ns_since as f64) / NANOSECONDS_PER_SECOND;
                let loader_ratio = if secs > 0.0 {
                    (wait_secs / secs as f64) * PERCENTAGE_DIVISOR as f64
                } else {
                    0.0
                };
                if ctx.structured.as_ref().map(|lg| lg.to_stdout).unwrap_or(false) {
                    eprintln!(
                    "[throughput] mode=stream epoch={} batches={} sps={:.0} bps={:.2} avg_batch={:.1} loader_ratio={:.1}%",
                    epoch + 1,
                    batch_count,
                    sps,
                    bps,
                    avg_bs,
                    loader_ratio
                    );
                } else {
                    println!(
                    "[throughput] mode=stream epoch={} batches={} sps={:.0} bps={:.2} avg_batch={:.1} loader_ratio={:.1}%",
                    epoch + 1,
                    batch_count,
                    sps,
                    bps,
                    avg_bs,
                    loader_ratio
                    );
                }
                if let Some(ref lg) = ctx.structured {
                    let mut rec = serde_json::json!({
                        "ts": chrono::Utc::now().to_rfc3339(),
                        "phase": "train",
                        "global_step": ctx.global_step as i64,
                        "epoch": (epoch + 1) as i64,
                        "lr": lr_base as f64,
                        "examples_sec": sps as f64,
                        "loader_ratio": (loader_ratio )/100.0,
                        "wall_time": secs as f64,
                    });
                    if let Some(ls) = last_loss_for_log {
                        rec.as_object_mut()
                            .unwrap()
                            .insert("train_loss".into(), serde_json::json!(ls as f64));
                    }
                    lg.write_json(&rec);
                }
                last_report = Instant::now();
                samples_since = 0;
                batches_since = 0;
                wait_ns_since = 0;
            }

            if let Some(interval) = ctx.save_every {
                if total_batches % interval == 0 {
                    let checkpoint_path =
                        ctx.out_dir.join(format!("checkpoint_batch_{total_batches}.fp32.bin"));
                    save_single_network(network, &checkpoint_path)?;
                    if ctx.structured.as_ref().map(|lg| lg.to_stdout).unwrap_or(false) {
                        eprintln!("Saved checkpoint: {}", checkpoint_path.display());
                    } else {
                        println!("Saved checkpoint: {}", checkpoint_path.display());
                    }
                }
            }
        }

        let avg_loss = if total_weight > 0.0 {
            total_loss / total_weight
        } else {
            0.0
        };
        let mut val_loss = None;
        let mut val_auc: Option<f64> = None;
        let mut val_ece: Option<f64> = None;
        let mut val_wsum: Option<f64> = None;
        if let Some(val_samples) = validation_samples {
            let vl = compute_validation_loss_single(network, val_samples, config);
            val_loss = Some(vl);
            val_auc = compute_val_auc_single(network, val_samples, config);
            if ctx.dash.val_is_jsonl && config.label_type == "wdl" {
                // Calibration CSV/PNG
                let mut cps = Vec::new();
                let mut probs = Vec::new();
                let mut labels = Vec::new();
                let mut wts = Vec::new();
                let mut scratch_val = SingleForwardScratch::new(network.acc_dim);
                for s in val_samples.iter() {
                    if let Some(cp) = s.cp {
                        let out = network.forward_with_scratch(&s.features, &mut scratch_val);
                        let p = 1.0 / (1.0 + (-out).exp());
                        cps.push(cp);
                        probs.push(p);
                        labels.push(s.label);
                        wts.push(s.weight);
                    }
                }
                if !cps.is_empty() {
                    let bins = calibration_bins(
                        &cps,
                        &probs,
                        &labels,
                        &wts,
                        config.cp_clip,
                        ctx.dash.calib_bins_n,
                    );
                    val_ece = ece_from_bins(&bins);
                    if ctx.dash.emit {
                        let mut w = csv::Writer::from_path(
                            ctx.out_dir.join(format!("calibration_epoch_{}.csv", epoch + 1)),
                        )?;
                        w.write_record([
                            "bin_left",
                            "bin_right",
                            "bin_center",
                            "count",
                            "weighted_count",
                            "mean_pred",
                            "mean_label",
                        ])?;
                        for b in &bins {
                            w.write_record([
                                b.left.to_string(),
                                b.right.to_string(),
                                format!("{:.1}", b.center),
                                b.count.to_string(),
                                format!("{:.3}", b.weighted_count),
                                format!("{:.6}", b.mean_pred),
                                format!("{:.6}", b.mean_label),
                            ])?;
                        }
                        w.flush()?;
                        if ctx.dash.do_plots {
                            let points: Vec<(i32, i32, f32, f64, f64)> = bins
                                .iter()
                                .map(|b| (b.left, b.right, b.center, b.mean_pred, b.mean_label))
                                .collect();
                            if let Err(e) = tools::plot::plot_calibration_png(
                                ctx.out_dir.join(format!("calibration_epoch_{}.png", epoch + 1)),
                                &points,
                            ) {
                                eprintln!("plot_calibration_png failed: {}", e);
                            }
                        }
                    }
                }
            }
            // Phase metrics (JSONL only)
            if ctx.dash.val_is_jsonl && ctx.dash.emit {
                let mut probs_buckets: [(Vec<f32>, Vec<f32>, Vec<f32>); 3] = Default::default();
                let mut cp_buckets: [(Vec<f32>, Vec<f32>, Vec<f32>); 3] = Default::default();
                #[inline]
                fn idx_of(phase: GamePhase) -> usize {
                    match phase {
                        GamePhase::Opening => 0,
                        GamePhase::MiddleGame => 1,
                        GamePhase::EndGame => 2,
                    }
                }
                match config.label_type.as_str() {
                    "wdl" => {
                        let mut scratch_phase = SingleForwardScratch::new(network.acc_dim);
                        for s in val_samples.iter() {
                            if let Some(ph) = s.phase {
                                let out = network.forward_with_scratch(&s.features, &mut scratch_phase);
                                let p = 1.0 / (1.0 + (-out).exp());
                                let b = &mut probs_buckets[idx_of(ph)];
                                b.0.push(p);
                                b.1.push(s.label);
                                b.2.push(s.weight);
                            }
                        }
                    }
                    "cp" => {
                        let mut scratch_phase = SingleForwardScratch::new(network.acc_dim);
                        for s in val_samples.iter() {
                            if let Some(ph) = s.phase {
                                let out = network.forward_with_scratch(&s.features, &mut scratch_phase);
                                let b = &mut cp_buckets[idx_of(ph)];
                                b.0.push(out);
                                b.1.push(s.label);
                                b.2.push(s.weight);
                            }
                        }
                    }
                    _ => {}
                }
                let mut wpm = csv::WriterBuilder::new().has_headers(false).from_writer(
                    std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(ctx.out_dir.join("phase_metrics.csv"))?,
                );
                let phases = [
                    GamePhase::Opening,
                    GamePhase::MiddleGame,
                    GamePhase::EndGame,
                ];
                for (i, ph) in phases.iter().enumerate() {
                    match config.label_type.as_str() {
                        "wdl" => {
                            let (ref probs, ref labels, ref wts) = probs_buckets[i];
                            if !probs.is_empty() {
                                let cnt = probs.len();
                                let wsum: f64 = wts.iter().map(|&x| x as f64).sum();
                                if let Some(m) = binary_metrics(probs, labels, wts) {
                                    wpm.write_record([
                                        (epoch + 1).to_string(),
                                        format!("{:?}", ph),
                                        cnt.to_string(),
                                        format!("{:.3}", wsum),
                                        format!("{:.6}", m.logloss),
                                        format!("{:.6}", m.brier),
                                        format!("{:.6}", m.accuracy),
                                        String::new(),
                                        String::new(),
                                    ])?;
                                }
                            }
                        }
                        "cp" => {
                            let (ref preds, ref labels, ref wts) = cp_buckets[i];
                            if !preds.is_empty() {
                                let cnt = preds.len();
                                let wsum: f64 = wts.iter().map(|&x| x as f64).sum();
                                if let Some(r) = regression_metrics(preds, labels, wts) {
                                    wpm.write_record([
                                        (epoch + 1).to_string(),
                                        format!("{:?}", ph),
                                        cnt.to_string(),
                                        format!("{:.3}", wsum),
                                        String::new(),
                                        String::new(),
                                        String::new(),
                                        format!("{:.6}", r.mae),
                                        format!("{:.6}", r.mse),
                                    ])?;
                                }
                            }
                        }
                        _ => {}
                    }
                }
                wpm.flush()?;
            }
            val_wsum = Some(val_samples.iter().map(|s| s.weight as f64).sum());
        }
        let epoch_secs = epoch_start.elapsed().as_secs_f32().max(MIN_ELAPSED_TIME as f32);
        let wait_secs_epoch = (wait_ns_epoch as f64) / NANOSECONDS_PER_SECOND;
        let loader_ratio_epoch = if epoch_secs > 0.0 {
            ((wait_secs_epoch / epoch_secs as f64) * PERCENTAGE_DIVISOR as f64) as f32
        } else {
            0.0
        };
        let epoch_sps = (total_samples_epoch as f32) / epoch_secs;
        if let Some(vl) = val_loss {
            if vl < *ctx.trackers.best_val_loss {
                *ctx.trackers.best_val_loss = vl;
                *ctx.trackers.best_network = Some(Network::Single(network.clone()));
                *ctx.trackers.best_epoch = Some(epoch + 1);
            }
            *ctx.trackers.last_val_loss = Some(vl);
        }
        if ctx.dash.emit {
            let mut w = csv::WriterBuilder::new().has_headers(false).from_writer(
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(ctx.out_dir.join("metrics.csv"))?,
            );
            w.write_record([
                (epoch + 1).to_string(),
                format!("{:.6}", avg_loss),
                val_loss.map(|v| format!("{:.6}", v)).unwrap_or_else(|| "".into()),
                val_auc.map(|v| format!("{:.6}", v)).unwrap_or_else(|| "".into()),
                val_ece.map(|v| format!("{:.6}", v)).unwrap_or_else(|| "".into()),
                format!("{:.3}", epoch_secs),
                format!("{:.3}", total_weight),
                val_wsum.map(|v| format!("{:.3}", v)).unwrap_or_else(|| "".into()),
                if Some(epoch + 1) == *ctx.trackers.best_epoch {
                    "1".into()
                } else {
                    "0".into()
                },
            ])?;
            w.flush()?;
        }
        if ctx.structured.as_ref().map(|lg| lg.to_stdout).unwrap_or(false) {
            eprintln!(
                "Epoch {}/{}: train_loss={:.4} val_loss={} batches={} time={:.2}s sps={:.0} loader_ratio={:.1}%",
                epoch + 1,
                config.epochs,
                avg_loss,
                val_loss.map(|v| format!("{:.4}", v)).unwrap_or_else(|| "NA".into()),
                batch_count,
                epoch_secs,
                epoch_sps,
                loader_ratio_epoch
            );
        } else {
            println!(
                "Epoch {}/{}: train_loss={:.4} val_loss={} batches={} time={:.2}s sps={:.0} loader_ratio={:.1}%",
                epoch + 1,
                config.epochs,
                avg_loss,
                val_loss.map(|v| format!("{:.4}", v)).unwrap_or_else(|| "NA".into()),
                batch_count,
                epoch_secs,
                epoch_sps,
                loader_ratio_epoch
            );
        }
        if let Some(ref lg) = ctx.structured {
            let mut rec_train = serde_json::json!({
                "ts": chrono::Utc::now().to_rfc3339(),
                "phase": "train",
                "global_step": ctx.global_step as i64,
                "epoch": (epoch + 1) as i64,
                "lr": last_lr_base as f64,
                "train_loss": avg_loss as f64,
                "examples_sec": epoch_sps as f64,
                "loader_ratio": (loader_ratio_epoch as f64)/100.0,
                "wall_time": epoch_secs as f64,
            });
            if let Some(obj) = ctx.training_config_json.clone() {
                rec_train.as_object_mut().unwrap().insert("training_config".into(), obj);
            }
            lg.write_json(&rec_train);
            let mut rec_val = serde_json::json!({
                "ts": chrono::Utc::now().to_rfc3339(),
                "phase": "val",
                "global_step": ctx.global_step as i64,
                "epoch": (epoch + 1) as i64,
                "wall_time": epoch_secs as f64,
            });
            if let Some(obj) = ctx.training_config_json.clone() {
                rec_val.as_object_mut().unwrap().insert("training_config".into(), obj);
            }
            if let Some(vl) = val_loss {
                rec_val
                    .as_object_mut()
                    .unwrap()
                    .insert("val_loss".to_string(), serde_json::json!(vl as f64));
            }
            if let Some(a) = val_auc {
                rec_val
                    .as_object_mut()
                    .unwrap()
                    .insert("val_auc".to_string(), serde_json::json!(a));
            }
            if let Some(e) = val_ece {
                rec_val
                    .as_object_mut()
                    .unwrap()
                    .insert("val_ece".to_string(), serde_json::json!(e));
            }
            lg.write_json(&rec_val);
        }

        print_zero_weight_debug(epoch, zero_weight_batches, &ctx.structured);

        loader.finish();
    }

    Ok(())
}
pub fn train_model_with_loader(
    network: &mut Network,
    train_samples: Vec<Sample>,
    validation_samples: &Option<Vec<Sample>>,
    config: &Config,
    rng: &mut StdRng,
    ctx: &mut TrainContext,
) -> Result<(), Box<dyn std::error::Error>> {
    let network = match network {
        Network::Single(inner) => inner,
        Network::Classic(_) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "Classic アーキの学習ループは実装中です",
            )
            .into());
        }
    };
    // Local aliases to minimize code churn (avoid double references)
    let out_dir = ctx.out_dir;
    let dash = &ctx.dash;
    let save_every = ctx.save_every;
    let best_network: &mut Option<Network> = ctx.trackers.best_network;
    let best_val_loss: &mut f32 = ctx.trackers.best_val_loss;
    let last_val_loss: &mut Option<f32> = ctx.trackers.last_val_loss;
    let best_epoch: &mut Option<usize> = ctx.trackers.best_epoch;
    let train_samples_arc = Arc::new(train_samples);
    let mut adam_state = if config.optimizer == "adam" {
        Some(SingleAdamState::new(network))
    } else {
        None
    };

    let mut total_batches = 0;

    if config.prefetch_batches > 0 {
        // Async prefetch path
        let mut async_loader = AsyncBatchLoader::new(
            train_samples_arc.len(),
            config.batch_size,
            config.prefetch_batches,
        );

        for epoch in 0..config.epochs {
            let epoch_start = Instant::now();
            let seed: u64 = rng.random();
            async_loader.start_epoch(config.shuffle, seed);

            let mut total_loss = 0.0;
            let mut total_weight = 0.0;
            let mut batch_count = 0usize;

            let mut last_report = Instant::now();
            let mut samples_since = 0usize;
            let mut batches_since = 0usize;
            let mut wait_ns_since: u128 = 0;
            let mut wait_ns_epoch: u128 = 0;
            let mut last_lr_base = config.learning_rate;
            let mut last_loss_for_log: Option<f32> = None;
            let mut zero_weight_batches: usize = 0;

            loop {
                let (maybe_indices, wait_dur) = async_loader.next_batch_with_wait();
                // consume last_loss_for_log to avoid unused-assignment lint across iterations
                let _ = last_loss_for_log;
                let Some(indices) = maybe_indices else { break };
                // LR scheduling
                let lr_base = lr_base_for(epoch, ctx.global_step, config, ctx.plateau.as_ref());
                // 先にバッチ重みを集計し、ゼロなら計算自体をスキップ
                let batch_weight: f32 =
                    indices.iter().map(|&idx| train_samples_arc[idx].weight).sum();
                if batch_weight > 0.0 {
                    let loss = train_batch_by_indices(
                        network,
                        &train_samples_arc,
                        &indices,
                        config,
                        &mut adam_state,
                        lr_base,
                    );
                    total_loss += loss * batch_weight;
                    total_weight += batch_weight;
                    last_loss_for_log = Some(loss);
                } else {
                    zero_weight_batches += 1;
                    last_loss_for_log = None;
                }

                let batch_len = indices.len();
                batch_count += 1;
                total_batches += 1;
                samples_since += batch_len;
                batches_since += 1;
                wait_ns_since += wait_dur.as_nanos();
                wait_ns_epoch += wait_dur.as_nanos();
                last_lr_base = lr_base;
                // Approximate compute time as time taken by train_batch (dominant)
                // Note: train_batch_by_indices already executed; we estimate by subtracting wait from interval wall time on print, but here we track per-batch compute as 0.
                // Instead, measure explicitly around forward+backward: do a local timing.
                // For minimal invasiveness, we cannot re-run compute; so we estimate compute time using throughput interval wall time at print.

                // Define completed-batch semantics: increment before logging
                ctx.global_step += 1;
                // Periodic throughput report
                if last_report.elapsed().as_secs_f32() >= config.throughput_interval_sec
                    && batches_since > 0
                {
                    let secs = last_report.elapsed().as_secs_f32().max(MIN_ELAPSED_TIME as f32);
                    let sps = samples_since as f32 / secs;
                    let bps = batches_since as f32 / secs;
                    let avg_bs = samples_since as f32 / batches_since as f32;
                    // compute_ns_since is not directly tracked; approximate as (secs - wait) * 1e9
                    let wait_secs = (wait_ns_since as f64) / NANOSECONDS_PER_SECOND;
                    let loader_ratio = if secs > 0.0 {
                        (wait_secs / secs as f64) * PERCENTAGE_DIVISOR as f64
                    } else {
                        0.0
                    };
                    if ctx.structured.as_ref().map(|lg| lg.to_stdout).unwrap_or(false) {
                        eprintln!(
                        "[throughput] mode=inmem loader=async epoch={} batches={} sps={:.0} bps={:.2} avg_batch={:.1} loader_ratio={:.1}%",
                        epoch + 1,
                        batch_count,
                        sps,
                        bps,
                        avg_bs,
                        loader_ratio
                        );
                    } else {
                        println!(
                        "[throughput] mode=inmem loader=async epoch={} batches={} sps={:.0} bps={:.2} avg_batch={:.1} loader_ratio={:.1}%",
                        epoch + 1,
                        batch_count,
                        sps,
                        bps,
                        avg_bs,
                        loader_ratio
                        );
                    }
                    if let Some(ref lg) = ctx.structured {
                        let mut rec = serde_json::json!({
                            "ts": chrono::Utc::now().to_rfc3339(),
                            "phase": "train",
                            "global_step": ctx.global_step as i64,
                            "epoch": (epoch + 1) as i64,
                            "lr": lr_base as f64,
                            "examples_sec": sps as f64,
                            "loader_ratio": (loader_ratio )/100.0,
                            "wall_time": secs as f64,
                        });
                        if let Some(ls) = last_loss_for_log {
                            rec.as_object_mut()
                                .unwrap()
                                .insert("train_loss".into(), serde_json::json!(ls as f64));
                        }
                        lg.write_json(&rec);
                    }
                    last_report = Instant::now();
                    samples_since = 0;
                    batches_since = 0;
                    wait_ns_since = 0;
                }

                // Save checkpoint if requested
                if let Some(interval) = save_every {
                    if total_batches % interval == 0 {
                        let checkpoint_path =
                            out_dir.join(format!("checkpoint_batch_{total_batches}.fp32.bin"));
                        save_single_network(network, &checkpoint_path)?;
                        println!("Saved checkpoint: {}", checkpoint_path.display());
                    }
                }
            }

            let avg_loss = if total_weight > 0.0 {
                total_loss / total_weight
            } else {
                0.0
            };
            let mut val_loss = None;
            let mut val_auc: Option<f64> = None;
            let mut val_ece: Option<f64> = None;
            let mut val_wsum: Option<f64> = None;
            if let Some(val_samples) = validation_samples {
                let vl = compute_validation_loss_single(network, val_samples, config);
                val_loss = Some(vl);
                val_auc = compute_val_auc_single(network, val_samples, config);
                if dash.val_is_jsonl && config.label_type == "wdl" {
                    let mut cps = Vec::new();
                    let mut probs = Vec::new();
                    let mut labels = Vec::new();
                    let mut wts = Vec::new();
                    let mut scratch_val = SingleForwardScratch::new(network.acc_dim);
                    for s in val_samples.iter() {
                        if let Some(cp) = s.cp {
                            let out = network.forward_with_scratch(&s.features, &mut scratch_val);
                            let p = 1.0 / (1.0 + (-out).exp());
                            cps.push(cp);
                            probs.push(p);
                            labels.push(s.label);
                            wts.push(s.weight);
                        }
                    }
                    if !cps.is_empty() {
                        let bins = calibration_bins(
                            &cps,
                            &probs,
                            &labels,
                            &wts,
                            config.cp_clip,
                            dash.calib_bins_n,
                        );
                        val_ece = ece_from_bins(&bins);
                        if dash.emit {
                            let mut w = csv::Writer::from_path(
                                out_dir.join(format!("calibration_epoch_{}.csv", epoch + 1)),
                            )?;
                            w.write_record([
                                "bin_left",
                                "bin_right",
                                "bin_center",
                                "count",
                                "weighted_count",
                                "mean_pred",
                                "mean_label",
                            ])?;
                            for b in &bins {
                                w.write_record([
                                    b.left.to_string(),
                                    b.right.to_string(),
                                    format!("{:.1}", b.center),
                                    b.count.to_string(),
                                    format!("{:.3}", b.weighted_count),
                                    format!("{:.6}", b.mean_pred),
                                    format!("{:.6}", b.mean_label),
                                ])?;
                            }
                            w.flush()?;
                            if dash.do_plots {
                                let points: Vec<(i32, i32, f32, f64, f64)> = bins
                                    .iter()
                                    .map(|b| (b.left, b.right, b.center, b.mean_pred, b.mean_label))
                                    .collect();
                                if let Err(e) = tools::plot::plot_calibration_png(
                                    out_dir.join(format!("calibration_epoch_{}.png", epoch + 1)),
                                    &points,
                                ) {
                                    eprintln!("plot_calibration_png failed: {}", e);
                                }
                            }
                        }
                    }
                }
                // Phase metrics
                if ctx.dash.val_is_jsonl && ctx.dash.emit {
                    let mut probs_buckets: [(Vec<f32>, Vec<f32>, Vec<f32>); 3] = Default::default();
                    let mut cp_buckets: [(Vec<f32>, Vec<f32>, Vec<f32>); 3] = Default::default();
                    #[inline]
                    fn idx_of(phase: GamePhase) -> usize {
                        match phase {
                            GamePhase::Opening => 0,
                            GamePhase::MiddleGame => 1,
                            GamePhase::EndGame => 2,
                        }
                    }
                    match config.label_type.as_str() {
                        "wdl" => {
                            let mut scratch_phase = SingleForwardScratch::new(network.acc_dim);
                            for s in val_samples.iter() {
                                if let Some(ph) = s.phase {
                                    let out = network.forward_with_scratch(&s.features, &mut scratch_phase);
                                    let p = 1.0 / (1.0 + (-out).exp());
                                    let b = &mut probs_buckets[idx_of(ph)];
                                    b.0.push(p);
                                    b.1.push(s.label);
                                    b.2.push(s.weight);
                                }
                            }
                        }
                        "cp" => {
                            let mut scratch_phase = SingleForwardScratch::new(network.acc_dim);
                            for s in val_samples.iter() {
                                if let Some(ph) = s.phase {
                                    let out = network.forward_with_scratch(&s.features, &mut scratch_phase);
                                    let b = &mut cp_buckets[idx_of(ph)];
                                    b.0.push(out);
                                    b.1.push(s.label);
                                    b.2.push(s.weight);
                                }
                            }
                        }
                        _ => {}
                    }
                    let mut wpm = csv::WriterBuilder::new().has_headers(false).from_writer(
                        std::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(ctx.out_dir.join("phase_metrics.csv"))?,
                    );
                    let phases = [
                        GamePhase::Opening,
                        GamePhase::MiddleGame,
                        GamePhase::EndGame,
                    ];
                    for (i, ph) in phases.iter().enumerate() {
                        match config.label_type.as_str() {
                            "wdl" => {
                                let (ref probs, ref labels, ref wts) = probs_buckets[i];
                                if !probs.is_empty() {
                                    let cnt = probs.len();
                                    let wsum: f64 = wts.iter().map(|&x| x as f64).sum();
                                    if let Some(m) = binary_metrics(probs, labels, wts) {
                                        wpm.write_record([
                                            (epoch + 1).to_string(),
                                            format!("{:?}", ph),
                                            cnt.to_string(),
                                            format!("{:.3}", wsum),
                                            format!("{:.6}", m.logloss),
                                            format!("{:.6}", m.brier),
                                            format!("{:.6}", m.accuracy),
                                            String::new(),
                                            String::new(),
                                        ])?;
                                    }
                                }
                            }
                            "cp" => {
                                let (ref preds, ref labels, ref wts) = cp_buckets[i];
                                if !preds.is_empty() {
                                    let cnt = preds.len();
                                    let wsum: f64 = wts.iter().map(|&x| x as f64).sum();
                                    if let Some(r) = regression_metrics(preds, labels, wts) {
                                        wpm.write_record([
                                            (epoch + 1).to_string(),
                                            format!("{:?}", ph),
                                            cnt.to_string(),
                                            format!("{:.3}", wsum),
                                            String::new(),
                                            String::new(),
                                            String::new(),
                                            format!("{:.6}", r.mae),
                                            format!("{:.6}", r.mse),
                                        ])?;
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    wpm.flush()?;
                }
                val_wsum = Some(val_samples.iter().map(|s| s.weight as f64).sum());
            }

            let epoch_secs = epoch_start.elapsed().as_secs_f32().max(MIN_ELAPSED_TIME as f32);
            let loader_ratio_epoch = if epoch_secs > 0.0 {
                let wait_secs = (wait_ns_epoch as f64) / NANOSECONDS_PER_SECOND;
                ((wait_secs / epoch_secs as f64) * PERCENTAGE_DIVISOR as f64) as f32
            } else {
                0.0
            };
            let epoch_sps = (train_samples_arc.len() as f32) / epoch_secs;
            if let Some(vl) = val_loss {
                if vl < *best_val_loss {
                    *best_val_loss = vl;
                    *best_network = Some(Network::Single(network.clone()));
                    *best_epoch = Some(epoch + 1);
                }
                *last_val_loss = Some(vl);
            }
            if dash.emit {
                let mut w = csv::WriterBuilder::new().has_headers(false).from_writer(
                    std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(out_dir.join("metrics.csv"))?,
                );
                w.write_record([
                    (epoch + 1).to_string(),
                    format!("{:.6}", avg_loss),
                    val_loss.map(|v| format!("{:.6}", v)).unwrap_or_else(|| "".into()),
                    val_auc.map(|v| format!("{:.6}", v)).unwrap_or_else(|| "".into()),
                    val_ece.map(|v| format!("{:.6}", v)).unwrap_or_else(|| "".into()),
                    format!("{:.3}", epoch_secs),
                    format!("{:.3}", total_weight),
                    val_wsum.map(|v| format!("{:.3}", v)).unwrap_or_else(|| "".into()),
                    if Some(epoch + 1) == *best_epoch {
                        "1".into()
                    } else {
                        "0".into()
                    },
                ])?;
                w.flush()?;
            }
            // Structured per-epoch logs (train/val)
            if let Some(ref lg) = ctx.structured {
                let mut rec_train = serde_json::json!({
                    "ts": chrono::Utc::now().to_rfc3339(),
                    "phase": "train",
                    "global_step": ctx.global_step as i64,
                    "epoch": (epoch + 1) as i64,
                    "lr": last_lr_base as f64,
                    "train_loss": avg_loss as f64,
                    "examples_sec": epoch_sps as f64,
                    "loader_ratio": (loader_ratio_epoch as f64)/100.0,
                    "wall_time": epoch_secs as f64,
                });
                if let Some(obj) = ctx.training_config_json.clone() {
                    rec_train.as_object_mut().unwrap().insert("training_config".into(), obj);
                }
                lg.write_json(&rec_train);
                let mut rec_val = serde_json::json!({
                    "ts": chrono::Utc::now().to_rfc3339(),
                    "phase": "val",
                    "global_step": ctx.global_step as i64,
                    "epoch": (epoch + 1) as i64,
                    "wall_time": epoch_secs as f64,
                });
                if let Some(obj) = ctx.training_config_json.clone() {
                    rec_val.as_object_mut().unwrap().insert("training_config".into(), obj);
                }
                if let Some(vl) = val_loss {
                    rec_val
                        .as_object_mut()
                        .unwrap()
                        .insert("val_loss".to_string(), serde_json::json!(vl as f64));
                }
                if let Some(a) = val_auc {
                    rec_val
                        .as_object_mut()
                        .unwrap()
                        .insert("val_auc".to_string(), serde_json::json!(a));
                }
                if let Some(e) = val_ece {
                    rec_val
                        .as_object_mut()
                        .unwrap()
                        .insert("val_ece".to_string(), serde_json::json!(e));
                }
                lg.write_json(&rec_val);
            }
            if ctx.structured.as_ref().map(|lg| lg.to_stdout).unwrap_or(false) {
                eprintln!(
                    "Epoch {}/{}: train_loss={:.4} val_loss={} batches={} time={:.2}s sps={:.0} loader_ratio={:.1}%",
                    epoch + 1, config.epochs, avg_loss, val_loss.map(|v| format!("{:.4}", v)).unwrap_or_else(|| "NA".into()), batch_count, epoch_secs, epoch_sps, loader_ratio_epoch
                );
            } else {
                println!(
                    "Epoch {}/{}: train_loss={:.4} val_loss={} batches={} time={:.2}s sps={:.0} loader_ratio={:.1}%",
                    epoch + 1, config.epochs, avg_loss, val_loss.map(|v| format!("{:.4}", v)).unwrap_or_else(|| "NA".into()), batch_count, epoch_secs, epoch_sps, loader_ratio_epoch
                );
            }
            print_zero_weight_debug(epoch, zero_weight_batches, &ctx.structured);
        }
        // Ensure worker fully finished at end
        async_loader.finish();
    } else {
        // Original synchronous loader path (still with throughput reporting)
        let mut batch_loader =
            BatchLoader::new(train_samples_arc.len(), config.batch_size, config.shuffle, rng);

        for epoch in 0..config.epochs {
            let epoch_start = Instant::now();
            batch_loader.reset(config.shuffle, rng);

            let mut total_loss = 0.0;
            let mut total_weight = 0.0;
            let mut batch_count = 0usize;

            let mut last_report = Instant::now();
            let mut samples_since = 0usize;
            let mut batches_since = 0usize;
            let mut last_lr_base = config.learning_rate;
            let mut last_loss_for_log: Option<f32> = None;
            let mut zero_weight_batches: usize = 0;

            while let Some(indices) = {
                let t0 = Instant::now();
                let next = batch_loader.next_batch();
                // We treat the time spent fetching indices as loader time in sync path
                let _wait = t0.elapsed();
                if let Some(ref _idxs) = next {
                    // Accumulate local variables by capturing outer mutable state via closures is cumbersome here.
                    // We will measure throughput window at print time similar to async path.
                }
                next
            } {
                // consume last_loss_for_log to avoid unused-assignment lint across iterations
                let _ = last_loss_for_log;
                // LR scheduling
                let lr_base = lr_base_for(epoch, ctx.global_step, config, ctx.plateau.as_ref());
                // 先にバッチ重みを集計し、ゼロなら計算自体をスキップ
                let batch_weight: f32 =
                    indices.iter().map(|&idx| train_samples_arc[idx].weight).sum();
                if batch_weight > 0.0 {
                    let loss = train_batch_by_indices(
                        network,
                        &train_samples_arc,
                        &indices,
                        config,
                        &mut adam_state,
                        lr_base,
                    );
                    total_loss += loss * batch_weight;
                    total_weight += batch_weight;
                    last_loss_for_log = Some(loss);
                } else {
                    zero_weight_batches += 1;
                    last_loss_for_log = None;
                }

                let batch_len = indices.len();
                batch_count += 1;
                total_batches += 1;
                samples_since += batch_len;
                batches_since += 1;
                last_lr_base = lr_base;

                ctx.global_step += 1;
                if last_report.elapsed().as_secs_f32() >= config.throughput_interval_sec
                    && batches_since > 0
                {
                    let secs = last_report.elapsed().as_secs_f32().max(MIN_ELAPSED_TIME as f32);
                    let sps = samples_since as f32 / secs;
                    let bps = batches_since as f32 / secs;
                    let avg_bs = samples_since as f32 / batches_since as f32;
                    if ctx.structured.as_ref().map(|lg| lg.to_stdout).unwrap_or(false) {
                        eprintln!(
                    "[throughput] mode=inmem loader=sync epoch={} batches={} sps={:.0} bps={:.2} avg_batch={:.1} loader_ratio=~0.0%",
                    epoch + 1,
                    batch_count,
                    sps,
                    bps,
                    avg_bs
                );
                    } else {
                        println!(
                    "[throughput] mode=inmem loader=sync epoch={} batches={} sps={:.0} bps={:.2} avg_batch={:.1} loader_ratio=~0.0%",
                    epoch + 1,
                    batch_count,
                    sps,
                    bps,
                    avg_bs
                );
                    }
                    if let Some(ref lg) = ctx.structured {
                        let mut rec = serde_json::json!({
                            "ts": chrono::Utc::now().to_rfc3339(),
                            "phase": "train",
                            "global_step": ctx.global_step as i64,
                            "epoch": (epoch + 1) as i64,
                            "lr": lr_base as f64,
                            "examples_sec": sps as f64,
                            "loader_ratio": 0.0f64,
                            "wall_time": secs as f64,
                        });
                        if let Some(ls) = last_loss_for_log {
                            rec.as_object_mut()
                                .unwrap()
                                .insert("train_loss".into(), serde_json::json!(ls as f64));
                        }
                        lg.write_json(&rec);
                    }
                    last_report = Instant::now();
                    samples_since = 0;
                    batches_since = 0;
                }

                // Save checkpoint if requested
                if let Some(interval) = save_every {
                    if total_batches % interval == 0 {
                        let checkpoint_path =
                            out_dir.join(format!("checkpoint_batch_{total_batches}.fp32.bin"));
                        save_single_network(network, &checkpoint_path)?;
                        if ctx.structured.as_ref().map(|lg| lg.to_stdout).unwrap_or(false) {
                            eprintln!("Saved checkpoint: {}", checkpoint_path.display());
                        } else {
                            println!("Saved checkpoint: {}", checkpoint_path.display());
                        }
                    }
                }
                // record last lr used (unused in sync loader path)
            }

            let avg_loss = if total_weight > 0.0 {
                total_loss / total_weight
            } else {
                0.0
            };
            let mut val_loss = None;
            let mut val_auc: Option<f64> = None;
            let mut val_ece: Option<f64> = None;
            let mut val_wsum: Option<f64> = None;
            if let Some(val_samples) = validation_samples {
                let vl = compute_validation_loss_single(network, val_samples, config);
                val_loss = Some(vl);
                val_auc = compute_val_auc_single(network, val_samples, config);
                if dash.val_is_jsonl && config.label_type == "wdl" {
                    let mut cps = Vec::new();
                    let mut probs = Vec::new();
                    let mut labels = Vec::new();
                    let mut wts = Vec::new();
                    let mut scratch_val = SingleForwardScratch::new(network.acc_dim);
                    for s in val_samples.iter() {
                        if let Some(cp) = s.cp {
                            let out = network.forward_with_scratch(&s.features, &mut scratch_val);
                            let p = 1.0 / (1.0 + (-out).exp());
                            cps.push(cp);
                            probs.push(p);
                            labels.push(s.label);
                            wts.push(s.weight);
                        }
                    }
                    if !cps.is_empty() {
                        let bins = calibration_bins(
                            &cps,
                            &probs,
                            &labels,
                            &wts,
                            config.cp_clip,
                            dash.calib_bins_n,
                        );
                        val_ece = ece_from_bins(&bins);
                        if dash.emit {
                            let mut w = csv::Writer::from_path(
                                out_dir.join(format!("calibration_epoch_{}.csv", epoch + 1)),
                            )?;
                            w.write_record([
                                "bin_left",
                                "bin_right",
                                "bin_center",
                                "count",
                                "weighted_count",
                                "mean_pred",
                                "mean_label",
                            ])?;
                            for b in &bins {
                                w.write_record([
                                    b.left.to_string(),
                                    b.right.to_string(),
                                    format!("{:.1}", b.center),
                                    b.count.to_string(),
                                    format!("{:.3}", b.weighted_count),
                                    format!("{:.6}", b.mean_pred),
                                    format!("{:.6}", b.mean_label),
                                ])?;
                            }
                            w.flush()?;
                            if dash.do_plots {
                                let points: Vec<(i32, i32, f32, f64, f64)> = bins
                                    .iter()
                                    .map(|b| (b.left, b.right, b.center, b.mean_pred, b.mean_label))
                                    .collect();
                                if let Err(e) = tools::plot::plot_calibration_png(
                                    out_dir.join(format!("calibration_epoch_{}.png", epoch + 1)),
                                    &points,
                                ) {
                                    eprintln!("plot_calibration_png failed: {}", e);
                                }
                            }
                        }
                    }
                }
                // Phase metrics
                if dash.val_is_jsonl && dash.emit {
                    let mut probs_buckets: [(Vec<f32>, Vec<f32>, Vec<f32>); 3] = Default::default();
                    let mut cp_buckets: [(Vec<f32>, Vec<f32>, Vec<f32>); 3] = Default::default();
                    #[inline]
                    fn idx_of(phase: GamePhase) -> usize {
                        match phase {
                            GamePhase::Opening => 0,
                            GamePhase::MiddleGame => 1,
                            GamePhase::EndGame => 2,
                        }
                    }
                    match config.label_type.as_str() {
                        "wdl" => {
                            let mut scratch_phase = SingleForwardScratch::new(network.acc_dim);
                            for s in val_samples.iter() {
                                if let Some(ph) = s.phase {
                                    let out = network.forward_with_scratch(&s.features, &mut scratch_phase);
                                    let p = 1.0 / (1.0 + (-out).exp());
                                    let b = &mut probs_buckets[idx_of(ph)];
                                    b.0.push(p);
                                    b.1.push(s.label);
                                    b.2.push(s.weight);
                                }
                            }
                        }
                        "cp" => {
                            let mut scratch_phase = SingleForwardScratch::new(network.acc_dim);
                            for s in val_samples.iter() {
                                if let Some(ph) = s.phase {
                                    let out = network.forward_with_scratch(&s.features, &mut scratch_phase);
                                    let b = &mut cp_buckets[idx_of(ph)];
                                    b.0.push(out);
                                    b.1.push(s.label);
                                    b.2.push(s.weight);
                                }
                            }
                        }
                        _ => {}
                    }
                    let mut wpm = csv::WriterBuilder::new().has_headers(false).from_writer(
                        std::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(out_dir.join("phase_metrics.csv"))?,
                    );
                    let phases = [
                        GamePhase::Opening,
                        GamePhase::MiddleGame,
                        GamePhase::EndGame,
                    ];
                    for (i, ph) in phases.iter().enumerate() {
                        match config.label_type.as_str() {
                            "wdl" => {
                                let (ref probs, ref labels, ref wts) = probs_buckets[i];
                                if !probs.is_empty() {
                                    let cnt = probs.len();
                                    let wsum: f64 = wts.iter().map(|&x| x as f64).sum();
                                    if let Some(m) = binary_metrics(probs, labels, wts) {
                                        wpm.write_record([
                                            (epoch + 1).to_string(),
                                            format!("{:?}", ph),
                                            cnt.to_string(),
                                            format!("{:.3}", wsum),
                                            format!("{:.6}", m.logloss),
                                            format!("{:.6}", m.brier),
                                            format!("{:.6}", m.accuracy),
                                            String::new(),
                                            String::new(),
                                        ])?;
                                    }
                                }
                            }
                            "cp" => {
                                let (ref preds, ref labels, ref wts) = cp_buckets[i];
                                if !preds.is_empty() {
                                    let cnt = preds.len();
                                    let wsum: f64 = wts.iter().map(|&x| x as f64).sum();
                                    if let Some(r) = regression_metrics(preds, labels, wts) {
                                        wpm.write_record([
                                            (epoch + 1).to_string(),
                                            format!("{:?}", ph),
                                            cnt.to_string(),
                                            format!("{:.3}", wsum),
                                            String::new(),
                                            String::new(),
                                            String::new(),
                                            format!("{:.6}", r.mae),
                                            format!("{:.6}", r.mse),
                                        ])?;
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    wpm.flush()?;
                }
                val_wsum = Some(val_samples.iter().map(|s| s.weight as f64).sum());
            }

            let epoch_secs = epoch_start.elapsed().as_secs_f32().max(MIN_ELAPSED_TIME as f32);
            let epoch_sps = (train_samples_arc.len() as f32) / epoch_secs;
            if let Some(vl) = val_loss {
                if vl < *best_val_loss {
                    *best_val_loss = vl;
                    *best_network = Some(Network::Single(network.clone()));
                    *best_epoch = Some(epoch + 1);
                }
                *last_val_loss = Some(vl);
            }
            if dash.emit {
                let mut w = csv::WriterBuilder::new().has_headers(false).from_writer(
                    std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(out_dir.join("metrics.csv"))?,
                );
                w.write_record([
                    (epoch + 1).to_string(),
                    format!("{:.6}", avg_loss),
                    val_loss.map(|v| format!("{:.6}", v)).unwrap_or_else(|| "".into()),
                    val_auc.map(|v| format!("{:.6}", v)).unwrap_or_else(|| "".into()),
                    val_ece.map(|v| format!("{:.6}", v)).unwrap_or_else(|| "".into()),
                    format!("{:.3}", epoch_secs),
                    format!("{:.3}", total_weight),
                    val_wsum.map(|v| format!("{:.3}", v)).unwrap_or_else(|| "".into()),
                    if Some(epoch + 1) == *best_epoch {
                        "1".into()
                    } else {
                        "0".into()
                    },
                ])?;
                w.flush()?;
            }
            // Structured per-epoch logs (train/val) for sync in-mem loader
            if let Some(ref lg) = ctx.structured {
                let mut rec_train = serde_json::json!({
                    "ts": chrono::Utc::now().to_rfc3339(),
                    "phase": "train",
                    "global_step": ctx.global_step as i64,
                    "epoch": (epoch + 1) as i64,
                    "lr": last_lr_base as f64,
                    "train_loss": avg_loss as f64,
                    "examples_sec": epoch_sps as f64,
                    "loader_ratio": 0.0f64,
                    "wall_time": epoch_secs as f64,
                });
                if let Some(obj) = ctx.training_config_json.clone() {
                    rec_train.as_object_mut().unwrap().insert("training_config".into(), obj);
                }
                lg.write_json(&rec_train);
                let mut rec_val = serde_json::json!({
                    "ts": chrono::Utc::now().to_rfc3339(),
                    "phase": "val",
                    "global_step": ctx.global_step as i64,
                    "epoch": (epoch + 1) as i64,
                    "wall_time": epoch_secs as f64,
                });
                if let Some(obj) = ctx.training_config_json.clone() {
                    rec_val.as_object_mut().unwrap().insert("training_config".into(), obj);
                }
                if let Some(vl) = val_loss {
                    rec_val
                        .as_object_mut()
                        .unwrap()
                        .insert("val_loss".to_string(), serde_json::json!(vl as f64));
                }
                if let Some(a) = val_auc {
                    rec_val
                        .as_object_mut()
                        .unwrap()
                        .insert("val_auc".to_string(), serde_json::json!(a));
                }
                if let Some(e) = val_ece {
                    rec_val
                        .as_object_mut()
                        .unwrap()
                        .insert("val_ece".to_string(), serde_json::json!(e));
                }
                lg.write_json(&rec_val);
            }
            if ctx.structured.as_ref().map(|lg| lg.to_stdout).unwrap_or(false) {
                eprintln!(
                "Epoch {}/{}: train_loss={:.4} val_loss={} batches={} time={:.2}s sps={:.0} loader_ratio=~0.0%",
                epoch + 1, config.epochs, avg_loss, val_loss.map(|v| format!("{:.4}", v)).unwrap_or_else(|| "NA".into()), batch_count, epoch_secs, epoch_sps
                );
            } else {
                println!(
                "Epoch {}/{}: train_loss={:.4} val_loss={} batches={} time={:.2}s sps={:.0} loader_ratio=~0.0%",
                epoch + 1, config.epochs, avg_loss, val_loss.map(|v| format!("{:.4}", v)).unwrap_or_else(|| "NA".into()), batch_count, epoch_secs, epoch_sps
                );
            }
            print_zero_weight_debug(epoch, zero_weight_batches, &ctx.structured);
        }
    }

    Ok(())
}

fn train_batch_by_indices(
    network: &mut SingleNetwork,
    samples: &[Sample],
    indices: &[usize],
    config: &Config,
    adam_state: &mut Option<SingleAdamState>,
    lr_base: f32,
) -> f32 {
    let mut total_loss = 0.0;
    let mut total_weight = 0.0;

    // Gradients for output layer (accumulated over batch)
    let mut grad_w2 = vec![0.0f32; network.w2.len()];
    let mut grad_b2 = 0.0f32;

    // Reusable forward scratch buffers
    let mut forward_scratch = SingleForwardScratch::new(network.acc_dim);

    // Pre-compute learning rate for Adam
    let lr_t = if let Some(adam) = adam_state.as_mut() {
        adam.t += 1;
        let t = adam.t as f32;
        lr_base * (1.0 - adam.beta2.powf(t)).sqrt() / (1.0 - adam.beta1.powf(t))
    } else {
        lr_base
    };

    for &idx in indices {
        let sample = &samples[idx];
        if sample.weight == 0.0 {
            continue; // skip updates entirely for zero-weight samples
        }
        // Forward pass with reused buffers
        let output = network.forward_with_scratch(&sample.features, &mut forward_scratch);

        // Compute loss and output gradient using numerically stable BCE
        let (loss, grad_output) = match config.label_type.as_str() {
            "wdl" => {
                let (l, g) = bce_with_logits(output, sample.label);
                (l * sample.weight, g * sample.weight)
            }
            "cp" => {
                let error = output - sample.label;
                (0.5 * error * error * sample.weight, error * sample.weight)
            }
            _ => unreachable!(),
        };

        total_loss += loss;
        total_weight += sample.weight;

        // Accumulate output layer gradients
        grad_b2 += grad_output;
        let activations = forward_scratch.activations();
        for (i, &act) in activations.iter().enumerate() {
            grad_w2[i] += grad_output * act;
        }

        // Immediate update for input layer (row-sparse)
        if let Some(adam) = adam_state.as_mut() {
            // Adam updates
            for (i, &act_val) in activations.iter().enumerate() {
                // Check if neuron is in linear region (ReLU derivative)
                if act_val <= 0.0 || act_val >= network.relu_clip {
                    continue;
                }

                let grad_act = grad_output * network.w2[i];

                // Update bias b0[i]
                let grad_b = grad_act;
                adam.m_b0[i] = adam.beta1 * adam.m_b0[i] + (1.0 - adam.beta1) * grad_b;
                adam.v_b0[i] = adam.beta2 * adam.v_b0[i] + (1.0 - adam.beta2) * grad_b * grad_b;
                network.b0[i] -= lr_t * adam.m_b0[i] / (adam.v_b0[i].sqrt() + adam.epsilon);

                // Update weights w0 for active features only
                for &feat_idx in &sample.features {
                    let idx = feat_idx as usize * network.acc_dim + i;
                    let grad_w = grad_act + config.l2_reg * network.w0[idx];

                    adam.m_w0[idx] = adam.beta1 * adam.m_w0[idx] + (1.0 - adam.beta1) * grad_w;
                    adam.v_w0[idx] =
                        adam.beta2 * adam.v_w0[idx] + (1.0 - adam.beta2) * grad_w * grad_w;
                    network.w0[idx] -=
                        lr_t * adam.m_w0[idx] / (adam.v_w0[idx].sqrt() + adam.epsilon);
                }
            }
        } else {
            // SGD updates
            for (i, &act_val) in activations.iter().enumerate() {
                // Check if neuron is in linear region
                if act_val <= 0.0 || act_val >= network.relu_clip {
                    continue;
                }

                let grad_act = grad_output * network.w2[i];

                // Update bias
                network.b0[i] -= lr_t * grad_act;

                // Update weights for active features
                for &feat_idx in &sample.features {
                    let idx = feat_idx as usize * network.acc_dim + i;
                    let grad_w = grad_act + config.l2_reg * network.w0[idx];
                    network.w0[idx] -= lr_t * grad_w;
                }
            }
        }
    }

    // Update output layer (weighted average + L2 reg)
    // Note: L2 is applied online (per-feature) for w0 in the sparse inner loop,
    // while w2 applies L2 to the batch-averaged gradient. This asymmetry is
    // intentional for performance (row-sparse updates on w0), and is documented
    // to aid reproducibility when comparing training dynamics.
    let sum_w = total_weight.max(1e-8);
    let inv_sum_w = 1.0 / sum_w;

    if let Some(adam) = adam_state.as_mut() {
        // Update w2
        for (i, grad_sum) in grad_w2.iter().enumerate() {
            let grad = grad_sum * inv_sum_w + config.l2_reg * network.w2[i];
            adam.m_w2[i] = adam.beta1 * adam.m_w2[i] + (1.0 - adam.beta1) * grad;
            adam.v_w2[i] = adam.beta2 * adam.v_w2[i] + (1.0 - adam.beta2) * grad * grad;
            network.w2[i] -= lr_t * adam.m_w2[i] / (adam.v_w2[i].sqrt() + adam.epsilon);
        }

        // Update b2
        let grad_b2_avg = grad_b2 * inv_sum_w;
        adam.m_b2 = adam.beta1 * adam.m_b2 + (1.0 - adam.beta1) * grad_b2_avg;
        adam.v_b2 = adam.beta2 * adam.v_b2 + (1.0 - adam.beta2) * grad_b2_avg * grad_b2_avg;
        network.b2 -= lr_t * adam.m_b2 / (adam.v_b2.sqrt() + adam.epsilon);
    } else {
        // SGD updates for output layer
        for (i, grad_sum) in grad_w2.iter().enumerate() {
            let grad = grad_sum * inv_sum_w + config.l2_reg * network.w2[i];
            network.w2[i] -= lr_t * grad;
        }
        network.b2 -= lr_t * grad_b2 * inv_sum_w;
    }

    if total_weight > 0.0 {
        total_loss / total_weight
    } else {
        0.0
    }
}

// Numerically stable binary cross-entropy with logits
#[inline]
fn bce_with_logits(logit: f32, target: f32) -> (f32, f32) {
    let max_val = 0.0f32.max(logit);
    let loss = max_val - logit * target + ((-logit.abs()).exp() + 1.0).ln();
    let grad = 1.0 / (1.0 + (-logit).exp()) - target;
    (loss, grad)
}

pub(crate) fn compute_validation_loss_single(
    network: &SingleNetwork,
    samples: &[Sample],
    config: &Config,
) -> f32 {
    let mut total_loss = 0.0;
    let mut total_weight = 0.0;

    let mut scratch = SingleForwardScratch::new(network.acc_dim);

    for sample in samples {
        let output = network.forward_with_scratch(&sample.features, &mut scratch);

        let loss = match config.label_type.as_str() {
            "wdl" => {
                let (loss, _) = bce_with_logits(output, sample.label);
                loss
            }
            "cp" => {
                let error = output - sample.label;
                0.5 * error * error
            }
            _ => unreachable!(),
        };

        total_loss += loss * sample.weight;
        total_weight += sample.weight;
    }

    if total_weight > 0.0 {
        total_loss / total_weight
    } else {
        0.0
    }
}

pub(crate) fn compute_val_auc_single(
    network: &SingleNetwork,
    samples: &[Sample],
    config: &Config,
) -> Option<f64> {
    if config.label_type != "wdl" || samples.is_empty() {
        return None;
    }
    let mut probs: Vec<f32> = Vec::with_capacity(samples.len());
    let mut labels: Vec<f32> = Vec::with_capacity(samples.len());
    let mut weights: Vec<f32> = Vec::with_capacity(samples.len());

    let mut scratch = SingleForwardScratch::new(network.acc_dim);
    for s in samples {
        let out = network.forward_with_scratch(&s.features, &mut scratch);
        let p = 1.0 / (1.0 + (-out).exp());
        // Treat strict positives/negatives only; skip exact boundary (label==0.5)
        if s.label > 0.5 {
            probs.push(p);
            labels.push(1.0);
            weights.push(s.weight);
        } else if s.label < 0.5 {
            probs.push(p);
            labels.push(0.0);
            weights.push(s.weight);
        }
    }
    if probs.is_empty() {
        None
    } else {
        roc_auc_weighted(&probs, &labels, &weights)
    }
}

pub(crate) fn compute_val_auc_and_ece_single(
    network: &SingleNetwork,
    samples: &[Sample],
    config: &Config,
    dash_val: &impl DashboardValKind,
) -> (Option<f64>, Option<f64>) {
    let auc = compute_val_auc_single(network, samples, config);
    if config.label_type != "wdl" || !dash_val.is_jsonl() {
        return (auc, None);
    }
    // Build cp-binned calibration and compute ECE
    let mut cps: Vec<i32> = Vec::new();
    let mut probs: Vec<f32> = Vec::new();
    let mut labels: Vec<f32> = Vec::new();
    let mut wts: Vec<f32> = Vec::new();
    let mut scratch = SingleForwardScratch::new(network.acc_dim);
    for s in samples {
        if let Some(cp) = s.cp {
            let out = network.forward_with_scratch(&s.features, &mut scratch);
            let p = 1.0 / (1.0 + (-out).exp());
            cps.push(cp);
            probs.push(p);
            labels.push(s.label);
            wts.push(s.weight);
        }
    }
    if cps.is_empty() {
        return (auc, None);
    }
    let bins = calibration_bins(&cps, &probs, &labels, &wts, config.cp_clip, dash_val.calib_bins());
    let ece = ece_from_bins(&bins);
    (auc, ece)
}

pub(crate) fn compute_val_auc_classic(
    network: &ClassicNetwork,
    samples: &[Sample],
    config: &Config,
) -> Option<f64> {
    if config.label_type != "wdl" || samples.is_empty() {
        return None;
    }
    let mut probs: Vec<f32> = Vec::with_capacity(samples.len());
    let mut labels: Vec<f32> = Vec::with_capacity(samples.len());
    let mut weights: Vec<f32> = Vec::with_capacity(samples.len());

    let mut scratch = ClassicForwardScratch::new(
        network.fp32.acc_dim,
        network.fp32.h1_dim,
        network.fp32.h2_dim,
    );

    for s in samples {
        let out = network.forward_with_scratch(&s.features, &mut scratch);
        let p = 1.0 / (1.0 + (-out).exp());
        if s.label > 0.5 {
            probs.push(p);
            labels.push(1.0);
            weights.push(s.weight);
        } else if s.label < 0.5 {
            probs.push(p);
            labels.push(0.0);
            weights.push(s.weight);
        }
    }
    if probs.is_empty() {
        None
    } else {
        roc_auc_weighted(&probs, &labels, &weights)
    }
}

pub(crate) fn compute_val_auc_and_ece_classic(
    network: &ClassicNetwork,
    samples: &[Sample],
    config: &Config,
    dash_val: &impl DashboardValKind,
) -> (Option<f64>, Option<f64>) {
    let auc = compute_val_auc_classic(network, samples, config);
    if config.label_type != "wdl" || !dash_val.is_jsonl() {
        return (auc, None);
    }

    let mut cps: Vec<i32> = Vec::new();
    let mut probs: Vec<f32> = Vec::new();
    let mut labels: Vec<f32> = Vec::new();
    let mut wts: Vec<f32> = Vec::new();

    let mut scratch = ClassicForwardScratch::new(
        network.fp32.acc_dim,
        network.fp32.h1_dim,
        network.fp32.h2_dim,
    );

    for s in samples {
        if let Some(cp) = s.cp {
            let out = network.forward_with_scratch(&s.features, &mut scratch);
            let p = 1.0 / (1.0 + (-out).exp());
            cps.push(cp);
            probs.push(p);
            labels.push(s.label);
            wts.push(s.weight);
        }
    }

    if cps.is_empty() {
        return (auc, None);
    }

    let bins = calibration_bins(&cps, &probs, &labels, &wts, config.cp_clip, dash_val.calib_bins());
    let ece = ece_from_bins(&bins);
    (auc, ece)
}
