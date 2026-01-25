//! NNUE 評価器（外部 API）
//!
//! Network と Stack のペアリングを内部で保証する。
//! NNUE 評価の推奨経路として、低レベル API を直接使用するより安全。
//!
//! # 使用例
//!
//! ```ignore
//! use std::sync::Arc;
//! use engine_core::nnue::{NNUENetwork, NNUEEvaluator};
//!
//! // ネットワークを読み込み
//! let network = Arc::new(NNUENetwork::load("model.nnue")?);
//!
//! // 評価器を作成（局面を指定して初期化）
//! let mut evaluator = NNUEEvaluator::new_with_position(network, &position);
//!
//! // 評価
//! let value = evaluator.evaluate(&position);
//!
//! // 探索時の操作
//! evaluator.push(dirty_piece);  // do_move 時
//! let value = evaluator.evaluate(&position);
//! evaluator.pop();              // undo_move 時
//!
//! // 並列探索用にクローン（局面を指定して初期化）
//! let mut thread_evaluator = evaluator.clone_for_thread(&position);
//! ```

use std::sync::Arc;

use super::accumulator::DirtyPiece;
use super::accumulator_layer_stacks::AccumulatorStackLayerStacks;
use super::accumulator_stack_variant::AccumulatorStackVariant;
use super::halfka::HalfKAStack;
use super::halfkp::HalfKPStack;
use super::network::NNUENetwork;
use super::spec::ArchitectureSpec;
use crate::position::Position;
use crate::types::Value;

/// NNUE 評価器（外部 API）
///
/// Network と Stack のペアリングを内部で保証する。
/// NNUE 評価の推奨経路として、低レベル API を直接使用するより安全。
///
/// # 設計
///
/// - `net` は `Arc` で共有（並列探索で複数スレッドが同じ重みを参照）
/// - `stack` はスレッド/探索文脈ごとに独立
///
/// # 使用方法
///
/// `new_with_position()` で局面を指定して作成し、即座に `evaluate()` 可能。
pub struct NNUEEvaluator {
    net: Arc<NNUENetwork>,
    stack: AccumulatorStackVariant,
}

impl NNUEEvaluator {
    /// 局面を指定して評価器を作成
    ///
    /// 内部で `reset()` を呼び出すため、即座に `evaluate()` 可能。
    ///
    /// # 例
    ///
    /// ```ignore
    /// let mut evaluator = NNUEEvaluator::new_with_position(network, &position);
    /// let value = evaluator.evaluate(&position);  // 即座に評価可能
    /// ```
    pub fn new_with_position(net: Arc<NNUENetwork>, pos: &Position) -> Self {
        let stack = AccumulatorStackVariant::from_network(&net);
        let mut evaluator = Self { net, stack };
        evaluator.reset(pos);
        evaluator
    }

    /// 並列探索用に評価器を複製し、指定局面で初期化
    ///
    /// 各スレッドで独立した探索状態を持つために使用。
    /// Network の重みは `Arc` で共有されるため、メモリ効率が良い。
    /// 内部で `reset()` まで行うため、即座に `evaluate()` 可能。
    ///
    /// # 引数
    ///
    /// - `pos`: 初期化する局面
    pub fn clone_for_thread(&self, pos: &Position) -> Self {
        let mut evaluator = Self {
            net: Arc::clone(&self.net),
            stack: AccumulatorStackVariant::from_network(&self.net),
        };
        evaluator.reset(pos);
        evaluator
    }

    // =========================================================================
    // 探索操作 API
    // =========================================================================

    /// スタックをリセット（探索開始時に呼び出す）
    ///
    /// ルート局面でアキュムレータをフル再計算する。
    ///
    /// # 引数
    ///
    /// - `pos`: リセット後の局面（アキュムレータ計算に使用）
    pub fn reset(&mut self, pos: &Position) {
        self.stack.reset();
        self.refresh_accumulator(pos);
    }

    /// 手を進める（do_move 時）
    ///
    /// アキュムレータスタックに新しいエントリをプッシュする。
    ///
    /// # 引数
    ///
    /// - `dirty_piece`: 指し手で変化した駒情報（差分更新に使用）
    #[inline]
    pub fn push(&mut self, dirty_piece: DirtyPiece) {
        self.stack.push(dirty_piece);
    }

    /// 手を戻す（undo_move 時）
    ///
    /// アキュムレータスタックから最新エントリをポップする。
    #[inline]
    pub fn pop(&mut self) {
        self.stack.pop();
    }

