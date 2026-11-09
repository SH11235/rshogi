use super::journal::{Pv, Reading, Settings};
use super::model::{child, side};
use super::*;
use rshogi_core::movegen::{MoveList, generate_legal_all};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

const ROOT: &str = "4k4/9/9/9/9/9/9/9/4K4 b -";

pub(super) fn settings() -> Settings {
    Settings::new(&ReadingArgs {
        engine: std::env::current_exe().unwrap(),
        engine_options: vec![
            format!("EvalFile={}", std::env::current_exe().unwrap().display()),
            "DrawValueBlack=-70".into(),
            "DrawValueWhite=70".into(),
        ],
        nodes: 100,
        multipv: 3,
        hash: 16,
    })
    .unwrap()
}
fn legal(key: &str) -> Vec<String> {
    let pos = crate::book_db::position_from_sfen(&format!("{key} 1")).unwrap();
    let mut list = MoveList::new();
    generate_legal_all(&pos, &mut list);
    let mut moves: Vec<_> = list.as_slice().iter().map(|m| m.to_usi()).collect();
    moves.sort();
    moves
}
pub(super) fn reading(key: &str, rows: &[(&str, i32)], settings: &Settings) -> Reading {
    let mut mp: Vec<_> = rows
        .iter()
        .map(|(m, v)| Pv {
            mv: (*m).into(),
            cp: *v,
            depth: 8,
        })
        .collect();
    for mv in legal(key) {
        if mp.len() == 3 {
            break;
        }
        if !mp.iter().any(|p| p.mv == mv) {
            mp.push(Pv {
                mv,
                cp: -600,
                depth: 8,
            });
        }
    }
    Reading {
        tag: "fixture".into(),
        ply: 1,
        key: key.into(),
        nodes_limit: settings.nodes,
        nodes: Some(101),
        sec: 0.1,
        threads: 1,
        hash: settings.hash,
        draw: settings.draw(),
        bestmove: mp[0].mv.clone(),
        multipv: mp,
    }
}
fn definition(dir: &Path, rounds: usize) -> Definition {
    let paths = dir.join("paths.tsv");
    std::fs::write(&paths,format!("{ROOT}\t1\t5i4i 5a4a 4i5i 4a5a 5i6i\tcycle\n{ROOT}\t1\t5i5h 5a5b 5h4i 5b4a\ttransposition\n")).unwrap();
    Definition {
        source: None,
        paths,
        paths_sha256: None,
        out: dir.join("out"),
        depth: 5,
        opp_n: 1,
        stop: 300,
        own: "b".into(),
        rounds,
        extra: vec!["5i6i".into()],
        fingerprint: None,
        draw: 17,
        draw_black: None,
        draw_white: None,
    }
}
fn corpus(settings: &Settings) -> BTreeMap<String, Reading> {
    let mut cache = BTreeMap::new();
    let routes = [
        vec![
            ("5i5h", 30),
            ("5a5b", 50),
            ("5h4i", -100),
            ("5b4a", 50),
            ("4i5i", -100),
            ("4a5a", 50),
        ],
        vec![("5i4i", 20), ("5a4a", 50), ("4i5i", -100)],
    ];
    for route in routes {
        let mut key = ROOT.to_owned();
        for (mv, cp) in route {
            cache.entry(key.clone()).or_insert_with(|| reading(&key, &[(mv, cp)], settings));
            key = child(&key, mv).unwrap();
        }
    }
    cache.insert(
        ROOT.into(),
        reading(ROOT, &[("5i5h", 30), ("5i4i", 20), ("5i6i", 10)], settings),
    );
    // 未展開の良い手を回の境界で追加させる。
    let c = child(&child(ROOT, "5i4i").unwrap(), "5a4a").unwrap();
    cache.insert(c.clone(), reading(&c, &[("4i5i", -100), ("4i3i", 10), ("4i4h", 0)], settings));
    loop {
        let mut added = Vec::new();
        for (key, r) in &cache {
            for p in &r.multipv {
                let c = child(key, &p.mv).unwrap();
                if !cache.contains_key(&c) {
                    added.push(c);
                }
            }
        }
        let before = cache.len();
        for key in added {
            let mv = legal(&key);
            cache
                .entry(key.clone())
                .or_insert_with(|| reading(&key, &[(&mv[0], 500)], settings));
        }
        // STOP の外側の読みは評価集合へ漏れないことも検査する。
        if cache.len() == before || cache.len() > 100 {
            break;
        }
    }
    cache
}

fn bfs(tree: &mut Tree, cache: &BTreeMap<String, Reading>) {
    for round in 0..=tree.def.rounds {
        let mut layer: BTreeSet<_> = tree.roots.keys().cloned().collect();
        let mut seen = layer.clone();
        for depth in 0..=tree.def.depth {
            let mut next = BTreeSet::new();
            for key in layer {
                tree.level
                    .entry(key.clone())
                    .and_modify(|d| *d = (*d).min(depth))
                    .or_insert(depth);
                tree.nodes.entry(key.clone()).or_insert_with(|| cache[&key].clone());
                if depth == tree.def.depth {
                    continue;
                }
                let mp = &tree.nodes[&key].multipv;
                let game = tree.game.get(&key).cloned().unwrap_or_default();
                let mut picks = Vec::new();
                let mut extras = Vec::new();
                if tree.roots.contains_key(&key) {
                    if side(&key) == tree.def.own {
                        picks.extend(mp.first().map(|p| p.mv.clone()));
                        picks.extend(game.iter().cloned());
                        let mut seen = BTreeSet::new();
                        picks.retain(|m| seen.insert(m.clone()));
                        if picks.len() == 1 && mp.len() > 1 {
                            picks.push(mp[1].mv.clone());
                        }
                        picks.extend(
                            tree.def
                                .extra
                                .iter()
                                .filter(|m| mp.iter().any(|p| &p.mv == *m))
                                .cloned(),
                        );
                    } else {
                        picks.extend(mp.iter().take(tree.def.opp_n).map(|p| p.mv.clone()));
                        picks.extend(
                            tree.def.extra.iter().filter(|m| child(&key, m).is_ok()).cloned(),
                        );
                        picks.extend(game);
                    }
                } else if mp.first().is_some_and(|p| p.cp.abs() < tree.def.stop) {
                    let n = if side(&key) == tree.def.own {
                        1
                    } else {
                        tree.def.opp_n
                    };
                    picks.extend(mp.iter().take(n).map(|p| p.mv.clone()));
                    extras.extend(game);
                }
                extras.extend(tree.force.get(&key).into_iter().flatten().cloned());
                tree.rule.entry(key.clone()).or_default().extend(picks.iter().cloned());
                for mv in picks.into_iter().chain(extras) {
                    tree.edge.entry(key.clone()).or_default().insert(mv.clone());
                    let c = child(&key, &mv).unwrap();
                    let ply = tree.ply[&key] + 1;
                    tree.ply.entry(c.clone()).and_modify(|p| *p = (*p).min(ply)).or_insert(ply);
                    if seen.insert(c.clone()) {
                        next.insert(c);
                    }
                }
            }
            layer = next;
        }
        if round == tree.def.rounds {
            break;
        }
        let values = reference_negamax(tree, &reference_reach(tree, &tree.edge));
        let mut added = false;
        for (key, un) in reference_unresolved(tree, &values) {
            if tree.level[&key] < tree.def.depth {
                tree.force.entry(key).or_default().extend(un.into_iter().map(|(_, m)| m));
                added = true;
            }
        }
        if !added {
            break;
        }
        tree.round += 1;
    }
}

#[test]
fn asynchronous_closure_matches_bfs_across_rounds_and_completion_orders() {
    let settings = settings();
    let cache = corpus(&settings);
    let references: Vec<_> = (0..=3)
        .map(|rounds| {
            let dir = tempfile::tempdir().unwrap();
            let mut tree = Tree::load(definition(dir.path(), rounds), &settings).unwrap();
            bfs(&mut tree, &cache);
            tree.edge.retain(|_, v| !v.is_empty());
            tree.rule.retain(|_, v| !v.is_empty());
            (dir, tree)
        })
        .collect();
    for permutation in 0..200 {
        let dir = tempfile::tempdir().unwrap();
        let mut tree = Tree::load(definition(dir.path(), 0), &settings).unwrap();
        let mut arrived = BTreeMap::new();
        let mut random = 0x13b8_f712_u64 + permutation;
        let mut saw_deep_transposition = false;
        for (rounds, (reference_dir, reference)) in references.iter().enumerate() {
            assert!(reference_dir.path().exists());
            tree.def.rounds = rounds;
            tree.finished = false;
            for step in 0..1000 {
                let mut missing = tree.advance(&arrived).unwrap();
                let c = child(&child(ROOT, "5i4i").unwrap(), "5a4a").unwrap();
                saw_deep_transposition |= tree.level.get(&c) == Some(&4);
                if tree.finished {
                    break;
                }
                missing.sort_by_key(|(k, d)| (*d, k.clone()));
                let index = if permutation == 1 {
                    // 浅い枝を遅らせ、長い経路の子孫を先に展開する。
                    let delayed = child(ROOT, "5i4i").unwrap();
                    missing
                        .iter()
                        .rposition(|(key, _)| key != &delayed)
                        .unwrap_or(missing.len() - 1)
                } else if permutation == 0 {
                    0
                } else {
                    random ^= random << 13;
                    random ^= random >> 7;
                    random ^= random << 17;
                    random as usize % missing.len()
                };
                let (key, depth) = missing.remove(index);
                assert!(depth <= tree.def.depth);
                arrived.insert(
                    key.clone(),
                    cache
                        .get(&key)
                        .unwrap_or_else(|| panic!("missing fixture {step}: {key}"))
                        .clone(),
                );
            }
            assert!(tree.finished);
            tree.edge.retain(|_, v| !v.is_empty());
            tree.rule.retain(|_, v| !v.is_empty());
            assert_eq!(tree.nodes, reference.nodes, "NODE round={rounds}");
            assert_eq!(tree.level, reference.level, "LEVEL round={rounds}");
            assert_eq!(tree.edge, reference.edge, "EDGE round={rounds}");
            assert_eq!(tree.rule, reference.rule, "RULE round={rounds}");
            assert_eq!(tree.force, reference.force, "FORCE round={rounds}");
            let values = tree.negamax(&tree.nodes.keys().cloned().collect()).unwrap();
            assert_eq!(values, reference_values(reference), "values round={rounds}");
            assert_eq!(tree.unresolved(&values), reference_unresolved(reference, &values));
            let rule_nodes = reference_reach(reference, &reference.rule);
            assert_eq!(
                tree.negamax(&rule_nodes).unwrap(),
                reference_negamax(reference, &rule_nodes)
            );
            let clean_dir = tempfile::tempdir().unwrap();
            let mut clean = Tree::load(definition(clean_dir.path(), rounds), &settings).unwrap();
            clean.advance(&tree.nodes).unwrap();
            let full_dir = tempfile::tempdir().unwrap();
            let mut full = Tree::load(definition(full_dir.path(), rounds), &settings).unwrap();
            full.advance(&cache).unwrap();
            assert!(cache.len() > tree.nodes.len());
            for other in [&clean, &full] {
                assert_eq!(other.nodes, tree.nodes);
                assert_eq!(other.level, tree.level);
                assert_eq!(other.force, tree.force);
                assert_eq!(other.negamax(&other.nodes.keys().cloned().collect()).unwrap(), values);
            }
        }
        if permutation == 1 {
            assert!(saw_deep_transposition);
        }
    }
}

