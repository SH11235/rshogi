use crate::{evaluation::nnue::features::extract_features, Color, Position};

/// SINGLE_CHANNEL (acc=256 -> 1) network for NNUE
/// - Returns raw centipawns from side-to-move perspective (cp, unclipped except final clamp).
/// - Weights are stored in FP32 for simplicity and correctness.
pub struct SingleChannelNet {
    pub n_feat: usize,        // e.g., SHOGI_BOARD_SIZE * FE_END
    pub acc_dim: usize,       // 256
    pub scale: f32,           // typically 600.0
    pub w0: Vec<f32>,         // [n_feat * acc_dim]
    pub b0: Option<Vec<f32>>, // [acc_dim]
    pub w2: Vec<f32>,         // [acc_dim]
    pub b2: f32,              // scalar
}

impl SingleChannelNet {
    #[inline]
    fn infer_with_active_indices(&self, active: &[usize], _stm: Color) -> i32 {
        let d = self.acc_dim;
        debug_assert!(d <= 256);
        let mut acc = [0f32; 256];

        // Accumulate embedding rows
        for &fid in active {
            debug_assert!(fid < self.n_feat);
            let base = fid * d;
            let row = &self.w0[base..base + d];
            for (a, r) in acc[..d].iter_mut().zip(row.iter()) {
                *a += *r;
            }
        }

        // Bias0 if present
        if let Some(ref b0) = self.b0 {
            for (a, b) in acc[..d].iter_mut().zip(b0.iter()) {
                *a += *b;
            }
        }

        // ReLU
        for v in &mut acc[..d] {
            if *v < 0.0 {
                *v = 0.0;
            }
        }

        // Output
        let mut cp = self.b2;
        for (w, a) in self.w2[..d].iter().zip(acc[..d].iter()) {
            cp += (*w) * (*a);
        }

        // Apply a conservative clip for stability in search
        let cp = cp.clamp(-32000.0, 32000.0);
        cp as i32
    }

    /// Evaluate a position by extracting HalfKP active features for side-to-move
    pub fn evaluate(&self, pos: &Position) -> i32 {
        let stm = pos.side_to_move;
        let king_sq = match stm {
            Color::Black => pos.board.king_square(Color::Black),
            Color::White => pos.board.king_square(Color::White),
        };
        let Some(ksq) = king_sq else { return 0 };

        // Extract oriented features for stm perspective
        let feats = extract_features(pos, ksq, stm);
        self.infer_with_active_indices(feats.as_slice(), stm)
    }
}
