use super::cut::{CutArgs, MasterExit, MasterStop, cut_root, select};
use super::value::{
    RootValues, ValueArgs, ValueInput, append_records, collect_nodes, drop_own_records, join_roots,
    register_nodes, value_root,
};
use super::*;
use crate::book_backprop::DrawValues;
use crate::book_db::{BookDb, BookMove, PositionEntry};
use crate::book_tree::ReadingArgs;
use crate::book_tree::audit::MasterValue;
use crate::book_tree::journal::{Journal, Pv, Reading, Settings};
use crate::book_tree::master_table::{COLUMNS, parse_master};
use crate::book_tree::model::child;
use crate::book_tree::targets::{DEFAULT_PRIORITY, Defaults, Line};
use rshogi_book::BookEntryMove;
use std::collections::BTreeSet;
use std::path::PathBuf;

const ROOT: &str = "4k4/9/9/9/9/9/9/9/4K4 b -";

fn key(moves: &[&str]) -> String {
    moves.iter().fold(ROOT.to_owned(), |k, m| child(&k, m).unwrap())
}

/// 小さい定跡。ROOT から 5i5h (A)・5i4i (B)。A の 5a5b (AB) と B の 5a4a (E) を伸ばし、
/// AB → 5h4i → 5b4a で E に合流する。
fn book() -> BTreeMap<String, Vec<(&'static str, i32)>> {
    BTreeMap::from([
        (key(&[]), vec![("5i5h", 50), ("5i4i", 40), ("5i6i", 10)]),
        (key(&["5i5h"]), vec![("5a5b", -50), ("5a4a", -60), ("5a6a", -200)]),
        (key(&["5i4i"]), vec![("5a4a", -40)]),
        (key(&["5i5h", "5a5b"]), vec![("5h4i", 30)]),
        (key(&["5i5h", "5a5b", "5h4i"]), vec![("5b4a", -30)]),
        (key(&["5i4i", "5a4a"]), vec![("4i5i", 0)]),
    ])
}

fn lookup(sfen: &str) -> Result<Vec<BookEntryMove>> {
    Ok(book()
        .get(crate::book_db::strip_ply(sfen))
        .into_iter()
        .flatten()
        .map(|(m, v)| BookEntryMove {
            move_usi: (*m).into(),
            value: *v,
            depth: 10,
            move_count: 1,
        })
        .collect())
}

fn params(max_plies: usize, max_positions: usize) -> CutParams {
    CutParams {
        own_diff: 30,
        opp_diff: 20,
        opp_top: 1,
        max_plies,
        max_positions,
        check_every: 1,
        master_exit: None,
    }
}

fn root() -> RootInfo {
    RootInfo {
        id: "r1".into(),
        own: "b".into(),
        key: ROOT.into(),
        ply: 1,
    }
}

fn kinds(tree: &Subtree) -> BTreeMap<String, (usize, Kind, bool)> {
    tree.nodes.iter().map(|(k, n)| (k.clone(), (n.level, n.kind, n.read))).collect()
}

#[test]
fn select_keeps_close_own_moves_and_top_or_close_opponent_moves() {
    let p = params(4, 100);
    let moves: Vec<(String, i32)> = [("a", 10), ("b", 50), ("c", 20), ("d", 50), ("e", -100)]
        .map(|(m, v)| (m.into(), v))
        .to_vec();
    assert_eq!(select(&moves, true, &p), [1, 3, 2]);
    let opp = CutParams {
        opp_diff: 35,
        opp_top: 3,
        ..p.clone()
    };
    // 上位 3 手 (b・d・c) と差 35 以内 (b・d・c) の和。
    assert_eq!(select(&moves, false, &opp), [1, 3, 2]);
    let opp = CutParams {
        opp_diff: 1000,
        opp_top: 0,
        ..p.clone()
    };
    assert_eq!(select(&moves, false, &opp), [1, 3, 2, 0, 4]);
    assert!(select(&[], true, &p).is_empty());
}

#[test]
fn cut_selects_moves_merges_positions_and_marks_exits() {
    let empty = BTreeMap::new();
    let tree = cut_root(&root(), &params(3, 100), &mut lookup, None, &empty).unwrap();
    let k = kinds(&tree);
    assert_eq!(k[&key(&[])], (0, Kind::Inner, true));
    assert_eq!(k[&key(&["5i5h"])], (1, Kind::Inner, false));
    assert_eq!(k[&key(&["5i5h", "5a5b"])], (2, Kind::Inner, true));
    // 相手の手番は上位 1 手と差 20 以内。5a6a (差 150) は伸ばさない。
    assert_eq!(k[&key(&["5i5h", "5a4a"])], (2, Kind::Nobook, true));
    assert!(!k.contains_key(&key(&["5i5h", "5a6a"])));
    // 自分の手番は差 30 以内。5i6i (差 40) は伸ばさない。
    assert!(!k.contains_key(&key(&["5i6i"])));
    assert_eq!(k[&key(&["5i4i", "5a4a"])], (2, Kind::Inner, true));
    assert_eq!(k[&key(&["5i5h", "5a5b", "5h4i"])], (3, Kind::Depth, true));
    assert_eq!(k[&key(&["5i4i", "5a4a", "4i5i"])], (3, Kind::Nobook, true));
    assert_eq!(tree.nodes.len(), 8);

    // 1 段深くすると AB → 5h4i → 5b4a が既にある E (段 2) に合流し、局面は増えない。
    let deeper = cut_root(&root(), &params(4, 100), &mut lookup, None, &empty).unwrap();
    assert_eq!(deeper.nodes.len(), 8);
    let ab4 = key(&["5i5h", "5a5b", "5h4i"]);
    assert_eq!(deeper.nodes[&ab4].kind, Kind::Inner);
    let e = key(&["5i4i", "5a4a"]);
    assert!(deeper.edges.iter().any(|x| x.key == ab4 && x.mv == "5b4a" && x.child == e));
    assert_eq!(deeper.nodes[&e].level, 2);

    // 局面数の上限: 根の子 2 局面で 3。A・B を伸ばすと超えるので出口。
    let capped = cut_root(&root(), &params(4, 3), &mut lookup, None, &empty).unwrap();
    let k = kinds(&capped);
    assert_eq!(k.len(), 3);
    assert_eq!(k[&key(&["5i5h"])], (1, Kind::Cap, true));
    assert_eq!(k[&key(&["5i4i"])], (1, Kind::Cap, true));
}