#[test]
fn parser_preserves_slots_bounds_mates_and_last_line() {
    let mut c = super::service::Collector::default();
    for line in [
        "info depth 3 nodes 5 multipv 1 score cp 30 pv 5i4i",
        "info depth 4 nodes 9 multipv 2 score cp 40 pv 5i4i",
        "info depth 5 nodes 10 multipv 1 score cp 99 upperbound pv 5i6i",
        "info depth 6 nodes 11 multipv 3 score mate -7 pv 5i6i",
        "info depth 7 nodes 12 multipv 1 score mate 5 pv 5i4i",
    ] {
        c.info(line).unwrap();
    }
    let (mp, nodes) = c.finish();
    assert_eq!(mp.iter().map(|p| p.cp).collect::<Vec<_>>(), [29995, 40, -29993]);
    assert_eq!(mp[0].mv, mp[1].mv);
    assert_eq!(nodes, Some(12));
}

#[test]
fn roots_ignore_stop_extra_rules_and_depth_game_union() {
    let s = settings();
    let cache = corpus(&s);
    let dir = tempfile::tempdir().unwrap();
    let mut def = definition(dir.path(), 0);
    def.stop = 0;
    def.extra = vec!["5i6h".into()];
    let mut tree = Tree::load(def, &s).unwrap();
    tree.advance(&cache).unwrap();
    assert!(tree.rule[ROOT].contains("5i5h"));
    assert!(tree.rule[ROOT].contains("5i4i"));
    assert!(!tree.rule[ROOT].contains("5i6h"));
    assert!(tree.game[ROOT].contains("5i6i"));
    let dir2 = tempfile::tempdir().unwrap();
    let mut def = definition(dir2.path(), 0);
    def.own = "w".into();
    def.extra = vec!["5i6h".into()];
    let mut tree = Tree::load(def, &s).unwrap();
    tree.advance(&cache).unwrap();
    assert!(tree.rule[ROOT].contains("5i6h"));
}

#[test]
fn replay_missing_reading_and_not_ready_remove_registration_material() {
    let s = settings();
    let dir = tempfile::tempdir().unwrap();
    let def = definition(dir.path(), 0);
    let mut tree = Tree::load(def.clone(), &s).unwrap();
    std::fs::create_dir_all(&def.out).unwrap();
    std::fs::write(def.out.join("own_nodes.tsv"), "stale").unwrap();
    // 木を作るだけでは出力に触れず、書き出しで登録材料を消す。
    let tree2 = Tree::load(def, &s).unwrap();
    assert!(tree2.def.out.join("own_nodes.tsv").exists());
    let cache = corpus(&s);
    tree.advance(&cache).unwrap();
    assert!(tree.finished);
    assert!(
        !tree
            .unresolved(&tree.negamax(&tree.nodes.keys().cloned().collect()).unwrap())
            .is_empty()
    );
    assert!(!tree.write_outputs().unwrap());
    assert!(!tree.def.out.join("own_nodes.tsv").exists());
    let journal_path = dir.path().join("j");
    let mut j = Journal::open(&journal_path, &s.fingerprint).unwrap();
    j.append(cache[ROOT].clone(), &s, "fixture".into()).unwrap();
    drop(j);
    let toml = dir.path().join("tree.toml");
    std::fs::write(
        &toml,
        "paths='paths.tsv'\nout='out'\ndepth=5\nopp_n=1\nstop=300\nown='b'\nrounds=0\n",
    )
    .unwrap();
    let err = run(TreeArgs {
        command: TreeCommand::Replay(ReplayArgs {
            journal: journal_path,
            tree: toml,
            fingerprint: None,
        }),
    })
    .unwrap_err();
    assert!(err.to_string().contains("読みが足りない"));
}

#[test]
fn stop_suppresses_game_but_force_is_added_afterwards_without_rule() {
    let s = settings();
    let cache = corpus(&s);
    let dir = tempfile::tempdir().unwrap();
    let mut def = definition(dir.path(), 0);
    def.stop = 0;
    let mut tree = Tree::load(def, &s).unwrap();
    let key = child(ROOT, "5i5h").unwrap();
    tree.force.insert(key.clone(), BTreeSet::from(["5a4a".into()]));
    tree.advance(&cache).unwrap();
    assert_eq!(tree.edge[&key], BTreeSet::from(["5a4a".into()]));
    assert!(tree.rule[&key].is_empty());
    assert!(tree.game[&key].contains("5a5b"));
}

#[test]
fn cycle_values_start_from_empty_ancestry_for_each_position() {
    let s = settings();
    let dir = tempfile::tempdir().unwrap();
    let mut def = definition(dir.path(), 0);
    def.draw = 17;
    let mut tree = Tree::load(def, &s).unwrap();
    let mut key = ROOT.to_owned();
    for mv in ["5i4i", "5a4a", "4i5i", "4a5a"] {
        tree.nodes.insert(key.clone(), reading(&key, &[(mv, 0)], &s));
        tree.edge.insert(key.clone(), BTreeSet::from([mv.into()]));
        key = child(&key, mv).unwrap();
    }
    assert_eq!(key, ROOT);
    let values = tree.negamax(&tree.nodes.keys().cloned().collect()).unwrap();
    for (key, edges) in &tree.edge {
        assert_eq!(values[key][edges.first().unwrap()], -17);
    }
    assert_eq!(values, reference_values(&tree));
}

#[test]
fn transpositions_into_a_cycle_match_the_reference_and_finish_past_the_exact_budget() {
    let s = settings();
    let dir = tempfile::tempdir().unwrap();
    let mut def = definition(dir.path(), 0);
    def.draw = 17;
    let mut tree = Tree::load(def, &s).unwrap();
    // 根から 2 通りの手順で同じ局面 (玉 3i・相手玉 4a) に合流し、そこから 4 手の循環に入る。
    let put = |tree: &mut Tree, key: &str, moves: &[&str]| {
        let lines: Vec<_> = moves.iter().map(|m| (*m, 0)).collect();
        tree.nodes.insert(key.to_owned(), reading(key, &lines, &s));
        tree.edge
            .insert(key.to_owned(), moves.iter().map(|m| (*m).to_owned()).collect());
        moves.iter().map(|m| child(key, m).unwrap()).collect::<Vec<_>>()
    };
    let [a1, a2]: [String; 2] = put(&mut tree, ROOT, &["5i4i", "5i4h"]).try_into().unwrap();
    let [b1]: [String; 1] = put(&mut tree, &a1, &["5a4a"]).try_into().unwrap();
    let [b2]: [String; 1] = put(&mut tree, &a2, &["5a4a"]).try_into().unwrap();
    let [c]: [String; 1] = put(&mut tree, &b1, &["4i3i"]).try_into().unwrap();
    assert_eq!(put(&mut tree, &b2, &["4h3i"]), std::slice::from_ref(&c));
    let [d]: [String; 1] = put(&mut tree, &c, &["4a5a"]).try_into().unwrap();
    let [e]: [String; 1] = put(&mut tree, &d, &["3i4i"]).try_into().unwrap();
    assert_eq!(put(&mut tree, &e, &["5a4a"]), std::slice::from_ref(&b1));
    let nodes: BTreeSet<String> = tree.nodes.keys().cloned().collect();
    assert_eq!(tree.negamax(&nodes).unwrap(), reference_values(&tree));
    let approximate = tree.negamax_within(&nodes, 0).unwrap();
    assert_eq!(approximate.keys().collect::<Vec<_>>(), nodes.iter().collect::<Vec<_>>());
}

#[test]
fn cycle_values_use_the_draw_of_the_side_that_closes_the_cycle() {
    let s = settings();
    // 4 手の循環は相手が閉じるので、各局面の値は閉じた側の値の符号を返したもの。
    // (draw_black, draw_white) → (先手番の局面の値, 後手番の局面の値)。省いた側は draw (17)。
    for (black, white, expected) in [
        (Some(-70), Some(70), (-70, 70)),
        (Some(-70), None, (-17, 70)),
        (None, Some(30), (-30, -17)),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut def = definition(dir.path(), 0);
        def.draw = 17;
        def.draw_black = black;
        def.draw_white = white;
        let mut tree = Tree::load(def, &s).unwrap();
        let mut key = ROOT.to_owned();
        for mv in ["5i4i", "5a4a", "4i5i", "4a5a"] {
            tree.nodes.insert(key.clone(), reading(&key, &[(mv, 0)], &s));
            tree.edge.insert(key.clone(), BTreeSet::from([mv.into()]));
            key = child(&key, mv).unwrap();
        }
        let values = tree.negamax(&tree.nodes.keys().cloned().collect()).unwrap();
        for (key, edges) in &tree.edge {
            let want = if key.split(' ').nth(1) == Some("b") {
                expected.0
            } else {
                expected.1
            };
            assert_eq!(values[key][edges.first().unwrap()], want, "{black:?} {white:?} {key}");
        }
        assert_eq!(values, reference_values(&tree));
    }
}

#[test]
fn declaration_win_leaf_value_comes_from_its_win_line() {
    let s = settings();
    let dir = tempfile::tempdir().unwrap();
    let mut tree = Tree::load(definition(dir.path(), 0), &s).unwrap();
    let win = child(ROOT, "5i4i").unwrap();
    tree.nodes.insert(ROOT.into(), reading(ROOT, &[("5i4i", 0)], &s));
    tree.edge.insert(ROOT.into(), BTreeSet::from(["5i4i".into()]));
    let mut r = reading(&win, &[], &s);
    r.bestmove = "win".into();
    r.multipv = vec![Pv {
        mv: "win".into(),
        cp: 30000,
        depth: 1,
    }];
    tree.nodes.insert(win.clone(), r.clone());
    let values = tree.negamax(&tree.nodes.keys().cloned().collect()).unwrap();
    assert_eq!(values[ROOT]["5i4i"], -30000);
    assert!(values[&win].is_empty());
    r.multipv.clear();
    tree.nodes.insert(win, r);
    let values = tree.negamax(&tree.nodes.keys().cloned().collect()).unwrap();
    assert_eq!(values[ROOT]["5i4i"], 0);
}

