use crate::params::{ADAM_BETA1, ADAM_BETA2, ADAM_EPSILON};
use engine_core::evaluation::nnue::features::FE_END;
use engine_core::shogi::SHOGI_BOARD_SIZE;
use rand::Rng;

#[derive(Clone)]
pub struct Network {
    pub w0: Vec<f32>,
    pub b0: Vec<f32>,
    pub w2: Vec<f32>,
    pub b2: f32,
    pub input_dim: usize,
    pub acc_dim: usize,
    pub relu_clip: f32,
}

impl Network {
    pub fn new(acc_dim: usize, relu_clip: i32, rng: &mut impl Rng) -> Self {
        let input_dim = SHOGI_BOARD_SIZE * FE_END;
        let w0_size = input_dim * acc_dim;
        let mut w0 = vec![0.0f32; w0_size];
        for w in w0.iter_mut() {
            *w = rng.random_range(-0.01..0.01);
        }

        let b0 = vec![0.0f32; acc_dim];

        let mut w2 = vec![0.0f32; acc_dim];
        for w in w2.iter_mut() {
            *w = rng.random_range(-0.01..0.01);
        }

        Network {
            w0,
            b0,
            w2,
            b2: 0.0,
            input_dim,
            acc_dim,
            relu_clip: relu_clip as f32,
        }
    }

    pub fn forward_with_buffers(
        &self,
        features: &[u32],
        acc_buffer: &mut Vec<f32>,
        activated_buffer: &mut Vec<f32>,
    ) -> f32 {
        acc_buffer.clear();
        acc_buffer.extend_from_slice(&self.b0);

        for &feat_idx in features {
            let feat_idx = feat_idx as usize;
            #[cfg(debug_assertions)]
            debug_assert!(
                feat_idx < self.input_dim,
                "feat_idx={} out of range {}",
                feat_idx,
                self.input_dim
            );

            let offset = feat_idx * self.acc_dim;
            for (i, acc_val) in acc_buffer.iter_mut().enumerate() {
                *acc_val += self.w0[offset + i];
            }
        }

        activated_buffer.resize(self.acc_dim, 0.0);
        for (i, &x) in acc_buffer.iter().enumerate() {
            activated_buffer[i] = x.max(0.0).min(self.relu_clip);
        }

        let mut output = self.b2;
        for (w, &act) in self.w2.iter().zip(activated_buffer.iter()) {
            output += w * act;
        }

        output
    }

    pub fn forward_into(&self, features: &[u32], acc: &mut [f32], act: &mut [f32]) -> f32 {
        acc.copy_from_slice(&self.b0);

        for &f in features {
            let f = f as usize;
            #[cfg(debug_assertions)]
            debug_assert!(f < self.input_dim);

            let off = f * self.acc_dim;
            for (i, acc_val) in acc.iter_mut().enumerate() {
                *acc_val += self.w0[off + i];
            }
        }

        for (i, &x) in acc.iter().enumerate() {
            act[i] = x.max(0.0).min(self.relu_clip);
        }

        let mut out = self.b2;
        for (i, &act_val) in act.iter().enumerate() {
            out += self.w2[i] * act_val;
        }
        out
    }
}

#[inline]
pub fn forward_into(network: &Network, features: &[u32], acc: &mut [f32], act: &mut [f32]) -> f32 {
    network.forward_into(features, acc, act)
}

pub struct AdamState {
    pub m_w0: Vec<f32>,
    pub v_w0: Vec<f32>,
    pub m_b0: Vec<f32>,
    pub v_b0: Vec<f32>,
    pub m_w2: Vec<f32>,
    pub v_w2: Vec<f32>,
    pub m_b2: f32,
    pub v_b2: f32,
    pub beta1: f32,
    pub beta2: f32,
    pub epsilon: f32,
    pub t: usize,
}

impl AdamState {
    pub fn new(network: &Network) -> Self {
        AdamState {
            m_w0: vec![0.0; network.w0.len()],
            v_w0: vec![0.0; network.w0.len()],
            m_b0: vec![0.0; network.b0.len()],
            v_b0: vec![0.0; network.b0.len()],
            m_w2: vec![0.0; network.w2.len()],
            v_w2: vec![0.0; network.w2.len()],
            m_b2: 0.0,
            v_b2: 0.0,
            beta1: ADAM_BETA1,
            beta2: ADAM_BETA2,
            epsilon: ADAM_EPSILON,
            t: 0,
        }
    }
}
