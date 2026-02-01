// NOTE: 公式表記(HalfKA_hm)をenum名に保持するため、非CamelCaseを許可する。
#![allow(non_camel_case_types)]

//! HalfKA_hm アーキテクチャ階層
//!
//! L1 サイズごとにモジュールを分割し、L2/L3/活性化の組み合わせを enum で表現。
//!
//! # 構造
//!
//! ```text
//! HalfKA_hmNetwork
//! ├── L256(HalfKA_hm_L256)
//! │   ├── CReLU_32_32
//! │   ├── SCReLU_32_32
//! │   └── Pairwise_32_32
//! ├── L512(HalfKA_hm_L512)
//! │   ├── CReLU_8_96
//! │   ├── SCReLU_8_96
//! │   └── Pairwise_8_96
//! └── L1024(HalfKA_hm_L1024)
//!     ├── CReLU_8_96
//!     ├── SCReLU_8_96
//!     ├── Pairwise_8_96
//!     ├── CReLU_8_32
//!     └── SCReLU_8_32
//! ```

mod l1024;
mod l256;
mod l512;

pub use l1024::HalfKA_hm_L1024;
pub use l256::HalfKA_hm_L256;
pub use l512::HalfKA_hm_L512;

use crate::nnue::accumulator::DirtyPiece;
use crate::nnue::network_halfka_hm::AccumulatorStackHalfKA_hm;
use crate::nnue::spec::{Activation, ArchitectureSpec};
use crate::position::Position;
use crate::types::Value;

/// HalfKA_hm 特徴量セットのネットワーク（第2階層）
///
/// L1 サイズごとにバリアントを持つ。
/// L2/L3/活性化の追加で変更不要（L1 enum 内に閉じる）。
pub enum HalfKA_hmNetwork {
    L256(HalfKA_hm_L256),
    L512(HalfKA_hm_L512),
    L1024(HalfKA_hm_L1024),
}

impl HalfKA_hmNetwork {
    /// 評価値を計算
    #[inline(always)]
    pub fn evaluate(&self, pos: &Position, stack: &HalfKA_hmStack) -> Value {
        match (self, stack) {
            (Self::L256(net), HalfKA_hmStack::L256(st)) => net.evaluate(pos, st),
            (Self::L512(net), HalfKA_hmStack::L512(st)) => net.evaluate(pos, st),
            (Self::L1024(net), HalfKA_hmStack::L1024(st)) => net.evaluate(pos, st),
            _ => unreachable!("L1 mismatch: network={}, stack={}", self.l1_size(), stack.l1_size()),
        }
    }

    /// Accumulator をフル再計算
    #[inline(always)]
    pub fn refresh_accumulator(&self, pos: &Position, stack: &mut HalfKA_hmStack) {
        match (self, stack) {
            (Self::L256(net), HalfKA_hmStack::L256(st)) => net.refresh_accumulator(pos, st),
            (Self::L512(net), HalfKA_hmStack::L512(st)) => net.refresh_accumulator(pos, st),
            (Self::L1024(net), HalfKA_hmStack::L1024(st)) => net.refresh_accumulator(pos, st),
            _ => unreachable!("L1 mismatch"),
        }
    }

    /// 差分更新（dirty piece ベース）
    #[inline(always)]
    pub fn update_accumulator(
        &self,
        pos: &Position,
        dirty: &DirtyPiece,
        stack: &mut HalfKA_hmStack,
        source_idx: usize,
    ) {
        match (self, stack) {
            (Self::L256(net), HalfKA_hmStack::L256(st)) => {
                net.update_accumulator(pos, dirty, st, source_idx)
            }
            (Self::L512(net), HalfKA_hmStack::L512(st)) => {
                net.update_accumulator(pos, dirty, st, source_idx)
            }
            (Self::L1024(net), HalfKA_hmStack::L1024(st)) => {
                net.update_accumulator(pos, dirty, st, source_idx)
            }
            _ => unreachable!("L1 mismatch"),
        }
    }

