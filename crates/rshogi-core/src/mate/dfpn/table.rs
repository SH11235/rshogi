//! df-pn 専用の置換表
//!
//! 通常探索の TT とは別物で、証明数・反証数・詰み手数・探索量を局面ごとに持つ。
//! 値は常に「攻め方が詰ませられるか」の向き (pn = 詰みの証明数) で保存する。
//! 経路に依存して決まった不詰はここに書かない (呼び出し側の責務)。
//!
//! 局面は [`TableKey`] で表す。表には 2 つの使い方があり、メソッドの `PIECES` で選ぶ。
//! - `PIECES = false`: 局面全体のキーが一致するエントリだけを使う。
//! - `PIECES = true`: 加えて、確定値を (盤面と手番のキー, 証明駒・反証駒) で書き、
//!   持駒違いの局面にも流用する。
//!   1 回の solve の中では駒の総数が変わらないので、盤面が同じなら攻め方の持駒が多いほど
//!   受け方の持駒は少ない。攻め方の手は増え、受け方の手は減るので、確定値は次の向きに流用できる:
//!   攻め方の持駒がより少なくて詰む → 詰み、攻め方の持駒がより多くて詰まない → 不詰。
//!   確定値の表 ([`MateTable::new`] の `proof_pieces`) が要る。
//!
//! 局面全体のキーのエントリはどちらの使い方でも同じ意味なので、`PIECES = false` で使った表を
//! そのまま `PIECES = true` で使い続けてよい。

use crate::types::{Hand, PieceType};

/// 証明数・反証数の無限大。確定 (pn == 0 または dn == 0) の相方にだけ使う。
pub(super) const PN_INF: u32 = u32::MAX;
/// 和を飽和させる上限。∞ と区別するため PN_INF より 1 小さい。
pub(super) const PN_MAX_FINITE: u32 = u32::MAX - 1;

/// 置換表に保存する 1 局面分の値
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct TableValue {
    pub pn: u32,
    pub dn: u32,
    /// 詰みのときの詰み手数 (この局面から終局までの手数) の上限。詰み以外では 0。
    /// 持駒の多い局面に流用した詰みでは、実際の詰みはこれ以下の手数になる。
    pub mate_len: u16,
    /// この局面を書き込んだ探索深さ (ルートからの手数) の最小値。
    /// 現在より浅い深さで書かれた値は循環を通っている可能性がある (TCA の判定に使う)。
    /// 確定値の表から引いた値では 0。
    pub min_depth: u16,
    /// 確定値の根拠の攻め方の持駒。詰みは証明駒 (これ以上持っていれば詰む)、
    /// 不詰は反証駒 (これ以下しか持っていなければ詰まない)。完全一致のエントリでは局面の持駒。
    pub hand: Hand,
}

/// 置換表での局面の識別子
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct TableKey {
    /// 局面全体のキー (盤面・手番・持駒)
    pub key: u64,
    /// 盤面と手番のキー (証明駒を使わないときは使わない)
    pub board: u64,
    /// 攻め方の持駒 (証明駒を使わないときは使わない)
    pub hand: Hand,
}

/// 持駒を、駒種ごとの枚数の上にガードビットを 1 つずつ置いて詰めたもの。
/// 駒種ごとの大小比較が 1 回の引き算で済む (`covers`)。
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct PackedHand(u32);

impl PackedHand {
    /// (駒種, 枚数の最下位ビットの位置, 枚数のビット幅)。ガードビットは枚数のすぐ上。
    const FIELDS: [(PieceType, u32, u32); 7] = [
        (PieceType::Pawn, 0, 5),
        (PieceType::Lance, 6, 3),
        (PieceType::Knight, 10, 3),
        (PieceType::Silver, 14, 3),
        (PieceType::Gold, 18, 3),
        (PieceType::Bishop, 22, 2),
        (PieceType::Rook, 25, 2),
    ];
    const GUARD: u32 = {
        let mut guard = 0;
        let mut i = 0;
        while i < Self::FIELDS.len() {
            let (_, shift, width) = Self::FIELDS[i];
            guard |= 1 << (shift + width);
            i += 1;
        }
        guard
    };

    #[inline]
    fn new(hand: Hand) -> Self {
        let mut packed = 0;
        for (pt, shift, _) in Self::FIELDS {
            packed |= hand.count(pt) << shift;
        }
        Self(packed)
    }

    fn unpack(self) -> Hand {
        let mut hand = Hand::EMPTY;
        for (pt, shift, width) in Self::FIELDS {
            hand = hand.set(pt, (self.0 >> shift) & ((1 << width) - 1));
        }
        hand
    }

