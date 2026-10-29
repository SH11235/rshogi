//! 最短手順モードのテスト
//!
//! 正解役は、正解手順の定義 (最短手数・受け方は最長・無駄合いは数えない・同手数なら駒余りの無い方) を
//! そのまま書いた総当たり。合法手は指し手生成を使わない [`legal_moves_by_rules`] で求める。

use std::sync::atomic::AtomicBool;

use rand::SeedableRng;
use rand_xoshiro::Xoshiro256PlusPlus;

use super::tests::{
    ZUKOU_001, brute_mate_by_rules, checks_by_rules, position, pv_usi, random_kingless_position,
    random_tsume_position,
};
use super::verify::{check_mate_pv_by_rules, legal_moves_by_rules};
use super::*;
use crate::types::{Color, Hand, PieceType};

fn solve_shortest(pos: &mut Position, mode: MateMode, nodes: u64) -> MateResult {
    solve_counting(pos, mode, ShortestMode::Tsume, nodes)
}

fn solve_counting(
    pos: &mut Position,
    mode: MateMode,
    shortest: ShortestMode,
    nodes: u64,
) -> MateResult {
    let stop = AtomicBool::new(false);
    let limits = MateLimits { nodes, time_ms: 0 };
    let mut solver = DfpnSolver::with_mode(16, mode);
    solver.set_shortest(Some(shortest));
    solver.solve(pos, &limits, &stop, |_| {})
}

fn shortest_len(sfen: &str, shortest: ShortestMode, nodes: u64) -> usize {
    let mut lens = Vec::new();
    for mode in MateMode::ALL {
        let mut pos = position(sfen);
        let MateResult::Mate(pv) = solve_counting(&mut pos, mode, shortest, nodes) else {
            panic!("{sfen} {mode:?} {shortest:?}: 詰みを返さない");
        };
        let futile_end = shortest == ShortestMode::Tsume;
        assert!(
            check_mate_pv_by_rules(&pos, &pv, futile_end),
            "{sfen} {mode:?} {shortest:?}: {}",
            pv_usi(&pv)
        );
        lens.push(pv.len());
    }
    assert_eq!(lens[0], lens[1], "{sfen} {shortest:?}: Speed と Power で手数が違う");
    lens[0]
}

// ---------------------------------------------------------------------------
// 正解役 (総当たり)
// ---------------------------------------------------------------------------

/// `plies` 手以内に詰ませたい受け方の局面 `pos` (王手されている) の応手 `mv` が無駄合いか: 駒打ちの合駒を
/// 王手している駒が取り (成・不成のどちらか)、受け方がその駒を取り返せず、取った駒を受け方の持駒に戻した
/// 局面 M の詰み手数が、駒打ち以外の応手の詰み手数 + 1 の最大 (無ければ 0) 以下。M が `plies` 手以内に
/// 詰まなければ無駄合いとしない
fn ref_is_futile(pos: &mut Position, mv: Move, plies: u32) -> bool {
    let checkers = pos.checkers();
    if !mv.is_drop() || checkers.count() != 1 {
        return false;
    }
    let checker = checkers.lsb().unwrap();
    let defender = pos.side_to_move();
    pos.do_move_with_missing_kings(mv);
    let captures: Vec<Move> = checks_by_rules(pos)
        .into_iter()
        .filter(|m| !m.is_drop() && m.from() == checker && m.to() == mv.to())
        .collect();
    let mut returned_len: Option<u32> = None;
    for cap in captures {
        pos.do_move_with_missing_kings(cap);
        let recapture = legal_moves_by_rules(pos).iter().any(|r| !r.is_drop() && r.to() == mv.to());
        let mut returned = with_piece_returned(pos, mv.drop_piece_type(), defender);
        pos.undo_move(cap);
        if recapture {
            continue;
        }
        let len = (0..=plies).step_by(2).find(|&n| ref_defense(&mut returned, n));
        returned_len = match (returned_len, len) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
    }
    pos.undo_move(mv);
    let Some(len) = returned_len else {
        return false;
    };
    if len == 0 {
        return true;
    }
    // 駒打ち以外の応手で、詰み手数が len − 1 以上 (len − 3 手以内に詰まない) のものがあるか
    legal_moves_by_rules(pos).into_iter().any(|r| {
        if r.is_drop() {
            return false;
        }
        if len < 4 {
            return true;
        }
        pos.do_move_with_missing_kings(r);
        let mated = ref_mate(pos, len - 3);
        pos.undo_move(r);
        !mated
    })
}

/// `pos` から攻め方の持駒の `pt` を 1 枚、受け方 `defender` の持駒に移した局面 (SFEN で作り直す)
fn with_piece_returned(pos: &Position, pt: PieceType, defender: Color) -> Position {
    let mut hands = [pos.hand(Color::Black), pos.hand(Color::White)];
    hands[(!defender).index()] = hands[(!defender).index()].sub(pt);
    hands[defender.index()] = hands[defender.index()].add(pt);
    let sfen = pos.to_sfen();
    let mut fields: Vec<String> = sfen.split_whitespace().map(str::to_string).collect();
    fields[2] = hand_sfen(hands);
    position(&fields.join(" "))
}