    /// 評価値を計算
    ///
    /// 必要に応じてアキュムレータを更新し、評価値を返す。
    ///
    /// # 引数
    ///
    /// - `pos`: 評価対象の局面
    ///
    /// # 戻り値
    ///
    /// 局面の評価値（手番側から見た評価値）
    #[inline(always)]
    pub fn evaluate(&mut self, pos: &Position) -> Value {
        // アキュムレータを更新（必要に応じて差分更新 or フル再計算）
        self.ensure_accumulator_computed(pos);

        // 評価
        self.evaluate_only(pos)
    }

    /// アキュムレータをフル再計算（ベンチマーク用）
    ///
    /// 通常は `reset()` を使用すること。
    /// ベンチマークでアキュムレータ計算のみを測定したい場合に使用。
    ///
    /// # 引数
    ///
    /// - `pos`: 計算対象の局面
    pub fn refresh(&mut self, pos: &Position) {
        self.refresh_accumulator(pos);
    }

    /// アキュムレータ更新なしで評価のみ実行（ベンチマーク用）
    ///
    /// アキュムレータが計算済みであることが前提。
    /// ベンチマークで評価部分のみを測定したい場合に使用。
    ///
    /// # 引数
    ///
    /// - `pos`: 評価対象の局面
    ///
    /// # 戻り値
    ///
    /// 局面の評価値（手番側から見た評価値）
    ///
    /// # 注意
    ///
    /// アキュムレータが未計算の場合、不正な評価値が返る。
    /// 通常は `evaluate()` を使用すること。
    #[inline(always)]
    pub fn evaluate_only(&self, pos: &Position) -> Value {
        match (&*self.net, &self.stack) {
            (NNUENetwork::HalfKA(net), AccumulatorStackVariant::HalfKA(st)) => {
                net.evaluate(pos, st)
            }
            (NNUENetwork::HalfKP(net), AccumulatorStackVariant::HalfKP(st)) => {
                net.evaluate(pos, st)
            }
            (NNUENetwork::LayerStacks(net), AccumulatorStackVariant::LayerStacks(st)) => {
                net.evaluate(pos, &st.current().accumulator)
            }
            _ => unreachable!("Network/Stack type mismatch"),
        }
    }

    // =========================================================================
    // 情報取得
    // =========================================================================

