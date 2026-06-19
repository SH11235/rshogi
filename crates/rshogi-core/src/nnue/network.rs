//! NNUEネットワーク全体の構造と評価関数
//!
//! 以下のアーキテクチャをサポート:
//! - **HalfKP**: classic NNUE（水匠/tanuki互換）
//! - **HalfKaSplit**: nnue-pytorch互換（Non-mirror）
//! - **HalfKaHmMerged^**: nnue-pytorch互換（Half-Mirror + Factorization）
//!
//! # 階層構造（4バリアント）
//!
//! ```text
//! NNUENetwork
//! ├── HalfKaSplit(HalfKaSplitNetwork)   // L256/L512/L1024 を内包
//! ├── HalfKaHmMerged(HalfKaHmMergedNetwork)   // L256/L512/L1024 を内包
//! ├── HalfKP(HalfKPNetwork)   // L256/L512 を内包
//! └── LayerStacks(Box<NetworkLayerStacks>)
//! ```
//!
//! **「Accumulator は L1 だけで決まる」** を活用し、L2/L3/活性化の追加時に
//! このファイルの変更は最小限で済む。

use super::accumulator_layer_stacks::LayerStacksAccCache;
#[cfg(feature = "layerstack-arch")]
use super::accumulator_layer_stacks::LayerStacksAccStack;
use super::accumulator_stack_variant::AccumulatorStackVariant;
use super::activation::detect_activation_from_arch;
use super::bona_piece::BonaPiece;
use super::bona_piece_halfka_hm_merged::FE_OLD_END;
use super::constants::{
    MAX_ARCH_LEN, MAX_LAYER_STACK_BUCKETS, NNUE_VERSION, NNUE_VERSION_HALFKA,
    NNUE_VERSION_LAYERSTACK_NUM_BUCKETS,
};
use super::halfka_hm_merged::{HalfKaHmMergedNetwork, HalfKaHmMergedStack};
use super::halfka_hm_split::{HalfKaHmSplitNetwork, HalfKaHmSplitStack};
use super::halfka_merged::{HalfKaMergedNetwork, HalfKaMergedStack};
use super::halfka_split::{HalfKaSplitNetwork, HalfKaSplitStack};
use super::halfkp::{HalfKPNetwork, HalfKPStack};
#[cfg(feature = "layerstack-arch")]
use super::network_layer_stacks::LayerStacksNetwork;
use super::spec::{Activation, FeatureSet};
#[cfg(feature = "halfkx-arch")]
use super::stats::{count_already_computed, count_refresh, count_update};
use crate::eval::material;
use crate::position::Position;
use crate::types::{Color, PieceType, Value};
use std::cell::Cell;
use std::fs::File;
use std::io::{self, BufReader, Cursor, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, Ordering};
use std::sync::{Arc, LazyLock, OnceLock, RwLock};

/// グローバルなNNUEネットワーク（HalfKP/HalfKaSplit/HalfKaHmMerged^）
static NETWORK: LazyLock<RwLock<Option<Arc<NNUENetwork>>>> = LazyLock::new(|| RwLock::new(None));

/// `is_nnue_initialized()` の高速パス用 AtomicBool キャッシュ
///
/// NNUE ロード時に true、クリア時に false に設定する。
/// `should_update_board_effects()` 等のホットパスから RwLock::read を回避するため。
static NNUE_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// FV_SCALE のグローバルオーバーライド設定
///
/// 0 = 自動判定（Network 構造体の fv_scale を使用）
/// 1以上 = 指定値でオーバーライド
///
/// YaneuraOuと同様にエンジンオプションで設定可能。
/// 評価関数によって異なる値が必要な場合に使用。
static FV_SCALE_OVERRIDE: AtomicI32 = AtomicI32::new(0);

/// NNUE アーキテクチャの明示指定
///
/// `auto` (デフォルト) では既存の自動検出を使用。
/// 外部モデルで arch_str が不正確な場合に明示指定で上書きする。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NNUEArchitectureOverride {
    /// 自動検出（デフォルト）
    Auto = 0,
    /// HalfKP
    HalfKP = 1,
    /// HalfKaHmMerged
    HalfKaHmMerged = 2,
    /// HalfKaSplit
    HalfKaSplit = 3,
    /// LayerStacks (PSQT なし)
    LayerStacks = 4,
    /// LayerStacks + PSQT
    LayerStacksPSQT = 5,
}

static NNUE_ARCHITECTURE_OVERRIDE: AtomicI32 =
    AtomicI32::new(NNUEArchitectureOverride::Auto as i32);

/// NNUE アーキテクチャの明示指定を取得
pub(crate) fn get_nnue_architecture_override() -> NNUEArchitectureOverride {
    match NNUE_ARCHITECTURE_OVERRIDE.load(std::sync::atomic::Ordering::Relaxed) {
        1 => NNUEArchitectureOverride::HalfKP,
        2 => NNUEArchitectureOverride::HalfKaHmMerged,
        3 => NNUEArchitectureOverride::HalfKaSplit,
        4 => NNUEArchitectureOverride::LayerStacks,
        5 => NNUEArchitectureOverride::LayerStacksPSQT,
        _ => NNUEArchitectureOverride::Auto,
    }
}

/// NNUE アーキテクチャの明示指定を設定
pub fn set_nnue_architecture_override(mode: NNUEArchitectureOverride) {
    NNUE_ARCHITECTURE_OVERRIDE.store(mode as i32, std::sync::atomic::Ordering::Relaxed);
}

/// USI オプション文字列から NNUEArchitectureOverride をパース
pub fn parse_nnue_architecture(value: &str) -> Option<NNUEArchitectureOverride> {
    match value.to_ascii_lowercase().as_str() {
        "auto" => Some(NNUEArchitectureOverride::Auto),
        "halfkp" => Some(NNUEArchitectureOverride::HalfKP),
        "halfka_hm" | "halfka_hm_merged" => Some(NNUEArchitectureOverride::HalfKaHmMerged),
        "halfka" | "halfka_split" => Some(NNUEArchitectureOverride::HalfKaSplit),
        "layerstacks" => Some(NNUEArchitectureOverride::LayerStacks),
        "layerstacks-psqt" | "layerstacks_psqt" => Some(NNUEArchitectureOverride::LayerStacksPSQT),
        _ => None,
    }
}

/// LayerStacks の bucket 選択モード
///
/// 現在は `Progress8KPAbs`（YaneuraOu 互換 progress.bin）のみをサポートする。
/// enum として残しているのは将来の bucket mode 追加に備えた前方互換性のため。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerStackBucketMode {
    /// 進行度方式(KP-absolute): YaneuraOu 互換 progress.bin で 8 バケットへ分割（bucket8は未使用）
    Progress8KPAbs = 4,
}

impl LayerStackBucketMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Progress8KPAbs => "progress8kpabs",
        }
    }
}

/// progress8kpabs で使用する重み数（81 king squares x FE_OLD_END BonaPiece）
pub const SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS: usize = 81 * FE_OLD_END;

/// `sigmoid(x) * N = k` となる x の閾値 (k = 1..N-1) を N ごとに保持するテーブル。
///
/// `OnceLock` で N 値ごとに lazy 初期化し、`ln(k / (N-k))` を pre-compute する。
/// ホットパス (`progress_sum_to_bucket`) では slice を取って `partition_point`
/// で `floor(sigmoid(sum) * N)` 相当を求める。tatara `progress_kpabs::bucket`
/// (`floor(p * N).clamp(0, N-1)`) と等価 (ADR `2026-05-26` §2.4.1 で証明)。
///
/// 配列添字 N (`1..=MAX_LAYER_STACK_BUCKETS`) ごとに `OnceLock<Box<[f32]>>`。
/// 添字 0 は使われない placeholder。
static PROGRESS_BUCKET_THRESHOLDS: [OnceLock<Box<[f32]>>; MAX_LAYER_STACK_BUCKETS + 1] =
    [const { OnceLock::new() }; MAX_LAYER_STACK_BUCKETS + 1];

/// N bucket 用の閾値 slice を取得 (必要なら初期化)。
fn layer_stack_progress_thresholds(num_buckets: usize) -> &'static [f32] {
    debug_assert!((1..=MAX_LAYER_STACK_BUCKETS).contains(&num_buckets));
    PROGRESS_BUCKET_THRESHOLDS[num_buckets]
        .get_or_init(|| {
            // N - 1 個の閾値 (N = 1 なら空配列)
            let mut v = Vec::with_capacity(num_buckets.saturating_sub(1));
            let n_f = num_buckets as f32;
            for k in 1..num_buckets {
                // t_k = ln(k / (N - k)), sigmoid(t_k) = k / N
                v.push(((k as f32) / (n_f - k as f32)).ln());
            }
            v.into_boxed_slice()
        })
        .as_ref()
}

// progress8kpabs の差分計算済み bucket index キャッシュ（スレッドローカル）
//
// `update_and_evaluate_layer_stacks` で差分計算した結果を格納し、
// `compute_layer_stack_progress8kpabs_bucket_index` 内で消費する。
// 一度消費されると None にリセットされる（1回限り）。
thread_local! {
    static CACHED_PROGRESS_BUCKET: Cell<Option<usize>> = const { Cell::new(None) };
}

/// LayerStacks bucket mode のグローバル設定
static LAYER_STACK_BUCKET_MODE: AtomicI32 =
    AtomicI32::new(LayerStackBucketMode::Progress8KPAbs as i32);

/// progress8kpabs 重みのデフォルト（未設定時は全ゼロ）
static LAYER_STACK_PROGRESS_KP_ABS_ZERO_WEIGHTS: [f32; SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS] =
    [0.0; SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS];

/// progress8kpabs 重みのグローバル設定
///
/// `progress.bin` 読み込み時に Box を leak してポインタだけ差し替える。
/// 設定は起動時の一度を想定し、評価ホットパスでは lock を取らない。
static LAYER_STACK_PROGRESS_KP_ABS_PTR: AtomicPtr<f32> = AtomicPtr::new(std::ptr::null_mut());

/// FV_SCALE オーバーライドを取得
///
/// 戻り値:
/// - `Some(value)`: オーバーライド値が設定されている
/// - `None`: 自動判定を使用（Network の fv_scale を使用）
pub fn get_fv_scale_override() -> Option<i32> {
    let value = FV_SCALE_OVERRIDE.load(Ordering::Relaxed);
    if value > 0 { Some(value) } else { None }
}

/// FV_SCALE オーバーライドを設定
///
/// 引数:
/// - `value`: 設定値（0 = 自動判定、1以上 = オーバーライド）
pub fn set_fv_scale_override(value: i32) {
    FV_SCALE_OVERRIDE.store(value.max(0), Ordering::Relaxed);
}

/// LayerStacks bucket mode を取得
pub fn get_layer_stack_bucket_mode() -> LayerStackBucketMode {
    // 現状は Progress8KPAbs のみ。int mapping は将来のモード追加用。
    let _ = LAYER_STACK_BUCKET_MODE.load(Ordering::Relaxed);
    LayerStackBucketMode::Progress8KPAbs
}

/// LayerStacks bucket mode を設定
pub fn set_layer_stack_bucket_mode(mode: LayerStackBucketMode) {
    LAYER_STACK_BUCKET_MODE.store(mode as i32, Ordering::Relaxed);
}

/// LayerStacks progress8kpabs 重みを取得
pub fn get_layer_stack_progress_kpabs_weights() -> &'static [f32] {
    let ptr = LAYER_STACK_PROGRESS_KP_ABS_PTR.load(Ordering::Relaxed);
    if ptr.is_null() {
        &LAYER_STACK_PROGRESS_KP_ABS_ZERO_WEIGHTS
    } else {
        // SAFETY: `set_layer_stack_progress_kpabs_weights()` で leaked Box の先頭ポインタを保存している。
        unsafe { std::slice::from_raw_parts(ptr.cast_const(), SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS) }
    }
}

/// LayerStacks progress8kpabs 重みを設定
pub fn set_layer_stack_progress_kpabs_weights(weights: Box<[f32]>) -> Result<(), String> {
    if weights.len() != SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS {
        return Err(format!(
            "progress8kpabs weights length mismatch: got {}, expected {}",
            weights.len(),
            SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS
        ));
    }

    let leaked = Box::leak(weights);
    let old_ptr = LAYER_STACK_PROGRESS_KP_ABS_PTR.swap(leaked.as_mut_ptr(), Ordering::Relaxed);
    // SAFETY: old_ptr は過去の同関数で Box::leak したスライスの先頭ポインタ（または null）。
    // USI プロトコルにより設定変更中は評価パスが実行されないため、参照者は存在しない。
    if !old_ptr.is_null() {
        unsafe {
            drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                old_ptr,
                SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS,
            )));
        }
    }
    Ok(())
}

/// LayerStacks progress8kpabs 重みを既定値（全ゼロ）へ戻す
pub fn reset_layer_stack_progress_kpabs_weights() {
    let old_ptr = LAYER_STACK_PROGRESS_KP_ABS_PTR.swap(std::ptr::null_mut(), Ordering::Relaxed);
    // SAFETY: 同上。old_ptr は Box::leak 由来のポインタ（または null）。
    if !old_ptr.is_null() {
        unsafe {
            drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                old_ptr,
                SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS,
            )));
        }
    }
}

