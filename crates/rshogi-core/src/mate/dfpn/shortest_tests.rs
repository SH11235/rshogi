//! 最短手順モードのテスト
//!
//! 正解役は、正解手順の定義 (最短手数・受け方は最長・無駄合いは数えない・同手数なら駒余りの無い方) を
//! そのまま書いた総当たり。合法手は指し手生成を使わない [`legal_moves_by_rules`] で求める。

use std::sync::atomic::AtomicBool;

use rand::SeedableRng;
use rand_xoshiro::Xoshiro256PlusPlus;

use super::tests::{
    ZUKOU_001, brute_defense_by_rules, brute_mate_by_rules, checks_by_rules, position, pv_usi,
    random_kingless_position, random_tsume_position,
};
use super::verify::{check_mate_pv_by_rules, legal_moves_by_rules};
use super::*;
use crate::types::{Color, Hand, PieceType};

/// 無駄合いの判定で、合駒を取った後の局面が詰むかを総当たりで調べる手数
const FUTILE_PLIES: u32 = 6;

fn solve_shortest(pos: &mut Position, mode: MateMode, nodes: u64) -> MateResult {
    let stop = AtomicBool::new(false);
    let limits = MateLimits { nodes, time_ms: 0 };
    let mut solver = DfpnSolver::with_mode(16, mode);
    solver.set_shortest(true);
    solver.solve(pos, &limits, &stop, |_| {})
}

// ---------------------------------------------------------------------------
// 正解役 (総当たり)
// ---------------------------------------------------------------------------

/// 王手されている局面 `pos` の応手 `mv` が無駄合いか: 駒打ちの合駒を王手している駒が取り
/// (成・不成のどちらか)、取った駒を受け方の持駒に戻した局面が `FUTILE_PLIES` 手以内に詰む
fn ref_is_futile(pos: &mut Position, mv: Move) -> bool {
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
    let mut futile = false;
    for cap in captures {
        pos.do_move_with_missing_kings(cap);
        let mut returned = with_piece_returned(pos, mv.drop_piece_type(), defender);
        pos.undo_move(cap);
        if brute_defense_by_rules(&mut returned, FUTILE_PLIES) {
            futile = true;
            break;
        }
    }
    pos.undo_move(mv);
    futile
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
        if ref_is_futile(pos, mv) {
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
        if ref_is_futile(pos, mv) {
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
            assert!(!ref_is_futile(&mut pos, mv), "{sfen}: {} 手目は無駄合い", i + 1);
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
            ref_is_futile(&mut pos, reply),
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
    // 最短の王手の後に、無駄合いでない合駒 (取ると取った駒を使わなければ詰まない) が最長の応手としてある
    for sfen in ["9/8k/9/6+R+L1/9/9/9/9/9 b LPsp 1"] {
        assert_matches_reference(sfen, 3);
        let mut pos = position(sfen);
        pos.clear_checkers_without_king();
        let mut effective = false;
        for mv in checks_by_rules(&mut pos) {
            pos.do_move_with_missing_kings(mv);
            if ref_defense(&mut pos, 2) {
                effective |= legal_moves_by_rules(&mut pos)
                    .into_iter()
                    .any(|r| r.is_drop() && !ref_is_futile(&mut pos, r));
            }
            pos.undo_move(mv);
        }
        assert!(effective, "{sfen}");
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
            solver.set_shortest(true);
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
    // 判定は数千ノードで終わるが、無駄合いの判定で最短手順を求めるのに 100 万ノードほどかかる
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