#[test]
fn master_positions_stop_and_use_or_read_master_values() {
    let e = key(&["5i4i", "5a4a"]);
    let empty = BTreeMap::new();
    let values = BTreeMap::from([
        (
            ROOT.to_owned(),
            MasterValue {
                main: Some(1),
                best: Some(1),
            },
        ),
        (
            e.clone(),
            MasterValue {
                main: Some(12),
                best: Some(30),
            },
        ),
    ]);
    let stop = MasterStop {
        values: &values,
        mode: MasterExit::Use,
    };
    let tree = cut_root(&root(), &params(4, 100), &mut lookup, Some(&stop), &empty).unwrap();
    // 根は元データにあっても伸ばす。
    assert_eq!(tree.nodes[ROOT].kind, Kind::Inner);
    assert_eq!((tree.nodes[&e].kind, tree.nodes[&e].read), (Kind::Master, false));
    assert!(!tree.nodes.contains_key(&key(&["5i4i", "5a4a", "4i5i"])));

    let read = MasterStop {
        values: &values,
        mode: MasterExit::Read,
    };
    let tree = cut_root(&root(), &params(4, 100), &mut lookup, Some(&read), &empty).unwrap();
    assert_eq!((tree.nodes[&e].kind, tree.nodes[&e].read), (Kind::Master, true));

    // 自分の手番で main の値が分からなければ読む。
    let unknown = BTreeMap::from([(
        e.clone(),
        MasterValue {
            main: None,
            best: Some(30),
        },
    )]);
    let stop = MasterStop {
        values: &unknown,
        mode: MasterExit::Use,
    };
    let tree = cut_root(&root(), &params(4, 100), &mut lookup, Some(&stop), &empty).unwrap();
    assert!(tree.nodes[&e].read);
}

#[test]
fn extra_moves_are_added_only_on_own_inner_positions() {
    let ab = key(&["5i5h", "5a5b"]);
    let a = key(&["5i5h"]);
    let extras = BTreeMap::from([
        (ab.clone(), vec!["5h6h".to_owned(), "5h4i".to_owned(), "9i9a".to_owned()]),
        (a.clone(), vec!["5a6a".to_owned()]),
    ]);
    let tree = cut_root(&root(), &params(4, 100), &mut lookup, None, &extras).unwrap();
    let added: Vec<_> = tree.edges.iter().filter(|e| e.extra).collect();
    assert_eq!(added.len(), 1);
    assert_eq!(
        (added[0].key.as_str(), added[0].mv.as_str(), added[0].book_value),
        (ab.as_str(), "5h6h", None)
    );
    assert_eq!(tree.nodes[&added[0].child].kind, Kind::Nobook);
    assert!(!tree.edges.iter().any(|e| e.key == a && e.mv == "5a6a"));
}

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

fn reading(key: &str, rows: &[(&str, i32)], s: &Settings) -> Reading {
    let multipv: Vec<Pv> = rows
        .iter()
        .map(|(m, cp)| Pv {
            mv: (*m).into(),
            cp: *cp,
            depth: 8,
        })
        .collect();
    Reading {
        tag: "t".into(),
        ply: 1,
        key: key.into(),
        nodes_limit: s.nodes,
        nodes: Some(101),
        sec: 0.1,
        threads: 1,
        hash: s.hash,
        draw: s.draw(),
        bestmove: multipv.first().map_or_else(|| "resign".to_owned(), |p| p.mv.clone()),
        multipv,
    }
}

