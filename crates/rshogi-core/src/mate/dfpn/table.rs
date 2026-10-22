//! df-pn 専用の置換表
//!
//! 通常探索の TT とは別物で、証明数・反証数・詰み手数・探索量を局面ごとに持つ。
//! 値は常に「攻め方が詰ませられるか」の向き (pn = 詰みの証明数) で保存する。
//! 経路に依存して決まった不詰はここに書かない (呼び出し側の責務)。

/// 証明数・反証数の無限大。確定 (pn == 0 または dn == 0) の相方にだけ使う。
pub(super) const PN_INF: u32 = u32::MAX;
/// 和を飽和させる上限。∞ と区別するため PN_INF より 1 小さい。
pub(super) const PN_MAX_FINITE: u32 = u32::MAX - 1;

/// 置換表に保存する 1 局面分の値
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct TableValue {
    pub pn: u32,
    pub dn: u32,
    /// 詰みのときの詰み手数 (この局面から終局までの手数)。詰み以外では 0。
    pub mate_len: u16,
    /// この局面を書き込んだ探索深さ (ルートからの手数) の最小値。
    /// 現在より浅い深さで書かれた値は循環を通っている可能性がある (TCA の判定に使う)。
    pub min_depth: u16,
}

#[derive(Clone, Copy, Default)]
struct Entry {
    /// `Position::key()`
    key: u64,
    pn: u32,
    dn: u32,
    /// この局面以下で探索したノード数。置き換えの優先度に使う。0 は空き。
    searched: u32,
    mate_len: u16,
    min_depth: u16,
    /// 書き込んだ探索の世代。表の世代と違うエントリは空きとみなす。
    generation: u8,
}

impl Entry {
    fn is_empty(&self, generation: u8) -> bool {
        self.searched == 0 || self.generation != generation
    }

    fn is_final(&self) -> bool {
        self.pn == 0 || self.dn == 0
    }

    /// 小さいほど先に追い出す。確定値は未確定値より常に残す。
    fn keep_priority(&self) -> u64 {
        let final_bonus = if self.is_final() { 1u64 << 32 } else { 0 };
        final_bonus | u64::from(self.searched)
    }
}

const BUCKET_SIZE: usize = 4;

/// キャッシュラインの境界にそろえた bucket (エントリ 32 byte × 4 = キャッシュライン 2 本)
#[derive(Clone, Copy, Default)]
#[repr(C, align(64))]
struct Bucket([Entry; BUCKET_SIZE]);

/// df-pn の置換表
pub(super) struct MateTable {
    buckets: Vec<Bucket>,
    /// 現在の世代。`clear` で進める (表全体を書き換えずに空にする)。
    generation: u8,
}

impl MateTable {
    /// `size_mb` MB 相当の bucket 数で確保する。最低 1 bucket。
    pub fn new(size_mb: usize) -> Self {
        let bytes = size_mb.saturating_mul(1024 * 1024);
        Self::with_bucket_count((bytes / std::mem::size_of::<Bucket>()).max(1))
    }

    fn with_bucket_count(bucket_count: usize) -> Self {
        Self {
            buckets: vec![Bucket::default(); bucket_count],
            generation: 1,
        }
    }

    /// bucket 数を直接指定する (置き換えが頻発する極小の表をテストで作る)
    #[cfg(test)]
    pub fn with_buckets(bucket_count: usize) -> Self {
        Self::with_bucket_count(bucket_count.max(1))
    }

