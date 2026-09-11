//! Runtime-dimension HalfKX inference used by `edition-universal`.
//!
//! Fixed editions deliberately keep using the const-generic implementations.

use std::io::{self, Read, Seek, SeekFrom};

use super::accumulator::{
    AlignedBox, DirtyPiece, IndexList, MAX_ACTIVE_FEATURES, MAX_CHANGED_FEATURES,
};
use super::activation::{CReLU, FtActivation, PairwiseCReLU, SCReLU, default_qa_for_arch};
use super::constants::{
    FV_SCALE, FV_SCALE_HALFKA, MAX_ARCH_LEN, NNUE_VERSION, NNUE_VERSION_HALFKA,
};
use super::features::{
    FeatureSet as FeatureSetTrait, HalfKPFeatureSet, HalfKaHmMergedFeatureSet,
    HalfKaHmSplitFeatureSet, HalfKaMergedFeatureSet, HalfKaSplitFeatureSet,
};
use super::layers::padded_input;
use super::network::{get_fv_scale_override, parse_fv_scale_from_arch};
use super::spec::{
    Activation, ArchitectureSpec, FeatureSet, parse_arch_dimensions, parse_architecture,
    parse_feature_input_dimensions, parse_halfkp_l1,
};
use crate::position::Position;
use crate::types::{Color, Value};

const STACK_CAPACITY: usize = 256;
const MAX_RUNTIME_DIMENSION: usize = 16_384;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeFeatureSet {
    HalfKP,
    HalfKaHmMerged,
    HalfKaSplit,
    HalfKaMerged,
    HalfKaHmSplit,
}

impl RuntimeFeatureSet {
    fn from_spec(feature_set: FeatureSet) -> io::Result<Self> {
        match feature_set {
            FeatureSet::HalfKP => Ok(Self::HalfKP),
            FeatureSet::HalfKaHmMerged => Ok(Self::HalfKaHmMerged),
            FeatureSet::HalfKaSplit => Ok(Self::HalfKaSplit),
            FeatureSet::HalfKaMerged => Ok(Self::HalfKaMerged),
            FeatureSet::HalfKaHmSplit => Ok(Self::HalfKaHmSplit),
            _ => Err(invalid_data(format!("{feature_set} is not a HalfKX feature set"))),
        }
    }

    fn dimensions(self) -> usize {
        match self {
            Self::HalfKP => HalfKPFeatureSet::DIMENSIONS,
            Self::HalfKaHmMerged => HalfKaHmMergedFeatureSet::DIMENSIONS,
            Self::HalfKaSplit => HalfKaSplitFeatureSet::DIMENSIONS,
            Self::HalfKaMerged => HalfKaMergedFeatureSet::DIMENSIONS,
            Self::HalfKaHmSplit => HalfKaHmSplitFeatureSet::DIMENSIONS,
        }
    }

    fn active(self, pos: &Position, perspective: Color) -> IndexList<MAX_ACTIVE_FEATURES> {
        match self {
            Self::HalfKP => HalfKPFeatureSet::collect_active_indices(pos, perspective),
            Self::HalfKaHmMerged => {
                HalfKaHmMergedFeatureSet::collect_active_indices(pos, perspective)
            }
            Self::HalfKaSplit => HalfKaSplitFeatureSet::collect_active_indices(pos, perspective),
            Self::HalfKaMerged => HalfKaMergedFeatureSet::collect_active_indices(pos, perspective),
            Self::HalfKaHmSplit => {
                HalfKaHmSplitFeatureSet::collect_active_indices(pos, perspective)
            }
        }
    }

    fn changed(
        self,
        dirty: &DirtyPiece,
        perspective: Color,
        pos: &Position,
    ) -> (IndexList<MAX_CHANGED_FEATURES>, IndexList<MAX_CHANGED_FEATURES>) {
        let king_sq = pos.king_square(perspective);
        match self {
            Self::HalfKP => HalfKPFeatureSet::collect_changed_indices(dirty, perspective, king_sq),
            Self::HalfKaHmMerged => {
                HalfKaHmMergedFeatureSet::collect_changed_indices(dirty, perspective, king_sq)
            }
            Self::HalfKaSplit => {
                HalfKaSplitFeatureSet::collect_changed_indices(dirty, perspective, king_sq)
            }
            Self::HalfKaMerged => {
                HalfKaMergedFeatureSet::collect_changed_indices(dirty, perspective, king_sq)
            }
            Self::HalfKaHmSplit => {
                HalfKaHmSplitFeatureSet::collect_changed_indices(dirty, perspective, king_sq)
            }
        }
    }

