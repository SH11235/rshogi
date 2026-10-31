//! 最短手順モード: 詰将棋のルールでの正解手順 ([`ShortestMode::Tsume`]) と、すべての応手を数える最短手順
//! ([`ShortestMode::All`]。下の 1・2 だけを使い、無駄合いの判定をせず駒余りも比べない)
//!
//! 正解手順の定義:
//! 1. 攻め方は王手を続け、最短の手数で詰ます。
//! 2. 受け方は最も長く逃れる応手を選ぶ。
//! 3. 無駄合いは受け方の手として数えない。無駄合いは、合駒を取った駒を受け方が取り返せず、取った駒を
//!    受け方に戻した局面の詰み手数が駒打ち以外の応手で逃れたときの詰み手数以下の合駒
//!    (`Searcher::futile_drop`)。無駄合いは受け方の局面の詰み手数を決めないので、手順の受け方の手は
//!    無駄合いでない応手から選べる。
//! 4. 受け方の応手で手数が同じなら、詰上りで攻め方に持駒が余らない方を選ぶ。
//!
//! 局面の最短の詰み手数 (Tsume では無駄合いを数えない) を「詰み手数」と呼ぶ。
//! - 根の詰み手数: 判定で証明した手数 L から、上限を L − 2 にして証明し直すことを、証明できなくなるまで
//!   繰り返す。上限付きの探索は無駄合いを手数に数えず、上限だけに依存する不詰を覚えておく
//!   (`Searcher::enable_shortest`)。置換表の詰みの手数は証明の長さ (詰み手数の上限) なので、
//!   上限付きの探索は上限を超える詰みを使わない。Tsume では作意推定の手順の長さを先に上限にする
//!   (`tsume_len`)。
//! - 手順: 詰み手数 d の局面で、攻め方は d − 1 手以内に詰む王手 (詰み手数はちょうど d − 1)、受け方は
//!   無駄合い (上限 d で判定) でない応手のうち d − 3 手以内に詰まないもの (ちょうど d − 1) から選ぶ。
//!
//! 4 の「持駒が余らない」は、攻め方も受け方も上の候補から選ぶ手順のうち、詰上りで攻め方の持駒が
//! 空になるものがあるかで決める (攻め方も余らない手順を選ぶ)。この探索の制限の中で、余らない手順が
//! 無いと分かるまで候補を調べる (攻め方の持駒が残りの攻め方の手数より多ければ必ず余るので調べない)。

use std::collections::HashMap;

use super::search::{Abort, NO_LEN_LIMIT, NodeArgs, Searcher, ShortestTables};
use super::state::MateState;
use super::{ShortestMode, estimate};
use crate::types::{Color, Hand, PieceType};

// テストの局面は小さいので、予算を小さくして打ち切った後の道も通す

/// 詰将棋のルールの手数を求める前に、すべての応手を数える手数を縮める証明 1 回あたりのノード数の予算。
/// 縮められる証明の多くはこの予算に収まり、探索量は縮められないことの確認に偏りやすいので予算を抑え、
/// 残りは推定と後の段で縮める
const ALL_STEP_BUDGET: u64 = if cfg!(test) { 30 } else { 2_000_000 };

/// 推定した手数 − 2 では詰まないことを、根で直接読み切る予算。
/// 根で読み切れる局面の多くは数千ノードで済み、読み切れない局面では根の試行に使ったノードの多くが無駄になる
const ROOT_DISPROOF_BUDGET: u64 = if cfg!(test) { 30 } else { 1_000_000 };

