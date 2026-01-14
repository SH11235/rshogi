//! NNUEネットワーク全体の構造と評価関数
//!
//! 以下のアーキテクチャをサポート:
//! - **HalfKP**: classic NNUE（水匠/tanuki互換）
//! - **HalfKA_hm^**: nnue-pytorch互換（Half-Mirror + Factorization）
//!
//! 評価値計算フロー:
//! - `FeatureTransformer` で特徴量を 512 次元に変換
//! - `AffineTransform` + `ClippedReLU` を 2 層適用して 32→32 と圧縮
//! - 出力層（32→1）で整数スコアを得て `FV_SCALE` でスケーリングし `Value` に変換
//! - グローバルな `NETWORK` にロードし、`evaluate` から利用する

use super::accumulator::{Accumulator, AccumulatorStack, Aligned};
use super::accumulator_nnue_pytorch::AccumulatorStackNnuePytorch;
use super::constants::{
    FV_SCALE, FV_SCALE_HALFKA, HIDDEN1_DIMENSIONS, HIDDEN2_DIMENSIONS, MAX_ARCH_LEN, NNUE_VERSION,
    NNUE_VERSION_HALFKA, OUTPUT_DIMENSIONS, TRANSFORMED_FEATURE_DIMENSIONS,
};
use super::feature_transformer::FeatureTransformer;
use super::feature_transformer_halfka::FeatureTransformerHalfKA;
use super::layers::{AffineTransform, ClippedReLU};
use super::network_halfka_dynamic::NetworkHalfKADynamic;
use super::network_layer_stacks::NetworkLayerStacks;
#[cfg(not(feature = "tournament"))]
use crate::eval::material;
use crate::position::Position;
use crate::types::Value;
use std::fs::File;
use std::io::{self, BufReader, Cursor, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::OnceLock;

/// グローバルなNNUEネットワーク（HalfKPまたはHalfKA_hm^）
static NETWORK: OnceLock<NNUENetwork> = OnceLock::new();

// =============================================================================
// NNUENetwork - アーキテクチャを抽象化するenum
// =============================================================================

/// NNUEネットワーク（HalfKPまたはHalfKA_hm^をラップ）
pub enum NNUENetwork {
    /// HalfKP classic NNUE
    HalfKP(Box<Network>),
    /// HalfKA_hm^ nnue-pytorch互換（256次元FC層）
    HalfKA(Box<NetworkHalfKA>),
    /// HalfKA_hm^ 動的サイズ（1024x2-8-96 など）
    HalfKADynamic(Box<NetworkHalfKADynamic>),
    /// LayerStacks（1536次元 + 9バケット）
    LayerStacks(Box<NetworkLayerStacks>),
}

impl NNUENetwork {
    /// ファイルから読み込み（バージョン自動判別）
    pub fn load<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);
        Self::read(&mut reader)
    }

    /// リーダーから読み込み（バージョン自動判別）
    pub fn read<R: Read + Seek>(reader: &mut R) -> io::Result<Self> {
        // バージョンを読み取り
        let mut buf4 = [0u8; 4];
        reader.read_exact(&mut buf4)?;
        let version = u32::from_le_bytes(buf4);

        // nnue-pytorch は NNUE_VERSION (0x7AF32F16) を使用するが、
        // アーキテクチャ文字列が "HalfKA" を含む場合は HalfKA_hm^ として扱う。
        // NNUE_VERSION_HALFKA (0x7AF32F20) も HalfKA_hm^ として扱う。
        match version {
            NNUE_VERSION | NNUE_VERSION_HALFKA => {
                // アーキテクチャ文字列を先に読んで判別する

                // ハッシュを読み飛ばし
                reader.read_exact(&mut buf4)?;

                // アーキテクチャ文字列長を読み取り
                reader.read_exact(&mut buf4)?;
                let arch_len = u32::from_le_bytes(buf4) as usize;
                if arch_len == 0 || arch_len > MAX_ARCH_LEN {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("Invalid arch string length: {arch_len}"),
                    ));
                }

                // アーキテクチャ文字列を読み取り
                let mut arch = vec![0u8; arch_len];
                reader.read_exact(&mut arch)?;
                let arch_str = String::from_utf8_lossy(&arch);

                // 位置を戻して全体を読み込み
                reader.seek(SeekFrom::Start(0))?;

                // アーキテクチャを判別
                // HalfKA_hm 系の判定（アーキテクチャ文字列に "HalfKA" を含む）
                if arch_str.contains("HalfKA") {
                    // HalfKA_hm^ には複数のアーキテクチャがある:
                    // - LayerStacks (1536次元 + 9バケット)
                    // - HalfKADynamic (動的サイズ: 1024x2-8-96 など)
                    // - HalfKA (256次元 FC層)
                    if arch_str.contains("->1536x2]") || arch_str.contains("LayerStacks") {
                        // LayerStacks (1536次元)
                        let network = NetworkLayerStacks::read(reader)?;
                        Ok(Self::LayerStacks(Box::new(network)))
                    } else if Self::detect_dynamic_l1(&arch_str) {
                        // HalfKADynamic (1024, 512 など動的サイズ)
                        let network = NetworkHalfKADynamic::read(reader)?;
                        Ok(Self::HalfKADynamic(Box::new(network)))
                    } else {
                        // HalfKA (256次元) - デフォルト
                        let network = NetworkHalfKA::read(reader)?;
                        Ok(Self::HalfKA(Box::new(network)))
                    }
                } else {
                    // HalfKP (classic NNUE)
                    let network = Network::read(reader)?;
                    Ok(Self::HalfKP(Box::new(network)))
                }
            }
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Unknown NNUE version: {version:#x}. Expected {NNUE_VERSION:#x} (HalfKP) or {NNUE_VERSION_HALFKA:#x} (HalfKA_hm^)"
                ),
            )),
        }
    }

    /// アーキテクチャ文字列から動的L1サイズを検出
    ///
    /// `->1024x2]` や `->512x2]` などのパターンを検出する。
    /// `->256x2]` は HalfKA (固定256次元) として扱う。
    fn detect_dynamic_l1(arch_str: &str) -> bool {
        // "->NNNx2]" パターンを探す
        if let Some(idx) = arch_str.find("x2]") {
            let before = &arch_str[..idx];
            if let Some(arrow_idx) = before.rfind("->") {
                let num_str = &before[arrow_idx + 2..];
                if let Ok(l1) = num_str.parse::<usize>() {
                    // 256以外の場合は動的サイズ (1024, 512 など)
                    return l1 != 256;
                }
            }
        }
        false
    }

    /// バイト列から読み込み（バージョン自動判別）
    pub fn from_bytes(bytes: &[u8]) -> io::Result<Self> {
        let mut cursor = Cursor::new(bytes);
        Self::read(&mut cursor)
    }

    /// 評価値を計算（HalfKP/HalfKA用）
    ///
    /// LayerStacks/HalfKADynamic は異なるアキュムレータを使用するため、
    /// それぞれ専用のメソッドを使用してください。
    ///
    /// # Panics
    ///
    /// HalfKADynamic または LayerStacks アーキテクチャで呼び出された場合にパニックします。
    /// これらのアーキテクチャには専用のメソッドを使用してください。
    ///
    // TODO: ライブラリコードとしては Result<Value, EvaluationError> を返すべき。
    // 現在は呼び出し元が多いため、将来的に段階的に移行する。
    pub fn evaluate(&self, pos: &Position, acc: &Accumulator) -> Value {
        match self {
            Self::HalfKP(net) => net.evaluate(pos, acc),
            Self::HalfKA(net) => net.evaluate(pos, acc),
            Self::HalfKADynamic(_) => {
                unreachable!(
                    "BUG: wrong accumulator type - HalfKADynamic requires \
                     AccumulatorHalfKADynamic. Use evaluate_halfka_dynamic() instead."
                )
            }
            Self::LayerStacks(_) => {
                unreachable!(
                    "BUG: wrong accumulator type - LayerStacks requires \
                     AccumulatorNnuePytorch. Use evaluate_layer_stacks() instead."
                )
            }
        }
    }

    /// 評価値を計算（LayerStacks用）
    ///
    /// # Panics
    ///
    /// LayerStacks 以外のアーキテクチャで呼び出された場合にパニックします。
    // TODO: Result<Value, EvaluationError> を返すように変更する
    pub fn evaluate_layer_stacks(
        &self,
        pos: &Position,
        acc: &super::accumulator_nnue_pytorch::AccumulatorNnuePytorch,
    ) -> Value {
        match self {
            Self::LayerStacks(net) => net.evaluate(pos, acc),
            _ => unreachable!(
                "BUG: evaluate_layer_stacks() called on non-LayerStacks architecture: {:?}",
                self.architecture_name()
            ),
        }
    }

    /// 評価値を計算（HalfKADynamic用）
    ///
    /// # Panics
    ///
    /// HalfKADynamic 以外のアーキテクチャで呼び出された場合にパニックします。
    // TODO: Result<Value, EvaluationError> を返すように変更する
    pub fn evaluate_halfka_dynamic(
        &self,
        pos: &Position,
        acc: &super::network_halfka_dynamic::AccumulatorHalfKADynamic,
    ) -> Value {
        match self {
            Self::HalfKADynamic(net) => net.evaluate(pos, acc),
            _ => unreachable!(
                "BUG: evaluate_halfka_dynamic() called on non-HalfKADynamic architecture: {:?}",
                self.architecture_name()
            ),
        }
    }

    /// LayerStacks アーキテクチャかどうか
    pub fn is_layer_stacks(&self) -> bool {
        matches!(self, Self::LayerStacks(_))
    }

    /// HalfKADynamic アーキテクチャかどうか
    pub fn is_halfka_dynamic(&self) -> bool {
        matches!(self, Self::HalfKADynamic(_))
    }

    /// HalfKADynamic の L1 サイズを取得（他のアーキテクチャでは None）
    pub fn get_halfka_dynamic_l1(&self) -> Option<usize> {
        match self {
            Self::HalfKADynamic(net) => Some(net.arch_l1),
            _ => None,
        }
    }

    /// アーキテクチャ名を取得
    pub fn architecture_name(&self) -> &'static str {
        match self {
            Self::HalfKP(_) => "HalfKP",
            Self::HalfKA(_) => "HalfKA_hm^",
            Self::HalfKADynamic(_) => "HalfKADynamic",
            Self::LayerStacks(_) => "LayerStacks",
        }
    }

    /// 差分計算を使わずにAccumulatorを計算（HalfKP/HalfKA用）
    pub fn refresh_accumulator(&self, pos: &Position, acc: &mut Accumulator) {
        match self {
            Self::HalfKP(net) => net.feature_transformer.refresh_accumulator(pos, acc),
            Self::HalfKA(net) => net.feature_transformer.refresh_accumulator(pos, acc),
            Self::HalfKADynamic(_) => {
                panic!(
                    "HalfKADynamic requires AccumulatorHalfKADynamic. Use refresh_accumulator_halfka_dynamic()."
                )
            }
            Self::LayerStacks(_) => {
                panic!(
                    "LayerStacks requires AccumulatorNnuePytorch. Use refresh_accumulator_layer_stacks()."
                )
            }
        }
    }

    /// 差分計算を使わずにAccumulatorを計算（LayerStacks用）
    pub fn refresh_accumulator_layer_stacks(
        &self,
        pos: &Position,
        acc: &mut super::accumulator_nnue_pytorch::AccumulatorNnuePytorch,
    ) {
        match self {
            Self::LayerStacks(net) => net.refresh_accumulator(pos, acc),
            _ => panic!("This method is only for LayerStacks architecture."),
        }
    }

    /// 差分計算を使わずにAccumulatorを計算（HalfKADynamic用）
    pub fn refresh_accumulator_halfka_dynamic(
        &self,
        pos: &Position,
        acc: &mut super::network_halfka_dynamic::AccumulatorHalfKADynamic,
    ) {
        match self {
            Self::HalfKADynamic(net) => net.refresh_accumulator(pos, acc),
            _ => panic!("This method is only for HalfKADynamic architecture."),
        }
    }

    /// 差分計算でAccumulatorを更新（HalfKP/HalfKA用）
    pub fn update_accumulator(
        &self,
        pos: &Position,
        dirty_piece: &super::accumulator::DirtyPiece,
        acc: &mut Accumulator,
        prev_acc: &Accumulator,
    ) {
        match self {
            Self::HalfKP(net) => {
                net.feature_transformer.update_accumulator(pos, dirty_piece, acc, prev_acc)
            }
            Self::HalfKA(net) => {
                net.feature_transformer.update_accumulator(pos, dirty_piece, acc, prev_acc)
            }
            Self::HalfKADynamic(_) => {
                panic!("HalfKADynamic requires AccumulatorHalfKADynamic. Use update_accumulator_halfka_dynamic().")
            }
            Self::LayerStacks(_) => {
                panic!("LayerStacks requires AccumulatorNnuePytorch. Use update_accumulator_layer_stacks().")
            }
        }
    }

    /// 差分計算でAccumulatorを更新（LayerStacks用）
    pub fn update_accumulator_layer_stacks(
        &self,
        pos: &Position,
        dirty_piece: &super::accumulator::DirtyPiece,
        acc: &mut super::accumulator_nnue_pytorch::AccumulatorNnuePytorch,
        prev_acc: &super::accumulator_nnue_pytorch::AccumulatorNnuePytorch,
    ) {
        match self {
            Self::LayerStacks(net) => net.update_accumulator(pos, dirty_piece, acc, prev_acc),
            _ => panic!("This method is only for LayerStacks architecture."),
        }
    }

    /// 差分計算でAccumulatorを更新（HalfKADynamic用）
    pub fn update_accumulator_halfka_dynamic(
        &self,
        pos: &Position,
        dirty_piece: &super::accumulator::DirtyPiece,
        acc: &mut super::network_halfka_dynamic::AccumulatorHalfKADynamic,
        prev_acc: &super::network_halfka_dynamic::AccumulatorHalfKADynamic,
    ) {
        match self {
            Self::HalfKADynamic(net) => net.update_accumulator(pos, dirty_piece, acc, prev_acc),
            _ => panic!("This method is only for HalfKADynamic architecture."),
        }
    }

    /// 複数手分の差分を適用してアキュムレータを更新（HalfKP/HalfKA用）
    pub fn forward_update_incremental(
        &self,
        pos: &Position,
        stack: &mut AccumulatorStack,
        source_idx: usize,
    ) -> bool {
        match self {
            Self::HalfKP(net) => {
                net.feature_transformer.forward_update_incremental(pos, stack, source_idx)
            }
            Self::HalfKA(net) => {
                net.feature_transformer.forward_update_incremental(pos, stack, source_idx)
            }
            Self::HalfKADynamic(_) => {
                panic!("HalfKADynamic requires AccumulatorStackHalfKADynamic. Use forward_update_incremental_halfka_dynamic().")
            }
            Self::LayerStacks(_) => {
                panic!("LayerStacks requires AccumulatorStackNnuePytorch. Use forward_update_incremental_layer_stacks().")
            }
        }
    }

    /// 複数手分の差分を適用してアキュムレータを更新（LayerStacks用）
    pub fn forward_update_incremental_layer_stacks(
        &self,
        pos: &Position,
        stack: &mut AccumulatorStackNnuePytorch,
        source_idx: usize,
    ) -> bool {
        match self {
            Self::LayerStacks(net) => net.forward_update_incremental(pos, stack, source_idx),
            _ => panic!("This method is only for LayerStacks architecture."),
        }
    }

    /// 複数手分の差分を適用してアキュムレータを更新（HalfKADynamic用）
    ///
    /// 注: 現在は未実装のため常に false を返し、全計算にフォールバックします。
    pub fn forward_update_incremental_halfka_dynamic(
        &self,
        _pos: &Position,
        _stack: &mut super::network_halfka_dynamic::AccumulatorStackHalfKADynamic,
        _source_idx: usize,
    ) -> bool {
        match self {
            Self::HalfKADynamic(_) => {
                // TODO: 複数手差分更新の実装
                false
            }
            _ => panic!("This method is only for HalfKADynamic architecture."),
        }
    }

    /// HalfKADynamic 用の新しいアキュムレータを作成
    pub fn new_accumulator_halfka_dynamic(
        &self,
    ) -> super::network_halfka_dynamic::AccumulatorHalfKADynamic {
        match self {
            Self::HalfKADynamic(net) => net.new_accumulator(),
            _ => panic!("This method is only for HalfKADynamic architecture."),
        }
    }

    /// HalfKADynamic 用の新しいアキュムレータスタックを作成
    pub fn new_accumulator_stack_halfka_dynamic(
        &self,
    ) -> super::network_halfka_dynamic::AccumulatorStackHalfKADynamic {
        match self {
            Self::HalfKADynamic(net) => net.new_accumulator_stack(),
            _ => panic!("This method is only for HalfKADynamic architecture."),
        }
    }
}