    /// 前方差分更新を試みる（成功したら true）
    #[inline(always)]
    pub fn forward_update_incremental(
        &self,
        pos: &Position,
        stack: &mut HalfKA_hmStack,
        source_idx: usize,
    ) -> bool {
        match (self, stack) {
            (Self::L256(net), HalfKA_hmStack::L256(st)) => {
                net.forward_update_incremental(pos, st, source_idx)
            }
            (Self::L512(net), HalfKA_hmStack::L512(st)) => {
                net.forward_update_incremental(pos, st, source_idx)
            }
            (Self::L1024(net), HalfKA_hmStack::L1024(st)) => {
                net.forward_update_incremental(pos, st, source_idx)
            }
            _ => unreachable!("L1 mismatch"),
        }
    }

    /// ファイルから読み込み
    ///
    /// L1/L2/L3/活性化に基づいて適切なバリアントを選択。
    ///
    /// # エラー
    ///
    /// - L2/L3 が 0 の場合（旧 bullet-shogi 形式）: 明確なエラーメッセージを返す
    /// - サポートされていない L1 の場合: エラーを返す
    pub fn read<R: std::io::Read + std::io::Seek>(
        reader: &mut R,
        l1: usize,
        l2: usize,
        l3: usize,
        activation: Activation,
    ) -> std::io::Result<Self> {
        // 旧形式フォールバック削除: L2/L3 が 0 の場合はエラー
        if l2 == 0 || l3 == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "HalfKA_hm L1={l1} network missing L2/L3 dimensions in header. \
                     This is an old bullet-shogi format that is no longer supported. \
                     Please re-export the model with a newer version of bullet-shogi."
                ),
            ));
        }

        match l1 {
            256 => {
                let net = HalfKA_hm_L256::read(reader, l2, l3, activation)?;
                Ok(Self::L256(net))
            }
            512 => {
                let net = HalfKA_hm_L512::read(reader, l2, l3, activation)?;
                Ok(Self::L512(net))
            }
            1024 => {
                let net = HalfKA_hm_L1024::read(reader, l2, l3, activation)?;
                Ok(Self::L1024(net))
            }
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Unsupported HalfKA_hm L1: {l1}"),
            )),
        }
    }

    /// L1 サイズを取得
    pub fn l1_size(&self) -> usize {
        match self {
            Self::L256(_) => 256,
            Self::L512(_) => 512,
            Self::L1024(_) => 1024,
        }
    }

    /// アーキテクチャ名を取得
    pub fn architecture_name(&self) -> &'static str {
        match self {
            Self::L256(net) => net.architecture_name(),
            Self::L512(net) => net.architecture_name(),
            Self::L1024(net) => net.architecture_name(),
        }
    }

    /// アーキテクチャ仕様を取得
    pub fn architecture_spec(&self) -> ArchitectureSpec {
        match self {
            Self::L256(net) => net.architecture_spec(),
            Self::L512(net) => net.architecture_spec(),
            Self::L1024(net) => net.architecture_spec(),
        }
    }

    /// サポートするアーキテクチャ一覧
    pub fn supported_specs() -> Vec<ArchitectureSpec> {
        let mut specs = Vec::new();
        specs.extend_from_slice(HalfKA_hm_L256::SUPPORTED_SPECS);
        specs.extend_from_slice(HalfKA_hm_L512::SUPPORTED_SPECS);
        specs.extend_from_slice(HalfKA_hm_L1024::SUPPORTED_SPECS);
        specs
    }
}

/// HalfKA_hm Accumulator スタック（L1 のみで決まる）
///
/// L2/L3/活性化の追加で変更不要。
pub enum HalfKA_hmStack {
    L256(AccumulatorStackHalfKA_hm<256>),
    L512(AccumulatorStackHalfKA_hm<512>),
    L1024(AccumulatorStackHalfKA_hm<1024>),
}