fn hand_sfen(hands: [Hand; 2]) -> String {
    const ORDER: [(PieceType, char); 7] = [
        (PieceType::Rook, 'R'),
        (PieceType::Bishop, 'B'),
        (PieceType::Gold, 'G'),
        (PieceType::Silver, 'S'),
        (PieceType::Knight, 'N'),
        (PieceType::Lance, 'L'),
        (PieceType::Pawn, 'P'),
    ];
    let mut out = String::new();
    for (i, hand) in hands.iter().enumerate() {
        for (pt, c) in ORDER {
            let n = hand.count(pt);
            if n > 1 {
                out.push_str(&n.to_string());
            }
            if n > 0 {
                out.push(if i == 0 { c } else { c.to_ascii_lowercase() });
            }
        }
    }
    if out.is_empty() { "-".to_string() } else { out }
}

/// 攻め方の手番の `pos` が、無駄合いを数えない手数で `plies` 手以内に詰むか
fn ref_mate(pos: &mut Position, plies: u32) -> bool {
    if plies == 0 {
        return false;
    }
    for mv in checks_by_rules(pos) {
        pos.do_move_with_missing_kings(mv);
        let mated = ref_defense(pos, plies - 1);
        pos.undo_move(mv);
        if mated {
            return true;
        }
    }
    false
}

/// 受け方の手番の `pos` が `plies` 手以内に詰むか (無駄合いは詰みとして数えない)
fn ref_defense(pos: &mut Position, plies: u32) -> bool {
    for mv in legal_moves_by_rules(pos) {
        if ref_is_futile(pos, mv, plies) {
            continue;
        }
        if plies == 0 {
            return false;
        }
        pos.do_move_with_missing_kings(mv);
        let mated = ref_mate(pos, plies - 1);
        pos.undo_move(mv);
        if !mated {
            return false;
        }
    }
    true
}

/// 最短の詰み手数 (`max` 手以内に詰まなければ `None`)
fn ref_len(pos: &mut Position, max: u32) -> Option<u32> {
    (1..=max).step_by(2).find(|&n| ref_mate(pos, n))
}

/// 詰み手数 `d` の局面から、両者が正解手順の候補を選んで詰上りで攻め方の持駒が空になる手順があるか
fn ref_clean(pos: &mut Position, d: u32, or_node: bool, attacker: Color) -> bool {
    if or_node {
        for mv in checks_by_rules(pos) {
            pos.do_move_with_missing_kings(mv);
            let ok = ref_defense(pos, d - 1) && ref_clean(pos, d - 1, false, attacker);
            pos.undo_move(mv);
            if ok {
                return true;
            }
        }
        return false;
    }
    if d == 0 {
        return pos.hand(attacker) == Hand::EMPTY;
    }
    for mv in legal_moves_by_rules(pos) {
        if ref_is_futile(pos, mv, d) {
            continue;
        }
        pos.do_move_with_missing_kings(mv);
        let longest = d < 4 || !ref_mate(pos, d - 3);
        let ok = longest && ref_clean(pos, d - 1, true, attacker);
        pos.undo_move(mv);
        if ok {
            return true;
        }
    }
    false
}

/// 手順の最後の局面で攻め方の持駒が空か
fn ends_clean(pos: &Position, pv: &[Move]) -> bool {
    let attacker = pos.side_to_move();
    let mut pos = pos.clone();
    pos.clear_checkers_without_king();
    for &mv in pv {
        pos.do_move_with_missing_kings(mv);
    }
    pos.hand(attacker) == Hand::EMPTY
}

/// 手順の各手が定義どおりか: 攻め方の手の後は残りの手数で詰み、受け方の手は無駄合いでなく残りの手数より
/// 短く詰まない (最長)。最後の局面の応手はすべて無駄合い
fn assert_pv_follows_definition(sfen: &str, pv: &[Move]) {
    let mut pos = position(sfen);
    pos.clear_checkers_without_king();
    let len = pv.len() as u32;
    for (i, &mv) in pv.iter().enumerate() {
        let rest = len - i as u32;
        if i % 2 == 1 {
            assert!(!ref_is_futile(&mut pos, mv, rest), "{sfen}: {} 手目は無駄合い", i + 1);
        }
        pos.do_move_with_missing_kings(mv);
        if i % 2 == 0 {
            assert!(ref_defense(&mut pos, rest - 1), "{sfen}: {} 手目の後に詰まない", i + 1);
        } else if rest >= 4 {
            assert!(!ref_mate(&mut pos, rest - 3), "{sfen}: {} 手目より長い応手がある", i + 1);
        }
    }
    for reply in legal_moves_by_rules(&mut pos) {
        assert!(
            ref_is_futile(&mut pos, reply, 0),
            "{sfen}: 詰上りに無駄合いでない応手 {}",
            reply.to_usi()
        );
    }
}