// =============================================================================
// NetworkHalfKA - HalfKA_hm^用ネットワーク
// =============================================================================

/// HalfKA_hm^用NNUEネットワーク
pub struct NetworkHalfKA {
    /// 特徴量変換器
    pub feature_transformer: FeatureTransformerHalfKA,
    /// 隠れ層1: 512 → 32
    pub hidden1: AffineTransform<{ TRANSFORMED_FEATURE_DIMENSIONS * 2 }, HIDDEN1_DIMENSIONS>,
    /// 隠れ層2: 32 → 32
    pub hidden2: AffineTransform<HIDDEN1_DIMENSIONS, HIDDEN2_DIMENSIONS>,
    /// 出力層: 32 → 1
    pub output: AffineTransform<HIDDEN2_DIMENSIONS, OUTPUT_DIMENSIONS>,
}

impl NetworkHalfKA {
    /// ファイルから読み込み
    pub fn load<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);
        Self::read(&mut reader)
    }

    /// リーダーから読み込み
    pub fn read<R: Read>(reader: &mut R) -> io::Result<Self> {
        // ヘッダを読み込み
        let mut buf4 = [0u8; 4];
        reader.read_exact(&mut buf4)?;
        let version = u32::from_le_bytes(buf4);

        if version != NNUE_VERSION_HALFKA {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Invalid NNUE version for HalfKA_hm^: {version:#x}, expected {NNUE_VERSION_HALFKA:#x}"
                ),
            ));
        }

        // 構造ハッシュを読み込み
        reader.read_exact(&mut buf4)?;
        let _hash = u32::from_le_bytes(buf4);

        // アーキテクチャ文字列を読み込み
        reader.read_exact(&mut buf4)?;
        let arch_len = u32::from_le_bytes(buf4) as usize;
        if arch_len == 0 || arch_len > MAX_ARCH_LEN {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Invalid arch string length: {arch_len} (max: {MAX_ARCH_LEN})"),
            ));
        }
        let mut arch = vec![0u8; arch_len];
        reader.read_exact(&mut arch)?;

        // 圧縮形式とFactorization状態を判定（アーキテクチャ文字列から）
        let arch_str = String::from_utf8_lossy(&arch);
        let is_leb128 = arch_str.contains("leb128");

        // Factorizedモデル（未coalesce）の検出
        // nnue-pytorchのFactorizerは訓練時のみ使用され、
        // serialize.pyで自動的にcoalesceされる。
        // "Factorizer"が含まれる場合は訓練中のcheckpointの可能性がある。
        if arch_str.contains("Factorizer") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Unsupported model format: factorized (non-coalesced) HalfKA_hm^ model detected.\n\
                     This engine only supports coalesced models (73,305 dimensions).\n\
                     Factorized models (74,934 dimensions) are for training only.\n\n\
                     To fix: Re-export the model using nnue-pytorch serialize.py:\n\
                       python serialize.py model.ckpt output.nnue\n\n\
                     The serialize.py script automatically coalesces factor weights.\n\
                     Architecture string: {arch_str}"
                ),
            ));
        }

        // パラメータを読み込み
        let feature_transformer = if is_leb128 {
            FeatureTransformerHalfKA::read_leb128(reader)?
        } else {
            FeatureTransformerHalfKA::read(reader)?
        };

        let hidden1 = if is_leb128 {
            AffineTransform::read_leb128(reader)?
        } else {
            AffineTransform::read(reader)?
        };

        let hidden2 = if is_leb128 {
            AffineTransform::read_leb128(reader)?
        } else {
            AffineTransform::read(reader)?
        };

        let output = if is_leb128 {
            AffineTransform::read_leb128(reader)?
        } else {
            AffineTransform::read(reader)?
        };

        // ファイル残余データの検証（factorizedモデル検出）
        // coalesce済みモデルでは、全層読み込み後にデータが残らない。
        // factorizedモデル（74,934次元）を誤って読んだ場合、
        // FTが73,305次元分しか読まないため、余りデータが発生する。
        // 余り = (74,934 - 73,305) × 256 × 2 = 834,048 bytes
        let mut probe = [0u8; 1];
        match reader.read(&mut probe) {
            Ok(0) => {
                // EOF到達 - 正常（coalesce済みモデル）
            }
            Ok(_) => {
                // 余りデータあり - おそらくfactorizedモデル
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "NNUE file has unexpected trailing data.\n\
                     This likely indicates a factorized (non-coalesced) model.\n\
                     This engine only supports coalesced models (73,305 dimensions).\n\n\
                     To fix: Re-export the model using nnue-pytorch serialize.py:\n\
                       python serialize.py model.ckpt output.nnue\n\n\
                     The serialize.py script automatically coalesces factor weights.",
                ));
            }
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                // EOF - 正常
            }
            Err(e) => {
                // その他のIOエラー
                return Err(e);
            }
        }

        Ok(Self {
            feature_transformer,
            hidden1,
            hidden2,
            output,
        })
    }

    /// 評価値を計算
    pub fn evaluate(&self, pos: &Position, acc: &Accumulator) -> Value {
        let mut transformed = Aligned([0u8; TRANSFORMED_FEATURE_DIMENSIONS * 2]);
        self.feature_transformer.transform(acc, pos.side_to_move(), &mut transformed.0);

        let mut hidden1_out = Aligned([0i32; HIDDEN1_DIMENSIONS]);
        self.hidden1.propagate(&transformed.0, &mut hidden1_out.0);

        let mut hidden1_relu = Aligned([0u8; HIDDEN1_DIMENSIONS]);
        ClippedReLU::propagate(&hidden1_out.0, &mut hidden1_relu.0);

        let mut hidden2_out = Aligned([0i32; HIDDEN2_DIMENSIONS]);
        self.hidden2.propagate(&hidden1_relu.0, &mut hidden2_out.0);

        let mut hidden2_relu = Aligned([0u8; HIDDEN2_DIMENSIONS]);
        ClippedReLU::propagate(&hidden2_out.0, &mut hidden2_relu.0);

        let mut output = Aligned([0i32; OUTPUT_DIMENSIONS]);
        self.output.propagate(&hidden2_relu.0, &mut output.0);

        Value::new(output.0[0] / FV_SCALE)
    }
}