    /// すべての駒種で `self` の枚数が `other` 以上か。
    /// ガードビットを立てて引くと、枚数が足りない駒種だけガードビットが借りで消える。
    #[inline]
    fn covers(self, other: Self) -> bool {
        ((self.0 | Self::GUARD) - other.0) & Self::GUARD == Self::GUARD
    }
}

/// 局面全体のキーが一致するときだけ使うエントリ (`PIECES = true` では未確定値だけを書く)。
/// キャッシュライン 1 本に 3 つ入るよう 20 byte に詰める。
///
/// 確定値は pn か dn の片方が必ず ∞ で、もう片方は 0 なので、2 つの欄にキーの上位 32bit と詰み手数を入れ、
/// キーは 64bit 全部を照合する (別の局面の詰み・不詰を採用すると結論を誤る)。未確定値はキーの下位 55bit だけを
/// 照合する。取り違えても探索の効率が落ちるだけで、結論は子の確定値からしか決まらない。
#[derive(Clone, Copy, Default)]
struct Entry {
    /// 局面全体のキーの下位 32bit
    key_lo: u32,
    /// 未確定値では pn、確定値ではキーの上位 32bit
    pn_or_key_hi: u32,
    /// 未確定値では dn、確定値では詰み手数 (不詰では 0)
    dn_or_mate_len: u32,
    /// 下位から順に、探索量 (`SEARCHED`)、世代 (`GENERATION`)、最小の探索深さ (`MIN_DEPTH`)、
    /// 確定値か (`FINAL`)、詰みか (`PROVEN`)、キーの 32〜54bit (`TAG`)。
    /// u64 にすると 8 byte 境界になるので 2 つに分ける。
    meta: [u32; 2],
}

/// `Entry::meta` の欄 (最下位ビットの位置, ビット幅)
type Field = (u32, u32);
/// この局面以下で探索したノード数。飽和する。置き換えの優先度に使い、0 は空き。
const SEARCHED: Field = (0, 20);
/// 書き込んだ探索の世代。表の世代と違うエントリは空きとみなす。
const GENERATION: Field = (20, 8);
const MIN_DEPTH: Field = (28, 11);
const FINAL: Field = (39, 1);
const PROVEN: Field = (40, 1);
const TAG: Field = (41, 23);
/// 書き込む探索深さは探索深さの上限 (`search::MAX_DEPTH`) 未満
const _: () = assert!(super::search::MAX_DEPTH <= 1 << MIN_DEPTH.1);

const fn field_max(field: Field) -> u64 {
    (1 << field.1) - 1
}

impl Entry {
    fn new(key: u64, value: &TableValue, searched: u32, generation: u8) -> Self {
        let is_final = value.pn == 0 || value.dn == 0;
        let mut e = Self {
            key_lo: key as u32,
            pn_or_key_hi: if is_final { key_hi(key) } else { value.pn },
            dn_or_mate_len: if is_final { 0 } else { value.dn },
            meta: [0; 2],
        };
        e.set(TAG, key_tag(key));
        e.set(FINAL, u64::from(is_final));
        e.set(PROVEN, u64::from(value.pn == 0));
        e.set_searched(searched, generation);
        e.set_lengths(value.mate_len, value.min_depth);
        e
    }

    fn meta(&self) -> u64 {
        u64::from(self.meta[0]) | u64::from(self.meta[1]) << 32
    }

    fn get(&self, field: Field) -> u64 {
        self.meta() >> field.0 & field_max(field)
    }

    /// `value` は欄の最大値で飽和させる
    fn set(&mut self, field: Field, value: u64) {
        let mask = field_max(field) << field.0;
        let meta = (self.meta() & !mask) | value.min(field_max(field)) << field.0;
        self.meta = [meta as u32, (meta >> 32) as u32];
    }

    fn pn_dn(&self) -> (u32, u32) {
        match (self.is_final(), self.is_proven()) {
            (false, _) => (self.pn_or_key_hi, self.dn_or_mate_len),
            (true, true) => (0, PN_INF),
            (true, false) => (PN_INF, 0),
        }
    }

    fn searched(&self) -> u32 {
        self.get(SEARCHED) as u32
    }

    fn set_searched(&mut self, searched: u32, generation: u8) {
        self.set(SEARCHED, u64::from(searched));
        self.set(GENERATION, u64::from(generation));
    }

    fn mate_len(&self) -> u16 {
        if self.is_proven() {
            self.dn_or_mate_len as u16
        } else {
            0
        }
    }

    fn min_depth(&self) -> u16 {
        self.get(MIN_DEPTH) as u16
    }

    /// 詰み手数は詰みのエントリにだけ書く (ほかでは 0 として扱う)
    fn set_lengths(&mut self, mate_len: u16, min_depth: u16) {
        if self.is_proven() {
            self.dn_or_mate_len = u32::from(mate_len);
        }
        self.set(MIN_DEPTH, u64::from(min_depth));
    }

