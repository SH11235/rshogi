//! df-pn のテスト
//!
//! - 既知の詰将棋 (cshogi の df-pn テストの局面) と、打ち歩詰め・不成・合駒・反復の局面
//! - 総当たりの AND/OR 探索 (短手数) を正解役にした乱択局面の突き合わせ
//! - 抽象的な AND/OR グラフでの GHI (経路依存の結果の誤再利用) と停滞の検査

use std::sync::atomic::AtomicBool;

use rand::{Rng, SeedableRng};
use rand_xoshiro::Xoshiro256PlusPlus;

use super::search::{Budget, MAX_DEPTH, Scratch, Searcher};
use super::state::MateState;
use super::table::MateTable;
use super::*;
use crate::movegen::{MoveList, generate_legal_all};
use crate::position::Position;
use crate::time::Instant;
use crate::types::Color;

fn position(sfen: &str) -> Position {
    let mut pos = Position::new();
    pos.set_sfen(sfen).unwrap_or_else(|e| panic!("{sfen}: {e:?}"));
    pos
}

fn solve_limited(pos: &mut Position, hash_mb: usize, nodes: u64) -> MateResult {
    let stop = AtomicBool::new(false);
    let limits = MateLimits { nodes, time_ms: 0 };
    DfpnSolver::new(hash_mb).solve(pos, &limits, &stop, |_| {})
}

fn pv_usi(pv: &[Move]) -> String {
    pv.iter().map(|m| m.to_usi()).collect::<Vec<_>>().join(" ")
}

/// 既知の詰みを解き、手順を検証する。`shortest` は最短の詰み手数 (不明なら 1)。
fn assert_mate(sfen: &str, shortest: usize) -> Vec<Move> {
    let mut pos = position(sfen);
    let before = pos.to_sfen();
    let pv = match solve_limited(&mut pos, 16, 5_000_000) {
        MateResult::Mate(pv) => pv,
        other => panic!("{sfen}: {other:?}"),
    };
    assert!(is_valid_mate_pv(&mut pos, &pv), "{sfen}: {}", pv_usi(&pv));
    // 最短は保証しないが、最短手数より短い手順は誤り
    assert!(pv.len() >= shortest, "{sfen}: {}", pv_usi(&pv));
    assert_eq!(pos.to_sfen(), before, "局面が元に戻っていない");
    pv
}

/// 詰み手順の受け方の各局面で、手順に選ばれなかった応手もすべて詰むことを、
/// 置換表を持ち越さない別のソルバーで確かめる (手順 1 本の検証では受け方の逃れを見落とす)。
fn assert_all_replies_are_mated(sfen: &str, pv: &[Move]) {
    let mut pos = position(sfen);
    for (i, &mv) in pv.iter().enumerate() {
        let gives_check = pos.gives_check(mv);
        pos.do_move(mv, gives_check);
        if i % 2 == 1 || i + 1 == pv.len() {
            continue;
        }
        // 受け方の手番: 手順の次の手 pv[i + 1] 以外の応手を調べる
        for reply in legal_moves(&pos) {
            if reply.raw() == pv[i + 1].raw() {
                continue;
            }
            let reply_gives_check = pos.gives_check(reply);
            pos.do_move(reply, reply_gives_check);
            let result = solve_limited(&mut pos, 16, 5_000_000);
            assert!(
                matches!(result, MateResult::Mate(_)),
                "{sfen}: {} の後の {} で逃れる ({result:?})",
                pv_usi(&pv[..=i]),
                reply.to_usi()
            );
            pos.undo_move(reply);
        }
    }
}

fn assert_no_mate(sfen: &str) {
    let mut pos = position(sfen);
    assert_eq!(solve_limited(&mut pos, 16, 5_000_000), MateResult::NoMate, "{sfen}");
}

// ---------------------------------------------------------------------------
// 総当たりの正解役
// ---------------------------------------------------------------------------

fn legal_moves(pos: &Position) -> Vec<Move> {
    let mut legal = MoveList::new();
    generate_legal_all(pos, &mut legal);
    legal.iter().copied().collect()
}

/// 攻め方の手 (合法な王手)。`allow_unpromote` が偽なら、同じ移動で成れる手がある不成を除く。
fn attack_moves(pos: &Position, allow_unpromote: bool) -> Vec<Move> {
    let checks: Vec<Move> = legal_moves(pos).into_iter().filter(|&m| pos.gives_check(m)).collect();
    if allow_unpromote {
        return checks;
    }
    let us = pos.side_to_move();
    let promotable = |m: Move| {
        !m.is_drop()
            && !m.is_promote()
            && pos.piece_on(m.from()).piece_type().can_promote()
            && crate::mate::can_promote(us, m.from(), m.to())
    };
    checks.into_iter().filter(|&m| !promotable(m)).collect()
}

