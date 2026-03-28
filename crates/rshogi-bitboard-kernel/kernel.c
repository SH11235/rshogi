/*
 * SSE2-only attackers_to_occ kernel
 *
 * ビルドフラグ: -O3 -msse2 -mno-avx -mno-avx2 -fno-lto
 */

#include <emmintrin.h>
#include <stdint.h>

typedef struct __attribute__((aligned(16))) {
    uint64_t p[2];
} BB128;

typedef struct __attribute__((aligned(32))) {
    uint64_t p[4];
} BB256;

/*
 * `attackers_to_occ` に必要な lookup table 群と Position フィールド。
 * Rust 側の `AttackersCtx` とレイアウトを一致させる。
 */
typedef struct {
    const BB128 (*pawn_effect)[81];
    const BB128 (*knight_effect)[81];
    const BB128 (*silver_effect)[81];
    const BB128 (*gold_effect)[81];
    const BB128 *by_type;
    const BB128 *by_color;
    const BB128 *golds_bb;
    const BB128 *hdk_bb;
    const BB128 *bishop_horse_bb;
    const BB128 *rook_dragon_bb;
    const BB128 (*lance_step_effect)[81];
    const BB128 (*qugiy_rook_mask)[2];
    const BB256 (*qugiy_bishop_mask)[2];
} AttackersCtx;

/* Piece type indices (must match rshogi-core PieceType enum) */
#define PT_PAWN   1
#define PT_LANCE  2
#define PT_KNIGHT 3
#define PT_SILVER 4

static inline BB128 bb128_from_u64_pair(uint64_t p0, uint64_t p1)
{
    BB128 bb = {{p0, p1}};
    return bb;
}

static inline BB128 bb128_and(BB128 lhs, BB128 rhs)
{
    return bb128_from_u64_pair(lhs.p[0] & rhs.p[0], lhs.p[1] & rhs.p[1]);
}

static inline BB128 bb128_or(BB128 lhs, BB128 rhs)
{
    return bb128_from_u64_pair(lhs.p[0] | rhs.p[0], lhs.p[1] | rhs.p[1]);
}

static inline BB128 bb128_xor(BB128 lhs, BB128 rhs)
{
    return bb128_from_u64_pair(lhs.p[0] ^ rhs.p[0], lhs.p[1] ^ rhs.p[1]);
}

static inline BB128 bb128_byte_reverse(BB128 bb)
{
    return bb128_from_u64_pair(__builtin_bswap64(bb.p[1]), __builtin_bswap64(bb.p[0]));
}

static inline void bb128_unpack(BB128 hi_in, BB128 lo_in, BB128 *hi_out, BB128 *lo_out)
{
    *hi_out = bb128_from_u64_pair(lo_in.p[1], hi_in.p[1]);
    *lo_out = bb128_from_u64_pair(lo_in.p[0], hi_in.p[0]);
}

static inline void bb128_decrement_pair(
    BB128 hi_in,
    BB128 lo_in,
    BB128 *hi_out,
    BB128 *lo_out)
{
    *hi_out = bb128_from_u64_pair(
        hi_in.p[0] + (lo_in.p[0] == 0 ? UINT64_MAX : 0),
        hi_in.p[1] + (lo_in.p[1] == 0 ? UINT64_MAX : 0));
    *lo_out = bb128_from_u64_pair(lo_in.p[0] - 1, lo_in.p[1] - 1);
}

static inline BB256 bb256_from_u64_array(
    uint64_t p0,
    uint64_t p1,
    uint64_t p2,
    uint64_t p3)
{
    BB256 bb = {{p0, p1, p2, p3}};
    return bb;
}

static inline BB256 bb256_new(BB128 bb)
{
    return bb256_from_u64_array(bb.p[0], bb.p[1], bb.p[0], bb.p[1]);
}

static inline BB256 bb256_and(BB256 lhs, BB256 rhs)
{
    return bb256_from_u64_array(
        lhs.p[0] & rhs.p[0],
        lhs.p[1] & rhs.p[1],
        lhs.p[2] & rhs.p[2],
        lhs.p[3] & rhs.p[3]);
}

static inline BB256 bb256_or(BB256 lhs, BB256 rhs)
{
    return bb256_from_u64_array(
        lhs.p[0] | rhs.p[0],
        lhs.p[1] | rhs.p[1],
        lhs.p[2] | rhs.p[2],
        lhs.p[3] | rhs.p[3]);
}

