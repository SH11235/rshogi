pub fn train_model(
    network: &mut Network,
    train_samples: &mut [Sample],
    validation_samples: &Option<Vec<Sample>>,
    config: &Config,
    rng: &mut StdRng,
    ctx: &mut TrainContext,
) -> Result<(), Box<dyn std::error::Error>> {
    match network {
        Network::Single(inner) => {
            train_model_single(inner, train_samples, validation_samples, config, rng, ctx)
        }
        Network::Classic(inner) => {
            train_model_classic(inner, train_samples, validation_samples, config, rng, ctx)
        }
    }
}

fn train_model_single(
    network: &mut SingleNetwork,
    train_samples: &mut [Sample],
    validation_samples: &Option<Vec<Sample>>,
    config: &Config,
    rng: &mut StdRng,
    ctx: &mut TrainContext,
) -> Result<(), Box<dyn std::error::Error>> {
    let n_samples = train_samples.len();
    let n_batches = n_samples.div_ceil(config.batch_size);
    let mut adam_state = if config.optimizer.eq_ignore_ascii_case("adam")
        || config.optimizer.eq_ignore_ascii_case("adamw")
    {
        if config.optimizer.eq_ignore_ascii_case("adamw") {
            emit_single_adamw_warning();
        }
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
        emit_epoch_logging(
            ctx.structured.as_ref(),
            ctx.training_config_json.as_ref(),
            ctx.global_step,
            epoch,
            config.epochs,
            avg_loss,
            val_loss,
            val_auc,
            val_ece,
            epoch_secs,
            epoch_sps,
            None,
            None,
            last_lr_base,
        );
        print_zero_weight_debug(epoch, zero_weight_batches, &ctx.structured);
    }

    Ok(())
}

fn train_model_with_loader_classic(
    network: &mut ClassicNetwork,
    train_samples: Vec<Sample>,
    validation_samples: &Option<Vec<Sample>>,
    config: &Config,
    rng: &mut StdRng,
    ctx: &mut TrainContext,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut owned = train_samples;
    train_model_classic(
        network,
        owned.as_mut_slice(),
        validation_samples,
        config,
        rng,
        ctx,
    )
}