    /// 全エントリを空にする。
    ///
    /// 世代を進めるだけで済ませる (小さな問題を繰り返し解くと、表全体の書き換えが
    /// 探索より重くなる)。世代が一周したときだけ実際に書き換える。
    pub fn clear(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            self.buckets.fill(Bucket::default());
            self.generation = 1;
        }
    }

    /// 局面 `key` の bucket をキャッシュに読み込ませておく。
    /// 探索時間の多くは置換表を引くときのキャッシュミスの待ちで、子の bucket をまとめて
    /// 先に読み込ませると待ちが重なる。
    #[inline]
    pub fn prefetch(&self, key: u64) {
        let ptr = &self.buckets[self.bucket_index(key)] as *const Bucket as *const i8;

        #[cfg(target_arch = "x86_64")]
        for line in 0..std::mem::size_of::<Bucket>() / 64 {
            // SAFETY: line * 64 < size_of::<Bucket>() なので ptr.add は self.buckets の生存中の
            // 要素の内側を指す。prefetch はメモリの内容を読み書きしない。
            unsafe {
                use std::arch::x86_64::{_MM_HINT_T0, _mm_prefetch};
                _mm_prefetch(ptr.add(line * 64), _MM_HINT_T0);
            }
        }

        #[cfg(not(target_arch = "x86_64"))]
        let _ = ptr;
    }

    fn bucket_index(&self, key: u64) -> usize {
        // 上位ビットで bucket を選ぶ (bucket 数が 2 の冪でなくても偏らない)
        ((u128::from(key) * self.buckets.len() as u128) >> 64) as usize
    }

    /// 局面の値を引く
    pub fn probe(&self, key: u64) -> Option<TableValue> {
        let generation = self.generation;
        self.buckets[self.bucket_index(key)]
            .0
            .iter()
            .find(|e| !e.is_empty(generation) && e.key == key)
            .map(|e| TableValue {
                pn: e.pn,
                dn: e.dn,
                mate_len: e.mate_len,
                min_depth: e.min_depth,
            })
    }

    /// 局面の値を書く。`searched` はこの局面以下の探索ノード数。
    /// `value.min_depth` は今回の探索深さで、既存エントリの最小深さと小さい方を残す。
    ///
    /// 同じ局面があれば上書きする。確定値を未確定値で上書きはしない
    /// (確定値は経路に依存しないものだけが書かれるので、後から覆らない)。
    /// bucket が残すべきエントリで埋まっていれば書かない (キャッシュの省略で、結論は変わらない)。
    pub fn store(&mut self, key: u64, value: TableValue, searched: u64) {
        let searched = u32::try_from(searched).unwrap_or(u32::MAX).max(1);
        let new_entry = Entry {
            key,
            pn: value.pn,
            dn: value.dn,
            searched,
            mate_len: value.mate_len,
            min_depth: value.min_depth,
            generation: self.generation,
        };
        let generation = self.generation;
        let index = self.bucket_index(key);
        let bucket = &mut self.buckets[index].0;

        if let Some(e) = bucket.iter_mut().find(|e| !e.is_empty(generation) && e.key == key) {
            if e.is_final() && !new_entry.is_final() {
                return;
            }
            let total = searched.saturating_add(e.searched);
            let min_depth = e.min_depth.min(new_entry.min_depth);
            // 詰み手数は短い証明を残す (詰み手順の復元は手数が減る向きにしか辿らない)
            let mate_len = if e.pn == 0 && new_entry.pn == 0 {
                e.mate_len.min(new_entry.mate_len)
            } else {
                new_entry.mate_len
            };
            *e = new_entry;
            e.searched = total;
            e.min_depth = min_depth;
            e.mate_len = mate_len;
            return;
        }

        if let Some(e) = bucket.iter_mut().find(|e| e.is_empty(generation)) {
            *e = new_entry;
            return;
        }

        let victim =
            bucket.iter_mut().min_by_key(|e| e.keep_priority()).expect("bucket は空でない");
        if victim.keep_priority() <= new_entry.keep_priority() {
            *victim = new_entry;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(pn: u32, dn: u32) -> TableValue {
        TableValue {
            pn,
            dn,
            mate_len: 0,
            min_depth: 0,
        }
    }

    #[test]
    fn probe_returns_stored_value() {
        let mut table = MateTable::new(1);
        assert_eq!(table.probe(42), None);
        table.store(42, value(3, 5), 10);
        assert_eq!(table.probe(42), Some(value(3, 5)));
        table.clear();
        assert_eq!(table.probe(42), None);
    }

    #[test]
    fn clear_hides_old_entries_across_generation_wrap() {
        let mut table = MateTable::new(1);
        for round in 0..600u64 {
            assert_eq!(table.probe(42), None, "round {round}");
            table.store(42, value(3, 5), 10);
            assert_eq!(table.probe(42), Some(value(3, 5)));
            table.clear();
        }
    }

    #[test]
    fn final_value_is_not_overwritten_by_unknown() {
        let mut table = MateTable::new(1);
        let proven = TableValue {
            pn: 0,
            dn: PN_INF,
            mate_len: 3,
            min_depth: 0,
        };
        table.store(7, proven, 1);
        table.store(7, value(2, 2), 100);
        assert_eq!(table.probe(7), Some(proven));
    }

    #[test]
    fn full_bucket_keeps_final_entries() {
        // 1 bucket だけの表で、確定値が未確定値に追い出されないことを確かめる
        let mut table = MateTable::with_buckets(1);
        let proven = TableValue {
            pn: 0,
            dn: PN_INF,
            mate_len: 1,
            min_depth: 0,
        };
        table.store(1, proven, 1);
        for key in 2..=10u64 {
            table.store(key, value(1, 1), 1_000);
        }
        assert_eq!(table.probe(1), Some(proven));
    }
}