static inline BB256 bb256_xor(BB256 lhs, BB256 rhs)
{
    return bb256_from_u64_array(
        lhs.p[0] ^ rhs.p[0],
        lhs.p[1] ^ rhs.p[1],
        lhs.p[2] ^ rhs.p[2],
        lhs.p[3] ^ rhs.p[3]);
}

static inline BB256 bb256_byte_reverse(BB256 bb)
{
    return bb256_from_u64_array(
        __builtin_bswap64(bb.p[1]),
        __builtin_bswap64(bb.p[0]),
        __builtin_bswap64(bb.p[3]),
        __builtin_bswap64(bb.p[2]));
}

static inline void bb256_unpack(BB256 hi_in, BB256 lo_in, BB256 *hi_out, BB256 *lo_out)
{
    *hi_out = bb256_from_u64_array(lo_in.p[1], hi_in.p[1], lo_in.p[3], hi_in.p[3]);
    *lo_out = bb256_from_u64_array(lo_in.p[0], hi_in.p[0], lo_in.p[2], hi_in.p[2]);
}

static inline void bb256_decrement_pair(
    BB256 hi_in,
    BB256 lo_in,
    BB256 *hi_out,
    BB256 *lo_out)
{
    *hi_out = bb256_from_u64_array(
        hi_in.p[0] + (lo_in.p[0] == 0 ? UINT64_MAX : 0),
        hi_in.p[1] + (lo_in.p[1] == 0 ? UINT64_MAX : 0),
        hi_in.p[2] + (lo_in.p[2] == 0 ? UINT64_MAX : 0),
        hi_in.p[3] + (lo_in.p[3] == 0 ? UINT64_MAX : 0));
    *lo_out = bb256_from_u64_array(
        lo_in.p[0] - 1,
        lo_in.p[1] - 1,
        lo_in.p[2] - 1,
        lo_in.p[3] - 1);
}

static inline BB128 bb256_merge(BB256 bb)
{
    return bb128_from_u64_pair(bb.p[0] | bb.p[2], bb.p[1] | bb.p[3]);
}

static inline uint32_t msb64(uint64_t x)
{
    return x == 0 ? 0u : 63u - (uint32_t)__builtin_clzll(x);
}

static inline BB128 rook_file_effect(const AttackersCtx *ctx, uint8_t sq, BB128 occupied)
{
    if (sq < 63) {
        const uint64_t mask = ctx->lance_step_effect[1][sq].p[0];
        const uint64_t em = occupied.p[0] & mask;
        const uint64_t t = em - 1;

        const uint64_t se = ctx->lance_step_effect[0][sq].p[0];
        const uint64_t mocc = se & occupied.p[0];
        const uint64_t up = UINT64_MAX << msb64(mocc | 1);

        return bb128_from_u64_pair(((em ^ t) & mask) | (up & se), 0);
    }

    const uint64_t mask = ctx->lance_step_effect[1][sq].p[1];
    const uint64_t em = occupied.p[1] & mask;
    const uint64_t t = em - 1;

    const uint64_t se = ctx->lance_step_effect[0][sq].p[1];
    const uint64_t mocc = se & occupied.p[1];
    const uint64_t up = UINT64_MAX << msb64(mocc | 1);

    return bb128_from_u64_pair(0, ((em ^ t) & mask) | (up & se));
}

static inline BB128 rook_rank_effect(const AttackersCtx *ctx, uint8_t sq, BB128 occupied)
{
    const BB128 mask_lo = ctx->qugiy_rook_mask[sq][0];
    const BB128 mask_hi = ctx->qugiy_rook_mask[sq][1];
    const BB128 rocc = bb128_byte_reverse(occupied);
    BB128 hi;
    BB128 lo;
    BB128 t1;
    BB128 t0;

    bb128_unpack(rocc, occupied, &hi, &lo);
    hi = bb128_and(hi, mask_hi);
    lo = bb128_and(lo, mask_lo);
    bb128_decrement_pair(hi, lo, &t1, &t0);
    t1 = bb128_and(bb128_xor(t1, hi), mask_hi);
    t0 = bb128_and(bb128_xor(t0, lo), mask_lo);
    bb128_unpack(t1, t0, &hi, &lo);

    return bb128_or(bb128_byte_reverse(hi), lo);
}

static inline BB128 rook_effect(const AttackersCtx *ctx, uint8_t sq, BB128 occupied)
{
    return bb128_or(rook_rank_effect(ctx, sq, occupied), rook_file_effect(ctx, sq, occupied));
}