    fn needs_refresh(self, dirty: &DirtyPiece, perspective: Color) -> bool {
        match self {
            Self::HalfKP => HalfKPFeatureSet::needs_refresh(dirty, perspective),
            Self::HalfKaHmMerged => HalfKaHmMergedFeatureSet::needs_refresh(dirty, perspective),
            Self::HalfKaSplit => HalfKaSplitFeatureSet::needs_refresh(dirty, perspective),
            Self::HalfKaMerged => HalfKaMergedFeatureSet::needs_refresh(dirty, perspective),
            Self::HalfKaHmSplit => HalfKaHmSplitFeatureSet::needs_refresh(dirty, perspective),
        }
    }
}

pub(crate) struct DynamicAffine {
    input_dim: usize,
    padded_input: usize,
    output_dim: usize,
    biases: AlignedBox<i32>,
    weights: AlignedBox<i8>,
}

impl DynamicAffine {
    pub(crate) fn read<R: Read>(
        reader: &mut R,
        input_dim: usize,
        output_dim: usize,
    ) -> io::Result<Self> {
        let padded_input = padded_input(input_dim);
        let mut biases = AlignedBox::new_zeroed(output_dim);
        let mut buf4 = [0; 4];
        for bias in biases.iter_mut() {
            reader.read_exact(&mut buf4)?;
            *bias = i32::from_le_bytes(buf4);
        }
        let weight_len = output_dim
            .checked_mul(padded_input)
            .ok_or_else(|| invalid_data("affine weight dimensions overflow"))?;
        let mut weights = AlignedBox::new_zeroed(weight_len);
        let mut byte = [0];
        for weight in weights.iter_mut() {
            reader.read_exact(&mut byte)?;
            *weight = byte[0] as i8;
        }
        Ok(Self {
            input_dim,
            padded_input,
            output_dim,
            biases,
            weights,
        })
    }

    pub(crate) fn weight_len(&self) -> usize {
        self.weights.len()
    }

    pub(crate) fn file_weight(&self, index: usize) -> i8 {
        self.weights[index]
    }

    pub(crate) fn apply_file_weight_delta(&mut self, index: usize, delta: i32) -> bool {
        let (value, clamped) = super::net_delta::add_i8_delta(self.weights[index], delta);
        self.weights[index] = value;
        clamped
    }

    pub(crate) fn bias(&self, index: usize) -> i32 {
        self.biases[index]
    }

    pub(crate) fn apply_bias_delta(&mut self, index: usize, delta: i32) -> bool {
        let (value, clamped) = super::net_delta::add_i32_delta(self.biases[index], delta);
        self.biases[index] = value;
        clamped
    }

