//! FeatureTransformerLayerStacks - LayerStacksアーキテクチャ用の1536次元Feature Transformer
//!
//! HalfKA_hm^ 特徴量（キングバケット×BonaPiece）から、
//! 片側 1536 次元×両視点の中間表現を生成する。

use super::accumulator::{Aligned, AlignedBox};
use super::accumulator::{DirtyPiece, IndexList, MAX_ACTIVE_FEATURES, MAX_CHANGED_FEATURES};
use super::accumulator_layer_stacks::{
    AccumulatorCacheLayerStacks, AccumulatorLayerStacks, AccumulatorStackLayerStacks,
};
use super::bona_piece::BonaPiece;
use super::bona_piece_halfka_hm::{halfka_index, is_hm_mirror, king_bucket, pack_bonapiece};
use super::constants::{HALFKA_HM_DIMENSIONS, NNUE_PYTORCH_L1, NUM_LAYER_STACK_BUCKETS};
use super::features::{Feature, FeatureSet, HalfKA_hm, HalfKA_hm_FeatureSet};
use super::leb128::read_compressed_tensor_i16_all;
use super::threat_features::{self, MAX_CHANGED_THREAT_FEATURES, THREAT_DIMENSIONS};
use crate::position::Position;
use crate::types::Color;
use std::io::{self, Read};
use std::mem::MaybeUninit;

/// 特徴インデックスの範囲外アクセス時のパニック
#[cold]
#[inline(never)]
fn feature_index_oob(index: usize, max: usize) -> ! {
    panic!("Feature index out of range: {index} (max: {max})")
}

#[inline]
fn append_changed_indices(
    dirty_piece: &DirtyPiece,
    perspective: Color,
    king_sq: crate::types::Square,
    removed: &mut IndexList<MAX_CHANGED_FEATURES>,
    added: &mut IndexList<MAX_CHANGED_FEATURES>,
) {
    <HalfKA_hm as Feature>::append_changed_indices(
        dirty_piece,
        perspective,
        king_sq,
        removed,
        added,
    );
}

#[inline]
fn append_active_indices(
    pos: &Position,
    perspective: Color,
    active: &mut IndexList<MAX_ACTIVE_FEATURES>,
) {
    <HalfKA_hm as Feature>::append_active_indices(pos, perspective, active);
}

#[inline]
fn feature_index_from_bona_piece(
    bp: BonaPiece,
    perspective: Color,
    king_sq: crate::types::Square,
) -> usize {
    let kb = king_bucket(king_sq, perspective);
    let hm_mirror = is_hm_mirror(king_sq, perspective);
    let packed = pack_bonapiece(bp, hm_mirror);
    halfka_index(kb, packed)
}

/// nnue-pytorch用のFeatureTransformer（1536次元出力）
#[repr(C, align(64))]
pub struct FeatureTransformerLayerStacks {
    /// バイアス [L1]
    pub biases: Aligned<[i16; NNUE_PYTORCH_L1]>,

    /// 重み [input_dimensions][L1]
    /// 64バイトアラインメントで確保
    pub weights: AlignedBox<i16>,

    /// PSQT バイアス [NUM_LAYER_STACK_BUCKETS]
    pub(crate) psqt_biases: [i32; NUM_LAYER_STACK_BUCKETS],

    /// PSQT 重み [HALFKA_HM_DIMENSIONS × NUM_LAYER_STACK_BUCKETS]
    /// レイアウト: psqt_weights[feature_idx * 9 + bucket]
    pub(crate) psqt_weights: AlignedBox<i32>,

    /// PSQT が有効か（アーキテクチャ文字列で判定）
    pub(crate) has_psqt: bool,

    /// Threat 重み [THREAT_DIMENSIONS × NNUE_PYTORCH_L1]
    /// レイアウト: threat_weights[feature_idx * L1 + neuron] (i8, feature-major)
    /// 64バイトアラインメントで確保
    pub(crate) threat_weights: AlignedBox<i8>,

    /// Threat が有効か（アーキテクチャ文字列で判定）
    pub(crate) has_threat: bool,
}

impl FeatureTransformerLayerStacks {
    /// ファイルから読み込み（非圧縮形式）
    pub fn read<R: Read>(reader: &mut R) -> io::Result<Self> {
        // バイアスを読み込み
        let mut biases = [0i16; NNUE_PYTORCH_L1];
        let mut buf = [0u8; 2];
        for bias in biases.iter_mut() {
            reader.read_exact(&mut buf)?;
            *bias = i16::from_le_bytes(buf);
        }

        // 重みを読み込み
        let weight_size = HALFKA_HM_DIMENSIONS * NNUE_PYTORCH_L1;
        let mut weights = AlignedBox::new_zeroed(weight_size);
        for weight in weights.iter_mut() {
            reader.read_exact(&mut buf)?;
            *weight = i16::from_le_bytes(buf);
        }

        Ok(Self {
            biases: Aligned(biases),
            weights,
            psqt_biases: [0; NUM_LAYER_STACK_BUCKETS],
            psqt_weights: AlignedBox::new_zeroed(0),
            has_psqt: false,
            threat_weights: AlignedBox::new_zeroed(0),
            has_threat: false,
        })
    }

