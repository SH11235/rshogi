//! AccumulatorStackVariant - 各アーキテクチャのスタックを統一的に扱う列挙型
//!
//! 探索時に使用するAccumulatorStackを1つだけ保持し、メモリ効率とパフォーマンスを向上させる。
//!
//! # 設計
//!
//! 固定 edition の const-generic stack と、universal edition の runtime-dimension
//! stack を同じ探索 API で扱う。

#[cfg(feature = "nnue-runtime-dimensions")]
use std::cell::RefCell;

use super::accumulator::DirtyPiece;
#[cfg(feature = "layerstack-arch")]
use super::accumulator_layer_stacks::LayerStacksAccStack;
#[cfg(feature = "nnue-runtime-dimensions")]
use super::dynamic_halfkx::DynamicHalfKxStack;
#[cfg(feature = "nnue-runtime-dimensions")]
use super::dynamic_layer_stacks::DynamicLayerStacksStack;
use super::halfka_hm_merged::HalfKaHmMergedStack;
use super::halfka_hm_split::HalfKaHmSplitStack;
use super::halfka_merged::HalfKaMergedStack;
use super::halfka_split::HalfKaSplitStack;
use super::halfkp::HalfKPStack;
use super::network::NNUENetwork;

/// アキュムレータスタックのバリアント（列挙型）
///
/// NNUEアーキテクチャに応じた適切なスタックを1つだけ保持する。
/// これにより、メモリ使用量を削減し、do_move/undo_moveの効率を向上させる。
///
#[non_exhaustive]
pub enum AccumulatorStackVariant {
    /// Runtime-dimension HalfKX（universal edition 専用）
    #[cfg(feature = "nnue-runtime-dimensions")]
    DynamicHalfKx(Box<RefCell<DynamicHalfKxStack>>),
    /// Runtime-dimension LayerStacks（universal edition 専用）
    #[cfg(feature = "nnue-runtime-dimensions")]
    DynamicLayerStacks(Box<RefCell<DynamicLayerStacksStack>>),
    /// HalfKaSplit 特徴量セット（L256/L512/L1024）
    HalfKaSplit(HalfKaSplitStack),
    /// HalfKaHmMerged 特徴量セット（L256/L512/L1024）
    HalfKaHmMerged(HalfKaHmMergedStack),
    /// HalfKaMerged 特徴量セット（L256/L512/L1024）
    HalfKaMerged(HalfKaMergedStack),
    /// HalfKaHmSplit 特徴量セット（L256/L512/L1024）
    HalfKaHmSplit(HalfKaHmSplitStack),
    /// HalfKP 特徴量セット（L256/L512）
    HalfKP(HalfKPStack),
    /// LayerStacks（L1=1536/768 + 9バケット）
    #[cfg(feature = "layerstack-arch")]
    LayerStacks(LayerStacksAccStack),
}

