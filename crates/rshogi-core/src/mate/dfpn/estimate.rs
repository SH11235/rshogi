//! 作意推定モード ([`ShortestMode::Estimate`]): 詰将棋のルール ([`ShortestMode::Tsume`] と同じ無駄合いの
//! 数え方) での正解手順を、最短であることを証明せずに推定する。
//!
//! 最短手順モードは「上限を 2 手縮めると詰まない」ことを証明するために、詰まない側の木を読み切る。
//! 長編ではこれが探索のほとんどを占めるので、ここでは縮める試みを手ごとのノード数の予算で打ち切る。
//! - 攻め方の局面: 置換表に残っている証明のうち最も短い王手を選び、さらに上限を 2 手ずつ縮めて予算内で
//!   証明できる限り短くする。
//! - 受け方の局面: 無駄合い (予算内で判定できたもの) を除く応手のうち、詰み手数の最も長いものを選ぶ。
//!   置換表に残っている証明の手数は最短とは限らず、そのまま比べると実は短く詰む応手を選んでしまう。
//!   そこで最も長く見える応手から順に、上と同じように予算内で縮めてから比べ直し、縮めても最も長いものを選ぶ。
//!
//! 手順の途中で攻め方の詰みが縮むと、上の受け方の局面で選ぶ応手が変わりうるので、手順が変わらなくなるまで
//! 根から選び直す (`MAX_PASSES` 回まで。推定全体で `RECHOOSE_BUDGET` ノードを超えたら前の手順を使う)。根の上限は証明した手数のまま
//! (手順の長さは、受け方が最善を選べていなければ根の詰み手数の上限にならない)。

use std::collections::{HashMap, HashSet};

use super::search::{Abort, MAX_DEPTH, NO_LEN_LIMIT, NodeArgs, Searcher, ShortestTables};
use super::state::MateState;

/// 詰みを縮める試み 1 回あたりのノード数の予算。縮められる試みはほとんどがこの予算に収まり、
/// 推定のノードの大半は縮められずに予算を使い切る試みが占める
const EFFORT: u64 = 30_000;

/// 攻め方の局面で、王手 1 つを読む予算
const PROBE: u64 = 10_000;

/// 手順を根から選び直す回数の上限
const MAX_PASSES: usize = 32;

/// 選び直すのは、推定に使ったノード数 (1 回目を含む) がこの予算に収まる間だけ (長編では選び直すたびに
/// 縮める試みが増えて膨らむ)
const RECHOOSE_BUDGET: u64 = 20_000_000;

/// 縮める試みの記録
struct Effort {
    /// 予算内に縮められなかった (局面, 詰み手数)。受け方の局面で比べるときと攻め方の局面で選ぶときに
    /// 同じ試みを繰り返さない。攻め方の局面は (局面, 上限 + 2)、王手の後の受け方の局面は (局面, 上限 + 1) で
    /// 覚える (どちらも上限を 2 手縮めて読んだ記録で、手番が違うので混ざらない)
    gave_up: HashSet<(u64, u16)>,
    /// 前の手順で、攻め方の局面から実際に詰んだ手数。受け方の局面で応手を比べるとき、置換表の証明の手数より
    /// 短ければこちらを使う (手順の先で詰みが縮んでも、置換表の上の局面の手数は縮まないため)
    realized: HashMap<u64, u16>,
}

/// 根 (攻め方の手番、詰みを証明して置換表に残っている) の推定手順を求める。局面は呼び出し前の状態に戻して返る。
pub(super) fn estimate_pv<'a, S: MateState + Clone + Send, const PIECES: bool>(
    searcher: &mut Searcher<'a, S, PIECES>,
    st: &mut S,
    tables: &'a ShortestTables,
) -> Result<Option<Vec<S::Mv>>, Abort> {
    searcher.enable_shortest(true, tables);
    let root = searcher.search(st, NodeArgs::unbounded(0, true, NO_LEN_LIMIT), &mut 0)?;
    if !root.is_proven() {
        return Ok(None);
    }
    let limit = root.mate_len;
    let mut effort = Effort {
        gave_up: HashSet::new(),
        realized: HashMap::new(),
    };
    let mut best = None;
    let start = searcher.nodes();
    for pass in 0..MAX_PASSES {
        let mut pv = Vec::new();
        let complete = if pass == 0 {
            walk(searcher, st, limit, &mut effort, &mut pv).map(Some)
        } else {
            let left = RECHOOSE_BUDGET.saturating_sub(searcher.nodes() - start);
            searcher.within(left, |s| walk(s, st, limit, &mut effort, &mut pv))
        };
        for &mv in pv.iter().rev() {
            st.undo_move(mv);
        }
        // 選び直しが予算を超えたら、前の手順を使う
        if complete? != Some(true) {
            break;
        }
        for (i, &mv) in pv.iter().enumerate() {
            st.do_move(mv);
            if i % 2 == 1 {
                let remaining = (pv.len() - i - 1) as u16;
                let e = effort.realized.entry(st.key()).or_insert(remaining);
                *e = (*e).min(remaining);
            }
        }
        for &mv in pv.iter().rev() {
            st.undo_move(mv);
        }
        let same = best.as_ref() == Some(&pv);
        best = Some(pv);
        if same {
            break;
        }
    }
    Ok(best)
}