/// 最短手順モードの手順が正解役と同じ手数で、駒余りの有無も一致することを、両方のモードで確かめる
fn assert_matches_reference(sfen: &str, expected_len: u32) {
    let mut pos = position(sfen);
    pos.clear_checkers_without_king();
    assert_eq!(ref_len(&mut pos, expected_len), Some(expected_len), "{sfen}: 正解役の手数");
    let attacker = pos.side_to_move();
    let clean = ref_clean(&mut pos, expected_len, true, attacker);
    for mode in MateMode::ALL {
        let mut pos = position(sfen);
        let MateResult::Mate(pv) = solve_shortest(&mut pos, mode, 5_000_000) else {
            panic!("{sfen} {mode:?}: 詰みを返さない");
        };
        assert!(check_mate_pv_by_rules(&pos, &pv, true), "{sfen} {mode:?}: {}", pv_usi(&pv));
        assert_eq!(pv.len() as u32, expected_len, "{sfen} {mode:?}: {}", pv_usi(&pv));
        assert_eq!(ends_clean(&pos, &pv), clean, "{sfen} {mode:?}: 駒余り {}", pv_usi(&pv));
        assert_pv_follows_definition(sfen, &pv);
    }
}

// ---------------------------------------------------------------------------
// 手作りの局面
// ---------------------------------------------------------------------------

#[test]
fn futile_interpositions_are_not_counted() {
    // ▲9一飛まで。受け方の歩の合駒は、飛車が取ると同じ形で詰むので無駄合い
    let sfen = "8k/6S2/7G1/9/9/9/9/9/R3K4 b 2p 1";
    assert_matches_reference(sfen, 1);
    let mut pos = position(sfen);
    let MateResult::Mate(pv) = solve_shortest(&mut pos, MateMode::Power, 100_000) else {
        panic!("{sfen}");
    };
    // 成・不成のどちらでも詰む
    assert!(matches!(pv_usi(&pv).as_str(), "9i9a" | "9i9a+"), "{}", pv_usi(&pv));
    // 無駄合いを数えると 7 手より長い (受け方の歩の合駒を数える)
    let mut pos = position("8k/9/9/6R1p/9/9/9/9/9 b GSp 1");
    assert!(!brute_mate_by_rules(&mut pos, 5));
    assert_matches_reference("8k/9/9/6R1p/9/9/9/9/9 b GSp 1", 5);
}

#[test]
fn positions_without_interpositions_match_the_reference() {
    // 受け方が持駒を持たない局面 (総当たりが重くならないよう駒の少ない局面)
    for (sfen, len) in [
        ("4k4/9/4P4/9/9/9/9/9/9 b G 1", 1),
        ("9/4k1g2/2G3R2/9/4G4/9/9/9/K8 b R 1", 3),
        ("5k3/9/5gR2/9/9/9/9/9/5K3 b GS 1", 3),
        ("1k7/1s7/3R5/N8/9/9/9/9/9 b 2G 1", 5),
    ] {
        assert_matches_reference(sfen, len);
    }
}

#[test]
fn effective_interpositions_are_counted() {
    // 最短の王手の後に、無駄合いでない合駒が最長の応手としてある。2 局面目は、後手の竜の王手 △9一竜 への ▲9二香を
    // 竜で取ると玉に竜を取り返されるので有効な合駒
    for sfen in [
        "9/8k/9/6+R+L1/9/9/9/9/9 b LPsp 1",
        "8+r/9/K8/2+r6/9/9/9/9/9 w L2g 1",
    ] {
        assert_matches_reference(sfen, 3);
        let mut pos = position(sfen);
        pos.clear_checkers_without_king();
        let mut effective = false;
        for mv in checks_by_rules(&mut pos) {
            pos.do_move_with_missing_kings(mv);
            if ref_defense(&mut pos, 2) {
                effective |= legal_moves_by_rules(&mut pos)
                    .into_iter()
                    .any(|r| r.is_drop() && !ref_is_futile(&mut pos, r, 2));
            }
            pos.undo_move(mv);
        }
        assert!(effective, "{sfen}");
    }
}

/// 取り返されて詰みが長くなる合駒は有効な合駒として数える。
/// - 7 手詰: ▲6四竜への△4四歩などは ▲同竜 △同玉と竜を取り返されて詰みが長くなる (無駄合いとすると 5 手)
/// - 9 手詰 (2 局面): ▲3一飛 (▲7一飛) への唯一の応手△2一金 (△2一香) は ▲同飛成 △同玉と取り返される。
///   取り返された後の詰みの手数で受け方の局面の手数が決まるので、無駄合いにすると手順が作れない
#[test]
fn recaptured_interpositions_are_counted() {
    for (sfen, len) in [
        ("ll1S1gBs1/2sR3p1/8l/1p4k1p/P2N2p2/2P4K1/1P1P+n4/6G2/1r6L b BGSPg2n9p 1", 7),
        ("8k/7pp/9/6P2/5B3/9/9/9/9 b RGNg 1", 9),
        ("8k/7pp/9/9/5B3/9/9/9/9 b RGNl 1", 9),
    ] {
        for mode in MateMode::ALL {
            let mut pos = position(sfen);
            let MateResult::Mate(pv) = solve_shortest(&mut pos, mode, 5_000_000) else {
                panic!("{sfen} {mode:?}");
            };
            assert!(check_mate_pv_by_rules(&pos, &pv, true), "{sfen} {mode:?}: {}", pv_usi(&pv));
            assert_eq!(pv.len(), len, "{sfen} {mode:?}: {}", pv_usi(&pv));
        }
    }
}

