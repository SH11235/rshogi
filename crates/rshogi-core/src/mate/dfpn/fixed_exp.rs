use std::collections::HashMap;
use std::sync::atomic::AtomicBool;

use super::search::{Budget, NO_LEN_LIMIT, NodeArgs, Scratch, Searcher, ShortestTables};
use super::state::{MateState, NoAttackerKing};
use super::table::MateTable;
use super::tests::position;
use crate::time::Instant;

fn run<S: MateState>(st: &mut S, attacker: crate::types::Color, shortest: bool, futility: bool, no_drops: bool, limit: u16, node_limit: u64) -> (String, u64, u128) {
    let stop = AtomicBool::new(false);
    let mut progress = |_| {};
    let budget = Budget {
        stop: &stop,
        halt: None,
        cancel: None,
        shared_nodes: None,
        node_limit,
        time_limit_ms: 0,
        start: Instant::now(),
        progress: &mut progress,
    };
    let table = MateTable::new(2048, true);
    let tables = ShortestTables::new();
    let mut scratch = Scratch::new();
    super::search::DBG_W.with(|m| m.borrow_mut().clear());
    super::search::DBG_G.with(|m| m.borrow_mut().clear());
    let root_key = st.table_key(attacker).key;
    let t0 = std::time::Instant::now();
    let mut searcher = Searcher::<_, true>::new(&table, &mut scratch, attacker, budget);
    if shortest {
        searcher.enable_shortest(futility, &tables);
    }
    if no_drops {
        searcher.dbg_set_no_drops();
    }
    let r = searcher.search(st, NodeArgs::unbounded(0, false, limit), &mut 0);
    if let Ok(r) = &r && r.is_disproven() {
        let (reach, leaves) = super::search::DBG_G.with(|m| {
            let m = m.borrow();
            let mut seen = std::collections::HashSet::new();
            let mut stack = vec![root_key];
            let mut leaves = 0u64;
            while let Some(k) = stack.pop() {
                if !seen.insert(k) {
                    continue;
                }
                match m.get(&k) {
                    Some(ks) => stack.extend(ks.iter().copied()),
                    None => leaves += 1,
                }
            }
            (seen.len(), leaves)
        });
        println!("\nW\tdag {reach}\tleaves {leaves}\ttree {}", r.dbg_w);
    }
    let tag = match r {
        Ok(r) if r.is_proven() => format!("mate{}", r.mate_len),
        Ok(r) if r.is_disproven() => "nomate".to_string(),
        Ok(_) => "unknown".to_string(),
        Err(_) => "abort".to_string(),
    };
    let nodes = searcher.dbg_nodes();
    drop(searcher);
    if tag == "nomate" && limit != NO_LEN_LIMIT && std::env::var("DBG_WALK").is_ok() {
        let mut w = Walker { table: &table, tables: &tables, attacker, seen: HashMap::new(), path: Vec::new(), or_nodes: 0, and_nodes: 0, gaps: 0, settled: 0, loops: 0, budget: 50_000_000 };
        w.walk(st, false, limit);
        println!("WIT\tor {}\tand {}\tgaps {}\tsettled {}\tloops {}\tdfpn {}", w.or_nodes, w.and_nodes, w.gaps, w.settled, w.loops, nodes);
    }
    (tag, nodes, t0.elapsed().as_millis())
}

/// 上限付きの不詰の証拠の木 (攻め方は全王手、受け方は表で上限内に詰まないと分かる応手 1 つ) の大きさ
struct Walker<'a> {
    table: &'a MateTable,
    tables: &'a ShortestTables,
    attacker: crate::types::Color,
    seen: HashMap<u64, u16>,
    path: Vec<u64>,
    or_nodes: u64,
    and_nodes: u64,
    gaps: u64,
    settled: u64,
    loops: u64,
    budget: u64,
}