// =============================================================================
// NNUENetwork - アーキテクチャを抽象化するenum
// =============================================================================

/// NNUEネットワーク（4バリアント階層構造）
///
/// **「Accumulator は L1 だけで決まる」** を活用した設計:
/// - HalfKaSplit(HalfKaSplitNetwork): L256/L512/L1024 を内包
/// - HalfKaHmMerged(HalfKaHmMergedNetwork): L256/L512/L1024 を内包
/// - HalfKP(HalfKPNetwork): L256/L512 を内包
/// - LayerStacks: 1536次元 + 9バケット
///
/// L2/L3/活性化の追加時、このenumの変更は不要。
/// 詳細は `halfka_split/` や `halfkp/` のモジュールで管理される。
pub enum NNUENetwork {
    /// HalfKaSplit 特徴量セット（L256/L512/L1024）
    HalfKaSplit(HalfKaSplitNetwork),
    /// HalfKaHmMerged 特徴量セット（L256/L512/L1024）
    HalfKaHmMerged(HalfKaHmMergedNetwork),
    /// HalfKaMerged 特徴量セット（L256/L512/L1024）
    HalfKaMerged(HalfKaMergedNetwork),
    /// HalfKaHmSplit 特徴量セット（L256/L512/L1024）
    HalfKaHmSplit(HalfKaHmSplitNetwork),
    /// HalfKP 特徴量セット（L256/L512）
    HalfKP(HalfKPNetwork),
    /// LayerStacks（L1=1536/768 + 9バケット）
    #[cfg(feature = "layerstack-arch")]
    LayerStacks(LayerStacksNetwork),
}

impl NNUENetwork {
    /// HalfKP でサポートされているアーキテクチャ一覧
    pub fn supported_halfkp_specs() -> Vec<super::spec::ArchitectureSpec> {
        HalfKPNetwork::supported_specs()
    }

    /// HalfKaHmMerged でサポートされているアーキテクチャ一覧
    pub fn supported_halfka_hm_specs() -> Vec<super::spec::ArchitectureSpec> {
        HalfKaHmMergedNetwork::supported_specs()
    }

    /// HalfKaSplit でサポートされているアーキテクチャ一覧
    pub fn supported_halfka_specs() -> Vec<super::spec::ArchitectureSpec> {
        HalfKaSplitNetwork::supported_specs()
    }

    /// ファイルから読み込み（バージョン自動判別）
    pub fn load<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);
        Self::read(&mut reader)
    }

    /// リーダーから読み込み（ファイルサイズ優先の自動判別）
    ///
    /// ファイルサイズからアーキテクチャを一意に検出し、適切なバリアントに委譲する。
    /// ヘッダーの description 文字列は活性化関数の検出にのみ使用する。
    pub fn read<R: Read + Seek>(reader: &mut R) -> io::Result<Self> {
        // 1. ファイルサイズを取得
        let file_size = reader.seek(SeekFrom::End(0))?;
        reader.seek(SeekFrom::Start(0))?;

        // 2. VERSION を読む
        let mut buf4 = [0u8; 4];
        reader.read_exact(&mut buf4)?;
        let version = u32::from_le_bytes(buf4);

        match version {
            NNUE_VERSION | NNUE_VERSION_HALFKA | NNUE_VERSION_LAYERSTACK_NUM_BUCKETS => {
                // 3. hash と arch_len を読む
                reader.read_exact(&mut buf4)?; // ネットワークハッシュ
                reader.read_exact(&mut buf4)?; // arch_len
                let arch_len = u32::from_le_bytes(buf4) as usize;
                if arch_len == 0 || arch_len > MAX_ARCH_LEN {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("Invalid arch string length: {arch_len}"),
                    ));
                }

                // アーキテクチャ文字列を読む（活性化関数・FeatureSet 検出用）
                let mut arch = vec![0u8; arch_len];
                reader.read_exact(&mut arch)?;
                let arch_str = String::from_utf8_lossy(&arch);

                // 活性化関数を検出
                let activation_str = detect_activation_from_arch(&arch_str);
                let activation = match activation_str {
                    "SCReLU" => Activation::SCReLU,
                    "PairwiseCReLU" => Activation::PairwiseCReLU,
                    _ => Activation::CReLU,
                };

                // FeatureSet を決定: USI オプションが明示指定されていればそちらを優先。
                // `NNUE_VERSION_LAYERSTACK_NUM_BUCKETS` は LayerStack 専用の
                // self-describing layout。arch_str 直後に `num_buckets: u32` field を
                // 持つため、非-LayerStack reader に渡すと file_size 検出が失敗 (ft_hash
                // 等が 4 byte ずれる) するか、悪ければ別 arch として誤読する。誤読を
                // 確実に防ぐため、当該 version + 非-LayerStack override の組合せをここで明示
                // reject する。
                let arch_override = get_nnue_architecture_override();
                if version == NNUE_VERSION_LAYERSTACK_NUM_BUCKETS
                    && !matches!(
                        arch_override,
                        NNUEArchitectureOverride::Auto
                            | NNUEArchitectureOverride::LayerStacks
                            | NNUEArchitectureOverride::LayerStacksPSQT,
                    )
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "NNUE (version {NNUE_VERSION_LAYERSTACK_NUM_BUCKETS:#x}, LayerStack \
                             with num_buckets header) は LayerStack 専用 layout。\
                             NNUE_ARCHITECTURE override の値 (LayerStack 以外) では読めない。\
                             override を `Auto` / `LayerStacks` / `LayerStacksPSQT` のいずれかに \
                             変更するか、対応する legacy net を使用すること。"
                        ),
                    ));
                }
                let effective_feature_set = if version == NNUE_VERSION_LAYERSTACK_NUM_BUCKETS
                    && matches!(arch_override, NNUEArchitectureOverride::Auto)
                {
                    FeatureSet::LayerStacks
                } else {
                    match arch_override {
                        NNUEArchitectureOverride::Auto => {
                            // 自動検出: ヘッダーから FeatureSet を取得
                            let parsed = super::spec::parse_architecture(&arch_str)
                                .map_err(|msg| io::Error::new(io::ErrorKind::InvalidData, msg))?;
                            parsed.feature_set
                        }
                        NNUEArchitectureOverride::HalfKP => FeatureSet::HalfKP,
                        NNUEArchitectureOverride::HalfKaHmMerged => FeatureSet::HalfKaHmMerged,
                        NNUEArchitectureOverride::HalfKaSplit => FeatureSet::HalfKaSplit,
                        NNUEArchitectureOverride::LayerStacks
                        | NNUEArchitectureOverride::LayerStacksPSQT => FeatureSet::LayerStacks,
                    }
                };

                // LayerStacks は特殊処理（FT が LEB128 圧縮のためファイルサイズ検出の対象外）
                if effective_feature_set == FeatureSet::LayerStacks {
                    #[cfg(feature = "layerstack-arch")]
                    {
                        // PSQT オーバーライド:
                        // LayerStacks → Some(false) (PSQT 強制 OFF)
                        // LayerStacksPSQT → Some(true) (PSQT 強制 ON)
                        // Auto → None (arch_str から自動検出)
                        let psqt_override = match arch_override {
                            NNUEArchitectureOverride::LayerStacks => Some(false),
                            NNUEArchitectureOverride::LayerStacksPSQT => Some(true),
                            _ => None,
                        };
                        reader.seek(SeekFrom::Start(0))?;
                        let (l1_from_arch, l2_from_arch, l3_from_arch) =
                            super::spec::parse_arch_dimensions(&arch_str);
                        let l1 = if l1_from_arch == 0 {
                            1536
                        } else {
                            l1_from_arch
                        };
                        let (l2, l3) = match (l2_from_arch, l3_from_arch) {
                            (0, 0) => (16, 32),
                            dims => dims,
                        };
                        let network = LayerStacksNetwork::read_with_options(
                            reader,
                            l1,
                            l2,
                            l3,
                            psqt_override,
                        )?;
                        return Ok(Self::LayerStacks(network));
                    }
                    #[cfg(not(feature = "layerstack-arch"))]
                    {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "LayerStacks NNUE model requires the `layerstack-arch` feature; \
                             rebuild rshogi-core with an Edition that enables it.",
                        ));
                    }
                }

                // 4. ファイルサイズからアーキテクチャを検出
                let detection = super::spec::detect_architecture_from_size(
                    file_size,
                    arch_len,
                    Some(effective_feature_set),
                )
                .ok_or_else(|| {
                    // 検出失敗時は候補を表示
                    let candidates = super::spec::list_candidate_architectures(file_size, arch_len);
                    let candidates_str: Vec<String> = candidates
                        .iter()
                        .take(5)
                        .map(|(spec, diff)| format!("{} (diff: {:+})", spec.name(), diff))
                        .collect();

                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "Unknown architecture: file_size={}, arch_len={}, feature_set={}. \
                             Closest candidates: [{}]",
                            file_size,
                            arch_len,
                            effective_feature_set,
                            candidates_str.join(", ")
                        ),
                    )
                })?;

                // 位置を戻して読み込み
                reader.seek(SeekFrom::Start(0))?;

                // 5. 検出したアーキテクチャで読み込み
                let l1 = detection.spec.l1;
                let l2 = detection.spec.l2;
                let l3 = detection.spec.l3;

                match detection.spec.feature_set {
                    FeatureSet::HalfKaHmMerged => {
                        let network = HalfKaHmMergedNetwork::read(reader, l1, l2, l3, activation)?;
                        Ok(Self::HalfKaHmMerged(network))
                    }
                    FeatureSet::HalfKaSplit => {
                        let network = HalfKaSplitNetwork::read(reader, l1, l2, l3, activation)?;
                        Ok(Self::HalfKaSplit(network))
                    }
                    FeatureSet::HalfKaMerged => {
                        let network = HalfKaMergedNetwork::read(reader, l1, l2, l3, activation)?;
                        Ok(Self::HalfKaMerged(network))
                    }
                    FeatureSet::HalfKaHmSplit => {
                        let network = HalfKaHmSplitNetwork::read(reader, l1, l2, l3, activation)?;
                        Ok(Self::HalfKaHmSplit(network))
                    }
                    FeatureSet::HalfKP => {
                        let network = HalfKPNetwork::read(reader, l1, l2, l3, activation)?;
                        Ok(Self::HalfKP(network))
                    }
                    FeatureSet::LayerStacks => {
                        // 上で処理済みなのでここには来ない
                        unreachable!()
                    }
                }
            }
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Unknown NNUE version: {version:#x}. Expected {NNUE_VERSION:#x} (HalfKP), \
                     {NNUE_VERSION_HALFKA:#x} (HalfKaHmMerged^ / legacy LayerStack), or \
                     {NNUE_VERSION_LAYERSTACK_NUM_BUCKETS:#x} (LayerStack with num_buckets header)"
                ),
            )),
        }
    }

    /// バイト列から読み込み（バージョン自動判別）
    pub fn from_bytes(bytes: &[u8]) -> io::Result<Self> {
        let mut cursor = Cursor::new(bytes);
        Self::read(&mut cursor)
    }

    /// LayerStacks アーキテクチャかどうか
    pub fn is_layer_stacks(&self) -> bool {
        #[cfg(feature = "layerstack-arch")]
        {
            matches!(self, Self::LayerStacks(_))
        }
        #[cfg(not(feature = "layerstack-arch"))]
        {
            false
        }
    }

    /// HalfKaSplit アーキテクチャかどうか
    pub fn is_halfka(&self) -> bool {
        matches!(self, Self::HalfKaSplit(_))
    }

    /// HalfKaHmMerged アーキテクチャかどうか
    pub fn is_halfka_hm(&self) -> bool {
        matches!(self, Self::HalfKaHmMerged(_))
    }

    /// HalfKP アーキテクチャかどうか
    pub fn is_halfkp(&self) -> bool {
        matches!(self, Self::HalfKP(_))
    }

    /// L1 サイズを取得
    pub fn l1_size(&self) -> usize {
        match self {
            Self::HalfKaSplit(net) => net.l1_size(),
            Self::HalfKaHmMerged(net) => net.l1_size(),
            Self::HalfKaMerged(net) => net.l1_size(),
            Self::HalfKaHmSplit(net) => net.l1_size(),
            Self::HalfKP(net) => net.l1_size(),
            #[cfg(feature = "layerstack-arch")]
            Self::LayerStacks(net) => net.l1_size(),
        }
    }

    /// アーキテクチャ名を取得
    pub fn architecture_name(&self) -> String {
        match self {
            Self::HalfKaSplit(net) => net.architecture_name(),
            Self::HalfKaHmMerged(net) => net.architecture_name(),
            Self::HalfKaMerged(net) => net.architecture_name(),
            Self::HalfKaHmSplit(net) => net.architecture_name(),
            Self::HalfKP(net) => net.architecture_name(),
            #[cfg(feature = "layerstack-arch")]
            Self::LayerStacks(_) => "LayerStacks".to_string(),
        }
    }

    /// アーキテクチャ仕様を取得
    pub fn architecture_spec(&self) -> super::spec::ArchitectureSpec {
        match self {
            Self::HalfKaSplit(net) => net.architecture_spec(),
            Self::HalfKaHmMerged(net) => net.architecture_spec(),
            Self::HalfKaMerged(net) => net.architecture_spec(),
            Self::HalfKaHmSplit(net) => net.architecture_spec(),
            Self::HalfKP(net) => net.architecture_spec(),
            #[cfg(feature = "layerstack-arch")]
            Self::LayerStacks(net) => net.architecture_spec(),
        }
    }

    /// LayerStacksNetwork への参照を取得
    ///
    /// LayerStacks アーキテクチャでない場合は panic。
    #[cfg(feature = "layerstack-arch")]
    pub fn as_layer_stacks(&self) -> &LayerStacksNetwork {
        match self {
            Self::LayerStacks(net) => net,
            _ => panic!("This method is only for LayerStacks architecture."),
        }
    }

    /// HalfKaHmMerged アキュムレータをフル再計算
    pub fn refresh_accumulator_halfka_hm(&self, pos: &Position, stack: &mut HalfKaHmMergedStack) {
        match self {
            Self::HalfKaHmMerged(net) => net.refresh_accumulator(pos, stack),
            _ => panic!("This method is only for HalfKaHmMerged architecture."),
        }
    }

    /// HalfKaSplit アキュムレータをフル再計算
    pub fn refresh_accumulator_halfka(&self, pos: &Position, stack: &mut HalfKaSplitStack) {
        match self {
            Self::HalfKaSplit(net) => net.refresh_accumulator(pos, stack),
            _ => panic!("This method is only for HalfKaSplit architecture."),
        }
    }

    /// HalfKaHmMerged 差分更新
    pub fn update_accumulator_halfka_hm(
        &self,
        pos: &Position,
        dirty: &super::accumulator::DirtyPiece,
        stack: &mut HalfKaHmMergedStack,
        source_idx: usize,
    ) {
        match self {
            Self::HalfKaHmMerged(net) => net.update_accumulator(pos, dirty, stack, source_idx),
            _ => panic!("This method is only for HalfKaHmMerged architecture."),
        }
    }

    /// HalfKaSplit 差分更新
    pub fn update_accumulator_halfka(
        &self,
        pos: &Position,
        dirty: &super::accumulator::DirtyPiece,
        stack: &mut HalfKaSplitStack,
        source_idx: usize,
    ) {
        match self {
            Self::HalfKaSplit(net) => net.update_accumulator(pos, dirty, stack, source_idx),
            _ => panic!("This method is only for HalfKaSplit architecture."),
        }
    }

    /// HalfKaHmMerged 前方差分更新
    pub fn forward_update_incremental_halfka_hm(
        &self,
        pos: &Position,
        stack: &mut HalfKaHmMergedStack,
        source_idx: usize,
    ) -> bool {
        match self {
            Self::HalfKaHmMerged(net) => net.forward_update_incremental(pos, stack, source_idx),
            _ => panic!("This method is only for HalfKaHmMerged architecture."),
        }
    }

    /// HalfKaSplit 前方差分更新
    pub fn forward_update_incremental_halfka(
        &self,
        pos: &Position,
        stack: &mut HalfKaSplitStack,
        source_idx: usize,
    ) -> bool {
        match self {
            Self::HalfKaSplit(net) => net.forward_update_incremental(pos, stack, source_idx),
            _ => panic!("This method is only for HalfKaSplit architecture."),
        }
    }

    /// HalfKaHmMerged 評価
    pub fn evaluate_halfka_hm(&self, pos: &Position, stack: &HalfKaHmMergedStack) -> Value {
        match self {
            Self::HalfKaHmMerged(net) => net.evaluate(pos, stack),
            _ => panic!("This method is only for HalfKaHmMerged architecture."),
        }
    }

    /// HalfKaSplit 評価
    pub fn evaluate_halfka(&self, pos: &Position, stack: &HalfKaSplitStack) -> Value {
        match self {
            Self::HalfKaSplit(net) => net.evaluate(pos, stack),
            _ => panic!("This method is only for HalfKaSplit architecture."),
        }
    }

    /// HalfKaMerged アキュムレータをフル再計算
    pub fn refresh_accumulator_halfka_merged(&self, pos: &Position, stack: &mut HalfKaMergedStack) {
        match self {
            Self::HalfKaMerged(net) => net.refresh_accumulator(pos, stack),
            _ => panic!("This method is only for HalfKaMerged architecture."),
        }
    }

    /// HalfKaMerged 差分更新
    pub fn update_accumulator_halfka_merged(
        &self,
        pos: &Position,
        dirty: &super::accumulator::DirtyPiece,
        stack: &mut HalfKaMergedStack,
        source_idx: usize,
    ) {
        match self {
            Self::HalfKaMerged(net) => net.update_accumulator(pos, dirty, stack, source_idx),
            _ => panic!("This method is only for HalfKaMerged architecture."),
        }
    }

    /// HalfKaMerged 前方差分更新
    pub fn forward_update_incremental_halfka_merged(
        &self,
        pos: &Position,
        stack: &mut HalfKaMergedStack,
        source_idx: usize,
    ) -> bool {
        match self {
            Self::HalfKaMerged(net) => net.forward_update_incremental(pos, stack, source_idx),
            _ => panic!("This method is only for HalfKaMerged architecture."),
        }
    }

    /// HalfKaMerged 評価
    pub fn evaluate_halfka_merged(&self, pos: &Position, stack: &HalfKaMergedStack) -> Value {
        match self {
            Self::HalfKaMerged(net) => net.evaluate(pos, stack),
            _ => panic!("This method is only for HalfKaMerged architecture."),
        }
    }

    /// HalfKaHmSplit アキュムレータをフル再計算
    pub fn refresh_accumulator_halfka_hm_split(
        &self,
        pos: &Position,
        stack: &mut HalfKaHmSplitStack,
    ) {
        match self {
            Self::HalfKaHmSplit(net) => net.refresh_accumulator(pos, stack),
            _ => panic!("This method is only for HalfKaHmSplit architecture."),
        }
    }

    /// HalfKaHmSplit 差分更新
    pub fn update_accumulator_halfka_hm_split(
        &self,
        pos: &Position,
        dirty: &super::accumulator::DirtyPiece,
        stack: &mut HalfKaHmSplitStack,
        source_idx: usize,
    ) {
        match self {
            Self::HalfKaHmSplit(net) => net.update_accumulator(pos, dirty, stack, source_idx),
            _ => panic!("This method is only for HalfKaHmSplit architecture."),
        }
    }

    /// HalfKaHmSplit 前方差分更新
    pub fn forward_update_incremental_halfka_hm_split(
        &self,
        pos: &Position,
        stack: &mut HalfKaHmSplitStack,
        source_idx: usize,
    ) -> bool {
        match self {
            Self::HalfKaHmSplit(net) => net.forward_update_incremental(pos, stack, source_idx),
            _ => panic!("This method is only for HalfKaHmSplit architecture."),
        }
    }

    /// HalfKaHmSplit 評価
    pub fn evaluate_halfka_hm_split(&self, pos: &Position, stack: &HalfKaHmSplitStack) -> Value {
        match self {
            Self::HalfKaHmSplit(net) => net.evaluate(pos, stack),
            _ => panic!("This method is only for HalfKaHmSplit architecture."),
        }
    }

    /// HalfKP アキュムレータをフル再計算
    pub fn refresh_accumulator_halfkp(&self, pos: &Position, stack: &mut HalfKPStack) {
        match self {
            Self::HalfKP(net) => net.refresh_accumulator(pos, stack),
            _ => panic!("This method is only for HalfKP architecture."),
        }
    }

    /// HalfKP 差分更新
    pub fn update_accumulator_halfkp(
        &self,
        pos: &Position,
        dirty: &super::accumulator::DirtyPiece,
        stack: &mut HalfKPStack,
        source_idx: usize,
    ) {
        match self {
            Self::HalfKP(net) => net.update_accumulator(pos, dirty, stack, source_idx),
            _ => panic!("This method is only for HalfKP architecture."),
        }
    }

    /// HalfKP 前方差分更新
    pub fn forward_update_incremental_halfkp(
        &self,
        pos: &Position,
        stack: &mut HalfKPStack,
        source_idx: usize,
    ) -> bool {
        match self {
            Self::HalfKP(net) => net.forward_update_incremental(pos, stack, source_idx),
            _ => panic!("This method is only for HalfKP architecture."),
        }
    }

    /// HalfKP 評価
    pub fn evaluate_halfkp(&self, pos: &Position, stack: &HalfKPStack) -> Value {
        match self {
            Self::HalfKP(net) => net.evaluate(pos, stack),
            _ => panic!("This method is only for HalfKP architecture."),
        }
    }
}