#[test]
fn same_length_replies_prefer_no_leftover_pieces() {
    // 受け方の最長の応手のうち、詰上りで攻め方の持駒が余るものと余らないものがある
    for (sfen, len) in [
        ("9/1k7/9/2+B6/9/9/9/9/4K4 b GS 1", 3),
        ("9/8k/6G2/8B/9/9/9/9/2K6 b GS 1", 3),
        ("9/9/5k3/3+R5/6G+R1/9/9/9/8K b B 1", 5),
        ("8r/9/6k2/5L3/4+R1S2/7S1/9/9/9 b GSl 1", 5),
    ] {
        assert_matches_reference(sfen, len);
    }
}

/// 乱択局面で、最短手順モードの手数と駒余りの有無が正解役と一致する
#[test]
fn shortest_mode_matches_reference_on_random_positions() {
    let mut rng = Xoshiro256PlusPlus::seed_from_u64(0x5807_0001);
    let (mut tested, mut mates) = (0, 0);
    while tested < 300 {
        let pos = if tested % 2 == 0 {
            random_tsume_position(&mut rng)
        } else {
            random_kingless_position(&mut rng)
        };
        let Some(mut pos) = pos else { continue };
        tested += 1;
        pos.clear_checkers_without_king();
        let sfen = pos.to_sfen();
        let reference = ref_len(&mut pos, 5);
        for mode in MateMode::ALL {
            let mut pos = position(&sfen);
            match (solve_shortest(&mut pos, mode, 2_000_000), reference) {
                (MateResult::Mate(pv), Some(len)) => {
                    assert!(
                        check_mate_pv_by_rules(&pos, &pv, true),
                        "{sfen} {mode:?}: {}",
                        pv_usi(&pv)
                    );
                    assert_eq!(pv.len() as u32, len, "{sfen} {mode:?}: {}", pv_usi(&pv));
                    let mut root = position(&sfen);
                    root.clear_checkers_without_king();
                    let attacker = root.side_to_move();
                    let clean = ref_clean(&mut root, len, true, attacker);
                    assert_eq!(
                        ends_clean(&pos, &pv),
                        clean,
                        "{sfen} {mode:?}: 駒余り {}",
                        pv_usi(&pv)
                    );
                    assert_pv_follows_definition(&sfen, &pv);
                }
                (MateResult::Mate(pv), None) => {
                    assert!(
                        pv.len() > 5,
                        "{sfen} {mode:?}: 正解役は 5 手以内に詰まない {}",
                        pv_usi(&pv)
                    );
                }
                (other, Some(len)) => panic!("{sfen} {mode:?}: {len} 手で詰むのに {other:?}"),
                (_, None) => {}
            }
        }
        mates += usize::from(reference.is_some());
    }
    assert!(mates >= 30, "詰みの局面が少なすぎる: {mates}");
}

/// 作意推定モードは小さい局面では予算内に縮めきれるので、正解役と同じ手数の正しい手順を返す
#[test]
fn estimate_mode_matches_reference_on_random_positions() {
    let mut rng = Xoshiro256PlusPlus::seed_from_u64(0x5807_0003);
    let (mut tested, mut mates) = (0, 0);
    while tested < 300 {
        let pos = if tested % 2 == 0 {
            random_tsume_position(&mut rng)
        } else {
            random_kingless_position(&mut rng)
        };
        let Some(mut pos) = pos else { continue };
        tested += 1;
        pos.clear_checkers_without_king();
        let sfen = pos.to_sfen();
        let reference = ref_len(&mut pos, 5);
        for mode in MateMode::ALL {
            let mut pos = position(&sfen);
            match (solve_counting(&mut pos, mode, ShortestMode::Estimate, 2_000_000), reference) {
                (MateResult::Mate(pv), Some(len)) => {
                    assert!(
                        check_mate_pv_by_rules(&pos, &pv, true),
                        "{sfen} {mode:?}: {}",
                        pv_usi(&pv)
                    );
                    assert_eq!(pv.len() as u32, len, "{sfen} {mode:?}: {}", pv_usi(&pv));
                }
                (MateResult::Mate(pv), None) => {
                    assert!(
                        pv.len() > 5,
                        "{sfen} {mode:?}: 正解役は 5 手以内に詰まない {}",
                        pv_usi(&pv)
                    );
                }
                (other, Some(len)) => panic!("{sfen} {mode:?}: {len} 手で詰むのに {other:?}"),
                (_, None) => {}
            }
        }
        mates += usize::from(reference.is_some());
    }
    assert!(mates >= 30, "詰みの局面が少なすぎる: {mates}");
}

