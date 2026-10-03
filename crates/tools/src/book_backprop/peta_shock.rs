//! YaneuraOu makebook2025.cpp の peta_shock。vd は局面自身でなく親手番視点。
use super::*;
use rshogi_core::movegen::{MoveList, generate_legal_all};
use rshogi_core::types::Value;

/// YO convergence_check と同じく、入力にない合法手でも既知局面へ合流すれば追加する。
pub(super) fn add_convergences(book: &BookDb, graph: &mut Graph, skipped: &[bool]) -> Result<()> {
    let index: BTreeMap<_, _> =
        graph.keys.iter().enumerate().map(|(i, k)| (k.as_str(), i)).collect();
    for (node, key) in graph.keys.iter().enumerate() {
        let entry = &book.entries[key];
        let mut pos = Position::new();
        pos.set_sfen(&entry.sfen)
            .map_err(|e| anyhow!("不正な SFEN: {}: {e}", entry.sfen))?;
        let mut legal = MoveList::new();
        generate_legal_all(&pos, &mut legal);
        let mut added = Vec::new();
        for &mv in legal.iter() {
            let usi = mv.to_usi();
            if entry.moves.iter().any(|row| row.move_usi.as_deref() == Some(&usi)) {
                continue;
            }
            let gives_check = pos.gives_check(mv);
            pos.do_move(mv, gives_check);
            let child = pos.to_sfen();
            pos.undo_move(mv);
            let edge = index
                .get(strip_ply(&child))
                .map(|&to| Edge {
                    to,
                    via_flip: false,
                })
                .or_else(|| {
                    rshogi_book::flipped_key(&child).and_then(|flipped| {
                        index.get(strip_ply(&flipped)).map(|&to| Edge { to, via_flip: true })
                    })
                });
            let Some(edge) = edge.filter(|edge| !skipped[edge.to]) else {
                continue;
            };
            graph.flip_edges += usize::from(edge.via_flip);
            graph.adjacency[node].push(edge.to);
            graph.moves[node].push(MoveValue {
                old: 0,
                new: 0,
                new_depth: None,
                edge: Some(edge),
                usable: true,
            });
            added.push(BookMove {
                move_usi: Some(usi),
                ponder_usi: None,
                value: 0,
                depth: 0,
                count: 0,
            });
        }
        graph.adjacency[node].sort_unstable();
        graph.adjacency[node].dedup();
        if !added.is_empty() {
            graph.converged_moves.insert(key.clone(), added);
        }
    }
    Ok(())
}

const BOOK_DEPTH_MAX: i32 = 9999;
const PERPETUAL_CHECKED: i32 = 9998;
const PERPETUAL_CHECK: i32 = 9997;
const BOOK_MAX_PLY: usize = 256;
// YO の VALUE_MATE に対応するエンジン定数。book_mine のラベル上限とは区別する。
const BOOK_VALUE_MAX: i32 = Value::MATE.raw();
const BOOK_VALUE_INF: i32 = i16::MAX as i32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ValueDepth {
    value: i32,
    depth: i32,
}

impl ValueDepth {
    const fn new(value: i32, depth: i32) -> Self {
        Self { value, depth }
    }

    fn better(self, other: Self) -> bool {
        if self.value != other.value {
            return self.value > other.value;
        }
        if self.depth == PERPETUAL_CHECK {
            return false;
        }
        if other.depth == PERPETUAL_CHECK {
            return true;
        }
        if self.value >= 0 {
            self.depth < other.depth
        } else {
            self.depth > other.depth
        }
    }

    fn for_parent(self) -> Self {
        Self::new(-self.value, self.depth.saturating_add(1).min(BOOK_DEPTH_MAX))
    }
}

struct PetaShock<'a> {
    graph: &'a Graph,
    entries: Vec<&'a PositionEntry>,
    options: BackpropOptions,
    vd: Vec<ValueDepth>,
    constant: Vec<bool>,
    checked: Vec<bool>,
    check_loop: Vec<bool>,
}

struct Frame {
    node: usize,
    next_move: usize,
    best: ValueDepth,
}

