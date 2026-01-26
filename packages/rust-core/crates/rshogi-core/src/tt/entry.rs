//! 置換表エントリー
//!
//! TTEntry: 10バイトのコンパクトなエントリ構造
//! TTData: 読み取り用のデータ構造

use super::{GENERATION_CYCLE, GENERATION_MASK};
use crate::types::{Bound, Move, Value, DEPTH_ENTRY_OFFSET};

/// 置換表エントリー
/// メモリ効率のため、フィールドを詰め込む（10バイト）
#[derive(Clone, Copy, Default)]
#[repr(C, packed)]
pub struct TTEntry {
    /// ハッシュキーの一部（衝突検出用）
    key16: u16,
    /// 探索深さ（DEPTH_OFFSETを引いた値）
    depth8: u8,
    /// generation(5bit) | pv(1bit) | bound(2bit)
    gen_bound8: u8,
    /// 最善手（16bit形式）
    move16: u16,
    /// 探索値
    value16: i16,
    /// 評価値
    eval16: i16,
}

// エントリサイズが10バイトであることを保証
const _: () = assert!(std::mem::size_of::<TTEntry>() == 10);

impl TTEntry {
    /// 新しい空のエントリを作成
    #[inline]
    pub const fn new() -> Self {
        Self {
            key16: 0,
            depth8: 0,
            gen_bound8: 0,
            move16: 0,
            value16: 0,
            eval16: 0,
        }
    }

    /// エントリが使用されているか
    #[inline]
    pub fn is_occupied(&self) -> bool {
        self.depth8 != 0
    }

    /// キーを取得
    #[inline]
    pub fn key16(&self) -> u16 {
        self.key16
    }

    /// 深さを取得（DEPTH_ENTRY_OFFSETを加算）
    #[inline]
    pub fn depth(&self) -> i32 {
        self.depth8 as i32 + DEPTH_ENTRY_OFFSET
    }

    /// 保存されている生のdepth8を取得
    #[inline]
    pub fn depth8(&self) -> u8 {
        self.depth8
    }

    /// エントリを読み取る
    pub fn read(&self) -> TTData {
        let mv = Move::from_u16_checked(self.move16).unwrap_or(Move::NONE);
        TTData {
            mv,
            value: Value::new(self.value16 as i32),
            eval: Value::new(self.eval16 as i32),
            depth: self.depth8 as i32 + DEPTH_ENTRY_OFFSET,
            bound: Bound::from_u8(self.gen_bound8 & 0x3).unwrap_or(Bound::None),
            is_pv: (self.gen_bound8 & 0x4) != 0,
        }
    }

    /// エントリに保存
    ///
    /// # 引数が多い理由
    /// この関数は探索のホットパスで頻繁に呼ばれるため、
    /// 構造体にまとめるオーバーヘッドを避けて個別の引数として渡している。
    /// YaneuraOu/Stockfishの実装に準拠。
    pub fn save(
        &mut self,
        key16: u16,
        value: Value,
        is_pv: bool,
        bound: Bound,
        depth: i32,
        mv: Move,
        eval: Value,
        generation8: u8,
    ) {
        // 新しい手がない場合は古い手を保持
        if mv != Move::NONE || key16 != self.key16 {
            self.move16 = mv.to_u16();
        }

        // 上書き条件：
        // - BOUND_EXACT（確定値）
        // - 異なるキー
        // - より深い探索 or PVノード優先
        // - 古いエントリ
        let d8 = depth - DEPTH_ENTRY_OFFSET;
        if bound == Bound::Exact
            || key16 != self.key16
            || d8 + 2 * (is_pv as i32) > self.depth8 as i32 - 4
            || self.relative_age(generation8) != 0
        {
            debug_assert!(d8 > 0 && d8 < 256);

            self.key16 = key16;
            self.depth8 = d8 as u8;
            self.gen_bound8 = generation8 | ((is_pv as u8) << 2) | bound as u8;
            self.value16 = value.raw() as i16;
            self.eval16 = eval.raw() as i16;
        } else if self.depth8 as i32 + DEPTH_ENTRY_OFFSET >= 5
            && Bound::from_u8(self.gen_bound8 & 0x3) != Some(Bound::Exact)
        {
            // 浅い置換を防ぐため、EXACT以外の深い項目はわずかに劣化させる
            self.depth8 = self.depth8.saturating_sub(1);
        }
    }