    fn is_empty(&self, generation: u8) -> bool {
        self.searched() == 0 || self.get(GENERATION) != u64::from(generation)
    }

    fn is_final(&self) -> bool {
        self.get(FINAL) != 0
    }

    fn is_proven(&self) -> bool {
        self.is_final() && self.get(PROVEN) != 0
    }

    fn matches(&self, key: TableKey, generation: u8) -> bool {
        self.key_lo == key.key as u32
            && self.get(TAG) == key_tag(key.key)
            && (!self.is_final() || self.pn_or_key_hi == key_hi(key.key))
            && !self.is_empty(generation)
    }

    /// 小さいほど先に追い出す。bucket に未確定値があれば、確定値より先に追い出す。
    fn keep_priority(&self) -> u64 {
        let final_bonus = if self.is_final() { 1u64 << 32 } else { 0 };
        final_bonus | u64::from(self.searched())
    }
}

#[inline]
fn key_hi(key: u64) -> u32 {
    (key >> 32) as u32
}

/// キーの 32〜54bit (`Entry` で下位 32bit に足して照合する)
#[inline]
fn key_tag(key: u64) -> u64 {
    key >> 32 & field_max(TAG)
}

/// 証明駒・反証駒で書く確定値のエントリ。キャッシュライン 1 本に 3 つ入るよう 4 byte 境界で詰める。
#[derive(Clone, Copy, Default)]
struct FinalEntry {
    /// 盤面と手番のキーの下位・上位 32bit
    board_lo: u32,
    board_hi: u32,
    /// 詰みは証明駒、不詰は反証駒
    hand: PackedHand,
    /// この局面以下で探索したノード数。置き換えの優先度に使う。0 は空き。
    searched: u32,
    /// 詰みの手数の上限 (不詰では 0)
    mate_len: u16,
    proven: bool,
    generation: u8,
}

impl FinalEntry {
    fn is_empty(&self, generation: u8) -> bool {
        self.searched == 0 || self.generation != generation
    }

    fn has_board(&self, board: u64) -> bool {
        self.board_lo == board as u32 && self.board_hi == (board >> 32) as u32
    }

    fn value(&self) -> TableValue {
        let (pn, dn) = if self.proven {
            (0, PN_INF)
        } else {
            (PN_INF, 0)
        };
        TableValue {
            pn,
            dn,
            mate_len: self.mate_len,
            min_depth: 0,
            hand: self.hand.unpack(),
        }
    }
}

const BUCKET_SIZE: usize = 3;
const FINAL_BUCKET_SIZE: usize = 3;

/// 局面全体のキーの bucket (エントリ 20 byte × 3 = キャッシュライン 1 本)
#[derive(Clone, Copy, Default)]
#[repr(C, align(64))]
struct Bucket([Entry; BUCKET_SIZE]);
const _: () = assert!(std::mem::size_of::<Bucket>() == 64);

/// 証明駒・反証駒の確定値の bucket (エントリ 20 byte × 3 = キャッシュライン 1 本)
#[derive(Clone, Copy, Default)]
#[repr(C, align(64))]
struct FinalBucket([FinalEntry; FINAL_BUCKET_SIZE]);

/// df-pn の置換表
///
/// 完全一致のエントリは局面 (盤面と持駒) で bucket を選ぶ。証明駒・反証駒の確定値は持駒違いの局面からも
/// 引けるよう、盤面だけで bucket を選ぶ別の表に置く。1 つの表に同じ盤面の持駒違いを束ねると、
/// 未確定値が 1 つの bucket に集まって追い出し合う。`PIECES = true` で引くと子の局面ごとに
/// キャッシュライン 2 本 (`false` は 1 本) を読む。
pub(super) struct MateTable {
    buckets: Vec<Bucket>,
    /// 証明駒・反証駒の確定値。`buckets` と同じ数か、作らないときは空。
    final_buckets: Vec<FinalBucket>,
    /// 現在の世代。`clear` で進める (表全体を書き換えずに空にする)。
    generation: u8,
    /// 持駒違いの局面に確定値を流用するか (差分検査のためにテストからだけ切る)
    superiority: bool,
}

impl MateTable {
    /// `size_mb` MB 相当の bucket 数で確保する。最低 1 bucket。
    /// `proof_pieces` なら確定値の表も (同じ予算の中に) 作り、`PIECES = true` で使えるようにする。
    pub fn new(size_mb: usize, proof_pieces: bool) -> Self {
        let bytes = size_mb.saturating_mul(1024 * 1024);
        let per_bucket = std::mem::size_of::<Bucket>()
            + if proof_pieces {
                std::mem::size_of::<FinalBucket>()
            } else {
                0
            };
        Self::with_bucket_count((bytes / per_bucket).clamp(1, u32::MAX as usize), proof_pieces)
    }