    #[inline]
    pub(crate) fn propagate(&self, input: &[u8], output: &mut [i32]) {
        debug_assert!(input.len() >= self.padded_input);
        debug_assert!(output.len() >= self.output_dim);

        #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
        // SAFETY: `input` covers `padded_input`, every weight row has exactly that padded
        // length, and the four-row/output loops only load or store complete 16-byte groups.
        unsafe {
            use std::arch::wasm32::*;

            use super::layers::{dot_i8x16_u8i8_preexpanded, haddx4, hsum_i32x4};

            let chunks = self.padded_input / 16;
            let input_ptr = input.as_ptr();
            let weights_ptr = self.weights.as_ptr();
            let mut output_index = 0;

            while output_index + 4 <= self.output_dim {
                let mut acc0 = i32x4_splat(0);
                let mut acc1 = i32x4_splat(0);
                let mut acc2 = i32x4_splat(0);
                let mut acc3 = i32x4_splat(0);
                let row0 = weights_ptr.add(output_index * self.padded_input);
                let row1 = row0.add(self.padded_input);
                let row2 = row1.add(self.padded_input);
                let row3 = row2.add(self.padded_input);
                for chunk in 0..chunks {
                    let offset = chunk * 16;
                    let input_vec = v128_load(input_ptr.add(offset).cast::<v128>());
                    let input_lo = i16x8_extend_low_u8x16(input_vec);
                    let input_hi = i16x8_extend_high_u8x16(input_vec);
                    acc0 = i32x4_add(
                        acc0,
                        dot_i8x16_u8i8_preexpanded(
                            input_lo,
                            input_hi,
                            v128_load(row0.add(offset).cast::<v128>()),
                        ),
                    );
                    acc1 = i32x4_add(
                        acc1,
                        dot_i8x16_u8i8_preexpanded(
                            input_lo,
                            input_hi,
                            v128_load(row1.add(offset).cast::<v128>()),
                        ),
                    );
                    acc2 = i32x4_add(
                        acc2,
                        dot_i8x16_u8i8_preexpanded(
                            input_lo,
                            input_hi,
                            v128_load(row2.add(offset).cast::<v128>()),
                        ),
                    );
                    acc3 = i32x4_add(
                        acc3,
                        dot_i8x16_u8i8_preexpanded(
                            input_lo,
                            input_hi,
                            v128_load(row3.add(offset).cast::<v128>()),
                        ),
                    );
                }
                let sums = haddx4(acc0, acc1, acc2, acc3);
                let biases = v128_load(self.biases.as_ptr().add(output_index).cast::<v128>());
                v128_store(
                    output.as_mut_ptr().add(output_index).cast::<v128>(),
                    i32x4_add(sums, biases),
                );
                output_index += 4;
            }

            while output_index < self.output_dim {
                let mut acc = i32x4_splat(0);
                let row = weights_ptr.add(output_index * self.padded_input);
                for chunk in 0..chunks {
                    let offset = chunk * 16;
                    let input_vec = v128_load(input_ptr.add(offset).cast::<v128>());
                    let weights_vec = v128_load(row.add(offset).cast::<v128>());
                    acc = i32x4_add(acc, super::layers::dot_i8x16_u8i8(input_vec, weights_vec));
                }
                output[output_index] = self.biases[output_index] + hsum_i32x4(acc);
                output_index += 1;
            }
            return;
        }

        #[allow(unreachable_code)]
        for (o, out) in output[..self.output_dim].iter_mut().enumerate() {
            let row = &self.weights[o * self.padded_input..(o + 1) * self.padded_input];
            let mut sum = self.biases[o];
            for (&x, &w) in input[..self.input_dim].iter().zip(row) {
                sum = sum.wrapping_add(i32::from(x) * i32::from(w));
            }
            *out = sum;
        }
    }
}

/// Runtime-dimension HalfKX network.
pub struct DynamicHalfKxNetwork {
    spec: ArchitectureSpec,
    feature_set: RuntimeFeatureSet,
    input_dimensions: usize,
    activation: Activation,
    ft_biases: AlignedBox<i16>,
    ft_weights: AlignedBox<i16>,
    l1: DynamicAffine,
    l2: DynamicAffine,
    output: DynamicAffine,
    fv_scale: i32,
    qa: i16,
}