    /// アーキテクチャ名を取得
    pub fn architecture_name(&self) -> &'static str {
        self.net.architecture_name()
    }

    /// アーキテクチャ仕様を取得
    pub fn architecture_spec(&self) -> ArchitectureSpec {
        self.net.architecture_spec()
    }

    /// ネットワークへの参照を取得
    pub fn network(&self) -> &Arc<NNUENetwork> {
        &self.net
    }

    /// L1 サイズを取得
    pub fn l1_size(&self) -> usize {
        self.net.l1_size()
    }

    // =========================================================================
    // 内部実装
    // =========================================================================

    /// アキュムレータをフル再計算
    fn refresh_accumulator(&mut self, pos: &Position) {
        match (&*self.net, &mut self.stack) {
            (NNUENetwork::HalfKA(net), AccumulatorStackVariant::HalfKA(st)) => {
                net.refresh_accumulator(pos, st);
            }
            (NNUENetwork::HalfKP(net), AccumulatorStackVariant::HalfKP(st)) => {
                net.refresh_accumulator(pos, st);
            }
            (NNUENetwork::LayerStacks(net), AccumulatorStackVariant::LayerStacks(st)) => {
                net.refresh_accumulator(pos, &mut st.current_mut().accumulator);
            }
            _ => unreachable!("Network/Stack type mismatch"),
        }
    }

    /// アキュムレータが計算済みか確認し、必要に応じて更新
    fn ensure_accumulator_computed(&mut self, pos: &Position) {
        match (&*self.net, &mut self.stack) {
            (NNUENetwork::HalfKA(net), AccumulatorStackVariant::HalfKA(st)) => {
                Self::update_halfka_accumulator(net, pos, st);
            }
            (NNUENetwork::HalfKP(net), AccumulatorStackVariant::HalfKP(st)) => {
                Self::update_halfkp_accumulator(net, pos, st);
            }
            (NNUENetwork::LayerStacks(net), AccumulatorStackVariant::LayerStacks(st)) => {
                Self::update_layer_stacks_accumulator(net, pos, st);
            }
            _ => unreachable!("Network/Stack type mismatch"),
        }
    }

    /// HalfKA アキュムレータを更新
    #[inline]
    fn update_halfka_accumulator(
        net: &super::halfka::HalfKANetwork,
        pos: &Position,
        stack: &mut HalfKAStack,
    ) {
        if stack.is_current_computed() {
            return;
        }

        let mut updated = false;

        // 1. 直前局面で差分更新を試行
        if let Some(prev_idx) = stack.current_previous() {
            if stack.is_entry_computed(prev_idx) {
                let dirty = stack.current_dirty_piece();
                net.update_accumulator(pos, &dirty, stack, prev_idx);
                updated = true;
            }
        }

        // 2. 失敗なら祖先探索 + 複数手差分更新を試行
        if !updated {
            if let Some((source_idx, _depth)) = stack.find_usable_accumulator() {
                updated = net.forward_update_incremental(pos, stack, source_idx);
            }
        }

        // 3. それでも失敗なら全計算
        if !updated {
            net.refresh_accumulator(pos, stack);
        }
    }

    /// HalfKP アキュムレータを更新
    #[inline]
    fn update_halfkp_accumulator(
        net: &super::halfkp::HalfKPNetwork,
        pos: &Position,
        stack: &mut HalfKPStack,
    ) {
        if stack.is_current_computed() {
            return;
        }

        let mut updated = false;

        // 1. 直前局面で差分更新を試行
        if let Some(prev_idx) = stack.current_previous() {
            if stack.is_entry_computed(prev_idx) {
                let dirty = stack.current_dirty_piece();
                net.update_accumulator(pos, &dirty, stack, prev_idx);
                updated = true;
            }
        }

        // 2. 失敗なら祖先探索 + 複数手差分更新を試行
        if !updated {
            if let Some((source_idx, _depth)) = stack.find_usable_accumulator() {
                updated = net.forward_update_incremental(pos, stack, source_idx);
            }
        }

        // 3. それでも失敗なら全計算
        if !updated {
            net.refresh_accumulator(pos, stack);
        }
    }

    /// LayerStacks アキュムレータを更新
    #[inline]
    fn update_layer_stacks_accumulator(
        net: &super::network_layer_stacks::NetworkLayerStacks,
        pos: &Position,
        stack: &mut AccumulatorStackLayerStacks,
    ) {
        let current_entry = stack.current();
        if current_entry.accumulator.computed_accumulation {
            return;
        }

        let mut updated = false;

        // 1. 直前局面で差分更新を試行
        if let Some(prev_idx) = current_entry.previous {
            let prev_computed = stack.entry_at(prev_idx).accumulator.computed_accumulation;
            if prev_computed {
                let dirty_piece = stack.current().dirty_piece;
                let (prev_acc, current_acc) = stack.get_prev_and_current_accumulators(prev_idx);
                net.update_accumulator(pos, &dirty_piece, current_acc, prev_acc);
                updated = true;
            }
        }

        // 2. 失敗なら祖先探索 + 複数手差分更新を試行
        if !updated {
            if let Some((source_idx, _depth)) = stack.find_usable_accumulator() {
                updated = net.forward_update_incremental(pos, stack, source_idx);
            }
        }

        // 3. それでも失敗なら全計算
        if !updated {
            let acc = &mut stack.current_mut().accumulator;
            net.refresh_accumulator(pos, acc);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// NNUEEvaluator の基本的な構築テスト
    ///
    /// ネットワークなしでのテスト（構造確認のみ）
    #[test]
    fn test_evaluator_construction() {
        // デフォルトの AccumulatorStackVariant と同様の構造確認
        let stack = AccumulatorStackVariant::new_default();
        assert!(stack.is_halfkp());
    }

    /// push/pop の対称性テスト
    #[test]
    fn test_stack_push_pop() {
        let mut stack = AccumulatorStackVariant::new_default();
        let dirty = DirtyPiece::default();

        stack.reset();
        stack.push(dirty);
        stack.push(dirty);
        stack.pop();
        stack.pop();
        // パニックしなければ成功
    }

    /// NNUEEvaluator のサイズテスト
    #[test]
    fn test_evaluator_size() {
        use std::mem::size_of;

        let evaluator_size = size_of::<NNUEEvaluator>();
        let arc_size = size_of::<Arc<NNUENetwork>>();
        let stack_size = size_of::<AccumulatorStackVariant>();

        eprintln!("NNUEEvaluator size: {evaluator_size} bytes");
        eprintln!("Arc<NNUENetwork> size: {arc_size} bytes");
        eprintln!("AccumulatorStackVariant size: {stack_size} bytes");

        // Evaluator は Arc + Stack のサイズ程度
        assert!(evaluator_size > 0);
    }

    /// AccumulatorStackVariant::matches_network のテスト
    #[test]
    fn test_stack_variant_type_checking() {
        use crate::nnue::network_halfka::AccumulatorStackHalfKA;
        use crate::nnue::network_halfkp::AccumulatorStackHalfKP;

        // HalfKA スタックの各 L1 サイズ
        let halfka_l256 = AccumulatorStackVariant::HalfKA(HalfKAStack::L256(
            AccumulatorStackHalfKA::<256>::new(),
        ));
        let halfka_l512 = AccumulatorStackVariant::HalfKA(HalfKAStack::L512(
            AccumulatorStackHalfKA::<512>::new(),
        ));
        let halfka_l1024 = AccumulatorStackVariant::HalfKA(HalfKAStack::L1024(
            AccumulatorStackHalfKA::<1024>::new(),
        ));

        // HalfKP スタックの各 L1 サイズ
        let halfkp_l256 = AccumulatorStackVariant::HalfKP(HalfKPStack::L256(
            AccumulatorStackHalfKP::<256>::new(),
        ));
        let halfkp_l512 = AccumulatorStackVariant::HalfKP(HalfKPStack::L512(
            AccumulatorStackHalfKP::<512>::new(),
        ));

        // 型の確認
        assert!(matches!(halfka_l256, AccumulatorStackVariant::HalfKA(_)));
        assert!(matches!(halfka_l512, AccumulatorStackVariant::HalfKA(_)));
        assert!(matches!(halfka_l1024, AccumulatorStackVariant::HalfKA(_)));
        assert!(matches!(halfkp_l256, AccumulatorStackVariant::HalfKP(_)));
        assert!(matches!(halfkp_l512, AccumulatorStackVariant::HalfKP(_)));

        // is_halfkp() の確認
        assert!(!halfka_l256.is_halfkp());
        assert!(!halfka_l512.is_halfkp());
        assert!(!halfka_l1024.is_halfkp());
        assert!(halfkp_l256.is_halfkp());
        assert!(halfkp_l512.is_halfkp());
    }

    /// 各バリアントの push/pop インデックス一貫性テスト
    #[test]
    fn test_all_variants_push_pop_consistency() {
        use crate::nnue::accumulator_layer_stacks::AccumulatorStackLayerStacks;
        use crate::nnue::network_halfka::AccumulatorStackHalfKA;
        use crate::nnue::network_halfkp::AccumulatorStackHalfKP;

        let dirty = DirtyPiece::default();

        // HalfKA L512
        let mut stack = AccumulatorStackVariant::HalfKA(HalfKAStack::L512(
            AccumulatorStackHalfKA::<512>::new(),
        ));
        stack.reset();
        stack.push(dirty);
        stack.push(dirty);
        stack.pop();
        stack.pop();
        // パニックしなければ成功

        // HalfKP L512
        let mut stack = AccumulatorStackVariant::HalfKP(HalfKPStack::L512(
            AccumulatorStackHalfKP::<512>::new(),
        ));
        stack.reset();
        stack.push(dirty);
        stack.push(dirty);
        stack.pop();
        stack.pop();
        // パニックしなければ成功

        // LayerStacks
        let mut stack = AccumulatorStackVariant::LayerStacks(AccumulatorStackLayerStacks::new());
        stack.reset();
        stack.push(dirty);
        stack.push(dirty);
        stack.pop();
        stack.pop();
        // パニックしなければ成功
    }

    /// 深い探索木での push/pop テスト
    #[test]
    fn test_deep_search_simulation() {
        let mut stack = AccumulatorStackVariant::new_default();
        let dirty = DirtyPiece::default();

        stack.reset();

        // 典型的な探索深さ (30手程度)
        const MAX_DEPTH: usize = 30;

        // 複数回の探索をシミュレート
        for _ in 0..5 {
            // 探索開始
            for _ in 0..MAX_DEPTH {
                stack.push(dirty);
            }

            // 探索終了
            for _ in 0..MAX_DEPTH {
                stack.pop();
            }
        }

        // パニックしなければ成功
    }

    /// network.rs の NNUENetwork enum が全アーキテクチャをカバーしていることの確認
    #[test]
    fn test_network_enum_coverage() {
        use crate::nnue::halfka::HalfKANetwork;
        use crate::nnue::halfkp::HalfKPNetwork;

        // HalfKA サポートアーキテクチャ数
        let halfka_specs = HalfKANetwork::supported_specs();
        assert_eq!(halfka_specs.len(), 15); // 256:3 + 512:6 + 1024:6

        // HalfKP サポートアーキテクチャ数
        let halfkp_specs = HalfKPNetwork::supported_specs();
        assert_eq!(halfkp_specs.len(), 12); // 256:3 + 512:6 + 1024:3

        // 全アーキテクチャで feature_set が正しいことを確認
        for spec in &halfka_specs {
            assert_eq!(
                spec.feature_set,
                crate::nnue::spec::FeatureSet::HalfKA,
                "HalfKA spec has wrong feature_set: {spec:?}"
            );
        }

        for spec in &halfkp_specs {
            assert_eq!(
                spec.feature_set,
                crate::nnue::spec::FeatureSet::HalfKP,
                "HalfKP spec has wrong feature_set: {spec:?}"
            );
        }
    }
}
