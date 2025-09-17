use crate::params::{ADAM_BETA1, ADAM_BETA2, ADAM_EPSILON};
use engine_core::evaluation::nnue::features::FE_END;
use engine_core::shogi::SHOGI_BOARD_SIZE;
use rand::Rng;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

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

    pub fn load(path: &Path) -> std::io::Result<Self> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);

        let mut line = String::new();
        reader.read_line(&mut line)?; // NNUE
        if !line.trim().eq_ignore_ascii_case("nnue") {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "Invalid NNUE magic"));
        }

        let mut acc_dim: Option<usize> = None;
        let mut relu_clip: Option<f32> = None;

        loop {
            line.clear();
            let bytes = reader.read_line(&mut line)?;
            if bytes == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "Unexpected EOF before END_HEADER",
                ));
            }
            let trimmed = line.trim();
            if trimmed == "END_HEADER" {
                break;
            }
            if let Some(rest) = trimmed.strip_prefix("ACC_DIM") {
                acc_dim = rest.trim().parse::<usize>().ok();
            } else if let Some(rest) = trimmed.strip_prefix("RELU_CLIP") {
                relu_clip = rest.trim().parse::<f32>().ok();
            }
        }

        let acc_dim = acc_dim.ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "Missing ACC_DIM header")
        })?;
        let relu_clip = relu_clip.unwrap_or(127.0);

        let read_u32 = |reader: &mut BufReader<File>| -> std::io::Result<u32> {
            let mut buf = [0u8; 4];
            reader.read_exact(&mut buf)?;
            Ok(u32::from_le_bytes(buf))
        };

        let input_dim = read_u32(&mut reader)? as usize;
        let acc_dim_file = read_u32(&mut reader)? as usize;
        if acc_dim_file != acc_dim {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("ACC_DIM mismatch: header {} vs payload {}", acc_dim, acc_dim_file),
            ));
        }

        let read_f32_vec =
            |reader: &mut BufReader<File>, len: usize| -> std::io::Result<Vec<f32>> {
                let mut buf = vec![0u8; len * 4];
                reader.read_exact(&mut buf)?;
                Ok(buf
                    .chunks_exact(4)
                    .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                    .collect())
            };

        let w0 = read_f32_vec(&mut reader, input_dim * acc_dim)?;
        let b0 = read_f32_vec(&mut reader, acc_dim)?;
        let w2 = read_f32_vec(&mut reader, acc_dim)?;

        let mut b2_bytes = [0u8; 4];
        reader.read_exact(&mut b2_bytes)?;
        let b2 = f32::from_le_bytes(b2_bytes);

        Ok(Self {
            w0,
            b0,
            w2,
            b2,
            input_dim,
            acc_dim,
            relu_clip,
        })
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
