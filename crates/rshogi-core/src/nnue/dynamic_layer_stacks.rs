//! Runtime-dimension LayerStacks inference for `edition-universal`.

use std::io::{self, Read, Seek, SeekFrom};

use super::accumulator::{
    AlignedBox, DirtyPiece, IndexList, MAX_ACTIVE_FEATURES, MAX_CHANGED_FEATURES,
};
use super::bona_piece::{BonaPiece, FE_END};
use super::bona_piece_effect_bucket::EffectBucketConfig;
use super::constants::{
    DEFAULT_NUM_BUCKETS, FV_SCALE_HALFKA, MAX_ARCH_LEN, MAX_LAYER_STACK_BUCKETS,
    NNUE_VERSION_HALFKA, NNUE_VERSION_LAYERSTACK_NUM_BUCKETS,
};
use super::dynamic_halfkx::DynamicAffine;
use super::effect_bucket_features::append_active_effect_bucket;
use super::features::{
    FeatureSet as FeatureSetTrait, HalfKPFeatureSet, HalfKaHmMergedFeatureSet,
    HalfKaHmSplitFeatureSet, HalfKaMergedFeatureSet, HalfKaSplitFeatureSet,
};
use super::layers::padded_input;
use super::leb128::read_layer_stacks_ft_i16;
use super::ls_feature_spec::{
    HalfKaHmMergedSpec, HalfKaHmSplitSpec, HalfKaMergedSpec, HalfKaSplitSpec, HalfKpSpec,
    LsFeatureSpec,
};
use super::net_delta::{NetCoefficientId, NetTensorKind, NetTensorShape, add_i16_delta};
use super::network::{
    LayerStackBucketMode, compute_layer_stack_progresskpabs_bucket_index, get_fv_scale_override,
    get_layer_stack_bucket_mode, get_layer_stack_progress_buckets,
    get_layer_stack_progress_kpabs_weights, parse_fv_scale_from_arch,
};
use super::network_layer_stacks::compute_layer_stack_kingrank9_bucket_index;
use super::piece_list::PieceNumber;
use super::spec::{
    Activation, ArchitectureSpec, FeatureSet, detect_layer_stacks_feature, parse_arch_dimensions,
    parse_effect_bucket_config, parse_feature_input_dimensions,
    validate_layer_stacks_architecture_header, validate_layer_stacks_dimensions,
};
use super::stats::{count_refresh, count_update};
use crate::position::Position;
use crate::types::{Color, Square, Value};

const STACK_CAPACITY: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeLsFeature {
    HalfKP,
    HalfKaHmMerged,
    HalfKaSplit,
    HalfKaMerged,
    HalfKaHmSplit,
    EffectBucket(EffectBucketConfig),
}

impl RuntimeLsFeature {
    fn dimensions(self) -> usize {
        match self {
            Self::HalfKP => HalfKPFeatureSet::DIMENSIONS,
            Self::HalfKaHmMerged => HalfKaHmMergedFeatureSet::DIMENSIONS,
            Self::HalfKaSplit => HalfKaSplitFeatureSet::DIMENSIONS,
            Self::HalfKaMerged => HalfKaMergedFeatureSet::DIMENSIONS,
            Self::HalfKaHmSplit => HalfKaHmSplitFeatureSet::DIMENSIONS,
            Self::EffectBucket(config) => config.dimensions(),
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
            Self::EffectBucket(config) => {
                let mut active = IndexList::new();
                append_active_effect_bucket(pos, config, perspective, &mut active);
                active
            }
        }
    }

    fn changed(
        self,
        dirty: &DirtyPiece,
        perspective: Color,
        pos: &Position,
    ) -> Option<(IndexList<MAX_CHANGED_FEATURES>, IndexList<MAX_CHANGED_FEATURES>)> {
        let king_sq = pos.king_square(perspective);
        match self {
            Self::HalfKP => {
                Some(HalfKPFeatureSet::collect_changed_indices(dirty, perspective, king_sq))
            }
            Self::HalfKaHmMerged => {
                Some(HalfKaHmMergedFeatureSet::collect_changed_indices(dirty, perspective, king_sq))
            }
            Self::HalfKaSplit => {
                Some(HalfKaSplitFeatureSet::collect_changed_indices(dirty, perspective, king_sq))
            }
            Self::HalfKaMerged => {
                Some(HalfKaMergedFeatureSet::collect_changed_indices(dirty, perspective, king_sq))
            }
            Self::HalfKaHmSplit => {
                Some(HalfKaHmSplitFeatureSet::collect_changed_indices(dirty, perspective, king_sq))
            }
            Self::EffectBucket(_) => None,
        }
    }

    fn needs_refresh(self, dirty: &DirtyPiece, perspective: Color) -> bool {
        match self {
            Self::HalfKP => HalfKPFeatureSet::needs_refresh(dirty, perspective),
            Self::HalfKaHmMerged => HalfKaHmMergedFeatureSet::needs_refresh(dirty, perspective),
            Self::HalfKaSplit => HalfKaSplitFeatureSet::needs_refresh(dirty, perspective),
            Self::HalfKaMerged => HalfKaMergedFeatureSet::needs_refresh(dirty, perspective),
            Self::HalfKaHmSplit => HalfKaHmSplitFeatureSet::needs_refresh(dirty, perspective),
            Self::EffectBucket(_) => true,
        }
    }

    fn includes_king_in_piece_list(self) -> bool {
        !matches!(self, Self::HalfKP)
    }

    fn feature_index(self, bp: BonaPiece, perspective: Color, king_sq: Square) -> Option<usize> {
        match self {
            Self::HalfKP => Some(HalfKpSpec::feature_index(bp, perspective, king_sq)),
            Self::HalfKaHmMerged => {
                Some(HalfKaHmMergedSpec::feature_index(bp, perspective, king_sq))
            }
            Self::HalfKaSplit => Some(HalfKaSplitSpec::feature_index(bp, perspective, king_sq)),
            Self::HalfKaMerged => Some(HalfKaMergedSpec::feature_index(bp, perspective, king_sq)),
            Self::HalfKaHmSplit => Some(HalfKaHmSplitSpec::feature_index(bp, perspective, king_sq)),
            Self::EffectBucket(_) => None,
        }
    }
}

struct DynamicLayerStacksCache {
    accumulations: AlignedBox<i16>,
    psqt: AlignedBox<i32>,
    piece_lists: Box<[[BonaPiece; PieceNumber::NB]]>,
    valid: Box<[bool]>,
}

impl DynamicLayerStacksCache {
    fn new(l1: usize, num_buckets: usize, has_psqt: bool) -> Self {
        let entries = Square::NUM * Color::NUM;
        Self {
            accumulations: AlignedBox::new_zeroed(entries * l1),
            psqt: AlignedBox::new_zeroed(entries * num_buckets * usize::from(has_psqt)),
            piece_lists: vec![[BonaPiece::ZERO; PieceNumber::NB]; entries].into_boxed_slice(),
            valid: vec![false; entries].into_boxed_slice(),
        }
    }

    #[inline]
    fn entry_index(king_sq: Square, perspective: Color) -> usize {
        king_sq.raw() as usize * Color::NUM + perspective.index()
    }

    fn clear(&mut self) {
        self.valid.fill(false);
    }
}