// =============================================================================
// Network - HalfKP用ネットワーク（既存）
// =============================================================================

/// NNUEネットワーク全体
pub struct Network {
    /// 特徴量変換器
    pub feature_transformer: FeatureTransformer,
    /// 隠れ層1: 512 → 32
    pub hidden1: AffineTransform<{ TRANSFORMED_FEATURE_DIMENSIONS * 2 }, HIDDEN1_DIMENSIONS>,
    /// 隠れ層2: 32 → 32
    pub hidden2: AffineTransform<HIDDEN1_DIMENSIONS, HIDDEN2_DIMENSIONS>,
    /// 出力層: 32 → 1
    pub output: AffineTransform<HIDDEN2_DIMENSIONS, OUTPUT_DIMENSIONS>,
}

impl Network {
    /// ファイルから読み込み
    pub fn load<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let mut file = File::open(path)?;
        Self::read(&mut file)
    }

    /// リーダーから読み込み
    pub fn read<R: Read>(reader: &mut R) -> io::Result<Self> {
        // ヘッダを読み込み
        let mut buf4 = [0u8; 4];
        reader.read_exact(&mut buf4)?;
        let version = u32::from_le_bytes(buf4);

        if version != NNUE_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Invalid NNUE version for HalfKP: {version:#x}, expected {NNUE_VERSION:#x}"
                ),
            ));
        }

        // 構造ハッシュを読み込み（検証はスキップ）
        reader.read_exact(&mut buf4)?;
        let _hash = u32::from_le_bytes(buf4);

        // アーキテクチャ文字列を読み込み
        reader.read_exact(&mut buf4)?;
        let arch_len = u32::from_le_bytes(buf4) as usize;
        if arch_len == 0 || arch_len > MAX_ARCH_LEN {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Invalid arch string length: {arch_len} (max: {MAX_ARCH_LEN})"),
            ));
        }
        let mut arch = vec![0u8; arch_len];
        reader.read_exact(&mut arch)?;

        // FeatureTransformerのレイヤーハッシュを読み飛ばす
        // (YaneuraOu/Stockfishフォーマットでは各レイヤーの前に4バイトのハッシュがある)
        reader.read_exact(&mut buf4)?;
        let _ft_hash = u32::from_le_bytes(buf4);

        // パラメータを読み込み
        let feature_transformer = FeatureTransformer::read(reader)?;

        // Networkのレイヤーハッシュを読み飛ばす
        reader.read_exact(&mut buf4)?;
        let _network_hash = u32::from_le_bytes(buf4);

        let hidden1 = AffineTransform::read(reader)?;
        let hidden2 = AffineTransform::read(reader)?;
        let output = AffineTransform::read(reader)?;

        Ok(Self {
            feature_transformer,
            hidden1,
            hidden2,
            output,
        })
    }

    /// 評価値を計算
    pub fn evaluate(&self, pos: &Position, acc: &Accumulator) -> Value {
        // 変換済み特徴量（64バイトアラインで SIMD アラインロードを有効化）
        let mut transformed = Aligned([0u8; TRANSFORMED_FEATURE_DIMENSIONS * 2]);
        self.feature_transformer.transform(acc, pos.side_to_move(), &mut transformed.0);

        // 入力密度の計測（diagnosticsフィーチャー有効時のみ）
        //
        // 計測結果（2025-12-18）:
        //   - hidden1層への入力密度: 約39-42%（安定して~40%）
        //   - サンプル数: 16,900,000+ evaluations
        //   - 結論: 密度40%はスパース最適化には高すぎる。密な行列積方式が正しい選択。
        //
        // 計測コマンド:
        //   RUSTFLAGS="-C target-cpu=native" cargo build -p tools --bin benchmark --release --features engine-core/diagnostics
        //   ./target/release/benchmark --internal --threads 1 --limit-type movetime --limit 10000 --nnue-file path/to/nn.bin
        #[cfg(feature = "diagnostics")]
        {
            use std::sync::atomic::{AtomicU64, Ordering};
            static CALL_COUNT: AtomicU64 = AtomicU64::new(0);
            static TOTAL_NONZERO: AtomicU64 = AtomicU64::new(0);
            static TOTAL_ELEMENTS: AtomicU64 = AtomicU64::new(0);

            let nonzero = transformed.0.iter().filter(|&&x| x != 0).count() as u64;
            let elements = transformed.0.len() as u64;

            TOTAL_NONZERO.fetch_add(nonzero, Ordering::Relaxed);
            TOTAL_ELEMENTS.fetch_add(elements, Ordering::Relaxed);
            let count = CALL_COUNT.fetch_add(1, Ordering::Relaxed) + 1;

            // 100000回ごとにログ出力
            if count.is_multiple_of(100000) {
                let total_nz = TOTAL_NONZERO.load(Ordering::Relaxed);
                let total_el = TOTAL_ELEMENTS.load(Ordering::Relaxed);
                let density = total_nz as f64 / total_el as f64 * 100.0;
                eprintln!(
                    "[NNUE density] hidden1 input: {total_nz}/{total_el} nonzero ({density:.1}%) over {count} evals"
                );
            }
        }

        // 隠れ層1（64バイトアラインバッファ使用）
        let mut hidden1_out = Aligned([0i32; HIDDEN1_DIMENSIONS]);
        self.hidden1.propagate(&transformed.0, &mut hidden1_out.0);

        let mut hidden1_relu = Aligned([0u8; HIDDEN1_DIMENSIONS]);
        ClippedReLU::propagate(&hidden1_out.0, &mut hidden1_relu.0);

        // 隠れ層2（64バイトアラインバッファ使用）
        let mut hidden2_out = Aligned([0i32; HIDDEN2_DIMENSIONS]);
        self.hidden2.propagate(&hidden1_relu.0, &mut hidden2_out.0);

        let mut hidden2_relu = Aligned([0u8; HIDDEN2_DIMENSIONS]);
        ClippedReLU::propagate(&hidden2_out.0, &mut hidden2_relu.0);

        // 出力層（64バイトアラインバッファ使用）
        let mut output = Aligned([0i32; OUTPUT_DIMENSIONS]);
        self.output.propagate(&hidden2_relu.0, &mut output.0);

        // スケーリング（nnue-pytorch形式はFV_SCALE=16）
        Value::new(output.0[0] / FV_SCALE_HALFKA)
    }
}