/// 段 4 で切った部分木の読む局面すべての読み。
fn readings(s: &Settings, root_rows: &[(&'static str, i32)]) -> BTreeMap<String, Reading> {
    let rows: Vec<(String, Vec<(&str, i32)>)> = vec![
        (key(&[]), root_rows.to_vec()),
        (key(&["5i5h", "5a5b"]), vec![("5h4i", 5)]),
        (key(&["5i5h", "5a4a"]), vec![("5h4h", 100), ("5h6h", 90)]),
        (key(&["5i4i", "5a4a"]), vec![("4i5i", 0)]),
        (key(&["5i4i", "5a4a", "4i5i"]), vec![("4a3a", -20)]),
    ];
    rows.into_iter().map(|(k, r)| (k.clone(), reading(&k, &r, s))).collect()
}

#[test]
fn value_root_propagates_exit_readings_and_marks_better_outside_moves() {
    let s = settings();
    let empty = BTreeMap::new();
    let tree = cut_root(&root(), &params(4, 100), &mut lookup, None, &empty).unwrap();
    let wanted: BTreeSet<&String> =
        tree.nodes.iter().filter(|(_, n)| n.read).map(|(k, _)| k).collect();
    let cache = readings(&s, &[("5i6i", 80), ("5i5h", 10)]);
    assert_eq!(wanted, cache.keys().collect());
    let work = tempfile::tempdir().unwrap();
    let draws = DrawValues {
        black: -70,
        white: 70,
    };
    let values = value_root(
        &root(),
        &tree,
        &ValueInput {
            readings: &cache,
            master: &BTreeMap::new(),
            master_known: &BTreeSet::new(),
            cond: "test",
        },
        draws,
        Some(40),
        work.path(),
    )
    .unwrap();
    let get = |k: &str| -> Vec<(String, i32)> {
        values.moves[k].iter().map(|m| (m.mv.clone(), m.value)).collect()
    };
    // E1 (後手番) の読み -20 → E 20 → AB4 -20 → AB 20 → A -20、AD は読み 100 → A の 5a4a -100。
    assert_eq!(get(&key(&["5i4i", "5a4a"])), [("4i5i".to_owned(), 20)]);
    assert_eq!(get(&key(&["5i5h"])), [("5a5b".to_owned(), -20), ("5a4a".to_owned(), -100)]);
    // 根の 2 手はどちらも 20 を保つ。同値なら出口へ短く届く 5i4i を残し、遠回りの 5i5h は 1 下げる
    // (book_backprop の同値手の補正)。確認点の読み (5i5h 10) は値に混ぜない。
    assert_eq!(get(ROOT), [("5i4i".to_owned(), 20), ("5i5h".to_owned(), 19)]);
    // 根の確認点: 候補の外の 5i6i (80) が候補の最善 (20) を 40 以上上回る。
    assert_eq!(values.marks.len(), 1);
    let m = &values.marks[0];
    assert_eq!(
        (m.key.as_str(), m.mv.as_str(), m.cp, m.best.as_str(), m.best_value),
        (ROOT, "5i6i", 80, "5i4i", 20)
    );

    let quiet = readings(&s, &[("5i6i", 59), ("5i5h", 10)]);
    let values = value_root(
        &root(),
        &tree,
        &ValueInput {
            readings: &quiet,
            master: &BTreeMap::new(),
            master_known: &BTreeSet::new(),
            cond: "test",
        },
        draws,
        Some(40),
        work.path(),
    )
    .unwrap();
    assert!(values.marks.is_empty());
    let values_all = value_root(
        &root(),
        &tree,
        &ValueInput {
            readings: &quiet,
            master: &BTreeMap::new(),
            master_known: &BTreeSet::new(),
            cond: "test",
        },
        draws,
        None,
        work.path(),
    )
    .unwrap();
    // 線を外すと、候補の外の 5i6i (59) が最善 (20) から 40 未満でも印になる。
    let marks: Vec<_> = values_all.marks.iter().map(|m| (m.key.as_str(), m.mv.as_str())).collect();
    assert_eq!(marks, [(ROOT, "5i6i")]);
    let nodes = register_nodes(0, &root(), "own", &tree, &values, &BTreeSet::new(), 4, s.nodes);
    assert_eq!(nodes[ROOT].moves[0].value_kind, "tree:4");
    let exit = key(&["5i5h", "5a4a"]);
    assert!(nodes[&exit].moves.iter().all(|m| m.value_kind == "single"));
}

#[test]
fn exit_readings_that_rejoin_the_graph_are_registered_as_propagated() {
    let s = settings();
    let tree = cut_root(&root(), &params(4, 100), &mut lookup, None, &BTreeMap::new()).unwrap();
    let exit = key(&["5i5h", "5a4a"]);
    let destination = key(&["5i4i", "5a4a", "4i5i"]);
    assert_eq!(child(&exit, "5h5i").unwrap(), destination);
    assert!(tree.nodes.contains_key(&destination));
    assert!(!tree.edges.iter().any(|e| e.key == exit));
    let mut cache = readings(&s, &[("5i6i", 0)]);
    cache.insert(exit.clone(), reading(&exit, &[("5h5i", 100), ("5h6h", 10)], &s));
    let work = tempfile::tempdir().unwrap();
    let values = value_root(
        &root(),
        &tree,
        &ValueInput {
            readings: &cache,
            master: &BTreeMap::new(),
            master_known: &BTreeSet::new(),
            cond: "test",
        },
        DrawValues::default(),
        Some(40),
        work.path(),
    )
    .unwrap();
    let joined = values.moves[&exit].iter().find(|m| m.mv == "5h5i").unwrap();
    assert!(!joined.expanded);
    assert_eq!(joined.destination_level, Some(tree.nodes[&destination].level));
    assert_eq!(joined.value, 20);
    let nodes = register_nodes(0, &root(), "own", &tree, &values, &BTreeSet::new(), 4, s.nodes);
    let moves = &nodes[&exit].moves;
    assert_eq!(moves.iter().find(|m| m.mv == "5h5i").unwrap().value_kind, "tree:2");
    assert_eq!(moves.iter().find(|m| m.mv == "5h6h").unwrap().value_kind, "single");

    let mut rejoined = tree.clone();
    rejoined.nodes.get_mut(&exit).unwrap().level = 4;
    rejoined.nodes.get_mut(&destination).unwrap().level = 1;
    let values = value_root(
        &root(),
        &rejoined,
        &ValueInput {
            readings: &cache,
            master: &BTreeMap::new(),
            master_known: &BTreeSet::new(),
            cond: "test",
        },
        DrawValues::default(),
        Some(40),
        work.path(),
    )
    .unwrap();
    let borrowed =
        register_nodes(0, &root(), "own", &rejoined, &values, &BTreeSet::new(), 4, s.nodes);
    let table = format!(
        "key\tply\tlevel\tremaining\tnodes\tmove\tvalue\texpanded\trank\n{exit}\t5\t4\t0\t100\t5h5i\t20\t0\t1\n{destination}\t2\t1\t3\t100\t4a3a\t-20\t0\t1\n"
    );
    let registered =
        crate::book_tree::register::parse_node_moves(&table, 0, Some(&BTreeSet::new())).unwrap();
    let borrowed_kind = &borrowed[&exit].moves.iter().find(|m| m.mv == "5h5i").unwrap().value_kind;
    assert_eq!(borrowed_kind, "tree:4");
    assert_eq!(borrowed_kind, &registered[&exit].moves[0].value_kind);
}

#[test]
fn value_root_uses_master_values_at_master_exits_and_draws_in_cycles() {
    let s = settings();
    let e = key(&["5i4i", "5a4a"]);
    let empty = BTreeMap::new();
    let master = BTreeMap::from([(
        e.clone(),
        MasterValue {
            main: Some(-15),
            best: Some(99),
        },
    )]);
    let stop = MasterStop {
        values: &master,
        mode: MasterExit::Use,
    };
    let tree = cut_root(&root(), &params(4, 100), &mut lookup, Some(&stop), &empty).unwrap();
    let cache = readings(&s, &[("5i5h", 10)]);
    let work = tempfile::tempdir().unwrap();
    let draws = DrawValues {
        black: -70,
        white: 70,
    };
    let values = value_root(
        &root(),
        &tree,
        &ValueInput {
            readings: &cache,
            master: &master,
            master_known: &BTreeSet::new(),
            cond: "test",
        },
        draws,
        Some(40),
        work.path(),
    )
    .unwrap();
    // E は先手番 (自分の手番) なので元データの main の値 -15。B の 5a4a は 15、根の 5i4i は -15。
    let root_moves: BTreeMap<_, _> =
        values.moves[ROOT].iter().map(|m| (m.mv.as_str(), m.value)).collect();
    assert_eq!(root_moves["5i4i"], -15);
    assert!(values.moves[&e].is_empty());

    // 部分木の中の循環 (根 → 5i5h → 5a5b → 5h5i で戻る) は千日手の値 (先手を自分として −70) になる。
    let cyc_book = |sfen: &str| -> Result<Vec<BookEntryMove>> {
        let k = crate::book_db::strip_ply(sfen);
        let rows: Vec<(&str, i32)> = if k == ROOT {
            vec![("5i5h", 0)]
        } else if k == key(&["5i5h"]) {
            vec![("5a5b", 0)]
        } else if k == key(&["5i5h", "5a5b"]) {
            vec![("5h5i", 0)]
        } else if k == key(&["5i5h", "5a5b", "5h5i"]) {
            vec![("5b5a", 0)]
        } else {
            vec![]
        };
        Ok(rows
            .into_iter()
            .map(|(m, v)| BookEntryMove {
                move_usi: m.into(),
                value: v,
                depth: 1,
                move_count: 1,
            })
            .collect())
    };
    let p = CutParams {
        check_every: 0,
        ..params(10, 100)
    };
    let tree = cut_root(&root(), &p, &mut { cyc_book }, None, &empty).unwrap();
    assert_eq!(tree.nodes.len(), 4);
    assert!(tree.nodes.values().all(|n| n.kind == Kind::Inner && !n.read));
    let values = value_root(
        &root(),
        &tree,
        &ValueInput {
            readings: &BTreeMap::new(),
            master: &BTreeMap::new(),
            master_known: &BTreeSet::new(),
            cond: "test",
        },
        draws,
        Some(40),
        work.path(),
    )
    .unwrap();
    assert_eq!(values.moves[ROOT][0].value, -70);
    assert_eq!(values.moves[ROOT][0].cond, "unknown");
}

#[test]
fn master_exit_conditions_follow_each_move_without_tainting_independent_branches() {
    let s = settings();
    let master_key = key(&["5i4i"]);
    let read_key = key(&["5i5h"]);
    let master = BTreeMap::from([(
        master_key.clone(),
        MasterValue {
            main: Some(-10),
            best: Some(-10),
        },
    )]);
    let stop = MasterStop {
        values: &master,
        mode: MasterExit::Use,
    };
    let mut lookup = |sfen: &str| -> Result<Vec<BookEntryMove>> {
        Ok(if crate::book_db::strip_ply(sfen) == ROOT {
            ["5i4i", "5i5h"]
                .into_iter()
                .map(|mv| BookEntryMove {
                    move_usi: mv.into(),
                    value: 0,
                    depth: 1,
                    move_count: 1,
                })
                .collect()
        } else {
            Vec::new()
        })
    };
    let tree =
        cut_root(&root(), &params(4, 100), &mut lookup, Some(&stop), &BTreeMap::new()).unwrap();
    assert_eq!(tree.nodes[&master_key].kind, Kind::Master);
    assert!(!tree.nodes[&master_key].read);
    let cache = BTreeMap::from([
        (ROOT.into(), reading(ROOT, &[("5i5h", 12), ("5i4i", 10)], &s)),
        (read_key.clone(), reading(&read_key, &[("5a4a", -12)], &s)),
    ]);
    let work = tempfile::tempdir().unwrap();
    for (known, expected) in [
        (BTreeSet::new(), "unknown"),
        (BTreeSet::from([master_key.clone()]), "ledger"),
    ] {
        let values = value_root(
            &root(),
            &tree,
            &ValueInput {
                readings: &cache,
                master: &master,
                master_known: &known,
                cond: "ledger",
            },
            DrawValues::default(),
            Some(40),
            work.path(),
        )
        .unwrap();
        let nodes = register_nodes(0, &root(), "own", &tree, &values, &BTreeSet::new(), 4, s.nodes);
        let moves = &nodes[ROOT].moves;
        assert_eq!(moves.iter().find(|m| m.mv == "5i4i").unwrap().cond, expected);
        assert_eq!(moves.iter().find(|m| m.mv == "5i5h").unwrap().cond, "ledger");
    }
}

fn write_book(path: &Path) {
    let mut db = BookDb::default();
    for (k, moves) in book() {
        db.entries.insert(
            k.clone(),
            PositionEntry {
                sfen: format!("{k} 1"),
                moves: moves
                    .iter()
                    .map(|(m, v)| BookMove {
                        move_usi: Some((*m).into()),
                        ponder_usi: None,
                        value: *v,
                        depth: 10,
                        count: 1,
                    })
                    .collect(),
            },
        );
    }
    db.write(path).unwrap();
}

fn cut_args(dir: &Path, out: &str, master: Option<PathBuf>) -> CutArgs {
    CutArgs::for_test(dir.join("big.db"), dir.join("roots.jsonl"), dir.join(out), master)
}

#[test]
fn read_cut_rejects_malformed_book_sha256() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write_book(&d.join("big.db"));
    std::fs::write(
        d.join("roots.jsonl"),
        format!("{{\"id\":\"r1\",\"root\":\"sfen {ROOT} 1\",\"own\":\"b\",\"depth\":6}}\n"),
    )
    .unwrap();
    super::cut::run(cut_args(d, "cut", None)).unwrap();
    assert!(read_cut(&d.join("cut")).is_ok());
    let meta = d.join("cut").join(META);
    let text = std::fs::read_to_string(&meta).unwrap();
    let sha = serde_json::from_str::<serde_json::Value>(&text).unwrap()["book"]["sha256"]
        .as_str()
        .unwrap()
        .to_string();
    std::fs::write(&meta, text.replace(&sha, "abc")).unwrap();
    assert!(read_cut(&d.join("cut")).is_err());
}

#[test]
fn cut_and_value_run_end_to_end_and_register_ext_rows() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write_book(&d.join("big.db"));
    std::fs::write(
        d.join("roots.jsonl"),
        format!("{{\"id\":\"r1\",\"root\":\"sfen {ROOT} 1\",\"own\":\"b\",\"depth\":6,\"memo\":\"x\"}}\n"),
    )
    .unwrap();
    super::cut::run(cut_args(d, "cut", None)).unwrap();
    // 同じ入力なら同じ出力。出力先が既にあれば止まる。
    super::cut::run(cut_args(d, "cut2", None)).unwrap();
    for name in [
        META,
        NODES,
        EDGES,
        "reads.tsv",
        "targets.jsonl",
        "summary.tsv",
    ] {
        assert_eq!(
            std::fs::read(d.join("cut").join(name)).unwrap(),
            std::fs::read(d.join("cut2").join(name)).unwrap(),
            "{name}"
        );
    }
    assert!(super::cut::run(cut_args(d, "cut", None)).is_err());

    let s = settings();
    let cache = readings(&s, &[("5i5h", 10), ("5i4i", 5)]);
    // キューの行は段 0 で、手番ごとに読む局面すべてを根に持つ。台帳の読みだけで木が終わる。
    let queue = std::fs::read_to_string(d.join("cut/targets.jsonl")).unwrap();
    let defaults = Defaults {
        opp_n: 2,
        stop: 300,
        rounds: 3,
        draw: 0,
    };
    let mut read_by_queue = BTreeSet::new();
    for text in queue.lines() {
        let line: Line = serde_json::from_str(text).unwrap();
        assert_eq!(line.depth, 0);
        let root = line.check().unwrap();
        let resolved = line.resolve(&root, &defaults).unwrap();
        let mut tree = resolved.build(&line.own, &d.join("q").join(&line.id)).unwrap();
        let missing = tree.advance(&cache).unwrap();
        assert!(missing.is_empty() && tree.finished, "{missing:?}");
        assert!(tree.write_outputs().unwrap());
        read_by_queue.extend(tree.nodes.keys().cloned());
    }
    assert_eq!(read_by_queue, cache.keys().cloned().collect());

    let journal = d.join("journal.jsonl");
    let mut j = Journal::open(&journal, &s.fingerprint).unwrap();
    for r in cache.values() {
        j.append(r.clone(), &s, "test".into()).unwrap();
    }
    drop(j);

    // --parts: 読む局面を速さの比で分け、どの局面も 1 つの分け先だけに入る。台帳にある局面は除く。
    let part_keys = |out: &str| -> Vec<Vec<String>> {
        (0..2)
            .map(|i| {
                let text =
                    std::fs::read_to_string(d.join(out).join(format!("parts/part-{i}.jsonl")))
                        .unwrap();
                text.lines()
                    .flat_map(|t| {
                        let line: Line = serde_json::from_str(t).unwrap();
                        assert_eq!((line.depth, line.priority), (0, DEFAULT_PRIORITY));
                        std::iter::once(line.root).chain(line.roots)
                    })
                    .map(|sfen| sfen.split(' ').skip(1).take(3).collect::<Vec<_>>().join(" "))
                    .collect()
            })
            .collect()
    };
    let mut args = cut_args(d, "cut_parts", None);
    args.set_parts(vec![2, 1], None, None);
    super::cut::run(args).unwrap();
    let parts = part_keys("cut_parts");
    let all: BTreeSet<String> = parts.iter().flatten().cloned().collect();
    assert_eq!(all.len(), parts.iter().map(Vec::len).sum::<usize>());
    assert_eq!(all, cache.keys().cloned().collect());
    // (割り当て済み + 1) / 速さ が最小の分け先へ (同じなら番号の小さい方): 速さ 2:1 なら 0,0,1 の繰り返し。
    let n = all.len();
    let expected0 = (0..n).filter(|i| i % 3 != 2).count();
    assert_eq!((parts[0].len(), parts[1].len()), (expected0, n - expected0));
    // 同じ入力なら同じ分け方。
    let mut args = cut_args(d, "cut_parts2", None);
    args.set_parts(vec![2, 1], None, None);
    super::cut::run(args).unwrap();
    for i in 0..2 {
        let f = format!("parts/part-{i}.jsonl");
        assert_eq!(
            std::fs::read(d.join("cut_parts").join(&f)).unwrap(),
            std::fs::read(d.join("cut_parts2").join(&f)).unwrap()
        );
    }
    // 台帳に読みのある局面だけを除く (一部だけ持つ台帳、指紋を指定)。
    let partial = d.join("partial.jsonl");
    let mut j = Journal::open(&partial, &s.fingerprint).unwrap();
    let known: BTreeSet<String> = cache.keys().take(n / 2).cloned().collect();
    for k in &known {
        j.append(cache[k].clone(), &s, "test".into()).unwrap();
    }
    drop(j);
    let mut args = cut_args(d, "cut_skip", None);
    args.set_parts(vec![2, 1], Some(partial), Some(s.fingerprint.clone()));
    super::cut::run(args).unwrap();
    let rest: BTreeSet<String> = part_keys("cut_skip").into_iter().flatten().collect();
    assert_eq!(rest, all.difference(&known).cloned().collect());
    let mut args = cut_args(d, "cut_skip_all", None);
    args.set_parts(vec![2, 1], Some(journal.clone()), None);
    super::cut::run(args).unwrap();
    assert!(part_keys("cut_skip_all").iter().all(Vec::is_empty));

    let ab = key(&["5i5h", "5a5b"]);
    let master = d.join("moves.tsv");
    let original =
        crate::book_tree::master_table::format_master(&[crate::book_tree::master_table::test_row(
            &[
                ("key", ab.clone()),
                ("ply", "3".into()),
                ("move", "5h6h".into()),
                ("role", "main".into()),
                ("source", "team".into()),
                ("evals", "1".into()),
                ("nodes", "1e9".into()),
                ("tier", "wide".into()),
                ("since", "old".into()),
                ("note", "n".into()),
            ],
        )]);
    std::fs::write(&master, &original).unwrap();
    let value_args = |out: &str, apply: bool| {
        ValueArgs::for_test(d.join("cut"), journal.clone(), master.clone(), d.join(out), apply)
    };
    super::value::run(value_args("v1", false)).unwrap();
    assert_eq!(std::fs::read_to_string(&master).unwrap(), original);
    let roots = std::fs::read_to_string(d.join("v1/roots.tsv")).unwrap();
    assert_eq!(roots.lines().nth(1).unwrap().split('\t').nth(5), Some("5i4i"));
    let conflicts = std::fs::read_to_string(d.join("v1/conflicts.tsv")).unwrap();
    assert_eq!(conflicts.lines().count(), 2);
    assert!(conflicts.lines().nth(1).unwrap().ends_with("\tr1"));
    assert!(super::value::run(value_args("v1", true)).is_err());
    super::value::run(value_args("v2", true)).unwrap();

    let rows = parse_master(&std::fs::read_to_string(&master).unwrap()).unwrap();
    let find = |k: &str, m: &str| rows.iter().find(|r| r.get("key") == k && r.get("move") == m);
    let main = find(ROOT, "5i4i").unwrap();
    let lineage = format!(
        "book:big.db@{}",
        &crate::book_tree::journal::digest(&d.join("big.db")).unwrap()[..8]
    );
    assert_eq!(
        (
            main.get("role"),
            main.get("source"),
            main.get("evals"),
            main.get("lineage"),
            main.get("tier")
        ),
        ("main", "ext", "20", lineage.as_str(), "wide")
    );
    assert_eq!(find(ROOT, "5i5h").unwrap().get("role"), "alt");
    // 読み由来でない main は替えず、伝播後の 1 位を alt に足す。
    assert_eq!(find(&ab, "5h6h").unwrap().get("role"), "main");
    assert_eq!(find(&ab, "5h4i").unwrap().get("role"), "alt");
    // 相手の手番の局面の手は、指さない record として残す (読みの値つき)。
    let reply = find(&key(&["5i5h"]), "5a5b").unwrap();
    assert_eq!((reply.get("role"), reply.get("source")), ("record", "ext"));
    assert!(!reply.get("evals").is_empty());
    // 自分の手番の出口は読みの値で登録する。
    assert_eq!(find(&key(&["5i5h", "5a4a"]), "5h4h").unwrap().get("role"), "main");

    // 読みが足りなければ何も書かずに止まる。
    let short = d.join("short.jsonl");
    let mut j = Journal::open(&short, &s.fingerprint).unwrap();
    j.append(cache[ROOT].clone(), &s, "test".into()).unwrap();
    drop(j);
    let args = ValueArgs::for_test(d.join("cut"), short, master.clone(), d.join("v3"), false);
    assert!(super::value::run(args).is_err());
    assert!(!d.join("v3").exists());
}

