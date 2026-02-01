//! HalfKP アーキテクチャ階層
//!
//! L1 サイズごとにモジュールを分割し、L2/L3/活性化の組み合わせを enum で表現。
//!
//! # 構造
//!
//! ```text
//! HalfKPNetwork
//! ├── L256(HalfKPL256)
//! │   ├── CReLU_32_32
//! │   ├── SCReLU_32_32
//! │   └── Pairwise_32_32
//! ├── L512(HalfKPL512)
//! │   ├── CReLU_8_96
//! │   ├── SCReLU_8_96
//! │   ├── Pairwise_8_96
//! │   ├── CReLU_32_32
//! │   ├── SCReLU_32_32
//! │   └── Pairwise_32_32
//! └── L1024(HalfKPL1024)
//!     ├── CReLU_8_32
//!     ├── SCReLU_8_32
//!     └── Pairwise_8_32
//! ```

mod l1024;
mod l256;
mod l512;
mod l768;

pub use l1024::HalfKPL1024;
pub use l256::HalfKPL256;
pub use l512::HalfKPL512;
pub use l768::HalfKPL768;

use crate::nnue::accumulator::DirtyPiece;
use crate::nnue::network_halfkp::AccumulatorStackHalfKP;
use crate::nnue::spec::{Activation, ArchitectureSpec};
use crate::position::Position;
use crate::types::Value;

/// HalfKP 特徴量セットのネットワーク（第2階層）
///
/// L1 サイズごとにバリアントを持つ。
/// L2/L3/活性化の追加で変更不要（L1 enum 内に閉じる）。
pub enum HalfKPNetwork {
    L256(HalfKPL256),
    L512(HalfKPL512),
    L768(HalfKPL768),
    L1024(HalfKPL1024),
}

impl HalfKPNetwork {
    /// 評価値を計算
    #[inline(always)]
    pub fn evaluate(&self, pos: &Position, stack: &HalfKPStack) -> Value {
        match (self, stack) {
            (Self::L256(net), HalfKPStack::L256(st)) => net.evaluate(pos, st),
            (Self::L512(net), HalfKPStack::L512(st)) => net.evaluate(pos, st),
            (Self::L768(net), HalfKPStack::L768(st)) => net.evaluate(pos, st),
            (Self::L1024(net), HalfKPStack::L1024(st)) => net.evaluate(pos, st),
            _ => unreachable!("L1 mismatch: network={}, stack={}", self.l1_size(), stack.l1_size()),
        }
    }

    /// Accumulator をフル再計算
    #[inline(always)]
    pub fn refresh_accumulator(&self, pos: &Position, stack: &mut HalfKPStack) {
        match (self, stack) {
            (Self::L256(net), HalfKPStack::L256(st)) => net.refresh_accumulator(pos, st),
            (Self::L512(net), HalfKPStack::L512(st)) => net.refresh_accumulator(pos, st),
            (Self::L768(net), HalfKPStack::L768(st)) => net.refresh_accumulator(pos, st),
            (Self::L1024(net), HalfKPStack::L1024(st)) => net.refresh_accumulator(pos, st),
            _ => unreachable!("L1 mismatch"),
        }
    }