impl DynamicHalfKxNetwork {
    pub(crate) fn read<R: Read + Seek>(
        reader: &mut R,
        feature_override: Option<FeatureSet>,
    ) -> io::Result<Self> {
        let file_size = reader.seek(SeekFrom::End(0))?;
        reader.seek(SeekFrom::Start(0))?;

        let mut buf4 = [0; 4];
        reader.read_exact(&mut buf4)?;
        let version = u32::from_le_bytes(buf4);
        if version != NNUE_VERSION && version != NNUE_VERSION_HALFKA {
            return Err(invalid_data(format!("unsupported HalfKX NNUE version: {version:#x}")));
        }
        reader.read_exact(&mut buf4)?; // structure hash
        reader.read_exact(&mut buf4)?;
        let arch_len = u32::from_le_bytes(buf4) as usize;
        if arch_len == 0 || arch_len > MAX_ARCH_LEN {
            return Err(invalid_data(format!("invalid architecture string length: {arch_len}")));
        }
        let mut arch = vec![0; arch_len];
        reader.read_exact(&mut arch)?;
        let arch = std::str::from_utf8(&arch)
            .map_err(|_| invalid_data("architecture string is not valid UTF-8"))?;
        if arch.contains("Factorizer") {
            return Err(invalid_data(
                "factorized training models must be coalesced before inference",
            ));
        }

        let (effective_feature_set, header_l1, header_l2, header_l3) =
            if let Some(feature_set) = feature_override {
                let (mut l1, l2, l3) = parse_arch_dimensions(arch);
                if feature_set == FeatureSet::HalfKP {
                    let halfkp_l1 = parse_halfkp_l1(arch);
                    if halfkp_l1 != 0 {
                        l1 = halfkp_l1;
                    }
                }
                (feature_set, l1, l2, l3)
            } else {
                let parsed = parse_architecture(arch).map_err(invalid_data)?;
                (parsed.feature_set, parsed.l1, parsed.l2, parsed.l3)
            };
        let feature_set = RuntimeFeatureSet::from_spec(effective_feature_set)?;
        let detected = super::spec::detect_architecture_from_size(
            file_size,
            arch_len,
            Some(effective_feature_set),
        );
        let (l1_dim, l2_dim, l3_dim) = detected
            .map(|d| (d.spec.l1, d.spec.l2, d.spec.l3))
            .unwrap_or((header_l1, header_l2, header_l3));
        validate_dimension("l1", l1_dim)?;
        validate_dimension("l2", l2_dim)?;
        validate_dimension("l3", l3_dim)?;
        let input_dimensions = feature_set.dimensions();
        // 明示 override は誤記ヘッダーを補正する。payload 自体のサイズ検証は維持する。
        if feature_override.is_none() {
            let header_dimensions = parse_feature_input_dimensions(arch).ok_or_else(|| {
                invalid_data("HalfKX architecture is missing FT input dimensions")
            })?;
            if header_dimensions != input_dimensions {
                return Err(invalid_data(format!(
                    "FT input dimension mismatch: header={header_dimensions}, feature_set={effective_feature_set} expects {input_dimensions}"
                )));
            }
        }
        let activation = if arch.contains("PairwiseCReLU") || arch.contains("-Pairwise") {
            Activation::PairwiseCReLU
        } else if arch.contains("SCReLU") {
            Activation::SCReLU
        } else {
            Activation::CReLU
        };
        let dense_input = l1_dim
            .checked_mul(2)
            .and_then(|v| v.checked_div(activation.output_dim_divisor()))
            .ok_or_else(|| invalid_data("invalid FT output dimensions"))?;

        reader.read_exact(&mut buf4)?; // feature transformer hash
        let mut ft_biases = AlignedBox::new_zeroed(l1_dim);
        let mut buf2 = [0; 2];
        for bias in ft_biases.iter_mut() {
            reader.read_exact(&mut buf2)?;
            *bias = i16::from_le_bytes(buf2);
        }
        let ft_weight_len = input_dimensions
            .checked_mul(l1_dim)
            .ok_or_else(|| invalid_data("FT weight dimensions overflow"))?;
        let mut ft_weights = AlignedBox::new_zeroed(ft_weight_len);
        for weight in ft_weights.iter_mut() {
            reader.read_exact(&mut buf2)?;
            *weight = i16::from_le_bytes(buf2);
        }

        reader.read_exact(&mut buf4)?; // dense network hash
        let l1 = DynamicAffine::read(reader, dense_input, l2_dim)?;
        let l2 = DynamicAffine::read(reader, l2_dim, l3_dim)?;
        let output = DynamicAffine::read(reader, l3_dim, 1)?;
        let consumed = reader.stream_position()?;
        if consumed != file_size {
            let trailing_len = (file_size - consumed) as usize;
            let mut trailing = vec![0; trailing_len];
            reader.read_exact(&mut trailing)?;
            let marker = b"bullet";
            let is_bullet_marker = trailing_len <= 64
                && trailing.iter().enumerate().all(|(i, &b)| b == marker[i % marker.len()]);
            if !is_bullet_marker {
                return Err(invalid_data(format!(
                    "NNUE payload size mismatch: consumed={consumed}, file_size={file_size}"
                )));
            }
        }

        let fv_scale = parse_fv_scale_from_arch(arch).unwrap_or(
            if effective_feature_set == FeatureSet::HalfKP {
                FV_SCALE
            } else {
                FV_SCALE_HALFKA
            },
        );
        let qa = parse_qa(arch).unwrap_or_else(|| default_qa_for_arch(arch));
        Ok(Self {
            spec: ArchitectureSpec::new(effective_feature_set, l1_dim, l2_dim, l3_dim, activation),
            feature_set,
            input_dimensions,
            activation,
            ft_biases,
            ft_weights,
            l1,
            l2,
            output,
            fv_scale,
            qa,
        })
    }