    /// LEB128圧縮形式から読み込み（自動検出）
    ///
    /// 最初のブロックを全デコードし、要素数で形式を判別する:
    /// - 要素数 == biases のみ → YO形式（2ブロック）: 続けて weights ブロックを読む
    /// - 要素数 == biases + weights → 旧bullet-shogi形式（1ブロック）
    pub fn read_leb128<R: Read>(reader: &mut R) -> io::Result<Self> {
        let weight_size = HALFKA_HM_DIMENSIONS * NNUE_PYTORCH_L1;
        let total_size = NNUE_PYTORCH_L1 + weight_size;

        // 最初のブロックを全値デコードして要素数で判別
        let first_block = read_compressed_tensor_i16_all(reader)?;

        if first_block.len() == total_size {
            // 旧bullet-shogi形式（1ブロック）: biases + weights が結合
            let mut biases = [0i16; NNUE_PYTORCH_L1];
            biases.copy_from_slice(&first_block[..NNUE_PYTORCH_L1]);

            let mut weights = AlignedBox::new_zeroed(weight_size);
            weights.copy_from_slice(&first_block[NNUE_PYTORCH_L1..]);

            return Ok(Self {
                biases: Aligned(biases),
                weights,
                psqt_biases: [0; NUM_LAYER_STACK_BUCKETS],
                psqt_weights: AlignedBox::new_zeroed(0),
                has_psqt: false,
                threat_weights: AlignedBox::new_zeroed(0),
                has_threat: false,
            });
        }

        if first_block.len() == NNUE_PYTORCH_L1 {
            // YO形式（2ブロック）: 次に weights ブロックを読み込み
            let weights_block = read_compressed_tensor_i16_all(reader)?;
            if weights_block.len() != weight_size {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "FT weights block size mismatch: got {}, expected {}",
                        weights_block.len(),
                        weight_size
                    ),
                ));
            }

            let mut biases = [0i16; NNUE_PYTORCH_L1];
            biases.copy_from_slice(&first_block);

            let mut weights = AlignedBox::new_zeroed(weight_size);
            weights.copy_from_slice(&weights_block);

            return Ok(Self {
                biases: Aligned(biases),
                weights,
                psqt_biases: [0; NUM_LAYER_STACK_BUCKETS],
                psqt_weights: AlignedBox::new_zeroed(0),
                has_psqt: false,
                threat_weights: AlignedBox::new_zeroed(0),
                has_threat: false,
            });
        }

        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "Unexpected LEB128 tensor size: got {}, expected {} or {}",
                first_block.len(),
                NNUE_PYTORCH_L1,
                total_size
            ),
        ))
    }

    /// PSQT 重み/バイアスをファイルから読み込み
    pub fn read_psqt<R: Read>(&mut self, reader: &mut R) -> io::Result<()> {
        let mut buf4 = [0u8; 4];

        // Biases: i32[NUM_LAYER_STACK_BUCKETS]
        for bias in self.psqt_biases.iter_mut() {
            reader.read_exact(&mut buf4)?;
            *bias = i32::from_le_bytes(buf4);
        }

        // Weights: i32[HALFKA_HM_DIMENSIONS × NUM_LAYER_STACK_BUCKETS]
        let weight_count = HALFKA_HM_DIMENSIONS * NUM_LAYER_STACK_BUCKETS;
        self.psqt_weights = AlignedBox::new_zeroed(weight_count);
        for w in self.psqt_weights.iter_mut() {
            reader.read_exact(&mut buf4)?;
            *w = i32::from_le_bytes(buf4);
        }

        // 注意: 読み込みが途中で失敗した場合、psqt_biases だけが更新された
        // 中途半端な状態になるが、呼び出し元でエラーが伝播し Self は破棄されるため問題ない。
        self.has_psqt = true;
        Ok(())
    }

    /// Threat 重みをファイルから読み込み (i8, raw)
    pub fn read_threat_weights<R: Read>(&mut self, reader: &mut R) -> io::Result<()> {
        let weight_count = THREAT_DIMENSIONS * NNUE_PYTORCH_L1;
        self.threat_weights = AlignedBox::new_zeroed(weight_count);
        let slice = unsafe {
            std::slice::from_raw_parts_mut(
                self.threat_weights.as_mut_ptr() as *mut u8,
                weight_count,
            )
        };
        reader.read_exact(slice)?;
        self.has_threat = true;
        Ok(())
    }

    /// Threat 重みの行を取得（i8[L1]）
    #[inline]
    fn threat_weight_row(&self, index: usize) -> &[i8] {
        let offset = index * NNUE_PYTORCH_L1;
        let end = offset + NNUE_PYTORCH_L1;
        debug_assert!(end <= self.threat_weights.len(), "threat index out of range: {index}");
        &self.threat_weights[offset..end]
    }

    /// Threat 重み (i8) を i16 アキュムレータに加算（スカラー版）
    #[inline]
    fn add_threat_weights(&self, accumulation: &mut [i16; NNUE_PYTORCH_L1], index: usize) {
        let weights = self.threat_weight_row(index);
        for (a, &w) in accumulation.iter_mut().zip(weights) {
            *a = a.wrapping_add(w as i16);
        }
    }

    /// Threat 重み (i8) を i16 アキュムレータから減算（スカラー版）
    #[inline]
    fn sub_threat_weights(&self, accumulation: &mut [i16; NNUE_PYTORCH_L1], index: usize) {
        let weights = self.threat_weight_row(index);
        for (a, &w) in accumulation.iter_mut().zip(weights) {
            *a = a.wrapping_sub(w as i16);
        }
    }

    /// PSQT アキュムレータのフル計算
    fn refresh_psqt(
        &self,
        active_indices: &IndexList<MAX_ACTIVE_FEATURES>,
        psqt_acc: &mut [i32; NUM_LAYER_STACK_BUCKETS],
    ) {
        *psqt_acc = self.psqt_biases;
        for index in active_indices.iter() {
            self.add_psqt_weights(psqt_acc, index);
        }
    }

    /// PSQT 重みを加算
    #[inline]
    fn add_psqt_weights(&self, psqt_acc: &mut [i32; NUM_LAYER_STACK_BUCKETS], index: usize) {
        let offset = index * NUM_LAYER_STACK_BUCKETS;
        debug_assert!(
            offset + NUM_LAYER_STACK_BUCKETS <= self.psqt_weights.len(),
            "psqt_weights index out of bounds: offset={offset}, len={}",
            self.psqt_weights.len()
        );
        for (bucket, acc) in psqt_acc.iter_mut().enumerate() {
            *acc += self.psqt_weights[offset + bucket];
        }
    }

    /// PSQT 重みを減算
    #[inline]
    fn sub_psqt_weights(&self, psqt_acc: &mut [i32; NUM_LAYER_STACK_BUCKETS], index: usize) {
        let offset = index * NUM_LAYER_STACK_BUCKETS;
        debug_assert!(
            offset + NUM_LAYER_STACK_BUCKETS <= self.psqt_weights.len(),
            "psqt_weights index out of bounds: offset={offset}, len={}",
            self.psqt_weights.len()
        );
        for (bucket, acc) in psqt_acc.iter_mut().enumerate() {
            *acc -= self.psqt_weights[offset + bucket];
        }
    }

    /// 差分計算を使わずにAccumulatorを計算
    pub fn refresh_accumulator(&self, pos: &Position, acc: &mut AccumulatorLayerStacks) {
        for perspective in [Color::Black, Color::White] {
            let p = perspective as usize;
            let accumulation = acc.get_mut(p);

            // バイアスで初期化
            accumulation.copy_from_slice(&self.biases.0);

            // アクティブな特徴量の重みを加算
            let mut active_indices = IndexList::new();
            append_active_indices(pos, perspective, &mut active_indices);
            for index in active_indices.iter() {
                self.add_weights(accumulation, index);
            }

            // PSQT アキュムレータ
            if self.has_psqt {
                self.refresh_psqt(&active_indices, &mut acc.psqt_accumulation[p]);
            }

            // Threat アキュムレータ（bias なし: piece FT と bias を共有）
            if self.has_threat {
                let king_sq = pos.king_square(perspective);
                let threat_acc = acc.get_threat_mut(p);
                threat_acc.fill(0);
                threat_features::for_each_active_threat_index(pos, perspective, king_sq, |idx| {
                    self.add_threat_weights(threat_acc, idx);
                });
            }
        }

        acc.computed_accumulation = true;
        acc.computed_score = false;
    }

    /// 差分計算でAccumulatorを更新
    pub fn update_accumulator(
        &self,
        pos: &Position,
        dirty_piece: &DirtyPiece,
        acc: &mut AccumulatorLayerStacks,
        prev_acc: &AccumulatorLayerStacks,
    ) {
        for perspective in [Color::Black, Color::White] {
            let p = perspective as usize;
            let reset = HalfKA_hm_FeatureSet::needs_refresh(dirty_piece, perspective);

            if reset {
                // 玉が移動した場合は全計算
                let accumulation = acc.get_mut(p);
                accumulation.copy_from_slice(&self.biases.0);

                let mut active_indices = IndexList::new();
                append_active_indices(pos, perspective, &mut active_indices);
                for index in active_indices.iter() {
                    self.add_weights(accumulation, index);
                }

                if self.has_psqt {
                    self.refresh_psqt(&active_indices, &mut acc.psqt_accumulation[p]);
                }
            } else {
                // 差分更新
                let mut removed = IndexList::new();
                let mut added = IndexList::new();
                append_changed_indices(
                    dirty_piece,
                    perspective,
                    pos.king_square(perspective),
                    &mut removed,
                    &mut added,
                );

                let prev = prev_acc.get(p);
                let curr = acc.get_mut(p);
                curr.copy_from_slice(prev);
                if !self.try_apply_dirty_piece_fast(
                    curr,
                    dirty_piece,
                    perspective,
                    pos.king_square(perspective),
                ) {
                    for index in removed.iter() {
                        self.sub_weights(curr, index);
                    }

                    for index in added.iter() {
                        self.add_weights(curr, index);
                    }
                }

                // PSQT 差分更新
                if self.has_psqt {
                    acc.psqt_accumulation[p] = prev_acc.psqt_accumulation[p];
                    for index in removed.iter() {
                        self.sub_psqt_weights(&mut acc.psqt_accumulation[p], index);
                    }
                    for index in added.iter() {
                        self.add_psqt_weights(&mut acc.psqt_accumulation[p], index);
                    }
                }
            }

            // Threat 更新
            if self.has_threat {
                let king_sq = pos.king_square(perspective);
                if reset {
                    // 玉が移動した場合は全計算
                    let threat_acc = acc.get_threat_mut(p);
                    threat_acc.fill(0);
                    threat_features::for_each_active_threat_index(
                        pos,
                        perspective,
                        king_sq,
                        |idx| {
                            self.add_threat_weights(threat_acc, idx);
                        },
                    );
                } else {
                    // Threat 差分更新
                    let prev_threat = prev_acc.get_threat(p);
                    let curr_threat = acc.get_threat_mut(p);
                    curr_threat.copy_from_slice(prev_threat);

                    let mut t_removed = IndexList::<MAX_CHANGED_THREAT_FEATURES>::new();
                    let mut t_added = IndexList::<MAX_CHANGED_THREAT_FEATURES>::new();
                    let ok = threat_features::append_changed_threat_indices(
                        pos,
                        dirty_piece,
                        perspective,
                        king_sq,
                        &mut t_removed,
                        &mut t_added,
                    );
                    if ok {
                        for idx in t_removed.iter() {
                            self.sub_threat_weights(curr_threat, idx);
                        }
                        for idx in t_added.iter() {
                            self.add_threat_weights(curr_threat, idx);
                        }
                    } else {
                        // overflow → full refresh
                        curr_threat.fill(0);
                        threat_features::for_each_active_threat_index(
                            pos,
                            perspective,
                            king_sq,
                            |idx| {
                                self.add_threat_weights(curr_threat, idx);
                            },
                        );
                    }
                }
            }
        }

        acc.computed_accumulation = true;
        acc.computed_score = false;
    }

    /// 差分計算でAccumulatorを更新（キャッシュ使用版）
    ///
    /// 玉移動時に full refresh が必要な視点では、AccumulatorCaches（Finny Tables）
    /// を参照して差分更新を行う。キャッシュにヒットした場合、全駒加算の代わりに
    /// 前回のキャッシュ状態との差分のみを適用するため高速。
    pub fn update_accumulator_with_cache(
        &self,
        pos: &Position,
        dirty_piece: &DirtyPiece,
        acc: &mut AccumulatorLayerStacks,
        prev_acc: &AccumulatorLayerStacks,
        cache: &mut AccumulatorCacheLayerStacks,
    ) {
        for perspective in [Color::Black, Color::White] {
            let p = perspective as usize;
            let reset = HalfKA_hm_FeatureSet::needs_refresh(dirty_piece, perspective);

            if reset {
                // 玉が移動した場合はキャッシュ経由で refresh
                self.refresh_perspective_with_cache(pos, perspective, acc.get_mut(p), cache);

                // PSQT はキャッシュ非対象なのでフル再計算
                if self.has_psqt {
                    let mut active_indices = IndexList::new();
                    append_active_indices(pos, perspective, &mut active_indices);
                    self.refresh_psqt(&active_indices, &mut acc.psqt_accumulation[p]);
                }
            } else {
                // 差分更新（キャッシュ不使用）
                let mut removed = IndexList::new();
                let mut added = IndexList::new();
                append_changed_indices(
                    dirty_piece,
                    perspective,
                    pos.king_square(perspective),
                    &mut removed,
                    &mut added,
                );

                let prev = prev_acc.get(p);
                let curr = acc.get_mut(p);
                curr.copy_from_slice(prev);
                if !self.try_apply_dirty_piece_fast(
                    curr,
                    dirty_piece,
                    perspective,
                    pos.king_square(perspective),
                ) {
                    for index in removed.iter() {
                        self.sub_weights(curr, index);
                    }

                    for index in added.iter() {
                        self.add_weights(curr, index);
                    }
                }

                // PSQT 差分更新
                if self.has_psqt {
                    acc.psqt_accumulation[p] = prev_acc.psqt_accumulation[p];
                    for index in removed.iter() {
                        self.sub_psqt_weights(&mut acc.psqt_accumulation[p], index);
                    }
                    for index in added.iter() {
                        self.add_psqt_weights(&mut acc.psqt_accumulation[p], index);
                    }
                }
            }

            // Threat 更新（キャッシュ版も非キャッシュ版と同じロジック）
            if self.has_threat {
                let king_sq = pos.king_square(perspective);
                if reset {
                    let threat_acc = acc.get_threat_mut(p);
                    threat_acc.fill(0);
                    threat_features::for_each_active_threat_index(
                        pos,
                        perspective,
                        king_sq,
                        |idx| {
                            self.add_threat_weights(threat_acc, idx);
                        },
                    );
                } else {
                    let prev_threat = prev_acc.get_threat(p);
                    let curr_threat = acc.get_threat_mut(p);
                    curr_threat.copy_from_slice(prev_threat);

                    let mut t_removed = IndexList::<MAX_CHANGED_THREAT_FEATURES>::new();
                    let mut t_added = IndexList::<MAX_CHANGED_THREAT_FEATURES>::new();
                    let ok = threat_features::append_changed_threat_indices(
                        pos,
                        dirty_piece,
                        perspective,
                        king_sq,
                        &mut t_removed,
                        &mut t_added,
                    );
                    if ok {
                        for idx in t_removed.iter() {
                            self.sub_threat_weights(curr_threat, idx);
                        }
                        for idx in t_added.iter() {
                            self.add_threat_weights(curr_threat, idx);
                        }
                    } else {
                        curr_threat.fill(0);
                        threat_features::for_each_active_threat_index(
                            pos,
                            perspective,
                            king_sq,
                            |idx| {
                                self.add_threat_weights(curr_threat, idx);
                            },
                        );
                    }
                }
            }
        }

        acc.computed_accumulation = true;
        acc.computed_score = false;
    }

    /// キャッシュ使用版の refresh（両視点）
    pub fn refresh_accumulator_with_cache(
        &self,
        pos: &Position,
        acc: &mut AccumulatorLayerStacks,
        cache: &mut AccumulatorCacheLayerStacks,
    ) {
        for perspective in [Color::Black, Color::White] {
            let p = perspective as usize;
            self.refresh_perspective_with_cache(pos, perspective, acc.get_mut(p), cache);

            // PSQT はキャッシュ非対象なのでフル再計算
            if self.has_psqt {
                let mut active_indices = IndexList::new();
                append_active_indices(pos, perspective, &mut active_indices);
                self.refresh_psqt(&active_indices, &mut acc.psqt_accumulation[p]);
            }

            // Threat はキャッシュ非対象なのでフル再計算
            if self.has_threat {
                let king_sq = pos.king_square(perspective);
                let threat_acc = acc.get_threat_mut(p);
                threat_acc.fill(0);
                threat_features::for_each_active_threat_index(pos, perspective, king_sq, |idx| {
                    self.add_threat_weights(threat_acc, idx);
                });
            }
        }

        acc.computed_accumulation = true;
        acc.computed_score = false;
    }

    /// 単一視点のキャッシュ経由 refresh
    ///
    /// アクティブ特徴量をソートして u32 配列に変換し、
    /// AccumulatorCacheLayerStacks::refresh_or_cache に委譲する。
    fn refresh_perspective_with_cache(
        &self,
        pos: &Position,
        perspective: Color,
        accumulation: &mut [i16; NNUE_PYTORCH_L1],
        cache: &mut AccumulatorCacheLayerStacks,
    ) {
        let king_sq = pos.king_square(perspective);
        let mut active_indices = IndexList::new();
        append_active_indices(pos, perspective, &mut active_indices);

        // 使用領域だけ初期化して、全 zero fill を避ける。
        let mut sorted_buf = [const { MaybeUninit::<u32>::uninit() }; MAX_ACTIVE_FEATURES];
        let len = active_indices.len();
        for (slot, idx) in sorted_buf[..len].iter_mut().zip(active_indices.iter()) {
            slot.write(idx as u32);
        }
        // SAFETY:
        // - `sorted_buf[..len]` は直前のループで全要素を初期化済み。
        // - `MaybeUninit<u32>` は `u32` と同じレイアウト・アライメントを持つ。
        // - `len <= MAX_ACTIVE_FEATURES` は `IndexList` の不変条件から保証される。
        let sorted =
            unsafe { std::slice::from_raw_parts_mut(sorted_buf.as_mut_ptr() as *mut u32, len) };
        sorted.sort_unstable();

        cache.refresh_or_cache(
            king_sq,
            perspective,
            sorted,
            &self.biases.0,
            accumulation,
            |acc, idx| self.add_weights(acc, idx),
            |acc, idx| self.sub_weights(acc, idx),
        );
    }

    /// 複数手分の差分を適用してアキュムレータを更新
    pub fn forward_update_incremental(
        &self,
        pos: &Position,
        stack: &mut AccumulatorStackLayerStacks,
        source_idx: usize,
    ) -> bool {
        let Some(path) = stack.collect_path(source_idx) else {
            // パスが途切れた場合、または MAX_PATH_LENGTH を超えた場合
            return false;
        };

        // source_acc から main + psqt + threat の全てをコピー
        let source_acc = stack.entry_at(source_idx).accumulator.clone();
        {
            let current_acc = &mut stack.current_mut().accumulator;
            for perspective in [Color::Black, Color::White] {
                let p = perspective as usize;
                current_acc.get_mut(p).copy_from_slice(source_acc.get(p));
                current_acc.psqt_accumulation[p] = source_acc.psqt_accumulation[p];
                if self.has_threat {
                    current_acc.get_threat_mut(p).copy_from_slice(source_acc.get_threat(p));
                }
            }
        }

        for entry_idx in path.iter() {
            let dirty_piece = stack.entry_at(entry_idx).dirty_piece;

            for perspective in [Color::Black, Color::White] {
                debug_assert!(
                    !dirty_piece.king_moved[perspective.index()],
                    "King moved between source and current"
                );

                let king_sq = pos.king_square(perspective);
                let mut removed = IndexList::new();
                let mut added = IndexList::new();
                append_changed_indices(
                    &dirty_piece,
                    perspective,
                    king_sq,
                    &mut removed,
                    &mut added,
                );

                let p = perspective as usize;
                let accumulation = stack.current_mut().accumulator.get_mut(p);
                if !self.try_apply_dirty_piece_fast(
                    accumulation,
                    &dirty_piece,
                    perspective,
                    king_sq,
                ) {
                    for index in removed.iter() {
                        self.sub_weights(accumulation, index);
                    }
                    for index in added.iter() {
                        self.add_weights(accumulation, index);
                    }
                }

                // PSQT 差分更新
                // try_apply_dirty_piece_fast は main path 専用なので、
                // PSQT は removed/added を必ず明示的に適用する。
                if self.has_psqt {
                    let psqt_acc = &mut stack.current_mut().accumulator.psqt_accumulation[p];
                    for index in removed.iter() {
                        self.sub_psqt_weights(psqt_acc, index);
                    }
                    for index in added.iter() {
                        self.add_psqt_weights(psqt_acc, index);
                    }
                }
            }
        }

        // Threat: forward_update_incremental では中間局面を再構成できないため、
        // 最終局面 (pos) で full refresh する。
        // append_changed_threat_indices は pos (after state) を参照するため、
        // 複数手パスで中間手の差分を正しく計算できない。
        if self.has_threat {
            for perspective in [Color::Black, Color::White] {
                let p = perspective as usize;
                let king_sq = pos.king_square(perspective);
                let threat_acc = stack.current_mut().accumulator.get_threat_mut(p);
                threat_acc.fill(0);
                threat_features::for_each_active_threat_index(pos, perspective, king_sq, |idx| {
                    self.add_threat_weights(threat_acc, idx);
                });
            }
        }

        stack.current_mut().accumulator.computed_accumulation = true;
        stack.current_mut().accumulator.computed_score = false;
        true
    }

    /// 重みを累積値に加算（SIMD最適化版）
    ///
    /// 1536 i16 要素を SIMD で加算。AVX512BW/AVX2/SSE2/WASM SIMD128 に対応。
    /// weights と accumulation は 64 バイトアラインされている前提で aligned load/store を使用。
    #[inline]
    fn add_weights(&self, accumulation: &mut [i16; NNUE_PYTORCH_L1], index: usize) {
        let weights = self.weight_row(index);

        // AVX-512 BW: 512bit = 32 x i16, 1536/32 = 48 iterations
        #[cfg(all(
            target_arch = "x86_64",
            target_feature = "avx512f",
            target_feature = "avx512bw"
        ))]
        {
            // SAFETY:
            // - weights: AlignedBox で 64 バイトアライン、各行は 3072 バイト (64の倍数)
            // - accumulation: Aligned<[i16; 1536]> で 64 バイトアライン
            // - 1536 要素 = 32 要素 × 48 回のループで完全にカバー
            unsafe {
                use std::arch::x86_64::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let weight_ptr = weights.as_ptr();

                for i in 0..48 {
                    let acc_vec = _mm512_load_si512(acc_ptr.add(i * 32) as *const __m512i);
                    let weight_vec = _mm512_load_si512(weight_ptr.add(i * 32) as *const __m512i);
                    let result = _mm512_add_epi16(acc_vec, weight_vec);
                    _mm512_store_si512(acc_ptr.add(i * 32) as *mut __m512i, result);
                }
            }
            return;
        }

        // AVX2: 256bit = 16 x i16, 1536/16 = 96 iterations
        #[cfg(all(
            target_arch = "x86_64",
            target_feature = "avx2",
            not(target_feature = "avx512bw")
        ))]
        {
            // SAFETY:
            // - weights: AlignedBox で 64 バイトアライン、各行は 3072 バイト (64の倍数)
            // - accumulation: Aligned<[i16; 1536]> で 64 バイトアライン
            // - 1536 要素 = 16 要素 × 96 回のループで完全にカバー
            unsafe {
                use std::arch::x86_64::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let weight_ptr = weights.as_ptr();

                for i in 0..96 {
                    let acc_vec = _mm256_load_si256(acc_ptr.add(i * 16) as *const __m256i);
                    let weight_vec = _mm256_load_si256(weight_ptr.add(i * 16) as *const __m256i);
                    let result = _mm256_add_epi16(acc_vec, weight_vec);
                    _mm256_store_si256(acc_ptr.add(i * 16) as *mut __m256i, result);
                }
            }
            return;
        }

        // SSE2: 128bit = 8 x i16, 1536/8 = 192 iterations
        #[cfg(all(
            target_arch = "x86_64",
            target_feature = "sse2",
            not(target_feature = "avx2")
        ))]
        {
            // SAFETY: 同上（16バイトアライン）
            unsafe {
                use std::arch::x86_64::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let weight_ptr = weights.as_ptr();

                for i in 0..192 {
                    let acc_vec = _mm_load_si128(acc_ptr.add(i * 8) as *const __m128i);
                    let weight_vec = _mm_load_si128(weight_ptr.add(i * 8) as *const __m128i);
                    let result = _mm_add_epi16(acc_vec, weight_vec);
                    _mm_store_si128(acc_ptr.add(i * 8) as *mut __m128i, result);
                }
            }
            return;
        }

        // WASM SIMD128: 128bit = 8 x i16, 1536/8 = 192 iterations
        #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
        {
            // SAFETY: WASM SIMD128 はアライメント不要
            unsafe {
                use std::arch::wasm32::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let weight_ptr = weights.as_ptr();

                for i in 0..192 {
                    let acc_vec = v128_load(acc_ptr.add(i * 8) as *const v128);
                    let weight_vec = v128_load(weight_ptr.add(i * 8) as *const v128);
                    let result = i16x8_add(acc_vec, weight_vec);
                    v128_store(acc_ptr.add(i * 8) as *mut v128, result);
                }
            }
            return;
        }

        // スカラーフォールバック（非飽和演算）
        #[allow(unreachable_code)]
        for (acc, &weight) in accumulation.iter_mut().zip(weights) {
            *acc = acc.wrapping_add(weight);
        }
    }

    #[inline]
    fn weight_row(&self, index: usize) -> &[i16] {
        let Some(offset) = index.checked_mul(NNUE_PYTORCH_L1) else {
            feature_index_oob(index, self.weights.len() / NNUE_PYTORCH_L1);
        };
        let Some(end) = offset.checked_add(NNUE_PYTORCH_L1) else {
            feature_index_oob(index, self.weights.len() / NNUE_PYTORCH_L1);
        };
        if end > self.weights.len() {
            feature_index_oob(index, self.weights.len() / NNUE_PYTORCH_L1);
        }
        &self.weights[offset..end]
    }

    #[inline]
    fn try_apply_dirty_piece_fast(
        &self,
        accumulation: &mut [i16; NNUE_PYTORCH_L1],
        dirty_piece: &DirtyPiece,
        perspective: Color,
        king_sq: crate::types::Square,
    ) -> bool {
        let changed = &dirty_piece.changed_piece;
        let old_new = |idx: usize| {
            let entry = &changed[idx];
            let old_bp = if perspective == Color::Black {
                entry.old_piece.fb
            } else {
                entry.old_piece.fw
            };
            let new_bp = if perspective == Color::Black {
                entry.new_piece.fb
            } else {
                entry.new_piece.fw
            };
            (old_bp, new_bp)
        };

        // dirty_num==1: 駒の移動（非捕獲）。打ち駒は old_bp==ZERO のためフォールバック。
        // dirty_num==2: 駒を取る指し手のみ。全 BonaPiece は非 ZERO のはずだが、
        //               ZERO チェックでフォールバックを保証する。
        // dirty_num==0: パス手（盤面変化なし）。_ => false でフォールバック。
        match dirty_piece.dirty_num as usize {
            1 => {
                let (old_bp, new_bp) = old_new(0);
                if old_bp != BonaPiece::ZERO && new_bp != BonaPiece::ZERO {
                    self.apply_sub_add_fused(
                        accumulation,
                        feature_index_from_bona_piece(old_bp, perspective, king_sq),
                        feature_index_from_bona_piece(new_bp, perspective, king_sq),
                    );
                    true
                } else {
                    false
                }
            }
            2 => {
                let (old_bp0, new_bp0) = old_new(0);
                let (old_bp1, new_bp1) = old_new(1);
                if old_bp0 != BonaPiece::ZERO
                    && new_bp0 != BonaPiece::ZERO
                    && old_bp1 != BonaPiece::ZERO
                    && new_bp1 != BonaPiece::ZERO
                {
                    self.apply_double_sub_add_fused(
                        accumulation,
                        feature_index_from_bona_piece(old_bp0, perspective, king_sq),
                        feature_index_from_bona_piece(new_bp0, perspective, king_sq),
                        feature_index_from_bona_piece(old_bp1, perspective, king_sq),
                        feature_index_from_bona_piece(new_bp1, perspective, king_sq),
                    );
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    #[inline]
    fn apply_sub_add_fused(
        &self,
        accumulation: &mut [i16; NNUE_PYTORCH_L1],
        sub_index: usize,
        add_index: usize,
    ) {
        let sub_weights = self.weight_row(sub_index);
        let add_weights = self.weight_row(add_index);

        #[cfg(all(
            target_arch = "x86_64",
            target_feature = "avx512f",
            target_feature = "avx512bw"
        ))]
        {
            // SAFETY:
            // - accumulation は Aligned<[i16; 1536]> 由来で 64 バイトアライン。
            // - weight row: AlignedBox の先頭が 64 バイトアライン、各行は
            //   NNUE_PYTORCH_L1(1536) × sizeof(i16)(2) = 3072 バイト = 64 × 48 なので
            //   全行の先頭も 64 バイト境界に揃う。
            // - 1536 要素を 32 要素ずつ 48 回で完全に走査する。
            unsafe {
                use std::arch::x86_64::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let sub_ptr = sub_weights.as_ptr();
                let add_ptr = add_weights.as_ptr();

                for i in 0..48 {
                    let acc_vec = _mm512_load_si512(acc_ptr.add(i * 32) as *const __m512i);
                    let sub_vec = _mm512_load_si512(sub_ptr.add(i * 32) as *const __m512i);
                    let add_vec = _mm512_load_si512(add_ptr.add(i * 32) as *const __m512i);
                    let result = _mm512_add_epi16(_mm512_sub_epi16(acc_vec, sub_vec), add_vec);
                    _mm512_store_si512(acc_ptr.add(i * 32) as *mut __m512i, result);
                }
            }
            return;
        }

        #[cfg(all(
            target_arch = "x86_64",
            target_feature = "avx2",
            not(target_feature = "avx512bw")
        ))]
        {
            // SAFETY:
            // - accumulation / weight row はともに 32 バイトアライン（3072 = 32 × 96）。
            // - 1536 要素を 16 要素ずつ 96 回で完全に走査する。
            unsafe {
                use std::arch::x86_64::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let sub_ptr = sub_weights.as_ptr();
                let add_ptr = add_weights.as_ptr();

                for i in 0..96 {
                    let acc_vec = _mm256_load_si256(acc_ptr.add(i * 16) as *const __m256i);
                    let sub_vec = _mm256_load_si256(sub_ptr.add(i * 16) as *const __m256i);
                    let add_vec = _mm256_load_si256(add_ptr.add(i * 16) as *const __m256i);
                    let result = _mm256_add_epi16(_mm256_sub_epi16(acc_vec, sub_vec), add_vec);
                    _mm256_store_si256(acc_ptr.add(i * 16) as *mut __m256i, result);
                }
            }
            return;
        }

        #[cfg(all(
            target_arch = "x86_64",
            target_feature = "sse2",
            not(target_feature = "avx2")
        ))]
        {
            // SAFETY:
            // - accumulation / weight row は 16 バイト境界にある。
            // - 1536 要素を 8 要素ずつ 192 回で完全に走査する。
            unsafe {
                use std::arch::x86_64::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let sub_ptr = sub_weights.as_ptr();
                let add_ptr = add_weights.as_ptr();

                for i in 0..192 {
                    let acc_vec = _mm_load_si128(acc_ptr.add(i * 8) as *const __m128i);
                    let sub_vec = _mm_load_si128(sub_ptr.add(i * 8) as *const __m128i);
                    let add_vec = _mm_load_si128(add_ptr.add(i * 8) as *const __m128i);
                    let result = _mm_add_epi16(_mm_sub_epi16(acc_vec, sub_vec), add_vec);
                    _mm_store_si128(acc_ptr.add(i * 8) as *mut __m128i, result);
                }
            }
            return;
        }

        #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
        {
            // SAFETY:
            // - WASM SIMD は unaligned load/store を許容する。
            // - 1536 要素を 8 要素ずつ 192 回で完全に走査する。
            unsafe {
                use std::arch::wasm32::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let sub_ptr = sub_weights.as_ptr();
                let add_ptr = add_weights.as_ptr();

                for i in 0..192 {
                    let acc_vec = v128_load(acc_ptr.add(i * 8) as *const v128);
                    let sub_vec = v128_load(sub_ptr.add(i * 8) as *const v128);
                    let add_vec = v128_load(add_ptr.add(i * 8) as *const v128);
                    let result = i16x8_add(i16x8_sub(acc_vec, sub_vec), add_vec);
                    v128_store(acc_ptr.add(i * 8) as *mut v128, result);
                }
            }
            return;
        }

        #[allow(unreachable_code)]
        for ((acc, &sub_weight), &add_weight) in
            accumulation.iter_mut().zip(sub_weights.iter()).zip(add_weights.iter())
        {
            *acc = acc.wrapping_sub(sub_weight).wrapping_add(add_weight);
        }
    }

    #[inline]
    fn apply_double_sub_add_fused(
        &self,
        accumulation: &mut [i16; NNUE_PYTORCH_L1],
        sub_index0: usize,
        add_index0: usize,
        sub_index1: usize,
        add_index1: usize,
    ) {
        let sub_weights0 = self.weight_row(sub_index0);
        let add_weights0 = self.weight_row(add_index0);
        let sub_weights1 = self.weight_row(sub_index1);
        let add_weights1 = self.weight_row(add_index1);

        #[cfg(all(
            target_arch = "x86_64",
            target_feature = "avx512f",
            target_feature = "avx512bw"
        ))]
        {
            // SAFETY:
            // - accumulation は Aligned<[i16; 1536]> 由来で 64 バイトアライン。
            // - weight row: AlignedBox の先頭が 64 バイトアライン、各行は
            //   NNUE_PYTORCH_L1(1536) × sizeof(i16)(2) = 3072 バイト = 64 × 48 なので
            //   全行の先頭も 64 バイト境界に揃う。
            // - 1536 要素を 32 要素ずつ 48 回で完全に走査する。
            unsafe {
                use std::arch::x86_64::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let sub_ptr0 = sub_weights0.as_ptr();
                let add_ptr0 = add_weights0.as_ptr();
                let sub_ptr1 = sub_weights1.as_ptr();
                let add_ptr1 = add_weights1.as_ptr();

                for i in 0..48 {
                    let acc_vec = _mm512_load_si512(acc_ptr.add(i * 32) as *const __m512i);
                    let sub_vec0 = _mm512_load_si512(sub_ptr0.add(i * 32) as *const __m512i);
                    let add_vec0 = _mm512_load_si512(add_ptr0.add(i * 32) as *const __m512i);
                    let sub_vec1 = _mm512_load_si512(sub_ptr1.add(i * 32) as *const __m512i);
                    let add_vec1 = _mm512_load_si512(add_ptr1.add(i * 32) as *const __m512i);
                    let result = _mm512_add_epi16(
                        _mm512_add_epi16(_mm512_sub_epi16(acc_vec, sub_vec0), add_vec0),
                        _mm512_sub_epi16(add_vec1, sub_vec1),
                    );
                    _mm512_store_si512(acc_ptr.add(i * 32) as *mut __m512i, result);
                }
            }
            return;
        }

        #[cfg(all(
            target_arch = "x86_64",
            target_feature = "avx2",
            not(target_feature = "avx512bw")
        ))]
        {
            // SAFETY:
            // - accumulation / 4 本の weight row はともに 32 バイトアライン（3072 = 32 × 96）。
            // - 1536 要素を 16 要素ずつ 96 回で完全に走査する。
            unsafe {
                use std::arch::x86_64::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let sub_ptr0 = sub_weights0.as_ptr();
                let add_ptr0 = add_weights0.as_ptr();
                let sub_ptr1 = sub_weights1.as_ptr();
                let add_ptr1 = add_weights1.as_ptr();

                for i in 0..96 {
                    let acc_vec = _mm256_load_si256(acc_ptr.add(i * 16) as *const __m256i);
                    let sub_vec0 = _mm256_load_si256(sub_ptr0.add(i * 16) as *const __m256i);
                    let add_vec0 = _mm256_load_si256(add_ptr0.add(i * 16) as *const __m256i);
                    let sub_vec1 = _mm256_load_si256(sub_ptr1.add(i * 16) as *const __m256i);
                    let add_vec1 = _mm256_load_si256(add_ptr1.add(i * 16) as *const __m256i);
                    let result = _mm256_add_epi16(
                        _mm256_add_epi16(_mm256_sub_epi16(acc_vec, sub_vec0), add_vec0),
                        _mm256_sub_epi16(add_vec1, sub_vec1),
                    );
                    _mm256_store_si256(acc_ptr.add(i * 16) as *mut __m256i, result);
                }
            }
            return;
        }

        #[cfg(all(
            target_arch = "x86_64",
            target_feature = "sse2",
            not(target_feature = "avx2")
        ))]
        {
            // SAFETY:
            // - accumulation / 4 本の weight row は 16 バイト境界にある。
            // - 1536 要素を 8 要素ずつ 192 回で完全に走査する。
            unsafe {
                use std::arch::x86_64::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let sub_ptr0 = sub_weights0.as_ptr();
                let add_ptr0 = add_weights0.as_ptr();
                let sub_ptr1 = sub_weights1.as_ptr();
                let add_ptr1 = add_weights1.as_ptr();

                for i in 0..192 {
                    let acc_vec = _mm_load_si128(acc_ptr.add(i * 8) as *const __m128i);
                    let sub_vec0 = _mm_load_si128(sub_ptr0.add(i * 8) as *const __m128i);
                    let add_vec0 = _mm_load_si128(add_ptr0.add(i * 8) as *const __m128i);
                    let sub_vec1 = _mm_load_si128(sub_ptr1.add(i * 8) as *const __m128i);
                    let add_vec1 = _mm_load_si128(add_ptr1.add(i * 8) as *const __m128i);
                    let result = _mm_add_epi16(
                        _mm_add_epi16(_mm_sub_epi16(acc_vec, sub_vec0), add_vec0),
                        _mm_sub_epi16(add_vec1, sub_vec1),
                    );
                    _mm_store_si128(acc_ptr.add(i * 8) as *mut __m128i, result);
                }
            }
            return;
        }

        #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
        {
            // SAFETY:
            // - WASM SIMD は unaligned load/store を許容する。
            // - 1536 要素を 8 要素ずつ 192 回で完全に走査する。
            unsafe {
                use std::arch::wasm32::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let sub_ptr0 = sub_weights0.as_ptr();
                let add_ptr0 = add_weights0.as_ptr();
                let sub_ptr1 = sub_weights1.as_ptr();
                let add_ptr1 = add_weights1.as_ptr();

                for i in 0..192 {
                    let acc_vec = v128_load(acc_ptr.add(i * 8) as *const v128);
                    let sub_vec0 = v128_load(sub_ptr0.add(i * 8) as *const v128);
                    let add_vec0 = v128_load(add_ptr0.add(i * 8) as *const v128);
                    let sub_vec1 = v128_load(sub_ptr1.add(i * 8) as *const v128);
                    let add_vec1 = v128_load(add_ptr1.add(i * 8) as *const v128);
                    let result = i16x8_add(
                        i16x8_add(i16x8_sub(acc_vec, sub_vec0), add_vec0),
                        i16x8_sub(add_vec1, sub_vec1),
                    );
                    v128_store(acc_ptr.add(i * 8) as *mut v128, result);
                }
            }
            return;
        }

        #[allow(unreachable_code)]
        for ((((acc, &sub_weight0), &add_weight0), &sub_weight1), &add_weight1) in accumulation
            .iter_mut()
            .zip(sub_weights0.iter())
            .zip(add_weights0.iter())
            .zip(sub_weights1.iter())
            .zip(add_weights1.iter())
        {
            *acc = acc
                .wrapping_sub(sub_weight0)
                .wrapping_add(add_weight0)
                .wrapping_sub(sub_weight1)
                .wrapping_add(add_weight1);
        }
    }

    /// 重みを累積値から減算（SIMD最適化版）
    ///
    /// 1536 i16 要素を SIMD で減算。AVX512BW/AVX2/SSE2/WASM SIMD128 に対応。
    /// weights と accumulation は 64 バイトアラインされている前提で aligned load/store を使用。
    #[inline]
    fn sub_weights(&self, accumulation: &mut [i16; NNUE_PYTORCH_L1], index: usize) {
        let weights = self.weight_row(index);

        // AVX-512 BW: 512bit = 32 x i16, 1536/32 = 48 iterations
        #[cfg(all(
            target_arch = "x86_64",
            target_feature = "avx512f",
            target_feature = "avx512bw"
        ))]
        {
            // SAFETY: add_weights と同様
            unsafe {
                use std::arch::x86_64::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let weight_ptr = weights.as_ptr();

                for i in 0..48 {
                    let acc_vec = _mm512_load_si512(acc_ptr.add(i * 32) as *const __m512i);
                    let weight_vec = _mm512_load_si512(weight_ptr.add(i * 32) as *const __m512i);
                    let result = _mm512_sub_epi16(acc_vec, weight_vec);
                    _mm512_store_si512(acc_ptr.add(i * 32) as *mut __m512i, result);
                }
            }
            return;
        }

        // AVX2: 256bit = 16 x i16, 1536/16 = 96 iterations
        #[cfg(all(
            target_arch = "x86_64",
            target_feature = "avx2",
            not(target_feature = "avx512bw")
        ))]
        {
            // SAFETY: add_weights と同様
            unsafe {
                use std::arch::x86_64::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let weight_ptr = weights.as_ptr();

                for i in 0..96 {
                    let acc_vec = _mm256_load_si256(acc_ptr.add(i * 16) as *const __m256i);
                    let weight_vec = _mm256_load_si256(weight_ptr.add(i * 16) as *const __m256i);
                    let result = _mm256_sub_epi16(acc_vec, weight_vec);
                    _mm256_store_si256(acc_ptr.add(i * 16) as *mut __m256i, result);
                }
            }
            return;
        }

        // SSE2: 128bit = 8 x i16, 1536/8 = 192 iterations
        #[cfg(all(
            target_arch = "x86_64",
            target_feature = "sse2",
            not(target_feature = "avx2")
        ))]
        {
            // SAFETY: 同上（16バイトアライン）
            unsafe {
                use std::arch::x86_64::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let weight_ptr = weights.as_ptr();

                for i in 0..192 {
                    let acc_vec = _mm_load_si128(acc_ptr.add(i * 8) as *const __m128i);
                    let weight_vec = _mm_load_si128(weight_ptr.add(i * 8) as *const __m128i);
                    let result = _mm_sub_epi16(acc_vec, weight_vec);
                    _mm_store_si128(acc_ptr.add(i * 8) as *mut __m128i, result);
                }
            }
            return;
        }

        // WASM SIMD128: 128bit = 8 x i16, 1536/8 = 192 iterations
        #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
        {
            // SAFETY: WASM SIMD128 はアライメント不要
            unsafe {
                use std::arch::wasm32::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let weight_ptr = weights.as_ptr();

                for i in 0..192 {
                    let acc_vec = v128_load(acc_ptr.add(i * 8) as *const v128);
                    let weight_vec = v128_load(weight_ptr.add(i * 8) as *const v128);
                    let result = i16x8_sub(acc_vec, weight_vec);
                    v128_store(acc_ptr.add(i * 8) as *mut v128, result);
                }
            }
            return;
        }

        // スカラーフォールバック（非飽和演算）
        #[allow(unreachable_code)]
        for (acc, &weight) in accumulation.iter_mut().zip(weights) {
            *acc = acc.wrapping_sub(weight);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nnue::accumulator::ChangedBonaPiece;
    use crate::nnue::bona_piece::ExtBonaPiece;
    use crate::nnue::piece_list::PieceNumber;
    use crate::types::{File, Piece, PieceType, Rank, Square};

    fn make_test_transformer() -> FeatureTransformerLayerStacks {
        FeatureTransformerLayerStacks {
            biases: Aligned([0; NNUE_PYTORCH_L1]),
            weights: AlignedBox::new_zeroed(HALFKA_HM_DIMENSIONS * NNUE_PYTORCH_L1),
            psqt_biases: [0; NUM_LAYER_STACK_BUCKETS],
            psqt_weights: AlignedBox::new_zeroed(0),
            has_psqt: false,
            threat_weights: AlignedBox::new_zeroed(0),
            has_threat: false,
        }
    }

    fn fill_weight_row(ft: &mut FeatureTransformerLayerStacks, index: usize, seed: i16) {
        let start = index * NNUE_PYTORCH_L1;
        for (i, slot) in ft.weights[start..start + NNUE_PYTORCH_L1].iter_mut().enumerate() {
            *slot = seed.wrapping_add((i % 29) as i16);
        }
    }

    fn apply_generic(
        ft: &FeatureTransformerLayerStacks,
        accumulation: &mut [i16; NNUE_PYTORCH_L1],
        dirty_piece: &DirtyPiece,
        perspective: Color,
        king_sq: Square,
    ) {
        let mut removed = IndexList::new();
        let mut added = IndexList::new();
        append_changed_indices(dirty_piece, perspective, king_sq, &mut removed, &mut added);
        for index in removed.iter() {
            ft.sub_weights(accumulation, index);
        }
        for index in added.iter() {
            ft.add_weights(accumulation, index);
        }
    }

    #[test]
    fn test_feature_transformer_dimensions() {
        // 次元数の確認
        assert_eq!(NNUE_PYTORCH_L1, 1536);
        assert_eq!(HALFKA_HM_DIMENSIONS, 73305);
    }

    #[test]
    fn test_try_apply_dirty_piece_fast_matches_generic_single_move() {
        let king_sq = Square::new(File::File5, Rank::Rank9);
        let mut ft = make_test_transformer();
        let mut dirty_piece = DirtyPiece::new();
        dirty_piece.dirty_num = 1;
        dirty_piece.piece_no[0] = PieceNumber(0);
        dirty_piece.changed_piece[0] = ChangedBonaPiece {
            old_piece: ExtBonaPiece::from_board(
                Piece::B_PAWN,
                Square::new(File::File7, Rank::Rank7),
            ),
            new_piece: ExtBonaPiece::from_board(
                Piece::B_PAWN,
                Square::new(File::File7, Rank::Rank6),
            ),
        };

        let old_index = feature_index_from_bona_piece(
            dirty_piece.changed_piece[0].old_piece.fb,
            Color::Black,
            king_sq,
        );
        let new_index = feature_index_from_bona_piece(
            dirty_piece.changed_piece[0].new_piece.fb,
            Color::Black,
            king_sq,
        );
        fill_weight_row(&mut ft, old_index, 11);
        fill_weight_row(&mut ft, new_index, 37);

        let mut generic = Aligned([5i16; NNUE_PYTORCH_L1]);
        let mut fast = Aligned([5i16; NNUE_PYTORCH_L1]);
        apply_generic(&ft, &mut generic.0, &dirty_piece, Color::Black, king_sq);
        assert!(ft.try_apply_dirty_piece_fast(&mut fast.0, &dirty_piece, Color::Black, king_sq));
        assert_eq!(generic.0, fast.0);
    }

    #[test]
    fn test_try_apply_dirty_piece_fast_matches_generic_capture() {
        let king_sq = Square::new(File::File5, Rank::Rank9);
        let mut ft = make_test_transformer();
        let mut dirty_piece = DirtyPiece::new();
        dirty_piece.dirty_num = 2;
        dirty_piece.piece_no[0] = PieceNumber(0);
        dirty_piece.changed_piece[0] = ChangedBonaPiece {
            old_piece: ExtBonaPiece::from_board(
                Piece::B_PAWN,
                Square::new(File::File2, Rank::Rank4),
            ),
            new_piece: ExtBonaPiece::from_board(
                Piece::B_PAWN,
                Square::new(File::File2, Rank::Rank3),
            ),
        };
        dirty_piece.piece_no[1] = PieceNumber(1);
        dirty_piece.changed_piece[1] = ChangedBonaPiece {
            old_piece: ExtBonaPiece::from_board(
                Piece::W_PAWN,
                Square::new(File::File2, Rank::Rank3),
            ),
            new_piece: ExtBonaPiece::from_hand(Color::Black, PieceType::Pawn, 1),
        };

        let indices = [
            feature_index_from_bona_piece(
                dirty_piece.changed_piece[0].old_piece.fb,
                Color::Black,
                king_sq,
            ),
            feature_index_from_bona_piece(
                dirty_piece.changed_piece[0].new_piece.fb,
                Color::Black,
                king_sq,
            ),
            feature_index_from_bona_piece(
                dirty_piece.changed_piece[1].old_piece.fb,
                Color::Black,
                king_sq,
            ),
            feature_index_from_bona_piece(
                dirty_piece.changed_piece[1].new_piece.fb,
                Color::Black,
                king_sq,
            ),
        ];
        for (seed, &index) in [13i16, 29, 43, 71].iter().zip(indices.iter()) {
            fill_weight_row(&mut ft, index, *seed);
        }

        let mut generic = Aligned([7i16; NNUE_PYTORCH_L1]);
        let mut fast = Aligned([7i16; NNUE_PYTORCH_L1]);
        apply_generic(&ft, &mut generic.0, &dirty_piece, Color::Black, king_sq);
        assert!(ft.try_apply_dirty_piece_fast(&mut fast.0, &dirty_piece, Color::Black, king_sq));
        assert_eq!(generic.0, fast.0);
    }

    #[test]
    fn test_try_apply_dirty_piece_fast_matches_generic_single_move_white() {
        // 後手視点: fw / king_sq.inverse() の分岐をカバー
        let king_sq = Square::new(File::File5, Rank::Rank1);
        let mut ft = make_test_transformer();
        let mut dirty_piece = DirtyPiece::new();
        dirty_piece.dirty_num = 1;
        dirty_piece.piece_no[0] = PieceNumber(0);
        dirty_piece.changed_piece[0] = ChangedBonaPiece {
            old_piece: ExtBonaPiece::from_board(
                Piece::W_PAWN,
                Square::new(File::File3, Rank::Rank3),
            ),
            new_piece: ExtBonaPiece::from_board(
                Piece::W_PAWN,
                Square::new(File::File3, Rank::Rank4),
            ),
        };

        let old_index = feature_index_from_bona_piece(
            dirty_piece.changed_piece[0].old_piece.fw,
            Color::White,
            king_sq,
        );
        let new_index = feature_index_from_bona_piece(
            dirty_piece.changed_piece[0].new_piece.fw,
            Color::White,
            king_sq,
        );
        fill_weight_row(&mut ft, old_index, 19);
        fill_weight_row(&mut ft, new_index, 53);

        let mut generic = Aligned([5i16; NNUE_PYTORCH_L1]);
        let mut fast = Aligned([5i16; NNUE_PYTORCH_L1]);
        apply_generic(&ft, &mut generic.0, &dirty_piece, Color::White, king_sq);
        assert!(ft.try_apply_dirty_piece_fast(&mut fast.0, &dirty_piece, Color::White, king_sq));
        assert_eq!(generic.0, fast.0);
    }

    #[test]
    fn test_try_apply_dirty_piece_fast_matches_generic_capture_white() {
        // 後手視点: dirty_num==2 の fw 分岐をカバー
        // 後手の角が先手の歩を取る想定
        let king_sq = Square::new(File::File5, Rank::Rank1);
        let mut ft = make_test_transformer();
        let mut dirty_piece = DirtyPiece::new();
        dirty_piece.dirty_num = 2;
        dirty_piece.piece_no[0] = PieceNumber(0);
        dirty_piece.changed_piece[0] = ChangedBonaPiece {
            old_piece: ExtBonaPiece::from_board(
                Piece::W_BISHOP,
                Square::new(File::File8, Rank::Rank2),
            ),
            new_piece: ExtBonaPiece::from_board(
                Piece::W_BISHOP,
                Square::new(File::File3, Rank::Rank7),
            ),
        };
        dirty_piece.piece_no[1] = PieceNumber(1);
        dirty_piece.changed_piece[1] = ChangedBonaPiece {
            old_piece: ExtBonaPiece::from_board(
                Piece::B_PAWN,
                Square::new(File::File3, Rank::Rank7),
            ),
            new_piece: ExtBonaPiece::from_hand(Color::White, PieceType::Pawn, 1),
        };

        let indices = [
            feature_index_from_bona_piece(
                dirty_piece.changed_piece[0].old_piece.fw,
                Color::White,
                king_sq,
            ),
            feature_index_from_bona_piece(
                dirty_piece.changed_piece[0].new_piece.fw,
                Color::White,
                king_sq,
            ),
            feature_index_from_bona_piece(
                dirty_piece.changed_piece[1].old_piece.fw,
                Color::White,
                king_sq,
            ),
            feature_index_from_bona_piece(
                dirty_piece.changed_piece[1].new_piece.fw,
                Color::White,
                king_sq,
            ),
        ];
        for (seed, &index) in [17i16, 31, 47, 67].iter().zip(indices.iter()) {
            fill_weight_row(&mut ft, index, *seed);
        }

        let mut generic = Aligned([7i16; NNUE_PYTORCH_L1]);
        let mut fast = Aligned([7i16; NNUE_PYTORCH_L1]);
        apply_generic(&ft, &mut generic.0, &dirty_piece, Color::White, king_sq);
        assert!(ft.try_apply_dirty_piece_fast(&mut fast.0, &dirty_piece, Color::White, king_sq));
        assert_eq!(generic.0, fast.0);
    }

    #[test]
    fn test_try_apply_dirty_piece_fast_returns_false_for_hand_only_change() {
        let king_sq = Square::new(File::File5, Rank::Rank9);
        let ft = make_test_transformer();
        let mut dirty_piece = DirtyPiece::new();
        dirty_piece.dirty_num = 1;
        dirty_piece.piece_no[0] = PieceNumber(0);
        dirty_piece.changed_piece[0] = ChangedBonaPiece {
            old_piece: ExtBonaPiece::ZERO,
            new_piece: ExtBonaPiece::from_hand(Color::Black, PieceType::Pawn, 1),
        };

        let mut accumulation = Aligned([0i16; NNUE_PYTORCH_L1]);
        assert!(!ft.try_apply_dirty_piece_fast(
            &mut accumulation.0,
            &dirty_piece,
            Color::Black,
            king_sq,
        ));
    }

    // =========================================================================
    // PSQT テスト
    // =========================================================================

    fn make_test_transformer_with_psqt() -> FeatureTransformerLayerStacks {
        let psqt_weight_count = HALFKA_HM_DIMENSIONS * NUM_LAYER_STACK_BUCKETS;
        let mut psqt_weights = AlignedBox::new_zeroed(psqt_weight_count);
        // 既知のパターンを設定: weight[feat][bucket] = (feat * 7 + bucket * 3) as i32
        for feat in 0..HALFKA_HM_DIMENSIONS {
            for bucket in 0..NUM_LAYER_STACK_BUCKETS {
                psqt_weights[feat * NUM_LAYER_STACK_BUCKETS + bucket] =
                    (feat as i32 * 7 + bucket as i32 * 3) % 1000 - 500;
            }
        }

        FeatureTransformerLayerStacks {
            biases: Aligned([0; NNUE_PYTORCH_L1]),
            weights: AlignedBox::new_zeroed(HALFKA_HM_DIMENSIONS * NNUE_PYTORCH_L1),
            psqt_biases: [10, 20, 30, 40, 50, 60, 70, 80, 90],
            psqt_weights,
            has_psqt: true,
            threat_weights: AlignedBox::new_zeroed(0),
            has_threat: false,
        }
    }

    /// refresh_psqt と add/sub_psqt_weights による差分更新が一致することを確認
    #[test]
    fn test_psqt_refresh_matches_incremental() {
        let ft = make_test_transformer_with_psqt();

        // 初期特徴量: [100, 200, 300]
        let mut active_initial = IndexList::new();
        let _ = active_initial.push(100);
        let _ = active_initial.push(200);
        let _ = active_initial.push(300);

        // フル計算
        let mut full_acc = [0i32; NUM_LAYER_STACK_BUCKETS];
        ft.refresh_psqt(&active_initial, &mut full_acc);

        // 差分: 200 を削除、400 を追加 → [100, 300, 400]
        let mut incr_acc = full_acc;
        ft.sub_psqt_weights(&mut incr_acc, 200);
        ft.add_psqt_weights(&mut incr_acc, 400);

        // フル計算（[100, 300, 400]）
        let mut active_updated = IndexList::new();
        let _ = active_updated.push(100);
        let _ = active_updated.push(300);
        let _ = active_updated.push(400);
        let mut full_updated = [0i32; NUM_LAYER_STACK_BUCKETS];
        ft.refresh_psqt(&active_updated, &mut full_updated);

        assert_eq!(incr_acc, full_updated, "差分更新とフル計算の結果が不一致");
    }

    /// PSQT 有効モデルで既知の入力に対して期待値を確認
    #[test]
    fn test_psqt_known_values() {
        let ft = make_test_transformer_with_psqt();

        let mut active = IndexList::new();
        let _ = active.push(0);
        let _ = active.push(1);

        let mut acc = [0i32; NUM_LAYER_STACK_BUCKETS];
        ft.refresh_psqt(&active, &mut acc);

        // feat=0: (0*7 + b*3) % 1000 - 500 = b*3 - 500
        // feat=1: (1*7 + b*3) % 1000 - 500 = 7 + b*3 - 500
        // bias + feat0 + feat1
        for (bucket, val) in acc.iter().enumerate() {
            let b = bucket as i32;
            let bias = (b + 1) * 10; // [10, 20, ..., 90]
            let w0 = b * 3 - 500;
            let w1 = 7 + b * 3 - 500;
            let expected = bias + w0 + w1;
            assert_eq!(*val, expected, "bucket {bucket}: expected {expected}, got {val}");
        }
    }
}
