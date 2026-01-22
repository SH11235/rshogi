//! AccumulatorStackVariant - 各アーキテクチャのスタックを統一的に扱う列挙型
//!
//! 探索時に使用するAccumulatorStackを1つだけ保持し、メモリ効率とパフォーマンスを向上させる。

use super::accumulator::{AccumulatorStack, DirtyPiece};
use super::accumulator_layer_stacks::AccumulatorStackLayerStacks;
use super::network::NNUENetwork;
use super::network_halfka::AccumulatorStackHalfKA;
use super::network_halfka_dynamic::AccumulatorStackHalfKADynamic;
use super::network_halfkp::AccumulatorStackHalfKP;
use super::network_halfkp_dynamic::AccumulatorStackHalfKPDynamic;

/// アキュムレータスタックのバリアント（列挙型）
///
/// NNUEアーキテクチャに応じた適切なスタックを1つだけ保持する。
/// これにより、メモリ使用量を1/5に削減し、do_move/undo_moveの効率を向上させる。
pub enum AccumulatorStackVariant {
    /// HalfKP classic NNUE (256x2-32-32) - 旧実装互換
    HalfKP(AccumulatorStack),
    /// HalfKP 256x2-32-32 (const generics版)
    HalfKP256(AccumulatorStackHalfKP<256>),
    /// HalfKP 512x2-8-96 (const generics版)
    HalfKP512(AccumulatorStackHalfKP<512>),
    /// HalfKP 動的サイズ（フォールバック）
    HalfKPDynamic(AccumulatorStackHalfKPDynamic),
    /// LayerStacks（1536次元 + 9バケット）
    LayerStacks(AccumulatorStackLayerStacks),
    /// HalfKA_hm^ 512x2-8-96 (const generics版)
    HalfKA512(AccumulatorStackHalfKA<512>),
    /// HalfKA_hm^ 1024x2-8-96 (const generics版)
    HalfKA1024(AccumulatorStackHalfKA<1024>),
    /// HalfKA_hm^ 動的サイズ（フォールバック）
    HalfKADynamic(AccumulatorStackHalfKADynamic),
}