#[test]
fn cut_rejects_bad_roots() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write_book(&d.join("big.db"));
    for (i, line) in [
        format!("{{\"id\":\"r1\",\"root\":\"sfen {ROOT} 1\",\"own\":\"w\"}}"),
        format!("{{\"id\":\"r 1\",\"root\":\"sfen {ROOT} 1\",\"own\":\"b\"}}"),
        format!("{{\"id\":\"r1\",\"root\":\"sfen {ROOT} 1\",\"own\":\"bw\"}}"),
        format!(
            "{{\"id\":\"r1\",\"root\":\"sfen {ROOT} 1\",\"own\":\"b\"}}\n{{\"id\":\"r1\",\"root\":\"sfen {ROOT} 1\",\"own\":\"b\"}}"
        ),
    ]
    .iter()
    .enumerate()
    {
        std::fs::write(d.join("roots.jsonl"), line).unwrap();
        assert!(super::cut::run(cut_args(d, &format!("o{i}"), None)).is_err(), "{line}");
    }
}

#[test]
fn a_mark_from_any_root_keeps_the_position_out_of_registration() {
    let s = settings();
    let empty = BTreeMap::new();
    let tree = cut_root(&root(), &params(4, 100), &mut lookup, None, &empty).unwrap();
    let cache = readings(&s, &[("5i5h", 10)]);
    let work = tempfile::tempdir().unwrap();
    let draws = DrawValues {
        black: -70,
        white: 70,
    };
    let values = value_root(
        &root(),
        &tree,
        &ValueInput {
            readings: &cache,
            master: &BTreeMap::new(),
            master_known: &BTreeSet::new(),
            cond: "test",
        },
        draws,
        Some(40),
        work.path(),
    )
    .unwrap();
    assert!(values.marks.is_empty());
    let ab = key(&["5i5h", "5a5b"]);
    let all = register_nodes(0, &root(), "own", &tree, &values, &BTreeSet::new(), 4, 100);
    assert!(all.contains_key(ROOT) && all.contains_key(&ab));
    let opp = key(&["5i5h"]);
    assert!(!all.contains_key(&opp));
    // 相手の手番の局面は、部分木の手と伝播後の値で別に集める。
    let theirs = register_nodes(0, &root(), "opp", &tree, &values, &BTreeSet::new(), 4, 100);
    assert!(!theirs.contains_key(ROOT));
    assert_eq!(theirs[&opp].moves.first().map(|m| m.mv.as_str()), Some("5a5b"));
    assert!(theirs[&opp].moves.iter().all(|m| m.value_kind.starts_with("tree:")));
    // 別の根がその局面を自分の手番として持てば、相手の手番の根の局面より先に使う。
    let mut other = root();
    other.id = "r2".into();
    other.own = "w".into();
    let mut own_values = RootValues {
        moves: values.moves.clone(),
        marks: Vec::new(),
    };
    own_values.moves.get_mut(&opp).unwrap().retain(|m| m.mv == "5a4a");
    let mut nodes = BTreeMap::new();
    let (_, opponent) = collect_nodes(
        &mut nodes,
        &[root(), other],
        &[tree.clone(), tree.clone()],
        &[
            RootValues {
                moves: values.moves.clone(),
                marks: Vec::new(),
            },
            own_values,
        ],
        &BTreeSet::new(),
        4,
        100,
    );
    assert_eq!(nodes[&opp].moves.iter().map(|m| m.mv.as_str()).collect::<Vec<_>>(), ["5a4a"]);
    assert!(nodes.contains_key(ROOT));
    // 自分の手番として持つ根のある局面は、相手の手番の局面として返さない。
    assert!(!opponent.contains_key(&opp));
    let (_, alone) = collect_nodes(
        &mut BTreeMap::new(),
        &[root()],
        std::slice::from_ref(&tree),
        &[RootValues {
            moves: values.moves.clone(),
            marks: Vec::new(),
        }],
        &BTreeSet::new(),
        4,
        100,
    );
    assert!(alone.contains_key(&opp) && !alone.contains_key(ROOT));
    let marked = BTreeSet::from([ab.as_str()]);
    let without = register_nodes(0, &root(), "own", &tree, &values, &marked, 4, 100);
    assert!(without.contains_key(ROOT) && !without.contains_key(&ab));
    assert_eq!(without[ROOT].remaining, 4);
}