fn reference_values(tree: &Tree) -> super::model::Values {
    reference_negamax(tree, &tree.nodes.keys().cloned().collect())
}

fn reference_negamax(tree: &Tree, nodes: &BTreeSet<String>) -> super::model::Values {
    fn evaluate(
        tree: &Tree,
        nodes: &BTreeSet<String>,
        key: &str,
        path: Vec<String>,
    ) -> BTreeMap<String, i32> {
        let mut rows: BTreeMap<_, _> =
            tree.nodes[key].multipv.iter().map(|p| (p.mv.clone(), p.cp)).collect();
        let moves: BTreeSet<_> = rows
            .keys()
            .cloned()
            .chain(tree.edge.get(key).into_iter().flatten().cloned())
            .collect();
        for mv in moves {
            let c = child(key, &mv).unwrap();
            if !nodes.contains(&c) {
                continue;
            }
            let value = if c == key || path.contains(&c) {
                let own = if key.split(' ').nth(1) == Some("b") {
                    tree.def.draw_black
                } else {
                    tree.def.draw_white
                };
                own.unwrap_or(tree.def.draw)
            } else {
                let mut ancestors = path.clone();
                ancestors.push(key.into());
                -evaluate(tree, nodes, &c, ancestors).values().max().copied().unwrap_or(0)
            };
            rows.insert(mv, value);
        }
        rows
    }
    nodes
        .iter()
        .map(|key| (key.clone(), evaluate(tree, nodes, key, Vec::new())))
        .collect()
}

fn reference_reach(tree: &Tree, edges: &BTreeMap<String, BTreeSet<String>>) -> BTreeSet<String> {
    let mut reached = BTreeSet::new();
    let mut pending: Vec<_> = tree.roots.keys().cloned().collect();
    while let Some(key) = pending.pop() {
        if tree.nodes.contains_key(&key) && reached.insert(key.clone()) {
            pending.extend(edges.get(&key).into_iter().flatten().map(|m| child(&key, m).unwrap()));
        }
    }
    reached
}

fn reference_unresolved(tree: &Tree, values: &super::model::Values) -> super::model::Pending {
    let mut result = BTreeMap::new();
    for (key, rows) in values {
        let edges = tree.edge.get(key).cloned().unwrap_or_default();
        if key.split_whitespace().nth(1) != Some(tree.def.own.as_str()) || edges.is_empty() {
            continue;
        }
        let best = edges.iter().map(|m| rows.get(m).copied().unwrap_or(-99999)).max().unwrap();
        let un: Vec<_> = tree.nodes[key]
            .multipv
            .iter()
            .filter(|p| !edges.contains(&p.mv) && p.cp > best)
            .map(|p| (p.cp, p.mv.clone()))
            .collect();
        if !un.is_empty() {
            result.insert(key.clone(), un);
        }
    }
    result
}

#[test]
fn explicit_model_is_required_and_invalid_paths_leave_outputs_untouched() {
    assert!(
        Settings::new(&ReadingArgs {
            engine: std::env::current_exe().unwrap(),
            engine_options: vec![],
            nodes: 100,
            multipv: 3,
            hash: 16
        })
        .is_err()
    );
    let s = settings();
    let dir = tempfile::tempdir().unwrap();
    let mut def = definition(dir.path(), 0);
    std::fs::create_dir(&def.out).unwrap();
    std::fs::write(def.out.join("own_nodes.tsv"), "stale").unwrap();
    def.paths_sha256 = Some("wrong".into());
    assert!(Tree::load(def.clone(), &s).is_err());
    def.paths_sha256 = None;
    def.paths = dir.path().join("absent");
    assert!(Tree::load(def.clone(), &s).is_err());
    assert_eq!(std::fs::read_to_string(def.out.join("own_nodes.tsv")).unwrap(), "stale");
}

#[test]
fn model_and_implicit_parameters_are_content_fingerprints() {
    let dir = tempfile::tempdir().unwrap();
    let engine = dir.path().join("engine");
    let model = dir.path().join("model");
    std::fs::write(&engine, "engine").unwrap();
    std::fs::write(&model, "model").unwrap();
    let args = ReadingArgs {
        engine,
        engine_options: vec![format!("EvalFile={}", model.display())],
        nodes: 100,
        multipv: 3,
        hash: 16,
    };
    let first = Settings::new(&args).unwrap();
    let params = dir.path().join("spsa.params");
    std::fs::write(&params, "first").unwrap();
    let second = Settings::new(&args).unwrap();
    assert_ne!(first.fingerprint, second.fingerprint);
    std::fs::write(&params, "second").unwrap();
    assert_ne!(second.fingerprint, Settings::new(&args).unwrap().fingerprint);
    std::fs::remove_file(params).unwrap();
    assert_eq!(first.fingerprint, Settings::new(&args).unwrap().fingerprint);
    std::fs::write(model, "different model").unwrap();
    assert_ne!(first.fingerprint, Settings::new(&args).unwrap().fingerprint);
}

#[test]
fn empty_game_adds_second_candidate_and_status_failure_does_not_block_outputs() {
    let s = settings();
    let dir = tempfile::tempdir().unwrap();
    let mut def = definition(dir.path(), 0);
    std::fs::write(&def.paths, format!("{ROOT}\t20\t\tfirst\n{ROOT}\t10\t\tsecond\n")).unwrap();
    def.depth = 0;
    def.stop = 0;
    def.extra.clear();
    let mut tree = Tree::load(def, &s).unwrap();
    let cache = BTreeMap::from([(
        ROOT.into(),
        reading(ROOT, &[("5i4i", 500), ("5i6i", 400), ("5i5h", 300)], &s),
    )]);
    tree.advance(&cache).unwrap();
    assert!(tree.finished);
    assert_eq!(tree.candidates[ROOT], vec!["5i4i", "5i6i"]);
    assert_eq!(tree.ply[ROOT], 20);
    std::fs::create_dir_all(tree.def.out.join("STATUS")).unwrap();
    assert!(tree.write_outputs().unwrap());
    assert!(tree.def.out.join("own_nodes.tsv").exists());
}

#[test]
fn short_multipv_and_terminal_readings_are_leaves() {
    let s = settings();
    for (key, count) in [
        ("k8/9/R8/9/9/9/9/9/8K w -", 2),
        ("k8/9/RP7/9/9/9/9/9/8K w -", 1),
        ("k8/1G7/RP7/9/9/9/9/9/8K w -", 0),
    ] {
        let moves = legal(key);
        assert_eq!(moves.len(), count);
        let mut r = reading(ROOT, &[("5i4i", 0)], &s);
        r.key = key.into();
        r.multipv = moves
            .iter()
            .map(|mv| Pv {
                mv: mv.clone(),
                cp: 500,
                depth: 8,
            })
            .collect();
        r.bestmove = moves.first().cloned().unwrap_or("resign".into());
        r.validate(&s).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mut def = definition(dir.path(), 0);
        std::fs::write(&def.paths, format!("{key}\t1\t\tterminal\n")).unwrap();
        def.depth = 0;
        def.own = "w".into();
        def.extra.clear();
        let mut tree = Tree::load(def, &s).unwrap();
        tree.advance(&BTreeMap::from([(key.into(), r.clone())])).unwrap();
        assert!(tree.finished);
        assert!(tree.write_outputs().unwrap());
        if count == 0 {
            assert!(tree.negamax(&tree.nodes.keys().cloned().collect()).unwrap()[key].is_empty());
            r.bestmove = "win".into();
            r.validate(&s).unwrap();
        }
    }
}

#[test]
fn replay_locks_before_touching_candidates_or_status_and_not_ready_returns_error() {
    let s = settings();
    let dir = tempfile::tempdir().unwrap();
    let def = definition(dir.path(), 0);
    std::fs::create_dir(&def.out).unwrap();
    let toml = dir.path().join("tree.toml");
    std::fs::write(
        &toml,
        "paths='paths.tsv'\nout='out'\ndepth=5\nopp_n=1\nstop=300\nown='b'\nrounds=0\n",
    )
    .unwrap();
    let journal_path = dir.path().join("journal");
    let mut journal = Journal::open(&journal_path, &s.fingerprint).unwrap();
    for r in corpus(&s).into_values() {
        journal.append(r, &s, "fixture".into()).unwrap();
    }
    let replay = || {
        run(TreeArgs {
            command: TreeCommand::Replay(ReplayArgs {
                journal: journal_path.clone(),
                tree: toml.clone(),
                fingerprint: None,
            }),
        })
    };
    std::fs::write(def.out.join("own_nodes.tsv"), "protected").unwrap();
    std::fs::write(def.out.join("STATUS"), "RUNNING\n").unwrap();
    assert!(replay().is_err());
    assert_eq!(std::fs::read_to_string(def.out.join("own_nodes.tsv")).unwrap(), "protected");
    drop(journal);
    let lock = super::journal::LockedFile::open(&def.out.join("tree.lock")).unwrap();
    assert!(replay().is_err());
    assert_eq!(std::fs::read_to_string(def.out.join("STATUS")).unwrap(), "RUNNING\n");
    assert_eq!(std::fs::read_to_string(def.out.join("own_nodes.tsv")).unwrap(), "protected");
    drop(lock);
    assert!(replay().unwrap_err().to_string().contains("登録禁止"));
    assert_eq!(std::fs::read_to_string(def.out.join("STATUS")).unwrap(), "NOT_READY\n");
    assert!(!def.out.join("own_nodes.tsv").exists());
}

#[test]
fn min_failure_is_reported_and_done_follows_all_artifacts() {
    let s = settings();
    let dir = tempfile::tempdir().unwrap();
    let mut def = definition(dir.path(), 0);
    def.depth = 0;
    let mut tree = Tree::load(def, &s).unwrap();
    tree.advance(&corpus(&s)).unwrap();
    std::fs::create_dir_all(tree.def.out.join("tree_bp_min.db")).unwrap();
    assert!(tree.write_outputs().unwrap());
    let report = std::fs::read_to_string(tree.def.out.join("report.txt")).unwrap();
    assert!(report.contains("book_backprop (min、参考) が失敗した:"));
    assert!(report.starts_with(&format!("根 {ROOT} 候補 ['5i5h', '5i4i', '5i6i'] 比べる手")));
    assert!(tree.def.out.join("own_nodes.tsv").exists());
    assert_eq!(std::fs::read_to_string(tree.def.out.join("STATUS")).unwrap(), "DONE\n");
}