impl HalfKA_hmStack {
    /// ネットワークに対応するスタックを生成
    ///
    /// バリアントマッチを使用し、新しい L1 追加時にコンパイル時に漏れ検知。
    pub fn from_network(net: &HalfKA_hmNetwork) -> Self {
        match net {
            HalfKA_hmNetwork::L256(_) => Self::L256(AccumulatorStackHalfKA_hm::<256>::new()),
            HalfKA_hmNetwork::L512(_) => Self::L512(AccumulatorStackHalfKA_hm::<512>::new()),
            HalfKA_hmNetwork::L1024(_) => Self::L1024(AccumulatorStackHalfKA_hm::<1024>::new()),
        }
    }

    /// L1 サイズを取得
    pub fn l1_size(&self) -> usize {
        match self {
            Self::L256(_) => 256,
            Self::L512(_) => 512,
            Self::L1024(_) => 1024,
        }
    }

    /// スタックをリセット
    pub fn reset(&mut self) {
        match self {
            Self::L256(s) => s.reset(),
            Self::L512(s) => s.reset(),
            Self::L1024(s) => s.reset(),
        }
    }

    /// ply を進める
    pub fn push(&mut self, dirty: DirtyPiece) {
        match self {
            Self::L256(s) => s.push(dirty),
            Self::L512(s) => s.push(dirty),
            Self::L1024(s) => s.push(dirty),
        }
    }

    /// ply を戻す
    pub fn pop(&mut self) {
        match self {
            Self::L256(s) => s.pop(),
            Self::L512(s) => s.pop(),
            Self::L1024(s) => s.pop(),
        }
    }

    /// 現在のインデックスを取得
    pub fn current_index(&self) -> usize {
        match self {
            Self::L256(s) => s.current_index(),
            Self::L512(s) => s.current_index(),
            Self::L1024(s) => s.current_index(),
        }
    }

    /// 祖先を辿って使用可能なアキュムレータを探す
    pub fn find_usable_accumulator(&self) -> Option<(usize, usize)> {
        match self {
            Self::L256(s) => s.find_usable_accumulator(),
            Self::L512(s) => s.find_usable_accumulator(),
            Self::L1024(s) => s.find_usable_accumulator(),
        }
    }

    /// 現在のアキュムレータが計算済みかどうか
    #[inline]
    pub fn is_current_computed(&self) -> bool {
        match self {
            Self::L256(s) => s.current().accumulator.computed_accumulation,
            Self::L512(s) => s.current().accumulator.computed_accumulation,
            Self::L1024(s) => s.current().accumulator.computed_accumulation,
        }
    }

    /// 現在のエントリの previous インデックス
    #[inline]
    pub fn current_previous(&self) -> Option<usize> {
        match self {
            Self::L256(s) => s.current().previous,
            Self::L512(s) => s.current().previous,
            Self::L1024(s) => s.current().previous,
        }
    }

    /// 指定インデックスのエントリが計算済みかどうか
    #[inline]
    pub fn is_entry_computed(&self, idx: usize) -> bool {
        match self {
            Self::L256(s) => s.entry_at(idx).accumulator.computed_accumulation,
            Self::L512(s) => s.entry_at(idx).accumulator.computed_accumulation,
            Self::L1024(s) => s.entry_at(idx).accumulator.computed_accumulation,
        }
    }

    /// 現在のエントリの dirty piece を取得
    #[inline]
    pub fn current_dirty_piece(&self) -> DirtyPiece {
        match self {
            Self::L256(s) => s.current().dirty_piece,
            Self::L512(s) => s.current().dirty_piece,
            Self::L1024(s) => s.current().dirty_piece,
        }
    }
}

