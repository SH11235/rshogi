use engine_core::shogi::Move;
use engine_core::Position;

fn main() {
    let sfen = "l5knl/1r1g2gb1/p1npppspp/2P3p2/1p7/4P3P/PPNP1PPP1/2S1K2R1/L1G1G1SNL b Sbp 16";
    let pos = Position::from_sfen(sfen).expect("Failed to parse SFEN");

    println!("Testing zobrist hash for 7d7c vs 7d7c+");
    println!("Initial position hash: {:016x}", pos.zobrist_hash);

    // 7d7c (no promotion)
    let move_no_promote = Move::from_usi("7d7c").expect("Failed to parse 7d7c");
    let mut pos_no_promote = pos.clone();
    pos_no_promote.do_move(move_no_promote);
    println!("\n7d7c (no promotion):");
    println!("  Move: {}", engine_core::usi::move_to_usi(&move_no_promote));
    println!("  Promote flag: {}", move_no_promote.is_promote());
    println!("  Hash: {:016x}", pos_no_promote.zobrist_hash);

    // 7d7c+ (promotion)
    let move_promote = Move::from_usi("7d7c+").expect("Failed to parse 7d7c+");
    let mut pos_promote = pos.clone();
    pos_promote.do_move(move_promote);
    println!("\n7d7c+ (promotion):");
    println!("  Move: {}", engine_core::usi::move_to_usi(&move_promote));
    println!("  Promote flag: {}", move_promote.is_promote());
    println!("  Hash: {:016x}", pos_promote.zobrist_hash);

    println!("\nHash comparison:");
    if pos_no_promote.zobrist_hash == pos_promote.zobrist_hash {
        println!("  ❌ SAME HASH - BUG DETECTED!");
        std::process::exit(1);
    } else {
        println!("  ✓ Different hashes - correct behavior");
        println!("  Difference: {:016x}", pos_no_promote.zobrist_hash ^ pos_promote.zobrist_hash);
    }
}