    /// 差分更新（dirty piece ベース）
    #[inline(always)]
    pub fn update_accumulator(
        &self,
        pos: &Position,
        dirty: &DirtyPiece,
        stack: &mut HalfKPStack,
        source_idx: usize,
    ) {
        match (self, stack) {
            (Self::L256(net), HalfKPStack::L256(st)) => {
                net.update_accumulator(pos, dirty, st, source_idx)
            }
            (Self::L512(net), HalfKPStack::L512(st)) => {
                net.update_accumulator(pos, dirty, st, source_idx)
            }
            (Self::L768(net), HalfKPStack::L768(st)) => {
                net.update_accumulator(pos, dirty, st, source_idx)
            }
            (Self::L1024(net), HalfKPStack::L1024(st)) => {
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
        stack: &mut HalfKPStack,
        source_idx: usize,
    ) -> bool {
        match (self, stack) {
            (Self::L256(net), HalfKPStack::L256(st)) => {
                net.forward_update_incremental(pos, st, source_idx)
            }
            (Self::L512(net), HalfKPStack::L512(st)) => {
                net.forward_update_incremental(pos, st, source_idx)
            }
            (Self::L768(net), HalfKPStack::L768(st)) => {
                net.forward_update_incremental(pos, st, source_idx)
            }
            (Self::L1024(net), HalfKPStack::L1024(st)) => {
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
                    "HalfKP L1={l1} network missing L2/L3 dimensions in header. \
                     This is an old bullet-shogi format that is no longer supported. \
                     Please re-export the model with a newer version of bullet-shogi."
                ),
            ));
        }

        match l1 {
            256 => {
                let net = HalfKPL256::read(reader, l2, l3, activation)?;
                Ok(Self::L256(net))
            }
            512 => {
                let net = HalfKPL512::read(reader, l2, l3, activation)?;
                Ok(Self::L512(net))
            }
            768 => {
                let net = HalfKPL768::read(reader, l2, l3, activation)?;
                Ok(Self::L768(net))
            }
            1024 => {
                let net = HalfKPL1024::read(reader, l2, l3, activation)?;
                Ok(Self::L1024(net))
            }
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Unsupported HalfKP L1: {l1}"),
            )),
        }
    }

    /// L1 サイズを取得
    pub fn l1_size(&self) -> usize {
        match self {
            Self::L256(_) => 256,
            Self::L512(_) => 512,
            Self::L768(_) => 768,
            Self::L1024(_) => 1024,
        }
    }

    /// アーキテクチャ名を取得
    pub fn architecture_name(&self) -> &'static str {
        match self {
            Self::L256(net) => net.architecture_name(),
            Self::L512(net) => net.architecture_name(),
            Self::L768(net) => net.architecture_name(),
            Self::L1024(net) => net.architecture_name(),
        }
    }

    /// アーキテクチャ仕様を取得
    pub fn architecture_spec(&self) -> ArchitectureSpec {
        match self {
            Self::L256(net) => net.architecture_spec(),
            Self::L512(net) => net.architecture_spec(),
            Self::L768(net) => net.architecture_spec(),
            Self::L1024(net) => net.architecture_spec(),
        }
    }

    /// サポートするアーキテクチャ一覧
    pub fn supported_specs() -> Vec<ArchitectureSpec> {
        let mut specs = Vec::new();
        specs.extend_from_slice(HalfKPL256::SUPPORTED_SPECS);
        specs.extend_from_slice(HalfKPL512::SUPPORTED_SPECS);
        specs.extend_from_slice(HalfKPL768::SUPPORTED_SPECS);
        specs.extend_from_slice(HalfKPL1024::SUPPORTED_SPECS);
        specs
    }
}

/// HalfKP Accumulator スタック（L1 のみで決まる）
///
/// L2/L3/活性化の追加で変更不要。
pub enum HalfKPStack {
    L256(AccumulatorStackHalfKP<256>),
    L512(AccumulatorStackHalfKP<512>),
    L768(AccumulatorStackHalfKP<768>),
    L1024(AccumulatorStackHalfKP<1024>),
}

impl HalfKPStack {
    /// ネットワークに対応するスタックを生成
    ///
    /// バリアントマッチを使用し、新しい L1 追加時にコンパイル時に漏れ検知。
    pub fn from_network(net: &HalfKPNetwork) -> Self {
        match net {
            HalfKPNetwork::L256(_) => Self::L256(AccumulatorStackHalfKP::<256>::new()),
            HalfKPNetwork::L512(_) => Self::L512(AccumulatorStackHalfKP::<512>::new()),
            HalfKPNetwork::L768(_) => Self::L768(AccumulatorStackHalfKP::<768>::new()),
            HalfKPNetwork::L1024(_) => Self::L1024(AccumulatorStackHalfKP::<1024>::new()),
        }
    }

    /// L1 サイズを取得
    pub fn l1_size(&self) -> usize {
        match self {
            Self::L256(_) => 256,
            Self::L512(_) => 512,
            Self::L768(_) => 768,
            Self::L1024(_) => 1024,
        }
    }

    /// スタックをリセット
    pub fn reset(&mut self) {
        match self {
            Self::L256(s) => s.reset(),
            Self::L512(s) => s.reset(),
            Self::L768(s) => s.reset(),
            Self::L1024(s) => s.reset(),
        }
    }

    /// ply を進める
    pub fn push(&mut self, dirty: DirtyPiece) {
        match self {
            Self::L256(s) => s.push(dirty),
            Self::L512(s) => s.push(dirty),
            Self::L768(s) => s.push(dirty),
            Self::L1024(s) => s.push(dirty),
        }
    }

    /// ply を戻す
    pub fn pop(&mut self) {
        match self {
            Self::L256(s) => s.pop(),
            Self::L512(s) => s.pop(),
            Self::L768(s) => s.pop(),
            Self::L1024(s) => s.pop(),
        }
    }