// =============================================================================
// arch_str メタデータパース
// =============================================================================

/// arch_str から fv_scale を抽出
///
/// bullet-shogi で学習したモデルは arch_str に "fv_scale=N" を含む。
/// 例: "Features=HalfKaHmMerged^[73305->256x2]-SCReLU,fv_scale=13,qa=127,qb=64,scale=600"
///
/// 戻り値:
/// - `Some(N)`: fv_scale=N が見つかり、妥当な範囲（1〜128）内の場合
/// - `None`: fv_scale が見つからない、またはパース失敗、または範囲外
///
/// 範囲外の値（0, 負数, 128超）は None を返し、フォールバック値が使用される。
/// これによりゼロ除算や不正な評価値スケーリングを防止する。
pub fn parse_fv_scale_from_arch(arch_str: &str) -> Option<i32> {
    /// fv_scale の許容最小値（ゼロ除算防止）
    const FV_SCALE_MIN: i32 = 1;
    /// fv_scale の許容最大値（実用的な上限）
    const FV_SCALE_MAX: i32 = 128;

    for part in arch_str.split(',') {
        if let Some(value) = part.strip_prefix("fv_scale=") {
            if let Ok(scale) = value.parse::<i32>() {
                // 妥当な範囲内のみ受け入れる
                if (FV_SCALE_MIN..=FV_SCALE_MAX).contains(&scale) {
                    return Some(scale);
                }
            }
            // fv_scale= が見つかったがパース失敗または範囲外の場合は None
            return None;
        }
    }
    None
}

/// LayerStacks bucket mode をパース
pub fn parse_layer_stack_bucket_mode(value: &str) -> Option<LayerStackBucketMode> {
    match value.trim().to_ascii_lowercase().as_str() {
        "progress8kpabs" => Some(LayerStackBucketMode::Progress8KPAbs),
        _ => None,
    }
}

/// progress8kpabs 重みに基づいて LayerStacks bucket index `[0, num_buckets)` を計算
///
/// `CACHED_PROGRESS_BUCKET` にキャッシュされた値がある場合はそちらを消費する。
/// `num_buckets` は net file の `num_buckets` (active net 由来) を渡す。
/// キャッシュは active net 1 つの不変条件のもとで作成・消費される
/// (ADR `2026-05-26` §2.4.3)。
pub fn compute_layer_stack_progress8kpabs_bucket_index(
    pos: &Position,
    _side_to_move: Color,
    weights: &[f32],
    num_buckets: usize,
) -> usize {
    // 差分計算済みキャッシュがあれば消費して返す
    let cached = CACHED_PROGRESS_BUCKET.with(|c| c.replace(None));
    if let Some(bucket) = cached {
        return bucket;
    }
    // フォールバック: 全駒スキャン
    let sum = compute_progress8kpabs_sum(pos, weights);
    progress_sum_to_bucket(sum, num_buckets)
}

/// progress8kpabs の重み付き和を全駒スキャンで計算（refresh 用）
pub fn compute_progress8kpabs_sum(pos: &Position, weights: &[f32]) -> f32 {
    debug_assert_eq!(
        weights.len(),
        SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS,
        "progress8kpabs weights length mismatch"
    );

    let sq_bk = pos.king_square(Color::Black).index();
    let sq_wk = pos.king_square(Color::White).inverse().index();
    // SAFETY: sq_bk, sq_wk は king_square().index() で 0..81 の範囲。
    // weights の長さは 81 * FE_OLD_END であり、(sq + 1) * FE_OLD_END <= weights.len()。
    let weights_b = unsafe { weights.get_unchecked(sq_bk * FE_OLD_END..(sq_bk + 1) * FE_OLD_END) };
    let weights_w = unsafe { weights.get_unchecked(sq_wk * FE_OLD_END..(sq_wk + 1) * FE_OLD_END) };

    let mut sum = 0.0f32;

    for sq in pos.occupied().iter() {
        let pc = pos.piece_on(sq);
        if pc.is_none() || pc.piece_type() == PieceType::King {
            continue;
        }

        let bp_b = BonaPiece::from_piece_square(pc, sq, Color::Black);
        if bp_b != BonaPiece::ZERO {
            sum += weights_b[bp_b.value() as usize];
        }

        let bp_w = BonaPiece::from_piece_square(pc, sq, Color::White);
        if bp_w != BonaPiece::ZERO {
            sum += weights_w[bp_w.value() as usize];
        }
    }

    for owner in [Color::Black, Color::White] {
        let hand = pos.hand(owner);
        for &pt in &PieceType::HAND_PIECES {
            let count = hand.count(pt);
            for c in 1..=count {
                let c_u8 = u8::try_from(c).expect("hand count fits in u8");

                let bp_b = BonaPiece::from_hand_piece(Color::Black, owner, pt, c_u8);
                if bp_b != BonaPiece::ZERO {
                    sum += weights_b[bp_b.value() as usize];
                }

                let bp_w = BonaPiece::from_hand_piece(Color::White, owner, pt, c_u8);
                if bp_w != BonaPiece::ZERO {
                    sum += weights_w[bp_w.value() as usize];
                }
            }
        }
    }

    sum
}

