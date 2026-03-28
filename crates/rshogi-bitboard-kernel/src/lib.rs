//! SSE2-only ビットボード演算 kernel
//!
//! `attackers_to_occ` の近接駒部分を、AVX を無効化した C コードで実装し、
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

/// 近接駒効きの lookup table と Position フィールドをまとめた構造体。
/// C kernel に渡す。
#[repr(C)]
pub struct NearCtx {
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
}

extern "C" {
    /// 近接駒の攻め駒を SSE2 で計算する
    ///
    /// # Safety
    /// - `ctx` の全ポインタが有効で 16 バイトアラインされていること
    /// - `sq` が 0..80 の範囲であること
    /// - `out` が有効な BB128 を指し 16 バイトアラインされていること
    pub fn attackers_near_pieces_sse2(ctx: *const NearCtx, sq: u8, out: *mut BB128);
}