/// 並列に解いても (手順を求める段階は 1 スレッド)、1 スレッドと同じ手数になる
#[test]
fn shortest_mode_with_threads_matches_single_thread() {
    let mut rng = Xoshiro256PlusPlus::seed_from_u64(0x5807_0002);
    let mut mates = 0;
    while mates < 20 {
        let Some(mut pos) = random_tsume_position(&mut rng) else {
            continue;
        };
        let MateResult::Mate(single) = solve_shortest(&mut pos, MateMode::Power, 2_000_000) else {
            continue;
        };
        mates += 1;
        for mode in MateMode::ALL {
            let stop = AtomicBool::new(false);
            let mut solver = DfpnSolver::with_mode(16, mode);
            solver.set_shortest(Some(ShortestMode::Tsume));
            solver.set_threads(4);
            solver.parallel_from_the_start();
            let limits = MateLimits {
                nodes: 2_000_000,
                time_ms: 0,
            };
            let MateResult::Mate(pv) = solver.solve(&mut pos, &limits, &stop, |_| {}) else {
                panic!("{} {mode:?}", pos.to_sfen());
            };
            assert_eq!(pv.len(), single.len(), "{} {mode:?}: {}", pos.to_sfen(), pv_usi(&pv));
        }
    }
}

/// 手順を求める段階でノード数の上限に達したら Unknown で、局面は元に戻る
#[test]
fn shortest_mode_stops_at_the_node_limit() {
    // 判定は上限内に終わるが、最短手順を求める段階は上限を超える
    let sfen = "ll1S1gBs1/2sR3p1/8l/1p4k1p/P2N2p2/2P4K1/1P1P+n4/6G2/1r6L b BGSPg2n9p 1";
    let mut pos = position(sfen);
    let before = pos.to_sfen();
    let stop = AtomicBool::new(false);
    let limits = MateLimits {
        nodes: 20_000,
        time_ms: 0,
    };
    let judged = DfpnSolver::with_mode(16, MateMode::Power).solve(&mut pos, &limits, &stop, |_| {});
    assert!(matches!(judged, MateResult::Mate(_)), "判定は上限内に終わる: {judged:?}");
    assert_eq!(solve_shortest(&mut pos, MateMode::Power, 20_000), MateResult::Unknown);
    assert_eq!(pos.to_sfen(), before);
}

/// 制限を変えて解いても、正解手順 (手数と駒余りの有無) か Unknown のどちらかを返す。
/// 無駄合いの判定や駒余りの比較の途中で制限に達しても、確かめ切れていない手順を返さない
#[test]
fn shortest_mode_returns_the_solution_or_unknown_under_limits() {
    for (sfen, len) in [
        ("ll1S1gBs1/2sR3p1/8l/1p4k1p/P2N2p2/2P4K1/1P1P+n4/6G2/1r6L b BGSPg2n9p 1", 7),
        ("9/9/5k3/3+R5/6G+R1/9/9/9/8K b B 1", 5),
        ("8k/7pp/9/6P2/5B3/9/9/9/9 b RGNg 1", 9),
    ] {
        for mode in MateMode::ALL {
            let stop = AtomicBool::new(false);
            let mut solver = DfpnSolver::with_mode(16, mode);
            solver.set_shortest(Some(ShortestMode::Tsume));
            let mut pos = position(sfen);
            let MateResult::Mate(full) =
                solver.solve(&mut pos, &MateLimits::default(), &stop, |_| {})
            else {
                panic!("{sfen} {mode:?}");
            };
            assert_eq!(full.len(), len, "{sfen} {mode:?}: {}", pv_usi(&full));
            let clean = ends_clean(&pos, &full);
            let total = solver.nodes();
            for i in 1..16 {
                let limits = MateLimits {
                    nodes: total * i / 16,
                    time_ms: 0,
                };
                match solver.solve(&mut pos, &limits, &stop, |_| {}) {
                    MateResult::Mate(pv) => {
                        assert_eq!(pv.len(), len, "{sfen} {mode:?} {limits:?}: {}", pv_usi(&pv));
                        assert_eq!(ends_clean(&pos, &pv), clean, "{sfen} {mode:?} {limits:?}");
                    }
                    MateResult::Unknown => {}
                    other => panic!("{sfen} {mode:?} {limits:?}: {other:?}"),
                }
            }
        }
    }
}