#[test]
fn opponent_root_expands_opp_n_replies() {
    // 相手の手番の根では MultiPV の上位 opp_n 手を伸ばし、自分の手番の根では 1 位と手順の手だけを伸ばす。
    let s = settings();
    let dir = tempfile::tempdir().unwrap();
    let mut def = definition(dir.path(), 0);
    def.depth = 1;
    def.opp_n = 3;
    def.extra = Vec::new();
    def.own = "w".into();
    let mut tree = Tree::load(def, &s).unwrap();
    tree.advance(&corpus(&s)).unwrap();
    assert!(tree.candidates[ROOT].contains(&"5i6i".to_owned()));
    let mut def = definition(dir.path(), 0);
    def.depth = 1;
    def.opp_n = 3;
    def.extra = Vec::new();
    def.out = dir.path().join("out-own");
    let mut tree = Tree::load(def, &s).unwrap();
    tree.advance(&corpus(&s)).unwrap();
    assert!(!tree.candidates[ROOT].contains(&"5i6i".to_owned()));
}

#[test]
fn report_includes_opponent_candidates_and_unexpanded_warning() {
    let s = settings();
    let dir = tempfile::tempdir().unwrap();
    let mut def = definition(dir.path(), 0);
    def.depth = 0;
    def.own = "w".into();
    let mut tree = Tree::load(def, &s).unwrap();
    tree.advance(&corpus(&s)).unwrap();
    tree.write_outputs().unwrap();
    assert!(
        std::fs::read_to_string(tree.def.out.join("report.txt"))
            .unwrap()
            .starts_with(&format!("根 (相手の手番) {ROOT} 伸ばす手"))
    );
    tree.def.own = "b".into();
    tree.candidates.insert(ROOT.into(), vec!["5i6i".into()]);
    tree.write_outputs().unwrap();
    assert!(std::fs::read_to_string(tree.def.out.join("report.txt")).unwrap()
        .contains("警告: 伸ばしていない手の読みの値が、候補の伝播後の 1 位を上回る (その手も伸ばしてから決める): ['5i5h', '5i4i']"));
}

#[test]
fn normalized_output_paths_reject_protected_files_and_overlapping_trees() {
    let dir = tempfile::tempdir().unwrap();
    let mut def = definition(dir.path(), 0);
    let journal = dir.path().join("journal");
    let state = dir.path().join("state");
    let reject = |d: &Definition, j: &Path| super::paths::reject_collisions(j, Some(&state), &[d]);
    for name in ["d0.jsonl", "own_nodes.tsv", "tree.lock", "unused/sub/file"] {
        assert!(reject(&def, &def.out.join(name)).is_err());
    }
    for out in [&state, &state.join("child"), dir.path()] {
        def.out = out.to_owned();
        assert!(reject(&def, &journal).is_err());
    }
    def.out = dir.path().join("out");
    for name in ["paths.tsv", "lock", "inbox/paths.tsv"] {
        def.paths = state.join(name);
        assert!(reject(&def, &journal).is_err());
    }
    def.paths = def.out.join("new/../paths.tsv");
    assert!(reject(&def, &journal).is_err());
    def.paths = dir.path().join("paths.tsv");
    assert!(reject(&def, &def.paths).is_err());
    let mut other = def.clone();
    for out in [
        def.out.clone(),
        def.out.join("child"),
        dir.path().to_owned(),
    ] {
        other.out = out;
        assert!(super::paths::reject_collisions(&journal, None, &[&def, &other]).is_err());
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(dir.path(), dir.path().join("alias")).unwrap();
        other.out = dir.path().join("alias/out/new/..");
        assert!(super::paths::reject_collisions(&journal, None, &[&def, &other]).is_err());
        std::fs::create_dir(&def.out).unwrap();
        std::fs::write(&journal, "protected").unwrap();
        std::os::unix::fs::symlink(&journal, def.out.join("d0.jsonl")).unwrap();
        assert!(super::paths::reject_collisions(&journal, None, &[&def]).is_err());
        std::fs::remove_file(def.out.join("d0.jsonl")).unwrap();
        std::os::unix::fs::symlink(&journal, def.out.join("STATUS")).unwrap();
        assert!(super::paths::reject_collisions(&journal, None, &[&def]).is_err());
        std::fs::remove_file(def.out.join("STATUS")).unwrap();
    }
    other.out = dir.path().join("other");
    other.paths = def.out.join("d0.jsonl");
    assert!(super::paths::reject_collisions(&journal, None, &[&def, &other]).is_err());
    other.paths = def.paths.clone();
    super::paths::reject_collisions(&journal, Some(&state), &[&def, &other]).unwrap();
}

#[test]
fn replay_rejects_journal_and_paths_collisions_before_changing_outputs() {
    let dir = tempfile::tempdir().unwrap();
    let def = definition(dir.path(), 0);
    std::fs::create_dir(&def.out).unwrap();
    std::fs::write(def.out.join("own_nodes.tsv"), "protected").unwrap();
    let toml = dir.path().join("tree.toml");
    std::fs::write(
        &toml,
        "paths='paths.tsv'\nout='out'\ndepth=0\nopp_n=1\nstop=300\nown='b'\nrounds=0\n",
    )
    .unwrap();
    for journal in [
        def.out.join("d0.jsonl"),
        def.out.join("tree.lock"),
        def.paths.clone(),
    ] {
        std::fs::write(&journal, "protected").unwrap();
        let error = run(TreeArgs {
            command: TreeCommand::Replay(ReplayArgs {
                journal: journal.clone(),
                tree: toml.clone(),
                fingerprint: None,
            }),
        })
        .unwrap_err();
        assert!(error.to_string().contains("衝突"));
        assert_eq!(std::fs::read_to_string(&journal).unwrap(), "protected");
        assert_eq!(std::fs::read_to_string(def.out.join("own_nodes.tsv")).unwrap(), "protected");
    }
}

fn serve_args(state: &Path, journal: &Path, out_root: &Path, engine: &Path) -> ServeArgs {
    ServeArgs {
        accept_fingerprint: Vec::new(),
        state: state.to_owned(),
        journal: Some(journal.to_owned()),
        reading: ReadingArgs {
            engine: engine.to_owned(),
            engine_options: vec![format!("EvalFile={}", engine.display())],
            nodes: 100,
            multipv: 3,
            hash: 16,
        },
        workers: 0,
        stall_secs: 1,
        prefetch: false,
        prefetch_workers: None,
        prefetch_preempt_below: 0.25,
        prefetch_work: super::model::PREFETCH_WORK,
        speculate_at: None,
        speculate_priority: None,
        max_restarts: 0,
        out_root: out_root.to_owned(),
        opp_n: 2,
        stop: 300,
        rounds: 3,
        draw: 0,
        trace_dir: None,
    }
}

#[test]
fn serve_startup_rejects_old_checkpoints_and_out_root_collisions_before_changing_outputs() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    std::fs::create_dir(&state).unwrap();
    let def = definition(dir.path(), 0);
    let out_root = dir.path().join("trees");
    let journal = dir.path().join("journal.jsonl");
    std::fs::write(&journal, "protected").unwrap();
    for inside in [state.clone(), state.join("trees")] {
        let error = service::serve(serve_args(&state, &journal, &inside, &def.paths)).unwrap_err();
        assert!(error.to_string().contains("衝突"), "{error:#}");
    }
    assert_eq!(std::fs::read_to_string(&journal).unwrap(), "protected");
    assert!(!state.join("targets.status.json").exists());
    let journal = dir.path().join("fresh.jsonl");
    std::fs::write(state.join("accepted.json"), "[]").unwrap();
    let error = service::serve(serve_args(&state, &journal, &out_root, &def.paths)).unwrap_err();
    assert!(error.to_string().contains("inbox 受付の state"), "{error:#}");
    assert!(!journal.exists());
    assert!(!state.join("targets.status.json").exists());
}

#[test]
fn serve_startup_rejects_trace_dir_on_state_out_root_or_journal() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    std::fs::create_dir(&state).unwrap();
    let def = definition(dir.path(), 0);
    let out_root = dir.path().join("trees");
    let journal = dir.path().join("journal.jsonl");
    for trace in [
        state.join("stop"),
        state.join("restart"),
        state.join("trace"),
        state.clone(),
        out_root.join("trace"),
        journal.clone(),
    ] {
        let mut args = serve_args(&state, &journal, &out_root, &def.paths);
        args.trace_dir = Some(trace.clone());
        let error = service::serve(args).unwrap_err();
        assert!(error.to_string().contains("trace-dir と保護パスが衝突"), "{trace:?}: {error:#}");
        assert!(!state.join("stop").exists());
        assert!(!state.join("trace").exists());
        assert!(!out_root.exists());
        assert!(!journal.exists());
        assert!(!state.join("targets.status.json").exists());
    }
}

#[cfg(unix)]
#[test]
fn serve_startup_rejects_symlinked_trace_dir() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    std::fs::create_dir(&state).unwrap();
    let def = definition(dir.path(), 0);
    let real = dir.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let trace = dir.path().join("trace");
    std::os::unix::fs::symlink(&real, &trace).unwrap();
    let journal = dir.path().join("journal.jsonl");
    let mut args = serve_args(&state, &journal, &dir.path().join("trees"), &def.paths);
    args.trace_dir = Some(trace);
    let error = service::serve(args).unwrap_err();
    assert!(error.to_string().contains("シンボリックリンク"), "{error:#}");
    assert!(std::fs::read_dir(&real).unwrap().next().is_none());
}

#[cfg(unix)]
#[test]
fn managed_paths_reject_live_and_dangling_symlinks() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let def = definition(dir.path(), 0);
    let journal = dir.path().join("journal");
    let state = dir.path().join("state");
    std::fs::create_dir_all(state.join("restart")).unwrap();
    std::fs::create_dir(&def.out).unwrap();
    for exists in [false, true] {
        let target = dir.path().join("target");
        if exists {
            std::fs::write(&target, "preserve").unwrap();
        }
        for path in [
            journal.clone(),
            state.join("lock"),
            state.join("accepted.json"),
            def.out.join("d0.jsonl"),
            def.out.join("STATUS"),
        ] {
            symlink(&target, &path).unwrap();
            let error =
                super::paths::reject_collisions(&journal, Some(&state), &[&def]).unwrap_err();
            assert!(error.to_string().contains("シンボリックリンク"), "{error:#}");
            std::fs::remove_file(path).unwrap();
        }
        for path in [&def.out, &state.join("restart"), &state] {
            std::fs::remove_dir_all(path).unwrap();
            symlink(&target, path).unwrap();
            assert!(super::paths::reject_collisions(&journal, Some(&state), &[&def]).is_err());
            std::fs::remove_file(path).unwrap();
            std::fs::create_dir_all(path).unwrap();
        }
        std::fs::create_dir_all(state.join("restart")).unwrap();
    }
}