#[test]
fn a_subtree_without_readings_is_valued_without_a_journal() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let mut db = BookDb::default();
    for (moves, mv) in [
        (&[][..], "5i5h"),
        (&["5i5h"][..], "5a5b"),
        (&["5i5h", "5a5b"][..], "5h5i"),
        (&["5i5h", "5a5b", "5h5i"][..], "5b5a"),
    ] {
        let k = key(moves);
        db.entries.insert(
            k.clone(),
            PositionEntry {
                sfen: format!("{k} 1"),
                moves: vec![BookMove {
                    move_usi: Some(mv.into()),
                    ponder_usi: None,
                    value: 0,
                    depth: 1,
                    count: 1,
                }],
            },
        );
    }
    db.write(&d.join("big.db")).unwrap();
    std::fs::write(
        d.join("roots.jsonl"),
        format!("{{\"id\":\"r1\",\"root\":\"sfen {ROOT} 1\",\"own\":\"b\"}}\n"),
    )
    .unwrap();
    let mut args = cut_args(d, "cut", None);
    args.set_check_every(0);
    super::cut::run(args).unwrap();
    assert_eq!(std::fs::read_to_string(d.join("cut/targets.jsonl")).unwrap(), "");
    let master = d.join("moves.tsv");
    std::fs::write(&master, format!("{}\n", COLUMNS.join("\t"))).unwrap();
    let args = ValueArgs::for_test(
        d.join("cut"),
        d.join("absent.jsonl"),
        master.clone(),
        d.join("v"),
        true,
    );
    super::value::run(args).unwrap();
    let rows = parse_master(&std::fs::read_to_string(&master).unwrap()).unwrap();
    let root_row = rows.iter().find(|r| r.get("key") == ROOT).unwrap();
    assert_eq!((root_row.get("move"), root_row.get("evals")), ("5i5h", "-70"));
    let mut rows = rows;
    for row in &mut rows {
        if row.get("key") == ROOT {
            row.set("role", "bench".into());
        }
    }
    std::fs::write(&master, crate::book_tree::master_table::format_master(&rows)).unwrap();
    let args = ValueArgs::for_test(
        d.join("cut"),
        d.join("absent.jsonl"),
        master.clone(),
        d.join("bench"),
        true,
    );
    super::value::run(args).unwrap();
    let updated = parse_master(&std::fs::read_to_string(&master).unwrap()).unwrap();
    assert_eq!(
        updated.iter().find(|r| r.get("key") == ROOT).unwrap(),
        rows.iter().find(|r| r.get("key") == ROOT).unwrap()
    );
}