/// NNUEを初期化（バージョン自動判別）
pub fn init_nnue<P: AsRef<Path>>(path: P) -> io::Result<()> {
    let network = NNUENetwork::load(path)?;
    NETWORK
        .set(network)
        .map_err(|_| io::Error::new(io::ErrorKind::AlreadyExists, "NNUE already initialized"))
}

/// バイト列からNNUEを初期化（バージョン自動判別）
pub fn init_nnue_from_bytes(bytes: &[u8]) -> io::Result<()> {
    let network = NNUENetwork::from_bytes(bytes)?;
    NETWORK
        .set(network)
        .map_err(|_| io::Error::new(io::ErrorKind::AlreadyExists, "NNUE already initialized"))
}

/// NNUEが初期化済みかどうか
pub fn is_nnue_initialized() -> bool {
    NETWORK.get().is_some()
}

// =============================================================================
// 内部ヘルパー関数（ロジック集約用）
// =============================================================================

/// HalfKP/HalfKA アキュムレータを更新して評価（内部実装）
///
/// `evaluate` と `evaluate_dispatch` から呼び出される共通ロジック。
/// network は既に取得済みで、アーキテクチャチェックも完了していることが前提。
#[inline]
fn update_and_evaluate_halfka(
    network: &NNUENetwork,
    pos: &Position,
    stack: &mut AccumulatorStack,
) -> Value {
    // アキュムレータの更新
    let current_entry = stack.current();
    if !current_entry.accumulator.computed_accumulation {
        let mut updated = false;

        // 1. 直前局面で差分更新を試行
        if let Some(prev_idx) = current_entry.previous {
            let prev_computed = stack.entry_at(prev_idx).accumulator.computed_accumulation;
            if prev_computed {
                let dirty_piece = stack.current().dirty_piece;
                let (prev_acc, current_acc) = stack.get_prev_and_current_accumulators(prev_idx);
                network.update_accumulator(pos, &dirty_piece, current_acc, prev_acc);
                updated = true;
            }
        }

        // 2. 失敗なら祖先探索 + 複数手差分更新を試行
        if !updated {
            if let Some((source_idx, _depth)) = stack.find_usable_accumulator() {
                updated = network.forward_update_incremental(pos, stack, source_idx);
            }
        }

        // 3. それでも失敗なら全計算
        if !updated {
            let acc = &mut stack.current_mut().accumulator;
            network.refresh_accumulator(pos, acc);
        }
    }

    // 評価
    let acc_ref = &stack.current().accumulator;
    network.evaluate(pos, acc_ref)
}

