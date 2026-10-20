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
        let values = tree.negamax(&tree.reach(&tree.edge)).unwrap();
        let mut added = false;
        for (key, un) in tree.unresolved(&values) {
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
    let references: Vec<_> = (0..=2)
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

fn reference_values(tree: &Tree) -> super::model::Values {
    fn evaluate(tree: &Tree, key: &str, path: Vec<String>) -> BTreeMap<String, i32> {
        let mut rows: BTreeMap<_, _> =
            tree.nodes[key].multipv.iter().map(|p| (p.mv.clone(), p.cp)).collect();
        let moves: BTreeSet<_> = rows
            .keys()
            .cloned()
            .chain(tree.edge.get(key).into_iter().flatten().cloned())
            .collect();
        for mv in moves {
            let c = child(key, &mv).unwrap();
            if !tree.nodes.contains_key(&c) {
                continue;
            }
            let value = if c == key || path.contains(&c) {
                tree.def.draw
            } else {
                let mut ancestors = path.clone();
                ancestors.push(key.into());
                -evaluate(tree, &c, ancestors).values().max().copied().unwrap_or(0)
            };
            rows.insert(mv, value);
        }
        rows
    }
    tree.nodes
        .keys()
        .map(|key| (key.clone(), evaluate(tree, key, Vec::new())))
        .collect()
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