#[test]
fn cut_reads_ybb_like_the_sorted_db_it_was_converted_from() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let testdata = Path::new(env!("CARGO_MANIFEST_DIR")).join("../rshogi-book/testdata");
    // 変換元の .db は並んでいないので、on the fly で引ける順に書き直す。
    BookDb::read(&testdata.join("small.db"))
        .unwrap()
        .write(&d.join("big.db"))
        .unwrap();
    std::fs::copy(testdata.join("small_depth.ybb"), d.join("big.ybb")).unwrap();
    std::fs::write(
        d.join("roots.jsonl"),
        "{\"id\":\"r1\",\"root\":\"sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1\",\"own\":\"b\"}\n",
    )
    .unwrap();
    super::cut::run(cut_args(d, "from_db", None)).unwrap();
    let ybb_args =
        CutArgs::for_test(d.join("big.ybb"), d.join("roots.jsonl"), d.join("from_ybb"), None);
    super::cut::run(ybb_args).unwrap();
    for name in [NODES, EDGES, "reads.tsv", "summary.tsv"] {
        let from_db = std::fs::read_to_string(d.join("from_db").join(name)).unwrap();
        assert_eq!(
            from_db,
            std::fs::read_to_string(d.join("from_ybb").join(name)).unwrap(),
            "{name}"
        );
        assert!(from_db.lines().count() > 1, "{name}");
    }
}

