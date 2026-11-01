//! df-pn 探索本体 (長井 2002 の depth-first proof-number search)
//!
//! - OR ノード = 攻め方の手番 (王手だけを指す)、AND ノード = 受け方の手番 (応手すべて)。
//! - 子の pn/dn は親のフレーム内の配列にだけ持ち、子の探索の戻り値でだけ更新する。
//!   置換表から読むのは子を展開したときだけ。経路依存の結果を置換表に書かない代わりに、
//!   フレームが生きている (= 経路が変わらない) 間はその値を使い続けられる。
//! - 探索経路上の局面に戻ったら、その経路では不詰とする。連続王手の千日手は攻め方の負けで、
//!   それ以外の反復も攻め方の進展にならない。この不詰は戻った先の祖先 (ループの始点) が経路上に
//!   ある間だけ成り立つので、始点の深さ `loop_depth` を付けて親へ伝える。ループの始点まで
//!   戻ったら (始点のノード自身から読み直しても同じ循環になるので) 通常の不詰として確定させる。
//!   詰み手数の上限 (PV 復元で使う) や探索深さの上限で打ち切った不詰は、上限付きの探索の中でだけ
//!   成り立つ (`limited`)。詰みは反復や上限を根拠にしないので、条件が付くのは不詰だけ。
//!   条件付きの不詰は置換表に書かない。ルートの不詰が上限に依存していたら判定できなかったとする。
//! - 子が自分より浅い深さで書かれた未確定値を使っていたら、TCA (Kishimoto 2010) で閾値を延長する。
//!   循環を通って膨らむ未確定値で、同じ子を選んでは閾値で戻ることを繰り返すのを防ぐ。
//! - 受け方の合駒 (同じ升への駒違い、守りの無い中合い) と成・不成の組は、組の中で前の手が確定するまで
//!   後ろの手を現局面の pn/dn に数えない (KomoringHeights の遅延展開)。安い合駒が詰めば高い合駒も
//!   たいてい詰むので、均等に数えて pn を過大に見積もるのを防ぐ。手を省くのではないので、
//!   受け方の詰み・攻め方の不詰は、すべての手が数えられてからしか確定しない。
//! - `PIECES` (解く力優先) では、確定値に攻め方の持駒の条件 (詰みは証明駒、不詰は反証駒) を付けて
//!   置換表に書き、持駒だけ違う局面に流用する (KomoringHeights / OSL と同じ考え方)。証明駒は子の
//!   証明駒の駒種ごとの最大に、受け方が合駒に使えたはずの駒 (攻め方が持っていないと受け方の手にある)
//!   を足す。反証駒は子の反証駒の最小から、持っていれば打って王手できる駒種を除く。
//!   経路や上限に依存する不詰には反証駒を付けない (局面の持駒のまま、置換表にも書かない)。
//! - 最短手順モード (`enable_shortest`) では、詰み手数の上限だけに依存する不詰を上限と組で覚えて、上限が
//!   それ以下の探索に使う。無駄合いを数えない数え方では、詰みも置換表に書かない。どちらも
//!   `ShortestTables` に局面の完全一致でだけ書き、置換表 (持駒違いに流用する) には上限なしの不詰と、
//!   全応手を数える詰みしか書かない。
//! - 打ち切り (ノード数・時間・停止) は `Abort` で全体を中断する。
//!   上限に達したノードに値を返させると、不詰と取り違えたり、同じ子を選び続けたりする。

use std::any::Any;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::MateProgress;
use super::state::FULL_HAND;
use super::state::MateState;
use super::table::{MateTable, PN_INF, PN_MAX_FINITE, TableKey, TableValue};
use crate::time::Instant;
use crate::types::{Color, Hand, PieceType};

/// 探索経路の最大長 (手数)。ここに達した局面は上限付きの不詰とする。
///
/// 探索は手数ごとに再帰する。wasm では実行エンジンの呼び出しスタックを 1 手あたり約 340〜400 byte
/// (ビルドで変わる) 使い、2048 手ではブラウザの Worker の既定 (Chromium で約 770 KB) を超えうるので、
/// native の半分の 1024 手にする。
pub(super) const MAX_DEPTH: usize = if cfg!(target_arch = "wasm32") {
    1024
} else {
    2048
};

/// 停止フラグ・時間を確かめるノード間隔
pub(super) const CHECK_INTERVAL: u64 = 1024;
/// 子の配列 (全フレーム分を 1 本に積む) にあらかじめ確保しておく要素数。
/// 経路上の各局面の候補手の合計で、超えたときだけ伸ばす (伸ばした領域はソルバーが持ち回る)。
const CHILDREN_RESERVE: usize = 1 << 16;
/// 進捗を知らせる間隔 (ミリ秒)
pub(super) const PROGRESS_INTERVAL_MS: u64 = 1000;

/// 探索の打ち切り。結果は Unknown になる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Abort;

/// 1 ノードの探索結果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct NodeResult {
    pub pn: u32,
    pub dn: u32,
    /// 詰みのときの詰み手数 (このノードから終局まで)。見つけた証明の手数で、最短とは限らない。
    pub mate_len: u16,
    /// 不詰が依存している経路上の祖先のうち最も浅いものの深さ。`NO_LOOP` なら依存しない。
    pub loop_depth: u16,
    /// 不詰が詰み手数の上限か探索深さの上限に依存している
    pub limited: bool,
    /// `limited` のうち、探索深さの上限に依存している
    pub depth_limited: bool,
    /// 攻め方の持駒の条件。詰みは証明駒、経路や上限に依存しない不詰は反証駒、
    /// それ以外 (未確定と条件付きの不詰) は局面の持駒。
    pub hand: Hand,
}

/// 不詰が経路上の祖先に依存しないことを表す `loop_depth`
const NO_LOOP: u16 = u16::MAX;

impl NodeResult {
    fn proven(mate_len: u16, hand: Hand) -> Self {
        Self {
            pn: 0,
            dn: PN_INF,
            mate_len,
            loop_depth: NO_LOOP,
            limited: false,
            depth_limited: false,
            hand,
        }
    }

    fn disproven(loop_depth: u16, limited: bool, hand: Hand) -> Self {
        Self {
            pn: PN_INF,
            dn: 0,
            mate_len: 0,
            loop_depth,
            limited,
            depth_limited: false,
            hand,
        }
    }

    /// 探索深さの上限で打ち切った不詰
    fn depth_disproven(hand: Hand) -> Self {
        Self {
            depth_limited: true,
            ..Self::disproven(NO_LOOP, true, hand)
        }
    }

    /// 詰み手数の上限だけに依存する不詰 (この局面から上限の手数以内には詰まない)
    fn is_len_bounded(&self) -> bool {
        self.is_disproven() && self.limited && !self.depth_limited && self.loop_depth == NO_LOOP
    }

    /// 局面そのものでなく、経路か上限に依存した結果か
    fn is_conditional(&self) -> bool {
        self.loop_depth != NO_LOOP || self.limited
    }

    pub fn is_proven(&self) -> bool {
        self.pn == 0
    }

    pub fn is_disproven(&self) -> bool {
        self.dn == 0
    }

    fn is_final(&self) -> bool {
        self.is_proven() || self.is_disproven()
    }
}

struct Child<M> {
    mv: M,
    pn: u32,
    dn: u32,
    mate_len: u16,
    loop_depth: u16,
    limited: bool,
    depth_limited: bool,
    /// 無駄合い (最短手順モードの受け方の局面だけ)。詰みとして数え、合駒と取る 2 手は詰み手数に数えない
    futile: bool,
    /// 子の局面での `NodeResult::hand`
    hand: Hand,
    /// 置換表に無く、まだ一度も探索していない子 (TCA の延長を戻す判定に使う)
    first_visit: bool,
    /// 現局面の pn/dn に数えるか。確定した子は常に数える。後回しの組で、組の中の前に確定していない
    /// 手がある確定していない手は数えない。
    active: bool,
    /// 後回しの組でこの手の次の手 (フレーム内の添字)。`NO_NEXT` なら無い。
    next: u16,
}

/// `Child::next` の「次の手が無い」
const NO_NEXT: u16 = u16::MAX;

impl<M> Child<M> {
    fn is_final(&self) -> bool {
        self.pn == 0 || self.dn == 0
    }

    /// 無駄合いの詰みにする。`proof` は合駒の後の局面の証明駒、`len` は合駒を取って取った駒を戻した局面の
    /// 詰み手数 (受け方の局面の詰み手数にそのまま数える)
    fn set_futile(&mut self, proof: Hand, len: u16) {
        self.pn = 0;
        self.dn = PN_INF;
        self.mate_len = len;
        self.loop_depth = NO_LOOP;
        self.limited = false;
        self.depth_limited = false;
        self.futile = true;
        self.hand = proof;
        self.first_visit = false;
    }
}

/// 探索するノードと閾値
#[derive(Debug, Clone, Copy)]
pub(super) struct NodeArgs {
    /// ルートからの手数
    pub depth: usize,
    /// 攻め方の手番か
    pub or_node: bool,
    pub thpn: u32,
    pub thdn: u32,
    /// この局面から詰ませる手数の上限。`NO_LEN_LIMIT` は無制限。
    pub len_limit: u16,
}

/// 詰み手数の上限なし
pub(super) const NO_LEN_LIMIT: u16 = u16::MAX;

impl NodeArgs {
    /// 閾値なし (解けるか打ち切りまで読む)。`len_limit` は詰み手数の上限。
    pub fn unbounded(depth: usize, or_node: bool, len_limit: u16) -> Self {
        Self {
            depth,
            or_node,
            thpn: PN_INF,
            thdn: PN_INF,
            len_limit,
        }
    }

    fn child_len_limit(&self) -> u16 {
        if self.len_limit == NO_LEN_LIMIT {
            NO_LEN_LIMIT
        } else {
            self.len_limit.saturating_sub(1)
        }
    }
}