struct DynamicLsBucket {
    l1: DynamicAffine,
    l2: DynamicAffine,
    output: DynamicAffine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DynamicLayerStacksSignature {
    feature: RuntimeLsFeature,
    l1: usize,
    l2: usize,
    l3: usize,
    num_buckets: usize,
    has_psqt: bool,
    threat_dimensions: usize,
}

/// Runtime-dimension LayerStacks network.
pub struct DynamicLayerStacksNetwork {
    spec: ArchitectureSpec,
    feature: RuntimeLsFeature,
    input_dimensions: usize,
    num_buckets: usize,
    ft_biases: AlignedBox<i16>,
    ft_weights: AlignedBox<i16>,
    psqt_biases: AlignedBox<i32>,
    psqt_weights: AlignedBox<i32>,
    threat_dimensions: usize,
    threat_weights: AlignedBox<i8>,
    buckets: Vec<DynamicLsBucket>,
    fv_scale: i32,
}

impl DynamicLayerStacksNetwork {
    pub(crate) fn read<R: Read + Seek>(
        reader: &mut R,
        psqt_override: Option<bool>,
    ) -> io::Result<Self> {
        let file_size = reader.seek(SeekFrom::End(0))?;
        reader.seek(SeekFrom::Start(0))?;
        let mut buf4 = [0; 4];
        reader.read_exact(&mut buf4)?;
        let version = u32::from_le_bytes(buf4);
        if version != NNUE_VERSION_HALFKA && version != NNUE_VERSION_LAYERSTACK_NUM_BUCKETS {
            return Err(invalid("invalid LayerStacks NNUE version"));
        }
        reader.read_exact(&mut buf4)?;
        reader.read_exact(&mut buf4)?;
        let arch_len = u32::from_le_bytes(buf4) as usize;
        if arch_len == 0 || arch_len > MAX_ARCH_LEN {
            return Err(invalid("invalid architecture string length"));
        }
        let mut arch = vec![0; arch_len];
        reader.read_exact(&mut arch)?;
        let arch =
            std::str::from_utf8(&arch).map_err(|_| invalid("architecture string is not UTF-8"))?;
        let num_buckets = if version == NNUE_VERSION_LAYERSTACK_NUM_BUCKETS {
            reader.read_exact(&mut buf4)?;
            u32::from_le_bytes(buf4) as usize
        } else {
            DEFAULT_NUM_BUCKETS
        };
        if !(1..=MAX_LAYER_STACK_BUCKETS).contains(&num_buckets) {
            return Err(invalid(format!(
                "invalid LayerStacks num_buckets={num_buckets}; expected 1..={MAX_LAYER_STACK_BUCKETS}"
            )));
        }

        let (l1, l2, l3) = parse_arch_dimensions(arch);
        validate_layer_stacks_dimensions(l1, l2, l3).map_err(invalid)?;
        let threat_dimensions =
            validate_layer_stacks_architecture_header(arch).map_err(invalid)?.unwrap_or(0);
        #[cfg(feature = "nnue-threat")]
        if threat_dimensions != 0 && threat_dimensions != super::threat_features::THREAT_DIMENSIONS
        {
            return Err(invalid(format!(
                "Threat dimensions mismatch: model={threat_dimensions}, runtime={}",
                super::threat_features::THREAT_DIMENSIONS
            )));
        }
        #[cfg(not(feature = "nnue-threat"))]
        if threat_dimensions != 0 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Threat model requires nnue-threat",
            ));
        }
        let parsed_feature = detect_layer_stacks_feature(arch).map_err(invalid)?;
        let feature = match parsed_feature {
            FeatureSet::HalfKP => RuntimeLsFeature::HalfKP,
            FeatureSet::HalfKaHmMerged => RuntimeLsFeature::HalfKaHmMerged,
            FeatureSet::HalfKaSplit => RuntimeLsFeature::HalfKaSplit,
            FeatureSet::HalfKaMerged => RuntimeLsFeature::HalfKaMerged,
            FeatureSet::HalfKaHmSplit => RuntimeLsFeature::HalfKaHmSplit,
            FeatureSet::HalfKaHmMergedEffectBucket => {
                let config = parse_effect_bucket_config(arch)
                    .ok_or_else(|| invalid("malformed EffectBucket token"))?;
                RuntimeLsFeature::EffectBucket(config)
            }
            FeatureSet::LayerStacks => {
                return Err(invalid("LayerStacks header does not identify its FT feature set"));
            }
        };
        let input_dimensions = parse_ft_input_dimensions(arch, feature, threat_dimensions)?;

        reader.read_exact(&mut buf4)?;
        let weight_len = input_dimensions
            .checked_mul(l1)
            .ok_or_else(|| invalid("FT dimensions overflow"))?;
        let mut ft_biases = AlignedBox::new_zeroed(l1);
        let ft_weights =
            read_layer_stacks_ft_i16(reader, &mut ft_biases, weight_len, AlignedBox::new_zeroed)?;

        let has_psqt = psqt_override.unwrap_or_else(|| arch.contains("PSQT="));
        let mut psqt_biases = AlignedBox::new_zeroed(if has_psqt { num_buckets } else { 0 });
        let mut psqt_weights = AlignedBox::new_zeroed(if has_psqt {
            input_dimensions * num_buckets
        } else {
            0
        });
        if has_psqt {
            read_i32s(reader, &mut psqt_biases)?;
            read_i32s(reader, &mut psqt_weights)?;
        }

        if arch.contains("ThreatProfile=") {
            reader.read_exact(&mut buf4)?;
            if u32::from_le_bytes(buf4) != 0 {
                return Err(invalid(
                    "runtime threat profiles other than profile 0 are not yet supported",
                ));
            }
        }
        let mut threat_weights: AlignedBox<i8> = AlignedBox::new_zeroed(
            threat_dimensions
                .checked_mul(l1)
                .ok_or_else(|| invalid("Threat dimensions overflow"))?,
        );
        if !threat_weights.is_empty() {
            // SAFETY: `i8` and `u8` have identical size/alignment, and the slice retains the
            // exact allocation length while only its byte signedness changes for `Read`.
            let bytes = unsafe {
                std::slice::from_raw_parts_mut(
                    threat_weights.as_mut_ptr().cast::<u8>(),
                    threat_weights.len(),
                )
            };
            reader.read_exact(bytes)?;
        }

        let mut buckets = Vec::with_capacity(num_buckets);
        for _ in 0..num_buckets {
            reader.read_exact(&mut buf4)?; // per-bucket FC hash
            buckets.push(DynamicLsBucket {
                l1: DynamicAffine::read(reader, l1, l2)?,
                l2: DynamicAffine::read(reader, 2 * (l2 - 1), l3)?,
                output: DynamicAffine::read(reader, l3, 1)?,
            });
        }
        let consumed = reader.stream_position()?;
        if consumed != file_size {
            return Err(invalid(format!(
                "unexpected trailing LayerStacks data: consumed={consumed}, file_size={file_size}"
            )));
        }
        Ok(Self {
            // Keep public introspection compatible with the const-generic LayerStacks path;
            // the concrete FT remains in `feature` for runtime dispatch.
            spec: ArchitectureSpec::new(FeatureSet::LayerStacks, l1, l2, l3, Activation::CReLU),
            feature,
            input_dimensions,
            num_buckets,
            ft_biases,
            ft_weights,
            psqt_biases,
            psqt_weights,
            threat_dimensions,
            threat_weights,
            buckets,
            fv_scale: parse_fv_scale_from_arch(arch).unwrap_or(FV_SCALE_HALFKA),
        })
    }

    pub(crate) fn spec(&self) -> ArchitectureSpec {
        self.spec
    }
    pub(crate) fn num_buckets(&self) -> usize {
        self.num_buckets
    }

    pub(crate) fn net_tensor_shape(&self, kind: NetTensorKind) -> NetTensorShape {
        match kind {
            NetTensorKind::OutputWeight => NetTensorShape {
                bucket_count: Some(self.num_buckets),
                element_count: self.buckets[0].output.weight_len(),
            },
            NetTensorKind::OutputBias => NetTensorShape {
                bucket_count: Some(self.num_buckets),
                element_count: 1,
            },
            NetTensorKind::FtBias => NetTensorShape {
                bucket_count: None,
                element_count: self.ft_biases.len(),
            },
            NetTensorKind::L2Weight => NetTensorShape {
                bucket_count: Some(self.num_buckets),
                element_count: self.buckets[0].l2.weight_len(),
            },
        }
    }

    pub(crate) fn net_coefficient(&self, id: &NetCoefficientId) -> i32 {
        match id.kind {
            NetTensorKind::OutputWeight => i32::from(
                self.buckets[id.bucket.expect("validated bucket")].output.file_weight(id.index),
            ),
            NetTensorKind::OutputBias => {
                self.buckets[id.bucket.expect("validated bucket")].output.bias(0)
            }
            NetTensorKind::FtBias => i32::from(self.ft_biases[id.index]),
            NetTensorKind::L2Weight => i32::from(
                self.buckets[id.bucket.expect("validated bucket")].l2.file_weight(id.index),
            ),
        }
    }

    pub(crate) fn apply_net_delta(&mut self, id: &NetCoefficientId, delta: i32) -> bool {
        match id.kind {
            NetTensorKind::OutputWeight => self.buckets[id.bucket.expect("validated bucket")]
                .output
                .apply_file_weight_delta(id.index, delta),
            NetTensorKind::OutputBias => self.buckets[id.bucket.expect("validated bucket")]
                .output
                .apply_bias_delta(0, delta),
            NetTensorKind::FtBias => {
                let (value, clamped) = add_i16_delta(self.ft_biases[id.index], delta);
                self.ft_biases[id.index] = value;
                clamped
            }
            NetTensorKind::L2Weight => self.buckets[id.bucket.expect("validated bucket")]
                .l2
                .apply_file_weight_delta(id.index, delta),
        }
    }

    pub(crate) fn requires_board_effects(&self) -> bool {
        matches!(self.feature, RuntimeLsFeature::EffectBucket(_))
    }
    pub(crate) fn new_stack(&self) -> DynamicLayerStacksStack {
        DynamicLayerStacksStack::new(self)
    }
    fn stack_signature(&self) -> DynamicLayerStacksSignature {
        DynamicLayerStacksSignature {
            feature: self.feature,
            l1: self.spec.l1,
            l2: self.spec.l2,
            l3: self.spec.l3,
            num_buckets: self.num_buckets,
            has_psqt: !self.psqt_weights.is_empty(),
            threat_dimensions: self.threat_dimensions,
        }
    }

    pub(crate) fn refresh(&self, pos: &Position, stack: &mut DynamicLayerStacksStack) {
        let current = stack.current;
        for perspective in [Color::Black, Color::White] {
            self.refresh_perspective(pos, perspective, stack);
        }
        stack.computed[current] = true;
    }

    fn refresh_perspective(
        &self,
        pos: &Position,
        perspective: Color,
        stack: &mut DynamicLayerStacksStack,
    ) {
        count_refresh!();
        let current = stack.current;
        let p = perspective.index();
        let start = (current * 2 + p) * self.spec.l1;
        let psqt_start = (current * 2 + p) * self.num_buckets;

        if matches!(self.feature, RuntimeLsFeature::EffectBucket(_)) {
            let acc = &mut stack.accumulations[start..start + self.spec.l1];
            acc.copy_from_slice(&self.ft_biases);
            let psqt = &mut stack.psqt[psqt_start..psqt_start + self.num_buckets];
            if self.psqt_biases.is_empty() {
                psqt.fill(0);
            } else {
                psqt.copy_from_slice(&self.psqt_biases);
            }
            for index in self.feature.active(pos, perspective).iter() {
                add_i16(acc, self.ft_row(index));
                if !self.psqt_weights.is_empty() {
                    add_i32(
                        psqt,
                        &self.psqt_weights
                            [index * self.num_buckets..(index + 1) * self.num_buckets],
                    );
                }
            }
            self.refresh_threat(pos, perspective, stack);
            return;
        }

        let king_sq = pos.king_square(perspective);
        let raw_piece_list = if perspective == Color::Black {
            pos.piece_list().piece_list_fb()
        } else {
            pos.piece_list().piece_list_fw()
        };
        let piece_list_owned;
        let piece_list = if self.feature.includes_king_in_piece_list() {
            raw_piece_list
        } else {
            piece_list_owned = {
                let mut pieces = *raw_piece_list;
                pieces[PieceNumber::KING as usize] = BonaPiece::ZERO;
                pieces[(PieceNumber::KING + 1) as usize] = BonaPiece::ZERO;
                pieces
            };
            &piece_list_owned
        };

        let entry = DynamicLayerStacksCache::entry_index(king_sq, perspective);
        let cache_acc_start = entry * self.spec.l1;
        let cache_psqt_start = entry * self.num_buckets;
        let DynamicLayerStacksStack {
            accumulations,
            psqt,
            cache,
            ..
        } = stack;
        let acc = &mut accumulations[start..start + self.spec.l1];
        let current_psqt = &mut psqt[psqt_start..psqt_start + self.num_buckets];

        if cache.valid[entry] {
            super::stats::count_cache_hit!();
            acc.copy_from_slice(
                &cache.accumulations[cache_acc_start..cache_acc_start + self.spec.l1],
            );
            if !self.psqt_weights.is_empty() {
                current_psqt.copy_from_slice(
                    &cache.psqt[cache_psqt_start..cache_psqt_start + self.num_buckets],
                );
            }

            let mut diff_count = 0;
            for (&cached, &current) in cache.piece_lists[entry].iter().zip(piece_list) {
                if cached == current {
                    continue;
                }
                let cached_index = (cached != BonaPiece::ZERO).then(|| {
                    self.feature
                        .feature_index(cached, perspective, king_sq)
                        .expect("non-EffectBucket feature")
                });
                let current_index = (current != BonaPiece::ZERO).then(|| {
                    self.feature
                        .feature_index(current, perspective, king_sq)
                        .expect("non-EffectBucket feature")
                });
                match (cached_index, current_index) {
                    (Some(sub), Some(add)) => sub_add_i16(acc, self.ft_row(sub), self.ft_row(add)),
                    (Some(sub), None) => sub_i16(acc, self.ft_row(sub)),
                    (None, Some(add)) => add_i16(acc, self.ft_row(add)),
                    (None, None) => {}
                }
                if !self.psqt_weights.is_empty() {
                    if let Some(index) = cached_index {
                        sub_i32(
                            current_psqt,
                            &self.psqt_weights
                                [index * self.num_buckets..(index + 1) * self.num_buckets],
                        );
                    }
                    if let Some(index) = current_index {
                        add_i32(
                            current_psqt,
                            &self.psqt_weights
                                [index * self.num_buckets..(index + 1) * self.num_buckets],
                        );
                    }
                }
                if cached_index.is_some() {
                    diff_count += 1;
                }
                if current_index.is_some() {
                    diff_count += 1;
                }
            }
            super::stats::count_refresh_diff!(diff_count);
        } else {
            super::stats::count_cache_miss!();
            acc.copy_from_slice(&self.ft_biases);
            if self.psqt_biases.is_empty() {
                current_psqt.fill(0);
            } else {
                current_psqt.copy_from_slice(&self.psqt_biases);
            }
            for &bp in piece_list {
                if bp == BonaPiece::ZERO {
                    continue;
                }
                let index = self
                    .feature
                    .feature_index(bp, perspective, king_sq)
                    .expect("non-EffectBucket feature");
                add_i16(acc, self.ft_row(index));
                if !self.psqt_weights.is_empty() {
                    add_i32(
                        current_psqt,
                        &self.psqt_weights
                            [index * self.num_buckets..(index + 1) * self.num_buckets],
                    );
                }
            }
        }

        cache.accumulations[cache_acc_start..cache_acc_start + self.spec.l1].copy_from_slice(acc);
        if !self.psqt_weights.is_empty() {
            cache.psqt[cache_psqt_start..cache_psqt_start + self.num_buckets]
                .copy_from_slice(current_psqt);
        }
        cache.piece_lists[entry].copy_from_slice(piece_list);
        cache.valid[entry] = true;
        self.refresh_threat(pos, perspective, stack);
    }

    fn refresh_threat(
        &self,
        pos: &Position,
        perspective: Color,
        stack: &mut DynamicLayerStacksStack,
    ) {
        if self.threat_dimensions == 0 {
            return;
        }
        let start = (stack.current * 2 + perspective.index()) * self.spec.l1;
        let threat = &mut stack.threat_accumulations[start..start + self.spec.l1];
        threat.fill(0);
        #[cfg(feature = "nnue-threat")]
        super::threat_features::for_each_active_threat_index(
            pos,
            perspective,
            pos.king_square(perspective),
            |idx| {
                let row = &self.threat_weights[idx * self.spec.l1..(idx + 1) * self.spec.l1];
                for (a, &w) in threat.iter_mut().zip(row) {
                    *a = a.wrapping_add(i16::from(w));
                }
            },
        );
    }

    pub(crate) fn ensure(&self, pos: &Position, stack: &mut DynamicLayerStacksStack) {
        if stack.computed[stack.current] {
            return;
        }
        if stack.current == 0 {
            self.refresh(pos, stack);
            return;
        }

        if stack.computed[stack.current - 1] {
            self.update_from_previous(pos, stack);
            return;
        }

        if self.threat_dimensions == 0
            && !matches!(self.feature, RuntimeLsFeature::EffectBucket(_))
            && let Some(source) = stack.find_usable_accumulator(4)
            && self.forward_update(pos, stack, source)
        {
            return;
        }

        self.refresh(pos, stack);
    }

    fn update_from_previous(&self, pos: &Position, stack: &mut DynamicLayerStacksStack) {
        let current = stack.current;
        let dirty = stack.dirty[current];
        for perspective in [Color::Black, Color::White] {
            if self.feature.needs_refresh(&dirty, perspective) {
                self.refresh_perspective(pos, perspective, stack);
                continue;
            }
            count_update!();
            let p = perspective.index();
            let l1 = self.spec.l1;
            let prev_start = ((current - 1) * 2 + p) * l1;
            let curr_start = (current * 2 + p) * l1;
            let (before, after) = stack.accumulations.split_at_mut(curr_start);
            let prev = &before[prev_start..prev_start + l1];
            let curr = &mut after[..l1];
            curr.copy_from_slice(prev);
            let fast_applied = self.try_apply_dirty_piece_fast(
                curr,
                &dirty,
                perspective,
                pos.king_square(perspective),
            );
            let changes = if !fast_applied || !self.psqt_weights.is_empty() {
                self.feature.changed(&dirty, perspective, pos)
            } else {
                None
            };
            if !fast_applied {
                let (removed, added) = changes.as_ref().expect("non-EffectBucket feature");
                self.apply_feature_changes(curr, removed, added);
            }

            let prev_psqt_start = ((current - 1) * 2 + p) * self.num_buckets;
            let curr_psqt_start = (current * 2 + p) * self.num_buckets;
            let (before, after) = stack.psqt.split_at_mut(curr_psqt_start);
            let prev = &before[prev_psqt_start..prev_psqt_start + self.num_buckets];
            let curr = &mut after[..self.num_buckets];
            curr.copy_from_slice(prev);
            if !self.psqt_weights.is_empty() {
                let (removed, added) = changes.as_ref().expect("non-EffectBucket feature");
                self.apply_psqt_changes(curr, removed, added);
            }
            self.refresh_threat(pos, perspective, stack);
        }
        stack.computed[current] = true;
    }

    fn forward_update(
        &self,
        pos: &Position,
        stack: &mut DynamicLayerStacksStack,
        source: usize,
    ) -> bool {
        let current = stack.current;
        debug_assert!(source < current);
        for perspective in [Color::Black, Color::White] {
            let p = perspective.index();
            let l1 = self.spec.l1;
            let source_start = (source * 2 + p) * l1;
            let current_start = (current * 2 + p) * l1;
            let (before, after) = stack.accumulations.split_at_mut(current_start);
            let source_acc = &before[source_start..source_start + l1];
            let current_acc = &mut after[..l1];
            current_acc.copy_from_slice(source_acc);

            let source_psqt_start = (source * 2 + p) * self.num_buckets;
            let current_psqt_start = (current * 2 + p) * self.num_buckets;
            let (before, after) = stack.psqt.split_at_mut(current_psqt_start);
            let source_psqt = &before[source_psqt_start..source_psqt_start + self.num_buckets];
            let current_psqt = &mut after[..self.num_buckets];
            current_psqt.copy_from_slice(source_psqt);

            for index in source + 1..=current {
                let dirty = stack.dirty[index];
                count_update!();
                let fast_applied = self.try_apply_dirty_piece_fast(
                    current_acc,
                    &dirty,
                    perspective,
                    pos.king_square(perspective),
                );
                let changes = if !fast_applied || !self.psqt_weights.is_empty() {
                    self.feature.changed(&dirty, perspective, pos)
                } else {
                    None
                };
                if !fast_applied {
                    let Some((removed, added)) = changes.as_ref() else {
                        return false;
                    };
                    self.apply_feature_changes(current_acc, removed, added);
                }
                if !self.psqt_weights.is_empty() {
                    let Some((removed, added)) = changes.as_ref() else {
                        return false;
                    };
                    self.apply_psqt_changes(current_psqt, removed, added);
                }
            }
        }
        stack.computed[current] = true;
        true
    }

    #[inline]
    fn apply_feature_changes(
        &self,
        accumulation: &mut [i16],
        removed: &IndexList<MAX_CHANGED_FEATURES>,
        added: &IndexList<MAX_CHANGED_FEATURES>,
    ) {
        match (removed.len(), added.len()) {
            (1, 1) => {
                sub_add_i16(accumulation, self.ft_row(removed.get(0)), self.ft_row(added.get(0)))
            }
            (2, 2) => double_sub_add_i16(
                accumulation,
                self.ft_row(removed.get(0)),
                self.ft_row(added.get(0)),
                self.ft_row(removed.get(1)),
                self.ft_row(added.get(1)),
            ),
            _ => {
                for index in removed.iter() {
                    sub_i16(accumulation, self.ft_row(index));
                }
                for index in added.iter() {
                    add_i16(accumulation, self.ft_row(index));
                }
            }
        }
    }

    #[inline]
    fn try_apply_dirty_piece_fast(
        &self,
        accumulation: &mut [i16],
        dirty: &DirtyPiece,
        perspective: Color,
        king_sq: Square,
    ) -> bool {
        if matches!(self.feature, RuntimeLsFeature::EffectBucket(_)) {
            return false;
        }

        let changed = &dirty.changed_piece;
        let old_new = |index: usize| {
            let entry = &changed[index];
            if perspective == Color::Black {
                (entry.old_piece.fb, entry.new_piece.fb)
            } else {
                (entry.old_piece.fw, entry.new_piece.fw)
            }
        };

        if !self.feature.includes_king_in_piece_list() {
            for entry in changed.iter().take(dirty.dirty_num as usize) {
                let (old, new) = if perspective == Color::Black {
                    (entry.old_piece.fb, entry.new_piece.fb)
                } else {
                    (entry.old_piece.fw, entry.new_piece.fw)
                };
                if old.value() as usize >= FE_END || new.value() as usize >= FE_END {
                    return false;
                }
            }
        }

        match dirty.dirty_num as usize {
            1 => {
                let (old, new) = old_new(0);
                if old == BonaPiece::ZERO || new == BonaPiece::ZERO {
                    return false;
                }
                let sub = self
                    .feature
                    .feature_index(old, perspective, king_sq)
                    .expect("non-EffectBucket feature");
                let add = self
                    .feature
                    .feature_index(new, perspective, king_sq)
                    .expect("non-EffectBucket feature");
                sub_add_i16(accumulation, self.ft_row(sub), self.ft_row(add));
                true
            }
            2 => {
                let (old0, new0) = old_new(0);
                let (old1, new1) = old_new(1);
                if [old0, new0, old1, new1].contains(&BonaPiece::ZERO) {
                    return false;
                }
                let sub0 = self
                    .feature
                    .feature_index(old0, perspective, king_sq)
                    .expect("non-EffectBucket feature");
                let add0 = self
                    .feature
                    .feature_index(new0, perspective, king_sq)
                    .expect("non-EffectBucket feature");
                let sub1 = self
                    .feature
                    .feature_index(old1, perspective, king_sq)
                    .expect("non-EffectBucket feature");
                let add1 = self
                    .feature
                    .feature_index(new1, perspective, king_sq)
                    .expect("non-EffectBucket feature");
                double_sub_add_i16(
                    accumulation,
                    self.ft_row(sub0),
                    self.ft_row(add0),
                    self.ft_row(sub1),
                    self.ft_row(add1),
                );
                true
            }
            _ => false,
        }
    }

    #[inline]
    fn apply_psqt_changes(
        &self,
        accumulation: &mut [i32],
        removed: &IndexList<MAX_CHANGED_FEATURES>,
        added: &IndexList<MAX_CHANGED_FEATURES>,
    ) {
        if self.psqt_weights.is_empty() {
            return;
        }
        for index in removed.iter() {
            sub_i32(
                accumulation,
                &self.psqt_weights[index * self.num_buckets..(index + 1) * self.num_buckets],
            );
        }
        for index in added.iter() {
            add_i32(
                accumulation,
                &self.psqt_weights[index * self.num_buckets..(index + 1) * self.num_buckets],
            );
        }
    }

    pub(crate) fn evaluate(&self, pos: &Position, stack: &mut DynamicLayerStacksStack) -> Value {
        let l1 = self.spec.l1;
        let base = stack.current * 2 * l1;
        let us = &stack.accumulations
            [base + pos.side_to_move().index() * l1..base + (pos.side_to_move().index() + 1) * l1];
        let them = &stack.accumulations[base + (!pos.side_to_move()).index() * l1
            ..base + ((!pos.side_to_move()).index() + 1) * l1];
        if self.threat_dimensions == 0 {
            sqr_transform_without_threat(us, them, &mut stack.transformed[..l1]);
        } else {
            let us_threat = &stack.threat_accumulations[base + pos.side_to_move().index() * l1
                ..base + (pos.side_to_move().index() + 1) * l1];
            let them_threat = &stack.threat_accumulations[base + (!pos.side_to_move()).index() * l1
                ..base + ((!pos.side_to_move()).index() + 1) * l1];
            sqr_transform_with_threat(
                us,
                us_threat,
                them,
                them_threat,
                &mut stack.transformed[..l1],
            );
        }
        let bucket_index = match get_layer_stack_bucket_mode() {
            LayerStackBucketMode::KingRank9 => compute_layer_stack_kingrank9_bucket_index(
                pos,
                pos.side_to_move(),
                self.num_buckets,
            ),
            LayerStackBucketMode::ProgressKPAbsQ16 => {
                super::progress_q16::configured_progress_q16_bucket(pos, self.num_buckets)
            }
            LayerStackBucketMode::ProgressKPAbs => {
                let routing_buckets = get_layer_stack_progress_buckets()
                    .expect("LayerStacks progress routing is not configured");
                assert!(
                    routing_buckets <= self.num_buckets,
                    "LayerStacks progress routing uses {routing_buckets} buckets, but the network stores only {}",
                    self.num_buckets
                );
                compute_layer_stack_progresskpabs_bucket_index(
                    pos,
                    pos.side_to_move(),
                    get_layer_stack_progress_kpabs_weights(),
                    routing_buckets,
                )
            }
        };
        let bucket = &self.buckets[bucket_index];
        bucket.l1.propagate(&stack.transformed, &mut stack.l1_out);
        let main_dim = self.spec.l2 - 1;
        let skip = stack.l1_out[main_dim];
        stack.l2_input.fill(0);
        for i in 0..main_dim {
            let v = stack.l1_out[i];
            stack.l2_input[i] = (((i64::from(v) * i64::from(v)) >> 19).clamp(0, 127)) as u8;
            stack.l2_input[main_dim + i] = (v >> 6).clamp(0, 127) as u8;
        }
        bucket.l2.propagate(&stack.l2_input, &mut stack.l2_out);
        stack.l3_input.fill(0);
        for (dst, &v) in stack.l3_input.iter_mut().zip(stack.l2_out.iter()) {
            *dst = (v >> 6).clamp(0, 127) as u8;
        }
        let mut output = [0];
        bucket.output.propagate(&stack.l3_input, &mut output);
        let psqt_base = stack.current * 2 * self.num_buckets;
        let psqt = if self.psqt_weights.is_empty() {
            0
        } else {
            (stack.psqt[psqt_base + pos.side_to_move().index() * self.num_buckets + bucket_index]
                - stack.psqt
                    [psqt_base + (!pos.side_to_move()).index() * self.num_buckets + bucket_index])
                / 2
        };
        Value::new(
            (output[0] + skip).saturating_add(psqt)
                / get_fv_scale_override().unwrap_or(self.fv_scale),
        )
    }

    fn ft_row(&self, index: usize) -> &[i16] {
        debug_assert!(index < self.input_dimensions);
        &self.ft_weights[index * self.spec.l1..(index + 1) * self.spec.l1]
    }
}