#[test]
fn values_are_joined_across_roots_before_choosing_the_main() {
    use crate::book_tree::register::{self, Node, NodeMove, Options};
    use crate::book_tree::schema::Schema;
    let node = |ply: &str, tree: usize, moves: &[(&str, i32)]| Node {
        ply: ply.into(),
        remaining: 2,
        nodes: 100,
        moves: moves
            .iter()
            .map(|(m, v)| NodeMove {
                cond: "test".into(),
                mv: (*m).into(),
                value: *v,
                value_kind: "tree:2".into(),
            })
            .collect(),
        tree,
    };
    // 後手の根の木は、5a5b の後の先手の手を大きい定跡の 5h4i だけで見て 5a5b を 1 位にした。
    // 先手の根の木はその局面を読み、5h6h が先手に大きく良いと分かっている。
    let a = key(&["5i5h"]);
    let ab = key(&["5i5h", "5a5b"]);
    let mut nodes = BTreeMap::from([
        (a.clone(), node("2", 1, &[("5a5b", -10), ("5a4a", -30)])),
        (ab.clone(), node("3", 0, &[("5h6h", 200), ("5h4i", 10)])),
    ]);
    let schema = Schema::default();
    let options = Options {
        since: "tag",
        note: "",
        alt_gap: 40,
        source: "ext",
        lineage: "book:big.db@00000000",
        schema: &schema,
        replace_curated: false,
        revive_bench: false,
    };
    let draws = DrawValues {
        black: -70,
        white: 70,
    };
    let joined = join_roots(&[], &mut nodes, &options, draws, 40, "test").unwrap();
    assert_eq!((joined.moves, joined.best), (1, 1));
    let moves: Vec<(&str, i32, &str, &str)> = nodes[&a]
        .moves
        .iter()
        .map(|m| (m.mv.as_str(), m.value, m.cond.as_str(), m.value_kind.as_str()))
        .collect();
    assert_eq!(
        moves,
        [
            ("5a4a", -30, "test", "tree:2"),
            ("5a5b", -200, "test", "unknown")
        ]
    );
    // 全体の値で main を選ぶので、登録後の点検で main が 1 位から負けない。
    let (rows, _, _) = register::register(Vec::new(), &nodes, &options).unwrap();
    let roles: Vec<(&str, &str)> = rows
        .iter()
        .filter(|r| r.get("key") == a)
        .map(|r| (r.get("move"), r.get("role")))
        .collect();
    assert_eq!(roles, [("5a4a", "main")]);
    // 登録後の点検でも、登録した手の値は全体の伝播の値と同じで、main は 1 位から負けない。
    let book = crate::book_tree::master_table::positions_from_rows(
        rows.iter().cloned().enumerate().map(|(i, r)| Ok((i + 2, r))),
    )
    .unwrap();
    let (findings, _, _) =
        crate::book_tree::audit::audit(&book, draws, 1, crate::book_tree::audit::Fill::Off, None)
            .unwrap();
    assert!(findings.is_empty());
    let values = crate::book_tree::audit::evaluate(&book.0, draws).unwrap();
    let i = book.0.keys().position(|k| *k == a).unwrap();
    assert_eq!(values[i], [Some(-30)]);

    // 先の局面の手に台帳と違う条件の値があれば、替わった手の条件は分からないとする。
    let mut other = BTreeMap::from([
        (a.clone(), node("2", 1, &[("5a5b", -10), ("5a4a", -30)])),
        (ab.clone(), node("3", 0, &[("5h6h", 200), ("5h4i", 10)])),
    ]);
    other.get_mut(&ab).unwrap().moves[1].cond = "other".into();
    join_roots(&[], &mut other, &options, draws, 40, "test").unwrap();
    let moved = other[&a].moves.iter().find(|m| m.mv == "5a5b").unwrap();
    assert_eq!((moved.value, moved.cond.as_str()), (-200, "unknown"));
}

