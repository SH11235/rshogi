//! 成れる角・飛・歩の不成 (同じ移動の成る手も合法な手) を定跡の main・alt にしない。
//!
//! 探索の手の生成はこの不成を作らないので、読みからは出ない。成った場合と比べて得になる局面もほぼ無く、
//! 相手が取り返せば成った後と同じ局面になる。桂・銀・香の不成は意味のある手なので対象にしない。

use super::master_table::{Row, mean_eval};
use super::register::Node;
use crate::book_db::position_from_sfen;
use anyhow::{Context, Result};
use rshogi_core::movegen::{MoveList, generate_legal_allowing_missing_kings};
use rshogi_core::position::Position;
use rshogi_core::types::{Move, PieceType};
use std::collections::{BTreeMap, BTreeSet};

/// 局面の合法手 `legal` のうち、成る手も合法な角・飛・歩の不成の USI。
pub(super) fn needless_unpromotions(pos: &Position, legal: &MoveList) -> BTreeSet<String> {
    let usi: BTreeSet<String> = legal.iter().map(|mv| mv.to_usi()).collect();
    legal
        .iter()
        .filter(|mv| !mv.is_drop() && mv.is_promote())
        .filter(|mv| {
            matches!(
                pos.piece_on(mv.from()).piece_type(),
                PieceType::Pawn | PieceType::Bishop | PieceType::Rook
            )
        })
        .map(|mv| Move::new_move(mv.from(), mv.to(), false).to_usi())
        .filter(|mv| usi.contains(mv))
        .collect()
}

/// 手番の側の敵陣 (成れる段) に出入りしうる盤上の移動の USI か。合法手の生成の前に候補を絞るだけに使う。
fn may_promote(key: &str, usi: &str) -> bool {
    let bytes = usi.as_bytes();
    if bytes.len() != 4 || usi.contains(['*', '+']) {
        return false;
    }
    let zone: &[u8] = if key.split_whitespace().nth(1) == Some("w") {
        b"ghi"
    } else {
        b"abc"
    };
    zone.contains(&bytes[1]) || zone.contains(&bytes[3])
}

/// 局面の合法手 (USI) と、そのうち成れる角・飛・歩の不成。
fn key_needless(key: &str) -> Result<(BTreeSet<String>, BTreeSet<String>)> {
    let pos = position_from_sfen(&format!("{key} 1"))?;
    let mut legal = MoveList::new();
    generate_legal_allowing_missing_kings(&pos, &mut legal);
    let needless = needless_unpromotions(&pos, &legal);
    Ok((legal.iter().map(|mv| mv.to_usi()).collect(), needless))
}

/// `key` の局面で `usi` が成れる角・飛・歩の不成か。
pub(super) fn is_needless(key: &str, usi: &str) -> Result<bool> {
    if !may_promote(key, usi) {
        return Ok(false);
    }
    let (_, needless) = key_needless(key).with_context(|| format!("不成の判定: {key}"))?;
    Ok(needless.contains(usi))
}

/// 元データの main・alt から不成を record にした数。
#[derive(Debug, Default, PartialEq)]
pub(super) struct Demoted {
    /// record にした行 (先手番・後手番)。
    pub rows: [usize; 2],
    /// そのうち main だった行。
    pub mains: usize,
    /// main を残りの手から選び直した局面。
    pub new_mains: usize,
    /// 指す手が残らず、record だけになった局面。
    pub emptied: usize,
    /// 行を書き換えた局面。
    pub keys: BTreeSet<String>,
}

impl Demoted {
    pub(super) fn total(&self) -> usize {
        self.rows[0] + self.rows[1]
    }

    pub(super) fn format(&self) -> String {
        format!(
            "成れる角・飛・歩の不成なので record にした行: 先手番 {}・後手番 {} (うち main {} / main を選び直した局面 {} / 指す手が残らない局面 {})",
            self.rows[0], self.rows[1], self.mains, self.new_mains, self.emptied
        )
    }
}

fn append_note(row: &mut Row, text: String) {
    let note = row.get("note");
    let joined = format!("{note}{}{text}", if note.is_empty() { "" } else { "。" });
    row.set("note", joined);
}