/// 無駄合いを数えない詰みは、持駒違いの局面に流用される置換表に書かず、最短手順モードの表に局面の完全一致で
/// だけ書く (攻め方の持駒が多い局面では他の応手が短くなり、無駄合いだった合駒が有効になって長くなりうる)
#[test]
fn futility_counted_mates_are_not_shared_across_hands() {
    use super::search::{Budget, NodeArgs, Scratch, Searcher, ShortestTables};
    use super::state::MateState;
    use super::table::MateTable;
    use crate::time::Instant;

    // ▲9一飛の後: 受け方の応手は無駄合いの歩打ちだけなので、無駄合いを数えなければ 0 手で詰み
    let mut pos = position("8k/6S2/7G1/9/9/9/9/9/R3K4 b 2p 1");
    let attacker = pos.side_to_move();
    MateState::do_move(&mut pos, Move::from_usi("9i9a").unwrap());
    let stop = AtomicBool::new(false);
    let mut progress = |_| {};
    let budget = Budget {
        stop: &stop,
        halt: None,
        cancel: None,
        shared_nodes: None,
        node_limit: 0,
        time_limit_ms: 0,
        start: Instant::now(),
        progress: &mut progress,
    };
    let table = MateTable::new(1, true);
    let tables = ShortestTables::new();
    let mut scratch = Scratch::new();
    let mut searcher = Searcher::<_, true>::new(&table, &mut scratch, attacker, budget);
    searcher.enable_shortest(true, &tables);
    let result = searcher.search(&mut pos, NodeArgs::unbounded(0, false, 0), &mut 0).unwrap();
    assert!(result.is_proven());
    let key = pos.table_key(attacker);
    assert_eq!(tables.mate_len(key.key), Some(0));
    for hand in [key.hand, key.hand.add(PieceType::Gold)] {
        let probed = table.probe::<true>(super::table::TableKey { hand, ..key });
        assert!(probed.is_none_or(|v| v.pn != 0), "{hand:?}: {probed:?}");
    }
}

/// 合駒を取った駒を受け方が取り返せるなら、詰み手数が同じでも有効な合駒。▲9一竜への△2一歩は、▲同竜を
/// △同玉と取り返せる (取って歩を戻した局面は 2 手で詰み、合駒以外の△1二玉も 2 手で詰むが、無駄合いにしない)
#[test]
fn recapturable_interpositions_are_effective() {
    use super::search::{Budget, Scratch, Searcher, ShortestTables};
    use super::state::MateState;
    use super::table::MateTable;
    use crate::time::Instant;

    let sfen = "g7k/RP7/6G2/8P/9/9/9/9/4K4 b p 1";
    assert_matches_reference(sfen, 3);
    // 攻め方の玉が無くても同じ
    assert_matches_reference("g7k/RP7/6G2/8P/9/9/9/9/9 b p 1", 3);
    let mut pos = position(sfen);
    let attacker = pos.side_to_move();
    MateState::do_move(&mut pos, Move::from_usi("9b9a+").unwrap());
    let drop = Move::from_usi("P*2a").unwrap();
    let drop = legal_moves_by_rules(&mut pos)
        .into_iter()
        .find(|m| m.raw() == drop.raw())
        .unwrap();
    assert!(!ref_is_futile(&mut pos, drop, 2));

    let stop = AtomicBool::new(false);
    let mut progress = |_| {};
    let budget = Budget {
        stop: &stop,
        halt: None,
        cancel: None,
        shared_nodes: None,
        node_limit: 0,
        time_limit_ms: 0,
        start: Instant::now(),
        progress: &mut progress,
    };
    let table = MateTable::new(1, true);
    let tables = ShortestTables::new();
    let mut scratch = Scratch::new();
    let mut searcher = Searcher::<_, true>::new(&table, &mut scratch, attacker, budget);
    searcher.enable_shortest(true, &tables);
    assert_eq!(searcher.futile_drop(&mut pos, drop, 2).unwrap(), None);
}

/// 最短手順を求める段階 (無駄合いの判定で別の局面を解いている間も) も進捗を知らせ、ノード数は増え続ける
#[test]
fn shortest_mode_reports_monotonic_progress() {
    // 最短手順を求め終わるまで時間制限より十分長くかかる局面 (85 手詰) を、時間制限で打ち切る
    let mut pos = position(
        "1lnpGpBs1/lg1g3p1/p2+N4l/1S1P2k2/2Sn5/Pp2pn1K1/GPp+pR1pP1/+p2+ps3p/1B1LP1R2 b P 5",
    );
    let stop = AtomicBool::new(false);
    let limits = MateLimits {
        nodes: 0,
        time_ms: 3_500,
    };
    let mut solver = DfpnSolver::with_mode(16, MateMode::Power);
    solver.set_shortest(Some(ShortestMode::Tsume));
    let mut reported = Vec::new();
    solver.solve(&mut pos, &limits, &stop, |p| reported.push((p.elapsed_ms, p.nodes)));
    assert!(reported.len() >= 2, "{reported:?}");
    assert!(reported.windows(2).all(|w| w[0].1 < w[1].1), "{reported:?}");
    // 部分探索に入れ子で入っても、通知の間隔は探索全体で約 1 秒
    assert!(reported.windows(2).all(|w| w[1].0 >= w[0].0 + 900), "{reported:?}");
}

