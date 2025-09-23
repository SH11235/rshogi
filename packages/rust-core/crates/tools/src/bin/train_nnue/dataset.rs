use crate::params::{
    BASELINE_MIN_EPS, CP_CLAMP_LIMIT, CP_TO_FLOAT_DIVISOR, GAP_WEIGHT_DIVISOR,
    LINE_BUFFER_CAPACITY, NON_EXACT_BOUND_WEIGHT, SELECTIVE_DEPTH_MARGIN, SELECTIVE_DEPTH_WEIGHT,
};
use crate::types::{Config, Sample, TrainingPosition};
use engine_core::game_phase::{detect_game_phase, GamePhase, Profile};
use engine_core::{
    evaluation::nnue::features::{extract_features, FE_END},
    shogi::SHOGI_BOARD_SIZE,
    Color, Position,
};
use std::io::{BufRead, BufReader, Read};
use tools::common::weighting as wcfg;
use tools::nnfc_v1::{
    flags as fc_flags, open_payload_reader as open_cache_payload_reader_shared, FEATURE_SET_ID_HALF,
};

const BUF_MB: usize = 4;

type CachePayload = (BufReader<Box<dyn Read>>, u64, u32);

pub fn open_jsonl_reader(path: &str) -> Result<Box<dyn BufRead>, Box<dyn std::error::Error>> {
    const BYTES_PER_MB: usize = crate::params::BYTES_PER_MB;
    tools::io_detect::open_maybe_compressed_reader(path, BUF_MB * BYTES_PER_MB)
}

pub fn load_samples(
    path: &str,
    config: &Config,
    weighting: &wcfg::WeightingConfig,
) -> Result<Vec<Sample>, Box<dyn std::error::Error>> {
    let mut reader = open_jsonl_reader(path)?;
    let mut samples = Vec::new();
    let mut skipped = 0;
    let mut line_buf: Vec<u8> = Vec::with_capacity(LINE_BUFFER_CAPACITY);

    loop {
        line_buf.clear();
        let n = reader.read_until(b'\n', &mut line_buf)?;
        if n == 0 {
            break;
        }
        if line_buf.iter().all(|b| b.is_ascii_whitespace()) {
            continue;
        }

        let pos_data: TrainingPosition = match serde_json::from_slice(&line_buf) {
            Ok(data) => data,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };

        if config.exclude_no_legal_move && pos_data.no_legal_move.unwrap_or(false) {
            skipped += 1;
            continue;
        }
        if config.exclude_fallback && pos_data.fallback_used.unwrap_or(false) {
            skipped += 1;
            continue;
        }

        // Prefer teacher-provided labels if available
        let cp = if let Some(ref ts) = pos_data.teacher_score {
            match (ts.kind.as_deref(), ts.value) {
                (Some("cp"), Some(v)) => v,
                // For mate labels, we do not map to cp here; fallback below
                _ => pos_data
                    .teacher_cp
                    .or(pos_data.eval)
                    .or_else(|| pos_data.lines.first().and_then(|l| l.score_cp))
                    .unwrap_or(0),
            }
        } else if let Some(v) = pos_data.teacher_cp {
            v
        } else if let Some(eval) = pos_data.eval {
            eval
        } else if let Some(line) = pos_data.lines.first() {
            line.score_cp.unwrap_or(0)
        } else {
            skipped += 1;
            continue;
        };

        let position = match Position::from_sfen(&pos_data.sfen) {
            Ok(pos) => pos,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };

        let (Some(black_king), Some(white_king)) = (
            position.board.king_square(Color::Black),
            position.board.king_square(Color::White),
        ) else {
            skipped += 1;
            continue;
        };

        let stm = position.side_to_move;
        let cp_black = if stm == Color::Black { cp } else { -cp };
        let cp_white = -cp_black;

        let phase = detect_game_phase(&position, position.ply as u32, Profile::Search);

        let base_weight = calculate_weight(&pos_data);
        let both_exact = is_exact_opt(&pos_data.bound1) && is_exact_opt(&pos_data.bound2);
        let mate_ring = pos_data.mate_boundary.unwrap_or(false);
        let mut weight = wcfg::apply_weighting(
            base_weight,
            weighting,
            Some(both_exact),
            pos_data.best2_gap_cp,
            Some(to_phase_kind(phase)),
            Some(mate_ring),
        );
        if let Some(tw) = pos_data.teacher_weight {
            if tw.is_finite() && tw > 0.0 {
                weight *= tw.min(1.0);
            }
        }

        {
            let feats = extract_features(&position, black_king, Color::Black);
            let features: Vec<u32> = feats.as_slice().iter().map(|&f| f as u32).collect();
            let label = match config.label_type.as_str() {
                "wdl" => cp_to_wdl(cp_black, config.scale),
                "cp" => {
                    (cp_black.clamp(-config.cp_clip, config.cp_clip) as f32) / CP_TO_FLOAT_DIVISOR
                }
                _ => continue,
            };
            samples.push(Sample {
                features,
                label,
                weight,
                cp: Some(cp_black),
                phase: Some(phase),
            });
        }

        {
            let feats = extract_features(&position, white_king, Color::White);
            let features: Vec<u32> = feats.as_slice().iter().map(|&f| f as u32).collect();
            let label = match config.label_type.as_str() {
                "wdl" => cp_to_wdl(cp_white, config.scale),
                "cp" => {
                    (cp_white.clamp(-config.cp_clip, config.cp_clip) as f32) / CP_TO_FLOAT_DIVISOR
                }
                _ => continue,
            };
            samples.push(Sample {
                features,
                label,
                weight,
                cp: Some(cp_white),
                phase: Some(phase),
            });
        }
    }

    if skipped > 0 {
        eprintln!("Skipped {} positions (invalid/filtered)", skipped);
    }

    Ok(samples)
}