    /// 相対的な世代（0 = 最新）
    #[inline]
    pub fn relative_age(&self, generation8: u8) -> u8 {
        let age = GENERATION_CYCLE
            .wrapping_add(generation8 as u16)
            .wrapping_sub(self.gen_bound8 as u16);
        (age & GENERATION_MASK) as u8
    }
}

/// 置換表から読み取ったデータ
#[derive(Clone, Copy, Debug)]
pub struct TTData {
    /// 最善手
    pub mv: Move,
    /// 探索値
    pub value: Value,
    /// 評価値
    pub eval: Value,
    /// 探索深さ
    pub depth: i32,
    /// 境界タイプ
    pub bound: Bound,
    /// PVノードかどうか
    pub is_pv: bool,
}

impl TTData {
    /// 空のデータ
    pub const EMPTY: Self = Self {
        mv: Move::NONE,
        value: Value::NONE,
        eval: Value::NONE,
        depth: DEPTH_ENTRY_OFFSET,
        bound: Bound::None,
        is_pv: false,
    };
}

impl Default for TTData {
    fn default() -> Self {
        Self::EMPTY
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{File, Rank, Square};

    #[test]
    fn test_tt_entry_new() {
        let entry = TTEntry::new();
        assert!(!entry.is_occupied());
        assert_eq!(entry.key16(), 0);
    }

    #[test]
    fn test_tt_entry_save_and_read() {
        let mut entry = TTEntry::new();

        let key = 0x1234u16;
        let value = Value::new(100);
        let eval = Value::new(-50);
        let depth = 10;
        let from = Square::new(File::File7, Rank::Rank7);
        let to = Square::new(File::File7, Rank::Rank6);
        let mv = Move::new_move(from, to, false);
        let bound = Bound::Exact;
        let is_pv = true;
        let gen = 8;

        entry.save(key, value, is_pv, bound, depth, mv, eval, gen);

        assert!(entry.is_occupied());
        assert_eq!(entry.key16(), key);

        let data = entry.read();
        assert_eq!(data.value.raw(), 100);
        assert_eq!(data.eval.raw(), -50);
        assert_eq!(data.depth, 10);
        assert_eq!(data.bound, Bound::Exact);
        assert!(data.is_pv);
    }

    #[test]
    fn test_tt_entry_relative_age() {
        let mut entry = TTEntry::new();
        entry.save(0, Value::ZERO, false, Bound::Lower, 10, Move::NONE, Value::ZERO, 8);

        // 同じ世代では0
        assert_eq!(entry.relative_age(8), 0);

        // 世代が進むと8刻みでageが増える（GENERATION_DELTA = 8）
        assert_eq!(entry.relative_age(16), 8);
    }

    #[test]
    fn test_tt_entry_decay_non_exact() {
        let mut entry = TTEntry::new();
        let key = 0x1234u16;

        // 深いLower境界を保存
        entry.save(key, Value::ZERO, false, Bound::Lower, 8, Move::NONE, Value::ZERO, 0);
        let depth_before = entry.depth8();

        // 同一世代・同一キー・浅いLowerを保存すると深さが1減衰する
        entry.save(key, Value::ZERO, false, Bound::Lower, 1, Move::NONE, Value::ZERO, 0);
        assert_eq!(entry.depth8(), depth_before - 1);
    }

    #[test]
    fn test_tt_data_empty() {
        let data = TTData::EMPTY;
        assert_eq!(data.mv, Move::NONE);
        assert_eq!(data.bound, Bound::None);
        assert!(!data.is_pv);
    }
}