/// 無駄合いの判定で別の局面を解いている間も、進捗をこの探索のノード数に足して知らせる
#[test]
fn futility_checks_forward_progress() {
    use super::search::{Budget, NodeArgs, Scratch, Searcher, ShortestTables};
    use super::state::MateState;
    use super::table::MateTable;
    use crate::time::Instant;

    // 85 手詰の途中の受け方の局面。上限 20 手での合駒の判定で、合駒を取って戻した局面などを別に解く
    let mut pos = position(
        "1l1p1p3/l1k2+R3/p1gNS+P2+P/1S7/2Sn5/Pp2pn1K1/GPp+p3P1/+p2+ps3p/1B1LP4 w Lrb2gn2p 70",
    );
    let attacker = !pos.side_to_move();
    let stop = AtomicBool::new(false);
    let mut reported = Vec::new();
    let mut progress = |p: MateProgress| reported.push(p.nodes);
    // 開始を過去にして、最初に数えるノードで進捗を知らせる時刻を過ぎているようにする
    let start = Instant::now().checked_sub(std::time::Duration::from_secs(5)).unwrap();
    let budget = Budget {
        stop: &stop,
        halt: None,
        cancel: None,
        shared_nodes: None,
        node_limit: 0,
        time_limit_ms: 0,
        start,
        progress: &mut progress,
    };
    let table = MateTable::new(16, true);
    let tables = ShortestTables::new();
    let mut scratch = Scratch::new();
    let mut searcher = Searcher::<_, true>::new(&table, &mut scratch, attacker, budget);
    configure(&mut searcher, MateMode::Power);
    searcher.enable_shortest(true, &tables);
    // この探索で先にノードを数えておき、進捗がそれに部分探索の分を足した値になることを確かめる
    searcher.search(&mut pos, NodeArgs::unbounded(0, false, 0), &mut 0).unwrap();
    let before = searcher.nodes();
    assert!(before > 0);
    // 部分探索の最初に数えるノードで知らせる
    searcher.set_next_progress(0);
    let mut drops = Vec::new();
    MateState::generate(&pos, false, &mut drops);
    drops.retain(|m| m.is_drop());
    for mv in drops {
        searcher.futile_drop(&mut pos, mv, 20).unwrap();
    }
    let nodes = searcher.nodes();
    // 最初の通知はこの探索自身から、それより後は部分探索から (この探索で数えた分より多い)
    assert!(reported.len() >= 2, "{reported:?}");
    assert_eq!(reported[0], before);
    assert!(
        reported[1..].iter().all(|&n| before < n && n <= nodes),
        "{reported:?} / {before}..{nodes}"
    );
}

/// 全応手を数える最短手順は合駒も受け方の手として数えるので、詰将棋のルールより長くなりうる
#[test]
fn all_mode_counts_every_interposition() {
    for (sfen, tsume, all) in [
        // ▲1七飛の 1 筋の合駒はどれも同飛と取って詰む無駄合いだが、全応手では合駒を続けて逃れる
        (
            "l7k/r4+B1P1/p1ns1P1G1/5sp2/2Ppp4/1pp6/PP1PP1gpb/2GS+s3R/LNK6 b 3Pg2n2lp 117",
            1,
            11,
        ),
        ("8k/6S2/7G1/9/9/9/9/9/R3K4 b 2p 1", 1, 5),
        ("g7k/RP7/6G2/8P/9/9/9/9/4K4 b p 1", 3, 3),
        ("ll1S1gBs1/2sR3p1/8l/1p4k1p/P2N2p2/2P4K1/1P1P+n4/6G2/1r6L b BGSPg2n9p 1", 7, 7),
    ] {
        assert_eq!(shortest_len(sfen, ShortestMode::Tsume, 5_000_000), tsume, "{sfen}");
        assert_eq!(shortest_len(sfen, ShortestMode::All, 5_000_000), all, "{sfen}");
    }
}

/// 全応手を数える数え方では、無駄合いの判定 (別の局面を解く) をしない
#[test]
fn all_mode_does_not_search_futility() {
    use super::search::{Budget, Scratch, Searcher, ShortestTables};
    use super::state::MateState;
    use super::table::MateTable;
    use crate::time::Instant;

    // ▲9一飛の後の歩の合駒は、詰将棋のルールでは無駄合い
    let mut pos = position("8k/6S2/7G1/9/9/9/9/9/R3K4 b 2p 1");
    let attacker = pos.side_to_move();
    MateState::do_move(&mut pos, Move::from_usi("9i9a").unwrap());
    let drop = Move::from_usi("P*5a").unwrap();
    let drop = legal_moves_by_rules(&mut pos)
        .into_iter()
        .find(|m| m.raw() == drop.raw())
        .unwrap();
    let stop = AtomicBool::new(false);
    let mut progress = |_| {};
    let budget = Budget {
        stop: &stop,
        halt: None,
        cancel: None,
        shared_nodes: None,
        node_limit: 0,
        time_limit_ms: 0,
        start: Instant::now(),
        progress: &mut progress,
    };
    let table = MateTable::new(1, true);
    let tables = ShortestTables::new();
    let mut scratch = Scratch::new();
    let mut searcher = Searcher::<_, true>::new(&table, &mut scratch, attacker, budget);
    searcher.enable_shortest(false, &tables);
    assert_eq!(searcher.futile_drop(&mut pos, drop, 4).unwrap(), None);
    assert_eq!(searcher.nodes(), 0);
    searcher.enable_shortest(true, &tables);
    assert!(searcher.futile_drop(&mut pos, drop, 4).unwrap().is_some());
    assert!(searcher.nodes() > 0);
}