impl Default for HalfKA_hmStack {
    fn default() -> Self {
        Self::L512(AccumulatorStackHalfKA_hm::<512>::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nnue::spec::FeatureSet;

    #[test]
    fn test_halfka_stack_from_network_l1_size() {
        // L256 ネットワークを仮定したスタック
        let stack = HalfKA_hmStack::L256(AccumulatorStackHalfKA_hm::<256>::new());
        assert_eq!(stack.l1_size(), 256);

        let stack = HalfKA_hmStack::L512(AccumulatorStackHalfKA_hm::<512>::new());
        assert_eq!(stack.l1_size(), 512);

        let stack = HalfKA_hmStack::L1024(AccumulatorStackHalfKA_hm::<1024>::new());
        assert_eq!(stack.l1_size(), 1024);
    }

    #[test]
    fn test_supported_specs_combined() {
        let specs = HalfKA_hmNetwork::supported_specs();
        // 256: 1, 512: 2, 1024: 2
        assert_eq!(specs.len(), 5);

        // 全て HalfKA_hm
        for spec in &specs {
            assert_eq!(spec.feature_set, FeatureSet::HalfKA_hm);
        }
    }

    /// push/pop の対称性と状態の一貫性テスト（L256）
    #[test]
    fn test_push_pop_index_consistency_l256() {
        let mut stack = HalfKA_hmStack::L256(AccumulatorStackHalfKA_hm::<256>::new());
        let dirty = DirtyPiece::default();

        stack.reset();
        let initial_index = stack.current_index();

        stack.push(dirty);
        assert_eq!(stack.current_index(), initial_index + 1);

        stack.push(dirty);
        assert_eq!(stack.current_index(), initial_index + 2);

        stack.pop();
        assert_eq!(stack.current_index(), initial_index + 1);

        stack.pop();
        assert_eq!(stack.current_index(), initial_index);
    }

    /// push/pop の対称性と状態の一貫性テスト（L512）
    #[test]
    fn test_push_pop_index_consistency_l512() {
        let mut stack = HalfKA_hmStack::L512(AccumulatorStackHalfKA_hm::<512>::new());
        let dirty = DirtyPiece::default();

        stack.reset();
        let initial_index = stack.current_index();

        stack.push(dirty);
        assert_eq!(stack.current_index(), initial_index + 1);

        stack.pop();
        assert_eq!(stack.current_index(), initial_index);
    }

    /// push/pop の対称性と状態の一貫性テスト（L1024）
    #[test]
    fn test_push_pop_index_consistency_l1024() {
        let mut stack = HalfKA_hmStack::L1024(AccumulatorStackHalfKA_hm::<1024>::new());
        let dirty = DirtyPiece::default();

        stack.reset();
        let initial_index = stack.current_index();

        stack.push(dirty);
        assert_eq!(stack.current_index(), initial_index + 1);

        stack.pop();
        assert_eq!(stack.current_index(), initial_index);
    }

    /// deep push/pop テスト（探索木の深さをシミュレート）
    #[test]
    fn test_deep_push_pop() {
        let mut stack = HalfKA_hmStack::default();
        let dirty = DirtyPiece::default();

        stack.reset();
        let initial_index = stack.current_index();

        // 探索木の深さをシミュレート
        const DEPTH: usize = 30;

        for i in 0..DEPTH {
            stack.push(dirty);
            assert_eq!(stack.current_index(), initial_index + i + 1);
        }

        for i in (0..DEPTH).rev() {
            stack.pop();
            assert_eq!(stack.current_index(), initial_index + i);
        }
    }

    /// アーキテクチャの仕様一覧の一貫性テスト
    #[test]
    fn test_architecture_spec_consistency() {
        for spec in HalfKA_hmNetwork::supported_specs() {
            assert_eq!(spec.feature_set, FeatureSet::HalfKA_hm);
            assert!(spec.l1 == 256 || spec.l1 == 512 || spec.l1 == 1024);
            assert!(spec.l2 > 0 && spec.l2 <= 128);
            assert!(spec.l3 > 0 && spec.l3 <= 128);
        }
    }
}
