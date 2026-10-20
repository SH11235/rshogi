use super::journal::{Pv, Reading};
use super::model::{child, side};
use super::*;
use rshogi_core::movegen::{MoveList, generate_legal_all};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

const ROOT: &str = "4k4/9/9/9/9/9/9/9/4K4 b -";

fn settings() -> Settings {
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
fn reading(key: &str, rows: &[(&str, i32)], settings: &Settings) -> Reading {
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
fn import_is_transactional_and_rejects_conflicting_declarations_and_values() {
    let dir = tempfile::tempdir().unwrap();
    let s = settings();
    let input = dir.path().join("d0.jsonl");
    let path = dir.path().join("journal");
    let r = reading(ROOT, &[("5i4i", 10)], &s);
    let mut j = Journal::open(&path).unwrap();
    assert!(Journal::open(&path).is_err());
    for field in ["nodes_limit", "threads", "hash", "draw", "multipv"] {
        let mut v = serde_json::to_value(&r).unwrap();
        v[field] = match field {
            "draw" => serde_json::json!({}),
            "multipv" => serde_json::json!([]),
            _ => serde_json::json!(999),
        };
        std::fs::write(&input, format!("{}\n{v}\n", serde_json::to_string(&r).unwrap())).unwrap();
        assert!(j.import(std::slice::from_ref(&input), &s).is_err());
        assert!(j.records_for(&s.fingerprint).is_empty());
    }
    std::fs::write(&input, format!("{}\n", serde_json::to_string(&r).unwrap())).unwrap();
    j.import(std::slice::from_ref(&input), &s).unwrap();
    let mut other = r.clone();
    other.multipv[0].cp += 1;
    std::fs::write(&input, serde_json::to_string(&other).unwrap()).unwrap();
    assert!(j.import(&[input], &s).is_err());
    drop(j);
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"{partial").unwrap();
    }
    let j = Journal::open(&path).unwrap();
    assert_eq!(j.records_for(&s.fingerprint).len(), 1);
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
    std::fs::write(def.out.join("own_nodes.tsv"), "stale").unwrap();
    let tree2 = Tree::load(def, &s).unwrap();
    assert!(!tree2.def.out.join("own_nodes.tsv").exists());
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
    let mut j = Journal::open(&journal_path).unwrap();
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
                tree.def.draw
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
fn explicit_model_is_required_and_invalid_paths_clear_stale_candidates() {
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
    assert!(!def.out.join("own_nodes.tsv").exists());
    std::fs::write(def.out.join("own_nodes.tsv"), "stale").unwrap();
    def.paths_sha256 = None;
    def.paths = dir.path().join("absent");
    assert!(Tree::load(def.clone(), &s).is_err());
    assert!(!def.out.join("own_nodes.tsv").exists());
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
    std::fs::create_dir(tree.def.out.join("STATUS")).unwrap();
    assert!(tree.write_outputs().unwrap());
    assert!(tree.def.out.join("own_nodes.tsv").exists());
}

#[test]
fn short_multipv_and_terminal_readings_are_leaves_but_import_requires_declaration() {
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
        assert!(r.validate_declaration(&s).is_err());
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
    let mut journal = Journal::open(&journal_path).unwrap();
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
    std::fs::create_dir(tree.def.out.join("tree_bp_min.db")).unwrap();
    assert!(tree.write_outputs().unwrap());
    let report = std::fs::read_to_string(tree.def.out.join("report.txt")).unwrap();
    assert!(report.contains("book_backprop (min、参考) が失敗した:"));
    assert!(report.starts_with(&format!("根 {ROOT} 候補 ['5i5h', '5i4i', '5i6i'] 比べる手")));
    assert!(tree.def.out.join("own_nodes.tsv").exists());
    assert_eq!(std::fs::read_to_string(tree.def.out.join("STATUS")).unwrap(), "DONE\n");
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

#[test]
fn serve_startup_rejects_accepted_collisions_before_changing_outputs() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    std::fs::create_dir(&state).unwrap();
    let def = definition(dir.path(), 0);
    std::fs::create_dir(&def.out).unwrap();
    let journal = def.out.join("d0.jsonl");
    std::fs::write(&journal, "protected").unwrap();
    std::fs::write(def.out.join("own_nodes.tsv"), "protected").unwrap();
    let toml = dir.path().join("tree.toml");
    std::fs::write(
        &toml,
        "paths='paths.tsv'\nout='out'\ndepth=0\nopp_n=1\nstop=300\nown='b'\nrounds=0\n",
    )
    .unwrap();
    let reading = ReadingArgs {
        engine: def.paths.clone(),
        engine_options: vec![format!("EvalFile={}", def.paths.display())],
        nodes: 100,
        multipv: 3,
        hash: 16,
    };
    let settings = Settings::new(&reading).unwrap();
    std::fs::write(state.join("accepted.json"), serde_json::to_vec(&serde_json::json!([{
        "out": def.out, "path": toml, "sha256": journal::digest(&toml).unwrap(),
        "paths_sha256": journal::digest(&def.paths).unwrap(), "fingerprint": settings.fingerprint
    }])).unwrap()).unwrap();
    let error = service::serve(ServeArgs {
        state,
        journal: Some(journal.clone()),
        reading,
        workers: 0,
        stall_secs: 1,
        max_restarts: 0,
    })
    .unwrap_err();
    assert!(error.to_string().contains("衝突"));
    assert_eq!(std::fs::read_to_string(journal).unwrap(), "protected");
    assert_eq!(std::fs::read_to_string(def.out.join("own_nodes.tsv")).unwrap(), "protected");
    assert!(!def.out.join("tree.lock").exists());
}

#[cfg(unix)]
#[test]
fn managed_paths_reject_live_and_dangling_symlinks() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let def = definition(dir.path(), 0);
    let journal = dir.path().join("journal");
    let state = dir.path().join("state");
    std::fs::create_dir_all(state.join("inbox")).unwrap();
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
        for path in [&def.out, &state.join("inbox"), &state] {
            std::fs::remove_dir_all(path).unwrap();
            symlink(&target, path).unwrap();
            assert!(super::paths::reject_collisions(&journal, Some(&state), &[&def]).is_err());
            std::fs::remove_file(path).unwrap();
            std::fs::create_dir_all(path).unwrap();
        }
        std::fs::create_dir_all(state.join("inbox")).unwrap();
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