/// `len_limit` 手以内に詰む根から手順を選んで `pv` に積む (局面は手順の後のまま)。
/// 手順を作れなければ `false`。
fn walk<S: MateState + Clone + Send, const PIECES: bool>(
    searcher: &mut Searcher<'_, S, PIECES>,
    st: &mut S,
    len_limit: u16,
    effort: &mut Effort,
    pv: &mut Vec<S::Mv>,
) -> Result<bool, Abort> {
    let mut moves = Vec::new();
    let mut proven = Vec::new();
    // 手順の i 手目を指す前の局面のキーと詰み手数の上限
    let mut keys = vec![st.key()];
    let mut limits = vec![len_limit];
    loop {
        if pv.len() >= MAX_DEPTH {
            return Ok(false);
        }
        let or_node = pv.len().is_multiple_of(2);
        let len_limit = limits[pv.len()];
        moves.clear();
        st.generate(or_node, &mut moves);
        if !or_node && moves.is_empty() {
            return Ok(true);
        }
        let next = if or_node {
            choose_attack(searcher, st, len_limit, effort, &mut proven)?
        } else {
            match choose_defense(searcher, st, &moves, len_limit, effort)? {
                Defense::Reply(mv, len) => Some((mv, len)),
                // 応手がすべて無駄合い
                Defense::OnlyFutile => return Ok(true),
                Defense::Failed => None,
            }
        };
        let Some((mv, child_len)) = next else {
            return Ok(false);
        };
        st.do_move(mv);
        pv.push(mv);
        let key = st.key();
        // 手順上の局面に戻ったら、そこまで巻き戻して小さい上限で選び直す (`pv::walk` と同じ)
        if let Some(i) = keys.iter().position(|&k| k == key) {
            for mv in pv.drain(i..).rev() {
                st.undo_move(mv);
            }
            keys.truncate(i + 1);
            limits.truncate(i + 1);
            limits[i] = child_len;
        } else {
            keys.push(key);
            limits.push(child_len);
        }
    }
}

/// 攻め方の局面で、`len_limit` 手以内に詰む王手のうち、予算内で縮められる限り短いものを選ぶ。
/// 戻り値は (指し手, その後の詰み手数)。`proven` は作業領域。
fn choose_attack<S: MateState + Clone + Send, const PIECES: bool>(
    searcher: &mut Searcher<'_, S, PIECES>,
    st: &mut S,
    len_limit: u16,
    effort: &mut Effort,
    proven: &mut Vec<(S::Mv, u16)>,
) -> Result<Option<(S::Mv, u16)>, Abort> {
    let Some(len) = attack_len(searcher, st, len_limit, effort, proven)? else {
        return Ok(None);
    };
    // 1 手詰めの近道で証明された局面は子を展開していない (応手が無駄合いだけの王手は近道を通らない)
    if len == 1
        && let Some(mv) = st.mate_1ply()
    {
        return Ok(Some((mv, 0)));
    }
    Ok(proven.iter().copied().filter(|&(_, l)| l < len).min_by_key(|&(_, l)| l))
}

/// `len_limit` 手以内に詰む攻め方の局面の詰み手数を、予算内で縮められる限り縮めて返す。上限内に詰まなければ
/// `None`。`proven` には詰み手数の分かった王手 (指し手, その後の詰み手数) を入れる。
///
/// 縮め方は 2 つ: 王手ごとに小さい予算で読み、置換表の別の証明に合流して短く詰む王手を探す (攻め方の回り道を
/// 除く。回り道の先の局面は根からの探索で証明済みのことが多い)。そのうえで上限を 2 手ずつ縮めて証明し直す。
fn attack_len<S: MateState + Clone + Send, const PIECES: bool>(
    searcher: &mut Searcher<'_, S, PIECES>,
    st: &mut S,
    len_limit: u16,
    effort: &mut Effort,
    proven: &mut Vec<(S::Mv, u16)>,
) -> Result<Option<u16>, Abort> {
    searcher.capture_proven(0);
    let r = searcher.search(st, NodeArgs::unbounded(0, true, len_limit), &mut 0)?;
    if !r.is_proven() {
        return Ok(None);
    }
    proven.clear();
    proven.extend_from_slice(searcher.proven_children());
    let mut len = r.mate_len;
    if len >= 3 {
        let mut checks = Vec::new();
        st.generate(true, &mut checks);
        for mv in checks {
            // 応手が無駄合いだけの王手が見つかると 1 手まで縮み、それより短い王手は無い
            if len < 3 {
                break;
            }
            if proven.iter().any(|&(m, _)| m == mv) {
                continue;
            }
            st.do_move(mv);
            let key = (st.key(), len - 1);
            let r = if effort.gave_up.contains(&key) {
                Ok(None)
            } else {
                searcher
                    .within(PROBE, |s| s.search_shared(st, NodeArgs::unbounded(0, false, len - 2)))
            };
            st.undo_move(mv);
            match r? {
                Some(r) if r.is_proven() => {
                    proven.push((mv, r.mate_len));
                    len = len.min(r.mate_len + 1);
                }
                _ => {
                    effort.gave_up.insert(key);
                }
            }
        }
    }
    Ok(Some(shorten(searcher, st, len, effort, Some(proven))?))
}