/// 停止条件と進捗通知
pub(super) struct Budget<'a> {
    pub stop: &'a AtomicBool,
    /// 並列に解くとき、ほかのスレッドが根の結論を出したら立つ
    pub halt: Option<&'a AtomicBool>,
    /// 並列に解くとき、どれかのスレッドが panic したら立つ。根の結論を出したスレッドも見続ける
    /// (`halt` は詰み手順の復元のために外す)。
    pub cancel: Option<&'a AtomicBool>,
    /// 並列に解くとき、全スレッドのノード数の合計とその上限 (0 は無制限)。進捗も合計で知らせる
    /// (`CHECK_INTERVAL` ごとに足し込むので、上限はスレッド数 × `CHECK_INTERVAL` まで超えうる)。
    pub shared_nodes: Option<(&'a AtomicU64, u64)>,
    /// このスレッドのノード数の上限。0 は無制限
    pub node_limit: u64,
    /// 0 は無制限
    pub time_limit_ms: u64,
    pub start: Instant,
    pub progress: &'a mut dyn FnMut(MateProgress),
}

/// `Scratch::path_filter` の升の数 (2 の冪)
const PATH_FILTER_SIZE: usize = 1 << 12;

#[inline]
fn path_filter_index(key: u64) -> usize {
    key as usize & (PATH_FILTER_SIZE - 1)
}

/// 探索の作業領域。ソルバーが solve をまたいで持ち回り、探索中に確保し直さない。
pub(super) struct Scratch<M> {
    /// ルートから現局面までの局面のキー (添字が深さ)
    path: Vec<u64>,
    /// `path` のキーを下位ビットで数えたもの。0 の升のキーは経路に無いので、経路を走査せずに済む
    path_filter: Vec<u16>,
    /// 経路上の各局面の子を、浅い順に 1 本に積んだもの。各フレームの子は末尾にある
    /// (深いフレームは戻るときに自分の子を取り除く)。
    children: Vec<Child<M>>,
    moves: Vec<M>,
    /// `moves` の各手を指した後の置換表のキー
    child_keys: Vec<TableKey>,
    /// 後回しにしてよい手の (組, 組の中で読む順, フレーム内の添字)
    delay_order: Vec<(u16, u8, u16)>,
    /// `capture_proven` で指定した深さのフレームが終わったときの、証明済みの子
    proven: Vec<(M, u16)>,
    /// 無駄合いの判定で別の局面を解くときの作業領域 (最初に使うときに作る)
    sub: Option<Box<Scratch<M>>>,
    /// 無駄合いの判定で生成する攻め方の手
    capture_moves: Vec<M>,
    /// 無駄合いの判定で生成する受け方の手
    reply_moves: Vec<M>,
    /// 無駄合いの判定で取った駒を戻した局面 (`Option<S>`。局面の型は solve ごとに変わりうる)
    returned: Option<Box<dyn Any + Send>>,
}

/// 最短手順モードの表。無駄合いの判定で解く局面 (別の作業領域で解く) や、並列に解くスレッドとも共有する。
///
/// 置換表と分けるのは、ここに書く値が無駄合いの数え方に依存するため。上限付きの不詰は上限と組でしか
/// 成り立たず、無駄合いを数えない詰み手数は他の応手の手数で決まる無駄合いに依存するので、持駒違いの局面に
/// 流用できない (攻め方の持駒が多いと他の応手が短くなり、無駄合いだった合駒が有効になって長くなりうる)。
/// どちらも局面の完全一致でだけ使う。
pub(super) struct ShortestTables {
    slots: OnceLock<Slots>,
    /// 鍵に混ぜる値。数え方を変えるたびに変えて、表を書き換えずに前の値を無効にする
    salt: AtomicU64,
}

struct Slots {
    /// 局面からこの手数以内には詰まない
    len_bounds: Box<[Slot]>,
    /// 無駄合いを数えない数え方で、局面がこの手数以内に詰む
    mates: Box<[Slot]>,
    /// 無駄合いの判定の結果 (`Futility::pack`)
    futility: Box<[Slot]>,
    /// 経路に依存する不詰 (`LoopDisproof`)
    loops: Box<[Slot]>,
}

/// 表の 1 件。スレッド間で鍵の照合と値の読み書きを分けずに済むよう、鍵を値と XOR して書く。
/// 値をそのまま XOR すると、2 つの書き込みの `check` と `data` を組み合わせて読んだときに値の差が
/// そのまま鍵の差になり、同じ枠を使う登録していない鍵と一致しうる (無駄合いの表は値の bit が添字の
/// bit の外にある)。値を `splitmix64_finalize` で混ぜてから XOR し、組み合わせた読み取りの鍵をでたらめな値にする。
#[derive(Default)]
struct Slot {
    check: AtomicU64,
    data: AtomicU64,
}

impl Slot {
    fn get(&self, salted: u64) -> Option<u64> {
        self.get_with(|_| Some(salted))
    }

    /// 値から照合する鍵を決める (鍵の一部を値から求める表のため)
    fn get_with(&self, salted: impl FnOnce(u64) -> Option<u64>) -> Option<u64> {
        let data = self.data.load(Ordering::Relaxed);
        let salted = salted(data)?;
        (self.check.load(Ordering::Relaxed) ^ splitmix64_finalize(data) == salted).then_some(data)
    }

    fn set(&self, salted: u64, data: u64) {
        self.data.store(data, Ordering::Relaxed);
        self.check.store(salted ^ splitmix64_finalize(data), Ordering::Relaxed);
    }
}

fn splitmix64_finalize(mut x: u64) -> u64 {
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

fn new_slots(size: usize) -> Box<[Slot]> {
    (0..size).map(|_| Slot::default()).collect()
}

impl ShortestTables {
    pub fn new() -> Self {
        Self {
            slots: OnceLock::new(),
            salt: AtomicU64::new(0),
        }
    }

    /// (初めてなら確保して) 空にする。探索中に呼ばない
    fn reset(&self) {
        self.slots.get_or_init(|| Slots {
            len_bounds: new_slots(LEN_BOUNDS_SIZE),
            mates: new_slots(LEN_BOUNDS_SIZE),
            futility: new_slots(FUTILITY_SIZE),
            loops: new_slots(LOOPS_SIZE),
        });
        self.salt.fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed);
    }

    fn salted(&self, key: u64) -> u64 {
        key ^ self.salt.load(Ordering::Relaxed)
    }

    fn get(&self, table: fn(&Slots) -> &[Slot], key: u64) -> Option<u64> {
        let table = table(self.slots.get()?);
        table[key as usize & (table.len() - 1)].get(self.salted(key))
    }

    /// 同じ局面の値があれば `keep` で残す方を選ぶ
    fn put(&self, table: fn(&Slots) -> &[Slot], key: u64, bound: u16, keep: fn(u16, u16) -> u16) {
        let Some(slots) = self.slots.get() else {
            return;
        };
        let table = table(slots);
        let salted = self.salted(key);
        let slot = &table[key as usize & (table.len() - 1)];
        let bound = match slot.get(salted) {
            Some(old) => keep(old as u16, bound),
            None => bound,
        };
        slot.set(salted, u64::from(bound));
    }

    /// 局面 `key` が何手以内に詰まないと分かっているか
    fn len_bound(&self, key: u64) -> Option<u16> {
        self.get(|s| &s.len_bounds, key).map(|v| v as u16)
    }

    fn store_len_bound(&self, key: u64, bound: u16) {
        self.put(|s| &s.len_bounds, key, bound, u16::max);
    }

    /// 局面 `key` が何手以内に詰むと分かっているか
    pub(super) fn mate_len(&self, key: u64) -> Option<u16> {
        self.get(|s| &s.mates, key).map(|v| v as u16)
    }

    fn store_mate_len(&self, key: u64, len: u16) {
        self.put(|s| &s.mates, key, len, u16::min);
    }

    fn futility(&self, key: u64) -> Option<Futility> {
        self.get(|s| &s.futility, key).map(Futility::unpack)
    }

    fn store_futility(&self, key: u64, futility: Futility) {
        if let Some(slots) = self.slots.get() {
            let table = &slots.futility;
            table[key as usize & (table.len() - 1)].set(self.salted(key), futility.pack());
        }
    }

    /// 経路の末尾 `path` の後に、詰み手数の上限 `len_limit` で局面 `key` が来たときに使える、覚えた
    /// 経路に依存する不詰
    fn loop_disproof(&self, key: u64, path: &[u64], len_limit: u16) -> Option<LoopDisproof> {
        let table = &self.slots.get()?.loops;
        let data = table[key as usize & (table.len() - 1)].get_with(|data| {
            let d = LoopDisproof::unpack(data);
            let span = usize::from(d.span);
            (LoopDisproof::tag_of(data) == LoopDisproof::tag(key)
                && (1..=path.len()).contains(&span)
                && len_limit <= d.limit)
                .then(|| self.salted(segment_key(key, &path[path.len() - span..])))
        })?;
        Some(LoopDisproof::unpack(data))
    }

    /// 局面 `key` が、直前の経路 `segment` (祖先、古い順) が経路上にある限り不詰
    fn store_loop_disproof(&self, key: u64, segment: &[u64], disproof: LoopDisproof) {
        if disproof.fits()
            && let Some(slots) = self.slots.get()
        {
            let table = &slots.loops;
            let salted = self.salted(segment_key(key, segment));
            table[key as usize & (table.len() - 1)].set(salted, disproof.pack(key));
        }
    }

    #[cfg(target_arch = "x86_64")]
    fn prefetch_loop(&self, key: u64) {
        use std::arch::x86_64::{_MM_HINT_T0, _mm_prefetch};
        if let Some(slots) = self.slots.get() {
            let table = &slots.loops;
            let slot = &table[key as usize & (table.len() - 1)] as *const Slot as *const i8;
            // SAFETY: slot は table の生存中の要素を指す。prefetch はメモリの内容を読み書きしない。
            unsafe { _mm_prefetch(slot, _MM_HINT_T0) };
        }
    }

    #[cfg(not(target_arch = "x86_64"))]
    fn prefetch_loop(&self, _key: u64) {}
}

