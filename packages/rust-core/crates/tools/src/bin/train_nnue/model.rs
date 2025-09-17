use crate::classic::ClassicFloatNetwork;
use crate::params::{
    ADAM_BETA1, ADAM_BETA2, ADAM_EPSILON, CLASSIC_ACC_DIM, CLASSIC_H1_DIM, CLASSIC_H2_DIM,
};
use crate::types::ArchKind;
use engine_core::evaluation::nnue::features::{flip_us_them, FE_END};
use engine_core::shogi::SHOGI_BOARD_SIZE;
use rand::Rng;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

#[derive(Clone)]
pub struct SingleNetwork {
    pub w0: Vec<f32>,
    pub b0: Vec<f32>,
    pub w2: Vec<f32>,
    pub b2: f32,
    pub input_dim: usize,
    pub acc_dim: usize,
    pub relu_clip: f32,
}

impl SingleNetwork {
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

        SingleNetwork {
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

        Ok(SingleNetwork {
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
#[allow(dead_code)]
pub fn forward_into_single(
    network: &SingleNetwork,
    features: &[u32],
    acc: &mut [f32],
    act: &mut [f32],
) -> f32 {
    network.forward_into(features, acc, act)
}

pub struct SingleForwardScratch {
    acc: Vec<f32>,
    act: Vec<f32>,
}

impl SingleForwardScratch {
    pub fn new(acc_dim: usize) -> Self {
        Self {
            acc: vec![0.0; acc_dim],
            act: vec![0.0; acc_dim],
        }
    }

    #[inline]
    pub fn forward(&mut self, network: &SingleNetwork, features: &[u32]) -> f32 {
        network.forward_into(features, &mut self.acc, &mut self.act)
    }

    #[inline]
    pub fn activations(&self) -> &[f32] {
        &self.act
    }

    #[inline]
    #[allow(dead_code)]
    pub fn activations_mut(&mut self) -> &mut [f32] {
        &mut self.act
    }

    #[inline]
    #[allow(dead_code)]
    pub fn accumulator(&self) -> &[f32] {
        &self.acc
    }
}

impl SingleNetwork {
    #[inline]
    pub fn forward_with_scratch(
        &self,
        features: &[u32],
        scratch: &mut SingleForwardScratch,
    ) -> f32 {
        scratch.forward(self, features)
    }
}

#[derive(Clone)]
pub struct ClassicNetwork {
    pub fp32: ClassicFloatNetwork,
    pub relu_clip: f32,
}

#[allow(dead_code)]
impl ClassicNetwork {
    pub fn new(
        acc_dim: usize,
        h1_dim: usize,
        h2_dim: usize,
        _relu_clip: i32,
        fan_in_ft_estimate: usize,
        rng: &mut impl Rng,
    ) -> Self {
        let input_dim = SHOGI_BOARD_SIZE * FE_END;
        let fp32 = ClassicFloatNetwork::he_uniform_with_dims(
            input_dim,
            acc_dim,
            h1_dim,
            h2_dim,
            fan_in_ft_estimate,
            rng,
        );
        ClassicNetwork {
            fp32,
            relu_clip: crate::params::CLASSIC_RELU_CLIP_F32,
        }
    }

    pub fn acc_dim(&self) -> usize {
        self.fp32.acc_dim
    }

    pub fn input_dim(&self) -> usize {
        self.fp32.input_dim
    }

    pub fn h1_dim(&self) -> usize {
        self.fp32.h1_dim
    }

    pub fn h2_dim(&self) -> usize {
        self.fp32.h2_dim
    }

    #[allow(dead_code)]
    pub fn forward_with_scratch(
        &self,
        features_us: &[u32],
        scratch: &mut ClassicForwardScratch,
    ) -> f32 {
        scratch.forward(self, features_us)
    }
}

#[allow(dead_code)]
pub struct ClassicForwardScratch {
    acc_us: Vec<f32>,
    acc_them: Vec<f32>,
    input: Vec<f32>,
    h1: Vec<f32>,
    h1_act: Vec<f32>,
    h2: Vec<f32>,
    h2_act: Vec<f32>,
    features_them: Vec<u32>,
}

#[allow(dead_code)]
impl ClassicForwardScratch {
    pub fn new(acc_dim: usize, h1_dim: usize, h2_dim: usize) -> Self {
        Self {
            acc_us: vec![0.0; acc_dim],
            acc_them: vec![0.0; acc_dim],
            input: vec![0.0; acc_dim * 2],
            h1: vec![0.0; h1_dim],
            h1_act: vec![0.0; h1_dim],
            h2: vec![0.0; h2_dim],
            h2_act: vec![0.0; h2_dim],
            features_them: Vec::with_capacity(256),
        }
    }

    pub fn forward(&mut self, network: &ClassicNetwork, features_us: &[u32]) -> f32 {
        let net = &network.fp32;
        let acc_dim = net.acc_dim;

        debug_assert_eq!(self.acc_us.len(), acc_dim);
        debug_assert_eq!(self.acc_them.len(), acc_dim);

        self.acc_us.copy_from_slice(&net.ft_biases);
        for &feat in features_us {
            let idx = feat as usize;
            if idx >= net.input_dim {
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

        let relu_clip = network.relu_clip;

        let in_dim_h1 = acc_dim * 2;
        for i in 0..net.h1_dim {
            let row = &net.hidden1_weights[i * in_dim_h1..(i + 1) * in_dim_h1];
            let mut sum = net.hidden1_biases[i];
            for (w, &x) in row.iter().zip(self.input.iter()) {
                sum += w * x;
            }
            self.h1[i] = sum;
            self.h1_act[i] = sum.max(0.0).min(relu_clip);
        }

        for i in 0..net.h2_dim {
            let row = &net.hidden2_weights[i * net.h1_dim..(i + 1) * net.h1_dim];
            let mut sum = net.hidden2_biases[i];
            for (w, &x) in row.iter().zip(self.h1_act.iter()) {
                sum += w * x;
            }
            self.h2[i] = sum;
            self.h2_act[i] = sum.max(0.0).min(relu_clip);
        }

        let mut out = net.output_bias;
        for (w, &x) in net.output_weights.iter().zip(self.h2_act.iter()) {
            out += w * x;
        }
        out
    }

    pub fn h2_activations(&self) -> &[f32] {
        &self.h2_act
    }

    pub fn input(&self) -> &[f32] {
        &self.input
    }
}

#[derive(Clone)]
pub enum Network {
    Single(SingleNetwork),
    Classic(ClassicNetwork),
}

#[allow(dead_code)]
impl Network {
    pub fn new_single(acc_dim: usize, relu_clip: i32, rng: &mut impl Rng) -> Self {
        Network::Single(SingleNetwork::new(acc_dim, relu_clip, rng))
    }

    pub fn new_classic(relu_clip: i32, fan_in_ft_estimate: usize, rng: &mut impl Rng) -> Self {
        Network::Classic(ClassicNetwork::new(
            CLASSIC_ACC_DIM,
            CLASSIC_H1_DIM,
            CLASSIC_H2_DIM,
            relu_clip,
            fan_in_ft_estimate,
            rng,
        ))
    }

    pub fn new_forward_scratch(&self) -> ForwardScratch {
        match self {
            Network::Single(net) => ForwardScratch::Single(SingleForwardScratch::new(net.acc_dim)),
            Network::Classic(net) => ForwardScratch::Classic(ClassicForwardScratch::new(
                net.fp32.acc_dim,
                net.fp32.h1_dim,
                net.fp32.h2_dim,
            )),
        }
    }

    pub fn forward_with_scratch(&self, features: &[u32], scratch: &mut ForwardScratch) -> f32 {
        match (self, scratch) {
            (Network::Single(net), ForwardScratch::Single(buf)) => {
                net.forward_with_scratch(features, buf)
            }
            (Network::Classic(net), ForwardScratch::Classic(buf)) => {
                net.forward_with_scratch(features, buf)
            }
            (Network::Single(net), scratch_ref) => {
                *scratch_ref = ForwardScratch::Single(SingleForwardScratch::new(net.acc_dim));
                if let ForwardScratch::Single(buf) = scratch_ref {
                    net.forward_with_scratch(features, buf)
                } else {
                    unreachable!()
                }
            }
            (Network::Classic(net), scratch_ref) => {
                *scratch_ref = ForwardScratch::Classic(ClassicForwardScratch::new(
                    net.fp32.acc_dim,
                    net.fp32.h1_dim,
                    net.fp32.h2_dim,
                ));
                if let ForwardScratch::Classic(buf) = scratch_ref {
                    net.forward_with_scratch(features, buf)
                } else {
                    unreachable!()
                }
            }
        }
    }

    pub fn arch(&self) -> ArchKind {
        match self {
            Network::Single(_) => ArchKind::Single,
            Network::Classic(_) => ArchKind::Classic,
        }
    }

    pub fn as_single(&self) -> Option<&SingleNetwork> {
        if let Network::Single(ref inner) = self {
            Some(inner)
        } else {
            None
        }
    }

    pub fn as_single_mut(&mut self) -> Option<&mut SingleNetwork> {
        if let Network::Single(ref mut inner) = self {
            Some(inner)
        } else {
            None
        }
    }

    pub fn as_classic(&self) -> Option<&ClassicNetwork> {
        if let Network::Classic(ref inner) = self {
            Some(inner)
        } else {
            None
        }
    }

    pub fn as_classic_mut(&mut self) -> Option<&mut ClassicNetwork> {
        if let Network::Classic(ref mut inner) = self {
            Some(inner)
        } else {
            None
        }
    }

    pub fn load(path: &Path) -> std::io::Result<Self> {
        SingleNetwork::load(path).map(Network::Single)
    }
}

#[inline]
#[allow(dead_code)]
pub fn forward_into(
    network: &SingleNetwork,
    features: &[u32],
    acc: &mut [f32],
    act: &mut [f32],
) -> f32 {
    forward_into_single(network, features, acc, act)
}

#[allow(dead_code)]
pub enum ForwardScratch {
    Single(SingleForwardScratch),
    Classic(ClassicForwardScratch),
}

pub struct SingleAdamState {
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

impl SingleAdamState {
    pub fn new(network: &SingleNetwork) -> Self {
        SingleAdamState {
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