impl Walker<'_> {
    fn known_escape<S: MateState>(&self, st: &S, d: u16) -> bool {
        let k = st.table_key(self.attacker);
        if self.table.probe::<true>(k).is_some_and(|v| v.dn == 0) {
            return true;
        }
        self.tables.len_bound(k.key).is_some_and(|b| b >= d)
    }

    fn walk<S: MateState>(&mut self, st: &mut S, or_node: bool, d: u16) {
        if self.or_nodes + self.and_nodes >= self.budget {
            return;
        }
        let key = st.key();
        if self.seen.get(&key).is_some_and(|&b| b >= d) {
            return;
        }
        if self.path.contains(&key) {
            self.loops += 1;
            return;
        }
        if !or_node && self.table.probe::<true>(st.table_key(self.attacker)).is_some_and(|v| v.dn == 0) {
            self.settled += 1;
            self.seen.insert(key, u16::MAX);
            return;
        }
        let mut moves = Vec::new();
        st.generate(or_node, &mut moves);
        self.path.push(key);
        if or_node {
            self.or_nodes += 1;
            if d > 0 {
                for mv in moves {
                    st.do_move(mv);
                    self.walk(st, false, d - 1);
                    st.undo_move(mv);
                }
            }
        } else {
            self.and_nodes += 1;
            let mut chosen = None;
            if d > 0 {
                for &mv in &moves {
                    st.do_move(mv);
                    let ok = self.known_escape(st, d - 1);
                    st.undo_move(mv);
                    if ok {
                        chosen = Some(mv);
                        break;
                    }
                }
            }
            match chosen {
                Some(mv) => {
                    st.do_move(mv);
                    self.walk(st, true, d - 1);
                    st.undo_move(mv);
                }
                None => {
                    if self.gaps == 0 {
                        for &mv in &moves {
                            let is_drop = st.is_drop(mv);
                            st.do_move(mv);
                            let k = st.table_key(self.attacker);
                            eprintln!("GAP d {d} drop {is_drop} probe {:?} lb {:?}", self.table.probe::<true>(k).map(|v| (v.pn, v.dn, v.mate_len)), self.tables.len_bound(k.key));
                            st.undo_move(mv);
                        }
                    }
                    self.gaps += 1
                }
            }
        }
        self.path.pop();
        self.seen.insert(key, d);
    }
}

/// DBG_M のファイル (limit \t sfen) の各局面 (受け方の手番) を、上限付き最短モードと上限なしの通常モードで解く
#[test]
#[ignore]
fn dbg_heavy_m() {
    let path = std::env::var("DBG_M").unwrap();
    let node_limit: u64 = std::env::var("DBG_NODES").ok().and_then(|v| v.parse().ok()).unwrap_or(200_000_000);
    let modes = std::env::var("DBG_MODES").unwrap_or_else(|_| "bu".to_string());
    for line in std::fs::read_to_string(path).unwrap().lines() {
        let mut f = line.split('\t');
        let limit: u16 = f.next().unwrap().parse().unwrap();
        let sfen = f.next().unwrap();
        let mut pos = position(sfen);
        pos.pause_board_effects(true);
        let attacker = !pos.side_to_move();
        let has_king = !pos.pieces(attacker, crate::types::PieceType::King).is_empty();
        for m in modes.chars() {
            let (shortest, futility, no_drops, lim) = match m {
                'b' => (true, true, false, limit),
                'u' => (false, false, false, NO_LEN_LIMIT),
                'n' => (true, false, true, limit),
                'f' => (true, false, false, limit),
                _ => continue,
            };
            let out = if has_king {
                let mut st = pos.clone();
                run(&mut st, attacker, shortest, futility, no_drops, lim, node_limit)
            } else {
                let mut st = NoAttackerKing::new(pos.clone());
                run(&mut st, attacker, shortest, futility, no_drops, lim, node_limit)
            };
            println!("RES\t{m}\t{limit}\t{}\t{}\t{}", out.0, out.1, out.2);
        }
    }
}