/// progress_sum から DirtyPiece の変化分を差分更新
///
/// 玉が動いていない場合にのみ使用可能。
/// DirtyPiece の ExtBonaPiece.fb/fw は progress8kpabs と同じ BonaPiece 体系。
#[cfg(feature = "nnue-progress-diff")]
#[inline]
pub fn update_progress8kpabs_sum_diff(
    prev_sum: f32,
    dirty_piece: &super::accumulator::DirtyPiece,
    sq_bk: usize,
    sq_wk: usize,
    weights: &[f32],
) -> f32 {
    // SAFETY: sq_bk, sq_wk は king_square().index() で 0..81 の範囲。
    // weights の長さは 81 * FE_OLD_END であり、(sq + 1) * FE_OLD_END <= weights.len()。
    debug_assert!(sq_bk < 81, "sq_bk out of range: {sq_bk}");
    debug_assert!(sq_wk < 81, "sq_wk out of range: {sq_wk}");
    debug_assert_eq!(
        weights.len(),
        SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS,
        "progress8kpabs weights length mismatch"
    );
    let weights_b = unsafe { weights.get_unchecked(sq_bk * FE_OLD_END..(sq_bk + 1) * FE_OLD_END) };
    let weights_w = unsafe { weights.get_unchecked(sq_wk * FE_OLD_END..(sq_wk + 1) * FE_OLD_END) };
    let mut sum = prev_sum;
    for i in 0..dirty_piece.dirty_num as usize {
        debug_assert!(i < dirty_piece.changed_piece.len());
        // SAFETY: dirty_num は最大 2 であり、changed_piece は [ChangedBonaPiece; 2]。
        let changed = unsafe { dirty_piece.changed_piece.get_unchecked(i) };

        // old の寄与を引く
        let old_fb = changed.old_piece.fb;
        if old_fb != BonaPiece::ZERO {
            let idx = old_fb.value() as usize;
            debug_assert!(idx < weights_b.len());
            // SAFETY: BonaPiece の値は FE_OLD_END 未満であり、weights_b の長さは FE_OLD_END。
            sum -= unsafe { *weights_b.get_unchecked(idx) };
        }
        let old_fw = changed.old_piece.fw;
        if old_fw != BonaPiece::ZERO {
            let idx = old_fw.value() as usize;
            debug_assert!(idx < weights_w.len());
            // SAFETY: BonaPiece の値は FE_OLD_END 未満であり、weights_w の長さは FE_OLD_END。
            sum -= unsafe { *weights_w.get_unchecked(idx) };
        }

        // new の寄与を足す
        let new_fb = changed.new_piece.fb;
        if new_fb != BonaPiece::ZERO {
            let idx = new_fb.value() as usize;
            debug_assert!(idx < weights_b.len());
            // SAFETY: BonaPiece の値は FE_OLD_END 未満であり、weights_b の長さは FE_OLD_END。
            sum += unsafe { *weights_b.get_unchecked(idx) };
        }
        let new_fw = changed.new_piece.fw;
        if new_fw != BonaPiece::ZERO {
            let idx = new_fw.value() as usize;
            debug_assert!(idx < weights_w.len());
            // SAFETY: BonaPiece の値は FE_OLD_END 未満であり、weights_w の長さは FE_OLD_END。
            sum += unsafe { *weights_w.get_unchecked(idx) };
        }
    }
    sum
}

/// progress_sum から bucket index を計算（閾値比較のみ）
///
/// 戻り値は `[0, num_buckets)`。式は tatara の
/// `floor(sigmoid(sum) × num_buckets).clamp(0, num_buckets - 1)` と等価。
/// 閾値テーブルは N ごとに `OnceLock` で lazy 構築される。
///
/// ADR `2026-05-26` §2.4.1 の等価性証明を参照。
#[inline]
pub fn progress_sum_to_bucket(sum: f32, num_buckets: usize) -> usize {
    let thresholds = layer_stack_progress_thresholds(num_buckets);
    thresholds.partition_point(|&t| sum >= t)
}

/// NNUEを初期化（バージョン自動判別）
pub fn init_nnue<P: AsRef<Path>>(path: P) -> io::Result<()> {
    let network = Arc::new(NNUENetwork::load(path)?);
    *NETWORK.write().expect("NNUE lock poisoned") = Some(network);
    NNUE_INITIALIZED.store(true, Ordering::Release);
    Ok(())
}

/// バイト列からNNUEを初期化（バージョン自動判別）
pub fn init_nnue_from_bytes(bytes: &[u8]) -> io::Result<()> {
    let network = Arc::new(NNUENetwork::from_bytes(bytes)?);
    *NETWORK.write().expect("NNUE lock poisoned") = Some(network);
    NNUE_INITIALIZED.store(true, Ordering::Release);
    Ok(())
}

/// グローバル NNUE をクリアする
pub fn clear_nnue() {
    // Safety: false を先に書いてから NETWORK をクリアすること。
    // 逆順にすると is_nnue_initialized() == true の直後に get_network() が None を返す
    // 短い窓が生じる。false-negative（ロード済みなのに false に見える瞬間）は安全。
    NNUE_INITIALIZED.store(false, Ordering::Release);
    *NETWORK.write().expect("NNUE lock poisoned") = None;
}

/// NNUEが初期化済みかどうか
///
/// AtomicBool キャッシュにより RwLock::read を回避する。
/// `init_nnue()` / `clear_nnue()` で更新される。
#[inline]
pub fn is_nnue_initialized() -> bool {
    NNUE_INITIALIZED.load(Ordering::Acquire)
}

// =============================================================================
// フォーマット検出
// =============================================================================

/// NNUE フォーマット情報
#[derive(Debug, Clone)]
pub struct NnueFormatInfo {
    /// アーキテクチャ名（例: "HalfKaSplit1024", "HalfKaHmMerged1024", "LayerStacks", "HalfKP256"）
    pub architecture: String,

    /// L1 次元（例: 256, 512, 1024, 1536）
    pub l1_dimension: u32,

    /// L2 次元（例: 8, 32）
    pub l2_dimension: u32,

    /// L3 次元（例: 32, 96）
    pub l3_dimension: u32,

    /// 活性化関数（"CReLU" or "SCReLU"）
    pub activation: String,

    /// バージョンヘッダ（生の u32 値）
    pub version: u32,

    /// アーキテクチャ文字列（生の文字列）
    pub arch_string: String,
}

/// NNUE ファイルのフォーマット情報を検出（ファイルサイズベースの自動判定）
///
/// nnue-pytorch が生成するファイルはヘッダーに不正確なアーキテクチャ情報を
/// 含むことがあるため、ファイルサイズから正確なアーキテクチャを検出する。
///
/// # 検出ロジック
/// 1. ヘッダーから FeatureSet と活性化関数を取得（ヒントとして使用）
/// 2. ファイルサイズから L1/L2/L3 を一意に検出（優先）
/// 3. 検出失敗時はヘッダーのパース結果にフォールバック（精度低下の可能性あり）
///
/// # Arguments
/// * `bytes` - NNUE ファイルの先頭バイト列（ヘッダー + アーキテクチャ文字列を含む）
/// * `file_size` - ファイル全体のサイズ（バイト単位）
///
/// # Returns
/// * `Ok(NnueFormatInfo)` - 検出されたフォーマット情報
/// * `Err(io::Error)` - ヘッダー解析失敗または不正なフォーマット
///
/// # Errors
/// - `InvalidData`: ファイルサイズ不足、不正なヘッダー、またはアーキテクチャ文字列長
///
/// # Examples
/// ```ignore
/// let bytes = std::fs::read("model.bin")?;
/// let file_size = bytes.len() as u64;
/// let info = detect_format(&bytes, file_size)?;
/// println!("Detected: {} (L1={}, L2={}, L3={})",
///          info.architecture, info.l1_dimension, info.l2_dimension, info.l3_dimension);
/// ```
pub fn detect_format(bytes: &[u8], file_size: u64) -> io::Result<NnueFormatInfo> {
    // 最小ヘッダーサイズ: version(4) + hash(4) + arch_len(4)
    const MIN_HEADER_SIZE: usize = 12;

    if bytes.len() < MIN_HEADER_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "NNUE file too small: {} bytes (need at least {} for header)",
                bytes.len(),
                MIN_HEADER_SIZE
            ),
        ));
    }

    // バージョンを読み取り
    let version = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);

    match version {
        NNUE_VERSION | NNUE_VERSION_HALFKA | NNUE_VERSION_LAYERSTACK_NUM_BUCKETS => {
            // アーキテクチャ文字列長を読み取り
            let arch_len = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize;

            // arch_len の妥当性をチェック（バッファオーバーリード防止）
            if arch_len == 0 || arch_len > MAX_ARCH_LEN {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("Invalid arch string length: {} (max: {})", arch_len, MAX_ARCH_LEN),
                ));
            }

            // 必要なバイト数をチェック
            let required_size = MIN_HEADER_SIZE + arch_len;
            if bytes.len() < required_size {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "NNUE file too small: {} bytes (need {} for arch string)",
                        bytes.len(),
                        required_size
                    ),
                ));
            }

            // アーキテクチャ文字列を読み取り
            let arch_str = String::from_utf8_lossy(&bytes[12..12 + arch_len]).to_string();

            // 活性化関数を検出（ヘッダーから）
            let activation = detect_activation_from_arch(&arch_str).to_string();

            // ヘッダーから FeatureSet を取得（検出のヒントに使用）
            let parsed = super::spec::parse_architecture(&arch_str)
                .map_err(|msg| io::Error::new(io::ErrorKind::InvalidData, msg))?;

            // ファイルサイズからアーキテクチャを検出（L1/L2/L3 の正確な値を取得）
            let (l1, l2, l3, feature_set, used_file_size_detection) = if let Some(detection) =
                super::spec::detect_architecture_from_size(
                    file_size,
                    arch_len,
                    Some(parsed.feature_set),
                ) {
                // ファイルサイズベースの検出成功
                (
                    detection.spec.l1,
                    detection.spec.l2,
                    detection.spec.l3,
                    detection.spec.feature_set,
                    true,
                )
            } else {
                // フォールバック: ヘッダーのパース結果を使用
                // 注意: ヘッダーが不正確な場合、誤った結果になる可能性がある
                (parsed.l1, parsed.l2, parsed.l3, parsed.feature_set, false)
            };

            // フォールバック時は警告情報をログ出力（デバッグビルド時のみ）
            #[cfg(debug_assertions)]
            if !used_file_size_detection {
                eprintln!(
                    "Warning: File size detection failed for size={}. \
                     Falling back to header parsing (may be inaccurate).",
                    file_size
                );
            }
            // used_file_size_detection を使用済みとしてマーク（リリースビルドでの警告抑制）
            let _ = used_file_size_detection;

            // アーキテクチャ名を決定
            let architecture = match feature_set {
                FeatureSet::LayerStacks => "LayerStacks".to_string(),
                FeatureSet::HalfKaHmMerged => format!("HalfKaHmMerged{}", l1),
                FeatureSet::HalfKaSplit => format!("HalfKaSplit{}", l1),
                FeatureSet::HalfKaMerged => format!("HalfKaMerged{}", l1),
                FeatureSet::HalfKaHmSplit => format!("HalfKaHmSplit{}", l1),
                FeatureSet::HalfKP => format!("HalfKP{}", l1),
            };

            Ok(NnueFormatInfo {
                architecture,
                l1_dimension: l1 as u32,
                l2_dimension: l2 as u32,
                l3_dimension: l3 as u32,
                activation,
                version,
                arch_string: arch_str,
            })
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Unknown NNUE version: 0x{version:08X}"),
        )),
    }
}

/// NNUEネットワークへの参照を取得（初期化されていない場合はNone）
///
/// AccumulatorStackVariant の初期化・更新に使用。
pub fn get_network() -> Option<Arc<NNUENetwork>> {
    NETWORK.read().expect("NNUE lock poisoned").clone()
}

// =============================================================================
// 内部ヘルパー関数（ロジック集約用）
// =============================================================================