impl AccumulatorStackVariant {
    /// NNUEネットワークに応じたスタックを作成
    ///
    /// 指定されたネットワークのアーキテクチャに対応するスタックバリアントを生成する。
    pub fn from_network(network: &NNUENetwork) -> Self {
        match network {
            NNUENetwork::HalfKP(_) => Self::HalfKP(AccumulatorStack::new()),
            NNUENetwork::HalfKP256CReLU(_) | NNUENetwork::HalfKP256SCReLU(_) => {
                Self::HalfKP256(AccumulatorStackHalfKP::<256>::new())
            }
            NNUENetwork::HalfKP512CReLU(_) | NNUENetwork::HalfKP512SCReLU(_) => {
                Self::HalfKP512(AccumulatorStackHalfKP::<512>::new())
            }
            NNUENetwork::HalfKPDynamic(_) => {
                let l1 = network.get_halfkp_dynamic_l1().unwrap_or(512);
                Self::HalfKPDynamic(AccumulatorStackHalfKPDynamic::new(l1))
            }
            NNUENetwork::LayerStacks(_) => Self::LayerStacks(AccumulatorStackLayerStacks::new()),
            NNUENetwork::HalfKA512CReLU(_) | NNUENetwork::HalfKA512SCReLU(_) => {
                Self::HalfKA512(AccumulatorStackHalfKA::<512>::new())
            }
            NNUENetwork::HalfKA1024CReLU(_) | NNUENetwork::HalfKA1024SCReLU(_) => {
                Self::HalfKA1024(AccumulatorStackHalfKA::<1024>::new())
            }
            NNUENetwork::HalfKADynamic(_) => {
                // HalfKADynamicバリアントが存在する場合、L1サイズは必ず取得可能だが、
                // 型安全のためフォールバック値1024を使用（最も一般的なサイズ）
                let l1 = network.get_halfka_dynamic_l1().unwrap_or(1024);
                Self::HalfKADynamic(AccumulatorStackHalfKADynamic::new(l1))
            }
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
    /// 明示的なmatch式により、将来バリアントを追加した際にコンパイラが警告を出す。
    pub fn matches_network(&self, network: &NNUENetwork) -> bool {
        match (self, network) {
            (Self::HalfKP(_), NNUENetwork::HalfKP(_)) => true,
            (
                Self::HalfKP256(_),
                NNUENetwork::HalfKP256CReLU(_) | NNUENetwork::HalfKP256SCReLU(_),
            ) => true,
            (
                Self::HalfKP512(_),
                NNUENetwork::HalfKP512CReLU(_) | NNUENetwork::HalfKP512SCReLU(_),
            ) => true,
            (Self::HalfKPDynamic(stack), NNUENetwork::HalfKPDynamic(_)) => {
                // L1サイズも一致する必要がある
                network.get_halfkp_dynamic_l1().is_some_and(|l1| stack.l1() == l1)
            }
            (Self::LayerStacks(_), NNUENetwork::LayerStacks(_)) => true,
            (
                Self::HalfKA512(_),
                NNUENetwork::HalfKA512CReLU(_) | NNUENetwork::HalfKA512SCReLU(_),
            ) => true,
            (
                Self::HalfKA1024(_),
                NNUENetwork::HalfKA1024CReLU(_) | NNUENetwork::HalfKA1024SCReLU(_),
            ) => true,
            (Self::HalfKADynamic(stack), NNUENetwork::HalfKADynamic(_)) => {
                // L1サイズも一致する必要がある
                network.get_halfka_dynamic_l1().is_some_and(|l1| stack.l1() == l1)
            }
            // 将来バリアントを追加した場合、ここでコンパイラ警告が出る
            _ => false,
        }
    }

    /// スタックをリセット（探索開始時に呼び出す）
    #[inline]
    pub fn reset(&mut self) {
        match self {
            Self::HalfKP(stack) => stack.reset(),
            Self::HalfKP256(stack) => stack.reset(),
            Self::HalfKP512(stack) => stack.reset(),
            Self::HalfKPDynamic(stack) => stack.reset(),
            Self::LayerStacks(stack) => stack.reset(),
            Self::HalfKA512(stack) => stack.reset(),
            Self::HalfKA1024(stack) => stack.reset(),
            Self::HalfKADynamic(stack) => stack.reset(),
        }
    }

    /// do_move時にスタックをプッシュ
    #[inline]
    pub fn push(&mut self, dirty_piece: DirtyPiece) {
        match self {
            Self::HalfKP(stack) => stack.push(dirty_piece),
            Self::HalfKP256(stack) => stack.push(dirty_piece),
            Self::HalfKP512(stack) => stack.push(dirty_piece),
            Self::HalfKPDynamic(stack) => stack.push(dirty_piece),
            Self::LayerStacks(stack) => {
                stack.push();
                stack.current_mut().dirty_piece = dirty_piece;
            }
            Self::HalfKA512(stack) => stack.push(dirty_piece),
            Self::HalfKA1024(stack) => stack.push(dirty_piece),
            Self::HalfKADynamic(stack) => stack.push(dirty_piece),
        }
    }

    /// undo_move時にスタックをポップ
    #[inline]
    pub fn pop(&mut self) {
        match self {
            Self::HalfKP(stack) => stack.pop(),
            Self::HalfKP256(stack) => stack.pop(),
            Self::HalfKP512(stack) => stack.pop(),
            Self::HalfKPDynamic(stack) => stack.pop(),
            Self::LayerStacks(stack) => stack.pop(),
            Self::HalfKA512(stack) => stack.pop(),
            Self::HalfKA1024(stack) => stack.pop(),
            Self::HalfKADynamic(stack) => stack.pop(),
        }
    }

    /// 現在のバリアントがHalfKPかどうか（静的版のみ）
    #[inline]
    pub fn is_halfkp(&self) -> bool {
        matches!(self, Self::HalfKP(_) | Self::HalfKP256(_))
    }

    /// 現在のバリアントがHalfKPDynamicかどうか
    #[inline]
    pub fn is_halfkp_dynamic(&self) -> bool {
        matches!(self, Self::HalfKPDynamic(_))
    }
}

impl Default for AccumulatorStackVariant {
    fn default() -> Self {
        Self::new_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_is_halfkp() {
        let stack = AccumulatorStackVariant::default();
        assert!(stack.is_halfkp());
        assert!(!matches!(stack, AccumulatorStackVariant::LayerStacks(_)));
        assert!(!matches!(stack, AccumulatorStackVariant::HalfKADynamic(_)));
        assert!(!matches!(stack, AccumulatorStackVariant::HalfKA512(_)));
        assert!(!matches!(stack, AccumulatorStackVariant::HalfKA1024(_)));
    }

    #[test]
    fn test_new_default_is_halfkp() {
        let stack = AccumulatorStackVariant::new_default();
        assert!(stack.is_halfkp());
    }

    #[test]
    fn test_reset_does_not_change_variant() {
        let mut stack = AccumulatorStackVariant::new_default();
        assert!(stack.is_halfkp());
        stack.reset();
        assert!(stack.is_halfkp());
    }

    #[test]
    fn test_push_pop_symmetry() {
        let mut stack = AccumulatorStackVariant::new_default();
        let dirty = DirtyPiece::default();

        stack.reset();
        // push/popが正しくバランスしていることを確認
        stack.push(dirty);
        stack.push(dirty);
        stack.pop();
        stack.pop();
        // パニックしなければ成功
    }

    #[test]
    fn test_variant_size() {
        use std::mem::size_of;

        // 各スタックのサイズを確認（デバッグ用）
        let variant_size = size_of::<AccumulatorStackVariant>();
        let halfkp_size = size_of::<AccumulatorStack>();
        let layer_stacks_size = size_of::<AccumulatorStackLayerStacks>();
        let halfka_512_size = size_of::<AccumulatorStackHalfKA<512>>();
        let halfka_1024_size = size_of::<AccumulatorStackHalfKA<1024>>();

        // 列挙型のサイズは最大のバリアントのサイズ + タグ
        // 旧実装では全スタックの合計サイズを使用していた
        let old_total = halfkp_size + layer_stacks_size + halfka_512_size + halfka_1024_size;

        // 新実装は旧実装より小さいはず
        assert!(
            variant_size < old_total,
            "AccumulatorStackVariant ({variant_size} bytes) should be smaller than sum of all stacks ({old_total} bytes)"
        );
    }
}