#[cfg(unix)]
#[test]
fn definitions_reject_symlinks_and_protect_real_path_from_outputs() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    std::fs::create_dir(&out).unwrap();
    let source = out.join("tree.toml");
    let link = dir.path().join("link.toml");
    std::os::unix::fs::symlink(&source, &link).unwrap();
    assert!(Definition::load(&link).unwrap_err().to_string().contains("シンボリックリンク"));
    std::fs::write(
        &source,
        "paths='../paths.tsv'\nout='.'\ndepth=0\nopp_n=1\nstop=300\nown='b'\nrounds=0\n",
    )
    .unwrap();
    assert!(Definition::load(&link).is_err());
    let def = Definition::load(&source).unwrap();
    let journal = dir.path().join("journal");
    assert!(super::paths::reject_collisions(&journal, None, &[&def]).is_err());
    let alias = dir.path().join("alias");
    std::os::unix::fs::symlink(&out, &alias).unwrap();
    let def = Definition::load(&alias.join("tree.toml")).unwrap();
    assert!(super::paths::reject_collisions(&journal, None, &[&def]).is_err());
    let mut without_source = def.clone();
    without_source.source = None;
    super::paths::reject_collisions(&journal, None, &[&without_source]).unwrap();
}

#[test]
fn single_pv_win_is_a_leaf_and_mixed_win_rows_are_rejected() {
    let s = settings();
    let mut collector = super::service::Collector::default();
    collector
        .info(
            "info depth 1 seldepth 0 multipv 1 score mate 0 nodes 0 time 0 nps 0 hashfull 0 pv win",
        )
        .unwrap();
    let mut r = reading(ROOT, &[], &s);
    r.bestmove = "win".into();
    (r.multipv, r.nodes) = collector.finish();
    r.validate(&s).unwrap();
    assert!(r.lines().is_empty());
    let dir = tempfile::tempdir().unwrap();
    let mut def = definition(dir.path(), 0);
    std::fs::write(&def.paths, format!("{ROOT}\t1\t\n")).unwrap();
    def.extra.clear();
    let mut tree = Tree::load(def, &s).unwrap();
    tree.advance(&BTreeMap::from([(ROOT.into(), r.clone())])).unwrap();
    assert!(tree.finished);
    assert!(tree.write_outputs().unwrap());
    assert!(tree.raw_rows(ROOT).is_empty());
    r.multipv.push(Pv {
        mv: "5i4i".into(),
        cp: 0,
        depth: 1,
    });
    assert!(r.validate(&s).is_err());
    r.multipv.remove(0);
    assert!(r.validate(&s).is_err());
    let mut collector = super::service::Collector::default();
    assert!(collector.info("info depth 1 multipv 2 score mate 0 pv win").is_err());
}

#[cfg(unix)]
#[test]
fn bw_line_is_rejected_whole_when_one_side_cannot_be_built() {
    use sha2::Digest;
    let dir = tempfile::tempdir().unwrap();
    let out_root = dir.path().join("trees");
    std::fs::create_dir_all(out_root.join("x")).unwrap();
    std::os::unix::fs::symlink(dir.path(), out_root.join("x/w")).unwrap();
    let checkpoint = targets::Checkpoint::new(out_root, "fp".into());
    let defaults = targets::Defaults {
        opp_n: 2,
        stop: 300,
        rounds: 3,
        draw: 0,
    };
    let lines = format!(
        "{}\n{}\n",
        serde_json::json!({"id":"x","root":format!("sfen {ROOT} 1"),"own":"bw","depth":2}),
        serde_json::json!({"id":"y","root":format!("sfen {ROOT} 1"),"own":"bw","depth":2})
    );
    let (next, built) = checkpoint.apply(lines.as_bytes(), &defaults, &sha2::Sha256::new(), &[]);
    assert_eq!(next.rejected.len(), 1);
    assert_eq!(next.rejected[0].id.as_deref(), Some("x"));
    assert!(next.rejected[0].reason.contains("シンボリックリンク"));
    let accepted: Vec<_> =
        next.targets.iter().map(|t| (t.id.as_str(), t.own.as_str(), t.order)).collect();
    assert_eq!(accepted, [("y", "b", 0), ("y", "w", 0)]);
    assert_eq!(built.len(), 2);
    assert_eq!(next.processed_bytes, lines.len() as u64);
}

#[test]
fn node_moves_lists_both_sides_with_propagated_values() {
    let s = settings();
    let dir = tempfile::tempdir().unwrap();
    let mut tree = Tree::load(definition(dir.path(), 3), &s).unwrap();
    let cache = corpus(&s);
    while !tree.finished {
        assert!(tree.advance(&cache).unwrap().is_empty() || tree.finished);
    }
    let parent = ["5i5h", "5a5b", "5h4i"]
        .iter()
        .fold(ROOT.to_owned(), |key, mv| child(&key, mv).unwrap());
    let destination = child(&parent, "5b4a").unwrap();
    assert!(tree.edge.get_mut(&parent).unwrap().remove("5b4a"));
    assert!(tree.reach(&tree.edge).contains(&destination));
    assert!(tree.write_outputs().unwrap());
    let text = std::fs::read_to_string(tree.def.out.join("node_moves.tsv")).unwrap();
    let nodes =
        super::register::parse_node_moves(&text, 0, Some(&tree.nodes.keys().cloned().collect()))
            .unwrap();
    let values = reference_values(&tree);
    assert!(nodes.keys().any(|k| side(k) == "w"));
    assert!(nodes.keys().any(|k| side(k) == "b"));
    let mut joined = 0;
    let mut single = 0;
    for (key, node) in &nodes {
        let expected: Vec<_> = tree.rank(key, &values[key], values[key].keys().cloned());
        let got: Vec<_> = node.moves.iter().map(|m| m.mv.clone()).collect();
        assert_eq!(got, expected, "{key}");
        for m in &node.moves {
            assert_eq!(m.value, values[key][&m.mv], "{key} {}", m.mv);
            let destination = child(key, &m.mv).unwrap();
            if let Some(child_node) = nodes.get(&destination) {
                assert_eq!(m.value_kind, format!("tree:{}", child_node.remaining + 1));
                if !tree.edge.get(key).is_some_and(|edges| edges.contains(&m.mv)) {
                    joined += 1;
                }
            } else if tree.nodes.contains_key(&destination) {
                assert_eq!(m.value_kind, "unknown");
            } else {
                assert_eq!(m.value_kind, "single");
                single += 1;
            }
        }
        assert_eq!(node.remaining, tree.def.depth - tree.level[key]);
    }
    assert!(joined > 0);
    assert!(single > 0);
}

#[test]
fn registration_fingerprint_requires_uniform_reading_provenance() {
    struct Cache<'a> {
        rows: &'a BTreeMap<String, Reading>,
        fingerprint: Option<&'a str>,
    }
    impl journal::Readings for Cache<'_> {
        fn get(&self, key: &str) -> Option<&Reading> {
            self.rows.get(key)
        }
        fn fingerprint(&self) -> Option<&str> {
            self.fingerprint
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let settings = settings();
    let rows = corpus(&settings);
    for mixed in [false, true] {
        let mut tree = Tree::load(definition(dir.path(), 2), &settings).unwrap();
        let partial = BTreeMap::from([(ROOT.to_owned(), rows[ROOT].clone())]);
        tree.advance(&Cache {
            rows: &partial,
            fingerprint: Some(&settings.fingerprint),
        })
        .unwrap();
        let other = "b".repeat(64);
        let cache = Cache {
            rows: &rows,
            fingerprint: Some(if mixed { &other } else { &settings.fingerprint }),
        };
        while !tree.finished {
            assert!(tree.advance(&cache).unwrap().is_empty());
        }
        tree.write_outputs().unwrap();
        let status: serde_json::Value = serde_json::from_slice(
            &std::fs::read(tree.def.out.join("registration_status.json")).unwrap(),
        )
        .unwrap();
        if mixed {
            assert!(status.get("fingerprint").is_none());
        } else {
            assert_eq!(status["fingerprint"], settings.fingerprint);
        }
        assert!(status.get("ready").is_some() && status.get("unresolved").is_some());
    }
}

#[test]
fn simulation_serial_bound_and_completion_order_preserve_tree() {
    use super::simulation::{Sample, Timing, preflight, simulate};
    use std::sync::Arc;

    let dir = tempfile::tempdir().unwrap();
    let settings = settings();
    let def = definition(dir.path(), 3);
    let cache = corpus(&settings);
    let mut expected = Tree::load(def.clone(), &settings).unwrap();
    assert!(expected.advance(&cache).unwrap().is_empty());
    assert!(expected.finished && !expected.force.is_empty());
    let lines = std::fs::read_to_string(&def.paths)
        .unwrap()
        .lines()
        .map(|line| {
            let cols: Vec<_> = line.split('\t').collect();
            (
                cols[0].into(),
                cols[1].parse().unwrap(),
                cols[2].split_whitespace().map(str::to_owned).collect(),
            )
        })
        .collect::<Vec<_>>();
    for reversed in [false, true] {
        let mut answers = cache.clone();
        let len = answers.len();
        for (i, reading) in answers.values_mut().enumerate() {
            reading.sec = if reversed { len - i } else { i + 1 } as f64;
        }
        let mut sample = Sample {
            params: def.params(),
            lines: lines.clone(),
            answers: Arc::new(answers),
            requested: BTreeSet::new(),
            fingerprint: "condition-a".into(),
            priority: 100,
            order: 0,
        };
        preflight(&mut sample).unwrap();
        for workers in [1, 4] {
            for timing in [Timing::Maximum, Timing::Mean] {
                let result = simulate(&[&sample], workers, timing).unwrap();
                assert!(result.elapsed + 1e-9 >= result.lower);
                if workers == 1 {
                    let sum = match timing {
                        Timing::Maximum => {
                            expected.nodes.keys().map(|key| sample.answers[key].sec).sum::<f64>()
                        }
                        Timing::Mean => {
                            sample.requested.iter().map(|key| sample.answers[key].sec).sum::<f64>()
                                / sample.requested.len() as f64
                                * expected.nodes.len() as f64
                        }
                    };
                    assert!((result.elapsed - sum).abs() < 1e-9);
                    assert!((result.elapsed - result.lower).abs() < 1e-9);
                    assert!(result.utilization.iter().all(|u| (u - 1.0).abs() < 1e-9));
                }
                let actual = &result.trees[0];
                assert_eq!(
                    actual.nodes.keys().collect::<Vec<_>>(),
                    expected.nodes.keys().collect::<Vec<_>>()
                );
                for (key, reading) in &actual.nodes {
                    assert_eq!(
                        serde_json::to_value(reading).unwrap(),
                        serde_json::to_value(&sample.answers[key]).unwrap()
                    );
                }
                assert_eq!(actual.edge, expected.edge);
                assert_eq!(actual.force, expected.force);
                assert_eq!(result.count, actual.nodes.len());
            }
        }
    }
}