    pub(crate) fn spec(&self) -> ArchitectureSpec {
        self.spec
    }
    pub(crate) fn l1_size(&self) -> usize {
        self.spec.l1
    }
    pub(crate) fn is_halfkp(&self) -> bool {
        self.spec.feature_set == FeatureSet::HalfKP
    }

    pub(crate) fn refresh(&self, pos: &Position, stack: &mut DynamicHalfKxStack) {
        for perspective in [Color::Black, Color::White] {
            self.refresh_perspective(pos, perspective, stack.current_accumulation_mut(perspective));
        }
        stack.computed[stack.current] = true;
    }

    fn refresh_perspective(&self, pos: &Position, perspective: Color, accumulation: &mut [i16]) {
        accumulation.copy_from_slice(&self.ft_biases);
        for index in self.feature_set.active(pos, perspective).iter() {
            add_row(accumulation, self.ft_row(index));
        }
    }

    pub(crate) fn ensure(&self, pos: &Position, stack: &mut DynamicHalfKxStack) {
        if stack.computed[stack.current] {
            return;
        }
        if stack.current == 0 || !stack.computed[stack.current - 1] {
            self.refresh(pos, stack);
            return;
        }
        let current = stack.current;
        let dirty = stack.dirty[current];
        for perspective in [Color::Black, Color::White] {
            let p = perspective.index();
            if self.feature_set.needs_refresh(&dirty, perspective) {
                self.refresh_perspective(
                    pos,
                    perspective,
                    stack.current_accumulation_mut(perspective),
                );
                continue;
            }
            let (removed, added) = self.feature_set.changed(&dirty, perspective, pos);
            let l1 = self.spec.l1;
            let prev_start = ((current - 1) * 2 + p) * l1;
            let curr_start = (current * 2 + p) * l1;
            let (before, after) = stack.accumulations.split_at_mut(curr_start);
            let prev = &before[prev_start..prev_start + l1];
            let curr = &mut after[..l1];
            curr.copy_from_slice(prev);
            for index in removed.iter() {
                sub_row(curr, self.ft_row(index));
            }
            for index in added.iter() {
                add_row(curr, self.ft_row(index));
            }
        }
        stack.computed[current] = true;
    }

    pub(crate) fn evaluate(&self, pos: &Position, stack: &mut DynamicHalfKxStack) -> Value {
        let l1 = self.spec.l1;
        let stm = pos.side_to_move().index();
        let opp = (!pos.side_to_move()).index();
        let base = stack.current * 2 * l1;
        stack.ft_raw[..l1]
            .copy_from_slice(&stack.accumulations[base + stm * l1..base + (stm + 1) * l1]);
        stack.ft_raw[l1..2 * l1]
            .copy_from_slice(&stack.accumulations[base + opp * l1..base + (opp + 1) * l1]);
        stack.ft_activated.fill(0);
        activate_i16(
            self.activation,
            &stack.ft_raw[..2 * l1],
            &mut stack.ft_activated[..self.l1.input_dim],
            self.qa,
        );
        self.l1.propagate(&stack.ft_activated, &mut stack.layer_i32);
        stack.layer_u8.fill(0);
        activate_i32(
            self.activation,
            &stack.layer_i32[..self.spec.l2],
            &mut stack.layer_u8[..self.spec.l2],
        );
        self.l2.propagate(&stack.layer_u8, &mut stack.layer2_i32);
        stack.layer2_u8.fill(0);
        activate_i32(
            self.activation,
            &stack.layer2_i32[..self.spec.l3],
            &mut stack.layer2_u8[..self.spec.l3],
        );
        let mut output = [0];
        self.output.propagate(&stack.layer2_u8, &mut output);
        Value::new(output[0] / get_fv_scale_override().unwrap_or(self.fv_scale))
    }

    #[inline]
    fn ft_row(&self, index: usize) -> &[i16] {
        debug_assert!(index < self.input_dimensions);
        let start = index * self.spec.l1;
        &self.ft_weights[start..start + self.spec.l1]
    }
}