/// 局面と、その直前の経路の局面の並びをまとめた鍵
fn segment_key(key: u64, segment: &[u64]) -> u64 {
    segment
        .iter()
        .fold(splitmix64_finalize(key), |h, &k| splitmix64_finalize(h ^ k))
}

/// 経路に依存する不詰。経路上の局面への反復で不詰になった結果は、反復先から直前までの局面が同じ順で
/// 経路にあれば、どこから来ても成り立つ (経路の局面が増えても、反復で止まる手が増えるだけ)。
/// 置換表には書けない結果で、写しの筋で同じ局面と経路の組が繰り返し現れる。
#[derive(Clone, Copy)]
struct LoopDisproof {
    /// 依存する経路の局面の数 (直前から数える)
    span: u16,
    /// 上限にも依存するなら、成り立つ詰み手数の上限の最大 (依存しないなら `NO_LEN_LIMIT`)
    limit: u16,
    hand: Hand,
}

impl LoopDisproof {
    const SPAN_BITS: u32 = 10;
    const LIMIT_BITS: u32 = 12;
    const NO_LIMIT: u64 = (1 << Self::LIMIT_BITS) - 1;

    /// 値に入れる局面の鍵の一部。経路の区間の鍵を計算する前に、別の局面の値をほぼ弾く
    fn tag(key: u64) -> u64 {
        key >> (64 - (32 - Self::SPAN_BITS - Self::LIMIT_BITS))
    }

    fn tag_of(v: u64) -> u64 {
        (v as u32 >> (Self::SPAN_BITS + Self::LIMIT_BITS)) as u64
    }

    /// 値に収まるか (区間が長すぎる・上限が大きすぎる結果は覚えない)
    fn fits(self) -> bool {
        u64::from(self.span) < 1 << Self::SPAN_BITS
            && (self.limit == NO_LEN_LIMIT || u64::from(self.limit) < Self::NO_LIMIT)
    }

    fn pack(self, key: u64) -> u64 {
        let limit = if self.limit == NO_LEN_LIMIT {
            Self::NO_LIMIT
        } else {
            u64::from(self.limit)
        };
        u64::from(self.span)
            | limit << Self::SPAN_BITS
            | Self::tag(key) << (Self::SPAN_BITS + Self::LIMIT_BITS)
            | u64::from(self.hand.raw()) << 32
    }

    fn unpack(v: u64) -> Self {
        let limit = (v >> Self::SPAN_BITS) & Self::NO_LIMIT;
        Self {
            span: (v & ((1 << Self::SPAN_BITS) - 1)) as u16,
            limit: if limit == Self::NO_LIMIT {
                NO_LEN_LIMIT
            } else {
                limit as u16
            },
            hand: Hand::from_raw((v >> 32) as u32),
        }
    }
}

/// 無駄合いの判定の結果: 無駄合いなら証明駒 `proof` と、合駒を取って戻した局面の詰み手数 `len`
#[derive(Clone, Copy)]
struct Futility {
    futile: bool,
    proof: Hand,
    len: u16,
}

impl Futility {
    fn pack(self) -> u64 {
        u64::from(self.futile) | u64::from(self.len) << 16 | u64::from(self.proof.raw()) << 32
    }

    fn unpack(v: u64) -> Self {
        Self {
            futile: v & 1 != 0,
            len: (v >> 16) as u16,
            proof: Hand::from_raw((v >> 32) as u32),
        }
    }
}

/// `ShortestTables` の無駄合いの表の要素数 (2 の冪)
const FUTILITY_SIZE: usize = 1 << 16;

/// `ShortestTables` の手数の表の要素数 (2 の冪)
const LEN_BOUNDS_SIZE: usize = 1 << 20;

/// `ShortestTables` の経路に依存する不詰の表の要素数 (2 の冪)
const LOOPS_SIZE: usize = 1 << 18;

impl<M> Scratch<M> {
    pub fn new() -> Self {
        Self {
            path: Vec::with_capacity(MAX_DEPTH),
            path_filter: vec![0; PATH_FILTER_SIZE],
            children: Vec::with_capacity(CHILDREN_RESERVE),
            moves: Vec::with_capacity(crate::movegen::MAX_MOVES),
            child_keys: Vec::with_capacity(crate::movegen::MAX_MOVES),
            delay_order: Vec::with_capacity(crate::movegen::MAX_MOVES),
            proven: Vec::with_capacity(crate::movegen::MAX_MOVES),
            sub: None,
            capture_moves: Vec::with_capacity(crate::movegen::MAX_MOVES),
            reply_moves: Vec::with_capacity(crate::movegen::MAX_MOVES),
            returned: None,
        }
    }
}

/// 1 回の solve の探索状態。
///
/// `PIECES` なら確定値を証明駒・反証駒で置換表に書き、持駒違いの局面に流用する
/// (難しい問題でノードが減るが、置換表を引くたびに読む量が増えて 1 ノードあたりは重い)。
pub(super) struct Searcher<'a, S: MateState, const PIECES: bool> {
    table: &'a MateTable,
    scratch: &'a mut Scratch<S::Mv>,
    /// 攻め方 (ルートの手番)。置換表の持駒はこちらの持駒で表す
    attacker: Color,
    nodes: u64,
    /// `budget.shared_nodes` に足し込み済みのノード数
    reported: u64,
    next_check: u64,
    next_progress_ms: u64,
    budget: Budget<'a>,
    /// この深さのフレームが終わるとき、証明済みの子を `scratch.proven` に写す (PV 復元用)
    capture_depth: Option<usize>,
    /// 置換表に無い子の pn/dn を局面から見積もるか (`MateState::initial_pn_dn`)
    estimate: bool,
    /// 攻め方の局面の dn を子の和でなく WPNS で数えるときの、子 1 つあたりの加算量 (0 なら和で数える)
    weak_disproof_step: u64,
    /// 1+ε の余裕として 2 番目の子の φ に掛ける倍率 (2^16 倍の固定小数点)。`None` は既定の 1/3。
    /// 子を選ぶたびに計算するので、割り算を避ける (既定は定数の割り算で掛け算になる)。
    slack: Option<u64>,
    /// 0 でなければ、φ が同じ子のどれを選ぶかをこの値で決める (並列に解くスレッドごとに変える)
    tie_seed: u32,
    /// 最短手順モード: 受け方の無駄合いを詰み手数に数えず、詰み手数の上限だけに依存する不詰を覚えておく
    shortest: bool,
    /// 最短手順モードで無駄合いを詰み手数に数えない
    futility: bool,
    /// 最短手順モードの表
    tables: Option<&'a ShortestTables>,
    /// 補助スレッドの作業領域 (`search_shared` で同じ局面を並列に読む。空なら 1 スレッド)
    helpers: &'a mut [Scratch<S::Mv>],
    /// `search_shared` で、補助スレッドを起こす前に 1 スレッドで読むノード数
    serial_nodes: u64,
}

/// `Searcher::solve_detached` の結果
enum Detached {
    /// 上限内に詰む (証明駒, 詰み手数)
    Mated(Hand, u16),
    /// 上限内に詰まない
    Escaped,
}

/// 証明駒を使わないときの置換表のキー。盤面のキーと攻め方の持駒は使わないので求めない。
#[inline]
fn exact_key(key: u64) -> TableKey {
    TableKey {
        key,
        board: 0,
        hand: Hand::EMPTY,
    }
}

#[inline]
fn add_pn(a: u64, b: u32) -> u64 {
    a + u64::from(b)
}

/// 和を pn/dn の値域に収める。∞ を含む和は ∞ (確定)、そうでなければ ∞ 未満で飽和する。
#[inline]
fn clamp_sum(sum: u64, has_inf: bool) -> u32 {
    if has_inf {
        PN_INF
    } else {
        sum.min(u64::from(PN_MAX_FINITE)) as u32
    }
}

impl<'a, S: MateState, const PIECES: bool> Searcher<'a, S, PIECES> {
    pub fn new(
        table: &'a MateTable,
        scratch: &'a mut Scratch<S::Mv>,
        attacker: Color,
        budget: Budget<'a>,
    ) -> Self {
        debug_assert!(!PIECES || table.has_proof_pieces());
        scratch.path.clear();
        scratch.path_filter.fill(0);
        scratch.children.clear();
        Self {
            table,
            scratch,
            attacker,
            nodes: 0,
            reported: 0,
            // 最初のノードで停止フラグを確かめる (開始前に立っていたら何も返さない)
            next_check: 0,
            next_progress_ms: PROGRESS_INTERVAL_MS,
            budget,
            capture_depth: None,
            estimate: false,
            weak_disproof_step: 0,
            slack: None,
            tie_seed: 0,
            shortest: false,
            futility: false,
            tables: None,
            helpers: &mut [],
            serial_nodes: 0,
        }
    }

    /// 最短手順モードにする ([`super::shortest`])。詰み手数の上限付きの探索で、上限だけに依存する不詰を
    /// `tables` に覚えておき、`futility` なら受け方の無駄合いを詰み手数に数えない。
    /// `futility` では、詰みは置換表に書かず `tables` に局面の完全一致で書く (`ShortestTables`)。
    /// 覚えた値は無駄合いの数え方で変わるので、呼ぶたびに捨てる。
    pub fn enable_shortest(&mut self, futility: bool, tables: &'a ShortestTables) {
        self.shortest = true;
        self.futility = futility;
        tables.reset();
        self.tables = Some(tables);
    }