impl AccumulatorStackVariant {
    /// NNUEネットワークに応じたスタックを作成
    ///
    /// 指定されたネットワークのアーキテクチャに対応するスタックバリアントを生成する。
    pub fn from_network(network: &NNUENetwork) -> Self {
        match network {
            #[cfg(feature = "nnue-runtime-dimensions")]
            NNUENetwork::DynamicHalfKx(net) => {
                Self::DynamicHalfKx(Box::new(RefCell::new(DynamicHalfKxStack::new(net))))
            }
            #[cfg(feature = "nnue-runtime-dimensions")]
            NNUENetwork::DynamicLayerStacks(net) => {
                Self::DynamicLayerStacks(Box::new(RefCell::new(net.new_stack())))
            }
            NNUENetwork::HalfKaSplit(net) => Self::HalfKaSplit(HalfKaSplitStack::from_network(net)),
            NNUENetwork::HalfKaHmMerged(net) => {
                Self::HalfKaHmMerged(HalfKaHmMergedStack::from_network(net))
            }
            NNUENetwork::HalfKaMerged(net) => {
                Self::HalfKaMerged(HalfKaMergedStack::from_network(net))
            }
            NNUENetwork::HalfKaHmSplit(net) => {
                Self::HalfKaHmSplit(HalfKaHmSplitStack::from_network(net))
            }
            NNUENetwork::HalfKP(net) => Self::HalfKP(HalfKPStack::from_network(net)),
            #[cfg(feature = "layerstack-arch")]
            NNUENetwork::LayerStacks(net) => Self::LayerStacks(net.new_acc_stack()),
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
            #[cfg(feature = "nnue-runtime-dimensions")]
            (Self::DynamicHalfKx(stack), NNUENetwork::DynamicHalfKx(net)) => {
                stack.borrow().matches_network(net)
            }
            #[cfg(feature = "nnue-runtime-dimensions")]
            (Self::DynamicLayerStacks(stack), NNUENetwork::DynamicLayerStacks(net)) => {
                stack.borrow().matches_network(net)
            }
            (Self::HalfKaSplit(stack), NNUENetwork::HalfKaSplit(net)) => {
                stack.l1_size() == net.l1_size()
            }
            (Self::HalfKaHmMerged(stack), NNUENetwork::HalfKaHmMerged(net)) => {
                stack.l1_size() == net.l1_size()
            }
            (Self::HalfKaMerged(stack), NNUENetwork::HalfKaMerged(net)) => {
                stack.l1_size() == net.l1_size()
            }
            (Self::HalfKaHmSplit(stack), NNUENetwork::HalfKaHmSplit(net)) => {
                stack.l1_size() == net.l1_size()
            }
            (Self::HalfKP(stack), NNUENetwork::HalfKP(net)) => stack.l1_size() == net.l1_size(),
            #[cfg(feature = "layerstack-arch")]
            (Self::LayerStacks(st), NNUENetwork::LayerStacks(net)) => {
                st.architecture_dims()
                    == (
                        net.architecture_spec().l1,
                        net.architecture_spec().l2,
                        net.architecture_spec().l3,
                    )
            }
            _ => false,
        }
    }

    /// スタックをリセット（探索開始時に呼び出す）
    #[inline]
    pub fn reset(&mut self) {
        match self {
            #[cfg(feature = "nnue-runtime-dimensions")]
            Self::DynamicHalfKx(stack) => stack.get_mut().reset(),
            #[cfg(feature = "nnue-runtime-dimensions")]
            Self::DynamicLayerStacks(stack) => stack.get_mut().reset(),
            Self::HalfKaSplit(stack) => stack.reset(),
            Self::HalfKaHmMerged(stack) => stack.reset(),
            Self::HalfKaMerged(stack) => stack.reset(),
            Self::HalfKaHmSplit(stack) => stack.reset(),
            Self::HalfKP(stack) => stack.reset(),
            #[cfg(feature = "layerstack-arch")]
            Self::LayerStacks(stack) => stack.reset(),
        }
    }

    /// do_move時にスタックをプッシュ
    #[inline]
    pub fn push(&mut self, dirty_piece: DirtyPiece) {
        #[cfg(all(feature = "mode-specific", feature = "layerstack-arch"))]
        if let Self::LayerStacks(stack) = self {
            stack.push();
            stack.set_current_dirty_piece(dirty_piece);
            return;
        }
        self.push_dispatch(dirty_piece);
    }

