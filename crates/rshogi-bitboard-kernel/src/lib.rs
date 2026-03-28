//! SSE2-only ビットボード演算 kernel
//!
//! `attackers_to_occ` を、AVX を無効化した C コードで実装し、
//! レジスタ spill を削減する。
//!
//! ビルド時に `build.rs` が `kernel.c` を `-O3 -msse2 -mno-avx -mno-avx2 -fno-lto`
//! でコンパイルする。

/// Bitboard と同一レイアウトの 128bit 型
#[derive(Clone, Copy)]
#[repr(C, align(16))]
pub struct BB128 {
    pub p: [u64; 2],
}

/// Bitboard256 と同一レイアウトの 256bit 型。
#[derive(Clone, Copy)]
#[repr(C, align(32))]
pub struct BB256 {
    pub p: [u64; 4],
}

/// `attackers_to_occ` に必要な lookup table と Position フィールドをまとめた構造体。
/// C kernel に渡す。
#[repr(C)]
pub struct AttackersCtx {
    /// pawn_effect[2][81]
    pub pawn_effect: *const BB128,
    /// knight_effect[2][81]
    pub knight_effect: *const BB128,
    /// silver_effect[2][81]
    pub silver_effect: *const BB128,
    /// gold_effect[2][81]
    pub gold_effect: *const BB128,
    /// by_type[PieceType::NUM + 1]
    pub by_type: *const BB128,
    /// by_color[2]
    pub by_color: *const BB128,
    /// golds_bb
    pub golds_bb: *const BB128,
    /// hdk_bb
    pub hdk_bb: *const BB128,
    /// bishop_horse_bb
    pub bishop_horse_bb: *const BB128,
    /// rook_dragon_bb
    pub rook_dragon_bb: *const BB128,
    /// lance_step_effect[2][81]
    pub lance_step_effect: *const BB128,
    /// qugiy_rook_mask[81][2]
    pub qugiy_rook_mask: *const BB128,
    /// qugiy_bishop_mask[81][2]
    pub qugiy_bishop_mask: *const BB256,
}

extern "C" {
    /// `attackers_to_occ` 全体を SSE2 / scalar helper で計算する
    ///
    /// # Safety
    /// - `ctx` の全ポインタが有効で 16 バイトアラインされていること
    /// - `occupied` が有効な BB128 を指し 16 バイトアラインされていること
    /// - `sq` が 0..80 の範囲であること
    /// - `out` が有効な BB128 を指し 16 バイトアラインされていること
    pub fn attackers_to_occ_sse2(
        ctx: *const AttackersCtx,
        occupied: *const BB128,
        sq: u8,
        out: *mut BB128,
    );
}