/// LayerStacks アキュムレータを更新して評価（内部実装）
///
/// `evaluate_layer_stacks` と `evaluate_dispatch` から呼び出される共通ロジック。
/// network は既に取得済みで、アーキテクチャチェックも完了していることが前提。
#[inline]
fn update_and_evaluate_layer_stacks(
    network: &NNUENetwork,
    pos: &Position,
    stack: &mut AccumulatorStackNnuePytorch,
) -> Value {
    // アキュムレータの更新
    let current_entry = stack.current();
    if !current_entry.accumulator.computed_accumulation {
        let mut updated = false;

        // 1. 直前局面で差分更新を試行
        if let Some(prev_idx) = current_entry.previous {
            let prev_computed = stack.entry_at(prev_idx).accumulator.computed_accumulation;
            if prev_computed {
                let dirty_piece = stack.current().dirty_piece;
                let (prev_acc, current_acc) = stack.get_prev_and_current_accumulators(prev_idx);
                network.update_accumulator_layer_stacks(pos, &dirty_piece, current_acc, prev_acc);
                updated = true;
            }
        }

        // 2. 失敗なら祖先探索 + 複数手差分更新を試行
        if !updated {
            if let Some((source_idx, _depth)) = stack.find_usable_accumulator() {
                updated = network.forward_update_incremental_layer_stacks(pos, stack, source_idx);
            }
        }

        // 3. それでも失敗なら全計算
        if !updated {
            let acc = &mut stack.current_mut().accumulator;
            network.refresh_accumulator_layer_stacks(pos, acc);
        }
    }

    // 評価
    let acc_ref = &stack.current().accumulator;
    network.evaluate_layer_stacks(pos, acc_ref)
}