    /// `search_shared` で使う補助スレッドの作業領域 (スレッド数 − 1 個) と、補助スレッドを起こす前に
    /// 1 スレッドで読むノード数を渡す
    pub fn set_helpers(&mut self, helpers: &'a mut [Scratch<S::Mv>], serial_nodes: u64) {
        self.helpers = helpers;
        self.serial_nodes = serial_nodes;
    }

    fn settings(&self) -> (bool, u64, bool, bool, Option<&'a ShortestTables>) {
        (
            self.estimate,
            self.weak_disproof_step,
            self.shortest,
            self.futility,
            self.tables,
        )
    }

    fn apply_settings(&mut self, settings: (bool, u64, bool, bool, Option<&'a ShortestTables>)) {
        (
            self.estimate,
            self.weak_disproof_step,
            self.shortest,
            self.futility,
            self.tables,
        ) = settings;
    }

    fn len_bound(&self, key: u64) -> Option<u16> {
        self.tables.and_then(|t| t.len_bound(key))
    }

    /// 置換表に無い子の pn/dn を局面から見積もる (df-pn+)。詰む問題ではノードが大きく減るが、
    /// 不詰の多い詰めろ判定では増える
    pub fn estimate_initial_values(&mut self) {
        self.estimate = true;
    }

    /// 攻め方の局面の dn を「子の dn の最大値 + (dn が 0 でない子の数 − 1) × `step`」で数える
    /// (WPNS, Ueda ほか 2008)。王手の後の局面どうしが合流すると同じ反証の手間を何度も足してしまい、
    /// 和では dn が膨らむ。`step` は子の初期値の単位にそろえる (`MateState::initial_pn_dn`)。0 は 1 として扱う。
    pub fn count_disproof_weakly(&mut self, step: u32) {
        self.weak_disproof_step = u64::from(step.max(1));
    }

    /// 1+ε の余裕を 2 番目の子の φ の `num / den` にする (並列に解くスレッドごとに探索をずらす)
    pub fn set_threshold_slack(&mut self, num: u64, den: u64) {
        self.slack = Some((num << 16) / den.max(1));
    }

    /// φ が同じ子のうちどれを選ぶかを `seed` (0 でない) で決める。既定は先に生成した子。
    /// 未展開の子はどれも同じ初期値になりやすく、全スレッドが同じ子を選ぶと同じ局面を重ねて読む
    pub fn set_tie_seed(&mut self, seed: u32) {
        debug_assert!(seed != 0);
        self.tie_seed = seed;
    }

    /// 次に進捗を知らせる経過時間 (ミリ秒)。前の探索から続けて知らせるときに使う
    pub fn set_next_progress(&mut self, ms: u64) {
        self.next_progress_ms = ms;
    }

    /// 攻め方
    pub fn attacker(&self) -> Color {
        self.attacker
    }

    /// これまでに探索したノード数
    pub fn nodes(&self) -> u64 {
        self.nodes
    }

    /// `f` (この探索器での探索) をあと `nodes` ノードまでで打ち切る。打ち切ったら `None`。
    /// 探索全体の制限 (ノード数・時間・停止) に達したときは `Abort` のまま返す。
    pub fn within<T>(
        &mut self,
        nodes: u64,
        f: impl FnOnce(&mut Self) -> Result<T, Abort>,
    ) -> Result<Option<T>, Abort> {
        let global = self.budget.node_limit;
        let local = self.nodes.saturating_add(nodes);
        let tighter = global == 0 || local < global;
        if tighter {
            self.budget.node_limit = local;
        }
        let result = f(self);
        self.budget.node_limit = global;
        match result {
            Ok(v) => Ok(Some(v)),
            Err(abort) => {
                let stopped = self.budget.stop.load(Ordering::Relaxed)
                    || self.budget.cancel.is_some_and(|c| c.load(Ordering::Relaxed))
                    || self.budget.time_limit_ms != 0
                        && self.budget.start.elapsed().as_millis() as u64
                            >= self.budget.time_limit_ms;
                if tighter && !stopped {
                    // 打ち切った探索の深さ 0 で証明済みの子を集める指定が残ると、次の探索が集めてしまう
                    self.capture_depth = None;
                    Ok(None)
                } else {
                    Err(abort)
                }
            }
        }
    }

    /// 局面が探索経路上の局面の反復なら、その祖先の深さ
    fn repetition_depth(&self, key: u64) -> Option<usize> {
        if self.scratch.path_filter[path_filter_index(key)] == 0 {
            return None;
        }
        self.scratch.path.iter().rposition(|&k| k == key)
    }

    /// 次の `search` で深さ `depth` のフレームが終わるとき、証明済みの子を記録させる
    pub fn capture_proven(&mut self, depth: usize) {
        self.capture_depth = Some(depth);
        self.scratch.proven.clear();
    }

    /// `capture_proven` で記録した証明済みの子 (指し手, 詰み手数)。
    /// 1 手詰めの近道で証明されたノードは子を展開しないので空になる。
    pub fn proven_children(&self) -> &[(S::Mv, u16)] {
        &self.scratch.proven
    }

    /// 根の結論が出たので、ほかのスレッドを止める。このスレッドはこの後 (詰み手順の復元) も止めない。
    pub fn halt_others(&mut self) {
        if let Some(halt) = self.budget.halt.take() {
            halt.store(true, Ordering::Relaxed);
        }
    }

    fn count_node(&mut self) -> Result<(), Abort> {
        self.nodes += 1;
        if self.budget.node_limit != 0 && self.nodes > self.budget.node_limit {
            return Err(Abort);
        }
        if self.nodes >= self.next_check {
            self.next_check = self.nodes + CHECK_INTERVAL;
            let total = match self.budget.shared_nodes {
                Some((shared, limit)) => {
                    let delta = self.nodes - self.reported;
                    self.reported = self.nodes;
                    let total = shared.fetch_add(delta, Ordering::Relaxed) + delta;
                    if limit != 0 && total > limit {
                        return Err(Abort);
                    }
                    total
                }
                None => self.nodes,
            };
            if self.budget.stop.load(Ordering::Relaxed)
                || self.budget.halt.is_some_and(|h| h.load(Ordering::Relaxed))
                || self.budget.cancel.is_some_and(|c| c.load(Ordering::Relaxed))
            {
                return Err(Abort);
            }
            let elapsed_ms = self.budget.start.elapsed().as_millis() as u64;
            if self.budget.time_limit_ms != 0 && elapsed_ms >= self.budget.time_limit_ms {
                return Err(Abort);
            }
            if elapsed_ms >= self.next_progress_ms {
                self.next_progress_ms = elapsed_ms + PROGRESS_INTERVAL_MS;
                (self.budget.progress)(MateProgress {
                    nodes: total,
                    elapsed_ms,
                });
            }
        }
        Ok(())
    }

    /// 局面 `st` を閾値まで探索する。`inc_flag` は TCA の延長の残り段数。
    ///
    /// 経路上の局面の反復なら探索せず、経路依存の不詰を返す。
    pub fn search(
        &mut self,
        st: &mut S,
        args: NodeArgs,
        inc_flag: &mut u32,
    ) -> Result<NodeResult, Abort> {
        self.count_node()?;
        let table_key = if PIECES {
            st.table_key(self.attacker)
        } else {
            exact_key(st.key())
        };
        let key = table_key.key;
        if args.depth >= MAX_DEPTH {
            return Ok(NodeResult::depth_disproven(table_key.hand));
        }
        if let Some(loop_depth) = self.repetition_depth(key) {
            return Ok(NodeResult::disproven(loop_depth as u16, false, table_key.hand));
        }

        self.scratch.path.push(key);
        self.scratch.path_filter[path_filter_index(key)] += 1;
        let frame_start = self.scratch.children.len();
        let result = self.search_node(st, table_key, args, frame_start, inc_flag);
        self.scratch.children.truncate(frame_start);
        self.scratch.path.pop();
        self.scratch.path_filter[path_filter_index(key)] -= 1;
        result
    }

    fn search_node(
        &mut self,
        st: &mut S,
        key: TableKey,
        args: NodeArgs,
        start: usize,
        inc_flag: &mut u32,
    ) -> Result<NodeResult, Abort> {
        let NodeArgs {
            depth,
            or_node,
            thpn: orig_thpn,
            thdn: orig_thdn,
            len_limit,
        } = args;
        let nodes_before = self.nodes;

        if or_node && len_limit == 0 {
            return Ok(NodeResult::disproven(NO_LOOP, true, key.hand));
        }
        // 1 手詰めは局面だけで決まる。未確定値か不詰が置換表にあるノードは前の訪問で 1 手詰めでないと
        // 判定済みなので調べない。詰みが置換表にあるノードを読むのは詰み手順の復元だけで、
        // そこでは 1 手詰めなら子を展開せずに済む。
        if or_node
            && self.table.probe::<PIECES>(key).is_none_or(|v| v.pn == 0)
            && let Some(proof) = if PIECES {
                st.mate_1ply_with_proof().map(|(_, proof)| proof)
            } else {
                st.mate_1ply().map(|_| key.hand)
            }
        {
            let result = NodeResult::proven(1, proof);
            self.store(key, depth, &result, 1);
            return Ok(result);
        }

        let has_old_child = self.expand(st, depth, or_node, len_limit)?;
        if self.scratch.children.len() == start {
            // 攻め方に王手が無ければ不詰、受け方に応手が無ければ詰み
            let result = match (or_node, PIECES) {
                (true, true) => {
                    let disproof = st.disproof_hand_for_checks(FULL_HAND, self.attacker);
                    NodeResult::disproven(NO_LOOP, false, disproof)
                }
                (true, false) => NodeResult::disproven(NO_LOOP, false, key.hand),
                (false, true) => {
                    NodeResult::proven(0, st.proof_hand_for_evasions(Hand::EMPTY, self.attacker))
                }
                (false, false) => NodeResult::proven(0, key.hand),
            };
            self.store(key, depth, &result, 1);
            return Ok(result);
        }
        // 受け方に応手があり、手数の上限で攻め方はもう指せない (最短手順モードで応手がすべて無駄合いなら詰み)
        if len_limit == 0 && !(self.shortest && self.aggregate(start, or_node).is_proven()) {
            return Ok(NodeResult::disproven(NO_LOOP, true, key.hand));
        }

        let orig_inc_flag = *inc_flag;
        if has_old_child {
            *inc_flag += 1;
        }

        let mut thpn = orig_thpn;
        let mut thdn = orig_thdn;
        let mut current = self.aggregate(start, or_node);
        if *inc_flag > 0 {
            extend_thresholds(&current, &mut thpn, &mut thdn);
        }

        while !current.is_final() && current.pn < thpn && current.dn < thdn {
            let (best, child_thpn, child_thdn) = self.select(start, or_node, thpn, thdn);
            let child = &mut self.scratch.children[best];
            let mv = child.mv;
            // 新しい局面を展開したら TCA の延長を 1 段減らす
            let consumed_extension = child.first_visit && *inc_flag > 0;
            child.first_visit = false;
            if consumed_extension {
                *inc_flag -= 1;
            }

            let child_args = NodeArgs {
                depth: depth + 1,
                or_node: !or_node,
                thpn: child_thpn,
                thdn: child_thdn,
                len_limit: args.child_len_limit(),
            };
            st.do_move(mv);
            let child_result = self.search(st, child_args, inc_flag);
            st.undo_move(mv);
            let child_result = child_result?;
            if consumed_extension && child_result.is_conditional() {
                // 条件付きの不詰は置換表に残らず、次に展開しても新しい局面に見える。
                // 延長を消費したことにすると、延長が循環の外へ届かないまま尽きる。
                *inc_flag += 1;
            }

            let child = &mut self.scratch.children[best];
            child.pn = child_result.pn;
            child.dn = child_result.dn;
            child.mate_len = child_result.mate_len;
            child.loop_depth = child_result.loop_depth;
            child.limited = child_result.limited;
            child.depth_limited = child_result.depth_limited;
            child.hand = child_result.hand;
            // 合駒が上限や経路のために詰まなかったら、無駄合いかを調べる
            if self.shortest
                && !or_node
                && child_result.is_disproven()
                && child_result.is_conditional()
                && st.is_drop(mv)
            {
                self.mark_futile_drop(st, best, mv, len_limit)?;
            }
            let child = &self.scratch.children[best];
            if child.is_final() && child.next != NO_NEXT {
                let next = child.next;
                self.activate_delayed(start, next);
            }

            current = self.aggregate(start, or_node);
            thpn = orig_thpn;
            thdn = orig_thdn;
            if *inc_flag > 0 {
                extend_thresholds(&current, &mut thpn, &mut thdn);
            } else if orig_inc_flag > 0 {
                // TCA の延長が終わったので親へ戻る
                break;
            }
        }
        *inc_flag = (*inc_flag).min(orig_inc_flag);

        let result = self.node_result(st, key.hand, start, depth, or_node, current);
        self.store(key, depth, &result, self.nodes - nodes_before);
        if result.is_disproven()
            && result.loop_depth != NO_LOOP
            && !result.depth_limited
            && usize::from(result.loop_depth) < depth
            && let Some(tables) = self.tables
        {
            let loop_depth = usize::from(result.loop_depth);
            debug_assert_eq!(self.scratch.path.len(), depth + 1);
            tables.store_loop_disproof(
                key.key,
                &self.scratch.path[loop_depth..depth],
                LoopDisproof {
                    span: (depth - loop_depth) as u16,
                    limit: if result.limited {
                        len_limit
                    } else {
                        NO_LEN_LIMIT
                    },
                    hand: result.hand,
                },
            );
        }
        if self.shortest
            && result.is_len_bounded()
            && let Some(tables) = self.tables
        {
            tables.store_len_bound(key.key, len_limit);
        }
        if self.capture_depth == Some(depth) {
            self.capture_depth = None;
            let proven = self.scratch.children[start..].iter().filter(|c| c.pn == 0);
            self.scratch.proven.extend(proven.map(|c| (c.mv, c.mate_len)));
        }
        Ok(result)
    }

    /// 子を生成し、置換表の値で初期化する。戻り値は「浅い深さで書かれた未確定値を使ったか」。
    ///
    /// 置換表の詰みのうち、詰み手数が子の上限 (`len_limit` − 1) を超えるものは使わない (未確定として読み直す)。
    /// 最短手順モードでは、上限内に詰まないと分かっている子を不詰にし、受け方の無駄合いを詰みにする。
    fn expand(
        &mut self,
        st: &mut S,
        depth: usize,
        or_node: bool,
        len_limit: u16,
    ) -> Result<bool, Abort> {
        let child_len_limit = if len_limit == NO_LEN_LIMIT {
            NO_LEN_LIMIT
        } else {
            len_limit.saturating_sub(1)
        };
        self.scratch.moves.clear();
        st.generate(or_node, &mut self.scratch.moves);
        // 置換表を引く前に子の bucket をまとめて読み込ませ、キャッシュミスの待ちを重ねる
        self.scratch.child_keys.clear();
        for &mv in &self.scratch.moves {
            let key = if PIECES {
                st.table_key_after(mv, self.attacker)
            } else {
                exact_key(st.key_after(mv))
            };
            self.table.prefetch::<PIECES>(key);
            if let Some(tables) = self.tables {
                tables.prefetch_loop(key.key);
            }
            self.scratch.child_keys.push(key);
        }

        let child_depth = (depth + 1) as u16;
        let mut has_old_child = false;
        for i in 0..self.scratch.moves.len() {
            let mv = self.scratch.moves[i];
            let child_table_key = self.scratch.child_keys[i];
            let child_key = child_table_key.key;
            let mut child = Child {
                mv,
                pn: 1,
                dn: 1,
                mate_len: 0,
                loop_depth: NO_LOOP,
                limited: false,
                depth_limited: false,
                futile: false,
                hand: child_table_key.hand,
                first_visit: true,
                active: true,
                next: NO_NEXT,
            };
            if let Some(loop_depth) = self.repetition_depth(child_key) {
                // 経路上の局面に戻る手は読むまでもなくこの経路では不詰。先に確定させておかないと、
                // 循環を通って膨らんだ未確定値に隠れて、この手が選ばれないまま閾値の延長が続く。
                child.pn = PN_INF;
                child.dn = 0;
                child.loop_depth = loop_depth as u16;
                child.first_visit = false;
            } else {
                let probed = self.table.probe::<PIECES>(child_table_key);
                if probed.is_some() {
                    child.first_visit = false;
                }
                // 置換表の詰みは無駄合いを数える証明だけで、その手数は無駄合いを数えない手数の上限にもなる
                // (持駒違いの局面に流用しても、全応手を数える手数は攻め方の持駒が多いほど短い)
                let mut usable = probed.filter(|v| v.pn != 0 || v.mate_len <= child_len_limit);
                // 上限より長い証明しか無い局面は、上限付きの探索が前に書いた未確定値から読み直す
                // (確定値の表を先に引くので、完全一致の表に書いた未確定値はそのままでは読まれない)
                if PIECES && usable.is_none() && probed.is_some() {
                    usable = self.table.probe_unsettled(child_table_key);
                }
                if usable.is_some_and(|v| v.pn != 0 && v.dn != 0 && v.min_depth < child_depth) {
                    has_old_child = true;
                }
                if self.futility
                    && usable.is_none_or(|v| v.pn != 0)
                    && let Some(len) = self.tables.and_then(|t| t.mate_len(child_key))
                    && len <= child_len_limit
                {
                    usable = Some(TableValue {
                        pn: 0,
                        dn: PN_INF,
                        mate_len: len,
                        min_depth: 0,
                        hand: child_table_key.hand,
                    });
                }
                let bounded = self.shortest
                    && child_len_limit != NO_LEN_LIMIT
                    && usable.is_none_or(|v| v.pn != 0 && v.dn != 0)
                    && self.len_bound(child_key).is_some_and(|b| b >= child_len_limit);
                let looped = if bounded || usable.is_some_and(|v| v.pn == 0 || v.dn == 0) {
                    None
                } else {
                    self.tables.and_then(|t| {
                        t.loop_disproof(child_key, &self.scratch.path, child_len_limit)
                    })
                };
                match usable {
                    _ if bounded => {
                        child.pn = PN_INF;
                        child.dn = 0;
                        child.limited = true;
                        child.first_visit = false;
                    }
                    _ if let Some(d) = looped => {
                        child.pn = PN_INF;
                        child.dn = 0;
                        child.loop_depth = child_depth - d.span;
                        child.limited = d.limit != NO_LEN_LIMIT;
                        child.hand = d.hand;
                        child.first_visit = false;
                    }
                    Some(v) => {
                        child.pn = v.pn;
                        child.dn = v.dn;
                        child.mate_len = v.mate_len;
                        child.hand = v.hand;
                    }
                    // 置換表の値を使えない子だけ見積もる (見積もりは利きを数えるので軽くない)
                    None if self.estimate => (child.pn, child.dn) = st.initial_pn_dn(mv, or_node),
                    None => {}
                }
            }
            self.scratch.children.push(child);
        }
        if self.shortest && !or_node {
            self.mark_futile_drops(st, len_limit)?;
        }
        self.link_delayed(st, or_node);
        Ok(has_old_child)
    }

    /// 受け方の局面の子 (現在のフレームの末尾、`scratch.moves` と同じ順) のうち、展開した時点で上限内に
    /// 詰まないと分かっている合駒が無駄合いなら詰みにする。それ以外の合駒は、読んで上限内に詰まなかったときに
    /// 調べる (`search_node`)。
    ///
    /// 上限が 0 なら子は読まないので、応手がすべて合駒のときだけ、すべて調べる (駒打ち以外の応手があれば
    /// この局面は上限内に詰まない)。
    fn mark_futile_drops(&mut self, st: &mut S, len_limit: u16) -> Result<(), Abort> {
        let n = self.scratch.moves.len();
        let start = self.scratch.children.len() - n;
        let all_drops = self.scratch.moves.iter().all(|&mv| st.is_drop(mv));
        if len_limit == 0 && !all_drops {
            return Ok(());
        }
        for i in 0..n {
            let mv = self.scratch.moves[i];
            let child = &self.scratch.children[start + i];
            // 上限や経路 (合駒の後に経路上の局面へ戻る) のために詰まないと分かっている子
            let bounded = child.dn == 0 && (child.limited || child.loop_depth != NO_LOOP);
            let unsettled = child.pn != 0 && !(child.dn == 0 && !bounded);
            if !st.is_drop(mv) || !(bounded || len_limit == 0 && unsettled) {
                continue;
            }
            self.mark_futile_drop(st, start + i, mv, len_limit)?;
        }
        Ok(())
    }

    fn mark_futile_drop(
        &mut self,
        st: &mut S,
        index: usize,
        drop: S::Mv,
        len_limit: u16,
    ) -> Result<(), Abort> {
        match self.futile_drop(st, drop, len_limit)? {
            Some((proof, len)) => self.scratch.children[index].set_futile(proof, len),
            // 上限を広げると無駄合いになりうる (取った後の詰みが上限内に収まる) ので、この合駒で
            // 詰まないことは上限に依存する。上限に依存しない不詰として覚えると、上限の広い同じ局面を誤って不詰にする
            None if self.futility && len_limit != NO_LEN_LIMIT => {
                let child = &mut self.scratch.children[index];
                if child.dn == 0 {
                    child.limited = true;
                }
            }
            None => {}
        }
        Ok(())
    }

    /// 詰み手数の上限が `len_limit` の受け方の局面 `st` (王手されている) で、合駒 `drop` が無駄合いなら、
    /// 合駒の後の局面の証明駒と、合駒を取って取った駒を戻した局面の詰み手数を返す。
    ///
    /// 無駄合い: 王手している駒が合駒を取り (成・不成のどちらか)、受け方がその駒を取り返せず、取った駒を
    /// 受け方の持駒に戻した局面 M の詰み手数が、駒打ち以外の応手で逃れたときの詰み手数 (応手の詰み手数 + 1 の
    /// 最大、無ければ 0) 以下。
    /// 合駒を取る 2 手が挟まるだけで、合駒が無かったときより詰みが長くならない合駒だけが無駄合いになり、
    /// 受け方の局面の詰み手数を決めることはない。取り返せる合駒と、取った後の詰みが長くなる合駒は有効な合駒として数える。
    /// M が `len_limit` の中で詰まなければ、無駄合いでもこの局面は上限内に詰まないので、無駄合いとしない。
    /// M と応手の後の局面は手順から外れた局面なので、経路を持たずに、この探索全体の制限の中で読み切る
    /// (読み切れなければ `Abort`)。盤上の駒の総数は変わらないので、置換表はそのまま共有できる。
    pub(super) fn futile_drop(
        &mut self,
        st: &mut S,
        drop: S::Mv,
        len_limit: u16,
    ) -> Result<Option<(Hand, u16)>, Abort> {
        if !self.futility {
            return Ok(None);
        }
        let before = st.table_key(self.attacker);
        let Some(tables) = self.tables else {
            return Ok(None);
        };
        // 同じ判定を探索中に何度も繰り返すので覚えておく
        let memo_key = (before.key ^ st.move_key(drop).wrapping_mul(0x9E37_79B9_7F4A_7C15))
            .wrapping_add(u64::from(len_limit).wrapping_mul(0xC2B2_AE3D_27D4_EB4F));
        if let Some(e) = tables.futility(memo_key) {
            return Ok(e.futile.then_some((e.proof, e.len)));
        }
        st.do_move(drop);
        let result = self.futile_after_drop(st, drop, before, len_limit);
        st.undo_move(drop);
        let mut found = result?;
        if let Some((_, len)) = found
            && len > 0
            && !self.has_reply_lasting(st, len)?
        {
            found = None;
        }
        let (proof, len) = found.unwrap_or((Hand::EMPTY, 0));
        tables.store_futility(
            memo_key,
            Futility {
                futile: found.is_some(),
                proof,
                len,
            },
        );
        Ok(found)
    }

    fn futile_after_drop(
        &mut self,
        st: &mut S,
        drop: S::Mv,
        before: TableKey,
        len_limit: u16,
    ) -> Result<Option<(Hand, u16)>, Abort> {
        let mut captures = std::mem::take(&mut self.scratch.capture_moves);
        captures.clear();
        st.interposition_captures(drop, &mut captures);
        // 取った駒を戻した局面の領域。判定のたびに局面を確保し直さない
        let mut slot = match self.scratch.returned.take().map(|b| b.downcast::<Option<S>>()) {
            Some(Ok(slot)) => slot,
            _ => Box::new(None),
        };
        let mut result = Ok(None);
        for &capture in &captures {
            // 取った後の局面の盤面に、合駒の前の持駒 (取った駒は受け方の手) を組み合わせた局面
            let after = st.table_key_after(capture, self.attacker);
            let returned = TableKey {
                key: after.board ^ before.key ^ before.board,
                board: after.board,
                hand: before.hand,
            };
            let probe_key = if PIECES {
                returned
            } else {
                exact_key(returned.key)
            };
            match self.table.probe::<PIECES>(probe_key) {
                Some(v) if v.dn == 0 => continue,
                _ if self.len_bound(returned.key).is_some_and(|b| b >= len_limit) => {
                    continue;
                }
                _ => {}
            }
            st.do_move(capture);
            // 合駒を取った駒を受け方が取り返せるなら、取った後の局面は合駒が無かったときと別物なので有効な合駒
            let built = !st.can_recapture(drop)
                && st.captured_piece_returned_into(self.attacker, &mut slot);
            st.undo_move(capture);
            let Some(returned_state) = Option::as_mut(&mut *slot).filter(|_| built) else {
                continue;
            };
            debug_assert_eq!(returned_state.key(), returned.key);
            match self.shortest_detached(returned_state, len_limit) {
                Ok(Some((proof, len))) => {
                    let proof = if PIECES { proof } else { before.hand };
                    // 取る手が複数 (成・不成) なら、詰み手数の短い方
                    let shorter = match result {
                        Ok(Some((_, best))) => len < best,
                        _ => true,
                    };
                    if shorter {
                        result = Ok(Some((proof, len)));
                    }
                }
                Ok(None) => {}
                Err(abort) => {
                    result = Err(abort);
                    break;
                }
            }
        }
        self.scratch.capture_moves = captures;
        self.scratch.returned = Some(slot);
        result
    }

    /// 受け方の手番の局面 `st` の、`len_limit` 手以内の最短の詰み手数とその証明駒 (上限を 2 手ずつ縮めて
    /// 証明し直す)
    fn shortest_detached(
        &mut self,
        st: &mut S,
        len_limit: u16,
    ) -> Result<Option<(Hand, u16)>, Abort> {
        let Detached::Mated(proof, len) = self.solve_detached(st, false, len_limit)? else {
            return Ok(None);
        };
        let mut best = (proof, len);
        while best.1 >= 2 {
            match self.solve_detached(st, false, best.1 - 2)? {
                Detached::Mated(proof, len) => best = (proof, len),
                Detached::Escaped => break,
            }
        }
        Ok(Some(best))
    }

    /// 受け方の局面 `st` に、`len` 手より短く詰まない (詰み手数 + 1 が `len` 以上の) 駒打ち以外の応手があるか
    fn has_reply_lasting(&mut self, st: &mut S, len: u16) -> Result<bool, Abort> {
        let mut replies = std::mem::take(&mut self.scratch.reply_moves);
        replies.clear();
        generate_replies(st, &mut replies);
        let mut result = Ok(false);
        for &mv in &replies {
            if st.is_drop(mv) {
                continue;
            }
            if len < 4 {
                result = Ok(true);
                break;
            }
            st.do_move(mv);
            let solved = self.solve_detached(st, true, len - 3);
            st.undo_move(mv);
            match solved {
                Ok(Detached::Escaped) => {
                    result = Ok(true);
                    break;
                }
                Ok(Detached::Mated(..)) => {}
                Err(abort) => {
                    result = Err(abort);
                    break;
                }
            }
        }
        self.scratch.reply_moves = replies;
        result
    }

    /// 局面 `st` (`or_node` なら攻め方の手番) を経路と無関係に `len_limit` 手の上限で解く。無駄合いの数え方と
    /// 最短手順モードの表はこの探索と同じ。この探索全体の制限 (残りのノード数・時間・停止) の中で読み切り、
    /// 制限に達したら `Abort`。
    fn solve_detached(
        &mut self,
        st: &mut S,
        or_node: bool,
        len_limit: u16,
    ) -> Result<Detached, Abort> {
        let mut scratch = self.scratch.sub.take().unwrap_or_else(|| Box::new(Scratch::new()));
        let remaining = match self.budget.node_limit {
            0 => 0,
            limit => limit.saturating_sub(self.nodes).max(1),
        };
        let (table, attacker, tables) = (self.table, self.attacker, self.tables);
        let (estimate, weak_disproof_step) = (self.estimate, self.weak_disproof_step);
        let (stop, halt, cancel, start) =
            (self.budget.stop, self.budget.halt, self.budget.cancel, self.budget.start);
        let (shared_nodes, time_limit_ms) = (self.budget.shared_nodes, self.budget.time_limit_ms);
        // 部分探索のノード数は 0 から数えるので、進捗にはこの探索で読んだ分を足して渡す (並列に解くときは
        // 部分探索も全スレッドの合計で知らせる)。通知の間隔は部分探索に引き継いで戻すので、入れ子になっても
        // 探索全体で約 1 秒ごとになる
        let base = if shared_nodes.is_some() {
            0
        } else {
            self.nodes
        };
        let parent_progress = &mut *self.budget.progress;
        let mut forward = |p: MateProgress| {
            parent_progress(MateProgress {
                nodes: base + p.nodes,
                ..p
            })
        };
        let budget = Budget {
            stop,
            halt,
            cancel,
            shared_nodes,
            node_limit: remaining,
            time_limit_ms,
            start,
            progress: &mut forward,
        };
        let (result, nodes, reported, next_progress_ms) = {
            let mut sub = Searcher::<S, PIECES>::new(table, &mut scratch, attacker, budget);
            sub.estimate = estimate;
            sub.weak_disproof_step = weak_disproof_step;
            sub.shortest = true;
            sub.futility = true;
            sub.tables = tables;
            sub.next_progress_ms = self.next_progress_ms;
            let result = sub.search(st, NodeArgs::unbounded(0, or_node, len_limit), &mut 0);
            (result, sub.nodes, sub.reported, sub.next_progress_ms)
        };
        self.next_progress_ms = next_progress_ms;
        self.scratch.sub = Some(scratch);
        self.nodes += nodes;
        // 部分探索が全スレッドの合計に足し込んだ分は、この探索から足し込み直さない
        self.reported += reported;
        match result? {
            r if r.is_proven() => Ok(Detached::Mated(r.hand, r.mate_len)),
            _ => Ok(Detached::Escaped),
        }
    }

    /// 局面 `st` を閾値なしで、補助スレッドと置換表・最短手順モードの表を共有して読む (補助が無ければ
    /// `search` と同じ)。どれかのスレッドが結論を出したらほかを止める。探索の途中 (フレームの中) からは呼べない。
    pub fn search_shared(&mut self, st: &mut S, args: NodeArgs) -> Result<NodeResult, Abort>
    where
        S: Clone + Send,
    {
        if self.helpers.is_empty() {
            return self.search(st, args, &mut 0);
        }
        if self.serial_nodes > 0 {
            // 易しい局面 (置換表で数ノードのうちに決まる読みが多い) はスレッドを起こす費用の方が大きいので、
            // 先に 1 スレッドで読む。打ち切ると `within` が証明済みの子を集める指定を消すので戻す
            let capture = self.capture_depth;
            if let Some(result) = self.within(self.serial_nodes, |s| s.search(st, args, &mut 0))? {
                return Ok(result);
            }
            if let Some(depth) = capture {
                self.capture_proven(depth);
            }
        }
        let halt = AtomicBool::new(false);
        let cancel = AtomicBool::new(false);
        // この探索器の上限の残りを全スレッドの合計に掛ける
        let limit = match self.budget.node_limit {
            0 => 0,
            limit => limit.saturating_sub(self.nodes).max(1),
        };
        let shared_nodes = AtomicU64::new(0);
        let (table, attacker, settings) = (self.table, self.attacker, self.settings());
        let capture = self.capture_depth.take();
        let (stop, start, time_limit_ms) =
            (self.budget.stop, self.budget.start, self.budget.time_limit_ms);
        let slots: Vec<OnceLock<(Result<NodeResult, Abort>, u64)>> =
            (0..self.helpers.len()).map(|_| OnceLock::new()).collect();
        let helpers: Vec<Box<dyn FnOnce() + Send + '_>> = self
            .helpers
            .iter_mut()
            .zip(&slots)
            .enumerate()
            .map(|(i, (scratch, slot))| {
                let mut st = st.clone();
                let (halt, cancel, shared_nodes) = (&halt, &cancel, &shared_nodes);
                Box::new(move || {
                    // panic したら呼び出したスレッドも止める (止めないと scope の終わりで待ち続ける)
                    let _cancel_on_panic = super::CancelOnPanic(cancel);
                    let mut quiet = |_| {};
                    let budget = Budget {
                        stop,
                        halt: Some(halt),
                        cancel: Some(cancel),
                        shared_nodes: Some((shared_nodes, limit)),
                        node_limit: 0,
                        time_limit_ms,
                        start,
                        progress: &mut quiet,
                    };
                    let mut sub = Searcher::<S, PIECES>::new(table, scratch, attacker, budget);
                    sub.apply_settings(settings);
                    if let Some(depth) = capture {
                        sub.capture_proven(depth);
                    }
                    let (num, den) = super::HELPER_SLACKS[i % super::HELPER_SLACKS.len()];
                    sub.set_threshold_slack(num, den);
                    sub.set_tie_seed(i as u32 + 1);
                    let result = sub.search(&mut st, args, &mut 0);
                    if result.is_ok() {
                        halt.store(true, Ordering::Relaxed);
                    }
                    let _ = slot.set((result, sub.nodes));
                }) as Box<dyn FnOnce() + Send + '_>
            })
            .collect();
        let base = self.nodes;
        let next_progress_ms = self.next_progress_ms;
        let parent_progress = &mut *self.budget.progress;
        let scratch = &mut *self.scratch;
        let (main_result, main_nodes, next_progress_ms) =
            super::run_with_helpers(helpers, &cancel, || {
                let mut forward = |p: MateProgress| {
                    parent_progress(MateProgress {
                        nodes: base + p.nodes,
                        ..p
                    })
                };
                let budget = Budget {
                    stop,
                    halt: Some(&halt),
                    cancel: Some(&cancel),
                    shared_nodes: Some((&shared_nodes, limit)),
                    node_limit: 0,
                    time_limit_ms,
                    start,
                    progress: &mut forward,
                };
                let mut sub = Searcher::<S, PIECES>::new(table, scratch, attacker, budget);
                sub.apply_settings(settings);
                if let Some(depth) = capture {
                    sub.capture_proven(depth);
                }
                sub.next_progress_ms = next_progress_ms;
                let result = sub.search(st, args, &mut 0);
                // 先に結論を出した補助スレッドも、打ち切られた補助スレッドも、ここで止める
                halt.store(true, Ordering::Relaxed);
                (result, sub.nodes, sub.next_progress_ms)
            });
        self.next_progress_ms = next_progress_ms;
        let mut result = main_result;
        self.nodes += main_nodes;
        for (i, slot) in slots.into_iter().enumerate() {
            let (r, n) = slot.into_inner().unwrap_or((Err(Abort), 0));
            self.nodes += n;
            if result.is_err() && r.is_ok() {
                result = r;
                // 返す結果と証明済みの子を同じスレッドのものにそろえる (手数の違う別の証明の子が混ざらないように)
                if capture.is_some() {
                    self.scratch.proven.clone_from(&self.helpers[i].proven);
                }
            }
        }
        result
    }

    /// 現在のフレームの子 (`scratch.moves` と同じ順) で後回しの組を作り、組の中で前の手が
    /// 確定していない手を数えないようにする。
    fn link_delayed(&mut self, st: &S, or_node: bool) {
        let n = self.scratch.moves.len();
        let start = self.scratch.children.len() - n;
        self.scratch.delay_order.clear();
        for (i, &mv) in self.scratch.moves.iter().enumerate() {
            if let Some((group, rank)) = st.delay_key(mv, or_node) {
                self.scratch.delay_order.push((group, rank, i as u16));
            }
        }
        if self.scratch.delay_order.len() < 2 {
            return;
        }
        // 組ごとに読む順に並べ、隣り合う同じ組の手をつなぐ
        self.scratch.delay_order.sort_unstable();
        for k in 1..self.scratch.delay_order.len() {
            let (group, _, i) = self.scratch.delay_order[k];
            let (prev_group, _, prev) = self.scratch.delay_order[k - 1];
            if group == prev_group {
                self.scratch.children[start + usize::from(prev)].next = i;
            }
        }

        // 確定していない手が組の中で先に出たら、それより後ろの確定していない手は数えない
        let mut current_group = None;
        let mut blocked = false;
        for &(group, _, i) in &self.scratch.delay_order {
            if current_group != Some(group) {
                current_group = Some(group);
                blocked = false;
            }
            let child = &mut self.scratch.children[start + usize::from(i)];
            if !child.is_final() {
                child.active = !blocked;
                blocked = true;
            }
        }
    }

    /// 後回しの組で、`next` から先の手を確定していない手が 1 つ数えられるまで数える
    fn activate_delayed(&mut self, start: usize, mut next: u16) {
        while next != NO_NEXT {
            let child = &mut self.scratch.children[start + usize::from(next)];
            child.active = true;
            if !child.is_final() {
                break;
            }
            next = child.next;
        }
    }

    /// 子の値から現局面の pn/dn を求める (OR: pn=min, dn=和 / AND: pn=和, dn=min)。
    /// `count_disproof_weakly` なら OR の dn は WPNS で数える。
    fn aggregate(&self, start: usize, or_node: bool) -> NodeResult {
        let children = &self.scratch.children[start..];
        let weak = or_node && self.weak_disproof_step != 0;
        let mut min = PN_INF;
        let mut sum = 0u64;
        let mut has_inf = false;
        let mut max = 0u32;
        let mut nonzero = 0u64;
        for c in children.iter().filter(|c| c.active) {
            let (phi, delta) = if or_node { (c.pn, c.dn) } else { (c.dn, c.pn) };
            min = min.min(phi);
            sum = add_pn(sum, delta);
            has_inf |= delta == PN_INF;
            max = max.max(delta);
            nonzero += u64::from(delta != 0);
        }
        let summed = if weak {
            let others = nonzero.saturating_sub(1) * self.weak_disproof_step;
            clamp_sum(u64::from(max) + others, has_inf)
        } else {
            clamp_sum(sum, has_inf)
        };
        let (pn, dn) = if or_node {
            (min, summed)
        } else {
            (summed, min)
        };
        NodeResult {
            pn,
            dn,
            mate_len: 0,
            loop_depth: NO_LOOP,
            limited: false,
            depth_limited: false,
            hand: Hand::EMPTY,
        }
    }

    /// 次に読む子 (`scratch.children` の添字) と、その子に渡す閾値を選ぶ
    fn select(&self, start: usize, or_node: bool, thpn: u32, thdn: u32) -> (usize, u32, u32) {
        let children = &self.scratch.children[start..];
        // φ = OR なら pn、AND なら dn (小さいほど有望)。δ はその相方。
        let phi = |c: &Child<S::Mv>| if or_node { c.pn } else { c.dn };
        let delta = |c: &Child<S::Mv>| if or_node { c.dn } else { c.pn };
        let (thphi, thdelta) = if or_node { (thpn, thdn) } else { (thdn, thpn) };

        let mut best = 0;
        let mut best_phi = PN_INF;
        let mut second_phi = PN_INF;
        let mut sum_delta = 0u64;
        let mut nonzero = 0u64;
        let seed = self.tie_seed;
        // 奇数を掛けるので順位は子ごとに異なり、添字が seed の子が最上位になる (スレッドごとに別の子を選ぶ)
        let rank = |i: usize| (i as u32 ^ seed).wrapping_mul(0x9E37_79B1);
        for (i, c) in children.iter().enumerate().filter(|(_, c)| c.active) {
            nonzero += u64::from(delta(c) != 0);
            let p = phi(c);
            if p < best_phi || (seed != 0 && p == best_phi && rank(i) < rank(best)) {
                second_phi = best_phi;
                best_phi = p;
                best = i;
            } else if p < second_phi {
                second_phi = p;
            }
            sum_delta = add_pn(sum_delta, delta(c));
        }

        // 1+ε (Pawlewicz & Lew 2007): 2 番目の子の φ を 1 超えた時点で戻ると、最善の子が入れ替わるたびに
        // 子を少し読んでは戻る往復を繰り返す。2 番目の φ の約 1/3 (最低 1) の余裕を持たせる (経験的に 1/3 前後が良く、
        // 1/2 以上では増える)。
        let widen = match self.slack {
            None => u64::from(second_phi / 3),
            Some(q16) => (u64::from(second_phi) * q16) >> 16,
        }
        .max(1);
        let child_thphi = thphi.min((u64::from(second_phi) + widen).min(u64::from(PN_INF)) as u32);
        let child_thdelta = if thdelta == PN_INF {
            PN_INF
        } else {
            // 和で数えるときは thδ − (δ の和 − 最善の子の δ)。ループ条件 (δ の和 < thδ) より正になる。
            // WPNS では thδ − (δ が 0 でない子の数 − 1) × 加算量: 最善の子の δ がこれに届けば
            // 現局面の δ が thδ に届く
            let others = if or_node && self.weak_disproof_step != 0 {
                nonzero.saturating_sub(1) * self.weak_disproof_step
            } else {
                sum_delta - u64::from(delta(&children[best]))
            };
            (u64::from(thdelta).saturating_sub(others)).min(u64::from(PN_INF)) as u32
        };
        if or_node {
            (start + best, child_thphi, child_thdelta)
        } else {
            (start + best, child_thdelta, child_thphi)
        }
    }

    /// ループを抜けた後の現局面の結果 (詰み手数・不詰の条件・持駒の条件を含む)。
    /// `hand` は局面の攻め方の持駒。
    fn node_result(
        &self,
        st: &S,
        hand: Hand,
        start: usize,
        depth: usize,
        or_node: bool,
        current: NodeResult,
    ) -> NodeResult {
        let children = &self.scratch.children[start..];
        // 受け方の詰み・攻め方の不詰はすべての子が根拠なので、後回しの組の手もすべて数えられている
        debug_assert!(
            !(current.is_proven() && !or_node || current.is_disproven() && or_node)
                || children.iter().all(|c| c.active)
        );
        // 受け方の局面の詰み手数は応手の詰み手数 + 1 の最大 (応手が無ければ 0)。無駄合いは合駒を取って戻した
        // 局面の詰み手数で数える (合駒と取る 2 手は数えない)。その手数は駒打ち以外の応手の手数以下なので、
        // 無駄合いが受け方の局面の手数を決めることはない
        let defense_len = || {
            children
                .iter()
                .map(|c| {
                    if c.futile {
                        c.mate_len
                    } else {
                        c.mate_len.saturating_add(1)
                    }
                })
                .max()
                .unwrap_or(0)
        };
        if current.is_proven() && !PIECES {
            let len = if or_node {
                children
                    .iter()
                    .filter(|c| c.pn == 0)
                    .map(|c| c.mate_len)
                    .min()
                    .unwrap_or(0)
                    .saturating_add(1)
            } else {
                defense_len()
            };
            return NodeResult::proven(len, hand);
        }
        if current.is_proven() {
            if or_node {
                // 詰み手数と証明駒は同じ子から取る (証明駒で流用した詰みもその手数で詰む)
                let best = children
                    .iter()
                    .filter(|c| c.pn == 0)
                    .min_by_key(|c| c.mate_len)
                    .expect("詰みの OR ノードには詰む子がある");
                let proof = st.hand_before(best.mv, best.hand);
                return NodeResult::proven(best.mate_len.saturating_add(1), proof);
            }
            let proof = children.iter().fold(Hand::EMPTY, |h, c| hand_max(h, c.hand));
            let proof = st.proof_hand_for_evasions(proof, self.attacker);
            return NodeResult::proven(defense_len(), proof);
        }
        if current.is_disproven() {
            // OR の不詰はすべての子が根拠 (条件はすべて引き継ぐ)。AND の不詰は逃れの子の
            // どれか 1 つが根拠になるので、条件の最も弱い子を選ぶ。
            let escape = || {
                children
                    .iter()
                    .filter(|c| c.dn == 0)
                    .max_by_key(|c| (!c.limited, c.loop_depth))
                    .expect("不詰の AND ノードには逃れの子がある")
            };
            let (loop_depth, limited, depth_limited) = if or_node {
                (
                    children.iter().map(|c| c.loop_depth).min().unwrap_or(NO_LOOP),
                    children.iter().any(|c| c.limited),
                    children.iter().any(|c| c.depth_limited),
                )
            } else {
                let e = escape();
                (e.loop_depth, e.limited, e.depth_limited)
            };
            let disproof = if !PIECES {
                hand
            } else if or_node {
                let disproof = children
                    .iter()
                    .fold(FULL_HAND, |h, c| hand_min(h, st.hand_before(c.mv, c.hand)));
                st.disproof_hand_for_checks(disproof, self.attacker)
            } else {
                let e = escape();
                st.disproof_hand_for_escape(e.mv, e.hand, self.attacker)
            };
            // ループの始点がこのノードかそれより深ければ、このノードから読み直しても同じ循環になる
            let loop_depth = if usize::from(loop_depth) >= depth {
                NO_LOOP
            } else {
                loop_depth
            };
            let result = NodeResult {
                depth_limited,
                ..NodeResult::disproven(loop_depth, limited, disproof)
            };
            return if result.is_conditional() {
                NodeResult { hand, ..result }
            } else {
                result
            };
        }
        NodeResult { hand, ..current }
    }

    fn store(&mut self, key: TableKey, depth: usize, result: &NodeResult, searched: u64) {
        if result.is_conditional() {
            return;
        }
        if self.futility && result.is_proven() {
            if let Some(tables) = self.tables {
                tables.store_mate_len(key.key, result.mate_len);
            }
            return;
        }
        self.table.store::<PIECES>(
            key,
            TableValue {
                pn: result.pn,
                dn: result.dn,
                mate_len: result.mate_len,
                min_depth: depth.min(u16::MAX as usize) as u16,
                hand: result.hand,
            },
            searched,
        );
    }
}

/// 受け方の応手を生成する。
///
/// 呼び出し側 (`Searcher::futile_drop`) のフレームは無駄合いの判定の入れ子 (`Searcher::solve_detached`)
/// のあいだスタックに残るので、生成の作業領域 (指し手の最大数ぶんの配列) をそこへ inline させない。
/// inline されると、入れ子 1 段ごとにその作業領域がスタックに積み重なる。
#[inline(never)]
fn generate_replies<S: MateState>(st: &S, out: &mut Vec<S::Mv>) {
    st.generate(false, out);
}

/// TCA: 現局面の値を少しだけ超えるまで閾値を延ばす
fn extend_thresholds(current: &NodeResult, thpn: &mut u32, thdn: &mut u32) {
    if current.is_final() {
        return;
    }
    *thpn = (*thpn).max(current.pn.saturating_add(1));
    *thdn = (*thdn).max(current.dn.saturating_add(1));
}

/// 駒種ごとの多い方
fn hand_max(a: Hand, b: Hand) -> Hand {
    PieceType::HAND_PIECES
        .iter()
        .fold(a, |h, &pt| h.set(pt, a.count(pt).max(b.count(pt))))
}

/// 駒種ごとの少ない方
fn hand_min(a: Hand, b: Hand) -> Hand {
    PieceType::HAND_PIECES
        .iter()
        .fold(a, |h, &pt| h.set(pt, a.count(pt).min(b.count(pt))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loop_disproof_needs_same_path_segment() {
        let tables = ShortestTables::new();
        tables.reset();
        let disproof = LoopDisproof {
            span: 2,
            limit: 9,
            hand: Hand::from_raw(5),
        };
        tables.store_loop_disproof(7, &[11, 13], disproof);
        let found = tables.loop_disproof(7, &[3, 11, 13], 9).unwrap();
        assert_eq!((found.span, found.limit, found.hand), (2, 9, Hand::from_raw(5)));
        assert!(tables.loop_disproof(7, &[11, 13], 10).is_none());
        assert!(tables.loop_disproof(7, &[11, 17], 9).is_none());
        assert!(tables.loop_disproof(7, &[13], 9).is_none());
        assert!(tables.loop_disproof(8, &[11, 13], 9).is_none());
        tables.reset();
        assert!(tables.loop_disproof(7, &[11, 13], 9).is_none());
    }
}
