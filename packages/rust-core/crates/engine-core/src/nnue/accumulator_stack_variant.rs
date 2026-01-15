//! AccumulatorStackVariant - 各アーキテクチャのスタックを統一的に扱う列挙型
//!
//! 探索時に使用するAccumulatorStackを1つだけ保持し、メモリ効率とパフォーマンスを向上させる。

use super::accumulator::{AccumulatorStack, DirtyPiece};
use super::accumulator_layer_stacks::AccumulatorStackLayerStacks;
use super::network::NNUENetwork;
use super::network_halfka_dynamic::AccumulatorStackHalfKADynamic;
use super::network_halfka_static::{AccumulatorStackHalfKA1024, AccumulatorStackHalfKA512};

/// アキュムレータスタックのバリアント（列挙型）
///
/// NNUEアーキテクチャに応じた適切なスタックを1つだけ保持する。
/// これにより、メモリ使用量を1/5に削減し、do_move/undo_moveの効率を向上させる。
pub enum AccumulatorStackVariant {
    /// HalfKP classic NNUE
    HalfKP(AccumulatorStack),
    /// LayerStacks（1536次元 + 9バケット）
    LayerStacks(AccumulatorStackLayerStacks),
    /// HalfKA_hm^ 動的サイズ
    HalfKADynamic(AccumulatorStackHalfKADynamic),
    /// HalfKA_hm^ 512x2-8-96 静的実装
    HalfKA512(AccumulatorStackHalfKA512),
    /// HalfKA_hm^ 1024x2-8-96 静的実装
    HalfKA1024(AccumulatorStackHalfKA1024),
}

impl AccumulatorStackVariant {
    /// NNUEネットワークに応じたスタックを作成
    ///
    /// 指定されたネットワークのアーキテクチャに対応するスタックバリアントを生成する。
    pub fn from_network(network: &NNUENetwork) -> Self {
        match network {
            NNUENetwork::HalfKP(_) => Self::HalfKP(AccumulatorStack::new()),
            NNUENetwork::LayerStacks(_) => Self::LayerStacks(AccumulatorStackLayerStacks::new()),
            NNUENetwork::HalfKADynamic(_) => {
                let l1 = network.get_halfka_dynamic_l1().unwrap_or(1024);
                Self::HalfKADynamic(AccumulatorStackHalfKADynamic::new(l1))
            }
            NNUENetwork::HalfKA512(_) => Self::HalfKA512(AccumulatorStackHalfKA512::new()),
            NNUENetwork::HalfKA1024(_) => Self::HalfKA1024(AccumulatorStackHalfKA1024::new()),
        }
    }

    /// デフォルトのスタック（HalfKP）を作成
    ///
    /// NNUEが未初期化の場合のフォールバック用。
    pub fn new_default() -> Self {
        Self::HalfKP(AccumulatorStack::new())
    }

    /// 現在のバリアントがネットワークと一致するか確認
    ///
    /// 一致しない場合は `from_network` で再作成が必要。
    pub fn matches_network(&self, network: &NNUENetwork) -> bool {
        matches!(
            (self, network),
            (Self::HalfKP(_), NNUENetwork::HalfKP(_))
                | (Self::LayerStacks(_), NNUENetwork::LayerStacks(_))
                | (Self::HalfKA512(_), NNUENetwork::HalfKA512(_))
                | (Self::HalfKA1024(_), NNUENetwork::HalfKA1024(_))
        ) || self.matches_halfka_dynamic(network)
    }

    /// HalfKADynamic の L1 サイズも含めた一致チェック
    fn matches_halfka_dynamic(&self, network: &NNUENetwork) -> bool {
        match self {
            Self::HalfKADynamic(stack) => {
                network.get_halfka_dynamic_l1().map(|l1| stack.l1() == l1).unwrap_or(false)
            }
            _ => false,
        }
    }

    /// スタックをリセット（探索開始時に呼び出す）
    #[inline]
    pub fn reset(&mut self) {
        match self {
            Self::HalfKP(stack) => stack.reset(),
            Self::LayerStacks(stack) => stack.reset(),
            Self::HalfKADynamic(stack) => stack.reset(),
            Self::HalfKA512(stack) => stack.reset(),
            Self::HalfKA1024(stack) => stack.reset(),
        }
    }

