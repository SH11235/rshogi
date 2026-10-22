//! 詰み手順 (PV) の復元
//!
//! 置換表の詰みは経路に依存しない「この局面から詰む」という事実なので、置換表を素直に辿ると、
//! 手順の途中の局面に戻る詰みを選んで循環しうる。そこで手順上の各局面を詰み手数の上限付きで
//! 探索し直し (置換表に残っていれば一瞬で終わる)、上限より短い証明だけを使う。
//! - 攻め方の局面: 上限内で証明できた王手のうち詰み手数が最小のもの。
//! - 受け方の局面: すべての応手が上限内で詰むことを確かめ、詰み手数が最大のものを選ぶ。
//!
//! 詰み手数の上限は手順に沿って厳密に減る。それでも手順上の局面 X に戻ることはある
//! (X を通ったときに見つかっていたのが X の最短でない詰みだった場合)。戻った時点の上限は
//! X を通ったときの上限より小さく、X はその上限内で詰むので、手順を X まで巻き戻して
//! 小さい上限で選び直す。巻き戻すと X の上限が減り、X より前の局面の上限は変わらない。
//! 手順に沿った上限の列が辞書式に減り続けるので、復元は必ず終わる。
//! 各探索は手順を経路に含めずに行う (経路上の局面を不詰とみなすと、置換表の詰みを証明し直せない)。
//! 復元できなければ `None` を返し、呼び出し側は詰みを主張しない。

use super::search::{Abort, MAX_DEPTH, NodeArgs, Searcher};
use super::state::MateState;

/// ルート局面 (攻め方の手番、`mate_len` 手以内の詰みが証明済み) から詰み手順を復元する。
///
/// 局面は呼び出し前の状態に戻して返る。
pub(super) fn extract_pv<S: MateState>(
    searcher: &mut Searcher<'_, S>,
    st: &mut S,
    mate_len: u16,
) -> Result<Option<Vec<S::Mv>>, Abort> {
    let mut pv = Vec::new();
    let result = walk(searcher, st, mate_len, &mut pv);
    for &mv in pv.iter().rev() {
        st.undo_move(mv);
    }
    Ok(if result? { Some(pv) } else { None })
}

fn walk<S: MateState>(
    searcher: &mut Searcher<'_, S>,
    st: &mut S,
    len_limit: u16,
    pv: &mut Vec<S::Mv>,
) -> Result<bool, Abort> {
    let mut moves = Vec::new();
    // 手順の i 手目を指す前の局面のキーと詰み手数の上限
    let mut keys = vec![st.key()];
    let mut limits = vec![len_limit];
    loop {
        if pv.len() >= MAX_DEPTH {
            return Ok(false);
        }
        let or_node = pv.len() % 2 == 0;
        let len_limit = limits[pv.len()];
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
        st.do_move(mv);
        pv.push(mv);
        let key = st.key();
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

/// 攻め方の局面で、`len_limit` 手以内で詰む王手のうち詰み手数が最小のものを選ぶ。
/// 戻り値は (指し手, その後の詰み手数)。
fn choose_attack<S: MateState>(
    searcher: &mut Searcher<'_, S>,
    st: &mut S,
    len_limit: u16,
) -> Result<Option<(S::Mv, u16)>, Abort> {
    searcher.capture_proven(0);
    let result = searcher.search(st, NodeArgs::unbounded(0, true, len_limit), &mut 0)?;
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
        .proven_children()
        .iter()
        .copied()
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
    let mut best: Option<(S::Mv, u16)> = None;
    for &mv in moves {
        st.do_move(mv);
        let result = searcher.search(st, NodeArgs::unbounded(0, true, child_limit), &mut 0);
        st.undo_move(mv);
        let result = result?;
        // 上限内で詰まない応手があれば手順は作れない
        if !result.is_proven() {
            return Ok(None);
        }
        if best.is_none_or(|(_, best_len)| result.mate_len > best_len) {
            best = Some((mv, result.mate_len));
        }
    }
    Ok(best)
}