#[test]
fn simulation_shared_keys_use_each_trees_answers_and_maximum_duration() {
    use super::simulation::{Sample, Timing, preflight, simulate};
    use std::sync::Arc;

    let dir = tempfile::tempdir().unwrap();
    let settings = settings();
    let mut params = definition(dir.path(), 0).params();
    params.depth = 0;
    let mut a = reading(ROOT, &[("5i5h", 30)], &settings);
    a.sec = 2.0;
    let mut b = reading(ROOT, &[("5i4i", 90)], &settings);
    b.sec = 5.0;
    let samples: Vec<_> = [a, b]
        .into_iter()
        .enumerate()
        .map(|(i, answer)| {
            let mut sample = Sample {
                params: params.clone(),
                lines: vec![(ROOT.into(), 1, vec![])],
                answers: Arc::new(BTreeMap::from([(ROOT.into(), answer)])),
                requested: BTreeSet::new(),
                fingerprint: format!("condition-{i}"),
                priority: 100,
                order: i as u64,
            };
            preflight(&mut sample).unwrap();
            sample
        })
        .collect();
    for selected in [[&samples[0], &samples[1]], [&samples[1], &samples[0]]] {
        let result = simulate(&selected, 2, Timing::Maximum).unwrap();
        assert_eq!(result.count, 1);
        assert_eq!(result.elapsed, 5.0);
        assert_eq!(result.lower, 2.5);
        assert_eq!(result.mixed, 1);
        let mean = simulate(&selected, 2, Timing::Mean).unwrap();
        assert_eq!(mean.elapsed, 5.0);
        assert_eq!(mean.lower, 5.0);
        for (tree, sample) in result.trees.iter().zip(selected) {
            assert_eq!(
                serde_json::to_value(&tree.nodes[ROOT]).unwrap(),
                serde_json::to_value(&sample.answers[ROOT]).unwrap()
            );
        }
    }
    let absent = Sample {
        answers: Arc::new(BTreeMap::new()),
        ..samples.into_iter().next().unwrap()
    };
    assert!(simulate(&[&absent], 2, Timing::Maximum).is_err());
    assert!(simulate(&[], 0, Timing::Maximum).is_err());
}

#[test]
fn simulation_later_demand_waits_for_busy_key_and_ignores_unused_answers() {
    use super::simulation::{Sample, Timing, preflight, simulate};
    use std::sync::Arc;

    let dir = tempfile::tempdir().unwrap();
    let settings = settings();
    let shared = child(ROOT, "5i5h").unwrap();
    let mut root_answer = reading(ROOT, &[("5i5h", 30)], &settings);
    root_answer.sec = 1.0;
    let mut shared_answer = reading(&shared, &[("5a5b", 30)], &settings);
    shared_answer.sec = 5.0;
    let mut params = definition(dir.path(), 0).params();
    params.depth = 0;
    params.extra.clear();
    params.own = "w".into();
    let mut unused_root = root_answer.clone();
    unused_root.sec = 100.0;
    let unused_key = child(ROOT, "5i4i").unwrap();
    let mut unused_answer = reading(&unused_key, &[("5a5b", 30)], &settings);
    unused_answer.sec = 1000.0;
    let mut first = Sample {
        params: params.clone(),
        lines: vec![(shared.clone(), 2, vec![])],
        answers: Arc::new(BTreeMap::from([
            (shared.clone(), shared_answer.clone()),
            (ROOT.into(), unused_root),
            (unused_key.clone(), unused_answer),
        ])),
        requested: BTreeSet::new(),
        fingerprint: "first".into(),
        priority: 100,
        order: 0,
    };
    params.depth = 1;
    shared_answer.sec = 2.0;
    let mut second = Sample {
        params,
        lines: vec![(ROOT.into(), 1, vec![])],
        answers: Arc::new(BTreeMap::from([
            (ROOT.into(), root_answer),
            (shared.clone(), shared_answer),
        ])),
        requested: BTreeSet::new(),
        fingerprint: "second".into(),
        priority: 100,
        order: 1,
    };
    preflight(&mut first).unwrap();
    preflight(&mut second).unwrap();
    assert_eq!(first.requested, BTreeSet::from([shared.clone()]));
    assert_eq!(second.requested, BTreeSet::from([ROOT.into(), shared.clone()]));
    for selected in [[&first, &second], [&second, &first]] {
        let result = simulate(&selected, 2, Timing::Maximum).unwrap();
        assert_eq!(result.elapsed, 5.0);
        assert_eq!(result.lower, 3.0);
        assert_eq!(result.count, 2);
        assert_eq!(result.mixed, 1);
        assert!((result.utilization.iter().sum::<f64>() / 20.0 - 0.6).abs() < 1e-9);
        for (tree, sample) in result.trees.iter().zip(selected) {
            assert_eq!(tree.nodes.keys().cloned().collect::<BTreeSet<_>>(), sample.requested);
            assert_eq!(tree.nodes[&shared].sec, sample.answers[&shared].sec);
        }
        let mean = simulate(&selected, 2, Timing::Mean).unwrap();
        assert_eq!(mean.elapsed, 3.0);
        assert_eq!(mean.lower, 3.0);
        assert_eq!(mean.count, 2);
    }
}

#[test]
fn top_group_keeps_the_last_full_group_from_multipv_one() {
    let mut group = super::service::TopGroup::new(3);
    let mut feed = |index: usize, mv: &str| {
        group.push(
            index,
            &Pv {
                mv: mv.into(),
                cp: 0,
                depth: 5,
            },
        )
    };
    feed(1, "5i4i");
    feed(2, "5i6i");
    feed(3, "5i5h");
    let moves = |group: &super::service::TopGroup| -> Vec<String> {
        group.complete.as_ref().unwrap().iter().map(|p| p.mv.clone()).collect()
    };
    assert_eq!(moves(&group), ["5i4i", "5i6i", "5i5h"]);
    // 1 位から続かない組は捨て、揃った前の組を保つ。
    let mut feed = |index: usize, mv: &str| {
        group.push(
            index,
            &Pv {
                mv: mv.into(),
                cp: 0,
                depth: 6,
            },
        )
    };
    feed(1, "5i6i");
    feed(3, "5i4i");
    feed(2, "5i5h");
    assert_eq!(moves(&group), ["5i4i", "5i6i", "5i5h"]);
}

#[test]
fn speculation_follows_tops_and_readings_within_depth_and_skips_tree_positions() {
    let s = settings();
    let dir = tempfile::tempdir().unwrap();
    let mut def = definition(dir.path(), 0);
    def.depth = 2;
    let mut tree = Tree::load(def, &s).unwrap();
    let mut cache = BTreeMap::new();
    tree.advance(&cache).unwrap();
    let a = child(ROOT, "5i4i").unwrap();
    let b = child(ROOT, "5i6h").unwrap();
    let c = child(ROOT, "5i5h").unwrap();
    let mut tops = BTreeMap::new();
    tops.insert(ROOT.to_owned(), (vec!["5i6h".to_owned(), "5i4i".to_owned()], 10));
    tops.insert(b.clone(), (vec!["5a5b".to_owned()], 400));
    tops.insert(a.clone(), (vec!["5a5b".to_owned()], 0));
    cache.insert(c.clone(), reading(&c, &[("5a4a", 0), ("5a5b", -10)], &s));
    let found: BTreeMap<_, _> = tree
        .speculate(&tops, &cache, 1000)
        .into_iter()
        .map(|c| (c.key, (c.level, c.ply)))
        .collect();
    // 根の途中の上位の手と実戦の手 (5i5h) の子。c は読み済みなので挙げずに、相手の手番の
    // opp_n (1) 手と実戦の手 (5a5b) で辿る。値が stop 以上の b は辿らない。a は途中の手と実戦の手で辿る。
    let mut expected = BTreeMap::from([(a.clone(), (1, 2)), (b.clone(), (1, 2))]);
    for (parent, mv) in [(&a, "5a5b"), (&a, "5a4a"), (&c, "5a4a"), (&c, "5a5b")] {
        expected.insert(child(parent, mv).unwrap(), (2, 3));
    }
    assert_eq!(found, expected);
    let limited = tree.speculate(&tops, &cache, 1);
    assert!(limited.iter().all(|c| c.level == 1) && limited.len() == 2, "浅い段から打ち切る");
    // 根を読み終えたら根の子は木の局面なので挙げない。
    cache.insert(ROOT.to_owned(), reading(ROOT, &[("5i4i", 10)], &s));
    assert!(tree.speculate(&tops, &cache, 1000).is_empty());
    tree.advance(&cache).unwrap();
    let after: BTreeSet<_> =
        tree.speculate(&tops, &cache, 1000).into_iter().map(|c| c.key).collect();
    assert!(after.iter().all(|key| !tree.level.contains_key(key)));
    assert!(after.contains(&child(&a, "5a5b").unwrap()));
}

#[test]
fn speculation_reaches_a_merged_position_at_its_shallowest_level() {
    let s = settings();
    let dir = tempfile::tempdir().unwrap();
    let mut tree = Tree::load(definition(dir.path(), 0), &s).unwrap();
    tree.advance(&BTreeMap::new()).unwrap();
    let path = |moves: &[&str]| moves.iter().fold(ROOT.to_owned(), |k, m| child(&k, m).unwrap());
    let shallow = path(&["5i4i"]);
    let deep = path(&["5i5h", "5a4a", "5h4i"]);
    tree.level.insert(shallow.clone(), 1);
    tree.ply.insert(shallow.clone(), 2);
    tree.level.insert(deep.clone(), 3);
    tree.ply.insert(deep.clone(), 4);
    let merged = child(&shallow, "5a5b").unwrap();
    assert_eq!(merged, child(&deep, "4a5b").unwrap());
    let tops = BTreeMap::from([
        (shallow.clone(), (vec!["5a5b".to_owned()], 0)),
        (deep.clone(), (vec!["4a5b".to_owned()], 0)),
        (merged.clone(), (vec!["4i3i".to_owned()], 0)),
    ]);
    let found: BTreeMap<_, _> = tree
        .speculate(&tops, &BTreeMap::new(), 1000)
        .into_iter()
        .map(|c| (c.key, (c.level, c.ply)))
        .collect();
    assert_eq!(found[&merged], (2, 3));
    assert_eq!(found[&child(&merged, "4i3i").unwrap()], (3, 4));
}

#[test]
fn serve_startup_rejects_speculate_options_without_their_prerequisites() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    std::fs::create_dir(&state).unwrap();
    let def = definition(dir.path(), 0);
    let out_root = dir.path().join("trees");
    let journal = dir.path().join("journal.jsonl");
    let mut args = serve_args(&state, &journal, &out_root, &def.paths);
    args.prefetch = true;
    args.speculate_priority = Some(0);
    let error = service::serve(args).unwrap_err();
    assert!(error.to_string().contains("speculate-priority"), "{error:#}");
    let mut args = serve_args(&state, &journal, &out_root, &def.paths);
    args.speculate_at = Some(0.25);
    let error = service::serve(args).unwrap_err();
    assert!(error.to_string().contains("speculate-at"), "{error:#}");
    assert!(!journal.exists());
}