pub fn open_cache_payload_reader(path: &str) -> Result<CachePayload, Box<dyn std::error::Error>> {
    let (r, header) = open_cache_payload_reader_shared(path)?;
    if header.feature_set_id != FEATURE_SET_ID_HALF {
        return Err(format!(
            "Unsupported feature_set_id: 0x{:08x} for file {}",
            header.feature_set_id, path
        )
        .into());
    }
    Ok((r, header.num_samples, header.flags_mask))
}

pub fn load_samples_from_cache(
    path: &str,
    weighting: &wcfg::WeightingConfig,
) -> Result<Vec<Sample>, Box<dyn std::error::Error>> {
    let (mut r, num_samples, flags_mask) = open_cache_payload_reader(path)?;
    eprintln!("Loading cache: {num_samples} samples");

    let mut samples = Vec::with_capacity(num_samples as usize);
    let mut unknown_flag_samples: u64 = 0;
    let mut unknown_flag_bits_accum: u32 = 0;

    for i in 0..num_samples {
        if i % 100000 == 0 && i > 0 {
            eprintln!("  Loaded {i}/{num_samples} samples...");
        }

        let mut nb = [0u8; 4];
        r.read_exact(&mut nb)?;
        let n_features = u32::from_le_bytes(nb) as usize;
        const MAX_FEATURES_PER_SAMPLE: usize = SHOGI_BOARD_SIZE * FE_END;
        if n_features > MAX_FEATURES_PER_SAMPLE {
            return Err(format!(
                "n_features={} exceeds sane limit {}; file {} may be corrupted",
                n_features, MAX_FEATURES_PER_SAMPLE, path
            )
            .into());
        }

        let mut features: Vec<u32> = vec![0u32; n_features];
        #[cfg(target_endian = "little")]
        {
            use bytemuck::cast_slice_mut;
            r.read_exact(cast_slice_mut::<u32, u8>(&mut features))?;
        }
        #[cfg(target_endian = "big")]
        {
            let mut buf = vec![0u8; n_features * 4];
            r.read_exact(&mut buf)?;
            for (dst, chunk) in features.iter_mut().zip(buf.chunks_exact(4)) {
                *dst = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
            }
        }

        let mut lb = [0u8; 4];
        r.read_exact(&mut lb)?;
        let label = f32::from_le_bytes(lb);

        let mut gapb = [0u8; 2];
        r.read_exact(&mut gapb)?;
        let gap = u16::from_le_bytes(gapb);

        let mut depth = [0u8; 1];
        r.read_exact(&mut depth)?;
        let depth = depth[0];

        let mut seldepth = [0u8; 1];
        r.read_exact(&mut seldepth)?;
        let seldepth = seldepth[0];

        let mut flags = [0u8; 1];
        r.read_exact(&mut flags)?;
        let flags = flags[0];
        let unknown = (flags as u32) & !flags_mask;
        if unknown != 0 {
            unknown_flag_samples += 1;
            unknown_flag_bits_accum |= unknown;
        }

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
        weight = wcfg::apply_weighting(
            weight,
            weighting,
            Some(both_exact),
            Some(gap as i32),
            None,
            Some(mate_ring),
        );

        samples.push(Sample {
            features,
            label,
            weight,
            cp: None,
            phase: None,
        });
    }

    if unknown_flag_samples > 0 {
        eprintln!(
            "Warning: {} samples contained unknown flag bits (mask=0x{:08x}, seen=0x{:08x})",
            unknown_flag_samples, flags_mask, unknown_flag_bits_accum
        );
    }

    Ok(samples)
}

pub fn is_cache_file(path: &str) -> bool {
    match open_cache_payload_reader_shared(path) {
        Ok((_r, header)) => header.feature_set_id == FEATURE_SET_ID_HALF,
        Err(_) => false,
    }
}

fn cp_to_wdl(cp: i32, scale: f32) -> f32 {
    let x = (cp as f32 / scale).clamp(-CP_CLAMP_LIMIT, CP_CLAMP_LIMIT);
    1.0 / (1.0 + (-x).exp())
}

fn is_exact_opt(s: &Option<String>) -> bool {
    s.as_deref()
        .map(|t| t.trim())
        .map(|t| t.eq_ignore_ascii_case("Exact"))
        .unwrap_or(false)
}

fn calculate_weight(pos_data: &TrainingPosition) -> f32 {
    let mut weight = 1.0;

    if let Some(gap) = pos_data.best2_gap_cp {
        let base_gap = (gap as f32 / GAP_WEIGHT_DIVISOR).clamp(BASELINE_MIN_EPS, 1.0);
        weight *= base_gap;
    }

    let both_exact = is_exact_opt(&pos_data.bound1) && is_exact_opt(&pos_data.bound2);
    weight *= if both_exact {
        1.0
    } else {
        NON_EXACT_BOUND_WEIGHT
    };

    if pos_data.mate_boundary.unwrap_or(false) {
        weight *= 0.5;
    }

    if let (Some(depth), Some(seldepth)) = (pos_data.depth, pos_data.seldepth) {
        if seldepth < depth.saturating_add(SELECTIVE_DEPTH_MARGIN as u8) {
            weight *= SELECTIVE_DEPTH_WEIGHT;
        }
    }

    weight
}

fn to_phase_kind(ph: GamePhase) -> wcfg::PhaseKind {
    match ph {
        GamePhase::Opening => wcfg::PhaseKind::Opening,
        GamePhase::MiddleGame => wcfg::PhaseKind::Middlegame,
        GamePhase::EndGame => wcfg::PhaseKind::Endgame,
    }
}