/// LayerStacks アキュムレータを更新して評価（キャッシュ対応版）
///
/// `LayerStacksNetwork::update_accumulator()` と `evaluate()` に委譲する。
/// AccumulatorCaches（Finny Tables）を使用して refresh を高速化する。
///
/// `nnue-progress-diff` feature 有効時は progress8kpabs モードで差分更新を試み、
/// 結果を `CACHED_PROGRESS_BUCKET` に格納して `evaluate()` 内の全駒スキャンを回避する。
/// Threat なし環境では +3〜4% NPS、Threat あり環境では cache 圧迫で退行するため
/// 運用モデルに応じて明示指定する。
#[cfg(feature = "layerstack-arch")]
#[inline(always)]
pub(crate) fn update_and_evaluate_layer_stacks_cached(
    net: &LayerStacksNetwork,
    pos: &Position,
    stack: &mut LayerStacksAccStack,
    acc_cache: &mut Option<LayerStacksAccCache>,
) -> Value {
    // アキュムレータの更新
    net.update_accumulator(pos, stack, acc_cache);

    // progress8kpabs: 差分更新を試み、結果を CACHED_PROGRESS_BUCKET に格納
    #[cfg(feature = "nnue-progress-diff")]
    if matches!(get_layer_stack_bucket_mode(), LayerStackBucketMode::Progress8KPAbs) {
        // bucket binning は net の num_buckets で駆動する (file から読んだ値、
        // ADR `2026-05-26` §2.4.3)。enum dispatch 経由で各 variant の値を取り、
        // ensure_progress_bucket に伝播する。
        let num_buckets = net.num_buckets();
        let bucket = match stack {
            #[cfg(feature = "layerstacks-1536x16x32")]
            LayerStacksAccStack::L1536x16x32(s) => ensure_progress_bucket(pos, s, num_buckets),
            #[cfg(feature = "layerstacks-1536x32x32")]
            LayerStacksAccStack::L1536x32x32(s) => ensure_progress_bucket(pos, s, num_buckets),
            #[cfg(feature = "layerstacks-768x16x32")]
            LayerStacksAccStack::L768x16x32(s) => ensure_progress_bucket(pos, s, num_buckets),
            #[cfg(feature = "layerstacks-768x8x32")]
            LayerStacksAccStack::L768x8x32(s) => ensure_progress_bucket(pos, s, num_buckets),
            #[cfg(feature = "layerstacks-512x16x32")]
            LayerStacksAccStack::L512x16x32(s) => ensure_progress_bucket(pos, s, num_buckets),
            #[cfg(not(any(
                feature = "layerstacks-1536x16x32",
                feature = "layerstacks-1536x32x32",
                feature = "layerstacks-768x16x32",
                feature = "layerstacks-768x8x32",
                feature = "layerstacks-512x16x32"
            )))]
            _ => unreachable!("no LayerStacks variant enabled"),
        };
        CACHED_PROGRESS_BUCKET.with(|c| c.set(Some(bucket)));
    }

    // 評価
    net.evaluate(pos, stack)
}

/// progress8kpabs の progress_sum を計算済みにして bucket index を返す
///
/// 差分更新が可能な場合（前局面が計算済み、玉移動なし）は DirtyPiece の差分で O(1) 更新。
/// それ以外は全駒スキャンにフォールバック。
#[cfg(feature = "nnue-progress-diff")]
#[inline]
fn ensure_progress_bucket<const L1: usize>(
    pos: &Position,
    stack: &mut super::accumulator_layer_stacks::AccumulatorStackLayerStacks<L1>,
    num_buckets: usize,
) -> usize {
    if !stack.current().computed_progress {
        let weights = get_layer_stack_progress_kpabs_weights();
        let current_entry = stack.current();
        let dirty = &current_entry.dirty_piece;
        let king_moved = dirty.king_moved[0] || dirty.king_moved[1];

        if !king_moved
            && let Some(prev_idx) = current_entry.previous
            && stack.entry_at(prev_idx).computed_progress
        {
            let prev_sum = stack.entry_at(prev_idx).progress_sum;
            let sq_bk = pos.king_square(Color::Black).index();
            let sq_wk = pos.king_square(Color::White).inverse().index();
            let new_sum = update_progress8kpabs_sum_diff(prev_sum, dirty, sq_bk, sq_wk, weights);
            let entry = stack.current_mut();
            entry.progress_sum = new_sum;
            entry.computed_progress = true;
        }

        if !stack.current().computed_progress {
            let sum = compute_progress8kpabs_sum(pos, weights);
            let entry = stack.current_mut();
            entry.progress_sum = sum;
            entry.computed_progress = true;
        }
    }
    progress_sum_to_bucket(stack.current().progress_sum, num_buckets)
}

/// HalfKaHmMerged アキュムレータを更新して評価（内部実装）
#[cfg(feature = "halfkx-arch")]
#[inline]
fn update_and_evaluate_halfka_hm(
    network: &NNUENetwork,
    pos: &Position,
    stack: &mut HalfKaHmMergedStack,
) -> Value {
    // アキュムレータの更新
    if !stack.is_current_computed() {
        let mut updated = false;

        // 1. 直前局面で差分更新を試行
        if let Some(prev_idx) = stack.current_previous()
            && stack.is_entry_computed(prev_idx)
        {
            let dirty = stack.current_dirty_piece();
            network.update_accumulator_halfka_hm(pos, &dirty, stack, prev_idx);
            updated = true;
        }

        // 2. 失敗なら祖先探索 + 複数手差分更新を試行
        if !updated && let Some((source_idx, _depth)) = stack.find_usable_accumulator() {
            updated = network.forward_update_incremental_halfka_hm(pos, stack, source_idx);
        }

        // 3. それでも失敗なら全計算
        if !updated {
            network.refresh_accumulator_halfka_hm(pos, stack);
        }
    }

    // 評価
    network.evaluate_halfka_hm(pos, stack)
}

/// HalfKaSplit アキュムレータを更新して評価（内部実装）
#[cfg(feature = "halfkx-arch")]
#[inline]
fn update_and_evaluate_halfka(
    network: &NNUENetwork,
    pos: &Position,
    stack: &mut HalfKaSplitStack,
) -> Value {
    // アキュムレータの更新
    if !stack.is_current_computed() {
        let mut updated = false;

        // 1. 直前局面で差分更新を試行
        if let Some(prev_idx) = stack.current_previous()
            && stack.is_entry_computed(prev_idx)
        {
            let dirty = stack.current_dirty_piece();
            network.update_accumulator_halfka(pos, &dirty, stack, prev_idx);
            updated = true;
        }

        // 2. 失敗なら祖先探索 + 複数手差分更新を試行
        if !updated && let Some((source_idx, _depth)) = stack.find_usable_accumulator() {
            updated = network.forward_update_incremental_halfka(pos, stack, source_idx);
        }

        // 3. それでも失敗なら全計算
        if !updated {
            network.refresh_accumulator_halfka(pos, stack);
        }
    }

    // 評価
    network.evaluate_halfka(pos, stack)
}

#[cfg(feature = "halfkx-arch")]
fn update_and_evaluate_halfka_merged(
    network: &NNUENetwork,
    pos: &Position,
    stack: &mut HalfKaMergedStack,
) -> Value {
    if !stack.is_current_computed() {
        let mut updated = false;

        if let Some(prev_idx) = stack.current_previous()
            && stack.is_entry_computed(prev_idx)
        {
            let dirty = stack.current_dirty_piece();
            network.update_accumulator_halfka_merged(pos, &dirty, stack, prev_idx);
            updated = true;
        }

        if !updated && let Some((source_idx, _depth)) = stack.find_usable_accumulator() {
            updated = network.forward_update_incremental_halfka_merged(pos, stack, source_idx);
        }

        if !updated {
            network.refresh_accumulator_halfka_merged(pos, stack);
        }
    }

    network.evaluate_halfka_merged(pos, stack)
}

#[cfg(feature = "halfkx-arch")]
fn update_and_evaluate_halfka_hm_split(
    network: &NNUENetwork,
    pos: &Position,
    stack: &mut HalfKaHmSplitStack,
) -> Value {
    if !stack.is_current_computed() {
        let mut updated = false;

        if let Some(prev_idx) = stack.current_previous()
            && stack.is_entry_computed(prev_idx)
        {
            let dirty = stack.current_dirty_piece();
            network.update_accumulator_halfka_hm_split(pos, &dirty, stack, prev_idx);
            updated = true;
        }

        if !updated && let Some((source_idx, _depth)) = stack.find_usable_accumulator() {
            updated = network.forward_update_incremental_halfka_hm_split(pos, stack, source_idx);
        }

        if !updated {
            network.refresh_accumulator_halfka_hm_split(pos, stack);
        }
    }

    network.evaluate_halfka_hm_split(pos, stack)
}

/// HalfKP アキュムレータを更新して評価（内部実装）
#[cfg(feature = "halfkx-arch")]
#[inline]
fn update_and_evaluate_halfkp(
    network: &NNUENetwork,
    pos: &Position,
    stack: &mut HalfKPStack,
) -> Value {
    // アキュムレータの更新
    if !stack.is_current_computed() {
        let mut updated = false;

        // 1. 直前局面で差分更新を試行
        if let Some(prev_idx) = stack.current_previous()
            && stack.is_entry_computed(prev_idx)
        {
            let dirty = stack.current_dirty_piece();
            network.update_accumulator_halfkp(pos, &dirty, stack, prev_idx);
            updated = true;
        }

        // 2. 失敗なら祖先探索 + 複数手差分更新を試行
        if !updated && let Some((source_idx, _depth)) = stack.find_usable_accumulator() {
            updated = network.forward_update_incremental_halfkp(pos, stack, source_idx);
        }

        // 3. それでも失敗なら全計算
        if !updated {
            network.refresh_accumulator_halfkp(pos, stack);
        }
    }

    // 評価
    network.evaluate_halfkp(pos, stack)
}

/// ロードされたNNUEがLayerStacksアーキテクチャかどうか
pub fn is_layer_stacks_loaded() -> bool {
    get_network().is_some_and(|n| n.is_layer_stacks())
}

/// ロードされたNNUEがHalfKaHmMerged256アーキテクチャかどうか
pub fn is_halfka_hm_256_loaded() -> bool {
    get_network().is_some_and(|n| n.is_halfka_hm() && n.l1_size() == 256)
}

/// ロードされたNNUEがHalfKaSplit256アーキテクチャかどうか
pub fn is_halfka_256_loaded() -> bool {
    get_network().is_some_and(|n| n.is_halfka() && n.l1_size() == 256)
}

/// ロードされたNNUEがHalfKaHmMerged512アーキテクチャかどうか
pub fn is_halfka_hm_512_loaded() -> bool {
    get_network().is_some_and(|n| n.is_halfka_hm() && n.l1_size() == 512)
}

/// ロードされたNNUEがHalfKaSplit512アーキテクチャかどうか
pub fn is_halfka_512_loaded() -> bool {
    get_network().is_some_and(|n| n.is_halfka() && n.l1_size() == 512)
}

/// ロードされたNNUEがHalfKaHmMerged1024アーキテクチャかどうか
pub fn is_halfka_hm_1024_loaded() -> bool {
    get_network().is_some_and(|n| n.is_halfka_hm() && n.l1_size() == 1024)
}

/// ロードされたNNUEがHalfKaSplit1024アーキテクチャかどうか
pub fn is_halfka_1024_loaded() -> bool {
    get_network().is_some_and(|n| n.is_halfka() && n.l1_size() == 1024)
}

/// 局面を評価（LayerStacks用）
///
/// LayerStacksAccStack を使って差分更新し、計算済みなら再利用する。
///
/// # Panics
/// NNUEが未ロードかつMaterial評価も無効の場合はパニックする。
#[cfg(feature = "layerstack-arch")]
pub fn evaluate_layer_stacks(pos: &Position, stack: &mut LayerStacksAccStack) -> Value {
    if material::is_material_enabled() {
        return material::evaluate_material(pos);
    }

    let Some(network) = get_network() else {
        panic!(
            "NNUE network not loaded and MaterialLevel not set. \
             Use 'setoption name EvalFile' or 'setoption name MaterialLevel'."
        );
    };

    let net = network.as_layer_stacks();
    update_and_evaluate_layer_stacks_cached(net, pos, stack, &mut None)
}