/// 根 (攻め方の手番、詰みを証明して置換表に残っている) の正解手順を求める。
/// 探索の結果が食い違って手順を作れなければ `None`。局面は呼び出し前の状態に戻して返る。
pub(super) fn shortest_pv<'a, S: MateState + Clone + Send, const PIECES: bool>(
    searcher: &mut Searcher<'a, S, PIECES>,
    st: &mut S,
    tables: &'a ShortestTables,
    mode: ShortestMode,
) -> Result<Option<Vec<S::Mv>>, Abort> {
    // 無駄合いを数えない手数は、すべての応手を数える手数以下。先に無駄合いの判定の要らない数え方で
    // 縮めておくと、無駄合いの判定 (別の局面を解く) をする上限付きの探索が減る
    searcher.enable_shortest(false, tables);
    // 証明は置換表に残っているので、読み直すと証明した手数が分かる
    let root = searcher.search(st, NodeArgs::unbounded(0, true, NO_LEN_LIMIT), &mut 0)?;
    if !root.is_proven() {
        return Ok(None);
    }
    let tsume = mode == ShortestMode::Tsume;
    let len = if tsume {
        tsume_len(searcher, st, tables, root.mate_len)?
    } else {
        descend(searcher, st, root.mate_len)?
    };

    let mut walker = Walker {
        memo: HashMap::new(),
        buffers: Vec::new(),
        tie_break: tsume,
    };
    if walker.choose(searcher, st, len, true)?.is_none() {
        return Ok(None);
    }
    let mut pv = Vec::with_capacity(usize::from(len));
    let mut d = len;
    let mut complete = false;
    while let Some(choice) = walker.memo.get(&(st.key(), d)) {
        let Some(mv) = choice.mv else {
            complete = d == 0;
            break;
        };
        st.do_move(mv);
        pv.push(mv);
        d -= 1;
    }
    for &mv in pv.iter().rev() {
        st.undo_move(mv);
    }
    Ok(complete.then_some(pv))
}

/// 詰将棋のルールでの根の詰み手数を求める (`len` は根を証明した手数)。
///
/// 上限を 2 手ずつ縮める証明は、最後の「縮めると詰まない」の証明が長編では探索のほとんどを占める。
/// そこで先に作意推定 ([`super::estimate`]) で短い手順を求め、その長さ L で詰むことと L − 2 手では詰まない
/// ことを証明する。推定の探索で置換表に残った局面が多いので、多くの場合どちらも安く済む。
/// - 詰むことの証明: 手順の局面を末端から根へ、残りの手数を上限にして証明する。根だけを証明すると、
///   攻め方の局面で手順以外の王手を先に読みに行き、長編では桁違いに重くなることがある。
/// - 詰まないことの証明: 根で予算内に読み切れなければ、手順上の攻め方の局面を末端から根へ、残りの手数 − 2 を
///   上限にして読み切ってから根を読む (上限付きの不詰は局面ごとに覚えているので、根の探索が手順の先で使う)。
fn tsume_len<'a, S: MateState + Clone + Send, const PIECES: bool>(
    searcher: &mut Searcher<'a, S, PIECES>,
    st: &mut S,
    tables: &'a ShortestTables,
    len: u16,
) -> Result<u16, Abort> {
    // すべての応手を数える手数は詰将棋のルールの手数以上なので、縮めた手数も上限に使える。最後の縮められない
    // 証明は長編で重いので、1 回ごとの予算で打ち切る
    let mut len = len;
    while len >= 3 {
        let r = searcher.within(ALL_STEP_BUDGET, |s| {
            s.search_shared(st, NodeArgs::unbounded(0, true, len - 2))
        })?;
        match r {
            Some(r) if r.is_proven() => len = r.mate_len,
            _ => break,
        }
    }
    // 推定は詰将棋のルールの数え方に切り替えてから読む
    let estimated = estimate::estimate_pv(searcher, st, tables)?;
    // 証明は予算で打ち切らない。打ち切ると、すべての応手を数える手数から 2 手ずつ縮める証明に戻り、長編ではそちらの
    // 方がずっと重い。推定が受け方の応手を選び損ねていれば、その局面の証明が詰まないと分かって縮める証明に戻る
    if let Some(pv) = estimated.filter(|pv| pv.len() <= usize::from(len))
        && prove_backward(searcher, st, &pv)?
    {
        len = pv.len() as u16;
        if len >= 3 {
            let r = searcher.within(ROOT_DISPROOF_BUDGET, |s| {
                s.search_shared(st, NodeArgs::unbounded(0, true, len - 2))
            })?;
            if r.is_none() {
                disprove_backward(searcher, st, &pv)?;
            }
        }
    }
    descend(searcher, st, len)
}

/// 手順の局面を末端から根へ、残りの手数を上限にして証明する。すべて証明できれば `true`
fn prove_backward<S: MateState, const PIECES: bool>(
    searcher: &mut Searcher<'_, S, PIECES>,
    st: &mut S,
    pv: &[S::Mv],
) -> Result<bool, Abort> {
    for &mv in pv {
        st.do_move(mv);
    }
    let mut proven = true;
    for (i, &mv) in pv.iter().enumerate().rev() {
        st.undo_move(mv);
        if proven {
            let remaining = (pv.len() - i) as u16;
            let r = searcher.search(st, NodeArgs::unbounded(0, i % 2 == 0, remaining), &mut 0);
            match r {
                Ok(r) => proven = r.is_proven(),
                Err(abort) => {
                    for &mv in pv[..i].iter().rev() {
                        st.undo_move(mv);
                    }
                    return Err(abort);
                }
            }
        }
    }
    Ok(proven)
}