    /// do_move時にスタックをプッシュ
    #[inline]
    pub fn push(&mut self, dirty_piece: DirtyPiece) {
        match self {
            Self::HalfKP(stack) => stack.push(dirty_piece),
            Self::LayerStacks(stack) => {
                stack.push();
                stack.current_mut().dirty_piece = dirty_piece;
            }
            Self::HalfKADynamic(stack) => stack.push(dirty_piece),
            Self::HalfKA512(stack) => stack.push(dirty_piece),
            Self::HalfKA1024(stack) => stack.push(dirty_piece),
        }
    }

    /// undo_move時にスタックをポップ
    #[inline]
    pub fn pop(&mut self) {
        match self {
            Self::HalfKP(stack) => stack.pop(),
            Self::LayerStacks(stack) => stack.pop(),
            Self::HalfKADynamic(stack) => stack.pop(),
            Self::HalfKA512(stack) => stack.pop(),
            Self::HalfKA1024(stack) => stack.pop(),
        }
    }

    /// 内部のHalfKPスタックへの参照を取得
    #[inline]
    pub fn as_halfkp(&self) -> Option<&AccumulatorStack> {
        match self {
            Self::HalfKP(stack) => Some(stack),
            _ => None,
        }
    }

    /// 内部のHalfKPスタックへの可変参照を取得
    #[inline]
    pub fn as_halfkp_mut(&mut self) -> Option<&mut AccumulatorStack> {
        match self {
            Self::HalfKP(stack) => Some(stack),
            _ => None,
        }
    }

    /// 内部のLayerStacksスタックへの参照を取得
    #[inline]
    pub fn as_layer_stacks(&self) -> Option<&AccumulatorStackLayerStacks> {
        match self {
            Self::LayerStacks(stack) => Some(stack),
            _ => None,
        }
    }

    /// 内部のLayerStacksスタックへの可変参照を取得
    #[inline]
    pub fn as_layer_stacks_mut(&mut self) -> Option<&mut AccumulatorStackLayerStacks> {
        match self {
            Self::LayerStacks(stack) => Some(stack),
            _ => None,
        }
    }

    /// 内部のHalfKADynamicスタックへの参照を取得
    #[inline]
    pub fn as_halfka_dynamic(&self) -> Option<&AccumulatorStackHalfKADynamic> {
        match self {
            Self::HalfKADynamic(stack) => Some(stack),
            _ => None,
        }
    }

    /// 内部のHalfKADynamicスタックへの可変参照を取得
    #[inline]
    pub fn as_halfka_dynamic_mut(&mut self) -> Option<&mut AccumulatorStackHalfKADynamic> {
        match self {
            Self::HalfKADynamic(stack) => Some(stack),
            _ => None,
        }
    }

    /// 内部のHalfKA512スタックへの参照を取得
    #[inline]
    pub fn as_halfka_512(&self) -> Option<&AccumulatorStackHalfKA512> {
        match self {
            Self::HalfKA512(stack) => Some(stack),
            _ => None,
        }
    }

    /// 内部のHalfKA512スタックへの可変参照を取得
    #[inline]
    pub fn as_halfka_512_mut(&mut self) -> Option<&mut AccumulatorStackHalfKA512> {
        match self {
            Self::HalfKA512(stack) => Some(stack),
            _ => None,
        }
    }

    /// 内部のHalfKA1024スタックへの参照を取得
    #[inline]
    pub fn as_halfka_1024(&self) -> Option<&AccumulatorStackHalfKA1024> {
        match self {
            Self::HalfKA1024(stack) => Some(stack),
            _ => None,
        }
    }

    /// 内部のHalfKA1024スタックへの可変参照を取得
    #[inline]
    pub fn as_halfka_1024_mut(&mut self) -> Option<&mut AccumulatorStackHalfKA1024> {
        match self {
            Self::HalfKA1024(stack) => Some(stack),
            _ => None,
        }
    }
}

impl Default for AccumulatorStackVariant {
    fn default() -> Self {
        Self::new_default()
    }
}