/// `plies` 手以内 (奇数) に詰むか。打ち歩詰めの歩打ちは合法手生成が除外する。
/// 反復を経る詰みがあれば、反復を省いたより短い詰みもあるので、反復は考えなくてよい。
fn brute_mate(pos: &mut Position, plies: u32, allow_unpromote: bool) -> bool {
    if plies == 0 {
        return false;
    }
    for mv in attack_moves(pos, allow_unpromote) {
        pos.do_move(mv, true);
        let mated = brute_defense(pos, plies - 1, allow_unpromote);
        pos.undo_move(mv);
        if mated {
            return true;
        }
    }
    false
}

fn brute_defense(pos: &mut Position, plies: u32, allow_unpromote: bool) -> bool {
    let replies = legal_moves(pos);
    if replies.is_empty() {
        return true;
    }
    if plies == 0 {
        return false;
    }
    replies.into_iter().all(|mv| {
        let gives_check = pos.gives_check(mv);
        pos.do_move(mv, gives_check);
        let mated = brute_mate(pos, plies - 1, allow_unpromote);
        pos.undo_move(mv);
        mated
    })
}

// ---------------------------------------------------------------------------
// 既知の局面
// ---------------------------------------------------------------------------

/// cshogi の df-pn テストの局面 (SFEN, 最短の詰み手数。最後の局面は最短が不明なので 1)
const KNOWN_TSUME: [(&str, usize); 6] = [
    ("+B+R5n1/5gk2/p1pps1gp1/4ppnsK/6pP1/1PPSP3L/PR1P1PP2/6S2/L2G1G3 w B2N2LP2p 1", 3),
    (
        "ln1g3+Rl/2sk1s+P2/2ppppb1p/p1b3p2/8P/P4P3/2PPP1P2/1+r2GS3/LN+p2KGNL w GN2Ps 36",
        7,
    ),
    (
        "ln6l/4g1G2/2s1pk3/3p1s2p/p1P4b1/2pPPbp1P/PP1S3K1/3g2RP1/4+r2NL w N2Pgsnl3p 98",
        9,
    ),
    (
        "ln5kl/2r6/p1ps2+N1p/4p1pg1/3l3p1/P1P1PPP1P/1P1P1G3/2GS2+n2/+b2K1s2L b RGS2Pbn2p 101",
        9,
    ),
    (
        "l6nl/3kPs3/5p2p/p1ppg2P1/7+r1/P1G3p1P/1pNbSP1GL/2+n1S1K2/L1r3P2 w BGS3Pn2p 110",
        11,
    ),
    ("8l/1R2S1kgr/3pp2p1/p2nlpp1p/5ns2/2+BPl3P/1PNK1PP2/1GGS2S2/5G1NL b B3P3p 1", 1),
];

/// 将棋図巧 第 1 番 (69 手詰め)
const ZUKOU_001: &str = "1pG1B4/Gs+P6/pP7/n1ls5/3k5/nL4+r1b/1+p1p+R4/1S7/2N5K b SP2gn2l11p 1";

#[test]
fn solves_one_ply_mate() {
    // ▲5二金まで (5三の歩が金に紐を付ける)
    assert_mate("4k4/9/4P4/9/9/9/9/9/4K4 b G 1", 1);
}

#[test]
fn solves_known_tsume() {
    for (sfen, shortest) in KNOWN_TSUME {
        let pv = assert_mate(sfen, shortest);
        assert_all_replies_are_mated(sfen, &pv);
    }
}

#[test]
fn pawn_drop_mate_is_not_a_mate() {
    // ▲1二歩は打ち歩詰め (2三金が紐、2一は3二銀が利く)。他に詰みは無い。
    let sfen = "8k/6S2/7G1/9/9/9/9/9/K8 b P 1";
    let mut pos = position(sfen);
    assert!(!brute_mate(&mut pos, 5, true), "総当たりで詰みがある: {sfen}");
    assert_no_mate(sfen);
    // 持駒が金なら ▲1二金で詰む
    assert_mate("8k/6S2/7G1/9/9/9/9/9/K8 b G 1", 1);
}

#[test]
fn no_check_means_no_mate() {
    assert_no_mate("4k4/9/9/9/9/9/9/9/4K4 b - 1");
}

#[test]
fn defender_escapes_by_interposition() {
    // 香を打つ王手に後手は持駒の金で合駒でき (玉も逃げられる)、攻め方の持駒が尽きて詰まない
    let sfen = "4k4/9/9/9/9/9/9/9/K8 b Lg 1";
    let mut pos = position(sfen);
    assert!(!brute_mate(&mut pos, 5, true));
    assert_no_mate(sfen);
}

