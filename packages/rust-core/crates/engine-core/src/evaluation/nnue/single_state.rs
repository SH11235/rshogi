use super::single::SingleChannelNet;
use crate::{Color, Position};

/// SINGLE_CHANNEL 用の増分 Acc（土台）。
/// - acc_dim はネットから取得（通常は 256）
/// - 値は ReLU 後を保持する（evaluate_from_accumulator にそのまま渡せる）
#[derive(Clone, Debug)]
pub struct SingleAcc {
    acc: Vec<f32>,
}

impl SingleAcc {
    #[inline]
    pub fn as_slice(&self) -> &[f32] {
        &self.acc
    }

    /// 現局面からフル再構築（差分なし）。白番視点では王座標を flip。
    pub fn refresh(pos: &Position, net: &SingleChannelNet) -> Self {
        let d = net.acc_dim;
        let mut acc = vec![0.0f32; d];

        // Bias0（ある場合のみ）
        if let Some(ref b0) = net.b0 {
            debug_assert_eq!(b0.len(), d);
            for (a, b) in acc.iter_mut().zip(b0.iter()) {
                *a += *b;
            }
        }

        let stm = pos.side_to_move;
        // HalfKP と同様、白番は王座標を flip
        let king_sq = match stm {
            Color::Black => pos.board.king_square(Color::Black),
            Color::White => pos.board.king_square(Color::White).map(|sq| sq.flip()),
        };

        if let Some(ksq) = king_sq {
            let feats = super::features::extract_features(pos, ksq, stm);
            // 行ベクトルを加算
            for &fid in feats.as_slice() {
                if fid >= net.n_feat {
                    continue;
                }
                let base = fid * d;
                let row = &net.w0[base..base + d];
                for (a, w) in acc.iter_mut().zip(row.iter()) {
                    *a += *w;
                }
            }
        }

        // ReLU
        for v in &mut acc {
            if *v < 0.0 {
                *v = 0.0;
            }
        }

        SingleAcc { acc }
    }
}