/// `len` 手以内に詰む攻め方の局面 `st` の詰みを、上限を 2 手ずつ縮めて予算内で証明できる限り短くし、
/// その手数を返す。`proven` があれば、最後に証明できた探索の証明済みの子 (指し手, 詰み手数) に置き換える。
fn shorten<S: MateState + Clone + Send, const PIECES: bool>(
    searcher: &mut Searcher<'_, S, PIECES>,
    st: &mut S,
    mut len: u16,
    effort: &mut Effort,
    mut proven: Option<&mut Vec<(S::Mv, u16)>>,
) -> Result<u16, Abort> {
    while len >= 3 && !effort.gave_up.contains(&(st.key(), len)) {
        searcher.capture_proven(0);
        let shorter = searcher
            .within(EFFORT, |s| s.search_shared(st, NodeArgs::unbounded(0, true, len - 2)))?;
        match shorter {
            Some(r) if r.is_proven() => {
                len = r.mate_len;
                if let Some(proven) = proven.as_deref_mut() {
                    proven.clear();
                    proven.extend_from_slice(searcher.proven_children());
                }
            }
            _ => {
                effort.gave_up.insert((st.key(), len));
                break;
            }
        }
    }
    Ok(len)
}

enum Defense<M> {
    /// 選んだ応手とその後の詰み手数
    Reply(M, u16),
    /// 応手がすべて無駄合い (詰上り)
    OnlyFutile,
    /// 上限内に詰まない応手があった (探索の結果の食い違い)
    Failed,
}

/// 受け方の局面で、無駄合いを除く応手のうち詰み手数が最も長いものを選ぶ。
/// 最も長く見える応手の詰みを縮めてから比べ直すことを、縮めても最も長いものが決まるまで繰り返す。
fn choose_defense<S: MateState + Clone + Send, const PIECES: bool>(
    searcher: &mut Searcher<'_, S, PIECES>,
    st: &mut S,
    moves: &[S::Mv],
    len_limit: u16,
    effort: &mut Effort,
) -> Result<Defense<S::Mv>, Abort> {
    // 上限 0 で詰むと証明済みの局面に応手があれば、どれも無駄合い
    let Some(child_limit) = len_limit.checked_sub(1) else {
        return Ok(Defense::OnlyFutile);
    };
    let mut proven = Vec::new();
    // (応手, 詰み手数, 縮め終えたか)
    let mut replies: Vec<(S::Mv, u16, bool)> = Vec::with_capacity(moves.len());
    for &mv in moves {
        st.do_move(mv);
        let r = searcher.search(st, NodeArgs::unbounded(0, true, child_limit), &mut 0);
        st.undo_move(mv);
        let r = r?;
        // 局面は上限内に詰むと証明済みなので、上限内に詰まない応手は無駄合いとして証明されている
        if !r.is_proven() {
            if st.is_drop(mv) {
                continue;
            }
            return Ok(Defense::Failed);
        }
        match effort.realized.get(&st.key_after(mv)) {
            Some(&len) if len < r.mate_len => replies.push((mv, len, true)),
            _ => replies.push((mv, r.mate_len, false)),
        }
    }
    loop {
        // 同じ手数なら先に生成した応手 (max_by_key は最後の最大を返すので逆順に探す)
        let Some(i) = (0..replies.len()).rev().max_by_key(|&i| replies[i].1) else {
            return Ok(Defense::OnlyFutile);
        };
        let (mv, len, refined) = replies[i];
        if refined {
            return Ok(Defense::Reply(mv, len));
        }
        if st.is_drop(mv)
            && searcher
                .within(EFFORT, |s| s.futile_drop(st, mv, len_limit))?
                .flatten()
                .is_some()
        {
            replies.swap_remove(i);
            continue;
        }
        st.do_move(mv);
        let shortened = attack_len(searcher, st, len, effort, &mut proven);
        st.undo_move(mv);
        let Some(shortened) = shortened? else {
            return Ok(Defense::Failed);
        };
        replies[i] = (mv, shortened, true);
    }
}