#[test]
fn attacker_in_check_is_solved_with_counter_checks() {
    // 攻め方が王手されている局面でも、王手を外しつつ王手をかける手から解く
    let sfen = "4k4/9/4P4/9/9/9/9/4r4/4K3G b G 1";
    let mut pos = position(sfen);
    assert!(pos.in_check());
    let expected = brute_mate(&mut pos, 7, true);
    match solve_limited(&mut pos, 16, 1_000_000) {
        MateResult::Mate(pv) => {
            assert!(is_valid_mate_pv(&mut pos, &pv), "{}", pv_usi(&pv));
        }
        MateResult::NoMate => assert!(!expected, "7 手以内に詰むのに NoMate"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn node_limit_gives_unknown_not_no_mate() {
    let (sfen, _) = KNOWN_TSUME[4];
    for nodes in [1, 2, 10, 100, 1_000] {
        let mut pos = position(sfen);
        let result = solve_limited(&mut pos, 1, nodes);
        assert!(!matches!(result, MateResult::NoMate), "nodes={nodes}: 詰む局面で NoMate");
    }
}

#[test]
fn stop_flag_set_before_solve_gives_unknown_even_for_trivial_positions() {
    // 1 ノードで不詰・1 手詰めと分かる局面でも、停止済みなら判定しない
    for sfen in [
        "4k4/9/9/9/9/9/9/9/4K4 b - 1",
        "4k4/9/4P4/9/9/9/9/9/4K4 b G 1",
    ] {
        let mut pos = position(sfen);
        let stop = AtomicBool::new(true);
        let result = DfpnSolver::new(1).solve(&mut pos, &MateLimits::default(), &stop, |_| {});
        assert_eq!(result, MateResult::Unknown, "{sfen}");
    }
}

#[test]
fn positions_without_a_king_are_unsupported() {
    // 攻め方の玉が無い詰将棋の局面。玉の位置が既定値のままなので王手の判定が狂う
    for sfen in [
        "4k4/8g/4P4/9/9/9/9/9/9 b G 1",
        "4k4/9/9/9/9/9/9/9/9 b R 1",
        "9/9/9/9/9/9/9/9/4K4 b R 1",
    ] {
        let mut pos = position(sfen);
        assert_eq!(solve_limited(&mut pos, 1, 1_000), MateResult::Unsupported, "{sfen}");
    }
}

#[test]
fn position_with_side_not_to_move_in_check_is_unsupported() {
    // 手番の先手の飛車が後手玉に利いている (後手が王手を放置した不正な局面)
    for sfen in [
        "4k4/9/9/9/4R4/9/9/9/4K4 b - 1",
        "4k4/9/9/9/4R4/9/9/9/4K4 b G 1",
    ] {
        let mut pos = position(sfen);
        assert_eq!(solve_limited(&mut pos, 1, 1_000), MateResult::Unsupported, "{sfen}");
    }
}

#[test]
fn stop_flag_gives_unknown() {
    let mut pos = position(ZUKOU_001);
    let stop = AtomicBool::new(true);
    let result = DfpnSolver::new(1).solve(&mut pos, &MateLimits::default(), &stop, |_| {});
    assert_eq!(result, MateResult::Unknown);
}

#[test]
fn time_limit_gives_unknown() {
    let mut pos = position(ZUKOU_001);
    let stop = AtomicBool::new(false);
    let limits = MateLimits {
        nodes: 0,
        time_ms: 50,
    };
    let start = Instant::now();
    let result = DfpnSolver::new(16).solve(&mut pos, &limits, &stop, |_| {});
    assert_eq!(result, MateResult::Unknown);
    assert!(start.elapsed().as_millis() < 2_000, "時間制限が効いていない");
}

#[test]
fn mate_pv_validator_rejects_bad_sequences() {
    let mut pos = position("4k4/9/4P4/9/9/9/9/9/4K4 b G 1");
    assert!(is_valid_mate_pv(&mut pos, &[Move::from_usi("G*5b").unwrap()]));
    // 王手でない手・詰まない手・空の手順は不正
    assert!(!is_valid_mate_pv(&mut pos, &[Move::from_usi("G*1a").unwrap()]));
    assert!(!is_valid_mate_pv(&mut pos, &[Move::from_usi("G*4b").unwrap()]));
    assert!(!is_valid_mate_pv(&mut pos, &[]));
}

// ---------------------------------------------------------------------------
// 手生成の同値
// ---------------------------------------------------------------------------

/// df-pn の手生成が「合法手のうち王手」「合法手すべて」と集合として一致する
#[test]
fn mate_move_generation_matches_legal_moves() {
    use crate::position::playout_test_support::RandomPlayout;
    let sorted = |moves: Vec<Move>| {
        let mut raw: Vec<u32> = moves.iter().map(|m| m.raw32()).collect();
        raw.sort_unstable();
        raw
    };
    let mut checked = 0;
    for index in 0..300 {
        let mut playout = RandomPlayout::new(0xD0F0, index);
        for _ in 0..160 {
            if playout.step().is_none() {
                break;
            }
            let pos = &playout.pos;
            let legal = legal_moves(pos);

            let mut attack = Vec::new();
            MateState::generate(pos, true, &mut attack);
            let expected: Vec<Move> =
                legal.iter().copied().filter(|&m| pos.gives_check(m)).collect();
            assert_eq!(sorted(attack), sorted(expected), "王手: {}", playout.describe());

            if pos.in_check() {
                let mut defense = Vec::new();
                MateState::generate(pos, false, &mut defense);
                assert_eq!(sorted(defense), sorted(legal), "応手: {}", playout.describe());
            }
            checked += 1;
        }
    }
    assert!(checked > 10_000);
}

// ---------------------------------------------------------------------------
// 乱択局面での総当たりとの突き合わせ
// ---------------------------------------------------------------------------

/// 玉の周りに数枚の駒を置いた詰将棋風の乱択局面 (攻め方 = 先手)
fn random_tsume_position(rng: &mut Xoshiro256PlusPlus) -> Option<Position> {
    const PIECES: [&str; 14] = [
        "P", "L", "N", "S", "G", "B", "R", "+P", "+L", "+N", "+S", "+B", "+R", "G",
    ];
    const HANDS: [&str; 12] = [
        "-", "G", "S", "P", "N", "L", "B", "R", "GS", "PL", "GG", "SN",
    ];
    let mut board = vec![vec![String::new(); 9]; 9];
    let kf = rng.random_range(0..9usize);
    let kr = rng.random_range(0..3usize);
    board[kr][kf] = "k".to_string();
    board[8][rng.random_range(0..9usize)] = "K".to_string();
    for _ in 0..rng.random_range(1..=4) {
        let f = (kf as i32 + rng.random_range(-2..=2)).clamp(0, 8) as usize;
        let r = (kr as i32 + rng.random_range(0..=3)).clamp(0, 8) as usize;
        if !board[r][f].is_empty() {
            continue;
        }
        let pc = PIECES[rng.random_range(0..PIECES.len())];
        let white = rng.random_range(0..4) == 0;
        board[r][f] = if white {
            pc.to_lowercase()
        } else {
            pc.to_string()
        };
    }
    let rows: Vec<String> = board
        .iter()
        .map(|row| {
            let mut out = String::new();
            let mut empty = 0;
            for cell in row {
                if cell.is_empty() {
                    empty += 1;
                    continue;
                }
                if empty > 0 {
                    out.push_str(&empty.to_string());
                    empty = 0;
                }
                out.push_str(cell);
            }
            if empty > 0 {
                out.push_str(&empty.to_string());
            }
            out
        })
        .collect();
    let hand = HANDS[rng.random_range(0..HANDS.len())];
    let sfen = format!("{} b {hand} 1", rows.join("/"));
    let mut pos = Position::new();
    pos.set_sfen(&sfen).ok()?;
    // 手番でない側 (後手) が王手されている局面・行き所のない駒・二歩は除く
    let white_king = pos.king_square(Color::White);
    if !pos.attackers_to_c(white_king, Color::Black).is_empty() || has_illegal_pieces(&pos) {
        return None;
    }
    Some(pos)
}

/// 行き所のない駒・二歩がある (乱択で置いた駒の検査)
fn has_illegal_pieces(pos: &Position) -> bool {
    use crate::types::PieceType;
    for color in [Color::Black, Color::White] {
        let mut pawn_files = [false; 9];
        for (pt, dead_ranks) in [
            (PieceType::Pawn, 1),
            (PieceType::Lance, 1),
            (PieceType::Knight, 2),
        ] {
            for sq in pos.pieces(color, pt).iter() {
                // 先手は 1 段目側、後手は 9 段目側が行き所のない段
                let rank = sq.rank().index();
                let from_edge = if color == Color::Black {
                    rank
                } else {
                    8 - rank
                };
                if from_edge < dead_ranks {
                    return true;
                }
                if pt == PieceType::Pawn {
                    let file = sq.file().index();
                    if pawn_files[file] {
                        return true;
                    }
                    pawn_files[file] = true;
                }
            }
        }
    }
    false
}

#[test]
fn agrees_with_brute_force_on_random_positions() {
    let mut rng = Xoshiro256PlusPlus::seed_from_u64(0x5EED_D0F0);
    let mut tested = 0;
    let mut mates = 0;
    let mut unknowns = 0;
    while tested < 400 {
        let Some(mut pos) = random_tsume_position(&mut rng) else {
            continue;
        };
        let sfen = pos.to_sfen();
        let brute = brute_mate(&mut pos, 5, true);
        match solve_limited(&mut pos, 1, 2_000_000) {
            MateResult::Mate(pv) => {
                assert!(is_valid_mate_pv(&mut pos, &pv), "{sfen}: {}", pv_usi(&pv));
            }
            MateResult::NoMate => assert!(!brute, "{sfen}: 5 手以内に詰むのに NoMate"),
            // 短手数の詰みは必ず見つかる。不詰側は深さ上限で判定できないことがある (飛車で玉を追い回す等)
            MateResult::Unknown | MateResult::Unsupported => {
                assert!(!brute, "{sfen}: 5 手以内に詰むのに Unknown");
                unknowns += 1;
            }
        }
        tested += 1;
        mates += usize::from(brute);
    }
    assert!(mates >= 20, "詰みの局面が少なすぎる: {mates}");
    assert!(unknowns * 20 <= tested, "判定できない局面が多すぎる: {unknowns}/{tested}");
}

#[test]
fn agrees_with_mate_1ply() {
    let mut rng = Xoshiro256PlusPlus::seed_from_u64(0x1_1A7E);
    let mut found = 0;
    let mut tested = 0;
    while tested < 1_000 {
        let Some(mut pos) = random_tsume_position(&mut rng) else {
            continue;
        };
        tested += 1;
        let mv = pos.mate_1ply();
        if mv == Move::NONE {
            continue;
        }
        found += 1;
        let MateResult::Mate(pv) = solve_limited(&mut pos, 1, 100_000) else {
            panic!("{}: mate_1ply={} なのに詰みを返さない", pos.to_sfen(), mv.to_usi());
        };
        assert_eq!(pv.len(), 1, "{}: {}", pos.to_sfen(), pv_usi(&pv));
    }
    assert!(found >= 20, "1 手詰めの局面が少なすぎる: {found}");
}

#[test]
fn finds_mates_that_need_unpromotion() {
    let mut rng = Xoshiro256PlusPlus::seed_from_u64(0xFADE);
    let mut found = 0;
    for _ in 0..20_000 {
        let Some(mut pos) = random_tsume_position(&mut rng) else {
            continue;
        };
        // 不成を許せば 3 手以内に詰み、成る手だけでは詰まない
        if brute_mate(&mut pos, 3, true) && !brute_mate(&mut pos, 3, false) {
            found += 1;
            let result = solve_limited(&mut pos, 1, 1_000_000);
            assert!(matches!(result, MateResult::Mate(_)), "{}", pos.to_sfen());
            if found >= 3 {
                break;
            }
        }
    }
    assert!(found > 0, "不成でしか詰まない局面が見つからない");
}

#[test]
fn table_size_does_not_contradict_the_answer() {
    let mut rng = Xoshiro256PlusPlus::seed_from_u64(0x7AB1E);
    let mut tested = 0;
    while tested < 100 {
        let Some(mut pos) = random_tsume_position(&mut rng) else {
            continue;
        };
        tested += 1;
        let sfen = pos.to_sfen();
        let big = solve_limited(&mut pos, 16, 1_000_000);
        let tiny = solve_tiny_table(&mut pos, 1_000_000);
        // 極小の表では打ち切り (Unknown) になりうるが、詰みと不詰が食い違ってはいけない
        let contradiction = matches!(
            (&big, &tiny),
            (MateResult::Mate(_), MateResult::NoMate) | (MateResult::NoMate, MateResult::Mate(_))
        );
        assert!(!contradiction, "{sfen}: {big:?} / {tiny:?}");
    }
}

/// 2 bucket しかない置換表で解く (置き換えが頻発する)
fn solve_tiny_table(pos: &mut Position, nodes: u64) -> MateResult {
    let mut table = MateTable::with_buckets(2);
    let stop = AtomicBool::new(false);
    let mut progress = |_| {};
    let budget = Budget {
        stop: &stop,
        node_limit: nodes,
        time_limit_ms: 0,
        start: Instant::now(),
        progress: &mut progress,
    };
    let mut scratch = Scratch::new();
    let mut searcher = Searcher::new(&mut table, &mut scratch, budget);
    match solve_with(&mut searcher, pos) {
        Some(SolveOutcome::Mate(pv)) => {
            assert!(is_valid_mate_pv(pos, &pv), "{}: {}", pos.to_sfen(), pv_usi(&pv));
            MateResult::Mate(pv)
        }
        Some(SolveOutcome::NoMate) => MateResult::NoMate,
        None => MateResult::Unknown,
    }
}

// ---------------------------------------------------------------------------
// 抽象 AND/OR グラフでの GHI と停滞の検査
// ---------------------------------------------------------------------------

/// 有向グラフ上の局面。偶数手目が OR (攻め方)、奇数手目が AND (受け方)。
/// 子の無い AND ノードが詰み、子の無い OR ノードが不詰。
struct Graph {
    edges: Vec<Vec<u32>>,
    stack: Vec<u32>,
}

impl Graph {
    fn new(edges: Vec<Vec<u32>>, root: u32) -> Self {
        Self {
            edges,
            stack: vec![root],
        }
    }

    fn node(&self) -> u32 {
        *self.stack.last().unwrap()
    }

    /// 最小不動点で求めた正解 (OR = いずれかの子が詰み、AND = すべての子が詰み)
    fn mate_set(&self, is_or: &[bool]) -> Vec<bool> {
        let n = self.edges.len();
        let mut mate = vec![false; n];
        loop {
            let mut changed = false;
            for v in 0..n {
                if mate[v] {
                    continue;
                }
                let kids = &self.edges[v];
                let now = if is_or[v] {
                    kids.iter().any(|&c| mate[c as usize])
                } else {
                    kids.iter().all(|&c| mate[c as usize])
                };
                if now {
                    mate[v] = true;
                    changed = true;
                }
            }
            if !changed {
                return mate;
            }
        }
    }
}

fn node_key(v: u32) -> u64 {
    (u64::from(v) + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15)
}

impl MateState for Graph {
    type Mv = u32;

    fn key(&self) -> u64 {
        node_key(self.node())
    }

    fn key_after(&self, m: u32) -> u64 {
        node_key(m)
    }

    fn generate(&self, _or_node: bool, out: &mut Vec<u32>) {
        out.extend_from_slice(&self.edges[self.node() as usize]);
    }

    fn mate_1ply(&mut self) -> Option<u32> {
        None
    }

    fn do_move(&mut self, m: u32) {
        self.stack.push(m);
    }

    fn undo_move(&mut self, _m: u32) {
        self.stack.pop();
    }
}

fn solve_graph(graph: &mut Graph, buckets: usize) -> Option<SolveOutcome<u32>> {
    solve_graph_with_limit(graph, buckets, 2_000_000)
}

fn solve_graph_with_limit(
    graph: &mut Graph,
    buckets: usize,
    node_limit: u64,
) -> Option<SolveOutcome<u32>> {
    let mut table = MateTable::with_buckets(buckets);
    let stop = AtomicBool::new(false);
    let mut progress = |_| {};
    let budget = Budget {
        stop: &stop,
        node_limit,
        time_limit_ms: 0,
        start: Instant::now(),
        progress: &mut progress,
    };
    let mut scratch = Scratch::new();
    let mut searcher = Searcher::new(&mut table, &mut scratch, budget);
    solve_with(&mut searcher, graph)
}

/// グラフ上の詰み手順を検証する: 辺に沿い、同じ節点に戻らず、子の無い AND で終わる
fn is_valid_graph_pv(graph: &Graph, root: u32, pv: &[u32]) -> bool {
    let mut seen = vec![root];
    let mut v = root;
    for &next in pv {
        if !graph.edges[v as usize].contains(&next) || seen.contains(&next) {
            return false;
        }
        seen.push(next);
        v = next;
    }
    pv.len() % 2 == 1 && graph.edges[v as usize].is_empty()
}

/// R(OR) → D(AND) → {A, X}(OR)、A → {B(AND), M(詰み)}、B → X、X → C(AND)、C → A。
/// A→B→X→C→A の循環を先に読むと X はその経路では不詰になるが、X 自体は X→C→A→M で詰む。
/// 経路依存の不詰を置換表に残すと、D の子 X を不詰と誤ってルートを NoMate にしてしまう。
#[test]
fn path_dependent_disproof_is_not_reused() {
    // 0:R 1:D 2:A 3:X 4:B 5:M 6:C
    let edges = vec![
        vec![1],
        vec![2, 3],
        vec![4, 5],
        vec![6],
        vec![3],
        vec![],
        vec![2],
    ];
    for buckets in [1, 2, 64] {
        let mut graph = Graph::new(edges.clone(), 0);
        let Some(SolveOutcome::Mate(pv)) = solve_graph(&mut graph, buckets) else {
            panic!("buckets={buckets}: 詰みを返さない");
        };
        assert!(is_valid_graph_pv(&graph, 0, &pv), "{pv:?}");
    }
}

/// 乱択グラフで最小不動点と一致し、打ち切り (停滞) も起きない
#[test]
fn agrees_with_fixed_point_on_random_graphs() {
    let mut rng = Xoshiro256PlusPlus::seed_from_u64(0x6A9F);
    let mut mates = 0;
    for case in 0..3_000 {
        // 節点 0..k が OR、k..2k が AND
        let k = rng.random_range(2..=8usize);
        let mut edges = vec![Vec::new(); 2 * k];
        let is_or: Vec<bool> = (0..2 * k).map(|v| v < k).collect();
        for (v, kids) in edges.iter_mut().enumerate() {
            for _ in 0..rng.random_range(0..=3) {
                let target = if v < k {
                    rng.random_range(k..2 * k)
                } else {
                    rng.random_range(0..k)
                };
                if !kids.contains(&(target as u32)) {
                    kids.push(target as u32);
                }
            }
        }
        let truth = Graph::new(edges.clone(), 0).mate_set(&is_or)[0];
        mates += usize::from(truth);
        for buckets in [1, 4, 256] {
            let mut graph = Graph::new(edges.clone(), 0);
            match solve_graph(&mut graph, buckets) {
                Some(SolveOutcome::Mate(pv)) => {
                    assert!(truth, "case {case} b={buckets}: 偽の詰み {edges:?}");
                    assert!(is_valid_graph_pv(&graph, 0, &pv), "case {case}: {pv:?} {edges:?}");
                }
                Some(SolveOutcome::NoMate) => {
                    assert!(!truth, "case {case} b={buckets}: 偽の不詰 {edges:?}");
                }
                None => panic!("case {case} b={buckets}: 小さなグラフで打ち切り (停滞) {edges:?}"),
            }
        }
    }
    assert!(mates > 100);
}

/// 長さ `len` の一本道 (OR → AND → ...)。`mate_at_end` なら末端は詰みの AND 終端、
/// そうでなければ不詰の OR 終端 (`len` の偶奇で決まるので一致を確かめる)
fn chain_graph(len: usize, mate_at_end: bool) -> Vec<Vec<u32>> {
    let mut edges: Vec<Vec<u32>> = (0..len).map(|v| vec![v as u32 + 1]).collect();
    // 節点 len は len が奇数なら AND (子なし = 詰み)、偶数なら OR (子なし = 不詰)
    assert_eq!(len % 2 == 1, mate_at_end);
    edges.push(Vec::new());
    edges
}

/// 深さの上限を超える一本道: 詰みが上限の先にあれば Unknown、不詰が上限の先にあっても Unknown
/// (上限で打ち切った手順を根拠に不詰とは言わない)
#[test]
fn depth_limit_gives_unknown_not_no_mate() {
    std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(|| {
            for mate_at_end in [true, false] {
                let len = MAX_DEPTH + 11 - usize::from(!mate_at_end);
                let mut graph = Graph::new(chain_graph(len, mate_at_end), 0);
                assert!(
                    solve_graph_with_limit(&mut graph, 1024, 0).is_none(),
                    "mate_at_end={mate_at_end}: 上限の先を読まずに結論を出した"
                );
            }
            // 上限より浅い一本道は解ける
            let mut graph = Graph::new(chain_graph(101, true), 0);
            let Some(SolveOutcome::Mate(pv)) = solve_graph_with_limit(&mut graph, 1024, 0) else {
                panic!("101 手の一本道が詰まない");
            };
            assert_eq!(pv.len(), 101);
            let mut graph = Graph::new(chain_graph(100, false), 0);
            assert!(matches!(
                solve_graph_with_limit(&mut graph, 1024, 0),
                Some(SolveOutcome::NoMate)
            ));
        })
        .unwrap()
        .join()
        .unwrap();
}

/// 深さの上限を超える枝があっても、別の枝の短い詰みは見つける
#[test]
fn deep_branch_does_not_hide_a_short_mate() {
    std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(|| {
            // 節点 0 (OR) → {1 (AND、深い一本道の入口), short (AND) }。short → s1 (OR) → s2 (AND、詰み)
            let mut edges = chain_graph(MAX_DEPTH + 10, false);
            let short = edges.len() as u32;
            edges[0].push(short);
            edges.push(vec![short + 1]);
            edges.push(vec![short + 2]);
            edges.push(Vec::new());
            let mut graph = Graph::new(edges, 0);
            let Some(SolveOutcome::Mate(pv)) = solve_graph_with_limit(&mut graph, 1024, 0) else {
                panic!("短い詰みを見つけられない");
            };
            assert_eq!(pv, vec![short, short + 1, short + 2]);
        })
        .unwrap()
        .join()
        .unwrap();
}

// ---------------------------------------------------------------------------
// 速度の計測 (release で手動実行)
// ---------------------------------------------------------------------------

/// 既知の詰将棋の解答時間・ノード数・nodes/s を出す
#[test]
#[ignore]
fn report_known_tsume_speed() {
    let stop = AtomicBool::new(false);
    let mut solver = DfpnSolver::new(256);
    let cases = KNOWN_TSUME.iter().copied().chain([(ZUKOU_001, 69)]);
    for (sfen, shortest) in cases {
        let mut pos = position(sfen);
        let start = Instant::now();
        let limits = MateLimits {
            nodes: 0,
            time_ms: 60_000,
        };
        let result = solver.solve(&mut pos, &limits, &stop, |_| {});
        let ms = start.elapsed().as_millis().max(1) as u64;
        let summary = match &result {
            MateResult::Mate(pv) => format!("mate {} 手 (最短 {shortest})", pv.len()),
            other => format!("{other:?}"),
        };
        println!(
            "{summary}  {ms} ms  nodes {}  {} knps  {sfen}",
            solver.nodes(),
            solver.nodes() / ms
        );
    }
}

/// 局面集を解いた結果の内訳・ノード数・時間を出す。
///
/// `DFPN_BENCH_SFEN` に 1 行 1 局面の SFEN ファイル (`#` で始まる行は読み飛ばす) を渡す。
/// 未指定なら詰将棋風の乱択局面を使う。各局面を手番そのままと、手番を反転した局面
/// (詰めろ判定の使い方) の両方で解く。`DFPN_BENCH_TIME_MS` は 1 局面の制限時間。
/// `DFPN_BENCH_OUT` を指定すると局面ごとの結果 (SFEN, 結果, 手順, ノード数, μs) を TSV で書く。
#[test]
#[ignore]
fn bench_positions() {
    let time_ms: u64 = std::env::var("DFPN_BENCH_TIME_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1000);
    let lines: Vec<String> = match std::env::var("DFPN_BENCH_SFEN") {
        Ok(path) => std::fs::read_to_string(&path)
            .expect("局面ファイルを読めない")
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(|l| l.strip_prefix("sfen ").unwrap_or(l).to_string())
            .collect(),
        Err(_) => {
            let mut rng = Xoshiro256PlusPlus::seed_from_u64(0xBE7C);
            let mut lines = Vec::new();
            while lines.len() < 2_000 {
                if let Some(pos) = random_tsume_position(&mut rng) {
                    let mut checks = Vec::new();
                    MateState::generate(&pos, true, &mut checks);
                    if !checks.is_empty() {
                        lines.push(pos.to_sfen());
                    }
                }
            }
            lines
        }
    };
    let mut sfens = Vec::new();
    for sfen in lines {
        if !position(&sfen).in_check() {
            let flipped = if sfen.contains(" b ") {
                sfen.replacen(" b ", " w ", 1)
            } else {
                sfen.replacen(" w ", " b ", 1)
            };
            sfens.push(flipped);
        }
        sfens.push(sfen);
    }

    let stop = AtomicBool::new(false);
    let mut solver = DfpnSolver::new(64);
    let limits = MateLimits { nodes: 0, time_ms };
    let (mut mate, mut nomate, mut unknown) = (0, 0, 0);
    let mut nodes = 0u64;
    let mut solved_ms = Vec::new();
    let mut rows = Vec::new();
    let start = Instant::now();
    for sfen in &sfens {
        let mut pos = position(sfen);
        let one = Instant::now();
        let result = solver.solve(&mut pos, &limits, &stop, |_| {});
        let micros = one.elapsed().as_micros() as u64;
        nodes += solver.nodes();
        let (tag, pv) = match &result {
            MateResult::Mate(pv) => {
                mate += 1;
                ("mate", pv_usi(pv))
            }
            MateResult::NoMate => {
                nomate += 1;
                ("nomate", String::new())
            }
            MateResult::Unknown | MateResult::Unsupported => {
                unknown += 1;
                ("unknown", String::new())
            }
        };
        if matches!(result, MateResult::Mate(_) | MateResult::NoMate) {
            solved_ms.push(micros);
        }
        rows.push(format!("{sfen}	{tag}	{pv}	{}	{micros}", solver.nodes()));
    }
    if let Ok(out) = std::env::var("DFPN_BENCH_OUT") {
        std::fs::write(
            &out,
            rows.join(
                "
",
            ) + "
",
        )
        .expect("結果を書けない");
    }
    let elapsed = start.elapsed().as_millis().max(1) as u64;
    solved_ms.sort_unstable();
    let pct = |p: usize| solved_ms.get(solved_ms.len() * p / 100).copied().unwrap_or(0);
    println!(
        "局面 {} (反転を含む)、制限 {time_ms} ms: mate {mate} / nomate {nomate} / unknown {unknown}",
        sfens.len()
    );
    println!(
        "合計 {elapsed} ms、nodes {nodes}、{} knps。判定できた局面の時間 (μs): 中央値 {} / 95% {} / 99% {} / 最大 {}",
        nodes / elapsed,
        pct(50),
        pct(95),
        pct(99),
        solved_ms.last().copied().unwrap_or(0)
    );
}