/// 手順上の攻め方の局面を末端から根へ、残りの手数 − 2 を上限にして読む
fn disprove_backward<S: MateState + Clone + Send, const PIECES: bool>(
    searcher: &mut Searcher<'_, S, PIECES>,
    st: &mut S,
    pv: &[S::Mv],
) -> Result<(), Abort> {
    for &mv in pv {
        st.do_move(mv);
    }
    for (i, &mv) in pv.iter().enumerate().rev() {
        st.undo_move(mv);
        let remaining = pv.len() - i;
        if i % 2 == 1 || remaining < 3 {
            continue;
        }
        let r = searcher.search_shared(st, NodeArgs::unbounded(0, true, (remaining - 2) as u16));
        if let Err(abort) = r {
            for &mv in pv[..i].iter().rev() {
                st.undo_move(mv);
            }
            return Err(abort);
        }
    }
    Ok(())
}

/// `len` 手以内に詰む根から、上限を 2 手ずつ減らして証明できる最短の手数を求める
fn descend<S: MateState + Clone + Send, const PIECES: bool>(
    searcher: &mut Searcher<'_, S, PIECES>,
    st: &mut S,
    mut len: u16,
) -> Result<u16, Abort> {
    while len >= 3 {
        let r = searcher.search_shared(st, NodeArgs::unbounded(0, true, len - 2))?;
        if !r.is_proven() {
            break;
        }
        len = r.mate_len;
    }
    Ok(len)
}

/// 局面で選んだ手 (受け方の詰上りは `None`) と、そこから持駒が余らない手順があるか
#[derive(Clone, Copy)]
struct Choice<M> {
    mv: Option<M>,
    clean: bool,
}

struct Walker<M> {
    /// (局面のキー, 詰み手数) ごとの選んだ手
    memo: HashMap<(u64, u16), Choice<M>>,
    /// 指し手の作業領域。手順の深さごとに 2 本使い、局面ごとに確保し直さない
    buffers: Vec<Vec<M>>,
    /// 持駒が余らない手順を探す (詰将棋のルール)。探さなければ最初の候補を選ぶ
    tie_break: bool,
}

impl<M: Copy + Eq> Walker<M> {
    fn take_buffer(&mut self) -> Vec<M> {
        self.buffers
            .pop()
            .unwrap_or_else(|| Vec::with_capacity(crate::movegen::MAX_MOVES))
    }

    fn return_buffer(&mut self, mut buffer: Vec<M>) {
        buffer.clear();
        self.buffers.push(buffer);
    }

    /// 詰み手数 `d` の局面 `st` の手を選んで覚え、持駒が余らない手順があるかを返す。
    /// 探索の結果が食い違ったら `None`。
    fn choose<S: MateState<Mv = M>, const PIECES: bool>(
        &mut self,
        searcher: &mut Searcher<'_, S, PIECES>,
        st: &mut S,
        d: u16,
        or_node: bool,
    ) -> Result<Option<bool>, Abort> {
        let key = (st.key(), d);
        if let Some(c) = self.memo.get(&key) {
            return Ok(Some(c.clean));
        }
        let mut first = self.take_buffer();
        let mut second = self.take_buffer();
        let choice = if or_node {
            self.choose_attack(searcher, st, d, &mut first, &mut second)
        } else {
            self.choose_defense(searcher, st, d, &mut first)
        };
        self.return_buffer(first);
        self.return_buffer(second);
        let choice = choice?;
        let Some(choice) = choice else {
            return Ok(None);
        };
        self.memo.insert(key, choice);
        Ok(Some(choice.clean))
    }

    /// 持駒が余らない手順を、最初の候補以外も調べて探すか。
    /// 攻め方は 1 手で持駒を高々 1 枚しか減らせないので、残りの攻め方の手数より多く持っていれば必ず余る
    fn may_search_alternatives<S: MateState>(
        &self,
        st: &S,
        attacker: Color,
        d: u16,
        or_node: bool,
    ) -> bool {
        let attacker_moves = if or_node { d.div_ceil(2) } else { d / 2 };
        self.tie_break && hand_count(st.table_key(attacker).hand) <= u32::from(attacker_moves)
    }