/// main・alt のうち成れる角・飛・歩の不成を record にする (行は消さない)。main を record にした局面は、残る
/// main・alt のうち curated の手があればその中から、合法な手、値の分かる手、値の高い手、元データの順の先を
/// main にする。並べ方は `master cycles` の替わりの main と同じだが、値は伝播前の evals の平均を使う。
/// 手が残らなければその局面は指さない手だけになる。
pub(super) fn demote_rows(rows: &mut [Row], since: &str) -> Result<Demoted> {
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, row) in rows.iter().enumerate() {
        if matches!(row.get("role"), "main" | "alt") {
            groups.entry(row.get("key").to_owned()).or_default().push(i);
        }
    }
    let mut summary = Demoted::default();
    for (key, indices) in groups {
        if !indices.iter().any(|&i| may_promote(&key, rows[i].get("move"))) {
            continue;
        }
        let (legal, needless) = key_needless(&key).with_context(|| format!("不成の判定: {key}"))?;
        let demoted: Vec<usize> = indices
            .iter()
            .copied()
            .filter(|&i| needless.contains(rows[i].get("move")))
            .collect();
        if demoted.is_empty() {
            continue;
        }
        let side = usize::from(key.split_whitespace().nth(1) == Some("w"));
        summary.keys.insert(key.clone());
        let mut main_demoted = false;
        for &i in &demoted {
            let row = &mut rows[i];
            main_demoted |= row.get("role") == "main";
            summary.mains += usize::from(row.get("role") == "main");
            summary.rows[side] += 1;
            let from = row.get("role").to_owned();
            row.set("role", "record".into());
            append_note(row, format!("{since}: 成れる角・飛・歩の不成なので {from} から record"));
        }
        if !main_demoted {
            continue;
        }
        let remaining: Vec<usize> =
            indices.iter().copied().filter(|i| !demoted.contains(i)).collect();
        if remaining.is_empty() {
            summary.emptied += 1;
            continue;
        }
        let pool: Vec<usize> = if remaining.iter().any(|&i| rows[i].get("tier") == "curated") {
            remaining.into_iter().filter(|&i| rows[i].get("tier") == "curated").collect()
        } else {
            remaining
        };
        let mut best = None;
        for i in pool {
            let value = mean_eval(rows[i].get("evals"))
                .with_context(|| format!("evals が不正です: {key} {}", rows[i].get("move")))?;
            let rank = (
                legal.contains(rows[i].get("move")),
                value.is_some(),
                value,
                std::cmp::Reverse(i),
            );
            if best.as_ref().is_none_or(|(r, _)| rank > *r) {
                best = Some((rank, i));
            }
        }
        let (_, chosen) = best.context("内部エラー: 手の残らない局面です")?;
        let row = &mut rows[chosen];
        row.set("role", "main".into());
        append_note(
            row,
            format!("{since}: main の不成を record にしたので残る手で値の一番高い手を main にした"),
        );
        summary.new_mains += 1;
    }
    Ok(summary)
}