impl Frame {
    fn new(node: usize) -> Self {
        Self {
            node,
            next_move: 0,
            best: ValueDepth::new(-BOOK_VALUE_INF, 0),
        }
    }
}

impl PetaShock<'_> {
    fn counts(&self, mv: &MoveValue) -> bool {
        !self.options.skip_unusable_moves || mv.usable
    }

    fn move_vd(&self, node: usize, idx: usize) -> ValueDepth {
        self.graph.moves[node][idx].edge.map_or_else(
            || {
                let mv = &self.entries[node].moves[idx];
                // YO read_book は探索 depth を捨て、葉からの距離を 0 で始める。
                ValueDepth::new(mv.value, 0)
            },
            |edge| self.vd[edge.to],
        )
    }

    fn best_for_parent(&self, node: usize) -> ValueDepth {
        let mut best = ValueDepth::new(-BOOK_VALUE_INF, BOOK_DEPTH_MAX);
        for (idx, mv) in self.graph.moves[node].iter().enumerate() {
            if self.counts(mv) {
                let vd = self.move_vd(node, idx);
                if vd.better(best) {
                    best = vd;
                }
            }
        }
        best.for_parent()
    }

    fn remove_const_nodes(&mut self) {
        for _ in 0..BOOK_MAX_PLY {
            let mut changed = false;
            for node in 0..self.vd.len() {
                if !self.constant[node]
                    && self.graph.moves[node]
                        .iter()
                        .filter(|mv| self.counts(mv))
                        .all(|mv| mv.edge.is_none_or(|edge| self.constant[edge.to]))
                {
                    self.vd[node] = self.best_for_parent(node);
                    self.constant[node] = true;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
    }

    fn extract_check_loop(&mut self) {
        // adjacency は合法な book 内辺だけ。via_flip でも同じ node index を辿る。
        for _ in 0..BOOK_MAX_PLY {
            let mut changed = false;
            for node in 0..self.vd.len() {
                if self.check_loop[node]
                    && !self.graph.adjacency[node].iter().any(|&child| {
                        self.graph.adjacency[child].iter().any(|&next| self.check_loop[next])
                    })
                {
                    self.check_loop[node] = false;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        let candidates: Vec<_> = (0..self.vd.len()).filter(|&n| self.check_loop[n]).collect();
        for node in candidates {
            for &child in &self.graph.adjacency[node] {
                if self.graph.adjacency[child].iter().any(|&next| self.check_loop[next]) {
                    self.check_loop[child] = true;
                }
            }
        }
    }

    // YO の再帰 DFS と同じ訪問順・trajectory の寿命を明示スタックで再現する。
    // 深い book でも Windows のスレッドスタックを消費しない。
    fn dfs(&self, root: usize, trajectory: &mut [bool], stack: &mut Vec<Frame>) -> ValueDepth {
        stack.push(Frame::new(root));
        trajectory[root] = true;
        while let Some(frame) = stack.last_mut() {
            let node = frame.node;
            if frame.next_move == self.graph.moves[node].len() {
                let result = frame.best.for_parent();
                trajectory[node] = false;
                stack.pop();
                if let Some(parent) = stack.last_mut() {
                    if result.better(parent.best) {
                        parent.best = result;
                    }
                } else {
                    return result;
                }
                continue;
            }
            let idx = frame.next_move;
            frame.next_move += 1;
            let mv = &self.graph.moves[node][idx];
            if !self.counts(mv) {
                continue;
            }
            let vd = if let Some(edge) = mv.edge {
                let child = edge.to;
                if !self.check_loop[child] {
                    self.vd[child]
                } else if trajectory[child] {
                    if self.checked[child] {
                        ValueDepth::new(-BOOK_VALUE_MAX, PERPETUAL_CHECK)
                    } else {
                        ValueDepth::new(BOOK_VALUE_MAX, PERPETUAL_CHECKED)
                    }
                } else {
                    trajectory[child] = true;
                    stack.push(Frame::new(child));
                    continue;
                }
            } else {
                self.move_vd(node, idx)
            };
            if vd.better(frame.best) {
                frame.best = vd;
            }
        }
        unreachable!("DFS は root の結果を返す")
    }

    fn run(&mut self) -> usize {
        self.remove_const_nodes();
        self.extract_check_loop();
        for node in 0..self.vd.len() {
            if !self.constant[node] {
                self.vd[node] = ValueDepth::new(
                    if !self.check_loop[node] {
                        0
                    } else if self.checked[node] {
                        -BOOK_VALUE_MAX
                    } else {
                        BOOK_VALUE_MAX
                    },
                    BOOK_DEPTH_MAX,
                );
            }
        }
        let mut trajectory = vec![false; self.vd.len()];
        let mut stack = Vec::new();
        for iter in 1..=BOOK_MAX_PLY + 100 {
            let mut changed = false;
            for node in 0..self.vd.len() {
                if self.constant[node] || self.check_loop[node] {
                    continue;
                }
                let mut best = self.best_for_parent(node);
                if best.depth > BOOK_MAX_PLY as i32 {
                    best.depth = BOOK_DEPTH_MAX;
                }
                changed |= self.vd[node] != best;
                self.vd[node] = best;
            }
            for node in 0..self.vd.len() {
                if self.check_loop[node] {
                    self.vd[node] = self.dfs(node, &mut trajectory, &mut stack);
                }
            }
            // YO は通常パスの更新数だけで停止判定する (DFS の更新数を含めない)。
            if !changed {
                return iter;
            }
        }
        BOOK_MAX_PLY + 100
    }
}

pub(super) fn propagate(
    book: &BookDb,
    graph: &mut Graph,
    options: BackpropOptions,
) -> Result<PropagationStats> {
    let entries: Vec<_> = graph
        .keys
        .iter()
        .map(|key| book.entries.get(key).ok_or_else(|| anyhow!("entry がありません: {key}")))
        .collect::<Result<_>>()?;
    let checked: Vec<_> = entries
        .iter()
        .map(|entry| {
            let mut pos = Position::new();
            pos.set_sfen(&entry.sfen)
                .map_err(|e| anyhow!("不正な SFEN: {}: {e}", entry.sfen))?;
            Ok(!pos.checkers().is_empty())
        })
        .collect::<Result<_>>()?;
    let mut engine = PetaShock {
        graph,
        entries,
        options,
        vd: vec![ValueDepth::new(0, BOOK_DEPTH_MAX); graph.keys.len()],
        constant: vec![false; graph.keys.len()],
        check_loop: checked.clone(),
        checked,
    };
    let iterations = engine.run();
    let scc = build_scc_graph(&graph.adjacency);
    let mut stats = PropagationStats {
        yo_nodes: Some((
            engine.check_loop.iter().filter(|&&v| v).count(),
            engine.constant.iter().filter(|&&v| !v).count(),
        )),
        nontrivial_sccs: scc.nontrivial.iter().filter(|&&v| v).count(),
        max_scc_size: scc.comps.iter().map(Vec::len).max().unwrap_or(0),
        scc_iters: vec![iterations],
        ..PropagationStats::default()
    };
    let mut scc_depth = vec![0; scc.comps.len()];
    for &comp in scc.topo.iter().rev() {
        scc_depth[comp] = scc.edges[comp].iter().map(|&c| scc_depth[c] + 1).max().unwrap_or(0);
    }
    // 出力補正は伝播完了後だけ行い、親へは補正前の vd を渡す。
    let vd = engine.vd;
    let check_loop = engine.check_loop;
    let checked = engine.checked;
    for (node, key) in graph.keys.iter().enumerate() {
        let entry = &book.entries[key];
        let moves = &mut graph.moves[node];
        let counts = |mv: &MoveValue| !options.skip_unusable_moves || mv.usable;
        let old_best = moves
            .iter()
            .take(entry.moves.len())
            .filter(|mv| counts(mv))
            .map(|mv| mv.old)
            .max()
            .unwrap_or(0);
        let mut output: Vec<_> = moves
            .iter()
            .enumerate()
            .map(|(idx, mv)| {
                mv.edge.map_or_else(
                    || ValueDepth::new(entry.moves[idx].value, 0),
                    |edge| {
                        let mut result = vd[edge.to];
                        if check_loop[edge.to] && checked[edge.to] {
                            result.depth = PERPETUAL_CHECK;
                        }
                        result
                    },
                )
            })
            .collect();
        let best = output
            .iter()
            .zip(moves.iter())
            .filter(|(_, mv)| counts(mv))
            .map(|(&v, _)| v)
            .reduce(|a, b| if b.better(a) { b } else { a });
        for (idx, value) in output.iter_mut().enumerate() {
            let mv = &mut moves[idx];
            if !counts(mv) {
                continue;
            }
            if let Some(best) = best
                && value.value == best.value
                && value.depth != best.depth
            {
                value.value = value.value.saturating_sub(1);
            }
            mv.new = value.value;
            mv.new_depth = Some(value.depth);
            if mv.new != mv.old {
                stats.updated_moves += 1;
                stats
                    .abs_deltas
                    .push((i64::from(mv.new) - i64::from(mv.old)).abs().min(i64::from(i32::MAX))
                        as i32);
            }
            let comp = scc.comp_of[node];
            if mv.new == 0
                && mv.edge.is_some_and(|e| scc.nontrivial[comp] && scc.comp_of[e.to] == comp)
            {
                stats.draw_moves += 1;
            }
        }
        let new_best = moves.iter().filter(|mv| counts(mv)).map(|mv| mv.new).max().unwrap_or(0);
        if old_best != new_best {
            stats.changed_nodes.push(NodeChange {
                sfen: entry.sfen.clone(),
                old_best,
                new_best,
            });
        }
        stats.depths.push(scc_depth[scc.comp_of[node]]);
    }
    stats.changed_nodes.sort_by(|a, b| {
        (i64::from(b.new_best) - i64::from(b.old_best))
            .abs()
            .cmp(&(i64::from(a.new_best) - i64::from(a.old_best)).abs())
            .then_with(|| a.sfen.cmp(&b.sfen))
    });
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    const START: &str = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
    const CHECK: &str = "4k4/4R4/9/9/9/9/9/9/4K4 w - 1";
    const KINGS: &str = "4k4/9/9/9/9/9/9/9/4K4 b - 1";

    fn run(
        input: &str,
        skip_unusable_moves: bool,
        skip_unsearched_children: bool,
    ) -> (BookDb, PropagationStats, String) {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("in.db");
        let dst = dir.path().join("out.db");
        let report = dir.path().join("report.md");
        std::fs::write(&src, input).unwrap();
        let stats = backprop_file_with(
            &src,
            &dst,
            Some(&report),
            123,
            1,
            MergeMode::Replace,
            BackpropOptions {
                yo_compat: true,
                skip_unusable_moves,
                skip_unsearched_children,
            },
        )
        .unwrap();
        let text = std::fs::read_to_string(&dst).unwrap();
        assert!(
            std::fs::read_to_string(report)
                .unwrap()
                .contains("yo-compat: check-loop nodes:")
        );
        (read_book_db(&dst).unwrap(), stats, text)
    }

    fn row<'a>(book: &'a BookDb, sfen: &str, usi: &str) -> &'a BookMove {
        book.entries[strip_ply(sfen)]
            .moves
            .iter()
            .find(|m| m.move_usi.as_deref() == Some(usi))
            .unwrap()
    }

    #[test]
    fn value_depth_order_and_parent_saturation() {
        for value in [-100, 0, 100] {
            let short = ValueDepth::new(value, 5);
            let long = ValueDepth::new(value, 10);
            assert_eq!(short.better(long), value >= 0);
            assert_eq!(long.better(short), value < 0);
            let check = ValueDepth::new(value, PERPETUAL_CHECK);
            assert!(short.better(check));
            assert!(!check.better(short));
            assert!(!check.better(check));
        }
        assert!(ValueDepth::new(1, PERPETUAL_CHECK).better(ValueDepth::new(0, 1)));
        assert_eq!(ValueDepth::new(42, 9999).for_parent(), ValueDepth::new(-42, 9999));
        assert_eq!(ValueDepth::new(42, PERPETUAL_CHECK).for_parent().depth, PERPETUAL_CHECKED);
    }

    #[test]
    fn leaf_input_depth_is_ignored_and_count_order_is_kept() {
        for value in [-20, 0, 20] {
            let input = format!(
                "{BOOK_HEADER}\nsfen {START}\n7g7f 3c3d {value} 5 1\n2g2f none {value} 10 9\n5i5h none {value} 5 2\n"
            );
            let (book, stats, text) = run(&input, false, false);
            assert_eq!(stats.yo_nodes, Some((0, 0)));
            for usi in ["7g7f", "5i5h", "2g2f"] {
                let mv = row(&book, START, usi);
                assert_eq!((mv.value, mv.depth), (value, 0));
            }
            assert_eq!(row(&book, START, "7g7f").ponder_usi.as_deref(), Some("3c3d"));
            assert!(text.lines().nth(2).unwrap().starts_with("2g2f"));
            assert_eq!(text, run(&input, false, false).2);
        }
    }

    #[test]
    fn chain_flip_uses_distance_and_skip_uses_input_depth() {
        for flip in [false, true] {
            for depth in [0, 12] {
                for skip in [false, true] {
                    let child = child_position_after_move(START, "7g7f").unwrap().to_sfen();
                    let (child, child_move) = if flip {
                        (rshogi_book::flipped_key(&child).unwrap(), "7g7f")
                    } else {
                        (child, "3c3d")
                    };
                    let input = format!(
                        "{BOOK_HEADER}\nsfen {START}\n7g7f none 123 9 7\nsfen {child}\n{child_move} none -50 {depth} 3\nnone none 0 0 1\n5e5d none 0 0 1\n"
                    );
                    let (book, _, _) = run(&input, true, skip);
                    let mv = row(&book, START, "7g7f");
                    let skipped = skip && depth == 0;
                    assert_eq!((mv.value, mv.depth), if skipped { (123, 0) } else { (50, 1) });
                    assert_eq!(row(&book, &child, "5e5d").value, 0);
                    assert_eq!(row(&book, &child, "5e5d").depth, 0);
                    assert_eq!(
                        book.entries[strip_ply(&child)]
                            .moves
                            .iter()
                            .find(|m| m.move_usi.is_none())
                            .unwrap()
                            .value,
                        0
                    );
                    let (included, _, _) = run(&input, false, skip);
                    assert_eq!(row(&included, START, "7g7f").value, if skipped { 123 } else { 0 });
                }
            }
        }
    }

    #[test]
    fn convergence_additions_respect_flip_and_unsearched_input() {
        for flip in [false, true] {
            for depth in [0, 12] {
                for skip in [false, true] {
                    let child = child_position_after_move(START, "7g7f").unwrap().to_sfen();
                    let (child, reply) = if flip {
                        (rshogi_book::flipped_key(&child).unwrap(), "7g7f")
                    } else {
                        (child, "3c3d")
                    };
                    let input = format!(
                        "{BOOK_HEADER}\nsfen {START}\n2g2f none 20 18 4\nsfen {child}\n{reply} none -50 {depth} 2\n"
                    );
                    let (book, _, _) = run(&input, true, skip);
                    let added = book.entries[strip_ply(START)]
                        .moves
                        .iter()
                        .find(|mv| mv.move_usi.as_deref() == Some("7g7f"));
                    assert_eq!(added.is_none(), skip && depth == 0);
                    if let Some(added) = added {
                        assert_eq!((added.value, added.depth, added.count), (50, 1, 0));
                        assert!(added.ponder_usi.is_none());
                    }
                    assert_eq!(row(&book, START, "2g2f").depth, 0);
                }
            }
        }
    }

    #[test]
    fn convergence_includes_legal_underpromotion() {
        let parent = "5k3/9/4P4/9/9/9/9/9/4K4 b - 1";
        let child = child_position_after_move(parent, "5c5b").unwrap().to_sfen();
        let input = format!(
            "{BOOK_HEADER}\nsfen {parent}\n5i4i none 0 10 1\nsfen {child}\n4a3a none -100 20 1\n"
        );
        let (book, _, _) = run(&input, false, false);
        let added = row(&book, parent, "5c5b");
        assert_eq!((added.value, added.depth), (100, 1));
    }

    fn cycle(
        start: &str,
        moves: &[&str],
        flip: bool,
        exit: Option<(usize, &str, i32)>,
    ) -> (String, Vec<(String, String)>) {
        let mut sfen = start.to_string();
        let mut text = format!("{BOOK_HEADER}\n");
        let mut rows = Vec::new();
        for (idx, &usi) in moves.iter().enumerate() {
            let flipped = flip && sfen.split_whitespace().nth(1) == Some("w");
            let key = if flipped {
                rshogi_book::flipped_key(&sfen).unwrap()
            } else {
                sfen.clone()
            };
            let mv = if flipped {
                rshogi_book::flip_usi_move(usi).unwrap()
            } else {
                usi.to_string()
            };
            text.push_str(&format!("sfen {key}\n{mv} none 73 10 1\n"));
            if let Some((at, exit_move, value)) = exit
                && at == idx
            {
                let exit_move = if flipped {
                    rshogi_book::flip_usi_move(exit_move).unwrap()
                } else {
                    exit_move.to_string()
                };
                text.push_str(&format!("{exit_move} none {value} 7 2\n"));
            }
            rows.push((key, mv));
            sfen = child_position_after_move(&sfen, usi).unwrap().to_sfen();
        }
        assert_eq!(strip_ply(&sfen), strip_ply(start));
        (text, rows)
    }

    #[test]
    fn ordinary_cycles_draw_or_take_winning_exit() {
        for flip in [false, true] {
            for exit in [None, Some((0, "5i6i", 100)), Some((0, "5i6i", -100))] {
                let (input, rows) = cycle(KINGS, &["5i4i", "5a4a", "4i5i", "4a5a"], flip, exit);
                let (book, stats, _) = run(&input, true, false);
                assert_eq!(stats.yo_nodes, Some((0, 4)));
                let mv = row(&book, &rows[0].0, &rows[0].1);
                if exit.is_some_and(|(_, _, value)| value > 0) {
                    // 勝てる出口へ回り道する枝は出力時に 1 下がる。
                    assert_eq!((mv.value, mv.depth), (99, 4));
                } else {
                    assert_eq!((mv.value, mv.depth), (0, BOOK_DEPTH_MAX));
                }
            }
        }
    }

    #[test]
    fn perpetual_check_signs_depth_and_equal_valued_escape() {
        for flip in [false, true] {
            for exit in [
                None,
                Some((1, "5b6b", -BOOK_VALUE_MAX)),
                Some((1, "5b6b", -40)),
            ] {
                let (input, rows) = cycle(CHECK, &["5a4a", "5b4b", "4a5a", "4b5b"], flip, exit);
                let (book, stats, text) = run(&input, true, false);
                assert_eq!(stats.yo_nodes, Some((4, 4)));
                let checking = row(&book, &rows[1].0, &rows[1].1);
                let checked = row(&book, &rows[0].0, &rows[0].1);
                let value = exit.map_or(-BOOK_VALUE_MAX, |(_, _, v)| v);
                assert_eq!(checked.value, -value);
                assert_eq!(checking.depth, PERPETUAL_CHECK);
                assert_eq!(checking.value, value - i32::from(exit.is_some()));
                assert_eq!(text, run(&input, true, false).2);
            }
        }
    }

    #[test]
    fn check_without_cycle_is_pruned_and_min_is_rejected() {
        let input = format!("{BOOK_HEADER}\nsfen {CHECK}\n5a4a none 20 8 1\n");
        assert_eq!(run(&input, true, false).1.yo_nodes, Some((0, 0)));
        let book = BookDb {
            entries: BTreeMap::new(),
        };
        let mut graph = build_graph(&book).unwrap();
        let err = propagate_values_with(
            &book,
            &mut graph,
            0,
            1000,
            MergeMode::Min,
            BackpropOptions {
                yo_compat: true,
                ..BackpropOptions::default()
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("--merge replace"));
    }
}