fn train_model_classic(
    network: &mut ClassicNetwork,
    train_samples: &mut [Sample],
    validation_samples: &Option<Vec<Sample>>,
    config: &Config,
    rng: &mut StdRng,
    ctx: &mut TrainContext,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut optimizer_state = ClassicOptimizerState::new(&network.fp32, config)
        .map_err(|msg| std::io::Error::new(std::io::ErrorKind::InvalidInput, msg))?;

    let n_samples = train_samples.len();
    let n_batches = n_samples.div_ceil(config.batch_size);

    let mut scratch = ClassicTrainScratch::new(
        network.fp32.acc_dim,
        network.fp32.h1_dim,
        network.fp32.h2_dim,
    );

    let mut total_batches = 0usize;

    for epoch in 0..config.epochs {
        let epoch_start = Instant::now();
        let mut last_lr_base = config.learning_rate;

        if config.shuffle {
            train_samples.shuffle(rng);
        }

        let mut total_loss = 0.0f32;
        let mut total_weight = 0.0f32;

        let mut last_report = Instant::now();
        let mut samples_since = 0usize;
        let mut batches_since = 0usize;
        let mut zero_weight_batches = 0usize;
        let mut last_loss_for_log: Option<f32> = None;

        for batch_idx in 0..n_batches {
            let _ = last_loss_for_log;
            let start = batch_idx * config.batch_size;
            let end = (start + config.batch_size).min(n_samples);
            let batch = &train_samples[start..end];
            if batch.is_empty() {
                continue;
            }

            let lr_base = lr_base_for(epoch, ctx.global_step, config, ctx.plateau.as_ref());

            let (loss_sum, weight_sum) = train_batch_classic(
                network,
                batch,
                config,
                &mut scratch,
                lr_base,
                &mut optimizer_state,
                config.grad_clip,
            );
            if weight_sum > 0.0 {
                total_loss += loss_sum;
                total_weight += weight_sum;
                last_loss_for_log = Some(loss_sum / weight_sum);
            } else {
                last_loss_for_log = None;
                zero_weight_batches += 1;
            }
            ctx.global_step += 1;
            samples_since += batch.len();
            batches_since += 1;
            total_batches += 1;
            last_lr_base = lr_base;

            if let Some(interval) = ctx.save_every {
                if total_batches % interval == 0 {
                    let checkpoint_path =
                        ctx.out_dir.join(format!("checkpoint_batch_{}.fp32.bin", total_batches));
                    save_classic_network(network, &checkpoint_path)?;
                    if ctx.structured.as_ref().map(|lg| lg.to_stdout).unwrap_or(false) {
                        eprintln!("Saved checkpoint: {}", checkpoint_path.display());
                    } else {
                        println!("Saved checkpoint: {}", checkpoint_path.display());
                    }
                }
            }

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
            let vl = compute_validation_loss_classic(network, val_samples, config);
            val_loss = Some(vl);
            let (auc, ece) =
                compute_val_auc_and_ece_classic(network, val_samples, config, &ctx.dash);
            val_auc = auc;
            val_ece = ece;
            val_wsum = Some(val_samples.iter().map(|s| s.weight as f64).sum());
        }

        if let Some(vl) = val_loss {
            if vl < *ctx.trackers.best_val_loss {
                *ctx.trackers.best_val_loss = vl;
                *ctx.trackers.best_network = Some(Network::Classic(network.clone()));
                *ctx.trackers.best_epoch = Some(epoch + 1);
            }
            *ctx.trackers.last_val_loss = Some(vl);
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

        let epoch_secs = epoch_start.elapsed().as_secs_f32().max(MIN_ELAPSED_TIME as f32);
        let epoch_sps = (n_samples as f32) / epoch_secs;

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

        emit_epoch_logging(
            ctx.structured.as_ref(),
            ctx.training_config_json.as_ref(),
            ctx.global_step,
            epoch,
            config.epochs,
            avg_loss,
            val_loss,
            val_auc,
            val_ece,
            epoch_secs,
            epoch_sps,
            None,
            None,
            last_lr_base,
        );

        print_zero_weight_debug(epoch, zero_weight_batches, &ctx.structured);
    }

    Ok(())
}

pub fn train_model_stream_cache(
    network: &mut Network,
    cache_path: &str,
    validation_samples: &Option<Vec<Sample>>,
    config: &Config,
    rng: &mut StdRng,
    ctx: &mut TrainContext,
    weighting: &wcfg::WeightingConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    match network {
        Network::Single(inner) => train_model_stream_cache_single(
            inner, cache_path, validation_samples, config, rng, ctx, weighting,
        ),
        Network::Classic(inner) => train_model_stream_cache_classic(
            inner, cache_path, validation_samples, config, rng, ctx, weighting,
        ),
    }
}

fn train_model_stream_cache_single(
    network: &mut SingleNetwork,
    cache_path: &str,
    validation_samples: &Option<Vec<Sample>>,
    config: &Config,
    rng: &mut StdRng,
    ctx: &mut TrainContext,
    weighting: &wcfg::WeightingConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let _ = rng;
    // Use ctx fields directly in this function to avoid borrow confusion
    // If prefetch=0, run synchronous streaming in the training thread (no background worker)
    if config.prefetch_batches == 0 {
        let mut adam_state = if config.optimizer.eq_ignore_ascii_case("adam")
            || config.optimizer.eq_ignore_ascii_case("adamw")
        {
            if config.optimizer.eq_ignore_ascii_case("adamw") {
                emit_single_adamw_warning();
            }
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
            let val_loss_opt = if has_val { Some(val_loss) } else { None };
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
            emit_epoch_logging(
                ctx.structured.as_ref(),
                ctx.training_config_json.as_ref(),
                ctx.global_step,
                epoch,
                config.epochs,
                avg_loss,
                val_loss_opt,
                val_auc,
                val_ece,
                epoch_secs,
                epoch_sps,
                Some(loader_ratio_epoch),
                Some(batch_count),
                last_lr_base,
            );

            // Emit metrics.csv (sync stream-cache path)
            if ctx.dash.emit {
                let mut w = csv::WriterBuilder::new().has_headers(false).from_writer(
                    std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(ctx.out_dir.join("metrics.csv"))?,
                );
                let val_loss_str = if has_val {
                    format!("{:.6}", val_loss_opt.unwrap())
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

    let mut total_batches = 0usize;
    let mut adam_state = if config.optimizer.eq_ignore_ascii_case("adam")
        || config.optimizer.eq_ignore_ascii_case("adamw")
    {
        if config.optimizer.eq_ignore_ascii_case("adamw") {
            emit_single_adamw_warning();
        }
        Some(SingleAdamState::new(network))
    } else {
        None
    };

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

fn train_model_stream_cache_classic(
    network: &mut ClassicNetwork,
    cache_path: &str,
    validation_samples: &Option<Vec<Sample>>,
    config: &Config,
    rng: &mut StdRng,
    ctx: &mut TrainContext,
    weighting: &wcfg::WeightingConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut optimizer_state = ClassicOptimizerState::new(&network.fp32, config)
        .map_err(|msg| std::io::Error::new(std::io::ErrorKind::InvalidInput, msg))?;

    if config.shuffle {
        log::warn!(
            "Classic stream-cache 学習では shuffle は無効です (--shuffle は無視されます)"
        );
    }

    let mut scratch = ClassicTrainScratch::new(
        network.fp32.acc_dim,
        network.fp32.h1_dim,
        network.fp32.h2_dim,
    );

    let _ = rng;

    let mut effective_prefetch = config.prefetch_batches.max(1);
    if let Some(bytes_cap) = config.prefetch_bytes.filter(|&b| b > 0) {
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

    let mut total_batches = 0usize;

    for epoch in 0..config.epochs {
        let epoch_start = Instant::now();
        loader.start_epoch()?;

        let mut total_loss = 0.0f32;
        let mut total_weight = 0.0f32;
        let mut batch_count = 0usize;
        let mut total_samples_epoch = 0usize;
        let mut zero_weight_batches = 0usize;

        let mut last_report = Instant::now();
        let mut samples_since = 0usize;
        let mut batches_since = 0usize;
        let mut wait_ns_since: u128 = 0;
        let mut wait_ns_epoch: u128 = 0;
        let mut last_loss_for_log: Option<f32> = None;
        let mut last_lr_base = config.learning_rate;

        loop {
            let (maybe_batch, wait_dur) = loader.next_batch_with_wait();
            let Some(batch_res) = maybe_batch else { break };
            let batch = match batch_res {
                Ok(b) => b,
                Err(msg) => {
                    loader.finish();
                    return Err(msg.into());
                }
            };
            let _ = last_loss_for_log;

            let lr_base = lr_base_for(epoch, ctx.global_step, config, ctx.plateau.as_ref());
            let batch_weight: f32 = batch.iter().map(|s| s.weight).sum();

            if batch_weight > 0.0 {
            let (loss_sum, weight_sum) = train_batch_classic(
                network,
                &batch,
                config,
                &mut scratch,
                lr_base,
                &mut optimizer_state,
                config.grad_clip,
            );
                total_loss += loss_sum;
                total_weight += weight_sum;
                last_loss_for_log = Some(loss_sum / weight_sum);
            } else {
                zero_weight_batches += 1;
                last_loss_for_log = None;
            }
            last_lr_base = lr_base;

            ctx.global_step += 1;
            batch_count += 1;
            let batch_len = batch.len();
            samples_since += batch_len;
            batches_since += 1;
            total_samples_epoch += batch_len;
            wait_ns_since += wait_dur.as_nanos();
            wait_ns_epoch += wait_dur.as_nanos();
            total_batches += 1;

            if let Some(interval) = ctx.save_every {
                if total_batches % interval == 0 {
                    let checkpoint_path =
                        ctx.out_dir.join(format!("checkpoint_batch_{}.fp32.bin", total_batches));
                    save_classic_network(network, &checkpoint_path)?;
                    if ctx.structured.as_ref().map(|lg| lg.to_stdout).unwrap_or(false) {
                        eprintln!("Saved checkpoint: {}", checkpoint_path.display());
                    } else {
                        println!("Saved checkpoint: {}", checkpoint_path.display());
                    }
                }
            }

            if last_report.elapsed().as_secs_f32() >= config.throughput_interval_sec
                && batches_since > 0
            {
                let secs = last_report.elapsed().as_secs_f32().max(MIN_ELAPSED_TIME as f32);
                let sps = samples_since as f32 / secs;
                let bps = batches_since as f32 / secs;
                let avg_bs = samples_since as f32 / batches_since as f32;
                let loader_ratio = if secs > 0.0 {
                    (wait_ns_since as f64 / NANOSECONDS_PER_SECOND) / secs as f64
                } else {
                    0.0
                } * 100.0;

                if ctx.structured.as_ref().map(|lg| lg.to_stdout).unwrap_or(false) {
                    eprintln!(
                        "[throughput] mode=stream-classic epoch={} batches={} sps={:.0} bps={:.2} avg_batch={:.1} loader_ratio={:.1}%",
                        epoch + 1,
                        batch_count,
                        sps,
                        bps,
                        avg_bs,
                        loader_ratio
                    );
                } else {
                    println!(
                        "[throughput] mode=stream-classic epoch={} batches={} sps={:.0} bps={:.2} avg_batch={:.1} loader_ratio={:.1}%",
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
                        "loader_ratio": loader_ratio / 100.0,
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
        }

        loader.finish();

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
            let vl = compute_validation_loss_classic(network, val_samples, config);
            val_loss = Some(vl);
            let (auc, ece) =
                compute_val_auc_and_ece_classic(network, val_samples, config, &ctx.dash);
            val_auc = auc;
            val_ece = ece;
            val_wsum = Some(val_samples.iter().map(|s| s.weight as f64).sum());
        }

        if let Some(vl) = val_loss {
            if vl < *ctx.trackers.best_val_loss {
                *ctx.trackers.best_val_loss = vl;
                *ctx.trackers.best_network = Some(Network::Classic(network.clone()));
                *ctx.trackers.best_epoch = Some(epoch + 1);
            }
            *ctx.trackers.last_val_loss = Some(vl);
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

        let epoch_secs = epoch_start.elapsed().as_secs_f32().max(MIN_ELAPSED_TIME as f32);
        let epoch_sps = (total_samples_epoch as f32) / epoch_secs;
        let loader_ratio_epoch = if epoch_secs > 0.0 {
            ((wait_ns_epoch as f64 / NANOSECONDS_PER_SECOND) / epoch_secs as f64)
                * PERCENTAGE_DIVISOR as f64
        } else {
            0.0
        } as f32;

        if ctx.structured.as_ref().map(|lg| lg.to_stdout).unwrap_or(false) {
            eprintln!(
                "Epoch {}/{}: train_loss={:.4} val_loss={} batches={} time={:.2}s sps={:.0} loader_ratio={:.1}%",
                epoch + 1,
                config.epochs,
                avg_loss,
                val_loss
                    .map(|v| format!("{:.4}", v))
                    .unwrap_or_else(|| "NA".into()),
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
                val_loss
                    .map(|v| format!("{:.4}", v))
                    .unwrap_or_else(|| "NA".into()),
                batch_count,
                epoch_secs,
                epoch_sps,
                loader_ratio_epoch
            );
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
    match network {
        Network::Single(inner) => train_model_with_loader_single(
            inner,
            train_samples,
            validation_samples,
            config,
            rng,
            ctx,
        ),
        Network::Classic(inner) => train_model_with_loader_classic(
            inner,
            train_samples,
            validation_samples,
            config,
            rng,
            ctx,
        ),
    }
}

fn train_model_with_loader_single(
    network: &mut SingleNetwork,
    train_samples: Vec<Sample>,
    validation_samples: &Option<Vec<Sample>>,
    config: &Config,
    rng: &mut StdRng,
    ctx: &mut TrainContext,
) -> Result<(), Box<dyn std::error::Error>> {
    // Local aliases to minimize code churn (avoid double references)
    let out_dir = ctx.out_dir;
    let dash = &ctx.dash;
    let save_every = ctx.save_every;
    let best_network: &mut Option<Network> = ctx.trackers.best_network;
    let best_val_loss: &mut f32 = ctx.trackers.best_val_loss;
    let last_val_loss: &mut Option<f32> = ctx.trackers.last_val_loss;
    let best_epoch: &mut Option<usize> = ctx.trackers.best_epoch;
    let train_samples_arc = Arc::new(train_samples);
    let mut adam_state = if config.optimizer.eq_ignore_ascii_case("adam")
        || config.optimizer.eq_ignore_ascii_case("adamw")
    {
        if config.optimizer.eq_ignore_ascii_case("adamw") {
            emit_single_adamw_warning();
        }
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

fn train_batch_classic(
    network: &mut ClassicNetwork,
    batch: &[Sample],
    config: &Config,
    scratch: &mut ClassicTrainScratch,
    lr_base: f32,
    optimizer: &mut ClassicOptimizerState,
    grad_clip: f32,
) -> (f32, f32) {
    let fp32 = &mut network.fp32;
    let relu_clip = CLASSIC_RELU_CLIP_F32;
    let acc_dim = fp32.acc_dim;
    let h1_dim = fp32.h1_dim;
    let h2_dim = fp32.h2_dim;
    let input_dim = acc_dim * 2;

    scratch.begin_grad_batch();
    let approx_rows = batch
        .len()
        .saturating_mul(config.estimated_features_per_sample);
    if approx_rows > 0 {
        scratch.reserve_ft_rows(approx_rows);
    }

    let mut grad_b3 = 0.0f32;

    let mut total_loss = 0.0f32;
    let mut total_weight = 0.0f32;

    for sample in batch {
        if sample.weight == 0.0 {
            continue;
        }

        scratch.zero_grads();
        let output = scratch.forward(fp32, relu_clip, &sample.features);

        let (loss, grad_out) = match config.label_type.as_str() {
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

        grad_b3 += grad_out;
        for i in 0..h2_dim {
            scratch.grad_w3[i] += grad_out * scratch.a2[i];
            let delta = grad_out * fp32.output_weights[i] * relu_clip_grad(scratch.z2[i], relu_clip);
            scratch.d_z2[i] = delta;
            for j in 0..h1_dim {
                let idx = i * h1_dim + j;
                scratch.grad_hidden2[idx] += delta * scratch.a1[j];
                scratch.d_a1[j] += delta * fp32.hidden2_weights[idx];
            }
            scratch.grad_hidden2_biases[i] += delta;
        }

        for (j, bias) in scratch
            .grad_hidden1_biases
            .iter_mut()
            .enumerate()
            .take(h1_dim)
        {
            let delta = scratch.d_a1[j] * relu_clip_grad(scratch.z1[j], relu_clip);
            scratch.d_z1[j] = delta;
            let base = j * input_dim;
            for k in 0..input_dim {
                let idx = base + k;
                scratch.grad_hidden1[idx] += delta * scratch.input[k];
                scratch.d_input[k] += delta * fp32.hidden1_weights[base + k];
            }
            *bias += delta;
        }

        {
            let (d_acc_us_src, d_acc_them_src) = scratch.d_input.split_at(acc_dim);
            scratch.d_acc_us_buf.copy_from_slice(d_acc_us_src);
            scratch.d_acc_them_buf.copy_from_slice(d_acc_them_src);
        }
        for i in 0..acc_dim {
            scratch.grad_ft_biases[i] += scratch.d_acc_us_buf[i] + scratch.d_acc_them_buf[i];
        }
        let src_ptr_us = scratch.d_acc_us_buf.as_ptr();
        let src_ptr_them = scratch.d_acc_them_buf.as_ptr();

        // SAFETY: d_acc_*_buf are freshly copied from d_input and are not modified while we update rows.
        for &feat in &sample.features {
            let idx = feat as usize;
            if idx >= fp32.input_dim {
                warn_classic_oor("backward(us)", feat, fp32.input_dim, fp32.acc_dim);
                continue;
            }
            let row = scratch.grad_row_mut(idx, acc_dim);
            for j in 0..acc_dim {
                unsafe {
                    *row.get_unchecked_mut(j) += *src_ptr_us.add(j);
                }
            }
        }

        // SAFETY: mirrored buffer for them-side follows the same invariants as above.
        for idx in 0..scratch.features_them.len() {
            let feat = scratch.features_them[idx];
            let idx = feat as usize;
            if idx >= fp32.input_dim {
                warn_classic_oor("backward(them)", feat, fp32.input_dim, fp32.acc_dim);
                continue;
            }
            let row = scratch.grad_row_mut(idx, acc_dim);
            for j in 0..acc_dim {
                unsafe {
                    *row.get_unchecked_mut(j) += *src_ptr_them.add(j);
                }
            }
        }
    }

    if total_weight <= 0.0 {
        return (0.0, 0.0);
    }

    let inv_sum = 1.0 / total_weight;

    grad_b3 *= inv_sum;
    for g in scratch.grad_w3.iter_mut() {
        *g *= inv_sum;
    }
    for g in scratch.grad_hidden2.iter_mut() {
        *g *= inv_sum;
    }
    for g in scratch.grad_hidden2_biases.iter_mut() {
        *g *= inv_sum;
    }
    for g in scratch.grad_hidden1.iter_mut() {
        *g *= inv_sum;
    }
    for g in scratch.grad_hidden1_biases.iter_mut() {
        *g *= inv_sum;
    }
    for g in scratch.grad_ft_biases.iter_mut() {
        *g *= inv_sum;
    }
    let stamp = scratch.current_grad_stamp();
    for row in scratch.grad_ft_rows.values_mut() {
        if row.last_stamp == stamp {
            for g in row.values.iter_mut() {
                *g *= inv_sum;
            }
        }
    }

    if grad_clip > 0.0 {
        let grads_mut = ClassicGradMut {
            b3: &mut grad_b3,
            w3: &mut scratch.grad_w3,
            hidden2: &mut scratch.grad_hidden2,
            hidden2_biases: &mut scratch.grad_hidden2_biases,
            hidden1: &mut scratch.grad_hidden1,
            hidden1_biases: &mut scratch.grad_hidden1_biases,
            ft_biases: &mut scratch.grad_ft_biases,
            ft_rows: &mut scratch.grad_ft_rows,
            ft_rows_stamp: stamp,
        };
        maybe_clip_gradients(grad_clip, grads_mut);
    }

    let grad_refs = ClassicGrads {
        w3: &scratch.grad_w3,
        b3: grad_b3,
        hidden2: &scratch.grad_hidden2,
        hidden2_biases: &scratch.grad_hidden2_biases,
        hidden1: &scratch.grad_hidden1,
        hidden1_biases: &scratch.grad_hidden1_biases,
        ft_biases: &scratch.grad_ft_biases,
        ft_rows: &scratch.grad_ft_rows,
        ft_rows_stamp: stamp,
    };
    let opt_params = ClassicOptParams { lr: lr_base, l2_reg: config.l2_reg, decoupled_weight_decay: false };
    optimizer.apply(fp32, grad_refs, opt_params);

    (total_loss, total_weight)
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

fn compute_validation_loss_classic(
    network: &ClassicNetwork,
    samples: &[Sample],
    config: &Config,
) -> f32 {
    let mut total_loss = 0.0f32;
    let mut total_weight = 0.0f32;
    let mut scratch = ClassicForwardScratch::new(
        network.fp32.acc_dim,
        network.fp32.h1_dim,
        network.fp32.h2_dim,
    );

    for sample in samples {
        let output = network.forward_with_scratch(&sample.features, &mut scratch);
        let loss = match config.label_type.as_str() {
            "wdl" => {
                let (l, _) = bce_with_logits(output, sample.label);
                l
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

fn compute_val_auc_classic(
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
        let p = sigmoid(out);
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

fn compute_val_auc_and_ece_classic(
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
            let p = sigmoid(out);
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

#[inline]
fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}
static WARNED_CLASSIC_OOR: AtomicBool = AtomicBool::new(false);

struct FtGradRow {
    values: Vec<f32>,
    last_stamp: u64,
}

impl FtGradRow {
    fn new(len: usize, stamp: u64) -> Self {
        Self {
            values: vec![0.0; len],
            last_stamp: stamp,
        }
    }

    fn ensure_stamp(&mut self, stamp: u64) {
        if self.last_stamp != stamp {
            self.values.fill(0.0);
            self.last_stamp = stamp;
        }
    }
}

struct ClassicTrainScratch {
    acc_us: Vec<f32>,
    acc_them: Vec<f32>,
    input: Vec<f32>,
    z1: Vec<f32>,
    a1: Vec<f32>,
    z2: Vec<f32>,
    a2: Vec<f32>,
    d_z2: Vec<f32>,
    d_a1: Vec<f32>,
    d_z1: Vec<f32>,
    d_input: Vec<f32>,
    features_them: Vec<u32>,
    d_acc_us_buf: Vec<f32>,
    d_acc_them_buf: Vec<f32>,
    grad_w3: Vec<f32>,
    grad_hidden2: Vec<f32>,
    grad_hidden2_biases: Vec<f32>,
    grad_hidden1: Vec<f32>,
    grad_hidden1_biases: Vec<f32>,
    grad_ft_biases: Vec<f32>,
    grad_ft_rows: HashMap<usize, FtGradRow>,
    grad_batch_stamp: u64,
}

impl ClassicTrainScratch {
    fn new(acc_dim: usize, h1_dim: usize, h2_dim: usize) -> Self {
        Self {
            acc_us: vec![0.0; acc_dim],
            acc_them: vec![0.0; acc_dim],
            input: vec![0.0; acc_dim * 2],
            z1: vec![0.0; h1_dim],
            a1: vec![0.0; h1_dim],
            z2: vec![0.0; h2_dim],
            a2: vec![0.0; h2_dim],
            d_z2: vec![0.0; h2_dim],
            d_a1: vec![0.0; h1_dim],
            d_z1: vec![0.0; h1_dim],
            d_input: vec![0.0; acc_dim * 2],
            features_them: Vec::new(),
            d_acc_us_buf: vec![0.0; acc_dim],
            d_acc_them_buf: vec![0.0; acc_dim],
            grad_w3: vec![0.0; h2_dim],
            grad_hidden2: vec![0.0; h1_dim * h2_dim],
            grad_hidden2_biases: vec![0.0; h2_dim],
            grad_hidden1: vec![0.0; (acc_dim * 2) * h1_dim],
            grad_hidden1_biases: vec![0.0; h1_dim],
            grad_ft_biases: vec![0.0; acc_dim],
            grad_ft_rows: HashMap::new(),
            grad_batch_stamp: 0,
        }
    }

    fn begin_grad_batch(&mut self) {
        self.grad_batch_stamp = self.grad_batch_stamp.wrapping_add(1);
        self.grad_w3.fill(0.0);
        self.grad_hidden2.fill(0.0);
        self.grad_hidden2_biases.fill(0.0);
        self.grad_hidden1.fill(0.0);
        self.grad_hidden1_biases.fill(0.0);
        self.grad_ft_biases.fill(0.0);
    }

    fn reserve_ft_rows(&mut self, approx_keys: usize) {
        if approx_keys > self.grad_ft_rows.capacity() {
            self.grad_ft_rows
                .reserve(approx_keys - self.grad_ft_rows.capacity());
        }
    }

    fn grad_row_mut(&mut self, idx: usize, acc_dim: usize) -> &mut [f32] {
        let stamp = self.grad_batch_stamp;
        let entry = self
            .grad_ft_rows
            .entry(idx)
            .or_insert_with(|| FtGradRow::new(acc_dim, stamp));
        entry.ensure_stamp(stamp);
        &mut entry.values
    }

    fn current_grad_stamp(&self) -> u64 {
        self.grad_batch_stamp
    }

    fn forward(
        &mut self,
        net: &ClassicFloatNetwork,
        relu_clip: f32,
        features_us: &[u32],
    ) -> f32 {
        let acc_dim = net.acc_dim;
        self.acc_us.copy_from_slice(&net.ft_biases);
        for &feat in features_us {
            let idx = feat as usize;
            if idx >= net.input_dim {
                warn_classic_oor("forward(us)", feat, net.input_dim, net.acc_dim);
                continue;
            }
            let base = idx * acc_dim;
            let row = &net.ft_weights[base..base + acc_dim];
            for (dst, &w) in self.acc_us.iter_mut().zip(row.iter()) {
                *dst += w;
            }
        }

        self.acc_them.copy_from_slice(&net.ft_biases);
        self.features_them.clear();
        self.features_them.reserve(features_us.len());
        for &feat in features_us {
            let flipped = flip_us_them(feat as usize) as u32;
            self.features_them.push(flipped);
            let idx = flipped as usize;
            if idx >= net.input_dim {
                warn_classic_oor("forward(them)", flipped, net.input_dim, net.acc_dim);
                continue;
            }
            let base = idx * acc_dim;
            let row = &net.ft_weights[base..base + acc_dim];
            for (dst, &w) in self.acc_them.iter_mut().zip(row.iter()) {
                *dst += w;
            }
        }

        self.input[..acc_dim].copy_from_slice(&self.acc_us);
        self.input[acc_dim..].copy_from_slice(&self.acc_them);

        let input_dim = acc_dim * 2;
        for i in 0..net.h1_dim {
            let row = &net.hidden1_weights[i * input_dim..(i + 1) * input_dim];
            let mut sum = net.hidden1_biases[i];
            for (w, &x) in row.iter().zip(self.input.iter()) {
                sum += w * x;
            }
            self.z1[i] = sum;
            self.a1[i] = sum.max(0.0).min(relu_clip);
        }

        for i in 0..net.h2_dim {
            let row = &net.hidden2_weights[i * net.h1_dim..(i + 1) * net.h1_dim];
            let mut sum = net.hidden2_biases[i];
            for (w, &x) in row.iter().zip(self.a1.iter()) {
                sum += w * x;
            }
            self.z2[i] = sum;
            self.a2[i] = sum.max(0.0).min(relu_clip);
        }

        let mut out = net.output_bias;
        for (w, &x) in net.output_weights.iter().zip(self.a2.iter()) {
            out += w * x;
        }
        out
    }

    fn zero_grads(&mut self) {
        self.d_z2.fill(0.0);
        self.d_a1.fill(0.0);
        self.d_z1.fill(0.0);
        self.d_input.fill(0.0);
    }
}

#[inline]
fn warn_classic_oor(ctx: &str, feat: u32, input_dim: usize, acc_dim: usize) {
    if !WARNED_CLASSIC_OOR.swap(true, Ordering::Relaxed) {
        let ft_len = input_dim.saturating_mul(acc_dim);
        log::warn!(
            "{}: feature index {} out of range (input_dim={}, acc_dim={}, ft_len={}); subsequent warnings suppressed",
            ctx,
            feat,
            input_dim,
            acc_dim,
            ft_len
        );
    }
}

fn emit_single_adamw_warning() {
    const MSG: &str =
        "--opt adamw は --arch single では Adam として扱います（decoupled weight decay 未対応）";
    log::warn!("{}", MSG);
    eprintln!("{}", MSG);
}

enum ClassicOptimizerState {
    Sgd,
    Adam(ClassicAdamState),
    AdamW(ClassicAdamState),
}

impl ClassicOptimizerState {
    fn new(net: &ClassicFloatNetwork, config: &Config) -> Result<Self, String> {
        let opt = config.optimizer.as_str();
        if opt.eq_ignore_ascii_case("sgd") {
            Ok(ClassicOptimizerState::Sgd)
        } else if opt.eq_ignore_ascii_case("adam") {
            Ok(ClassicOptimizerState::Adam(ClassicAdamState::new(net, 0.0)))
        } else if opt.eq_ignore_ascii_case("adamw") {
            Ok(ClassicOptimizerState::AdamW(ClassicAdamState::new(net, config.l2_reg)))
        } else {
            Err(format!(
                "Unsupported optimizer '{}' for Classic architecture (supported: sgd, adam, adamw)",
                opt
            ))
        }
    }

    fn apply(
        &mut self,
        net: &mut ClassicFloatNetwork,
        grads: ClassicGrads<'_>,
        mut params: ClassicOptParams,
    ) {
        match self {
            ClassicOptimizerState::Sgd => apply_sgd(net, grads, params),
            ClassicOptimizerState::Adam(state) => {
                params.decoupled_weight_decay = false;
                state.apply(net, grads, params)
            }
            ClassicOptimizerState::AdamW(state) => {
                // In AdamW, decouple weight decay and do not apply L2 regularization term in the gradient
                params.decoupled_weight_decay = true;
                params.l2_reg = 0.0;
                state.apply(net, grads, params)
            }
        }
    }
}

struct ClassicAdamState {
    beta1: f32,
    beta2: f32,
    epsilon: f32,
    weight_decay: f32,
    t: usize,
    m_w3: Vec<f32>,
    v_w3: Vec<f32>,
    m_output_bias: f32,
    v_output_bias: f32,
    m_hidden2: Vec<f32>,
    v_hidden2: Vec<f32>,
    m_hidden2_biases: Vec<f32>,
    v_hidden2_biases: Vec<f32>,
    m_hidden1: Vec<f32>,
    v_hidden1: Vec<f32>,
    m_hidden1_biases: Vec<f32>,
    v_hidden1_biases: Vec<f32>,
    m_ft_biases: Vec<f32>,
    v_ft_biases: Vec<f32>,
    ft_rows: HashMap<usize, AdamFtRowState>,
}

impl ClassicAdamState {
    fn new(net: &ClassicFloatNetwork, weight_decay: f32) -> Self {
        ClassicAdamState {
            beta1: ADAM_BETA1,
            beta2: ADAM_BETA2,
            epsilon: ADAM_EPSILON,
            weight_decay,
            t: 0,
            m_w3: vec![0.0; net.output_weights.len()],
            v_w3: vec![0.0; net.output_weights.len()],
            m_output_bias: 0.0,
            v_output_bias: 0.0,
            m_hidden2: vec![0.0; net.hidden2_weights.len()],
            v_hidden2: vec![0.0; net.hidden2_weights.len()],
            m_hidden2_biases: vec![0.0; net.hidden2_biases.len()],
            v_hidden2_biases: vec![0.0; net.hidden2_biases.len()],
            m_hidden1: vec![0.0; net.hidden1_weights.len()],
            v_hidden1: vec![0.0; net.hidden1_weights.len()],
            m_hidden1_biases: vec![0.0; net.hidden1_biases.len()],
            v_hidden1_biases: vec![0.0; net.hidden1_biases.len()],
            m_ft_biases: vec![0.0; net.ft_biases.len()],
            v_ft_biases: vec![0.0; net.ft_biases.len()],
            ft_rows: HashMap::new(),
        }
    }

    fn apply(
        &mut self,
        net: &mut ClassicFloatNetwork,
        grads: ClassicGrads<'_>,
        params: ClassicOptParams,
    ) {
        self.t += 1;
        let t = self.t as f32;
        let lr_t = params.lr * (1.0 - self.beta2.powf(t)).sqrt() / (1.0 - self.beta1.powf(t));
        let weight_decay = if params.decoupled_weight_decay {
            self.weight_decay
        } else {
            0.0
        };
        let l2_reg = if params.decoupled_weight_decay { 0.0 } else { params.l2_reg };

        for (i, w) in net.output_weights.iter_mut().enumerate() {
            let grad = grads.w3[i] + l2_reg * *w;
            self.m_w3[i] = self.beta1 * self.m_w3[i] + (1.0 - self.beta1) * grad;
            self.v_w3[i] = self.beta2 * self.v_w3[i] + (1.0 - self.beta2) * grad * grad;
            *w -= lr_t * self.m_w3[i] / (self.v_w3[i].sqrt() + self.epsilon);
            if weight_decay > 0.0 {
                *w -= params.lr * weight_decay * *w;
            }
        }

        self.m_output_bias =
            self.beta1 * self.m_output_bias + (1.0 - self.beta1) * grads.b3;
        self.v_output_bias =
            self.beta2 * self.v_output_bias + (1.0 - self.beta2) * grads.b3 * grads.b3;
        net.output_bias -= lr_t * self.m_output_bias / (self.v_output_bias.sqrt() + self.epsilon);

        for (i, hb) in net
            .hidden2_biases
            .iter_mut()
            .enumerate()
            .take(net.h2_dim)
        {
            let base = i * net.h1_dim;
            for j in 0..net.h1_dim {
                let idx = base + j;
                let grad = grads.hidden2[idx] + l2_reg * net.hidden2_weights[idx];
                self.m_hidden2[idx] = self.beta1 * self.m_hidden2[idx] + (1.0 - self.beta1) * grad;
                self.v_hidden2[idx] =
                    self.beta2 * self.v_hidden2[idx] + (1.0 - self.beta2) * grad * grad;
                net.hidden2_weights[idx] -=
                    lr_t * self.m_hidden2[idx] / (self.v_hidden2[idx].sqrt() + self.epsilon);
                if weight_decay > 0.0 {
                    net.hidden2_weights[idx] -=
                        params.lr * weight_decay * net.hidden2_weights[idx];
                }
            }
            self.m_hidden2_biases[i] = self.beta1 * self.m_hidden2_biases[i]
                + (1.0 - self.beta1) * grads.hidden2_biases[i];
            self.v_hidden2_biases[i] = self.beta2 * self.v_hidden2_biases[i]
                + (1.0 - self.beta2) * grads.hidden2_biases[i] * grads.hidden2_biases[i];
            *hb -= lr_t * self.m_hidden2_biases[i]
                / (self.v_hidden2_biases[i].sqrt() + self.epsilon);
        }

        let input_dim = net.acc_dim * 2;
        for (j, hb) in net
            .hidden1_biases
            .iter_mut()
            .enumerate()
            .take(net.h1_dim)
        {
            let base = j * input_dim;
            for k in 0..input_dim {
                let idx = base + k;
                let grad = grads.hidden1[idx] + l2_reg * net.hidden1_weights[idx];
                self.m_hidden1[idx] = self.beta1 * self.m_hidden1[idx] + (1.0 - self.beta1) * grad;
                self.v_hidden1[idx] =
                    self.beta2 * self.v_hidden1[idx] + (1.0 - self.beta2) * grad * grad;
                net.hidden1_weights[idx] -=
                    lr_t * self.m_hidden1[idx] / (self.v_hidden1[idx].sqrt() + self.epsilon);
                if weight_decay > 0.0 {
                    net.hidden1_weights[idx] -=
                        params.lr * weight_decay * net.hidden1_weights[idx];
                }
            }
            self.m_hidden1_biases[j] = self.beta1 * self.m_hidden1_biases[j]
                + (1.0 - self.beta1) * grads.hidden1_biases[j];
            self.v_hidden1_biases[j] = self.beta2 * self.v_hidden1_biases[j]
                + (1.0 - self.beta2) * grads.hidden1_biases[j] * grads.hidden1_biases[j];
            *hb -= lr_t * self.m_hidden1_biases[j]
                / (self.v_hidden1_biases[j].sqrt() + self.epsilon);
        }

        for (i, fb) in net
            .ft_biases
            .iter_mut()
            .enumerate()
            .take(net.acc_dim)
        {
            self.m_ft_biases[i] = self.beta1 * self.m_ft_biases[i]
                + (1.0 - self.beta1) * grads.ft_biases[i];
            self.v_ft_biases[i] = self.beta2 * self.v_ft_biases[i]
                + (1.0 - self.beta2) * grads.ft_biases[i] * grads.ft_biases[i];
            *fb -= lr_t * self.m_ft_biases[i]
                / (self.v_ft_biases[i].sqrt() + self.epsilon);
        }

        for (&feat_idx, row) in grads.ft_rows.iter() {
            if row.last_stamp != grads.ft_rows_stamp {
                continue;
            }
            if feat_idx >= net.input_dim {
                continue;
            }
            let row_grads = &row.values;
            let base = feat_idx * net.acc_dim;
            let row = &mut net.ft_weights[base..base + net.acc_dim];
            let entry = self.ft_rows.entry(feat_idx).or_insert_with(|| AdamFtRowState::new(net.acc_dim));
            for j in 0..net.acc_dim {
                let grad = row_grads[j] + l2_reg * row[j];
                entry.m[j] = self.beta1 * entry.m[j] + (1.0 - self.beta1) * grad;
                entry.v[j] = self.beta2 * entry.v[j] + (1.0 - self.beta2) * grad * grad;
                row[j] -= lr_t * entry.m[j] / (entry.v[j].sqrt() + self.epsilon);
                if weight_decay > 0.0 {
                    row[j] -= params.lr * weight_decay * row[j];
                }
            }
        }
    }
}

struct AdamFtRowState {
    m: Vec<f32>,
    v: Vec<f32>,
}

impl AdamFtRowState {
    fn new(len: usize) -> Self {
        Self {
            m: vec![0.0; len],
            v: vec![0.0; len],
        }
    }
}

fn apply_sgd(
    net: &mut ClassicFloatNetwork,
    grads: ClassicGrads<'_>,
    params: ClassicOptParams,
) {
    for (i, w) in net.output_weights.iter_mut().enumerate() {
        let grad = grads.w3[i] + params.l2_reg * *w;
        *w -= params.lr * grad;
    }
    net.output_bias -= params.lr * grads.b3;

    for (i, hb) in net
        .hidden2_biases
        .iter_mut()
        .enumerate()
        .take(net.h2_dim)
    {
        let base = i * net.h1_dim;
        for j in 0..net.h1_dim {
            let idx = base + j;
            let grad = grads.hidden2[idx] + params.l2_reg * net.hidden2_weights[idx];
            net.hidden2_weights[idx] -= params.lr * grad;
        }
        *hb -= params.lr * grads.hidden2_biases[i];
    }

    let input_dim = net.acc_dim * 2;
    for (j, hb) in net
        .hidden1_biases
        .iter_mut()
        .enumerate()
        .take(net.h1_dim)
    {
        let base = j * input_dim;
        for k in 0..input_dim {
            let idx = base + k;
            let grad = grads.hidden1[idx] + params.l2_reg * net.hidden1_weights[idx];
            net.hidden1_weights[idx] -= params.lr * grad;
        }
        *hb -= params.lr * grads.hidden1_biases[j];
    }

    for (i, fb) in net
        .ft_biases
        .iter_mut()
        .enumerate()
        .take(net.acc_dim)
    {
        *fb -= params.lr * grads.ft_biases[i];
    }

    for (&feat_idx, row) in grads.ft_rows.iter() {
        if row.last_stamp != grads.ft_rows_stamp {
            continue;
        }
        let row_grads = &row.values;
        if feat_idx >= net.input_dim {
            continue;
        }
        let base = feat_idx * net.acc_dim;
        let row = &mut net.ft_weights[base..base + net.acc_dim];
        for j in 0..net.acc_dim {
            let grad = row_grads[j] + params.l2_reg * row[j];
            row[j] -= params.lr * grad;
        }
    }
}

struct ClassicGrads<'a> {
    w3: &'a [f32],
    b3: f32,
    hidden2: &'a [f32],
    hidden2_biases: &'a [f32],
    hidden1: &'a [f32],
    hidden1_biases: &'a [f32],
    ft_biases: &'a [f32],
    ft_rows: &'a HashMap<usize, FtGradRow>,
    ft_rows_stamp: u64,
}

#[derive(Clone, Copy)]
struct ClassicOptParams {
    lr: f32,
    l2_reg: f32,
    decoupled_weight_decay: bool,
}

struct ClassicGradMut<'a> {
    b3: &'a mut f32,
    w3: &'a mut [f32],
    hidden2: &'a mut [f32],
    hidden2_biases: &'a mut [f32],
    hidden1: &'a mut [f32],
    hidden1_biases: &'a mut [f32],
    ft_biases: &'a mut [f32],
    ft_rows: &'a mut HashMap<usize, FtGradRow>,
    ft_rows_stamp: u64,
}

fn maybe_clip_gradients(clip: f32, grads: ClassicGradMut<'_>) {
    use std::cmp::Ordering;
    if clip.partial_cmp(&0.0) != Some(Ordering::Greater) {
        return;
    }

    let mut norm_sq = (*grads.b3 as f64) * (*grads.b3 as f64);
    norm_sq += grads.w3.iter().map(|g| (*g as f64) * (*g as f64)).sum::<f64>();
    norm_sq += grads.hidden2.iter().map(|g| (*g as f64) * (*g as f64)).sum::<f64>();
    norm_sq += grads
        .hidden2_biases
        .iter()
        .map(|g| (*g as f64) * (*g as f64))
        .sum::<f64>();
    norm_sq += grads.hidden1.iter().map(|g| (*g as f64) * (*g as f64)).sum::<f64>();
    norm_sq += grads
        .hidden1_biases
        .iter()
        .map(|g| (*g as f64) * (*g as f64))
        .sum::<f64>();
    norm_sq += grads.ft_biases.iter().map(|g| (*g as f64) * (*g as f64)).sum::<f64>();
    for row in grads.ft_rows.values() {
        if row.last_stamp != grads.ft_rows_stamp {
            continue;
        }
        norm_sq += row
            .values
            .iter()
            .map(|g| (*g as f64) * (*g as f64))
            .sum::<f64>();
    }

    if !norm_sq.is_finite() {
        return;
    }

    let norm = norm_sq.sqrt() as f32;
    if norm <= clip || !norm.is_finite() {
        return;
    }
    let scale = clip / norm;

    *grads.b3 *= scale;
    for g in grads.w3.iter_mut() {
        *g *= scale;
    }
    for g in grads.hidden2.iter_mut() {
        *g *= scale;
    }
    for g in grads.hidden2_biases.iter_mut() {
        *g *= scale;
    }
    for g in grads.hidden1.iter_mut() {
        *g *= scale;
    }
    for g in grads.hidden1_biases.iter_mut() {
        *g *= scale;
    }
    for g in grads.ft_biases.iter_mut() {
        *g *= scale;
    }
    for row in grads.ft_rows.values_mut() {
        if row.last_stamp != grads.ft_rows_stamp {
            continue;
        }
        for g in row.values.iter_mut() {
            *g *= scale;
        }
    }
}

fn emit_epoch_logging(
    structured: Option<&StructuredLogger>,
    training_config: Option<&serde_json::Value>,
    global_step: u64,
    epoch_index: usize,
    total_epochs: usize,
    avg_loss: f32,
    val_loss: Option<f32>,
    val_auc: Option<f64>,
    val_ece: Option<f64>,
    epoch_secs: f32,
    epoch_sps: f32,
    loader_ratio_pct: Option<f64>,
    batch_count: Option<usize>,
    last_lr_base: f32,
) {
    let epoch_display = epoch_index + 1;
    let val_loss_str = val_loss
        .map(|v| format!("{:.4}", v))
        .unwrap_or_else(|| "NA".into());
    let message = match (batch_count, loader_ratio_pct) {
        (Some(batches), Some(ratio)) => format!(
            "Epoch {}/{}: train_loss={:.4} val_loss={} batches={} time={:.2}s sps={:.0} loader_ratio={:.1}%",
            epoch_display,
            total_epochs,
            avg_loss,
            val_loss_str,
            batches,
            epoch_secs,
            epoch_sps,
            ratio
        ),
        (Some(batches), None) => format!(
            "Epoch {}/{}: train_loss={:.4} val_loss={} batches={} time={:.2}s sps={:.0}",
            epoch_display,
            total_epochs,
            avg_loss,
            val_loss_str,
            batches,
            epoch_secs,
            epoch_sps
        ),
        (None, Some(ratio)) => format!(
            "Epoch {}/{}: train_loss={:.4} val_loss={} time={:.2}s sps={:.0} loader_ratio={:.1}%",
            epoch_display,
            total_epochs,
            avg_loss,
            val_loss_str,
            epoch_secs,
            epoch_sps,
            ratio
        ),
        (None, None) => format!(
            "Epoch {}/{}: train_loss={:.4} val_loss={} time={:.2}s sps={:.0}",
            epoch_display,
            total_epochs,
            avg_loss,
            val_loss_str,
            epoch_secs,
            epoch_sps
        ),
    };

    let human_to_stderr = structured.map(|lg| lg.to_stdout).unwrap_or(false);
    if human_to_stderr {
        eprintln!("{}", message);
    } else {
        println!("{}", message);
    }

    if let Some(lg) = structured {
        // Structured logs consume loader_ratio as 0.0–1.0 fraction; human logs use percentage.
        let loader_ratio_fraction = loader_ratio_pct.map(|pct| pct / 100.0).unwrap_or(0.0);
        let mut rec_train = serde_json::json!({
            "ts": chrono::Utc::now().to_rfc3339(),
            "phase": "train",
            "global_step": global_step as i64,
            "epoch": epoch_display as i64,
            "lr": last_lr_base as f64,
            "train_loss": avg_loss as f64,
            "examples_sec": epoch_sps as f64,
            "loader_ratio": loader_ratio_fraction,
            "wall_time": epoch_secs as f64,
        });
        if let Some(cfg) = training_config {
            rec_train
                .as_object_mut()
                .unwrap()
                .insert("training_config".into(), cfg.clone());
        }
        if let Some(batches) = batch_count {
            rec_train
                .as_object_mut()
                .unwrap()
                .insert("batches".into(), serde_json::json!(batches as i64));
        }
        lg.write_json(&rec_train);

        let mut rec_val = serde_json::json!({
            "ts": chrono::Utc::now().to_rfc3339(),
            "phase": "val",
            "global_step": global_step as i64,
            "epoch": epoch_display as i64,
            "wall_time": epoch_secs as f64,
        });
        if let Some(cfg) = training_config {
            rec_val
                .as_object_mut()
                .unwrap()
                .insert("training_config".into(), cfg.clone());
        }
        if let Some(vl) = val_loss {
            rec_val
                .as_object_mut()
                .unwrap()
                .insert("val_loss".into(), serde_json::json!(vl as f64));
        }
        if let Some(auc) = val_auc {
            rec_val
                .as_object_mut()
                .unwrap()
                .insert("val_auc".into(), serde_json::json!(auc));
        }
        if let Some(ece) = val_ece {
            rec_val
                .as_object_mut()
                .unwrap()
                .insert("val_ece".into(), serde_json::json!(ece));
        }
        lg.write_json(&rec_val);
    }
}

#[inline]
fn relu_clip_grad(z: f32, clip: f32) -> f32 {
    if z <= 0.0 || z >= clip {
        0.0
    } else {
        1.0
    }
}