    /// 現在のインデックスを取得
    pub fn current_index(&self) -> usize {
        match self {
            Self::L256(s) => s.current_index(),
            Self::L512(s) => s.current_index(),
            Self::L768(s) => s.current_index(),
            Self::L1024(s) => s.current_index(),
        }
    }

    /// 祖先を辿って使用可能なアキュムレータを探す
    pub fn find_usable_accumulator(&self) -> Option<(usize, usize)> {
        match self {
            Self::L256(s) => s.find_usable_accumulator(),
            Self::L512(s) => s.find_usable_accumulator(),
            Self::L768(s) => s.find_usable_accumulator(),
            Self::L1024(s) => s.find_usable_accumulator(),
        }
    }

    /// 現在のアキュムレータが計算済みかどうか
    #[inline]
    pub fn is_current_computed(&self) -> bool {
        match self {
            Self::L256(s) => s.current().accumulator.computed_accumulation,
            Self::L512(s) => s.current().accumulator.computed_accumulation,
            Self::L768(s) => s.current().accumulator.computed_accumulation,
            Self::L1024(s) => s.current().accumulator.computed_accumulation,
        }
    }

    /// 現在のエントリの previous インデックス
    #[inline]
    pub fn current_previous(&self) -> Option<usize> {
        match self {
            Self::L256(s) => s.current().previous,
            Self::L512(s) => s.current().previous,
            Self::L768(s) => s.current().previous,
            Self::L1024(s) => s.current().previous,
        }
    }

    /// 指定インデックスのエントリが計算済みかどうか
    #[inline]
    pub fn is_entry_computed(&self, idx: usize) -> bool {
        match self {
            Self::L256(s) => s.entry_at(idx).accumulator.computed_accumulation,
            Self::L512(s) => s.entry_at(idx).accumulator.computed_accumulation,
            Self::L768(s) => s.entry_at(idx).accumulator.computed_accumulation,
            Self::L1024(s) => s.entry_at(idx).accumulator.computed_accumulation,
        }
    }

    /// 現在のエントリの dirty piece を取得
    #[inline]
    pub fn current_dirty_piece(&self) -> DirtyPiece {
        match self {
            Self::L256(s) => s.current().dirty_piece,
            Self::L512(s) => s.current().dirty_piece,
            Self::L768(s) => s.current().dirty_piece,
            Self::L1024(s) => s.current().dirty_piece,
        }
    }
}

impl Default for HalfKPStack {
    fn default() -> Self {
        Self::L256(AccumulatorStackHalfKP::<256>::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nnue::spec::FeatureSet;

    #[test]
    fn test_halfkp_stack_from_network_l1_size() {
        let stack = HalfKPStack::L256(AccumulatorStackHalfKP::<256>::new());
        assert_eq!(stack.l1_size(), 256);

        let stack = HalfKPStack::L512(AccumulatorStackHalfKP::<512>::new());
        assert_eq!(stack.l1_size(), 512);

        let stack = HalfKPStack::L1024(AccumulatorStackHalfKP::<1024>::new());
        assert_eq!(stack.l1_size(), 1024);
    }

    #[test]
    fn test_supported_specs_combined() {
        let specs = HalfKPNetwork::supported_specs();
        // 256: 1, 512: 3, 768: 1, 1024: 2
        assert_eq!(specs.len(), 7);

        // 全て HalfKP
        for spec in &specs {
            assert_eq!(spec.feature_set, FeatureSet::HalfKP);
        }
    }

    /// push/pop の対称性と状態の一貫性テスト（L256）
    #[test]
    fn test_push_pop_index_consistency_l256() {
        let mut stack = HalfKPStack::L256(AccumulatorStackHalfKP::<256>::new());
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
        let mut stack = HalfKPStack::L512(AccumulatorStackHalfKP::<512>::new());
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
        let mut stack = HalfKPStack::L1024(AccumulatorStackHalfKP::<1024>::new());
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
        let mut stack = HalfKPStack::default();
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
        for spec in HalfKPNetwork::supported_specs() {
            assert_eq!(spec.feature_set, FeatureSet::HalfKP);
            assert!(spec.l1 == 256 || spec.l1 == 512 || spec.l1 == 768 || spec.l1 == 1024);
            assert!(spec.l2 > 0 && spec.l2 <= 128);
            assert!(spec.l3 > 0 && spec.l3 <= 128);
        }
    }
}