    fn with_bucket_count(bucket_count: usize, proof_pieces: bool) -> Self {
        Self {
            buckets: vec![Bucket::default(); bucket_count],
            final_buckets: vec![
                FinalBucket::default();
                if proof_pieces { bucket_count } else { 0 }
            ],
            generation: 1,
            superiority: true,
        }
    }

    /// bucket 数を直接指定する (置き換えが頻発する極小の表をテストで作る)。確定値の表も作る。
    #[cfg(test)]
    pub fn with_buckets(bucket_count: usize) -> Self {
        Self::with_bucket_count(bucket_count.max(1), true)
    }

    /// 持駒違いの局面への流用を止める (流用ありの版との差分検査用)
    #[cfg(test)]
    pub fn without_superiority(mut self) -> Self {
        self.superiority = false;
        self
    }

    /// 確定値の表を持つか (`PIECES = true` で使えるか)
    pub fn has_proof_pieces(&self) -> bool {
        !self.final_buckets.is_empty()
    }

    /// 全エントリを空にする。
    ///
    /// 世代を進めるだけで済ませる (小さな問題を繰り返し解くと、表全体の書き換えが
    /// 探索より重くなる)。世代が一周したときだけ実際に書き換える。
    pub fn clear(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            self.buckets.fill(Bucket::default());
            self.final_buckets.fill(FinalBucket::default());
            self.generation = 1;
        }
    }

    fn index_of(&self, hash: u64) -> usize {
        // 上位 32bit で bucket を選ぶ (bucket 数が 2 の冪でなくても偏らない)。bucket 数は 2^32 未満
        // (`new` で抑える) なので 64bit の掛け算で済む (wasm には 128bit の掛け算が無い)。
        // 2 つの表は bucket 数が同じ
        (((hash >> 32) * self.buckets.len() as u64) >> 32) as usize
    }

    fn exact_index(&self, key: TableKey) -> usize {
        self.index_of(key.key)
    }

    fn final_index(&self, key: TableKey) -> usize {
        self.index_of(key.board)
    }

    /// 局面 `key` を引くときに読む bucket をキャッシュに読み込ませておく。
    /// 探索時間の多くは置換表を引くときのキャッシュミスの待ちで、子の bucket をまとめて
    /// 先に読み込ませると待ちが重なる。
    #[cfg(target_arch = "x86_64")]
    #[inline]
    pub fn prefetch<const PIECES: bool>(&self, key: TableKey) {
        use std::arch::x86_64::{_MM_HINT_T0, _mm_prefetch};
        let exact = &self.buckets[self.exact_index(key)] as *const Bucket as *const i8;
        // SAFETY: exact は self.buckets の生存中の要素を指す。prefetch はメモリの内容を読み書きしない。
        unsafe { _mm_prefetch(exact, _MM_HINT_T0) };
        if PIECES {
            let fin = &self.final_buckets[self.final_index(key)] as *const FinalBucket as *const i8;
            // SAFETY: fin は self.final_buckets の生存中の要素を指す。prefetch は内容を読み書きしない。
            unsafe { _mm_prefetch(fin, _MM_HINT_T0) };
        }
    }

    /// prefetch の無いアーキテクチャでは何もしない (bucket の位置の計算も省く)
    #[cfg(not(target_arch = "x86_64"))]
    #[inline]
    pub fn prefetch<const PIECES: bool>(&self, _key: TableKey) {}

    /// 局面の値を引く。
    ///
    /// `PIECES` なら確定値を持駒違いの局面からも流用する (攻め方の持駒が今以下で詰む /
    /// 今以上で詰まない)。それ以外は同じ局面のものだけを返す。
    pub fn probe<const PIECES: bool>(&self, key: TableKey) -> Option<TableValue> {
        let generation = self.generation;
        if PIECES {
            let hand = PackedHand::new(key.hand);
            let mut proven: Option<&FinalEntry> = None;
            let mut disproven = None;
            for e in &self.final_buckets[self.final_index(key)].0 {
                if !e.has_board(key.board) || e.is_empty(generation) {
                    continue;
                }
                if !(self.superiority || e.hand == hand) {
                    continue;
                }
                if e.proven {
                    if hand.covers(e.hand) && proven.is_none_or(|p| e.mate_len < p.mate_len) {
                        proven = Some(e);
                    }
                } else if e.hand.covers(hand) {
                    disproven = Some(e);
                }
            }
            if let Some(e) = proven.or(disproven) {
                return Some(e.value());
            }
        }
        self.buckets[self.exact_index(key)]
            .0
            .iter()
            .find(|e| e.matches(key, generation))
            .map(|e| TableValue {
                pn: e.pn_dn().0,
                dn: e.pn_dn().1,
                mate_len: e.mate_len(),
                min_depth: e.min_depth(),
                hand: key.hand,
            })
    }

    /// 同じ局面 (持駒も同じ) の完全一致のエントリがあるか
    #[cfg(test)]
    pub fn contains_exact(&self, key: TableKey) -> bool {
        self.buckets[self.exact_index(key)]
            .0
            .iter()
            .any(|e| e.matches(key, self.generation))
    }

    /// 局面 `key` の値を書く。`searched` はこの局面以下の探索ノード数。
    /// `value.min_depth` は今回の探索深さで、既存エントリの最小深さと小さい方を残す。
    ///
    /// `PIECES` なら確定値は `value.hand` (証明駒・反証駒) の持駒で確定値の表に書き (bucket が埋まっていれば
    /// 探索量の最も少ない確定値を置き換える)、同じ局面の完全一致のエントリは消す。それ以外は完全一致のエントリに書く。
    /// 同じ局面の確定値を未確定値で上書きはしない (確定値は経路に依存しないものだけが書かれるので、後から覆らない)。
    /// 完全一致の表に新しい局面を書くとき、bucket が埋まっていれば残す優先度の最も低いエントリを置き換える
    /// (確定値で埋まっていれば確定値も。表はキャッシュなので結論は変わらず、読み直しが増えるだけ)。
    /// 新しいエントリの探索量の方が少なくても書く: 探索量は読み直すたびに増えるだけなので、比べて書かないでいると、前に読んで今は
    /// 使わない大きなエントリが居座り、今読んでいる局面の値を書けずに同じ局面を読み直し続ける
    /// (表が小さいほど起こる)。
    pub fn store<const PIECES: bool>(&mut self, key: TableKey, value: TableValue, searched: u64) {
        let searched = u32::try_from(searched).unwrap_or(u32::MAX).max(1);
        let generation = self.generation;
        let is_final = value.pn == 0 || value.dn == 0;
        let exact_index = self.exact_index(key);
        let bucket = &mut self.buckets[exact_index].0;
        let same = bucket.iter_mut().find(|e| e.matches(key, generation));

        if PIECES && is_final {
            // 確定したので同じ局面の完全一致のエントリは要らない (引くときは確定値の表を先に見る)
            let mut searched = searched;
            if let Some(e) = same {
                searched = searched.saturating_add(e.searched());
                e.set_searched(0, generation);
            }
            self.store_final(key, value, searched);
            return;
        }

        let new_entry = Entry::new(key.key, &value, searched, generation);
        if let Some(e) = same {
            if e.is_final() && !is_final {
                return;
            }
            let total = searched.saturating_add(e.searched());
            let min_depth = e.min_depth().min(value.min_depth);
            // 詰み手数は短い証明を残す (詰み手順の復元は手数が減る向きにしか辿らない)
            let mate_len = if e.is_proven() && value.pn == 0 {
                e.mate_len().min(value.mate_len)
            } else {
                value.mate_len
            };
            *e = new_entry;
            e.set_searched(total, generation);
            e.set_lengths(mate_len, min_depth);
            return;
        }
        if let Some(e) = bucket.iter_mut().find(|e| e.is_empty(generation)) {
            *e = new_entry;
            return;
        }
        *bucket.iter_mut().min_by_key(|e| e.keep_priority()).expect("bucket は空でない") =
            new_entry;
    }

    fn store_final(&mut self, key: TableKey, value: TableValue, searched: u32) {
        let generation = self.generation;
        let hand = PackedHand::new(if self.superiority {
            value.hand
        } else {
            key.hand
        });
        let proven = value.pn == 0;
        let final_index = self.final_index(key);
        let bucket = &mut self.final_buckets[final_index].0;
        // 同じ持駒の条件の同じ確定値があればまとめる。詰み手数は短い証明を残す
        if let Some(e) = bucket.iter_mut().find(|e| {
            e.has_board(key.board)
                && e.hand == hand
                && e.proven == proven
                && !e.is_empty(generation)
        }) {
            e.searched = e.searched.saturating_add(searched);
            e.mate_len = e.mate_len.min(value.mate_len);
            return;
        }
        let new_entry = FinalEntry {
            board_lo: key.board as u32,
            board_hi: (key.board >> 32) as u32,
            hand,
            searched,
            mate_len: value.mate_len,
            proven,
            generation,
        };
        if let Some(e) = bucket.iter_mut().find(|e| e.is_empty(generation)) {
            *e = new_entry;
            return;
        }
        // 探索量が少なくても必ず書く。書かずに捨てると、同じ局面の完全一致のエントリは消してあるので
        // 結果がどこにも残らず、同じ局面を証明し直し続けて探索が止まらないことがある
        *bucket.iter_mut().min_by_key(|e| e.searched).expect("bucket は空でない") = new_entry;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::PieceType;

    fn value(pn: u32, dn: u32) -> TableValue {
        TableValue {
            pn,
            dn,
            mate_len: 0,
            min_depth: 0,
            hand: Hand::EMPTY,
        }
    }

    fn proven(mate_len: u16, hand: Hand) -> TableValue {
        TableValue {
            pn: 0,
            dn: PN_INF,
            mate_len,
            min_depth: 0,
            hand,
        }
    }

    fn disproven(hand: Hand) -> TableValue {
        TableValue {
            pn: PN_INF,
            dn: 0,
            mate_len: 0,
            min_depth: 0,
            hand,
        }
    }

    fn key(board: u64, hand: Hand) -> TableKey {
        TableKey {
            key: board ^ u64::from(hand.raw()).wrapping_mul(0x9E37_79B9_7F4A_7C15),
            board,
            hand,
        }
    }

    fn hand(pieces: &[PieceType]) -> Hand {
        pieces.iter().fold(Hand::EMPTY, |h, &pt| h.add(pt))
    }

    #[test]
    fn packed_hand_covers_matches_piece_by_piece_comparison() {
        let max = [18u32, 4, 4, 4, 4, 2, 2];
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut random_hand = || {
            let mut h = Hand::EMPTY;
            for (i, &pt) in PieceType::HAND_PIECES.iter().enumerate() {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                h = h.set(pt, (state % u64::from(max[i] + 1)) as u32);
            }
            h
        };
        for _ in 0..100_000 {
            let (a, b) = (random_hand(), random_hand());
            assert_eq!(PackedHand::new(a).covers(PackedHand::new(b)), a.is_superior_or_equal(b));
            assert!(PackedHand::new(a).covers(PackedHand::new(a)));
        }
    }

    #[test]
    fn probe_returns_stored_value() {
        let mut table = MateTable::new(1, true);
        let k = key(42, Hand::EMPTY);
        assert_eq!(table.probe::<true>(k), None);
        table.store::<true>(k, value(3, 5), 10);
        assert_eq!(table.probe::<true>(k), Some(value(3, 5)));
        assert!(table.contains_exact(k));
        table.clear();
        assert_eq!(table.probe::<true>(k), None);
    }

    #[test]
    fn clear_hides_old_entries_across_generation_wrap() {
        let mut table = MateTable::new(1, true);
        let k = key(42, Hand::EMPTY);
        for round in 0..600u64 {
            assert_eq!(table.probe::<true>(k), None, "round {round}");
            table.store::<true>(k, value(3, 5), 10);
            assert_eq!(table.probe::<true>(k), Some(value(3, 5)));
            table.clear();
        }
    }

    #[test]
    fn final_value_is_not_overwritten_by_unknown() {
        let mut table = MateTable::new(1, true);
        let k = key(7, Hand::EMPTY);
        table.store::<true>(k, proven(3, Hand::EMPTY), 1);
        table.store::<true>(k, value(2, 2), 100);
        assert_eq!(table.probe::<true>(k), Some(proven(3, Hand::EMPTY)));
    }

    #[test]
    fn full_bucket_keeps_final_entries() {
        // 1 bucket だけの表で、確定値が未確定値に追い出されないことを確かめる
        let mut table = MateTable::with_buckets(1);
        table.store::<true>(key(1, Hand::EMPTY), proven(1, Hand::EMPTY), 1);
        for board in 2..=10u64 {
            table.store::<true>(key(board, Hand::EMPTY), value(1, 1), 1_000);
        }
        assert_eq!(table.probe::<true>(key(1, Hand::EMPTY)), Some(proven(1, Hand::EMPTY)));
    }

    #[test]
    fn full_bucket_takes_the_new_entry_even_if_it_searched_less() {
        // 探索量の最も少ない未確定値 (局面 2) が置き換わり、ほかは残る
        let mut table = MateTable::with_buckets(1);
        for (board, searched) in [(1u64, 300), (2, 100), (3, 400)] {
            table.store::<true>(key(board, Hand::EMPTY), value(1, 1), searched);
        }
        table.store::<true>(key(5, Hand::EMPTY), value(2, 2), 1);
        assert_eq!(table.probe::<true>(key(5, Hand::EMPTY)), Some(value(2, 2)));
        assert_eq!(table.probe::<true>(key(2, Hand::EMPTY)), None);
        for board in [1u64, 3] {
            assert_eq!(table.probe::<true>(key(board, Hand::EMPTY)), Some(value(1, 1)));
        }
    }

    #[test]
    fn bucket_full_of_final_values_takes_an_unknown_value() {
        // 完全一致の表が確定値で埋まっていれば、探索量の最も少ない確定値が未確定値に置き換わる
        let mut table = MateTable::with_bucket_count(1, false);
        for (board, searched) in [(1u64, 300), (2, 100), (3, 400)] {
            table.store::<false>(key(board, Hand::EMPTY), proven(1, Hand::EMPTY), searched);
        }
        table.store::<false>(key(5, Hand::EMPTY), value(2, 2), 1);
        assert_eq!(table.probe::<false>(key(5, Hand::EMPTY)), Some(value(2, 2)));
        assert_eq!(table.probe::<false>(key(2, Hand::EMPTY)), None);
        assert_eq!(table.probe::<false>(key(3, Hand::EMPTY)), Some(proven(1, Hand::EMPTY)));
    }

    #[test]
    fn full_final_bucket_takes_the_new_final_value() {
        // 確定値の表 (同じ盤面の bucket) が探索量の多い確定値で埋まっていても、新しい確定値を書く
        let gold = hand(&[PieceType::Gold]);
        let silver = hand(&[PieceType::Silver]);
        let mut table = MateTable::with_buckets(1);
        for (board, searched) in [(1u64, 3_000), (2, 1_000), (3, 2_000)] {
            table.store::<true>(key(board, gold), proven(3, gold), searched);
        }
        table.store::<true>(key(4, silver), value(2, 2), 1);
        table.store::<true>(key(4, silver), proven(5, Hand::EMPTY), 1);
        assert_eq!(table.probe::<true>(key(4, gold)), Some(proven(5, Hand::EMPTY)));
        // 探索量の最も少ない確定値 (局面 2) が置き換わる
        assert_eq!(table.probe::<true>(key(2, gold)), None);
        assert_eq!(table.probe::<true>(key(1, gold)), Some(proven(3, gold)));
    }

    #[test]
    fn final_values_are_reused_only_in_the_sound_direction() {
        let gold = hand(&[PieceType::Gold]);
        let gold_silver = hand(&[PieceType::Gold, PieceType::Silver]);
        let silver = hand(&[PieceType::Silver]);

        // 金で詰む → 金銀でも詰む。銀だけ・持駒なしでは分からない
        let mut table = MateTable::new(1, true);
        table.store::<true>(key(9, gold), proven(5, gold), 1);
        assert_eq!(table.probe::<true>(key(9, gold_silver)), Some(proven(5, gold)));
        assert_eq!(table.probe::<true>(key(9, silver)), None);
        assert_eq!(table.probe::<true>(key(9, Hand::EMPTY)), None);
        // 盤面が違えば使わない
        assert_eq!(table.probe::<true>(key(10, gold_silver)), None);

        // 金銀で詰まない → 金だけ・持駒なしでも詰まない。金銀を超える持駒では分からない
        let mut table = MateTable::new(1, true);
        table.store::<true>(key(9, gold_silver), disproven(gold_silver), 1);
        assert_eq!(table.probe::<true>(key(9, gold)), Some(disproven(gold_silver)));
        assert_eq!(table.probe::<true>(key(9, Hand::EMPTY)), Some(disproven(gold_silver)));
        assert_eq!(table.probe::<true>(key(9, gold_silver.add(PieceType::Pawn))), None);

        // 未確定値は持駒が同じときだけ
        let mut table = MateTable::new(1, true);
        table.store::<true>(key(9, gold), value(3, 3), 1);
        assert_eq!(table.probe::<true>(key(9, gold_silver)), None);

        // 流用を止めた表では持駒が同じときだけ
        let mut table = MateTable::new(1, true).without_superiority();
        table.store::<true>(key(9, gold), proven(5, gold), 1);
        assert_eq!(table.probe::<true>(key(9, gold_silver)), None);
        assert_eq!(table.probe::<true>(key(9, gold)), Some(proven(5, gold)));
    }

    #[test]
    fn final_value_is_stored_with_its_proof_pieces() {
        // 金銀を持つ局面で、金だけで詰むと分かった。局面の未確定値は詰みに置き換わり、
        // 金だけを持つ局面にも流用できる
        let gold = hand(&[PieceType::Gold]);
        let gold_silver = hand(&[PieceType::Gold, PieceType::Silver]);
        let mut table = MateTable::with_buckets(1);
        table.store::<true>(key(9, gold_silver), value(2, 2), 1);
        table.store::<true>(key(9, gold_silver), proven(5, gold), 1);
        assert!(!table.contains_exact(key(9, gold_silver)));
        assert_eq!(table.probe::<true>(key(9, gold_silver)), Some(proven(5, gold)));
        assert_eq!(table.probe::<true>(key(9, gold)), Some(proven(5, gold)));

        // 反証駒は局面の持駒より多くてよい
        let mut table = MateTable::with_buckets(1);
        table.store::<true>(key(9, gold), disproven(gold_silver), 1);
        assert_eq!(table.probe::<true>(key(9, gold_silver)), Some(disproven(gold_silver)));
    }

    #[test]
    fn exact_use_reuses_only_the_same_position() {
        let gold = hand(&[PieceType::Gold]);
        let gold_silver = hand(&[PieceType::Gold, PieceType::Silver]);
        let mut table = MateTable::new(1, false);
        assert!(!table.has_proof_pieces());
        table.store::<false>(key(9, gold), proven(5, gold), 1);
        assert_eq!(table.probe::<false>(key(9, gold)), Some(proven(5, gold)));
        assert_eq!(table.probe::<false>(key(9, gold_silver)), None);
        // 確定値は未確定値で上書きしない
        table.store::<false>(key(9, gold), value(2, 2), 100);
        assert_eq!(table.probe::<false>(key(9, gold)), Some(proven(5, gold)));
    }

    #[test]
    fn shortest_reusable_mate_is_returned() {
        let gold = hand(&[PieceType::Gold]);
        let gold_silver = hand(&[PieceType::Gold, PieceType::Silver]);
        let mut table = MateTable::new(1, true);
        table.store::<true>(key(9, gold), proven(9, gold), 1);
        table.store::<true>(key(9, gold_silver), proven(5, gold_silver), 1);
        assert_eq!(table.probe::<true>(key(9, gold_silver)).map(|v| v.mate_len), Some(5));
        assert_eq!(table.probe::<true>(key(9, gold)).map(|v| v.mate_len), Some(9));
    }

    fn exact(key: u64) -> TableKey {
        TableKey {
            key,
            board: 0,
            hand: Hand::EMPTY,
        }
    }

    #[test]
    fn unknown_values_are_matched_by_the_lower_55_bits() {
        let mut table = MateTable::with_bucket_count(1, false);
        table.store::<false>(exact(5), value(1, 1), 1);
        for bit in 32..55 {
            assert_eq!(table.probe::<false>(exact(5 | 1 << bit)), None, "bit {bit}");
        }
        assert_eq!(table.probe::<false>(exact(5)), Some(value(1, 1)));
    }

    #[test]
    fn final_values_are_matched_by_the_whole_key() {
        let mut table = MateTable::with_bucket_count(1, false);
        table.store::<false>(exact(5), proven(3, Hand::EMPTY), 1);
        table.store::<false>(exact(6), disproven(Hand::EMPTY), 1);
        assert_eq!(table.probe::<false>(exact(5 | 1 << 63)), None);
        assert_eq!(table.probe::<false>(exact(6 | 1 << 50)), None);
        assert_eq!(table.probe::<false>(exact(5)), Some(proven(3, Hand::EMPTY)));
        assert_eq!(table.probe::<false>(exact(6)), Some(disproven(Hand::EMPTY)));
    }

    #[test]
    fn entry_fields_keep_their_limits() {
        let at_depth = |depth: u16, v: TableValue| TableValue {
            min_depth: depth,
            ..v
        };
        let mate = at_depth(2047, proven(u16::MAX - 1, Hand::EMPTY));
        let e = Entry::new(u64::MAX, &mate, (1 << 20) - 1, u8::MAX);
        assert_eq!(
            (e.searched(), e.mate_len(), e.min_depth()),
            ((1 << 20) - 1, u16::MAX - 1, 2047)
        );
        assert!(!e.is_empty(u8::MAX) && e.is_proven());
        let e = Entry::new(u64::MAX, &value(7, 9), 1 << 20, u8::MAX);
        assert_eq!((e.searched(), e.pn_dn()), ((1 << 20) - 1, (7, 9)));
        assert!(!e.is_empty(u8::MAX) && !e.is_final());
    }

    #[test]
    fn rewriting_an_entry_keeps_the_shallowest_depth_and_shortest_mate() {
        let mut table = MateTable::with_bucket_count(1, false);
        table.generation = u8::MAX;
        let k = exact(0xFFFF_FFFF_FFFF_FFFF);
        let at_depth = |depth: u16, v: TableValue| TableValue {
            min_depth: depth,
            ..v
        };
        table.store::<false>(k, at_depth(7, value(3, 3)), u64::MAX);
        table.store::<false>(k, at_depth(2047, value(4, 4)), u64::MAX);
        assert_eq!(table.probe::<false>(k), Some(at_depth(7, value(4, 4))));
        table.store::<false>(k, at_depth(9, proven(9, Hand::EMPTY)), 1);
        table.store::<false>(k, at_depth(3, proven(5, Hand::EMPTY)), 1);
        table.store::<false>(k, at_depth(8, proven(7, Hand::EMPTY)), 1);
        assert_eq!(table.probe::<false>(k), Some(at_depth(3, proven(5, Hand::EMPTY))));
    }
}