/// 乱択局面で、全応手を数える最短手順の手数が総当たり (すべての応手を数える) の最短手数と一致する
#[test]
fn all_mode_matches_brute_force_on_random_positions() {
    let mut rng = Xoshiro256PlusPlus::seed_from_u64(0x5807_0003);
    let (mut tested, mut mates) = (0, 0);
    while tested < 200 {
        let pos = if tested % 2 == 0 {
            random_tsume_position(&mut rng)
        } else {
            random_kingless_position(&mut rng)
        };
        let Some(mut pos) = pos else { continue };
        tested += 1;
        pos.clear_checkers_without_king();
        let sfen = pos.to_sfen();
        let brute = (1..=5).step_by(2).find(|&n| brute_mate_by_rules(&mut pos, n));
        for mode in MateMode::ALL {
            let mut pos = position(&sfen);
            match (solve_counting(&mut pos, mode, ShortestMode::All, 2_000_000), brute) {
                (MateResult::Mate(pv), Some(len)) => {
                    assert!(
                        check_mate_pv_by_rules(&pos, &pv, false),
                        "{sfen} {mode:?}: {}",
                        pv_usi(&pv)
                    );
                    assert_eq!(pv.len() as u32, len, "{sfen} {mode:?}: {}", pv_usi(&pv));
                }
                (MateResult::Mate(pv), None) => {
                    assert!(
                        pv.len() > 5,
                        "{sfen} {mode:?}: 総当たりは 5 手以内に詰まない {}",
                        pv_usi(&pv)
                    );
                }
                (other, Some(len)) => panic!("{sfen} {mode:?}: {len} 手で詰むのに {other:?}"),
                (_, None) => {}
            }
        }
        mates += usize::from(brute.is_some());
    }
    assert!(mates >= 20, "詰みの局面が少なすぎる: {mates}");
}

/// 全応手を数える手数は、詰将棋のルールの手数以上 (既知の長手数の局面。release で手動実行)
#[test]
#[ignore]
fn all_mode_is_not_shorter_than_tsume_on_long_mates() {
    let kingless = ZUKOU_001.replace("2N5K", "2N6");
    for sfen in [
        "2p1kS1nl/1+L1g2+P2/3p1p3/4p2p1/3+blNp2/+Rb4P2/3PP4/3G1G3/3K1L+r2 w g3s2n8p 128",
        "ll1S1gBs1/2sR3p1/8l/1p4k1p/P2N2p2/2P4K1/1P1P+n4/6G2/1r6L b BGSPg2n9p 1",
        "1lnpGpBs1/lg1g3p1/p2+N4l/1S1P2k2/2Sn5/Pp2pn1K1/GPp+pR1pP1/+p2+ps3p/1B1LP1R2 b P 5",
        ZUKOU_001,
        kingless.as_str(),
    ] {
        let tsume = shortest_len(sfen, ShortestMode::Tsume, 0);
        let all = shortest_len(sfen, ShortestMode::All, 0);
        assert!(all >= tsume, "{sfen}: 全応手 {all} / 詰将棋 {tsume}");
    }
}

/// 将棋図巧 第 1 番 (作意 69 手)。攻め方の玉がある既存の局面と、玉を除いた原図の両方 (release で手動実行)
#[test]
#[ignore]
fn solves_zukou_001_in_its_intended_length() {
    let kingless = ZUKOU_001.replace("2N5K", "2N6");
    for sfen in [ZUKOU_001, kingless.as_str()] {
        for mode in MateMode::ALL {
            let mut pos = position(sfen);
            let MateResult::Mate(pv) = solve_shortest(&mut pos, mode, 0) else {
                panic!("{sfen} {mode:?}");
            };
            assert!(check_mate_pv_by_rules(&pos, &pv, true), "{sfen} {mode:?}: {}", pv_usi(&pv));
            assert_eq!(pv.len(), 69, "{sfen} {mode:?}: {}", pv_usi(&pv));
        }
    }
}

/// 作意推定も将棋図巧 第 1 番で作意の手順を返す。推定は置換表に残った証明の手数を頼りに選ぶので、表が小さいと
/// 外れやすい (16 MB では受け方が短く詰む応手を選んだ 49 手になる) (release で手動実行)
#[test]
#[ignore]
fn estimate_mode_finds_zukou_001_intended_length() {
    let kingless = ZUKOU_001.replace("2N5K", "2N6");
    for sfen in [ZUKOU_001, kingless.as_str()] {
        for mode in MateMode::ALL {
            let mut pos = position(sfen);
            let stop = AtomicBool::new(false);
            let mut solver = DfpnSolver::with_mode(64, mode);
            solver.set_shortest(Some(ShortestMode::Estimate));
            let MateResult::Mate(pv) = solver.solve(
                &mut pos,
                &MateLimits {
                    nodes: 0,
                    time_ms: 0,
                },
                &stop,
                |_| {},
            ) else {
                panic!("{sfen} {mode:?}");
            };
            assert!(check_mate_pv_by_rules(&pos, &pv, true), "{sfen} {mode:?}: {}", pv_usi(&pv));
            assert_eq!(pv.len(), 69, "{sfen} {mode:?}: {}", pv_usi(&pv));
        }
    }
}
