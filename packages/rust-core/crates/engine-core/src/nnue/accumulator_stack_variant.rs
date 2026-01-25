//! AccumulatorStackVariant - 各アーキテクチャのスタックを統一的に扱う列挙型
//!
//! 探索時に使用するAccumulatorStackを1つだけ保持し、メモリ効率とパフォーマンスを向上させる。
//!
//! # 設計
//!
//! **「Accumulator は L1 だけで決まる」** を活用し、3バリアントに集約:
//! - HalfKA(HalfKAStack): L256/L512/L1024 を内包
//! - HalfKP(HalfKPStack): L256/L512 を内包
//! - LayerStacks: 1536次元 + 9バケット
//!
//! L2/L3/活性化の追加時にこのファイルの変更は不要。

use super::accumulator::DirtyPiece;
use super::accumulator_layer_stacks::AccumulatorStackLayerStacks;
use super::halfka::HalfKAStack;
use super::halfkp::HalfKPStack;
use super::network::NNUENetwork;

/// アキュムレータスタックのバリアント（列挙型）
///
/// NNUEアーキテクチャに応じた適切なスタックを1つだけ保持する。
/// これにより、メモリ使用量を削減し、do_move/undo_moveの効率を向上させる。
///
/// # 3バリアント構造
///
/// L1 サイズのみで分類し、L2/L3/活性化は内部で処理:
/// - **HalfKA**: L256/L512/L1024 を HalfKAStack で管理
/// - **HalfKP**: L256/L512 を HalfKPStack で管理
/// - **LayerStacks**: 1536次元 + 9バケット
pub enum AccumulatorStackVariant {
    /// HalfKA 特徴量セット（L256/L512/L1024）
    HalfKA(HalfKAStack),
    /// HalfKP 特徴量セット（L256/L512）
    HalfKP(HalfKPStack),
    /// LayerStacks（1536次元 + 9バケット）
    LayerStacks(AccumulatorStackLayerStacks),
}

impl AccumulatorStackVariant {
    /// NNUEネットワークに応じたスタックを作成
    ///
    /// 指定されたネットワークのアーキテクチャに対応するスタックバリアントを生成する。
    pub fn from_network(network: &NNUENetwork) -> Self {
        match network {
            NNUENetwork::HalfKA(net) => Self::HalfKA(HalfKAStack::from_network(net)),
            NNUENetwork::HalfKP(net) => Self::HalfKP(HalfKPStack::from_network(net)),
            NNUENetwork::LayerStacks(_) => Self::LayerStacks(AccumulatorStackLayerStacks::new()),
        }
    }

    /// デフォルトのスタック（HalfKP L256）を作成
    ///
    /// NNUEが未初期化の場合のフォールバック用。
    pub fn new_default() -> Self {
        Self::HalfKP(HalfKPStack::default())
    }

    /// 現在のバリアントがネットワークと一致するか確認
    ///
    /// 一致しない場合は `from_network` で再作成が必要。
    pub fn matches_network(&self, network: &NNUENetwork) -> bool {
        match (self, network) {
            (Self::HalfKA(stack), NNUENetwork::HalfKA(net)) => stack.l1_size() == net.l1_size(),
            (Self::HalfKP(stack), NNUENetwork::HalfKP(net)) => stack.l1_size() == net.l1_size(),
            (Self::LayerStacks(_), NNUENetwork::LayerStacks(_)) => true,
            _ => false,
        }
    }

    /// スタックをリセット（探索開始時に呼び出す）
    #[inline]
    pub fn reset(&mut self) {
        match self {
            Self::HalfKA(stack) => stack.reset(),
            Self::HalfKP(stack) => stack.reset(),
            Self::LayerStacks(stack) => stack.reset(),
        }
    }

    /// do_move時にスタックをプッシュ
    #[inline]
    pub fn push(&mut self, dirty_piece: DirtyPiece) {
        match self {
            Self::HalfKA(stack) => stack.push(dirty_piece),
            Self::HalfKP(stack) => stack.push(dirty_piece),
            Self::LayerStacks(stack) => {
                stack.push();
                stack.current_mut().dirty_piece = dirty_piece;
            }
        }
    }

    /// undo_move時にスタックをポップ
    #[inline]
    pub fn pop(&mut self) {
        match self {
            Self::HalfKA(stack) => stack.pop(),
            Self::HalfKP(stack) => stack.pop(),
            Self::LayerStacks(stack) => stack.pop(),
        }
    }

    /// 現在のバリアントがHalfKPかどうか
    #[inline]
    pub fn is_halfkp(&self) -> bool {
        matches!(self, Self::HalfKP(_))
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
        assert!(matches!(stack, AccumulatorStackVariant::HalfKP(_)));
        assert!(!matches!(stack, AccumulatorStackVariant::LayerStacks(_)));
        assert!(!matches!(stack, AccumulatorStackVariant::HalfKA(_)));
    }