/// アーキテクチャに応じて適切な評価関数を呼び出す
///
/// AccumulatorStackVariant を受け取り、内部のバリアントに応じて
/// 適切な評価関数を呼び出す。
///
/// `acc_cache` は LayerStacks 用 AccumulatorCaches（Finny Tables）。
/// LayerStacks 以外のアーキテクチャでは無視される。
///
/// # Panics
/// NNUEが未ロードかつMaterial評価も無効の場合はパニックする。
pub fn evaluate_dispatch(
    pos: &Position,
    stack: &mut AccumulatorStackVariant,
    acc_cache: &mut Option<LayerStacksAccCache>,
) -> Value {
    // layerstack-arch 無効ビルドでは LayerStacks variant が存在せず acc_cache は使われない。
    #[cfg(not(feature = "layerstack-arch"))]
    let _ = acc_cache;

    if material::is_material_enabled() {
        return material::evaluate_material(pos);
    }

    let Some(network) = get_network() else {
        panic!(
            "NNUE network not loaded and MaterialLevel not set. \
             Use 'setoption name EvalFile' or 'setoption name MaterialLevel'."
        );
    };

    // バリアントに応じて適切な評価関数を呼び出し
    match stack {
        #[cfg(feature = "layerstack-arch")]
        AccumulatorStackVariant::LayerStacks(s) => {
            let net = network.as_layer_stacks();
            update_and_evaluate_layer_stacks_cached(net, pos, s, acc_cache)
        }
        #[cfg(feature = "halfkx-arch")]
        AccumulatorStackVariant::HalfKaSplit(s) => update_and_evaluate_halfka(&network, pos, s),
        #[cfg(feature = "halfkx-arch")]
        AccumulatorStackVariant::HalfKaHmMerged(s) => {
            update_and_evaluate_halfka_hm(&network, pos, s)
        }
        #[cfg(feature = "halfkx-arch")]
        AccumulatorStackVariant::HalfKaMerged(s) => {
            update_and_evaluate_halfka_merged(&network, pos, s)
        }
        #[cfg(feature = "halfkx-arch")]
        AccumulatorStackVariant::HalfKaHmSplit(s) => {
            update_and_evaluate_halfka_hm_split(&network, pos, s)
        }
        #[cfg(feature = "halfkx-arch")]
        AccumulatorStackVariant::HalfKP(s) => update_and_evaluate_halfkp(&network, pos, s),
        #[cfg(not(feature = "halfkx-arch"))]
        AccumulatorStackVariant::HalfKaSplit(_)
        | AccumulatorStackVariant::HalfKaHmMerged(_)
        | AccumulatorStackVariant::HalfKaMerged(_)
        | AccumulatorStackVariant::HalfKaHmSplit(_)
        | AccumulatorStackVariant::HalfKP(_) => {
            unreachable!("halfkx-arch disabled: HalfKX variant cannot exist in this build")
        }
    }
}

/// アキュムレータを計算済みにする（評価値の計算はしない）
///
/// TTヒット時など、評価値はTTから取得するが、
/// 次のノードの差分更新のためにアキュムレータだけは計算しておく必要がある場合に使用。
/// YaneuraOu/Stockfish互換の動作を実現する。
///
/// `acc_cache` は LayerStacks 用 AccumulatorCaches（Finny Tables）。
pub fn ensure_accumulator_computed(
    pos: &Position,
    stack: &mut AccumulatorStackVariant,
    acc_cache: &mut Option<LayerStacksAccCache>,
) {
    // layerstack-arch 無効ビルドでは LayerStacks variant が存在せず acc_cache は使われない。
    #[cfg(not(feature = "layerstack-arch"))]
    let _ = acc_cache;

    // NNUEがなければ何もしない
    let Some(network) = get_network() else {
        return;
    };

    // バリアントに応じてアキュムレータを更新（評価はしない）
    match stack {
        #[cfg(feature = "layerstack-arch")]
        AccumulatorStackVariant::LayerStacks(s) => {
            let net = network.as_layer_stacks();
            net.update_accumulator(pos, s, acc_cache);
        }
        #[cfg(feature = "halfkx-arch")]
        AccumulatorStackVariant::HalfKaSplit(s) => {
            update_accumulator_only_halfka(&network, pos, s);
        }
        #[cfg(feature = "halfkx-arch")]
        AccumulatorStackVariant::HalfKaHmMerged(s) => {
            update_accumulator_only_halfka_hm(&network, pos, s);
        }
        #[cfg(feature = "halfkx-arch")]
        AccumulatorStackVariant::HalfKaMerged(s) => {
            update_accumulator_only_halfka_merged(&network, pos, s);
        }
        #[cfg(feature = "halfkx-arch")]
        AccumulatorStackVariant::HalfKaHmSplit(s) => {
            update_accumulator_only_halfka_hm_split(&network, pos, s);
        }
        #[cfg(feature = "halfkx-arch")]
        AccumulatorStackVariant::HalfKP(s) => {
            update_accumulator_only_halfkp(&network, pos, s);
        }
        #[cfg(not(feature = "halfkx-arch"))]
        AccumulatorStackVariant::HalfKaSplit(_)
        | AccumulatorStackVariant::HalfKaHmMerged(_)
        | AccumulatorStackVariant::HalfKaMerged(_)
        | AccumulatorStackVariant::HalfKaHmSplit(_)
        | AccumulatorStackVariant::HalfKP(_) => {
            unreachable!("halfkx-arch disabled: HalfKX variant cannot exist in this build")
        }
    }
}

/// HalfKaHmMerged アキュムレータを更新のみ（評価なし）
#[cfg(feature = "halfkx-arch")]
#[inline]
fn update_accumulator_only_halfka_hm(
    network: &NNUENetwork,
    pos: &Position,
    stack: &mut HalfKaHmMergedStack,
) {
    if stack.is_current_computed() {
        count_already_computed!();
        return;
    }

    let mut updated = false;

    // 直前局面で差分更新を試行
    if let Some(prev_idx) = stack.current_previous()
        && stack.is_entry_computed(prev_idx)
    {
        let dirty = stack.current_dirty_piece();
        network.update_accumulator_halfka_hm(pos, &dirty, stack, prev_idx);
        count_update!();
        updated = true;
    }

    // 失敗なら全計算
    if !updated {
        network.refresh_accumulator_halfka_hm(pos, stack);
        count_refresh!();
    }
}

/// HalfKaSplit アキュムレータを更新のみ（評価なし）
#[cfg(feature = "halfkx-arch")]
#[inline]
fn update_accumulator_only_halfka(
    network: &NNUENetwork,
    pos: &Position,
    stack: &mut HalfKaSplitStack,
) {
    if stack.is_current_computed() {
        count_already_computed!();
        return;
    }

    let mut updated = false;

    // 直前局面で差分更新を試行
    if let Some(prev_idx) = stack.current_previous()
        && stack.is_entry_computed(prev_idx)
    {
        let dirty = stack.current_dirty_piece();
        network.update_accumulator_halfka(pos, &dirty, stack, prev_idx);
        count_update!();
        updated = true;
    }

    // 失敗なら全計算
    if !updated {
        network.refresh_accumulator_halfka(pos, stack);
        count_refresh!();
    }
}

#[cfg(feature = "halfkx-arch")]
fn update_accumulator_only_halfka_merged(
    network: &NNUENetwork,
    pos: &Position,
    stack: &mut HalfKaMergedStack,
) {
    if stack.is_current_computed() {
        count_already_computed!();
        return;
    }

    let mut updated = false;

    if let Some(prev_idx) = stack.current_previous()
        && stack.is_entry_computed(prev_idx)
    {
        let dirty = stack.current_dirty_piece();
        network.update_accumulator_halfka_merged(pos, &dirty, stack, prev_idx);
        count_update!();
        updated = true;
    }

    if !updated {
        network.refresh_accumulator_halfka_merged(pos, stack);
        count_refresh!();
    }
}

#[cfg(feature = "halfkx-arch")]
fn update_accumulator_only_halfka_hm_split(
    network: &NNUENetwork,
    pos: &Position,
    stack: &mut HalfKaHmSplitStack,
) {
    if stack.is_current_computed() {
        count_already_computed!();
        return;
    }

    let mut updated = false;

    if let Some(prev_idx) = stack.current_previous()
        && stack.is_entry_computed(prev_idx)
    {
        let dirty = stack.current_dirty_piece();
        network.update_accumulator_halfka_hm_split(pos, &dirty, stack, prev_idx);
        count_update!();
        updated = true;
    }

    if !updated {
        network.refresh_accumulator_halfka_hm_split(pos, stack);
        count_refresh!();
    }
}