/// Per-search runtime accumulator and preallocated inference scratch.
pub struct DynamicHalfKxStack {
    spec: ArchitectureSpec,
    l1: usize,
    halfkp: bool,
    current: usize,
    accumulations: AlignedBox<i16>,
    computed: Vec<bool>,
    dirty: Vec<DirtyPiece>,
    ft_raw: AlignedBox<i16>,
    ft_activated: AlignedBox<u8>,
    layer_i32: AlignedBox<i32>,
    layer_u8: AlignedBox<u8>,
    layer2_i32: AlignedBox<i32>,
    layer2_u8: AlignedBox<u8>,
}

impl DynamicHalfKxStack {
    pub(crate) fn new(net: &DynamicHalfKxNetwork) -> Self {
        Self {
            spec: net.spec,
            l1: net.spec.l1,
            halfkp: net.is_halfkp(),
            current: 0,
            accumulations: AlignedBox::new_zeroed(STACK_CAPACITY * 2 * net.spec.l1),
            computed: vec![false; STACK_CAPACITY],
            dirty: (0..STACK_CAPACITY).map(|_| DirtyPiece::default()).collect(),
            ft_raw: AlignedBox::new_zeroed(2 * net.spec.l1),
            ft_activated: AlignedBox::new_zeroed(net.l1.padded_input),
            layer_i32: AlignedBox::new_zeroed(net.spec.l2),
            layer_u8: AlignedBox::new_zeroed(padded_input(net.spec.l2)),
            layer2_i32: AlignedBox::new_zeroed(net.spec.l3),
            layer2_u8: AlignedBox::new_zeroed(padded_input(net.spec.l3)),
        }
    }

    pub(crate) fn matches_network(&self, net: &DynamicHalfKxNetwork) -> bool {
        self.spec == net.spec
    }
    pub(crate) fn is_halfkp(&self) -> bool {
        self.halfkp
    }
    pub(crate) fn reset(&mut self) {
        self.current = 0;
        self.computed[0] = false;
        self.dirty[0].clear();
    }
    pub(crate) fn push(&mut self, dirty: DirtyPiece) {
        assert!(self.current + 1 < STACK_CAPACITY, "dynamic NNUE accumulator stack overflow");
        self.current += 1;
        self.computed[self.current] = false;
        self.dirty[self.current] = dirty;
    }
    pub(crate) fn pop(&mut self) {
        if self.current > 0 {
            self.current -= 1;
        }
    }
    fn current_accumulation_mut(&mut self, perspective: Color) -> &mut [i16] {
        let start = (self.current * 2 + perspective.index()) * self.l1;
        &mut self.accumulations[start..start + self.l1]
    }
}

fn activate_i16(kind: Activation, input: &[i16], output: &mut [u8], qa: i16) {
    match kind {
        Activation::CReLU => CReLU::activate_i16_to_u8(input, output, qa),
        Activation::SCReLU => SCReLU::activate_i16_to_u8(input, output, qa),
        Activation::PairwiseCReLU => PairwiseCReLU::activate_i16_to_u8(input, output, qa),
    }
}

fn activate_i32(kind: Activation, input: &[i32], output: &mut [u8]) {
    match kind {
        Activation::CReLU => CReLU::activate_i32_to_u8(input, output),
        Activation::SCReLU => SCReLU::activate_i32_to_u8(input, output),
        Activation::PairwiseCReLU => PairwiseCReLU::activate_i32_to_u8(input, output),
    }
}

#[inline]
fn add_row(acc: &mut [i16], row: &[i16]) {
    #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
    // SAFETY: `chunks` is bounded by both slices; each v128 access covers eight in-range
    // i16 elements and the scalar tail starts immediately after the last vector.
    unsafe {
        use std::arch::wasm32::*;
        let chunks = acc.len().min(row.len()) / 8;
        for i in 0..chunks {
            let offset = i * 8;
            let a = v128_load(acc.as_ptr().add(offset).cast::<v128>());
            let b = v128_load(row.as_ptr().add(offset).cast::<v128>());
            v128_store(acc.as_mut_ptr().add(offset).cast::<v128>(), i16x8_add(a, b));
        }
        for (a, &w) in acc[chunks * 8..].iter_mut().zip(&row[chunks * 8..]) {
            *a = a.wrapping_add(w);
        }
        return;
    }
    #[allow(unreachable_code)]
    for (a, &w) in acc.iter_mut().zip(row) {
        *a = a.wrapping_add(w);
    }
}