/// HalfKADynamic アキュムレータを更新して評価（内部実装）
///
/// `evaluate_halfka_dynamic` と `evaluate_dispatch` から呼び出される共通ロジック。
/// network は既に取得済みで、アーキテクチャチェックも完了していることが前提。
#[inline]
fn update_and_evaluate_halfka_dynamic(
    network: &NNUENetwork,
    pos: &Position,
    stack: &mut super::network_halfka_dynamic::AccumulatorStackHalfKADynamic,
) -> Value {
    // アキュムレータの更新
    let current_entry = stack.current();
    if !current_entry.accumulator.computed_accumulation {
        let mut updated = false;

        // 1. 直前局面で差分更新を試行
        if let Some(prev_idx) = current_entry.previous {
            let prev_computed = stack.entry_at(prev_idx).accumulator.computed_accumulation;
            if prev_computed {
                let dirty_piece = stack.current().dirty_piece;
                let (prev_acc, current_acc) = stack.get_prev_and_current_accumulators(prev_idx);
                network.update_accumulator_halfka_dynamic(pos, &dirty_piece, current_acc, prev_acc);
                updated = true;
            }
        }

        // 2. forward_update_incremental は未実装なのでスキップ

        // 3. それでも失敗なら全計算
        if !updated {
            let acc = &mut stack.current_mut().accumulator;
            network.refresh_accumulator_halfka_dynamic(pos, acc);
        }
    }

    // 評価
    let acc_ref = &stack.current().accumulator;
    network.evaluate_halfka_dynamic(pos, acc_ref)
}