/// HalfKP アキュムレータを更新のみ（評価なし）
#[cfg(feature = "halfkx-arch")]
#[inline]
fn update_accumulator_only_halfkp(network: &NNUENetwork, pos: &Position, stack: &mut HalfKPStack) {
    if stack.is_current_computed() {
        count_already_computed!();
        return;
    }

    let mut updated = false;

    // 直前局面で差分更新を試行
    if let Some(prev_idx) = stack.current_previous()
        && stack.is_entry_computed(prev_idx)
    {
        let dirty = stack.current_dirty_piece();
        network.update_accumulator_halfkp(pos, &dirty, stack, prev_idx);
        count_update!();
        updated = true;
    }

    // 失敗なら全計算
    if !updated {
        network.refresh_accumulator_halfkp(pos, stack);
        count_refresh!();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nnue::constants::DEFAULT_NUM_BUCKETS;
    use crate::position::SFEN_HIRATE;

    /// NNUENetwork のアーキテクチャ自動検出テスト
    ///
    /// 外部NNUEファイルが必要なため通常はスキップ。
    /// 実行方法: `NNUE_TEST_FILE=/path/to/file.nnue cargo test test_nnue_network_auto_detect_layer_stacks -- --ignored`
    ///
    /// テスト結果 (epoch82.nnue):
    /// - LayerStacks として正しく認識される
    /// - 評価値: 0 (学習初期のモデル)
    #[cfg(feature = "layerstack-arch")]
    #[test]
    #[ignore]
    fn test_nnue_network_auto_detect_layer_stacks() {
        let path = std::env::var("NNUE_TEST_FILE")
            .unwrap_or_else(|_| "/path/to/your/layer_stacks.nnue".to_string());
        let network = match NNUENetwork::load(path) {
            Ok(n) => n,
            Err(e) => {
                eprintln!("Skipping test: {e}");
                return;
            }
        };

        // LayerStacks として認識されることを確認
        assert!(network.is_layer_stacks(), "epoch82.nnue should be detected as LayerStacks");
        assert_eq!(network.architecture_name(), "LayerStacks");

        // LayerStacks 用の評価が動作することを確認
        let mut pos = crate::position::Position::new();
        pos.set_sfen(SFEN_HIRATE).unwrap();

        let ls_net = network.as_layer_stacks();
        let mut stack = ls_net.new_acc_stack();
        let mut acc_cache = Some(ls_net.new_acc_cache());
        ls_net.update_accumulator(&pos, &mut stack, &mut acc_cache);
        let value = ls_net.evaluate(&pos, &stack);
        eprintln!("LayerStacks evaluate: {}", value.raw());

        // 評価値が妥当な範囲内
        assert!(value.raw().abs() < 1000);
    }

    /// detect_format のファイルサイズベース検出テスト
    ///
    /// AobaNNUE.bin のようにヘッダーが不正確なファイルでも
    /// ファイルサイズから正確なアーキテクチャを検出できることを確認する。
    ///
    /// 実行方法:
    /// ```bash
    /// NNUE_AOBA_FILE=/path/to/AobaNNUE.bin cargo test test_detect_format_aoba -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore]
    fn test_detect_format_aoba() {
        let path = std::env::var("NNUE_AOBA_FILE").unwrap_or_else(|_| "AobaNNUE.bin".to_string());
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("Skipping test: {e}");
                return;
            }
        };

        let file_size = bytes.len() as u64;
        let info = detect_format(&bytes, file_size).expect("Failed to detect format");

        eprintln!("File: {path}");
        eprintln!("Architecture: {}", info.architecture);
        eprintln!(
            "L1: {}, L2: {}, L3: {}",
            info.l1_dimension, info.l2_dimension, info.l3_dimension
        );
        eprintln!("Activation: {}", info.activation);
        eprintln!("Arch string (header): {}", info.arch_string);

        // AobaNNUE.bin はヘッダーで 256 を主張するが、実際は 768-16-64
        assert_eq!(
            info.architecture, "HalfKP768",
            "Should detect HalfKP768 from file size, not HalfKP256 from header"
        );
        assert_eq!(info.l1_dimension, 768, "L1 should be 768, not 256 from header");
        assert_eq!(info.l2_dimension, 16, "L2 should be 16");
        assert_eq!(info.l3_dimension, 64, "L3 should be 64");
        // ヘッダーが不正確であることを確認（256 を主張している）
        assert!(
            info.arch_string.contains("256"),
            "Header should claim 256, but file size detection should override it"
        );
    }

    /// detect_format のフォールバックテスト
    ///
    /// ファイルサイズベースの検出が失敗した場合に、
    /// ヘッダーのパース結果にフォールバックすることを確認する。
    #[test]
    fn test_detect_format_fallback_to_header() {
        // 架空のファイルサイズ（既知のアーキテクチャと一致しない）
        let unknown_file_size = 12345678u64;

        // 有効なヘッダーを持つバイト列を作成
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&NNUE_VERSION_HALFKA.to_le_bytes()); // version
        bytes.extend_from_slice(&0u32.to_le_bytes()); // hash

        let arch_str = "Features=HalfKaHmMerged[73305->512x2],l2=8,l3=96";
        let arch_len = arch_str.len() as u32;
        bytes.extend_from_slice(&arch_len.to_le_bytes());
        bytes.extend_from_slice(arch_str.as_bytes());

        let info =
            detect_format(&bytes, unknown_file_size).expect("Should fallback to header parsing");

        // ヘッダーからパースした値が使われることを確認
        assert_eq!(info.architecture, "HalfKaHmMerged512");
        assert_eq!(info.l1_dimension, 512);
        assert_eq!(info.l2_dimension, 8);
        assert_eq!(info.l3_dimension, 96);
    }

    /// detect_format のエラーハンドリングテスト
    #[test]
    fn test_detect_format_error_cases() {
        // ケース1: ファイルサイズが小さすぎる
        let bytes = vec![0u8; 5];
        let result = detect_format(&bytes, 5);
        assert!(result.is_err(), "Should fail for too small file");
        assert!(
            result.unwrap_err().to_string().contains("too small"),
            "Error message should mention 'too small'"
        );

        // ケース2: arch_len = 0（不正）
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&NNUE_VERSION.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes()); // arch_len = 0
        let result = detect_format(&bytes, 100);
        assert!(result.is_err(), "Should fail for arch_len = 0");
        assert!(
            result.unwrap_err().to_string().contains("Invalid arch string length"),
            "Error message should mention invalid arch string length"
        );

        // ケース3: arch_len が MAX_ARCH_LEN を超える
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&NNUE_VERSION.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&(MAX_ARCH_LEN as u32 + 1).to_le_bytes());
        let result = detect_format(&bytes, 100);
        assert!(result.is_err(), "Should fail for arch_len > MAX_ARCH_LEN");

        // ケース4: バッファが arch_len 分のデータを含まない
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&NNUE_VERSION.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&100u32.to_le_bytes()); // arch_len = 100
        // bytes は 12 バイトのみ、arch_str 用のデータがない
        let result = detect_format(&bytes, 1000);
        assert!(result.is_err(), "Should fail when buffer is too small for arch_str");

        // ケース5: 不正なバージョン
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0xDEADBEEFu32.to_le_bytes());
        bytes.extend_from_slice(&[0u8; 100]);
        let result = detect_format(&bytes, 112);
        assert!(result.is_err(), "Should fail for unknown version");
        assert!(
            result.unwrap_err().to_string().contains("Unknown NNUE version"),
            "Error message should mention unknown version"
        );
    }

    /// parse_fv_scale_from_arch のユニットテスト
    #[test]
    fn test_parse_fv_scale_from_arch() {
        // bullet-shogi 形式の arch_str
        assert_eq!(
            parse_fv_scale_from_arch(
                "Features=HalfKaHmMerged^[73305->256x2]-SCReLU,fv_scale=13,qa=127,qb=64,scale=600"
            ),
            Some(13)
        );
        assert_eq!(
            parse_fv_scale_from_arch(
                "Features=HalfKaHmMerged^[73305->512x2]-SCReLU,fv_scale=20,qa=127,qb=64,scale=400"
            ),
            Some(20)
        );
        assert_eq!(
            parse_fv_scale_from_arch(
                "Features=HalfKaHmMerged^[73305->1024x2]-SCReLU,fv_scale=16,qa=127,qb=64,scale=508"
            ),
            Some(16)
        );

        // fv_scale が含まれていない従来形式
        assert_eq!(parse_fv_scale_from_arch("Features=HalfKP[125388->256x2]"), None);
        assert_eq!(parse_fv_scale_from_arch("Features=HalfKaHmMerged^[73305->512x2]"), None);

        // 空文字列
        assert_eq!(parse_fv_scale_from_arch(""), None);

        // 不正な fv_scale 値（文字列）
        assert_eq!(
            parse_fv_scale_from_arch("Features=HalfKaHmMerged^[73305->256x2],fv_scale=abc"),
            None
        );
    }

    /// parse_fv_scale_from_arch の境界値・エラーケーステスト
    #[test]
    fn test_parse_fv_scale_edge_cases() {
        // 境界値（許容範囲内）
        assert_eq!(parse_fv_scale_from_arch("fv_scale=1"), Some(1));
        assert_eq!(parse_fv_scale_from_arch("fv_scale=128"), Some(128));
        assert_eq!(parse_fv_scale_from_arch("fv_scale=64"), Some(64));

        // 境界値（範囲外 - ゼロ除算防止）
        assert_eq!(parse_fv_scale_from_arch("fv_scale=0"), None);
        assert_eq!(parse_fv_scale_from_arch("fv_scale=129"), None);

        // 不正な値（負数）
        assert_eq!(parse_fv_scale_from_arch("fv_scale=-1"), None);
        assert_eq!(parse_fv_scale_from_arch("fv_scale=-100"), None);

        // 不正な値（極端に大きい値）
        assert_eq!(parse_fv_scale_from_arch("fv_scale=99999"), None);
        assert_eq!(parse_fv_scale_from_arch("fv_scale=2147483647"), None);

        // ホワイトスペースを含む（パース失敗を期待）
        assert_eq!(parse_fv_scale_from_arch("fv_scale= 16"), None);
        assert_eq!(parse_fv_scale_from_arch("fv_scale=16 "), None);

        // 複数の fv_scale がある場合（最初のものが使用される）
        assert_eq!(parse_fv_scale_from_arch("fv_scale=10,fv_scale=20"), Some(10));

        // fv_scale= の後に何もない
        assert_eq!(parse_fv_scale_from_arch("fv_scale="), None);

        // 小数点を含む（パース失敗を期待）
        assert_eq!(parse_fv_scale_from_arch("fv_scale=16.5"), None);

        // プレフィックスが部分一致する場合（マッチしない）
        assert_eq!(parse_fv_scale_from_arch("my_fv_scale=16"), None);
        assert_eq!(parse_fv_scale_from_arch("fv_scale_v2=16"), None);
    }

    #[test]
    fn test_parse_layer_stack_bucket_mode() {
        assert_eq!(
            parse_layer_stack_bucket_mode("progress8kpabs"),
            Some(LayerStackBucketMode::Progress8KPAbs)
        );
        assert_eq!(
            parse_layer_stack_bucket_mode("PROGRESS8KPABS"),
            Some(LayerStackBucketMode::Progress8KPAbs)
        );
        assert_eq!(
            parse_layer_stack_bucket_mode(" progress8kpabs "),
            Some(LayerStackBucketMode::Progress8KPAbs)
        );
        assert_eq!(parse_layer_stack_bucket_mode("unknown"), None);
        assert_eq!(parse_layer_stack_bucket_mode("progress8"), None);
        assert_eq!(parse_layer_stack_bucket_mode("progress8gikou"), None);
        assert_eq!(parse_layer_stack_bucket_mode("kingrank9"), None);
        assert_eq!(parse_layer_stack_bucket_mode("ply9"), None);
    }

    #[test]
    fn test_parse_nnue_architecture() {
        assert_eq!(parse_nnue_architecture("auto"), Some(NNUEArchitectureOverride::Auto));
        assert_eq!(parse_nnue_architecture("AUTO"), Some(NNUEArchitectureOverride::Auto));
        assert_eq!(parse_nnue_architecture("Auto"), Some(NNUEArchitectureOverride::Auto));
        assert_eq!(parse_nnue_architecture("halfkp"), Some(NNUEArchitectureOverride::HalfKP));
        // 旧名 (underscore short form) と新名 (underscore long form) の両方を受理。
        assert_eq!(
            parse_nnue_architecture("halfka_hm"),
            Some(NNUEArchitectureOverride::HalfKaHmMerged)
        );
        assert_eq!(
            parse_nnue_architecture("halfka_hm_merged"),
            Some(NNUEArchitectureOverride::HalfKaHmMerged)
        );
        assert_eq!(parse_nnue_architecture("halfka"), Some(NNUEArchitectureOverride::HalfKaSplit));
        assert_eq!(
            parse_nnue_architecture("halfka_split"),
            Some(NNUEArchitectureOverride::HalfKaSplit)
        );
        assert_eq!(
            parse_nnue_architecture("layerstacks"),
            Some(NNUEArchitectureOverride::LayerStacks)
        );
        assert_eq!(
            parse_nnue_architecture("layerstacks-psqt"),
            Some(NNUEArchitectureOverride::LayerStacksPSQT)
        );
        assert_eq!(
            parse_nnue_architecture("layerstacks_psqt"),
            Some(NNUEArchitectureOverride::LayerStacksPSQT)
        );
        assert_eq!(parse_nnue_architecture("unknown"), None);
        assert_eq!(parse_nnue_architecture(""), None);
    }

    #[test]
    fn test_compute_layer_stack_progress8kpabs_bucket_index_range() {
        let mut pos = Position::new();
        pos.set_sfen(SFEN_HIRATE).unwrap();

        let weights = vec![0.0f32; SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS];
        // legacy 配布 net 互換の N=9 (DEFAULT_NUM_BUCKETS) で確認: sigmoid(0) = 0.5 →
        // floor(0.5 * 9) = 4 (中間 bucket)。
        let b = compute_layer_stack_progress8kpabs_bucket_index(
            &pos,
            pos.side_to_move(),
            &weights,
            DEFAULT_NUM_BUCKETS,
        );
        assert_eq!(b, 4, "zero-weight progress8kpabs at N=9 should map to bucket 4");
    }

    #[test]
    fn test_progress_bucket_thresholds_match_sigmoid() {
        // tatara `progress_kpabs::bucket` の `floor(p * N).clamp(0, N-1)` と engine 側
        // partition_point 方式が N ∈ {1, 2, 3, 4, 5, 8, 9, 12, 16} で一致することを
        // 各 sum 値で確認 (ADR `2026-05-26` §2.4.1 の等価性 + tests)。
        let sigmoid_bucket = |sum: f32, n: usize| -> usize {
            let p = (1.0 / (1.0 + (-sum).exp())).clamp(0.0, 1.0);
            let raw = (p * n as f32).floor() as i32;
            raw.clamp(0, (n as i32) - 1) as usize
        };
        let sums = [
            f32::NEG_INFINITY,
            -10.0,
            -5.0,
            -3.0,
            -2.5,
            -1.5,
            -0.8,
            -0.3,
            0.0,
            0.3,
            0.8,
            1.5,
            2.5,
            3.0,
            5.0,
            10.0,
            f32::INFINITY,
        ];
        for &n in &[1usize, 2, 3, 4, 5, 8, 9, 12, MAX_LAYER_STACK_BUCKETS] {
            for &sum in &sums {
                let want = sigmoid_bucket(sum, n);
                let got = progress_sum_to_bucket(sum, n);
                assert_eq!(want, got, "mismatch at n={n}, sum={sum}");
            }
        }
        // 閾値 t_k ちょうど での tie-break: sum = t_k ⇔ sigmoid(t_k) = k/N で
        // tatara floor(k) = k、engine partition_point = k。
        for n in 2..=MAX_LAYER_STACK_BUCKETS {
            for k in 1..n {
                let t_k = ((k as f32) / (n as f32 - k as f32)).ln();
                assert_eq!(
                    progress_sum_to_bucket(t_k, n),
                    k,
                    "tie-break failed at n={n}, k={k}, t_k={t_k}"
                );
            }
        }
    }

    #[test]
    fn test_progress_sum_to_bucket_extremes() {
        for n in 1..=MAX_LAYER_STACK_BUCKETS {
            assert_eq!(progress_sum_to_bucket(f32::NEG_INFINITY, n), 0);
            assert_eq!(progress_sum_to_bucket(f32::INFINITY, n), n - 1);
        }
        // N=1 は常に bucket 0
        for &sum in &[-100.0, 0.0, 100.0] {
            assert_eq!(progress_sum_to_bucket(sum, 1), 0);
        }
    }

    /// LayerStack num_buckets-header layout の最小ヘッダを in-memory で作成
    ///
    /// `num_buckets` 検証や override 分岐の reject 経路を unit test するために、
    /// FT/PSQT/LayerStack block を含まないヘッダだけの buffer を返す。
    /// load 側は header 検証で reject する想定なので、それ以降の block は不要。
    #[cfg(feature = "layerstack-arch")]
    fn build_num_buckets_header(num_buckets: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&NNUE_VERSION_LAYERSTACK_NUM_BUCKETS.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes()); // network_hash (dummy)
        let arch = b"Features=HalfKaHmMerged^(40),LayerStacks,FV_SCALE=28";
        bytes.extend_from_slice(&(arch.len() as u32).to_le_bytes());
        bytes.extend_from_slice(arch);
        bytes.extend_from_slice(&num_buckets.to_le_bytes());
        bytes
    }

    /// num_buckets-header layout で `num_buckets = 0` / 上限超過の値は
    /// `InvalidData` で reject される
    #[cfg(feature = "layerstack-arch")]
    #[test]
    fn test_layer_stack_num_buckets_out_of_range_rejected() {
        for &n in &[0u32, (MAX_LAYER_STACK_BUCKETS as u32) + 1, 100, 1024] {
            let bytes = build_num_buckets_header(n);
            let err = match NNUENetwork::from_bytes(&bytes) {
                Ok(_) => panic!(
                    "num_buckets={n} は reject されるべき (許容範囲 1..={MAX_LAYER_STACK_BUCKETS})"
                ),
                Err(e) => e,
            };
            assert_eq!(err.kind(), io::ErrorKind::InvalidData, "n={n}");
            let msg = err.to_string();
            assert!(
                msg.contains("num_buckets") || msg.contains(&n.to_string()),
                "n={n} のエラーメッセージに num_buckets / 値が含まれていない: {msg}"
            );
        }
    }

    /// num_buckets-header layout を `NNUE_ARCHITECTURE=HalfKP` 等の
    /// 非-LayerStack override で読むと早期 reject される (silent misread を防ぐ)
    #[cfg(feature = "layerstack-arch")]
    #[test]
    fn test_layer_stack_num_buckets_header_with_non_layerstack_override_rejected() {
        let bytes = build_num_buckets_header(9);
        let previous_override = get_nnue_architecture_override();
        for override_mode in [
            NNUEArchitectureOverride::HalfKP,
            NNUEArchitectureOverride::HalfKaHmMerged,
            NNUEArchitectureOverride::HalfKaSplit,
        ] {
            set_nnue_architecture_override(override_mode);
            let err = match NNUENetwork::from_bytes(&bytes) {
                Ok(_) => panic!(
                    "num_buckets-header net を {override_mode:?} override で読んだら reject されるべき"
                ),
                Err(e) => e,
            };
            assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{override_mode:?}");
            let msg = err.to_string();
            assert!(
                msg.contains("LayerStack"),
                "{override_mode:?} のエラーメッセージに LayerStack が含まれていない: {msg}"
            );
        }
        set_nnue_architecture_override(previous_override);
    }

    #[cfg(feature = "nnue-progress-diff")]
    #[test]
    fn test_progress8kpabs_diff_update() {
        use crate::types::Move;

        // ランダムな重みを生成（固定シード）
        let mut weights = vec![0.0f32; SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS];
        let mut rng: u64 = 12345;
        for w in weights.iter_mut() {
            // 簡易 xorshift
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            *w = ((rng as i64 % 1000) as f32) / 1000.0;
        }

        let mut pos = Position::new();
        pos.set_sfen(SFEN_HIRATE).unwrap();

        // 初期局面での全駒スキャン sum
        let sum0 = compute_progress8kpabs_sum(&pos, &weights);

        // いくつかの手を実行して差分更新と全計算を比較
        let moves_usi = [
            "7g7f", "3c3d", "2g2f", "8c8d", "2f2e", "8d8e", "6i7h", "4a3b",
        ];
        let mut prev_sum = sum0;

        for &mv_str in &moves_usi {
            let mv = Move::from_usi(mv_str).expect("valid move");
            let gives_check = pos.gives_check(mv);
            let dirty = pos.do_move(mv, gives_check);

            // 全駒スキャンによる正解値
            let expected_sum = compute_progress8kpabs_sum(&pos, &weights);
            let expected_bucket = progress_sum_to_bucket(expected_sum, DEFAULT_NUM_BUCKETS);

            if dirty.king_moved[0] || dirty.king_moved[1] {
                // 玉が動いた場合は差分更新不可（全計算にフォールバック）
                prev_sum = expected_sum;
            } else {
                // 差分更新
                let sq_bk = pos.king_square(Color::Black).index();
                let sq_wk = pos.king_square(Color::White).inverse().index();
                let diff_sum =
                    update_progress8kpabs_sum_diff(prev_sum, &dirty, sq_bk, sq_wk, &weights);
                let diff_bucket = progress_sum_to_bucket(diff_sum, DEFAULT_NUM_BUCKETS);

                assert!(
                    (diff_sum - expected_sum).abs() < 1e-5,
                    "sum mismatch after {mv_str}: diff={diff_sum}, expected={expected_sum}"
                );
                assert_eq!(diff_bucket, expected_bucket, "bucket mismatch after {mv_str}");

                prev_sum = diff_sum;
            }
        }
    }

    /// HalfKP 768x2-16-64 ファイルの読み込みテスト
    ///
    /// nnue-pytorch がハードコードした不正確なヘッダーを持つファイルを
    /// ファイルサイズベースの自動検出で正しく読み込めることを確認する。
    ///
    /// 実行方法:
    /// ```bash
    /// cargo test test_nnue_halfkp_768_auto_detect -- --ignored
    /// ```
    #[test]
    #[ignore]
    fn test_nnue_halfkp_768_auto_detect() {
        // ワークスペースルートからの相対パス
        let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("Failed to find workspace root");
        let default_path = workspace_root
            .join("eval/halfkp_768x2-16-64_crelu/AobaNNUE_HalfKP_768x2_16_64_FV_SCALE_40.bin");
        let path = std::env::var("NNUE_HALFKP_768_FILE")
            .unwrap_or_else(|_| default_path.display().to_string());

        let network = match NNUENetwork::load(&path) {
            Ok(n) => n,
            Err(e) => {
                eprintln!("Skipping test: {e}");
                return;
            }
        };

        // HalfKP として認識されることを確認
        assert!(network.is_halfkp(), "File should be detected as HalfKP");

        // L1=768 が検出されることを確認
        assert_eq!(network.l1_size(), 768, "L1 should be 768");

        // アーキテクチャ仕様を確認
        let spec = network.architecture_spec();
        assert_eq!(spec.l1, 768, "spec.l1 should be 768");
        assert_eq!(spec.l2, 16, "spec.l2 should be 16");
        assert_eq!(spec.l3, 64, "spec.l3 should be 64");

        eprintln!("Successfully loaded HalfKP 768x2-16-64 network");
        eprintln!("Architecture name: {}", network.architecture_name());

        // HalfKP 用の評価が動作することを確認
        let mut pos = crate::position::Position::new();
        pos.set_sfen(SFEN_HIRATE).unwrap();

        // HalfKPStack を作成して評価
        use crate::nnue::halfkp::HalfKPStack;
        let mut stack = HalfKPStack::from_network(match &network {
            NNUENetwork::HalfKP(net) => net,
            _ => unreachable!(),
        });

        network.refresh_accumulator_halfkp(&pos, &mut stack);
        let value = network.evaluate_halfkp(&pos, &stack);

        eprintln!("HalfKP 768 evaluate: {}", value.raw());

        // 評価値が妥当な範囲内
        assert!(value.raw().abs() < 10000, "Evaluation {} is out of expected range", value.raw());
    }

    /// HalfKaHmMerged 256x2-32-32 ファイルの読み込みテスト
    ///
    /// nnue-pytorch 形式のファイルを FT hash を使って正しく読み込めることを確認する。
    ///
    /// 実行方法:
    /// ```bash
    /// cargo test test_nnue_halfka_hm_256_auto_detect -- --ignored
    /// ```
    #[test]
    #[ignore]
    fn test_nnue_halfka_hm_256_auto_detect() {
        // ワークスペースルートからの相対パス
        let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("Failed to find workspace root");
        let default_path = workspace_root.join("eval/halfka_hm_256x2-32-32_crelu/v28_epoch65.nnue");
        let path = std::env::var("NNUE_HALFKA_HM_256_FILE")
            .unwrap_or_else(|_| default_path.display().to_string());

        let network = match NNUENetwork::load(&path) {
            Ok(n) => n,
            Err(e) => {
                eprintln!("Skipping test: {e}");
                return;
            }
        };

        // HalfKaHmMerged として認識されることを確認
        assert!(network.is_halfka_hm(), "File should be detected as HalfKaHmMerged");

        // L1=256 が検出されることを確認
        assert_eq!(network.l1_size(), 256, "L1 should be 256");

        // アーキテクチャ仕様を確認
        let spec = network.architecture_spec();
        assert_eq!(spec.l1, 256, "spec.l1 should be 256");
        assert_eq!(spec.l2, 32, "spec.l2 should be 32");
        assert_eq!(spec.l3, 32, "spec.l3 should be 32");

        eprintln!("Successfully loaded HalfKaHmMerged 256x2-32-32 network");
        eprintln!("Architecture name: {}", network.architecture_name());

        // HalfKaHmMerged 用の評価が動作することを確認
        let mut pos = crate::position::Position::new();
        pos.set_sfen(SFEN_HIRATE).unwrap();

        // HalfKaHmMergedStack を作成して評価
        use crate::nnue::halfka_hm_merged::HalfKaHmMergedStack;
        let mut stack = HalfKaHmMergedStack::from_network(match &network {
            NNUENetwork::HalfKaHmMerged(net) => net,
            _ => unreachable!(),
        });

        network.refresh_accumulator_halfka_hm(&pos, &mut stack);
        let value = network.evaluate_halfka_hm(&pos, &stack);

        eprintln!("HalfKaHmMerged 256 evaluate: {}", value.raw());

        // 評価値が妥当な範囲内
        assert!(value.raw().abs() < 10000, "Evaluation {} is out of expected range", value.raw());
    }

    /// HalfKaHmMerged 1024x2-8-96 ファイルの読み込みテスト
    ///
    /// 実行方法:
    /// ```bash
    /// cargo test test_nnue_halfka_hm_1024_auto_detect -- --ignored
    /// ```
    #[test]
    #[ignore]
    fn test_nnue_halfka_hm_1024_auto_detect() {
        // ワークスペースルートからの相対パス
        let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("Failed to find workspace root");
        let default_path = workspace_root.join("eval/halfka_hm_1024x2-8-96_crelu/epoch20_v2.nnue");
        let path = std::env::var("NNUE_HALFKA_HM_1024_FILE")
            .unwrap_or_else(|_| default_path.display().to_string());

        let network = match NNUENetwork::load(&path) {
            Ok(n) => n,
            Err(e) => {
                eprintln!("Skipping test: {e}");
                return;
            }
        };

        // HalfKaHmMerged として認識されることを確認
        assert!(network.is_halfka_hm(), "File should be detected as HalfKaHmMerged");

        // L1=1024 が検出されることを確認
        assert_eq!(network.l1_size(), 1024, "L1 should be 1024");

        // アーキテクチャ仕様を確認
        let spec = network.architecture_spec();
        assert_eq!(spec.l1, 1024, "spec.l1 should be 1024");
        assert_eq!(spec.l2, 8, "spec.l2 should be 8");
        assert_eq!(spec.l3, 96, "spec.l3 should be 96");

        eprintln!("Successfully loaded HalfKaHmMerged 1024x2-8-96 network");
        eprintln!("Architecture name: {}", network.architecture_name());

        // HalfKaHmMerged 用の評価が動作することを確認
        let mut pos = crate::position::Position::new();
        pos.set_sfen(SFEN_HIRATE).unwrap();

        // HalfKaHmMergedStack を作成して評価
        use crate::nnue::halfka_hm_merged::HalfKaHmMergedStack;
        let mut stack = HalfKaHmMergedStack::from_network(match &network {
            NNUENetwork::HalfKaHmMerged(net) => net,
            _ => unreachable!(),
        });

        network.refresh_accumulator_halfka_hm(&pos, &mut stack);
        let value = network.evaluate_halfka_hm(&pos, &stack);

        eprintln!("HalfKaHmMerged 1024 evaluate: {}", value.raw());

        // 評価値が妥当な範囲内
        assert!(value.raw().abs() < 10000, "Evaluation {} is out of expected range", value.raw());
    }

    /// HalfKP 256x2-32-32 ファイル (suisho5.bin) の読み込みテスト
    ///
    /// ファイルサイズベースの検出で正しく読み込めることを確認する。
    ///
    /// 実行方法:
    /// ```bash
    /// cargo test test_nnue_halfkp_256_suisho5 -- --ignored
    /// ```
    #[test]
    #[ignore]
    fn test_nnue_halfkp_256_suisho5() {
        // ワークスペースルートからの相対パス
        let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("Failed to find workspace root");
        let default_path = workspace_root.join("eval/halfkp_256x2-32-32_crelu/suisho5.bin");
        let path = std::env::var("NNUE_HALFKP_256_FILE")
            .unwrap_or_else(|_| default_path.display().to_string());

        let network = match NNUENetwork::load(&path) {
            Ok(n) => n,
            Err(e) => {
                eprintln!("Skipping test: {e}");
                return;
            }
        };

        // HalfKP として認識されることを確認
        assert!(network.is_halfkp(), "File should be detected as HalfKP");

        // L1=256 が検出されることを確認
        assert_eq!(network.l1_size(), 256, "L1 should be 256");

        // アーキテクチャ仕様を確認
        let spec = network.architecture_spec();
        assert_eq!(spec.l1, 256, "spec.l1 should be 256");
        assert_eq!(spec.l2, 32, "spec.l2 should be 32");
        assert_eq!(spec.l3, 32, "spec.l3 should be 32");

        eprintln!("Successfully loaded HalfKP 256x2-32-32 network (suisho5)");
        eprintln!("Architecture name: {}", network.architecture_name());

        // HalfKP 用の評価が動作することを確認
        let mut pos = crate::position::Position::new();
        pos.set_sfen(SFEN_HIRATE).unwrap();

        // HalfKPStack を作成して評価
        use crate::nnue::halfkp::HalfKPStack;
        let mut stack = HalfKPStack::from_network(match &network {
            NNUENetwork::HalfKP(net) => net,
            _ => unreachable!(),
        });

        network.refresh_accumulator_halfkp(&pos, &mut stack);
        let value = network.evaluate_halfkp(&pos, &stack);

        eprintln!("HalfKP 256 evaluate: {}", value.raw());

        // 評価値が妥当な範囲内
        assert!(value.raw().abs() < 10000, "Evaluation {} is out of expected range", value.raw());
    }
}
