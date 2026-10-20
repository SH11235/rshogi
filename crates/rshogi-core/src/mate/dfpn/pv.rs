//! 詰み手順 (PV) の復元
//!
//! 置換表の詰みは経路に依存しない「この局面から詰む」という事実なので、置換表を素直に辿ると、
//! 手順の途中の局面に戻る詰みを選んで循環しうる。そこで手順上の各局面を詰み手数の上限付きで
//! 探索し直し (置換表に残っていれば一瞬で終わる)、上限より短い証明だけを使う。
//! - 攻め方の局面: 上限内で証明できた王手のうち詰み手数が最小のもの。
//! - 受け方の局面: すべての応手が上限内で詰むことを確かめ、詰み手数が最大のものを選ぶ。
//!
//! 詰み手数は手順に沿って厳密に減るので、同じ局面には戻らない。復元できなければ `None` を返し、
//! 呼び出し側は詰みを主張しない。

use super::search::{Abort, MAX_DEPTH, NodeArgs, Searcher};
use super::state::MateState;

/// ルート局面 (攻め方の手番、`mate_len` 手以内の詰みが証明済み) から詰み手順を復元する。
///
/// 局面と探索経路は呼び出し前の状態に戻して返る。
pub(super) fn extract_pv<S: MateState>(
    searcher: &mut Searcher<'_, S>,
    st: &mut S,
    mate_len: u16,
) -> Result<Option<Vec<S::Mv>>, Abort> {
    let mut pv = Vec::new();
    let result = walk(searcher, st, mate_len, &mut pv);
    for &mv in pv.iter().rev() {
        st.undo_move(mv);
        searcher.pop_path();
    }
    Ok(if result? { Some(pv) } else { None })
}

fn walk<S: MateState>(
    searcher: &mut Searcher<'_, S>,
    st: &mut S,
    mut len_limit: u16,
    pv: &mut Vec<S::Mv>,
) -> Result<bool, Abort> {
    let mut moves = Vec::new();
    loop {
        if pv.len() >= MAX_DEPTH {
            return Ok(false);
        }
        let or_node = pv.len() % 2 == 0;
        moves.clear();
        st.generate(or_node, &mut moves);
        if !or_node && moves.is_empty() {
            return Ok(true);
        }

        let next = if or_node {
            choose_attack(searcher, st, len_limit)?
        } else {
            choose_defense(searcher, st, &moves, len_limit)?
        };
        let Some((mv, child_len)) = next else {
            return Ok(false);
        };
        searcher.push_path(st);
        st.do_move(mv);
        pv.push(mv);
        len_limit = child_len;
    }
}

/// 攻め方の局面で、`len_limit` 手以内で詰む王手のうち詰み手数が最小のものを選ぶ。
/// 戻り値は (指し手, その後の詰み手数)。
fn choose_attack<S: MateState>(
    searcher: &mut Searcher<'_, S>,
    st: &mut S,
    len_limit: u16,
) -> Result<Option<(S::Mv, u16)>, Abort> {
    let depth = searcher.path_len();
    let result = searcher.search(st, NodeArgs::unbounded(depth, true, len_limit), &mut 0)?;
    if !result.is_proven() {
        return Ok(None);
    }
    if result.mate_len == 1 {
        // 1 手詰めの近道で証明された局面は子を展開していない
        if let Some(mv) = st.mate_1ply() {
            return Ok(Some((mv, 0)));
        }
    }
    let best = searcher
        .proven_children(depth)
        .filter(|&(_, len)| len < len_limit)
        .min_by_key(|&(_, len)| len);
    Ok(best)
}

/// 受け方の局面で、すべての応手が `len_limit - 1` 手以内で詰むことを確かめ、
/// 詰み手数が最大の応手を選ぶ。戻り値は (指し手, その後の詰み手数)。
fn choose_defense<S: MateState>(
    searcher: &mut Searcher<'_, S>,
    st: &mut S,
    moves: &[S::Mv],
    len_limit: u16,
) -> Result<Option<(S::Mv, u16)>, Abort> {
    let Some(child_limit) = len_limit.checked_sub(1) else {
        return Ok(None);
    };
    let child_depth = searcher.path_len() + 1;
    searcher.push_path(st);
    let result = longest_reply(searcher, st, moves, child_depth, child_limit);
    searcher.pop_path();
    result
}

fn longest_reply<S: MateState>(
    searcher: &mut Searcher<'_, S>,
    st: &mut S,
    moves: &[S::Mv],
    child_depth: usize,
    child_limit: u16,
) -> Result<Option<(S::Mv, u16)>, Abort> {
    let mut best: Option<(S::Mv, u16)> = None;
    for &mv in moves {
        st.do_move(mv);
        let result =
            searcher.search(st, NodeArgs::unbounded(child_depth, true, child_limit), &mut 0);
        st.undo_move(mv);
        let result = result?;
        // 上限内で詰まない応手 (経路上の局面に戻る応手を含む) があれば手順は作れない
        if !result.is_proven() {
            return Ok(None);
        }
        if best.is_none_or(|(_, best_len)| result.mate_len > best_len) {
            best = Some((mv, result.mate_len));
        }
    }
    Ok(best)
}
