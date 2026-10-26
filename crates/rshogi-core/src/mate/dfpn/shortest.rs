//! 最短手順モード: 詰将棋のルールでの正解手順
//!
//! 正解手順の定義:
//! 1. 攻め方は王手を続け、最短の手数で詰ます。
//! 2. 受け方は最も長く逃れる応手を選ぶ。
//! 3. 無駄合いは受け方の手として数えない。無駄合いは、合駒を取って取った駒を受け方に戻した局面の詰み手数が
//!    駒打ち以外の応手で逃れたときの詰み手数以下の合駒 (`Searcher::futile_drop`)。無駄合いは受け方の局面の
//!    詰み手数を決めないので、手順の受け方の手は無駄合いでない応手から選べる。
//! 4. 受け方の応手で手数が同じなら、詰上りで攻め方に持駒が余らない方を選ぶ。
//!
//! 局面の最短の詰み手数 (無駄合いを数えない) を「詰み手数」と呼ぶ。
//! - 根の詰み手数: 判定で証明した手数 L から、上限を L − 2 にして証明し直すことを、証明できなくなるまで
//!   繰り返す。上限付きの探索は無駄合いを手数に数えず、上限だけに依存する不詰を覚えておく
//!   (`Searcher::enable_shortest`)。置換表の詰みの手数は証明の長さ (詰み手数の上限) なので、
//!   上限付きの探索は上限を超える詰みを使わない。
//! - 手順: 詰み手数 d の局面で、攻め方は d − 1 手以内に詰む王手 (詰み手数はちょうど d − 1)、受け方は
//!   無駄合い (上限 d で判定) でない応手のうち d − 3 手以内に詰まないもの (ちょうど d − 1) から選ぶ。
//!
//! 4 の「持駒が余らない」は、攻め方も受け方も上の候補から選ぶ手順のうち、詰上りで攻め方の持駒が
//! 空になるものがあるかで決める (攻め方も余らない手順を選ぶ)。この探索の制限の中で、余らない手順が
//! 無いと分かるまで候補を調べる (攻め方の持駒が残りの攻め方の手数より多ければ必ず余るので調べない)。

use std::cell::RefCell;
use std::collections::HashMap;

use super::search::{Abort, NO_LEN_LIMIT, NodeArgs, Searcher, ShortestTables};
use super::state::MateState;
use crate::types::{Color, Hand, PieceType};

/// 根 (攻め方の手番、詰みを証明して置換表に残っている) の正解手順を求める。
/// 探索の結果が食い違って手順を作れなければ `None`。局面は呼び出し前の状態に戻して返る。
pub(super) fn shortest_pv<'a, S: MateState, const PIECES: bool>(
    searcher: &mut Searcher<'a, S, PIECES>,
    st: &mut S,
    tables: &'a RefCell<ShortestTables>,
) -> Result<Option<Vec<S::Mv>>, Abort> {
    // 無駄合いを数えない手数は、すべての応手を数える手数以下。先に無駄合いの判定の要らない数え方で
    // 縮めておくと、無駄合いの判定 (別の局面を解く) をする上限付きの探索が減る
    searcher.enable_shortest(false, tables);
    // 証明は置換表に残っているので、読み直すと証明した手数が分かる
    let root = searcher.search(st, NodeArgs::unbounded(0, true, NO_LEN_LIMIT), &mut 0)?;
    if !root.is_proven() {
        return Ok(None);
    }
    let mut len = descend(searcher, st, root.mate_len)?;
    searcher.enable_shortest(true, tables);
    len = descend(searcher, st, len)?;

    let mut walker = Walker {
        memo: HashMap::new(),
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

/// `len` 手以内に詰む根から、上限を 2 手ずつ減らして証明できる最短の手数を求める
fn descend<S: MateState, const PIECES: bool>(
    searcher: &mut Searcher<'_, S, PIECES>,
    st: &mut S,
    mut len: u16,
) -> Result<u16, Abort> {
    while len >= 3 {
        let r = searcher.search(st, NodeArgs::unbounded(0, true, len - 2), &mut 0)?;
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
}

impl<M: Copy + Eq> Walker<M> {
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
        let choice = if or_node {
            self.choose_attack(searcher, st, d)?
        } else {
            self.choose_defense(searcher, st, d)?
        };
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
        hand_count(st.table_key(attacker).hand) <= u32::from(attacker_moves)
    }

    fn choose_attack<S: MateState<Mv = M>, const PIECES: bool>(
        &mut self,
        searcher: &mut Searcher<'_, S, PIECES>,
        st: &mut S,
        d: u16,
    ) -> Result<Option<Choice<M>>, Abort> {
        // 詰み手数 d − 1 で詰む王手を 1 つ (置換表に残っていればいくつか) 探索で見つける
        searcher.capture_proven(0);
        let r = searcher.search(st, NodeArgs::unbounded(0, true, d), &mut 0)?;
        if !r.is_proven() || r.mate_len != d {
            return Ok(None);
        }
        let mut tried: Vec<M> = searcher
            .proven_children()
            .iter()
            .filter(|&&(_, len)| len < d)
            .map(|&(mv, _)| mv)
            .collect();
        if tried.is_empty() {
            // 1 手詰めの近道で証明された局面は子を展開していない
            match st.mate_1ply() {
                Some(mv) if d == 1 => tried.push(mv),
                _ => return Ok(None),
            }
        }
        let first = tried[0];
        for &mv in &tried {
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
        let mut checks = Vec::new();
        st.generate(true, &mut checks);
        for mv in checks {
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

    fn choose_defense<S: MateState<Mv = M>, const PIECES: bool>(
        &mut self,
        searcher: &mut Searcher<'_, S, PIECES>,
        st: &mut S,
        d: u16,
    ) -> Result<Option<Choice<M>>, Abort> {
        if d == 0 {
            return Ok(Some(Choice {
                mv: None,
                clean: st.table_key(searcher.attacker()).hand == Hand::EMPTY,
            }));
        }
        let mut replies = Vec::new();
        st.generate(false, &mut replies);
        // 詰み手数がちょうど d − 1 の応手 (無駄合いを除く)
        let mut candidates = Vec::new();
        for mv in replies {
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
            candidates.push(mv);
        }
        let Some(&first) = candidates.first() else {
            return Ok(None);
        };
        for &mv in &candidates {
            if mv != first && !self.may_search_alternatives(st, searcher.attacker(), d, false) {
                break;
            }
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
        Ok(Some(Choice {
            mv: Some(first),
            clean: false,
        }))
    }
}

fn hand_count(hand: Hand) -> u32 {
    PieceType::HAND_PIECES.iter().map(|&pt| hand.count(pt)).sum()
}