/// 木から登録する手のうち、成れる角・飛・歩の不成を外し、外した手の数を返す。手が残らない局面は登録しない。
pub(super) fn drop_node_moves(nodes: &mut BTreeMap<String, Node>) -> Result<usize> {
    let mut dropped = 0;
    for (key, node) in nodes.iter_mut() {
        if !node.moves.iter().any(|m| may_promote(key, &m.mv)) {
            continue;
        }
        let (_, needless) = key_needless(key).with_context(|| format!("不成の判定: {key}"))?;
        let before = node.moves.len();
        node.moves.retain(|m| !needless.contains(&m.mv));
        dropped += before - node.moves.len();
    }
    nodes.retain(|_, node| !node.moves.is_empty());
    Ok(dropped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::book_tree::master_table::{test_row, validate};

    fn needless(sfen: &str) -> BTreeSet<String> {
        key_needless(sfen).unwrap().1
    }

    #[test]
    fn bishop_rook_and_pawn_unpromotions_of_both_sides() {
        let black = needless("4k4/9/9/8P/R3B4/9/9/9/4K4 b -");
        for mv in ["5e2b", "5e3c", "5e8b", "9e9b", "9e9a", "1d1c"] {
            assert!(black.contains(mv), "{mv}: {black:?}");
        }
        for mv in ["5e6f", "5e4d", "9e8e"] {
            assert!(!black.contains(mv), "{mv}");
        }
        let white = needless("4k4/7b1/9/9/8r/p8/9/9/4K4 w -");
        for mv in ["2b7g", "2b8h", "1e1h", "9f9g"] {
            assert!(white.contains(mv), "{mv}: {white:?}");
        }
        for mv in ["2b3c", "1e2e"] {
            assert!(!white.contains(mv), "{mv}");
        }
    }

    #[test]
    fn forced_promotions_promoted_pieces_and_minor_pieces_are_not_needless() {
        // 一段目への歩 (1b1a) は不成が反則、馬 (5e) は成れない、桂・銀・香の不成は対象外。
        assert!(needless("4k4/8P/9/3N1S2L/4+B4/9/9/9/4K4 b -").is_empty());
    }

    #[test]
    fn positions_without_a_king_are_judged_by_the_rules() {
        let found = needless("4k4/9/9/P8/8r/9/9/9/9 b -");
        assert!(found.contains("9d9c"), "{found:?}");
    }

    #[test]
    fn node_moves_drop_needless_unpromotions_and_empty_nodes() {
        use super::super::register::NodeMove;
        let node = |moves: &[&str]| Node {
            ply: "2".into(),
            remaining: 0,
            nodes: 1,
            moves: moves
                .iter()
                .map(|mv| NodeMove {
                    mv: (*mv).into(),
                    value: 0,
                    value_kind: "single".into(),
                    cond: "unknown".into(),
                })
                .collect(),
            tree: 0,
        };
        let only = "4k4/9/9/9/9/9/9/9/4K4 w -".replace("4k4/9", "4k4/7b1");
        let mut nodes = BTreeMap::from([
            (WHITE.to_owned(), node(&["2b7g", "2b3c", "1e1h", "2b7g+"])),
            (only.clone(), node(&["2b7g"])),
        ]);
        assert_eq!(drop_node_moves(&mut nodes).unwrap(), 3);
        let moves: Vec<_> = nodes[WHITE].moves.iter().map(|m| m.mv.as_str()).collect();
        assert_eq!(moves, ["2b3c", "2b7g+"]);
        assert!(!nodes.contains_key(&only));
    }

    fn row(key: &str, mv: &str, role: &str, tier: &str, evals: &str) -> Row {
        test_row(&[
            ("key", key.into()),
            ("ply", "10".into()),
            ("move", mv.into()),
            ("role", role.into()),
            ("source", "ext".into()),
            ("evals", evals.into()),
            ("tier", tier.into()),
            ("since", "t0".into()),
        ])
    }

    const WHITE: &str = "4k4/7b1/9/9/8r/p8/9/9/4K4 w -";

    #[test]
    fn demoted_main_is_replaced_by_the_best_remaining_move() {
        let mut rows = vec![
            row(WHITE, "2b7g", "main", "wide", "50"),
            row(WHITE, "2b7g+", "alt", "wide", "40"),
            row(WHITE, "2b3c", "alt", "wide", "45"),
            row(WHITE, "1e1h", "alt", "wide", "10"),
        ];
        let summary = demote_rows(&mut rows, "T").unwrap();
        assert_eq!(
            summary,
            Demoted {
                rows: [0, 2],
                mains: 1,
                new_mains: 1,
                emptied: 0,
                keys: BTreeSet::from([WHITE.to_owned()]),
            }
        );
        let roles: Vec<_> = rows.iter().map(|r| r.get("role")).collect();
        assert_eq!(roles, ["record", "alt", "main", "record"]);
        assert!(rows[0].get("note").contains("T: 成れる角・飛・歩の不成なので main から record"));
        assert!(rows[2].get("note").contains("T: main の不成を record にしたので"));
        validate(&rows).unwrap();
        assert_eq!(demote_rows(&mut rows, "T").unwrap(), Demoted::default());
    }

    #[test]
    fn curated_moves_are_preferred_and_empty_positions_become_records() {
        let mut rows = vec![
            row(WHITE, "2b7g", "main", "curated", "50"),
            row(WHITE, "2b3c", "alt", "wide", "45"),
            row(WHITE, "1e2e", "alt", "curated", "-20"),
        ];
        demote_rows(&mut rows, "T").unwrap();
        assert_eq!(rows[2].get("role"), "main");
        validate(&rows).unwrap();

        let mut rows = vec![row(WHITE, "2b7g", "main", "wide", "50")];
        let summary = demote_rows(&mut rows, "T").unwrap();
        assert_eq!(summary.emptied, 1);
        assert_eq!(rows[0].get("role"), "record");
        validate(&rows).unwrap();
    }
}