static inline BB128 bishop_effect(const AttackersCtx *ctx, uint8_t sq, BB128 occupied)
{
    const BB256 mask_lo = ctx->qugiy_bishop_mask[sq][0];
    const BB256 mask_hi = ctx->qugiy_bishop_mask[sq][1];
    const BB256 occ2 = bb256_new(occupied);
    const BB256 rocc2 = bb256_new(bb128_byte_reverse(occupied));
    BB256 hi;
    BB256 lo;
    BB256 t1;
    BB256 t0;

    bb256_unpack(rocc2, occ2, &hi, &lo);
    hi = bb256_and(hi, mask_hi);
    lo = bb256_and(lo, mask_lo);
    bb256_decrement_pair(hi, lo, &t1, &t0);
    t1 = bb256_and(bb256_xor(t1, hi), mask_hi);
    t0 = bb256_and(bb256_xor(t0, lo), mask_lo);
    bb256_unpack(t1, t0, &hi, &lo);

    return bb256_merge(bb256_or(bb256_byte_reverse(hi), lo));
}

static inline BB128 compute_near_attackers(const AttackersCtx *ctx, uint8_t sq)
{
    const __m128i pawn = _mm_load_si128((const __m128i *)&ctx->by_type[PT_PAWN]);
    const __m128i knight = _mm_load_si128((const __m128i *)&ctx->by_type[PT_KNIGHT]);
    const __m128i hdk = _mm_load_si128((const __m128i *)ctx->hdk_bb);
    const __m128i golds = _mm_load_si128((const __m128i *)ctx->golds_bb);
    const __m128i black = _mm_load_si128((const __m128i *)&ctx->by_color[0]);
    const __m128i white = _mm_load_si128((const __m128i *)&ctx->by_color[1]);
    const __m128i silver_hdk = _mm_or_si128(
        _mm_load_si128((const __m128i *)&ctx->by_type[PT_SILVER]), hdk);
    const __m128i golds_hdk = _mm_or_si128(golds, hdk);
    const __m128i black_att = _mm_and_si128(
        _mm_or_si128(
            _mm_or_si128(
                _mm_and_si128(_mm_load_si128((const __m128i *)&ctx->pawn_effect[1][sq]), pawn),
                _mm_and_si128(_mm_load_si128((const __m128i *)&ctx->knight_effect[1][sq]), knight)),
            _mm_or_si128(
                _mm_and_si128(_mm_load_si128((const __m128i *)&ctx->silver_effect[1][sq]), silver_hdk),
                _mm_and_si128(_mm_load_si128((const __m128i *)&ctx->gold_effect[1][sq]), golds_hdk))),
        black);
    const __m128i white_att = _mm_and_si128(
        _mm_or_si128(
            _mm_or_si128(
                _mm_and_si128(_mm_load_si128((const __m128i *)&ctx->pawn_effect[0][sq]), pawn),
                _mm_and_si128(_mm_load_si128((const __m128i *)&ctx->knight_effect[0][sq]), knight)),
            _mm_or_si128(
                _mm_and_si128(_mm_load_si128((const __m128i *)&ctx->silver_effect[0][sq]), silver_hdk),
                _mm_and_si128(_mm_load_si128((const __m128i *)&ctx->gold_effect[0][sq]), golds_hdk))),
        white);
    BB128 out;
    _mm_store_si128((__m128i *)&out, _mm_or_si128(black_att, white_att));
    return out;
}

__attribute__((noinline, visibility("hidden")))
void attackers_to_occ_sse2(
    const AttackersCtx * __restrict ctx,
    const BB128 * __restrict occupied,
    uint8_t sq,
    BB128 * __restrict out)
{
    const BB128 near = compute_near_attackers(ctx, sq);
    const BB128 bishop = bb128_and(bishop_effect(ctx, sq, *occupied), *ctx->bishop_horse_bb);
    const BB128 rook_eff = rook_effect(ctx, sq, *occupied);
    const BB128 lances = bb128_or(
        bb128_and(ctx->lance_step_effect[1][sq], bb128_and(ctx->by_type[PT_LANCE], ctx->by_color[0])),
        bb128_and(ctx->lance_step_effect[0][sq], bb128_and(ctx->by_type[PT_LANCE], ctx->by_color[1])));
    const BB128 rook_lance = bb128_and(rook_eff, bb128_or(*ctx->rook_dragon_bb, lances));

    *out = bb128_or(bb128_or(near, bishop), rook_lance);
}
