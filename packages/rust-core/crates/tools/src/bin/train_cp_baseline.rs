use std::fs::{create_dir_all, File};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::time::SystemTime;

use engine_core::shogi::board::{Color, PieceType};
use engine_core::Position;

#[derive(Clone, Debug)]
struct Config {
    epochs: usize,
    lr: f32,
    l2: f32,
}

fn now_stamp() -> String {
    let dur = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
    format!("{}", dur.as_secs())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() < 2 {
        eprintln!("Usage: train_cp_baseline <train.txt> <valid.txt> [epochs] [lr] [l2] [out_dir]");
        eprintln!("  defaults: epochs=3 lr=1e-3 l2=1e-6 out_dir=runs/cp_baseline_<ts>");
        std::process::exit(1);
    }
    let train_path = PathBuf::from(&args[0]);
    let valid_path = PathBuf::from(&args[1]);
    let epochs = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(3);
    let lr = args.get(3).and_then(|s| s.parse::<f32>().ok()).unwrap_or(1e-3);
    let l2 = args.get(4).and_then(|s| s.parse::<f32>().ok()).unwrap_or(1e-6);
    let out_dir = args
        .get(5)
        .cloned()
        .unwrap_or_else(|| format!("runs/cp_baseline_{}", now_stamp()));

    let cfg = Config { epochs, lr, l2 };
    println!("config: epochs={} lr={} l2={} out_dir={} ", cfg.epochs, cfg.lr, cfg.l2, out_dir);

    // Feature dimension: bias(1) + side_to_move(1) + board counts (2*7) + hand counts (2*7) = 30
    const DIM: usize = 30;
    let mut w = vec![0f32; DIM];

    train(&train_path, &cfg, &mut w)?;

    // Evaluate on train and valid
    let (mse_t, mae_t) = evaluate(&train_path, &w)?;
    let (mse_v, mae_v) = evaluate(&valid_path, &w)?;
    println!(
        "train: MSE={:.2} MAE={:.2} | valid: MSE={:.2} MAE={:.2}",
        mse_t, mae_t, mse_v, mae_v
    );

    // Save weights
    let out_dir = PathBuf::from(out_dir);
    create_dir_all(&out_dir)?;
    let mut f = File::create(out_dir.join("weights.txt"))?;
    for v in &w {
        writeln!(f, "{:.8}", v)?;
    }
    println!("saved: {}", out_dir.join("weights.txt").display());

    Ok(())
}

fn clip_cp(cp: i32) -> f32 {
    let c = cp.clamp(-32000, 32000);
    c as f32
}

fn extract_features(pos: &Position) -> [f32; 30] {
    let mut feat = [0f32; 30];
    let mut idx = 0usize;
    // bias
    feat[idx] = 1.0;
    idx += 1;
    // side to move
    feat[idx] = if pos.side_to_move == Color::Black {
        1.0
    } else {
        -1.0
    };
    idx += 1;
    // Board counts for non-king piece types: Black then White
    for &pt in &[
        PieceType::Rook,
        PieceType::Bishop,
        PieceType::Gold,
        PieceType::Silver,
        PieceType::Knight,
        PieceType::Lance,
        PieceType::Pawn,
    ] {
        let c = pos.board.piece_bb[Color::Black as usize][pt as usize].count_ones() as f32;
        feat[idx] = c;
        idx += 1;
    }
    for &pt in &[
        PieceType::Rook,
        PieceType::Bishop,
        PieceType::Gold,
        PieceType::Silver,
        PieceType::Knight,
        PieceType::Lance,
        PieceType::Pawn,
    ] {
        let c = pos.board.piece_bb[Color::White as usize][pt as usize].count_ones() as f32;
        feat[idx] = c;
        idx += 1;
    }
    // Hand counts Black then White (order matches Position.hands)
    for i in 0..7 {
        feat[idx] = pos.hands[Color::Black as usize][i] as f32;
        idx += 1;
    }
    for i in 0..7 {
        feat[idx] = pos.hands[Color::White as usize][i] as f32;
        idx += 1;
    }
    debug_assert_eq!(idx, 30);
    feat
}

fn parse_line(line: &str) -> Option<(Position, f32)> {
    let eval_idx = line.find(" eval ")?;
    let sfen = line[..eval_idx].trim();
    let rest = &line[eval_idx + 6..];
    let cp_str = rest.split_whitespace().next()?;
    let cp = cp_str.parse::<i32>().ok()?;
    let pos = Position::from_sfen(sfen).ok()?;
    Some((pos, clip_cp(cp)))
}

fn train(path: &PathBuf, cfg: &Config, w: &mut [f32]) -> Result<(), Box<dyn std::error::Error>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let lines: Vec<String> = reader.lines().collect::<Result<_, _>>()?;
    println!("train: {} lines", lines.len());
    if lines.is_empty() {
        return Ok(());
    }

    // Simple shuffled order (deterministic): even-odd interleave
    let mut order: Vec<usize> = (0..lines.len()).collect();
    // No RNG dependency: do a cheap permutation
    order.sort_by_key(|&i| (i % 2, i / 2));

    for ep in 0..cfg.epochs {
        let mut mse = 0f64;
        let mut mae = 0f64;
        let mut n = 0f64;
        for &i in &order {
            if let Some((pos, y)) = parse_line(&lines[i]) {
                let x = extract_features(&pos);
                // y_pred = w·x
                let mut y_pred = 0f32;
                for j in 0..w.len() {
                    y_pred += w[j] * x[j];
                }
                let err = y_pred - y;
                // SGD update: w <- w - lr * (err*x + l2*w)
                for j in 0..w.len() {
                    let grad = err * x[j] + cfg.l2 * w[j];
                    w[j] -= cfg.lr * grad;
                }
                let e = err as f64;
                mse += e * e;
                mae += e.abs();
                n += 1.0;
            }
        }
        if n > 0.0 {
            mse /= n;
            mae /= n;
        }
        println!("epoch {}: MSE={:.2} MAE={:.2}", ep + 1, mse, mae);
    }
    Ok(())
}

fn evaluate(path: &PathBuf, w: &[f32]) -> Result<(f64, f64), Box<dyn std::error::Error>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut mse = 0f64;
    let mut mae = 0f64;
    let mut n = 0f64;
    for line in reader.lines() {
        let line = line?;
        if let Some((pos, y)) = parse_line(&line) {
            let x = extract_features(&pos);
            let mut y_pred = 0f32;
            for j in 0..w.len() {
                y_pred += w[j] * x[j];
            }
            let e = (y_pred - y) as f64;
            mse += e * e;
            mae += e.abs();
            n += 1.0;
        }
    }
    if n > 0.0 {
        mse /= n;
        mae /= n;
    }
    Ok((mse, mae))
}