fn import_file(path: &Path, settings: &Settings, readings: &[Reading]) {
    let mut journal = Journal::open(path, &settings.fingerprint).unwrap();
    for reading in readings {
        journal.append(reading.clone(), settings, "fixture".into()).unwrap();
    }
}

fn drain_import(
    inbox: &mut super::live_import::Inbox,
    journal: &mut Journal,
    settings: &Settings,
) -> Vec<String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut messages = Vec::new();
    loop {
        assert!(std::time::Instant::now() < deadline, "{}", inbox.status());
        if let Some(message) = inbox.poll(journal, settings).unwrap() {
            messages.push(message);
        }
        if matches!(inbox.status()["state"].as_str(), Some("done" | "failed")) {
            return messages;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

#[test]
fn live_import_appends_filters_duplicates_conflicts_and_satisfies_tree() {
    use super::journal::Readings;
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let mut inbox = super::live_import::Inbox::new(&state, Default::default()).unwrap();
    let settings = settings();
    let existing = reading(ROOT, &[("5i5h", 10)], &settings);
    let other_key = child(ROOT, "5i5h").unwrap();
    let added = reading(&other_key, &[], &settings);
    let mut conflict = existing.clone();
    conflict.multipv[0].cp += 1;
    let dst = dir.path().join("dst.jsonl");
    let mut journal = Journal::open(&dst, &settings.fingerprint).unwrap();
    journal.append(existing.clone(), &settings, "engine".into()).unwrap();
    let source = dir.path().join("source.jsonl");
    import_file(&source, &settings, &[conflict, added.clone()]);
    let same = dir.path().join("same.jsonl");
    import_file(&same, &settings, std::slice::from_ref(&existing));
    // 同じ値の重複と食い違いは別の受付ファイルで扱う。
    let first = state.join("import/01.jsonl");
    std::fs::copy(&same, &first).unwrap();
    assert!(drain_import(&mut inbox, &mut journal, &settings).is_empty());
    assert_eq!(inbox.status()["counts"]["skipped_same"], 1);
    let mut foreign = settings.clone();
    foreign.nodes += 1;
    foreign.fingerprint = foreign.computed_fingerprint().unwrap();
    let foreign_path = dir.path().join("foreign.jsonl");
    import_file(&foreign_path, &foreign, &[reading(ROOT, &[], &foreign)]);
    let mut combined = Vec::new();
    for line in std::fs::read_to_string(&source).unwrap().lines() {
        let mut record: serde_json::Value = serde_json::from_str(line).unwrap();
        record["prefetch"] = true.into();
        combined.extend(serde_json::to_vec(&record).unwrap());
        combined.push(b'\n');
    }
    combined.extend_from_within(..);
    combined.extend(std::fs::read(&foreign_path).unwrap());
    std::fs::write(state.join("import/02.jsonl"), combined).unwrap();
    let tree_dir = dir.path().join("tree");
    std::fs::create_dir(&tree_dir).unwrap();
    let mut def = definition(&tree_dir, 0);
    def.depth = 0;
    def.extra.clear();
    std::fs::write(&def.paths, format!("{other_key}\t2\t\n")).unwrap();
    let mut tree = Tree::load(def, &settings).unwrap();
    let missing = tree.advance(&journal).unwrap();
    assert!(missing.iter().any(|(key, _)| key == &other_key));
    assert!(drain_import(&mut inbox, &mut journal, &settings).is_empty());
    assert_eq!(inbox.status()["counts"]["appended"], 1);
    assert_eq!(inbox.status()["counts"]["conflicts"], 2);
    assert_eq!(inbox.status()["counts"]["skipped_same"], 1);
    assert_eq!(inbox.status()["counts"]["skipped_condition"], 1);
    assert!(state.join("import/done/02.jsonl").exists());
    assert_eq!(journal.get(ROOT), Some(&existing));
    assert_eq!(journal.get(&other_key), Some(&added));
    assert!(journal.prefetched.contains(&other_key));
    assert!(!journal.prefetched.contains(ROOT));
    let tsv = std::fs::read_to_string(state.join("import/done/02.jsonl.conflicts.tsv")).unwrap();
    assert_eq!(tsv.lines().count(), 3);
    assert_eq!(tsv.lines().nth(1), tsv.lines().nth(2));
    assert!(tsv.contains("existing_bestmove"));
    tree.advance(&journal).unwrap();
    assert!(tree.finished);
    let size = std::fs::metadata(&dst).unwrap().len();
    let mut late = added.clone();
    late.multipv[0].cp += 50;
    let mut alerts = Vec::new();
    assert!(
        !inbox
            .complete_worker(&mut journal, &settings, late, true, |m| alerts.push(m.to_owned()))
            .unwrap()
    );
    assert_eq!(alerts.len(), 1);
    assert!(alerts[0].contains(&other_key));
    assert!(
        !inbox
            .complete_worker(&mut journal, &settings, added.clone(), true, |_| panic!("同じ値"))
            .unwrap()
    );
    assert_eq!(inbox.status()["discarded_worker"], 2);
    assert_eq!(std::fs::metadata(&dst).unwrap().len(), size);
    std::fs::copy(state.join("import/done/02.jsonl"), state.join("import/03.jsonl")).unwrap();
    drain_import(&mut inbox, &mut journal, &settings);
    assert_eq!(inbox.status()["counts"]["appended"], 0);
    assert_eq!(std::fs::metadata(&dst).unwrap().len(), size);
}

#[test]
fn live_import_rejects_whole_invalid_file_and_ignores_unpublished_files() {
    let dir = tempfile::tempdir().unwrap();
    let settings = settings();
    let mut inbox = super::live_import::Inbox::new(dir.path(), Default::default()).unwrap();
    let dst = dir.path().join("journal.jsonl");
    let mut journal = Journal::open(&dst, &settings.fingerprint).unwrap();
    let good = dir.path().join("good.jsonl");
    import_file(&good, &settings, &[reading(ROOT, &[], &settings)]);
    let mut bad = std::fs::read(&good).unwrap();
    bad.extend(b"not json\n");
    std::fs::write(dir.path().join("import/01.jsonl"), bad).unwrap();
    std::fs::write(dir.path().join("import/writing.tmp"), "not json").unwrap();
    let messages = drain_import(&mut inbox, &mut journal, &settings);
    assert_eq!(messages.len(), 1);
    assert!(messages[0].contains("取り込み失敗"));
    assert!(dir.path().join("import/failed/01.jsonl").exists());
    assert_eq!(std::fs::metadata(&dst).unwrap().len(), 0);
    std::fs::copy(&good, dir.path().join("import/02.jsonl")).unwrap();
    assert!(drain_import(&mut inbox, &mut journal, &settings).is_empty());
    inbox.poll(&mut journal, &settings).unwrap();
    assert!(dir.path().join("import/writing.tmp").exists());
    assert!(!dir.path().join("import/failed/writing.tmp").exists());
}

#[test]
fn live_import_restarts_after_partial_append_without_duplicate_records() {
    use super::journal::Readings;
    let dir = tempfile::tempdir().unwrap();
    let settings = settings();
    let mut inbox = super::live_import::Inbox::new(dir.path(), Default::default()).unwrap();
    let source = dir.path().join("import/01.jsonl");
    let row = |col| match col {
        0 => "K8".to_owned(),
        8 => "8K".to_owned(),
        n => format!("{n}K{}", 8 - n),
    };
    let mut readings = Vec::new();
    for white in 0..9 {
        for black in 0..9 {
            let key = format!("{}/9/9/9/9/9/9/9/{} b -", row(white).to_lowercase(), row(black));
            readings.push(reading(&key, &[], &settings));
        }
    }
    import_file(&source, &settings, &readings);
    let dst = dir.path().join("journal.jsonl");
    let mut journal = Journal::open(&dst, &settings.fingerprint).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        assert!(std::time::Instant::now() < deadline);
        inbox.poll_short(&mut journal, &settings).unwrap();
        if inbox.status()["state"] == "appending" {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(inbox.status()["counts"]["appended"].as_u64().unwrap() < 81);
    assert!(source.exists());
    drop(inbox);
    drop(journal);
    // 周回の途中で、完全な行と次の行の一部だけが永続化された状態を再現する。
    let persisted = std::fs::read_to_string(&dst).unwrap().lines().count();
    let source_bytes = std::fs::read_to_string(&source).unwrap();
    let rows: Vec<_> = source_bytes.split_inclusive('\n').collect();
    {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new().append(true).open(&dst).unwrap();
        file.write_all(rows[persisted].as_bytes()).unwrap();
        let tail = rows[persisted + 1].as_bytes();
        file.write_all(&tail[..tail.len() / 2]).unwrap();
        file.sync_data().unwrap();
    }
    let mut journal = Journal::open(&dst, &settings.fingerprint).unwrap();
    let mut inbox = super::live_import::Inbox::new(dir.path(), Default::default()).unwrap();
    drain_import(&mut inbox, &mut journal, &settings);
    for reading in &readings {
        assert_eq!(journal.get(&reading.key), Some(reading));
    }
    assert_eq!(std::fs::read_to_string(&dst).unwrap().lines().count(), 81);
}

#[test]
fn live_import_rejects_source_conflicts_and_invalid_fingerprints_before_appending() {
    let dir = tempfile::tempdir().unwrap();
    let settings = settings();
    let good = dir.path().join("good.jsonl");
    let original = reading(ROOT, &[], &settings);
    import_file(&good, &settings, std::slice::from_ref(&original));
    let first = std::fs::read(&good).unwrap();
    let mut rec: serde_json::Value = serde_json::from_slice(&first).unwrap();
    rec["reading"]["multipv"][0]["cp"] = 12345.into();
    let mut conflict = first.clone();
    conflict.extend(serde_json::to_vec(&rec).unwrap());
    conflict.push(b'\n');
    let mut forged: serde_json::Value = serde_json::from_slice(&first).unwrap();
    forged["settings"]["fingerprint"] = "forged".into();
    let mut forged = serde_json::to_vec(&forged).unwrap();
    forged.push(b'\n');
    for (i, bytes) in [conflict, forged].into_iter().enumerate() {
        let state = dir.path().join(i.to_string());
        let mut inbox = super::live_import::Inbox::new(&state, Default::default()).unwrap();
        let dst = state.join("journal.jsonl");
        let mut journal = Journal::open(&dst, &settings.fingerprint).unwrap();
        std::fs::write(state.join("import/01.jsonl"), bytes).unwrap();
        assert_eq!(drain_import(&mut inbox, &mut journal, &settings).len(), 1);
        assert_eq!(std::fs::metadata(&dst).unwrap().len(), 0);
    }
}

#[test]
fn live_import_checks_other_binaries_by_source_version() {
    let dir = tempfile::tempdir().unwrap();
    let mut dst = settings();
    dst.engine_version = Some("stable-source".into());
    let mut src = dst.clone();
    src.engine_sha256 = "other-cpu-build".into();
    src.fingerprint = src.computed_fingerprint().unwrap();
    let path = dir.path().join("compatible.jsonl");
    import_file(&path, &src, &[reading(ROOT, &[], &src)]);
    let staged = super::journal::stage_live_source(&path, &dst, &Default::default()).unwrap();
    assert!(staged.file.metadata().unwrap().len() > 0);
    assert_eq!(staged.skipped_condition, 0);
    src.engine_version = Some("different-source".into());
    let path = dir.path().join("incompatible.jsonl");
    import_file(&path, &src, &[reading(ROOT, &[], &src)]);
    assert!(super::journal::stage_live_source(&path, &dst, &Default::default()).is_err());
}

#[test]
fn live_import_merges_known_version_before_checking_compatibility() {
    let dir = tempfile::tempdir().unwrap();
    let mut dst = settings();
    dst.engine_version = Some("stable-source".into());
    let mut src = dst.clone();
    src.engine_sha256 = "other-cpu-build".into();
    src.fingerprint = src.computed_fingerprint().unwrap();
    let path = dir.path().join("source.jsonl");
    import_file(&path, &src, &[reading(ROOT, &[], &src)]);
    let known = std::fs::read_to_string(&path).unwrap();
    let mut old: serde_json::Value = serde_json::from_str(&known).unwrap();
    old["settings"]["engine_version"] = serde_json::Value::Null;
    let old = format!("{old}\n");
    for bytes in [format!("{old}{known}"), format!("{known}{old}")] {
        std::fs::write(&path, bytes).unwrap();
        let staged = super::journal::stage_live_source(&path, &dst, &Default::default()).unwrap();
        assert_eq!(staged.skipped_condition, 0);
        assert_eq!(std::io::BufRead::lines(std::io::BufReader::new(staged.file)).count(), 2);
    }
}

#[test]
fn live_import_batch_keeps_first_duplicate_metadata() {
    use super::journal::Readings;
    let dir = tempfile::tempdir().unwrap();
    let settings = settings();
    let mut inbox = super::live_import::Inbox::new(dir.path(), Default::default()).unwrap();
    let first = reading(ROOT, &[], &settings);
    let source = dir.path().join("source.jsonl");
    import_file(&source, &settings, std::slice::from_ref(&first));
    let bytes = std::fs::read_to_string(source).unwrap();
    let mut later: serde_json::Value = serde_json::from_str(&bytes).unwrap();
    later["reading"]["sec"] = 99.into();
    later["reading"]["ply"] = 20.into();
    later["prefetch"] = true.into();
    std::fs::write(dir.path().join("import/duplicates.jsonl"), format!("{bytes}{later}\n"))
        .unwrap();
    let mut journal =
        Journal::open(&dir.path().join("journal.jsonl"), &settings.fingerprint).unwrap();
    drain_import(&mut inbox, &mut journal, &settings);
    assert_eq!(journal.get(ROOT), Some(&first));
    assert!(journal.prefetched.contains(ROOT));
    assert_eq!(inbox.status()["counts"]["appended"], 1);
    assert_eq!(inbox.status()["counts"]["skipped_same"], 1);
}

#[test]
fn live_import_preserves_prefetch_from_duplicate_in_later_chunk() {
    let dir = tempfile::tempdir().unwrap();
    let settings = settings();
    let mut inbox = super::live_import::Inbox::new(dir.path(), Default::default()).unwrap();
    let first = reading(ROOT, &[], &settings);
    let source = dir.path().join("source.jsonl");
    import_file(&source, &settings, std::slice::from_ref(&first));
    let bytes = std::fs::read_to_string(source).unwrap();
    let mut later: serde_json::Value = serde_json::from_str(&bytes).unwrap();
    later["prefetch"] = true.into();
    let incoming = format!("{}{later}\n", bytes.repeat(1024));
    std::fs::write(dir.path().join("import/duplicates.jsonl"), incoming).unwrap();
    let path = dir.path().join("journal.jsonl");
    let mut journal = Journal::open(&path, &settings.fingerprint).unwrap();
    assert!(drain_import(&mut inbox, &mut journal, &settings).is_empty());
    assert!(journal.prefetched.contains(ROOT));
    assert_eq!(inbox.status()["counts"]["appended"], 1);
    assert_eq!(inbox.status()["counts"]["skipped_same"], 1024);
    drop(journal);
    let journal = Journal::open(&path, &settings.fingerprint).unwrap();
    assert!(journal.prefetched.contains(ROOT));
    assert_eq!(std::fs::read_to_string(path).unwrap().lines().count(), 1);
}

#[test]
fn accepted_fingerprints_are_repeatable_and_validated_on_both_commands() {
    use clap::Parser;
    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        tree: TreeArgs,
    }
    let fp = "0123456789abcdef".repeat(4);
    let other = "f".repeat(64);
    for command in ["import-journal", "serve"] {
        let mut base = vec!["test", command, "--engine", "engine", "--nodes", "100"];
        if command == "serve" {
            base.extend(["--state", "state", "--out-root", "out"]);
        } else {
            base.extend(["--from-journal", "source", "--journal", "destination"]);
        }
        let mut valid = base.clone();
        valid.extend(["--accept-fingerprint", &fp, "--accept-fingerprint", &other]);
        let parsed = Cli::try_parse_from(valid).unwrap();
        if let TreeCommand::Serve(args) = parsed.tree.command {
            assert_eq!(args.accept_fingerprint, [fp.clone(), other.clone()]);
        }
        for invalid in [
            "".into(),
            "a".repeat(63),
            "a".repeat(65),
            "A".repeat(64),
            "g".repeat(64),
        ] {
            let mut args = base.clone();
            args.extend(["--accept-fingerprint", &invalid]);
            let err = Cli::try_parse_from(args).err().unwrap().to_string();
            assert!(err.contains("64 桁の小文字の 16 進数"), "{err}");
        }
    }
}

#[test]
fn live_import_accepts_only_listed_foreign_net_and_keeps_destination_settings() {
    use super::journal::Readings;
    let dir = tempfile::tempdir().unwrap();
    let dst = settings();
    let mut src = dst.clone();
    src.content_sha256.insert("EvalFile".into(), "a".repeat(64));
    src.fingerprint = src.computed_fingerprint().unwrap();
    assert_ne!(src.fingerprint, dst.fingerprint);
    let mut unlisted = src.clone();
    unlisted.content_sha256.insert("EvalFile".into(), "b".repeat(64));
    unlisted.fingerprint = unlisted.computed_fingerprint().unwrap();
    let other_key = child(ROOT, "5i5h").unwrap();
    for accept in [false, true] {
        let state = dir.path().join(accept.to_string());
        let accepted = if accept {
            BTreeSet::from([src.fingerprint.clone()])
        } else {
            BTreeSet::new()
        };
        let mut inbox = super::live_import::Inbox::new(&state, accepted).unwrap();
        let path = state.join("journal.jsonl");
        let mut journal = Journal::open(&path, &dst.fingerprint).unwrap();
        let source = state.join("import/source.jsonl");
        import_file(&source, &src, &[reading(ROOT, &[], &src)]);
        import_file(&source, &unlisted, &[reading(&other_key, &[], &unlisted)]);
        assert!(drain_import(&mut inbox, &mut journal, &dst).is_empty());
        assert_eq!(inbox.status()["counts"]["appended"], usize::from(accept));
        assert_eq!(inbox.status()["counts"]["skipped_condition"], if accept { 1 } else { 2 });
        assert_eq!(journal.get(ROOT).is_some(), accept);
        assert!(journal.get(&other_key).is_none());
        let text = std::fs::read_to_string(&path).unwrap();
        if accept {
            let record: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
            assert_eq!(record["settings"]["fingerprint"], dst.fingerprint);
            assert_eq!(record["settings"], serde_json::to_value(&dst).unwrap());
            let provenance = record["provenance"].as_str().unwrap();
            assert!(provenance.contains(&format!(
                "fingerprint={}:force=false:accepted=true:path=",
                src.fingerprint
            )));
        } else {
            assert!(text.is_empty());
        }
    }
}

#[test]
fn live_import_rejects_accepted_condition_mismatches_before_any_append_and_continues() {
    use super::journal::Readings;
    let dst = settings();
    for field in [
        "nodes",
        "hash",
        "multipv",
        "DrawValueBlack",
        "DrawValueWhite",
        "option",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut src = dst.clone();
        match field {
            "nodes" => src.nodes += 1,
            "hash" => {
                src.hash += 1;
                src.options.insert("USI_Hash".into(), src.hash.to_string());
            }
            "multipv" => {
                src.multipv -= 1;
                src.options.insert("MultiPV".into(), src.multipv.to_string());
            }
            "option" => {
                src.options.insert("SlowMover".into(), "50".into());
            }
            key => {
                src.options.insert(key.into(), "0".into());
            }
        }
        src.fingerprint = src.computed_fingerprint().unwrap();
        let mut inbox =
            super::live_import::Inbox::new(dir.path(), BTreeSet::from([src.fingerprint.clone()]))
                .unwrap();
        let path = dir.path().join("journal.jsonl");
        let mut journal = Journal::open(&path, &dst.fingerprint).unwrap();
        let existing = reading(ROOT, &[], &dst);
        journal.append(existing, &dst, "fixture".into()).unwrap();
        let before = std::fs::read(&path).unwrap();
        let other_key = child(ROOT, "5i5h").unwrap();
        let source = dir.path().join("import/01.jsonl");
        import_file(&source, &dst, &[reading(&other_key, &[], &dst)]);
        let mut incoming = reading(ROOT, &[], &src);
        incoming.multipv.truncate(src.multipv);
        import_file(&source, &src, &[incoming]);
        let messages = drain_import(&mut inbox, &mut journal, &dst);
        assert_eq!(messages.len(), 1, "{field}");
        assert!(messages[0].contains("取り込み失敗"), "{messages:?}");
        assert!(messages[0].contains(field), "{messages:?}");
        assert!(dir.path().join("import/failed/01.jsonl").exists());
        assert_eq!(std::fs::read(&path).unwrap(), before, "{field}");
        import_file(&dir.path().join("import/02.jsonl"), &dst, &[reading(&other_key, &[], &dst)]);
        assert!(drain_import(&mut inbox, &mut journal, &dst).is_empty());
        assert!(dir.path().join("import/done/02.jsonl").exists());
        assert!(journal.get(&other_key).is_some());
    }
}