pub struct DynamicLayerStacksStack {
    signature: DynamicLayerStacksSignature,
    current: usize,
    accumulations: AlignedBox<i16>,
    threat_accumulations: AlignedBox<i16>,
    psqt: AlignedBox<i32>,
    computed: Vec<bool>,
    dirty: Vec<DirtyPiece>,
    cache: DynamicLayerStacksCache,
    transformed: AlignedBox<u8>,
    l1_out: AlignedBox<i32>,
    l2_input: AlignedBox<u8>,
    l2_out: AlignedBox<i32>,
    l3_input: AlignedBox<u8>,
}

impl DynamicLayerStacksStack {
    fn new(net: &DynamicLayerStacksNetwork) -> Self {
        Self {
            signature: net.stack_signature(),
            current: 0,
            accumulations: AlignedBox::new_zeroed(STACK_CAPACITY * 2 * net.spec.l1),
            threat_accumulations: AlignedBox::new_zeroed(
                STACK_CAPACITY * 2 * net.spec.l1 * usize::from(net.threat_dimensions != 0),
            ),
            psqt: AlignedBox::new_zeroed(STACK_CAPACITY * 2 * net.num_buckets),
            computed: vec![false; STACK_CAPACITY],
            dirty: (0..STACK_CAPACITY).map(|_| DirtyPiece::default()).collect(),
            cache: DynamicLayerStacksCache::new(
                net.spec.l1,
                net.num_buckets,
                !net.psqt_weights.is_empty(),
            ),
            transformed: AlignedBox::new_zeroed(padded_input(net.spec.l1)),
            l1_out: AlignedBox::new_zeroed(net.spec.l2),
            l2_input: AlignedBox::new_zeroed(padded_input(2 * (net.spec.l2 - 1))),
            l2_out: AlignedBox::new_zeroed(net.spec.l3),
            l3_input: AlignedBox::new_zeroed(padded_input(net.spec.l3)),
        }
    }
    pub(crate) fn matches_network(&self, net: &DynamicLayerStacksNetwork) -> bool {
        self.signature == net.stack_signature()
    }
    pub(crate) fn reset(&mut self) {
        self.current = 0;
        self.computed[0] = false;
        self.dirty[0].clear();
        // A same-shaped EvalFile reuses this stack. Its cached accumulators were
        // computed from the previous network weights and must not survive reset.
        self.cache.clear();
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

    fn find_usable_accumulator(&self, max_depth: usize) -> Option<usize> {
        if self.current == 0
            || self.dirty[self.current].king_moved[Color::Black.index()]
            || self.dirty[self.current].king_moved[Color::White.index()]
        {
            return None;
        }

        for depth in 1..=max_depth.min(self.current) {
            let index = self.current - depth;
            if self.computed[index] {
                return Some(index);
            }
            if self.dirty[index].king_moved[Color::Black.index()]
                || self.dirty[index].king_moved[Color::White.index()]
            {
                return None;
            }
        }
        None
    }
}

fn read_i32s<R: Read>(reader: &mut R, dst: &mut [i32]) -> io::Result<()> {
    let mut b = [0; 4];
    for v in dst {
        reader.read_exact(&mut b)?;
        *v = i32::from_le_bytes(b);
    }
    Ok(())
}
fn add_i16(a: &mut [i16], b: &[i16]) {
    #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
    // SAFETY: `chunks` is bounded by both slices; each v128 access covers eight in-range
    // i16 elements and the scalar tail starts immediately after the last vector.
    unsafe {
        use std::arch::wasm32::*;
        let chunks = a.len().min(b.len()) / 8;
        for i in 0..chunks {
            let offset = i * 8;
            let va = v128_load(a.as_ptr().add(offset).cast::<v128>());
            let vb = v128_load(b.as_ptr().add(offset).cast::<v128>());
            v128_store(a.as_mut_ptr().add(offset).cast::<v128>(), i16x8_add(va, vb));
        }
        for (a, &b) in a[chunks * 8..].iter_mut().zip(&b[chunks * 8..]) {
            *a = a.wrapping_add(b);
        }
        return;
    }
    #[allow(unreachable_code)]
    for (a, &b) in a.iter_mut().zip(b) {
        *a = a.wrapping_add(b);
    }
}
fn sub_i16(a: &mut [i16], b: &[i16]) {
    #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
    // SAFETY: `chunks` is bounded by both slices; each v128 access covers eight in-range
    // i16 elements and the scalar tail starts immediately after the last vector.
    unsafe {
        use std::arch::wasm32::*;
        let chunks = a.len().min(b.len()) / 8;
        for i in 0..chunks {
            let offset = i * 8;
            let va = v128_load(a.as_ptr().add(offset).cast::<v128>());
            let vb = v128_load(b.as_ptr().add(offset).cast::<v128>());
            v128_store(a.as_mut_ptr().add(offset).cast::<v128>(), i16x8_sub(va, vb));
        }
        for (a, &b) in a[chunks * 8..].iter_mut().zip(&b[chunks * 8..]) {
            *a = a.wrapping_sub(b);
        }
        return;
    }
    #[allow(unreachable_code)]
    for (a, &b) in a.iter_mut().zip(b) {
        *a = a.wrapping_sub(b);
    }
}
fn sub_add_i16(a: &mut [i16], sub: &[i16], add: &[i16]) {
    #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
    // SAFETY: all slices have the same validated L1 length. Each v128 access covers
    // eight in-range i16 elements and the scalar tail follows the last vector.
    unsafe {
        use std::arch::wasm32::*;
        let chunks = a.len().min(sub.len()).min(add.len()) / 8;
        for i in 0..chunks {
            let offset = i * 8;
            let va = v128_load(a.as_ptr().add(offset).cast::<v128>());
            let vs = v128_load(sub.as_ptr().add(offset).cast::<v128>());
            let vd = v128_load(add.as_ptr().add(offset).cast::<v128>());
            v128_store(a.as_mut_ptr().add(offset).cast::<v128>(), i16x8_add(i16x8_sub(va, vs), vd));
        }
        for ((a, &sub), &add) in
            a[chunks * 8..].iter_mut().zip(&sub[chunks * 8..]).zip(&add[chunks * 8..])
        {
            *a = a.wrapping_sub(sub).wrapping_add(add);
        }
        return;
    }
    #[allow(unreachable_code)]
    for ((a, &sub), &add) in a.iter_mut().zip(sub).zip(add) {
        *a = a.wrapping_sub(sub).wrapping_add(add);
    }
}
fn double_sub_add_i16(a: &mut [i16], sub0: &[i16], add0: &[i16], sub1: &[i16], add1: &[i16]) {
    #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
    // SAFETY: all slices have the same validated L1 length. Each v128 access covers
    // eight in-range i16 elements and the scalar tail follows the last vector.
    unsafe {
        use std::arch::wasm32::*;
        let chunks = a.len().min(sub0.len()).min(add0.len()).min(sub1.len()).min(add1.len()) / 8;
        for i in 0..chunks {
            let offset = i * 8;
            let va = v128_load(a.as_ptr().add(offset).cast::<v128>());
            let vs0 = v128_load(sub0.as_ptr().add(offset).cast::<v128>());
            let vd0 = v128_load(add0.as_ptr().add(offset).cast::<v128>());
            let vs1 = v128_load(sub1.as_ptr().add(offset).cast::<v128>());
            let vd1 = v128_load(add1.as_ptr().add(offset).cast::<v128>());
            let result = i16x8_add(i16x8_sub(i16x8_add(i16x8_sub(va, vs0), vd0), vs1), vd1);
            v128_store(a.as_mut_ptr().add(offset).cast::<v128>(), result);
        }
        for ((((a, &sub0), &add0), &sub1), &add1) in a[chunks * 8..]
            .iter_mut()
            .zip(&sub0[chunks * 8..])
            .zip(&add0[chunks * 8..])
            .zip(&sub1[chunks * 8..])
            .zip(&add1[chunks * 8..])
        {
            *a = a.wrapping_sub(sub0).wrapping_add(add0).wrapping_sub(sub1).wrapping_add(add1);
        }
        return;
    }
    #[allow(unreachable_code)]
    for ((((a, &sub0), &add0), &sub1), &add1) in
        a.iter_mut().zip(sub0).zip(add0).zip(sub1).zip(add1)
    {
        *a = a.wrapping_sub(sub0).wrapping_add(add0).wrapping_sub(sub1).wrapping_add(add1);
    }
}
fn add_i32(a: &mut [i32], b: &[i32]) {
    for (a, &b) in a.iter_mut().zip(b) {
        *a = a.wrapping_add(b);
    }
}
fn sub_i32(a: &mut [i32], b: &[i32]) {
    for (a, &b) in a.iter_mut().zip(b) {
        *a = a.wrapping_sub(b);
    }
}

fn sqr_transform_without_threat(us: &[i16], them: &[i16], output: &mut [u8]) {
    let half = us.len() / 2;
    #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
    let mut processed = 0;
    #[cfg(not(all(target_arch = "wasm32", target_feature = "simd128")))]
    let processed = 0;

    #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
    // SAFETY: both accumulator slices have the same even L1 length and `output` has L1
    // elements. The loop condition reserves 16 elements in each half before every load/store.
    unsafe {
        use std::arch::wasm32::*;
        let zero = i16x8_splat(0);
        let max127 = i16x8_splat(127);
        for (acc, out_offset) in [(us, 0usize), (them, half)] {
            let mut offset = 0;
            while offset + 16 <= half {
                let a0 = v128_load(acc.as_ptr().add(offset).cast::<v128>());
                let b0 = v128_load(acc.as_ptr().add(half + offset).cast::<v128>());
                let a1 = v128_load(acc.as_ptr().add(offset + 8).cast::<v128>());
                let b1 = v128_load(acc.as_ptr().add(half + offset + 8).cast::<v128>());
                let product0 = u16x8_shr(
                    i16x8_mul(
                        i16x8_min(i16x8_max(a0, zero), max127),
                        i16x8_min(i16x8_max(b0, zero), max127),
                    ),
                    7,
                );
                let product1 = u16x8_shr(
                    i16x8_mul(
                        i16x8_min(i16x8_max(a1, zero), max127),
                        i16x8_min(i16x8_max(b1, zero), max127),
                    ),
                    7,
                );
                v128_store(
                    output.as_mut_ptr().add(out_offset + offset).cast::<v128>(),
                    u8x16_narrow_i16x8(product0, product1),
                );
                offset += 16;
            }
            processed = offset;
        }
    }

    for i in processed..half {
        output[i] = ((u32::from(us[i].clamp(0, 127) as u16)
            * u32::from(us[half + i].clamp(0, 127) as u16))
            >> 7) as u8;
        output[half + i] = ((u32::from(them[i].clamp(0, 127) as u16)
            * u32::from(them[half + i].clamp(0, 127) as u16))
            >> 7) as u8;
    }
}

fn sqr_transform_with_threat(
    us: &[i16],
    us_threat: &[i16],
    them: &[i16],
    them_threat: &[i16],
    output: &mut [u8],
) {
    let half = us.len() / 2;
    #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
    let mut processed = 0;
    #[cfg(not(all(target_arch = "wasm32", target_feature = "simd128")))]
    let processed = 0;

    #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
    // SAFETY: all four accumulator slices have the same even L1 length and `output` has L1
    // elements. The loop condition reserves 16 elements in each half before every load/store.
    unsafe {
        use std::arch::wasm32::*;
        let zero = i16x8_splat(0);
        let max127 = i16x8_splat(127);
        for (acc, threat, out_offset) in [(us, us_threat, 0usize), (them, them_threat, half)] {
            let mut offset = 0;
            while offset + 16 <= half {
                let a0 = i16x8_add(
                    v128_load(acc.as_ptr().add(offset).cast::<v128>()),
                    v128_load(threat.as_ptr().add(offset).cast::<v128>()),
                );
                let b0 = i16x8_add(
                    v128_load(acc.as_ptr().add(half + offset).cast::<v128>()),
                    v128_load(threat.as_ptr().add(half + offset).cast::<v128>()),
                );
                let a1 = i16x8_add(
                    v128_load(acc.as_ptr().add(offset + 8).cast::<v128>()),
                    v128_load(threat.as_ptr().add(offset + 8).cast::<v128>()),
                );
                let b1 = i16x8_add(
                    v128_load(acc.as_ptr().add(half + offset + 8).cast::<v128>()),
                    v128_load(threat.as_ptr().add(half + offset + 8).cast::<v128>()),
                );
                let product0 = u16x8_shr(
                    i16x8_mul(
                        i16x8_min(i16x8_max(a0, zero), max127),
                        i16x8_min(i16x8_max(b0, zero), max127),
                    ),
                    7,
                );
                let product1 = u16x8_shr(
                    i16x8_mul(
                        i16x8_min(i16x8_max(a1, zero), max127),
                        i16x8_min(i16x8_max(b1, zero), max127),
                    ),
                    7,
                );
                v128_store(
                    output.as_mut_ptr().add(out_offset + offset).cast::<v128>(),
                    u8x16_narrow_i16x8(product0, product1),
                );
                offset += 16;
            }
            processed = offset;
        }
    }

    for i in processed..half {
        let us0 = us[i].wrapping_add(us_threat[i]);
        let us1 = us[half + i].wrapping_add(us_threat[half + i]);
        let them0 = them[i].wrapping_add(them_threat[i]);
        let them1 = them[half + i].wrapping_add(them_threat[half + i]);
        output[i] = ((u32::from(us0.clamp(0, 127) as u16) * u32::from(us1.clamp(0, 127) as u16))
            >> 7) as u8;
        output[half + i] = ((u32::from(them0.clamp(0, 127) as u16)
            * u32::from(them1.clamp(0, 127) as u16))
            >> 7) as u8;
    }
}
fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

fn parse_ft_input_dimensions(
    arch: &str,
    feature: RuntimeLsFeature,
    threat_dimensions: usize,
) -> io::Result<usize> {
    let reported = parse_feature_input_dimensions(arch)
        .ok_or_else(|| invalid("missing FT input dimensions"))?;
    let input_dimensions = feature.dimensions();

    // Threat weights are stored in a separate block after the FT (and optional PSQT) data. Some
    // exporters report only the FT dimension in `Features=...`, while older tatara models report
    // FT + Threat there. Normalize both header conventions to the actual FT row count.
    if reported == input_dimensions {
        return Ok(input_dimensions);
    }
    if threat_dimensions != 0 && input_dimensions.checked_add(threat_dimensions) == Some(reported) {
        return Ok(input_dimensions);
    }

    Err(invalid(format!(
        "FT input dimension mismatch: header={reported}, runtime={input_dimensions}, threat={threat_dimensions}"
    )))
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "layerstack-arch")]
    use std::fs::File;
    #[cfg(feature = "layerstack-arch")]
    use std::io::BufReader;
    use std::io::Cursor;
    use std::sync::Arc;

    use super::*;
    use crate::nnue::accumulator_stack_variant::AccumulatorStackVariant;
    use crate::nnue::evaluator::NNUEEvaluator;
    use crate::nnue::net_delta::{NetCoefficientId, NetDelta, NetTensorKind};
    use crate::nnue::network::NNUENetwork;
    #[cfg(feature = "layerstack-arch")]
    use crate::nnue::network::set_layer_stack_bucket_mode;
    #[cfg(feature = "layerstack-arch")]
    use crate::nnue::network_layer_stacks::LayerStacksNetwork;
    use crate::position::SFEN_HIRATE;
    use crate::types::Move;

    #[test]
    fn layer_stacks_ft_detection_accepts_underscore_headers() {
        let cases = [
            ("HalfKP", 125_388, FeatureSet::HalfKP),
            ("HalfKA", 138_510, FeatureSet::HalfKaSplit),
            ("HalfKA_merged", 131_949, FeatureSet::HalfKaMerged),
            ("HalfKA_hm_split", 76_950, FeatureSet::HalfKaHmSplit),
            ("HalfKA_hm", 73_305, FeatureSet::HalfKaHmMerged),
        ];
        for (keyword, input_dim, expected) in cases {
            let arch = format!(
                "Features={keyword}(Friend)[{input_dim}->1536x2],Network=(ClippedReLU[32](SqrClippedReLU[30]))"
            );
            assert_eq!(detect_layer_stacks_feature(&arch).unwrap(), expected, "keyword={keyword}");
        }
    }

    #[test]
    fn layer_stacks_ft_detection_rejects_unknown_keyword_substrings() {
        let arch = "Features=UnknownHalfKaHmMerged(Friend)[73305->1536x2],Network=(ClippedReLU[32](SqrClippedReLU[30]))";
        assert!(detect_layer_stacks_feature(arch).is_err());
    }

    fn zero_affine(input_dim: usize, output_dim: usize) -> DynamicAffine {
        let bytes = vec![0; output_dim * 4 + output_dim * padded_input(input_dim)];
        DynamicAffine::read(&mut Cursor::new(bytes), input_dim, output_dim).unwrap()
    }

    fn coefficient(kind: NetTensorKind, bucket: Option<usize>, index: usize) -> NetCoefficientId {
        NetCoefficientId {
            kind,
            bucket,
            index,
        }
    }

    fn evaluated_values(
        bytes: &[u8],
        deltas: Option<&[NetDelta]>,
        num_buckets: usize,
    ) -> Vec<Value> {
        let mut network = NNUENetwork::from_bytes(bytes).expect("synthetic LayerStacks");
        if let Some(deltas) = deltas {
            network.apply_net_deltas(deltas).expect("valid deltas");
        }
        crate::nnue::configure_layer_stack_routing(
            LayerStackBucketMode::ProgressKPAbs,
            num_buckets,
            Some(num_buckets),
        )
        .expect("routing");

        let mut pos = Position::new();
        pos.set_sfen(SFEN_HIRATE).expect("hirate");
        let mut evaluator = NNUEEvaluator::new_with_position(Arc::new(network), &pos);
        let mut values = vec![evaluator.evaluate(&pos)];
        for move_text in ["7g7f", "3c3d", "2g2f"] {
            let mv = Move::from_usi(move_text).expect("move");
            let gives_check = pos.gives_check(mv);
            let dirty = pos.do_move(mv, gives_check);
            evaluator.push(dirty);
            values.push(evaluator.evaluate(&pos));
        }
        values
    }

    #[test]
    fn dynamic_layer_stacks_net_delta_matches_file_edits_and_validates_shape() {
        let routing_guard = crate::nnue::network::layer_stack_routing_test_guard();
        use crate::nnue::net_delta::test_utils::build_synthetic_layer_stacks;

        crate::nnue::reset_layer_stack_progress_kpabs_weights();

        for num_buckets in [4, 9] {
            let synthetic = build_synthetic_layer_stacks(
                "HalfKP",
                HalfKPFeatureSet::DIMENSIONS,
                32,
                4,
                2,
                num_buckets,
            );
            crate::nnue::configure_layer_stack_routing(
                LayerStackBucketMode::ProgressKPAbs,
                num_buckets,
                Some(num_buckets),
            )
            .expect("routing");
            let mut pos = Position::new();
            pos.set_sfen(SFEN_HIRATE).expect("hirate");
            let selected_bucket = compute_layer_stack_progresskpabs_bucket_index(
                &pos,
                pos.side_to_move(),
                get_layer_stack_progress_kpabs_weights(),
                num_buckets,
            );
            assert_eq!(selected_bucket, num_buckets / 2);

            let baseline = evaluated_values(&synthetic.bytes, None, num_buckets);
            let empty = evaluated_values(&synthetic.bytes, Some(&[]), num_buckets);
            let zero = evaluated_values(
                &synthetic.bytes,
                Some(&[NetDelta {
                    id: coefficient(NetTensorKind::FtBias, None, 0),
                    delta: 0,
                }]),
                num_buckets,
            );
            assert_eq!(baseline, empty);
            assert_eq!(baseline, zero);

            let cases = [
                (
                    coefficient(NetTensorKind::OutputWeight, Some(selected_bucket), 0),
                    synthetic.buckets[selected_bucket].output_weights,
                    64,
                ),
                (
                    coefficient(NetTensorKind::OutputBias, Some(selected_bucket), 0),
                    synthetic.buckets[selected_bucket].output_bias,
                    256,
                ),
                (coefficient(NetTensorKind::FtBias, None, 0), synthetic.ft_biases, 48),
                (
                    coefficient(NetTensorKind::L2Weight, Some(selected_bucket), 3),
                    synthetic.buckets[selected_bucket].l2_weights + 3,
                    64,
                ),
            ];
            for (id, byte_offset, delta) in cases {
                let network = NNUENetwork::from_bytes(&synthetic.bytes).expect("network");
                let base = network.net_coefficient(&id).expect("coefficient");
                let edited_value = base + delta;
                let mut edited = synthetic.bytes.clone();
                match id.kind {
                    NetTensorKind::OutputBias => edited[byte_offset..byte_offset + 4]
                        .copy_from_slice(&edited_value.to_le_bytes()),
                    NetTensorKind::FtBias => {
                        edited[byte_offset] =
                            u8::try_from(edited_value).expect("single-byte LEB128");
                    }
                    NetTensorKind::OutputWeight | NetTensorKind::L2Weight => {
                        edited[byte_offset] = edited_value as i8 as u8;
                    }
                }
                let from_file = evaluated_values(&edited, None, num_buckets);
                let from_delta = evaluated_values(
                    &synthetic.bytes,
                    Some(&[NetDelta {
                        id: id.clone(),
                        delta,
                    }]),
                    num_buckets,
                );
                assert_ne!(
                    from_delta,
                    baseline,
                    "{}: baseline={baseline:?}, from_delta={from_delta:?}",
                    id.usi_name()
                );
                assert_eq!(from_file, from_delta, "{}", id.usi_name());

                let mut network = NNUENetwork::from_bytes(&synthetic.bytes).expect("network");
                network
                    .apply_net_deltas(&[NetDelta {
                        id: id.clone(),
                        delta,
                    }])
                    .expect("apply");
                assert_eq!(network.net_coefficient(&id).expect("coefficient"), edited_value);
            }

            let mut network = NNUENetwork::from_bytes(&synthetic.bytes).expect("network");
            let bad_bucket = NetDelta {
                id: coefficient(NetTensorKind::OutputWeight, Some(num_buckets), 0),
                delta: 1,
            };
            assert!(network.apply_net_deltas(&[bad_bucket]).is_err());
            let out_w_len = network
                .net_tensor_shape(NetTensorKind::OutputWeight)
                .expect("shape")
                .element_count;
            let bad_index = NetDelta {
                id: coefficient(NetTensorKind::OutputWeight, Some(0), out_w_len),
                delta: 1,
            };
            assert!(network.apply_net_deltas(&[bad_index]).is_err());

            let saturating_id = coefficient(NetTensorKind::OutputWeight, Some(0), 0);
            let report = network
                .apply_net_deltas(&[NetDelta {
                    id: saturating_id.clone(),
                    delta: 1_000,
                }])
                .expect("saturating delta");
            assert_eq!(report.clamped, 1);
            assert_eq!(network.net_coefficient(&saturating_id).expect("coefficient"), 127);
        }
        crate::nnue::reset_layer_stack_progress_buckets();
        crate::nnue::reset_layer_stack_progress_kpabs_weights();

        drop(routing_guard);
    }

    #[test]
    fn threat_headers_accept_separate_and_combined_reported_dimensions() {
        let feature = RuntimeLsFeature::HalfKaHmMerged;
        let separate = format!(
            "Features=HalfKaHmMerged(Friend)[{}->1024x2],Threat=216720,Network=...",
            feature.dimensions()
        );
        let combined = format!(
            "Features=HalfKaHmMerged(Friend)[{}->1024x2],Threat=216720,Network=...",
            feature.dimensions() + 216_720
        );

        assert_eq!(
            parse_ft_input_dimensions(&separate, feature, 216_720).unwrap(),
            feature.dimensions()
        );
        assert_eq!(
            parse_ft_input_dimensions(&combined, feature, 216_720).unwrap(),
            feature.dimensions()
        );
    }

    fn test_network(
        feature: RuntimeLsFeature,
        l1: usize,
        l2: usize,
        l3: usize,
        num_buckets: usize,
        has_psqt: bool,
        threat_dimensions: usize,
    ) -> DynamicLayerStacksNetwork {
        DynamicLayerStacksNetwork {
            spec: ArchitectureSpec::new(FeatureSet::LayerStacks, l1, l2, l3, Activation::CReLU),
            feature,
            input_dimensions: feature.dimensions(),
            num_buckets,
            ft_biases: AlignedBox::new_zeroed(l1),
            ft_weights: AlignedBox::new_zeroed(0),
            psqt_biases: AlignedBox::new_zeroed(usize::from(has_psqt)),
            psqt_weights: AlignedBox::new_zeroed(usize::from(has_psqt)),
            threat_dimensions,
            threat_weights: AlignedBox::new_zeroed(0),
            buckets: Vec::new(),
            fv_scale: FV_SCALE_HALFKA,
        }
    }

    fn weighted_test_network() -> DynamicLayerStacksNetwork {
        let mut net =
            test_network(RuntimeLsFeature::HalfKaHmMerged, 8, 4, 3, DEFAULT_NUM_BUCKETS, false, 0);
        for (i, bias) in net.ft_biases.iter_mut().enumerate() {
            *bias = i as i16 - 4;
        }
        net.ft_weights = AlignedBox::new_zeroed(net.input_dimensions * net.spec.l1);
        for (i, weight) in net.ft_weights.iter_mut().enumerate() {
            *weight = (i % 251) as i16 - 125;
        }
        net
    }

    fn assert_current_accumulators_equal(
        left: &DynamicLayerStacksStack,
        right: &DynamicLayerStacksStack,
        l1: usize,
    ) {
        let left_start = left.current * 2 * l1;
        let right_start = right.current * 2 * l1;
        assert_eq!(
            &left.accumulations[left_start..left_start + 2 * l1],
            &right.accumulations[right_start..right_start + 2 * l1]
        );
    }

    #[test]
    fn no_threat_stack_does_not_allocate_threat_accumulators() {
        let net = weighted_test_network();
        let stack = net.new_stack();
        assert!(stack.threat_accumulations.is_empty());
    }

    #[test]
    fn finny_cache_refresh_matches_uncached_refresh() {
        let net = weighted_test_network();
        let mut pos = Position::new();
        pos.set_sfen(SFEN_HIRATE).unwrap();
        let mut cached = net.new_stack();
        net.refresh(&pos, &mut cached);

        let mv = Move::from_usi("7g7f").unwrap();
        let gives_check = pos.gives_check(mv);
        pos.do_move(mv, gives_check);
        cached.reset();
        net.refresh(&pos, &mut cached);

        let mut uncached = net.new_stack();
        net.refresh(&pos, &mut uncached);
        assert_current_accumulators_equal(&cached, &uncached, net.spec.l1);
    }

    #[test]
    fn reset_invalidates_finny_cache_before_same_shape_network_switch() {
        let first = weighted_test_network();
        let mut second = weighted_test_network();
        for bias in second.ft_biases.iter_mut() {
            *bias = bias.wrapping_add(37);
        }
        for weight in second.ft_weights.iter_mut() {
            *weight = weight.wrapping_mul(3).wrapping_add(11);
        }

        let mut pos = Position::new();
        pos.set_sfen(SFEN_HIRATE).unwrap();
        let mut reused = first.new_stack();
        first.refresh(&pos, &mut reused);

        let mv = Move::from_usi("7g7f").unwrap();
        let gives_check = pos.gives_check(mv);
        pos.do_move(mv, gives_check);
        reused.reset();
        assert!(reused.cache.valid.iter().all(|&valid| !valid));
        second.refresh(&pos, &mut reused);

        let mut fresh = second.new_stack();
        second.refresh(&pos, &mut fresh);
        assert_current_accumulators_equal(&reused, &fresh, second.spec.l1);
    }

    #[test]
    fn ancestor_update_matches_full_refresh() {
        let net = weighted_test_network();
        let mut pos = Position::new();
        pos.set_sfen(SFEN_HIRATE).unwrap();
        let mut incremental = net.new_stack();
        net.refresh(&pos, &mut incremental);

        for move_text in ["7g7f", "3c3d"] {
            let mv = Move::from_usi(move_text).unwrap();
            let gives_check = pos.gives_check(mv);
            let dirty = pos.do_move(mv, gives_check);
            incremental.push(dirty);
        }
        assert!(!incremental.computed[1]);
        net.ensure(&pos, &mut incremental);

        let mut refreshed = net.new_stack();
        net.refresh(&pos, &mut refreshed);
        assert_current_accumulators_equal(&incremental, &refreshed, net.spec.l1);
    }

    #[test]
    fn king_move_cache_update_matches_full_refresh() {
        let net = weighted_test_network();
        let mut pos = Position::new();
        pos.set_sfen("4k4/9/9/9/9/9/9/9/4K4 b - 1").unwrap();
        let mut incremental = net.new_stack();
        net.refresh(&pos, &mut incremental);

        for move_text in ["5i6h", "5a6b", "6h5i"] {
            let mv = Move::from_usi(move_text).unwrap();
            let gives_check = pos.gives_check(mv);
            let dirty = pos.do_move(mv, gives_check);
            incremental.push(dirty);
            net.ensure(&pos, &mut incremental);

            let mut refreshed = net.new_stack();
            net.refresh(&pos, &mut refreshed);
            assert_current_accumulators_equal(&incremental, &refreshed, net.spec.l1);
        }
    }

    #[test]
    fn fused_updates_match_separate_add_and_subtract() {
        let mut expected = [11i16, -22, 33, -44, 55, -66, 77, -88];
        let mut actual = expected;
        let sub0 = [1i16, 2, 3, 4, 5, 6, 7, 8];
        let add0 = [8i16, 7, 6, 5, 4, 3, 2, 1];
        let sub1 = [-2i16, 4, -6, 8, -10, 12, -14, 16];
        let add1 = [16i16, -14, 12, -10, 8, -6, 4, -2];
        sub_i16(&mut expected, &sub0);
        add_i16(&mut expected, &add0);
        sub_i16(&mut expected, &sub1);
        add_i16(&mut expected, &add1);
        double_sub_add_i16(&mut actual, &sub0, &add0, &sub1, &add1);
        assert_eq!(actual, expected);
    }

    #[test]
    fn stack_is_rebuilt_for_every_runtime_layer_stacks_identity_change() {
        let base = NNUENetwork::DynamicLayerStacks(Box::new(test_network(
            RuntimeLsFeature::HalfKaHmMerged,
            8,
            4,
            3,
            9,
            false,
            0,
        )));
        let stack = AccumulatorStackVariant::from_network(&base);
        assert!(stack.matches_network(&base));

        let switches = [
            test_network(RuntimeLsFeature::HalfKaHmMerged, 10, 4, 3, 9, false, 0),
            test_network(RuntimeLsFeature::HalfKaHmMerged, 8, 5, 3, 9, false, 0),
            test_network(RuntimeLsFeature::HalfKaHmMerged, 8, 4, 5, 9, false, 0),
            test_network(RuntimeLsFeature::HalfKaHmMerged, 8, 4, 3, 4, false, 0),
            test_network(RuntimeLsFeature::HalfKP, 8, 4, 3, 9, false, 0),
            test_network(RuntimeLsFeature::HalfKaHmMerged, 8, 4, 3, 9, true, 0),
            test_network(RuntimeLsFeature::HalfKaHmMerged, 8, 4, 3, 9, false, 1),
        ];
        for switched_net in switches {
            let switched = NNUENetwork::DynamicLayerStacks(Box::new(switched_net));
            assert!(!stack.matches_network(&switched));
            assert!(AccumulatorStackVariant::from_network(&switched).matches_network(&switched));
        }
    }

    #[test]
    fn public_evaluator_keeps_board_effects_for_runtime_effect_bucket() {
        let routing_guard = crate::nnue::network::layer_stack_routing_test_guard();
        let feature = RuntimeLsFeature::EffectBucket(EffectBucketConfig::KINGFIXED_2X2);
        let l1 = 2;
        let l2 = 2;
        let l3 = 1;
        let num_buckets = DEFAULT_NUM_BUCKETS;
        crate::nnue::configure_layer_stack_routing(
            LayerStackBucketMode::ProgressKPAbs,
            num_buckets,
            Some(num_buckets),
        )
        .unwrap();
        let mut net = test_network(feature, l1, l2, l3, num_buckets, false, 0);
        net.ft_weights = AlignedBox::new_zeroed(feature.dimensions() * l1);
        net.buckets = (0..num_buckets)
            .map(|_| DynamicLsBucket {
                l1: zero_affine(l1, l2),
                l2: zero_affine(2 * (l2 - 1), l3),
                output: zero_affine(l3, 1),
            })
            .collect();

        let mut pos = Position::new();
        pos.set_sfen(SFEN_HIRATE).unwrap();
        let mut evaluator = NNUEEvaluator::new_with_position(
            Arc::new(NNUENetwork::DynamicLayerStacks(Box::new(net))),
            &pos,
        );
        assert_eq!(evaluator.evaluate(&pos), Value::ZERO);

        let mv = Move::from_usi("7g7f").unwrap();
        let dirty = pos.do_move(mv, pos.gives_check(mv));
        evaluator.push(dirty);
        assert_eq!(evaluator.evaluate(&pos), Value::ZERO);

        // routing はプロセスグローバルのため、他テストへ持ち越さないよう未設定へ戻す。
        crate::nnue::reset_layer_stack_progress_buckets();

        drop(routing_guard);
    }

    #[cfg(feature = "layerstack-arch")]
    #[test]
    #[ignore]
    fn real_model_matches_const_generic() {
        let path = std::env::var("NNUE_DYNAMIC_LS_COMPARE_FILE")
            .expect("set NNUE_DYNAMIC_LS_COMPARE_FILE to a LayerStacks NNUE file");
        let mut dynamic_reader = BufReader::new(File::open(&path).unwrap());
        let dynamic = DynamicLayerStacksNetwork::read(&mut dynamic_reader, None).unwrap();

        let mut static_reader = BufReader::new(File::open(&path).unwrap());
        let static_net = LayerStacksNetwork::read_with_options(
            &mut static_reader,
            dynamic.spec.l1,
            dynamic.spec.l2,
            dynamic.spec.l3,
            None,
        )
        .unwrap();

        set_layer_stack_bucket_mode(LayerStackBucketMode::KingRank9);
        let mut pos = Position::new();
        pos.set_sfen(SFEN_HIRATE).unwrap();
        let mut dynamic_stack = dynamic.new_stack();
        dynamic.refresh(&pos, &mut dynamic_stack);
        let dynamic_value = dynamic.evaluate(&pos, &mut dynamic_stack);

        let mut static_stack = static_net.new_acc_stack();
        static_net.update_accumulator(&pos, &mut static_stack, &mut None);
        let static_value = static_net.evaluate(&pos, &static_stack);
        assert_eq!(dynamic_value, static_value);

        for move_text in ["7g7f", "3c3d", "2g2f", "8c8d", "6i7h", "4a3b"] {
            let mv = Move::from_usi(move_text).unwrap();
            let gives_check = pos.gives_check(mv);
            let dirty = pos.do_move(mv, gives_check);
            dynamic_stack.push(dirty);
            dynamic.ensure(&pos, &mut dynamic_stack);
            let incremental = dynamic.evaluate(&pos, &mut dynamic_stack);

            let mut refreshed_stack = dynamic.new_stack();
            dynamic.refresh(&pos, &mut refreshed_stack);
            let refreshed = dynamic.evaluate(&pos, &mut refreshed_stack);
            assert_eq!(incremental, refreshed, "dynamic update mismatch after {move_text}");
        }
    }
}