fn solve_on<S: MateState>(table: &MateTable, tables: &ShortestTables, st: &mut S, attacker: crate::types::Color, limit: u16) -> (String, u64) {
    let stop = AtomicBool::new(false);
    let mut progress = |_| {};
    let budget = Budget {
        stop: &stop,
        halt: None,
        cancel: None,
        shared_nodes: None,
        node_limit: 200_000_000,
        time_limit_ms: 0,
        start: Instant::now(),
        progress: &mut progress,
    };
    let mut scratch = Scratch::new();
    let mut searcher = Searcher::<_, true>::new(table, &mut scratch, attacker, budget);
    searcher.enable_shortest(true, tables);
    let r = searcher.search(st, NodeArgs::unbounded(0, false, limit), &mut 0);
    let tag = match r {
        Ok(r) if r.is_proven() => format!("mate{}", r.mate_len),
        Ok(r) if r.is_disproven() => "nomate".to_string(),
        Ok(_) => "unknown".to_string(),
        Err(_) => "abort".to_string(),
    };
    (tag, searcher.dbg_nodes())
}

fn sigma_case<S: MateState>(st: &mut S, attacker: crate::types::Color, lo: u16, hi: u16) {
    use super::search::{DBG_S, DBG_SIGMA};
    let set_sigma = |v: Option<HashMap<u64, u64>>| DBG_SIGMA.with(|s| *s.borrow_mut() = v);
    // 上限 lo で解き、受け方の逃れを集める。同じ表のまま hi を応手なしの固定で解く (warm)
    DBG_S.with(|m| m.borrow_mut().clear());
    set_sigma(None);
    let table = MateTable::new(2048, true);
    let tables = ShortestTables::new();
    let lo_r = solve_on(&table, &tables, st, attacker, lo);
    let sigma = DBG_S.with(|m| m.borrow().clone());
    let warm = solve_on(&table, &tables, st, attacker, hi);
    // 新しい表で hi を応手固定で解く (cold sigma)
    let table2 = MateTable::new(2048, true);
    let tables2 = ShortestTables::new();
    set_sigma(Some(sigma.clone()));
    let sig_cold = solve_on(&table2, &tables2, st, attacker, hi);
    // lo を解いた表で応手を固定して hi (warm sigma)
    let table3 = MateTable::new(2048, true);
    let tables3 = ShortestTables::new();
    set_sigma(None);
    let _ = solve_on(&table3, &tables3, st, attacker, lo);
    set_sigma(Some(sigma));
    let sig_warm = solve_on(&table3, &tables3, st, attacker, hi);
    set_sigma(None);
    // 比較: 新しい表で hi を普通に
    let table4 = MateTable::new(2048, true);
    let tables4 = ShortestTables::new();
    let cold = solve_on(&table4, &tables4, st, attacker, hi);
    println!("SIG\tlo {lo} {:?}\thi {hi}\tcold {:?}\twarm {:?}\tsig_cold {:?}\tsig_warm {:?}", lo_r, cold, warm, sig_cold, sig_warm);
}

#[test]
#[ignore]
fn dbg_sigma() {
    let path = std::env::var("DBG_M").unwrap();
    let step: u16 = std::env::var("DBG_STEP").ok().and_then(|v| v.parse().ok()).unwrap_or(4);
    for line in std::fs::read_to_string(path).unwrap().lines() {
        let mut f = line.split('\t');
        let hi: u16 = f.next().unwrap().parse().unwrap();
        let sfen = f.next().unwrap();
        let mut pos = position(sfen);
        pos.pause_board_effects(true);
        let attacker = !pos.side_to_move();
        let lo = hi - step;
        if !pos.pieces(attacker, crate::types::PieceType::King).is_empty() {
            let mut st = pos.clone();
            sigma_case(&mut st, attacker, lo, hi);
        } else {
            let mut st = NoAttackerKing::new(pos.clone());
            sigma_case(&mut st, attacker, lo, hi);
        }
    }
}