#[test]
fn append_records_adds_new_moves_and_refreshes_only_own_source_records() {
    use crate::book_tree::master_table::test_row;
    use crate::book_tree::register::{Node, NodeMove, Options};
    let opp = key(&["5i5h"]);
    let row = |mv: &str, role: &str, source: &str, tier: &str| {
        let mut r = test_row(&[
            ("key", opp.clone()),
            ("move", mv.into()),
            ("role", role.into()),
            ("evals", "1".into()),
        ]);
        r.set("source", source.into());
        r.set("tier", tier.into());
        r
    };
    let mut rows = vec![
        row("5a5b", "record", "ext", "wide"),
        row("5a4a", "record", "read", "wide"),
        row("5a6a", "bench", "ext", "wide"),
    ];
    let schema = crate::book_tree::schema::Schema::load(None).unwrap();
    let options = Options {
        since: "v2",
        note: "",
        alt_gap: 30,
        source: "ext",
        lineage: "book:big.db@00000000",
        schema: &schema,
        replace_curated: false,
        revive_bench: false,
    };
    let node = |mv: &str, value: i32| NodeMove {
        cond: "test".into(),
        mv: mv.into(),
        value,
        value_kind: "single".into(),
    };
    let opponent = BTreeMap::from([(
        opp.clone(),
        Node {
            ply: "2".into(),
            remaining: 0,
            nodes: 100,
            moves: vec![
                node("5a5b", -20),
                node("5a4a", -30),
                node("5a6a", -40),
                node("5a4b", -50),
            ],
            tree: 0,
        },
    )]);
    assert_eq!(append_records(&mut rows, &opponent, &options), (1, 1));
    let get = |mv: &str| rows.iter().find(|r| r.get("move") == mv).unwrap();
    assert_eq!((get("5a5b").get("evals"), get("5a5b").get("since")), ("-20", "v2"));
    assert_eq!(get("5a4a").get("evals"), "1");
    assert_eq!((get("5a6a").get("role"), get("5a6a").get("evals")), ("bench", "1"));
    assert_eq!((get("5a4b").get("role"), get("5a4b").get("evals")), ("record", "-50"));
}

#[test]
fn records_left_on_a_position_now_registered_as_own_are_dropped() {
    use crate::book_tree::master_table::test_row;
    let own = key(&["5i5h"]);
    let other = key(&["5i4i"]);
    let row = |k: &str, mv: &str, source: &str| {
        let mut r = test_row(&[
            ("key", k.into()),
            ("move", mv.into()),
            ("role", "record".into()),
        ]);
        r.set("source", source.into());
        r.set("tier", "wide".into());
        r
    };
    let mut rows = vec![
        row(&own, "5a5b", "ext"),
        row(&own, "5a4a", "read"),
        row(&other, "5a5b", "ext"),
    ];
    let nodes = BTreeMap::from([(
        own.clone(),
        crate::book_tree::register::Node {
            ply: "2".into(),
            remaining: 0,
            nodes: 1,
            moves: Vec::new(),
            tree: 0,
        },
    )]);
    drop_own_records(&mut rows, &nodes, "ext");
    let left: Vec<_> = rows.iter().map(|r| (r.get("key") == own, r.get("move"))).collect();
    assert_eq!(left, [(true, "5a4a"), (false, "5a5b")]);
}