/// 局面を評価
///
/// AccumulatorStack を使って差分更新し、計算済みなら再利用する。
///
/// # フォールバック動作
/// - 通常ビルド: NNUEが初期化されていない場合は駒得評価にフォールバック
/// - tournamentビルド: NNUEが初期化されていない場合はパニック
///
/// # 遅延評価パターン
/// 1. 直前局面で差分更新を試行
/// 2. 失敗なら祖先探索 + 複数手差分更新を試行
/// 3. それでも失敗なら全計算
///
/// # 注意
/// LayerStacks アーキテクチャの場合は `evaluate_layer_stacks` を使用してください。
pub fn evaluate(pos: &Position, stack: &mut AccumulatorStack) -> Value {
    // tournamentビルド: NNUEが必須（フォールバックなし）
    #[cfg(feature = "tournament")]
    let network = NETWORK.get().expect(
        "NNUE network is not initialized. \
         Tournament build requires NNUE to be loaded before evaluation. \
         Call init_nnue() or init_nnue_from_bytes() first.",
    );

    // 通常ビルド: NNUEがなければMaterial評価にフォールバック
    #[cfg(not(feature = "tournament"))]
    let Some(network) = NETWORK.get() else {
        return material::evaluate_material(pos);
    };

    // LayerStacks/HalfKADynamic は別のアキュムレータ型を使用する
    if network.is_layer_stacks() {
        panic!(
            "LayerStacks architecture detected. Use evaluate_layer_stacks() with AccumulatorStackNnuePytorch."
        );
    }
    if network.is_halfka_dynamic() {
        panic!(
            "HalfKADynamic architecture detected. Use evaluate_halfka_dynamic() with AccumulatorStackHalfKADynamic."
        );
    }

    // 差分更新の成功率計測（diagnosticsフィーチャー有効時のみ）
    // 0=cached, 1=diff_success, 2=no_prev, 3=prev_not_computed, 4=update_failed,
    // 5=refresh, 6=ancestor_success
    #[cfg(feature = "diagnostics")]
    let mut diff_update_result: u8 = 0;

    // AccumulatorStack 上の Accumulator をインプレースで更新
    {
        let current_entry = stack.current();
        if !current_entry.accumulator.computed_accumulation {
            let mut updated = false;

            // 1. 直前局面で差分更新を試行
            // YaneuraOu classic と同様に、update_accumulator は視点ごとに reset を判定し、
            // 常に成功する（玉移動した視点は再構築、それ以外は差分更新）。
            if let Some(prev_idx) = current_entry.previous {
                let prev_computed = stack.entry_at(prev_idx).accumulator.computed_accumulation;
                if prev_computed {
                    // DirtyPieceをコピーして借用を解消
                    let dirty_piece = stack.current().dirty_piece;
                    // split_at_mut を使用して clone を回避
                    let (prev_acc, current_acc) = stack.get_prev_and_current_accumulators(prev_idx);
                    network.update_accumulator(pos, &dirty_piece, current_acc, prev_acc);
                    updated = true;
                    #[cfg(feature = "diagnostics")]
                    {
                        diff_update_result = 1; // diff_success
                    }
                } else {
                    #[cfg(feature = "diagnostics")]
                    {
                        diff_update_result = 3; // prev_not_computed
                    }
                }
            } else {
                #[cfg(feature = "diagnostics")]
                {
                    diff_update_result = 2; // no_prev
                }
            }

            // 2. 失敗なら祖先探索 + 複数手差分更新を試行
            if !updated {
                if let Some((source_idx, _depth)) = stack.find_usable_accumulator() {
                    updated = network.forward_update_incremental(pos, stack, source_idx);
                    #[cfg(feature = "diagnostics")]
                    if updated {
                        diff_update_result = 6; // ancestor_success
                    }
                }
            }

            // 3. それでも失敗なら全計算
            if !updated {
                let acc = &mut stack.current_mut().accumulator;
                network.refresh_accumulator(pos, acc);
            }
        }
        // else: cached (diff_update_result = 0)
    }

    // 差分更新の成功率をログ出力（diagnosticsフィーチャー有効時のみ）
    #[cfg(feature = "diagnostics")]
    {
        use std::sync::atomic::{AtomicU64, Ordering};
        static TOTAL_EVALS: AtomicU64 = AtomicU64::new(0);
        static CACHED: AtomicU64 = AtomicU64::new(0);
        static DIFF_SUCCESS: AtomicU64 = AtomicU64::new(0);
        static ANCESTOR_SUCCESS: AtomicU64 = AtomicU64::new(0);
        static NO_PREV: AtomicU64 = AtomicU64::new(0);
        static PREV_NOT_COMPUTED: AtomicU64 = AtomicU64::new(0);
        static UPDATE_FAILED: AtomicU64 = AtomicU64::new(0);

        match diff_update_result {
            0 => {
                CACHED.fetch_add(1, Ordering::Relaxed);
            }
            1 => {
                DIFF_SUCCESS.fetch_add(1, Ordering::Relaxed);
            }
            2 => {
                NO_PREV.fetch_add(1, Ordering::Relaxed);
            }
            3 => {
                PREV_NOT_COMPUTED.fetch_add(1, Ordering::Relaxed);
            }
            4 | 5 => {
                UPDATE_FAILED.fetch_add(1, Ordering::Relaxed);
            }
            6 => {
                ANCESTOR_SUCCESS.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }

        let count = TOTAL_EVALS.fetch_add(1, Ordering::Relaxed) + 1;

        // 100000回ごとにログ出力
        if count.is_multiple_of(100000) {
            let cached = CACHED.load(Ordering::Relaxed);
            let diff_ok = DIFF_SUCCESS.load(Ordering::Relaxed);
            let ancestor_ok = ANCESTOR_SUCCESS.load(Ordering::Relaxed);
            let no_prev = NO_PREV.load(Ordering::Relaxed);
            let prev_nc = PREV_NOT_COMPUTED.load(Ordering::Relaxed);
            let upd_fail = UPDATE_FAILED.load(Ordering::Relaxed);

            let need_compute = count - cached;
            let total_diff_ok = diff_ok + ancestor_ok;
            let diff_rate = if need_compute > 0 {
                total_diff_ok as f64 / need_compute as f64 * 100.0
            } else {
                0.0
            };
            // refresh = 全計算が必要だった回数 = 計算が必要な回数 - 差分更新成功回数
            let refresh_count = need_compute - total_diff_ok;
            let refresh_rate = if need_compute > 0 {
                refresh_count as f64 / need_compute as f64 * 100.0
            } else {
                0.0
            };

            eprintln!(
                    "[NNUE diff] total={count} cached={cached} | need_compute={need_compute} diff_ok={total_diff_ok}({diff_rate:.1}%) refresh={refresh_rate:.1}% | direct={diff_ok} ancestor={ancestor_ok} no_prev={no_prev} prev_nc={prev_nc} upd_fail={upd_fail}"
                );
        }
    }

    // 不変借用で評価
    let acc_ref = &stack.current().accumulator;
    network.evaluate(pos, acc_ref)
}

/// ロードされたNNUEがLayerStacksアーキテクチャかどうか
pub fn is_layer_stacks_loaded() -> bool {
    NETWORK.get().map(|n| n.is_layer_stacks()).unwrap_or(false)
}

/// ロードされたNNUEがHalfKADynamicアーキテクチャかどうか
pub fn is_halfka_dynamic_loaded() -> bool {
    NETWORK.get().map(|n| n.is_halfka_dynamic()).unwrap_or(false)
}

/// ロードされたHalfKADynamicのL1サイズを取得（未ロードまたは別アーキテクチャの場合はNone）
pub fn get_halfka_dynamic_l1() -> Option<usize> {
    NETWORK.get().and_then(|n| n.get_halfka_dynamic_l1())
}

/// 局面を評価（LayerStacks用）
///
/// AccumulatorStackNnuePytorch を使って差分更新し、計算済みなら再利用する。
///
/// # フォールバック動作
/// - 通常ビルド: NNUEが初期化されていない場合は駒得評価にフォールバック
/// - tournamentビルド: NNUEが初期化されていない場合はパニック
pub fn evaluate_layer_stacks(pos: &Position, stack: &mut AccumulatorStackNnuePytorch) -> Value {
    // tournamentビルド: NNUEが必須（フォールバックなし）
    #[cfg(feature = "tournament")]
    let network = NETWORK.get().expect(
        "NNUE network is not initialized. \
         Tournament build requires NNUE to be loaded before evaluation. \
         Call init_nnue() or init_nnue_from_bytes() first.",
    );

    // 通常ビルド: NNUEがなければMaterial評価にフォールバック
    #[cfg(not(feature = "tournament"))]
    let Some(network) = NETWORK.get() else {
        return material::evaluate_material(pos);
    };

    // LayerStacks 以外はエラー
    if !network.is_layer_stacks() {
        panic!("Non-LayerStacks architecture detected. Use evaluate() with AccumulatorStack.");
    }

    // 内部ヘルパー関数を呼び出し
    update_and_evaluate_layer_stacks(network, pos, stack)
}

/// アーキテクチャに応じて適切な評価関数を呼び出す
///
/// 一度の NETWORK.get() 呼び出しでアーキテクチャを判定し、
/// 適切な評価関数を呼び出す。レースコンディションを回避するために使用。
///
/// # フォールバック動作
/// - 通常ビルド: NNUEが初期化されていない場合は駒得評価にフォールバック
/// - tournamentビルド: NNUEが初期化されていない場合はパニック
pub fn evaluate_dispatch(
    pos: &Position,
    stack: &mut AccumulatorStack,
    stack_layer_stacks: &mut AccumulatorStackNnuePytorch,
    stack_halfka_dynamic: &mut super::network_halfka_dynamic::AccumulatorStackHalfKADynamic,
) -> Value {
    // tournamentビルド: NNUEが必須（フォールバックなし）
    #[cfg(feature = "tournament")]
    let network = NETWORK.get().expect(
        "NNUE network is not initialized. \
         Tournament build requires NNUE to be loaded before evaluation. \
         Call init_nnue() or init_nnue_from_bytes() first.",
    );

    // 通常ビルド: NNUEがなければMaterial評価にフォールバック
    #[cfg(not(feature = "tournament"))]
    let Some(network) = NETWORK.get() else {
        return material::evaluate_material(pos);
    };

    // アーキテクチャに応じて内部ヘルパー関数を呼び出し
    if network.is_layer_stacks() {
        update_and_evaluate_layer_stacks(network, pos, stack_layer_stacks)
    } else if network.is_halfka_dynamic() {
        update_and_evaluate_halfka_dynamic(network, pos, stack_halfka_dynamic)
    } else {
        update_and_evaluate_halfka(network, pos, stack)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::position::SFEN_HIRATE;

    /// NNUEが初期化されていない場合のフォールバック動作をテスト
    /// tournamentビルドではフォールバックがないため、このテストはスキップ
    #[test]
    #[cfg(not(feature = "tournament"))]
    fn test_evaluate_fallback() {
        let mut pos = Position::new();
        pos.set_sfen(SFEN_HIRATE).unwrap();
        let mut stack = AccumulatorStack::new();

        // NNUEが初期化されていない場合はフォールバック
        let value = evaluate(&pos, &mut stack);

        // フォールバック評価が動作することを確認
        assert!(value.raw().abs() < 1000);
    }

    #[test]
    fn test_accumulator_cached_after_evaluate() {
        // AccumulatorStack を使った評価キャッシュのテスト。
        // 評価後に AccumulatorStack の Accumulator が computed_accumulation = true で残り、
        // 再度 evaluate を呼んでもフラグが維持されることを確認する。

        let mut pos = Position::new();
        pos.set_sfen(SFEN_HIRATE).unwrap();
        let mut stack = AccumulatorStack::new();

        // 手動で accumulator を計算済みにする
        stack.current_mut().accumulator.computed_accumulation = true;

        // 1回目の evaluate: computed_accumulation が true のままならそのまま評価する
        let value1 = evaluate(&pos, &mut stack);
        assert!(stack.current().accumulator.computed_accumulation);

        // 2回目もフラグが維持されていることを確認
        let value2 = evaluate(&pos, &mut stack);
        assert!(stack.current().accumulator.computed_accumulation);

        // フォールバックの駒得評価は手番に依存して符号が変わる可能性があるが、
        // ここでは「計算が成功し、フラグが維持された」ことのみ検証する。
        let _ = (value1, value2);
    }

    #[test]
    fn test_debug_network_layers() {
        use crate::nnue::layers::ClippedReLU;

        let path =
            "/home/sh11235/development/shogi/packages/rust-core/memo/nnue-pytorch/eval/nn.bin";
        let network = match Network::load(path) {
            Ok(n) => n,
            Err(e) => {
                eprintln!("Skipping test: {e}");
                return;
            }
        };

        // 初期局面
        let mut pos = Position::new();
        pos.set_sfen(SFEN_HIRATE).unwrap();

        // Accumulator
        let mut acc = Accumulator::new();
        network.feature_transformer.refresh_accumulator(&pos, &mut acc);

        eprintln!("=== Network Layer Debug ===\n");

        // 1. FeatureTransformer 出力
        let mut transformed = Aligned([0u8; TRANSFORMED_FEATURE_DIMENSIONS * 2]);
        network
            .feature_transformer
            .transform(&acc, pos.side_to_move(), &mut transformed.0);

        let nonzero = transformed.0.iter().filter(|&&x| x != 0).count();
        let sum: u64 = transformed.0.iter().map(|&x| x as u64).sum();
        let min = *transformed.0.iter().min().unwrap();
        let max = *transformed.0.iter().max().unwrap();
        eprintln!("Transform output (512 u8):");
        eprintln!("  nonzero={nonzero}, sum={sum}, min={min}, max={max}");
        eprintln!("  first 10: {:?}", &transformed.0[..10]);

        // 2. hidden1 層
        eprintln!("\nHidden1 layer (512→32):");
        eprintln!("  biases: {:?}", &network.hidden1.biases);
        let bias_sum: i64 = network.hidden1.biases.iter().map(|&v| v as i64).sum();
        eprintln!("  bias sum={bias_sum}");

        // hidden1 の重みの範囲を確認
        let weight_min = *network.hidden1.weights.iter().min().unwrap();
        let weight_max = *network.hidden1.weights.iter().max().unwrap();
        eprintln!(
            "  weights: min={weight_min}, max={weight_max}, len={}",
            network.hidden1.weights.len()
        );

        let mut hidden1_out = Aligned([0i32; HIDDEN1_DIMENSIONS]);
        network.hidden1.propagate(&transformed.0, &mut hidden1_out.0);
        eprintln!("  output (i32): {:?}", &hidden1_out.0);
        let h1_sum: i64 = hidden1_out.0.iter().map(|&v| v as i64).sum();
        eprintln!("  sum={h1_sum}");

        // 3. hidden1 ReLU
        let mut hidden1_relu = Aligned([0u8; HIDDEN1_DIMENSIONS]);
        ClippedReLU::<HIDDEN1_DIMENSIONS>::propagate(&hidden1_out.0, &mut hidden1_relu.0);
        eprintln!("\nHidden1 ReLU (i32 >> 6, clamp 0-127):");
        eprintln!("  output (u8): {:?}", &hidden1_relu.0);

        // 4. hidden2 層
        eprintln!("\nHidden2 layer (32→32):");
        eprintln!("  biases: {:?}", &network.hidden2.biases);

        let mut hidden2_out = Aligned([0i32; HIDDEN2_DIMENSIONS]);
        network.hidden2.propagate(&hidden1_relu.0, &mut hidden2_out.0);
        eprintln!("  output (i32): {:?}", &hidden2_out.0);

        // 5. hidden2 ReLU
        let mut hidden2_relu = Aligned([0u8; HIDDEN2_DIMENSIONS]);
        ClippedReLU::<HIDDEN2_DIMENSIONS>::propagate(&hidden2_out.0, &mut hidden2_relu.0);
        eprintln!("\nHidden2 ReLU:");
        eprintln!("  output (u8): {:?}", &hidden2_relu.0);

        // 6. output 層
        eprintln!("\nOutput layer (32→1):");
        eprintln!("  biases: {:?}", &network.output.biases);

        let mut output_val = Aligned([0i32; OUTPUT_DIMENSIONS]);
        network.output.propagate(&hidden2_relu.0, &mut output_val.0);
        eprintln!("  raw output: {}", output_val.0[0]);
        eprintln!("  / FV_SCALE({}): {}", FV_SCALE, output_val.0[0] / FV_SCALE);

        // Network.evaluate() の結果
        let score = network.evaluate(&pos, &acc);
        eprintln!("\nNetwork.evaluate() = {}", score.raw());
    }

    /// NNUENetwork のアーキテクチャ自動検出テスト
    ///
    /// 外部NNUEファイルが必要なため通常はスキップ。
    /// 実行方法: `NNUE_TEST_FILE=/path/to/file.nnue cargo test test_nnue_network_auto_detect_layer_stacks -- --ignored`
    ///
    /// テスト結果 (epoch82.nnue):
    /// - LayerStacks として正しく認識される
    /// - 評価値: 0 (学習初期のモデル)
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

        let mut acc = crate::nnue::AccumulatorNnuePytorch::new();
        network.refresh_accumulator_layer_stacks(&pos, &mut acc);

        let value = network.evaluate_layer_stacks(&pos, &acc);
        eprintln!("LayerStacks evaluate: {}", value.raw());

        // 評価値が妥当な範囲内
        assert!(value.raw().abs() < 1000);
    }

    /// epoch20_v2.nnue (HalfKADynamic 1024x2-8-96) の自動判別テスト
    #[test]
    fn test_nnue_network_auto_detect_halfka_dynamic() {
        use std::path::Path;

        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("epoch20_v2.nnue");

        if !path.exists() {
            eprintln!("Skipping test: NNUE file not found at {path:?}");
            return;
        }

        let network = NNUENetwork::load(&path).expect("Failed to load NNUE file");

        // HalfKADynamic として認識されることを確認
        assert!(
            network.is_halfka_dynamic(),
            "epoch20_v2.nnue should be detected as HalfKADynamic, but got: {}",
            network.architecture_name()
        );
        assert_eq!(network.architecture_name(), "HalfKADynamic");

        // HalfKADynamic 用の評価が動作することを確認
        let mut pos = crate::position::Position::new();
        pos.set_sfen(SFEN_HIRATE).unwrap();

        let mut acc = network.new_accumulator_halfka_dynamic();
        network.refresh_accumulator_halfka_dynamic(&pos, &mut acc);

        let value = network.evaluate_halfka_dynamic(&pos, &acc);
        eprintln!("HalfKADynamic evaluate (hirate): {}", value.raw());

        // 評価値が妥当な範囲内（初期局面は概ね0に近いはず）
        assert!(value.raw().abs() < 500, "Hirate eval should be near 0, got: {}", value.raw());
    }
}