    #[test]
    fn test_new_default_is_halfkp() {
        let stack = AccumulatorStackVariant::new_default();
        assert!(stack.is_halfkp());
        assert!(matches!(stack, AccumulatorStackVariant::HalfKP(_)));
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

    /// push/pop の対称性と状態の一貫性テスト
    ///
    /// 各バリアントで push/pop 後にスタックインデックスが正しいことを確認
    #[test]
    fn test_push_pop_index_consistency_halfkp() {
        let mut stack = HalfKPStack::default();
        let dirty = DirtyPiece::default();

        stack.reset();
        let initial_index = stack.current_index();

        // push でインデックスが増加
        stack.push(dirty);
        assert_eq!(stack.current_index(), initial_index + 1);

        stack.push(dirty);
        assert_eq!(stack.current_index(), initial_index + 2);

        stack.push(dirty);
        assert_eq!(stack.current_index(), initial_index + 3);

        // pop でインデックスが減少
        stack.pop();
        assert_eq!(stack.current_index(), initial_index + 2);

        stack.pop();
        assert_eq!(stack.current_index(), initial_index + 1);

        stack.pop();
        assert_eq!(stack.current_index(), initial_index);
    }

    #[test]
    fn test_push_pop_index_consistency_halfka() {
        let mut stack = HalfKAStack::default();
        let dirty = DirtyPiece::default();

        stack.reset();
        let initial_index = stack.current_index();

        // push でインデックスが増加
        stack.push(dirty);
        assert_eq!(stack.current_index(), initial_index + 1);

        stack.push(dirty);
        assert_eq!(stack.current_index(), initial_index + 2);

        // pop でインデックスが減少
        stack.pop();
        assert_eq!(stack.current_index(), initial_index + 1);

        stack.pop();
        assert_eq!(stack.current_index(), initial_index);
    }

    /// 各 L1 サイズでスタックが正しく作成されることを確認
    #[test]
    fn test_halfka_stack_l1_sizes() {
        use crate::nnue::network_halfka::AccumulatorStackHalfKA;

        let l256_stack = HalfKAStack::L256(AccumulatorStackHalfKA::<256>::new());
        let l512_stack = HalfKAStack::L512(AccumulatorStackHalfKA::<512>::new());
        let l1024_stack = HalfKAStack::L1024(AccumulatorStackHalfKA::<1024>::new());

        assert_eq!(l256_stack.l1_size(), 256);
        assert_eq!(l512_stack.l1_size(), 512);
        assert_eq!(l1024_stack.l1_size(), 1024);
    }

    #[test]
    fn test_halfkp_stack_l1_sizes() {
        use crate::nnue::network_halfkp::AccumulatorStackHalfKP;

        let l256_stack = HalfKPStack::L256(AccumulatorStackHalfKP::<256>::new());
        let l512_stack = HalfKPStack::L512(AccumulatorStackHalfKP::<512>::new());
        let l1024_stack = HalfKPStack::L1024(AccumulatorStackHalfKP::<1024>::new());

        assert_eq!(l256_stack.l1_size(), 256);
        assert_eq!(l512_stack.l1_size(), 512);
        assert_eq!(l1024_stack.l1_size(), 1024);
    }

    /// deep push/pop テスト（探索木の深さをシミュレート）
    #[test]
    fn test_deep_push_pop() {
        let mut stack = AccumulatorStackVariant::new_default();
        let dirty = DirtyPiece::default();

        stack.reset();

        // 探索木の深さをシミュレート（典型的な深さ 20-30 程度）
        const DEPTH: usize = 30;

        for _ in 0..DEPTH {
            stack.push(dirty);
        }

        for _ in 0..DEPTH {
            stack.pop();
        }

        // パニックしなければ成功
    }

    #[test]
    fn test_variant_size() {
        use std::mem::size_of;

        // 各スタックのサイズを確認（デバッグ用）
        let variant_size = size_of::<AccumulatorStackVariant>();
        let layer_stacks_size = size_of::<AccumulatorStackLayerStacks>();
        let halfka_stack_size = size_of::<HalfKAStack>();
        let halfkp_stack_size = size_of::<HalfKPStack>();

        // 新設計では最大のバリアントのサイズ + タグになる
        // 各サブスタックも enum なので効率的
        eprintln!("AccumulatorStackVariant size: {variant_size} bytes");
        eprintln!("HalfKAStack size: {halfka_stack_size} bytes");
        eprintln!("HalfKPStack size: {halfkp_stack_size} bytes");
        eprintln!("LayerStacks size: {layer_stacks_size} bytes");

        // 列挙型のサイズは最大のバリアントのサイズ + タグ
        assert!(variant_size > 0);
    }
}