    /// `tried` と `checks` は空の作業領域
    fn choose_attack<S: MateState<Mv = M>, const PIECES: bool>(
        &mut self,
        searcher: &mut Searcher<'_, S, PIECES>,
        st: &mut S,
        d: u16,
        tried: &mut Vec<M>,
        checks: &mut Vec<M>,
    ) -> Result<Option<Choice<M>>, Abort> {
        // 詰み手数 d − 1 で詰む王手を 1 つ (置換表に残っていればいくつか) 探索で見つける
        searcher.capture_proven(0);
        let r = searcher.search(st, NodeArgs::unbounded(0, true, d), &mut 0)?;
        if !r.is_proven() || r.mate_len != d {
            return Ok(None);
        }
        tried.extend(
            searcher
                .proven_children()
                .iter()
                .filter(|&&(_, len)| len < d)
                .map(|&(mv, _)| mv),
        );
        if tried.is_empty() {
            // 1 手詰めの近道で証明された局面は子を展開していない
            match st.mate_1ply() {
                Some(mv) if d == 1 => tried.push(mv),
                _ => return Ok(None),
            }
        }
        let first = tried[0];
        for &mv in tried.iter() {
            st.do_move(mv);
            let clean = self.choose(searcher, st, d - 1, false);
            st.undo_move(mv);
            match clean? {
                Some(true) => {
                    return Ok(Some(Choice {
                        mv: Some(mv),
                        clean: true,
                    }));
                }
                Some(false) => {}
                None if mv == first => return Ok(None),
                None => {}
            }
            if !self.may_search_alternatives(st, searcher.attacker(), d, true) {
                return Ok(Some(Choice {
                    mv: Some(first),
                    clean: false,
                }));
            }
        }
        // 探索で見つけていない王手も、詰み手数 d − 1 で詰むものを調べる
        st.generate(true, checks);
        for &mv in checks.iter() {
            if tried.contains(&mv)
                || !self.may_search_alternatives(st, searcher.attacker(), d, true)
            {
                continue;
            }
            st.do_move(mv);
            let clean = match searcher.search(st, NodeArgs::unbounded(0, false, d - 1), &mut 0) {
                Ok(r) if r.is_proven() => self.choose(searcher, st, d - 1, false),
                Ok(_) => Ok(Some(false)),
                Err(abort) => Err(abort),
            };
            st.undo_move(mv);
            if clean? == Some(true) {
                return Ok(Some(Choice {
                    mv: Some(mv),
                    clean: true,
                }));
            }
        }
        Ok(Some(Choice {
            mv: Some(first),
            clean: false,
        }))
    }

    /// `replies` は空の作業領域。
    /// 応手は生成順に、選ぶのに要る分だけ調べる (合駒は生成の最後なので、無駄合いの判定の多くを省ける)
    fn choose_defense<S: MateState<Mv = M>, const PIECES: bool>(
        &mut self,
        searcher: &mut Searcher<'_, S, PIECES>,
        st: &mut S,
        d: u16,
        replies: &mut Vec<M>,
    ) -> Result<Option<Choice<M>>, Abort> {
        if d == 0 {
            return Ok(Some(Choice {
                mv: None,
                clean: st.table_key(searcher.attacker()).hand == Hand::EMPTY,
            }));
        }
        st.generate(false, replies);
        let alternatives = self.may_search_alternatives(st, searcher.attacker(), d, false);
        let mut first = None;
        // 詰み手数がちょうど d − 1 の応手 (無駄合いを除く)
        for &mv in replies.iter() {
            if first.is_some() && !alternatives {
                break;
            }
            if st.is_drop(mv) && searcher.futile_drop(st, mv, d)?.is_some() {
                continue;
            }
            if d >= 4 {
                st.do_move(mv);
                let r = searcher.search(st, NodeArgs::unbounded(0, true, d - 3), &mut 0);
                st.undo_move(mv);
                if r?.is_proven() {
                    continue;
                }
            }
            first.get_or_insert(mv);
            st.do_move(mv);
            let clean = self.choose(searcher, st, d - 1, true);
            st.undo_move(mv);
            match clean? {
                Some(true) => {
                    return Ok(Some(Choice {
                        mv: Some(mv),
                        clean: true,
                    }));
                }
                Some(false) => {}
                None => return Ok(None),
            }
        }
        Ok(first.map(|mv| Choice {
            mv: Some(mv),
            clean: false,
        }))
    }
}

fn hand_count(hand: Hand) -> u32 {
    PieceType::HAND_PIECES.iter().map(|&pt| hand.count(pt)).sum()
}