#[inline]
fn sub_row(acc: &mut [i16], row: &[i16]) {
    #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
    // SAFETY: `chunks` is bounded by both slices; each v128 access covers eight in-range
    // i16 elements and the scalar tail starts immediately after the last vector.
    unsafe {
        use std::arch::wasm32::*;
        let chunks = acc.len().min(row.len()) / 8;
        for i in 0..chunks {
            let offset = i * 8;
            let a = v128_load(acc.as_ptr().add(offset).cast::<v128>());
            let b = v128_load(row.as_ptr().add(offset).cast::<v128>());
            v128_store(acc.as_mut_ptr().add(offset).cast::<v128>(), i16x8_sub(a, b));
        }
        for (a, &w) in acc[chunks * 8..].iter_mut().zip(&row[chunks * 8..]) {
            *a = a.wrapping_sub(w);
        }
        return;
    }
    #[allow(unreachable_code)]
    for (a, &w) in acc.iter_mut().zip(row) {
        *a = a.wrapping_sub(w);
    }
}

fn parse_qa(arch: &str) -> Option<i16> {
    arch.split(',').find_map(|part| part.strip_prefix("qa=")?.parse().ok())
}

pub(crate) fn validate_dimension(name: &str, value: usize) -> io::Result<()> {
    if value == 0 || value > MAX_RUNTIME_DIMENSION {
        Err(invalid_data(format!("invalid runtime {name} dimension: {value}")))
    } else {
        Ok(())
    }
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::io::BufReader;

    use super::*;
    use crate::nnue::accumulator_stack_variant::AccumulatorStackVariant;
    use crate::nnue::halfkp::{HalfKPNetwork, HalfKPStack};
    use crate::nnue::net_delta::{NetCoefficientId, NetDelta, NetTensorKind};
    use crate::nnue::network::NNUENetwork;
    use crate::position::SFEN_HIRATE;

    fn zero_affine(input_dim: usize, output_dim: usize) -> DynamicAffine {
        DynamicAffine {
            input_dim,
            padded_input: padded_input(input_dim),
            output_dim,
            biases: AlignedBox::new_zeroed(output_dim),
            weights: AlignedBox::new_zeroed(output_dim * padded_input(input_dim)),
        }
    }

    #[test]
    fn dynamic_affine_file_edit_matches_delta() {
        for (input_dim, output_dim) in [(30, 32), (32, 1)] {
            let weight_len = output_dim * padded_input(input_dim);
            let file_index = weight_len - 1;
            let mut edited_bytes = vec![0; output_dim * std::mem::size_of::<i32>() + weight_len];
            edited_bytes[output_dim * std::mem::size_of::<i32>() + file_index] = 19;
            let edited =
                DynamicAffine::read(&mut std::io::Cursor::new(edited_bytes), input_dim, output_dim)
                    .expect("edited affine");

            let mut delta = DynamicAffine::read(
                &mut std::io::Cursor::new(vec![
                    0;
                    output_dim * std::mem::size_of::<i32>() + weight_len
                ]),
                input_dim,
                output_dim,
            )
            .expect("base affine");
            assert!(!delta.apply_file_weight_delta(file_index, 19));
            assert_eq!(&*delta.biases, &*edited.biases);
            assert_eq!(&*delta.weights, &*edited.weights);

            delta.weights[file_index] = i8::MAX;
            assert!(delta.apply_file_weight_delta(file_index, 1));
            assert_eq!(delta.file_weight(file_index), i8::MAX);
        }
    }

    fn test_network(spec: ArchitectureSpec) -> DynamicHalfKxNetwork {
        let feature_set = RuntimeFeatureSet::from_spec(spec.feature_set).unwrap();
        let dense_input = 2 * spec.l1 / spec.activation.output_dim_divisor();
        DynamicHalfKxNetwork {
            spec,
            feature_set,
            input_dimensions: feature_set.dimensions(),
            activation: spec.activation,
            ft_biases: AlignedBox::new_zeroed(spec.l1),
            ft_weights: AlignedBox::new_zeroed(0),
            l1: zero_affine(dense_input, spec.l2),
            l2: zero_affine(spec.l2, spec.l3),
            output: zero_affine(spec.l3, 1),
            fv_scale: FV_SCALE,
            qa: 127,
        }
    }

    #[test]
    fn net_delta_rejects_dynamic_halfkx() {
        let spec = ArchitectureSpec::new(FeatureSet::HalfKP, 32, 4, 2, Activation::CReLU);
        let mut network = NNUENetwork::DynamicHalfKx(Box::new(test_network(spec)));
        let error = network
            .apply_net_deltas(&[NetDelta {
                id: NetCoefficientId {
                    kind: NetTensorKind::FtBias,
                    bucket: None,
                    index: 0,
                },
                delta: 1,
            }])
            .expect_err("HalfKX must reject net deltas");
        assert!(error.to_string().contains("unsupported architecture"));
    }

    #[test]
    fn stack_is_rebuilt_for_every_runtime_architecture_identity_change() {
        let base_spec = ArchitectureSpec::new(FeatureSet::HalfKP, 8, 4, 3, Activation::CReLU);
        let base = NNUENetwork::DynamicHalfKx(Box::new(test_network(base_spec)));
        let stack = AccumulatorStackVariant::from_network(&base);
        assert!(stack.matches_network(&base));

        for switched_spec in [
            ArchitectureSpec::new(FeatureSet::HalfKP, 10, 4, 3, Activation::CReLU),
            ArchitectureSpec::new(FeatureSet::HalfKP, 8, 5, 3, Activation::CReLU),
            ArchitectureSpec::new(FeatureSet::HalfKP, 8, 4, 5, Activation::CReLU),
            ArchitectureSpec::new(FeatureSet::HalfKP, 8, 4, 3, Activation::SCReLU),
            ArchitectureSpec::new(FeatureSet::HalfKP, 8, 4, 3, Activation::PairwiseCReLU),
            ArchitectureSpec::new(FeatureSet::HalfKaHmMerged, 8, 4, 3, Activation::CReLU),
        ] {
            let switched = NNUENetwork::DynamicHalfKx(Box::new(test_network(switched_spec)));
            assert!(!stack.matches_network(&switched), "switch to {switched_spec:?}");
            assert!(
                AccumulatorStackVariant::from_network(&switched).matches_network(&switched),
                "replacement stack for {switched_spec:?}"
            );
        }
    }

    /// 実ファイルを dynamic / const-generic の両方で読み、評価値を比較する。
    ///
    /// `NNUE_DYNAMIC_COMPARE_FILE` に既知形状の HalfKP net を指定して実行する。
    #[test]
    #[ignore]
    fn real_halfkp_matches_const_generic() {
        let path = std::env::var("NNUE_DYNAMIC_COMPARE_FILE")
            .expect("set NNUE_DYNAMIC_COMPARE_FILE to a HalfKP NNUE file");

        let mut dynamic_reader = BufReader::new(File::open(&path).unwrap());
        let dynamic = DynamicHalfKxNetwork::read(&mut dynamic_reader, None).unwrap();
        assert_eq!(dynamic.spec.feature_set, FeatureSet::HalfKP);

        let mut static_reader = BufReader::new(File::open(&path).unwrap());
        let static_net = HalfKPNetwork::read(
            &mut static_reader,
            dynamic.spec.l1,
            dynamic.spec.l2,
            dynamic.spec.l3,
            dynamic.spec.activation,
        )
        .unwrap();

        for sfen in [
            SFEN_HIRATE,
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL w - 2",
            "lnsgkgsnl/1r5b1/pppppp1pp/6p2/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL b - 3",
        ] {
            let mut pos = Position::new();
            pos.set_sfen(sfen).unwrap();
            // 各局面で双方を fresh に構築し、他局面の accumulator を流用しない。
            let mut dynamic_stack = DynamicHalfKxStack::new(&dynamic);
            dynamic.refresh(&pos, &mut dynamic_stack);
            let dynamic_value = dynamic.evaluate(&pos, &mut dynamic_stack);

            let mut static_stack = HalfKPStack::from_network(&static_net);
            static_net.refresh_accumulator(&pos, &mut static_stack);
            let static_value = static_net.evaluate(&pos, &static_stack);
            assert_eq!(dynamic_value, static_value, "{sfen}");
            println!("fresh HalfKP: {sfen}: {static_value:?}");
        }
    }
}
