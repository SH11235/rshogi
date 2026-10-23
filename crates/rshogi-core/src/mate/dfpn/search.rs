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
//! - 打ち切り (ノード数・時間・停止) は `Abort` で全体を中断する。
//!   上限に達したノードに値を返させると、不詰と取り違えたり、同じ子を選び続けたりする。

use std::sync::atomic::{AtomicBool, Ordering};

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
const CHECK_INTERVAL: u64 = 1024;
/// 子の配列 (全フレーム分を 1 本に積む) にあらかじめ確保しておく要素数。
/// 経路上の各局面の候補手の合計で、超えたときだけ伸ばす (伸ばした領域はソルバーが持ち回る)。
const CHILDREN_RESERVE: usize = 1 << 16;
/// 進捗を知らせる間隔 (ミリ秒)
const PROGRESS_INTERVAL_MS: u64 = 1000;

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
            hand,
        }
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
    /// 0 は無制限
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
}

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
        }
    }
}

/// 1 回の solve の探索状態。
///
/// `PIECES` なら確定値を証明駒・反証駒で置換表に書き、持駒違いの局面に流用する
/// (難しい問題でノードが減るが、置換表を引くたびに読む量が増えて 1 ノードあたりは重い)。
pub(super) struct Searcher<'a, S: MateState, const PIECES: bool> {
    table: &'a mut MateTable,
    scratch: &'a mut Scratch<S::Mv>,
    /// 攻め方 (ルートの手番)。置換表の持駒はこちらの持駒で表す
    attacker: Color,
    nodes: u64,
    next_check: u64,
    next_progress_ms: u64,
    budget: Budget<'a>,
    /// この深さのフレームが終わるとき、証明済みの子を `scratch.proven` に写す (PV 復元用)
    capture_depth: Option<usize>,
    /// 置換表に無い子の pn/dn を局面から見積もるか (`MateState::initial_pn_dn`)
    estimate: bool,
    /// 攻め方の局面の dn を子の和でなく WPNS で数えるときの、子 1 つあたりの加算量 (0 なら和で数える)
    weak_disproof_step: u64,
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
        table: &'a mut MateTable,
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
            // 最初のノードで停止フラグを確かめる (開始前に立っていたら何も返さない)
            next_check: 0,
            next_progress_ms: PROGRESS_INTERVAL_MS,
            budget,
            capture_depth: None,
            estimate: false,
            weak_disproof_step: 0,
        }
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

    /// 前の探索で `nodes` ノード読んだ続きとして数える (ノード数の上限と進捗はその合計に掛かる)
    pub fn continue_from(&mut self, nodes: u64) {
        self.nodes = nodes;
        self.next_check = nodes;
    }

    /// これまでに探索したノード数
    pub fn nodes(&self) -> u64 {
        self.nodes
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

    fn count_node(&mut self) -> Result<(), Abort> {
        self.nodes += 1;
        if self.budget.node_limit != 0 && self.nodes > self.budget.node_limit {
            return Err(Abort);
        }
        if self.nodes >= self.next_check {
            self.next_check = self.nodes + CHECK_INTERVAL;
            if self.budget.stop.load(Ordering::Relaxed) {
                return Err(Abort);
            }
            let elapsed_ms = self.budget.start.elapsed().as_millis() as u64;
            if self.budget.time_limit_ms != 0 && elapsed_ms >= self.budget.time_limit_ms {
                return Err(Abort);
            }
            if elapsed_ms >= self.next_progress_ms {
                self.next_progress_ms = elapsed_ms + PROGRESS_INTERVAL_MS;
                (self.budget.progress)(MateProgress {
                    nodes: self.nodes,
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
            return Ok(NodeResult::disproven(NO_LOOP, true, table_key.hand));
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

        let has_old_child = self.expand(st, depth, or_node, args.child_len_limit());
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
        if len_limit == 0 {
            // 受け方に応手があり、手数の上限で攻め方はもう指せない
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
            child.hand = child_result.hand;
            if child_result.is_final() && child.next != NO_NEXT {
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
        if self.capture_depth == Some(depth) {
            self.capture_depth = None;
            let proven = self.scratch.children[start..].iter().filter(|c| c.pn == 0);
            self.scratch.proven.extend(proven.map(|c| (c.mv, c.mate_len)));
        }
        Ok(result)
    }

    /// 子を生成し、置換表の値で初期化する。戻り値は「浅い深さで書かれた未確定値を使ったか」。
    ///
    /// 置換表の詰みのうち、詰み手数が `child_len_limit` を超えるものは使わない (未確定として読み直す)。
    fn expand(&mut self, st: &S, depth: usize, or_node: bool, child_len_limit: u16) -> bool {
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
                if let Some(v) = probed {
                    child.first_visit = false;
                    if v.pn != 0 && v.dn != 0 && v.min_depth < child_depth {
                        has_old_child = true;
                    }
                }
                match probed {
                    Some(v) if v.pn != 0 || v.mate_len <= child_len_limit => {
                        child.pn = v.pn;
                        child.dn = v.dn;
                        child.mate_len = v.mate_len;
                        child.hand = v.hand;
                    }
                    // 置換表の値を使えない子だけ見積もる (見積もりは利きを数えるので軽くない)
                    _ if self.estimate => (child.pn, child.dn) = st.initial_pn_dn(mv, or_node),
                    _ => {}
                }
            }
            self.scratch.children.push(child);
        }
        self.link_delayed(st, or_node);
        has_old_child
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
        for (i, c) in children.iter().enumerate().filter(|(_, c)| c.active) {
            nonzero += u64::from(delta(c) != 0);
            let p = phi(c);
            if p < best_phi {
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
        let child_thphi = thphi.min(second_phi.saturating_add((second_phi / 3).max(1)));
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
        if current.is_proven() && !PIECES {
            let len = if or_node {
                children.iter().filter(|c| c.pn == 0).map(|c| c.mate_len).min()
            } else {
                children.iter().map(|c| c.mate_len).max()
            };
            return NodeResult::proven(len.unwrap_or(0).saturating_add(1), hand);
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
            let len = children.iter().map(|c| c.mate_len).max().unwrap_or(0);
            let proof = children.iter().fold(Hand::EMPTY, |h, c| hand_max(h, c.hand));
            let proof = st.proof_hand_for_evasions(proof, self.attacker);
            return NodeResult::proven(len.saturating_add(1), proof);
        }
        if current.is_disproven() {
            // OR の不詰はすべての子が根拠 (条件はすべて引き継ぐ)。AND の不詰は逃れの子の
            // どれか 1 つが根拠になるので、条件の最も弱い子を選ぶ。
            let (loop_depth, limited, disproof) = if !PIECES {
                let (loop_depth, limited) = if or_node {
                    (
                        children.iter().map(|c| c.loop_depth).min().unwrap_or(NO_LOOP),
                        children.iter().any(|c| c.limited),
                    )
                } else {
                    children
                        .iter()
                        .filter(|c| c.dn == 0)
                        .map(|c| (c.loop_depth, c.limited))
                        .max_by_key(|&(loop_depth, limited)| (!limited, loop_depth))
                        .expect("不詰の AND ノードには逃れの子がある")
                };
                (loop_depth, limited, hand)
            } else if or_node {
                let disproof = children
                    .iter()
                    .fold(FULL_HAND, |h, c| hand_min(h, st.hand_before(c.mv, c.hand)));
                (
                    children.iter().map(|c| c.loop_depth).min().unwrap_or(NO_LOOP),
                    children.iter().any(|c| c.limited),
                    st.disproof_hand_for_checks(disproof, self.attacker),
                )
            } else {
                let escape = children
                    .iter()
                    .filter(|c| c.dn == 0)
                    .max_by_key(|c| (!c.limited, c.loop_depth))
                    .expect("不詰の AND ノードには逃れの子がある");
                (
                    escape.loop_depth,
                    escape.limited,
                    st.disproof_hand_for_escape(escape.mv, escape.hand, self.attacker),
                )
            };
            // ループの始点がこのノードかそれより深ければ、このノードから読み直しても同じ循環になる
            let loop_depth = if usize::from(loop_depth) >= depth {
                NO_LOOP
            } else {
                loop_depth
            };
            let result = NodeResult::disproven(loop_depth, limited, disproof);
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