    // 固定LS構成では通常のpushを他アーキテクチャの分岐配置から分離する。
    // デフォルトスタックなどの非LSバリアントも有効なのでフォールバックを保持する。
    #[cfg_attr(all(feature = "mode-specific", feature = "layerstack-arch"), cold)]
    #[cfg_attr(
        all(feature = "mode-specific", feature = "layerstack-arch"),
        inline(never)
    )]
    #[cfg_attr(
        not(all(feature = "mode-specific", feature = "layerstack-arch")),
        inline
    )]
    fn push_dispatch(&mut self, dirty_piece: DirtyPiece) {
        match self {
            #[cfg(feature = "nnue-runtime-dimensions")]
            Self::DynamicHalfKx(stack) => stack.get_mut().push(dirty_piece),
            #[cfg(feature = "nnue-runtime-dimensions")]
            Self::DynamicLayerStacks(stack) => stack.get_mut().push(dirty_piece),
            Self::HalfKaSplit(stack) => stack.push(dirty_piece),
            Self::HalfKaHmMerged(stack) => stack.push(dirty_piece),
            Self::HalfKaMerged(stack) => stack.push(dirty_piece),
            Self::HalfKaHmSplit(stack) => stack.push(dirty_piece),
            Self::HalfKP(stack) => stack.push(dirty_piece),
            #[cfg(feature = "layerstack-arch")]
            Self::LayerStacks(stack) => {
                stack.push();
                stack.set_current_dirty_piece(dirty_piece);
            }
        }
    }

    /// undo_move時にスタックをポップ
    #[inline]
    pub fn pop(&mut self) {
        match self {
            #[cfg(feature = "nnue-runtime-dimensions")]
            Self::DynamicHalfKx(stack) => stack.get_mut().pop(),
            #[cfg(feature = "nnue-runtime-dimensions")]
            Self::DynamicLayerStacks(stack) => stack.get_mut().pop(),
            Self::HalfKaSplit(stack) => stack.pop(),
            Self::HalfKaHmMerged(stack) => stack.pop(),
            Self::HalfKaMerged(stack) => stack.pop(),
            Self::HalfKaHmSplit(stack) => stack.pop(),
            Self::HalfKP(stack) => stack.pop(),
            #[cfg(feature = "layerstack-arch")]
            Self::LayerStacks(stack) => stack.pop(),
        }
    }

    /// 現在のバリアントがHalfKPかどうか
    #[inline]
    pub fn is_halfkp(&self) -> bool {
        if matches!(self, Self::HalfKP(_)) {
            return true;
        }
        #[cfg(feature = "nnue-runtime-dimensions")]
        {
            matches!(self, Self::DynamicHalfKx(stack) if stack.borrow().is_halfkp())
        }
        #[cfg(not(feature = "nnue-runtime-dimensions"))]
        {
            false
        }
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
        #[cfg(feature = "layerstack-arch")]
        assert!(!matches!(stack, AccumulatorStackVariant::LayerStacks(_)));
        assert!(!matches!(stack, AccumulatorStackVariant::HalfKaSplit(_)));
        assert!(!matches!(stack, AccumulatorStackVariant::HalfKaHmMerged(_)));
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
    fn test_push_pop_index_consistency_halfka_hm() {
        let mut stack = HalfKaHmMergedStack::default();
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
    fn test_halfka_hm_stack_l1_sizes() {
        use crate::nnue::network_halfka_hm_merged::AccumulatorStackHalfKaHmMerged;

        let l256_stack = HalfKaHmMergedStack::L256(AccumulatorStackHalfKaHmMerged::<256>::new());
        let l512_stack = HalfKaHmMergedStack::L512(AccumulatorStackHalfKaHmMerged::<512>::new());
        let l1024_stack = HalfKaHmMergedStack::L1024(AccumulatorStackHalfKaHmMerged::<1024>::new());

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
        let halfka_stack_size = size_of::<HalfKaHmMergedStack>();
        let halfkp_stack_size = size_of::<HalfKPStack>();

        // 新設計では最大のバリアントのサイズ + タグになる
        // 各サブスタックも enum なので効率的
        eprintln!("AccumulatorStackVariant size: {variant_size} bytes");
        eprintln!("HalfKaHmMergedStack size: {halfka_stack_size} bytes");
        eprintln!("HalfKPStack size: {halfkp_stack_size} bytes");
        #[cfg(feature = "layerstack-arch")]
        {
            let layer_stacks_size = size_of::<LayerStacksAccStack>();
            eprintln!("LayerStacks size: {layer_stacks_size} bytes");
        }

        // 列挙型のサイズは最大のバリアントのサイズ + タグ
        assert!(variant_size > 0);
    }
}
