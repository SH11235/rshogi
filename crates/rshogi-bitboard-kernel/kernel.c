/*
 * SSE2-only attackers_to_occ / see_ge kernel
 *
 * ビルドフラグ: -O3 -msse2 -mno-avx -mno-avx2 -fno-lto
 */

#include <emmintrin.h>
#include <stdbool.h>
#include <stdint.h>

typedef struct __attribute__((aligned(16))) {
    uint64_t p[2];
} BB128;

typedef struct __attribute__((aligned(32))) {
    uint64_t p[4];
} BB256;

typedef struct {
    const BB128 (*pawn_effect)[81];
    const BB128 (*knight_effect)[81];
    const BB128 (*silver_effect)[81];
    const BB128 (*gold_effect)[81];
    const BB128 (*lance_step_effect)[81];
    const BB128 (*qugiy_rook_mask)[2];
    const BB256 (*qugiy_bishop_mask)[2];
    const BB128 *by_type;
    const BB128 *by_color;
    const BB128 *golds_bb;
    const BB128 *hdk_bb;
    const BB128 *bishop_horse_bb;
    const BB128 *rook_dragon_bb;
    const BB128 (*qugiy_step_effect)[81];
} AttackersCtx;

/* Piece type indices (must match rshogi-core PieceType enum) */
#define PT_PAWN 1
#define PT_LANCE 2
#define PT_KNIGHT 3
#define PT_SILVER 4
#define PT_BISHOP 5
#define PT_ROOK 6
#define PT_GOLD 7
#define PT_KING 8
#define PT_PRO_PAWN 9
#define PT_PRO_LANCE 10
#define PT_PRO_KNIGHT 11
#define PT_PRO_SILVER 12
#define PT_HORSE 13
#define PT_DRAGON 14

/* Direct enum (must match rshogi-core Direct) */
#define DIRECT_RU 0
#define DIRECT_R 1
#define DIRECT_RD 2
#define DIRECT_U 3
#define DIRECT_D 4
#define DIRECT_LU 5
#define DIRECT_L 6
#define DIRECT_LD 7

static inline BB128 bb128_from_u64_pair(uint64_t p0, uint64_t p1)
{
    BB128 bb = {{p0, p1}};
    return bb;
}

static inline bool bb128_is_empty(BB128 bb)
{
    return (bb.p[0] | bb.p[1]) == 0;
}

static inline BB128 bb128_and(BB128 lhs, BB128 rhs)
{
    return bb128_from_u64_pair(lhs.p[0] & rhs.p[0], lhs.p[1] & rhs.p[1]);
}

static inline BB128 bb128_andnot(BB128 lhs, BB128 rhs)
{
    return bb128_from_u64_pair(lhs.p[0] & ~rhs.p[0], lhs.p[1] & ~rhs.p[1]);
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

static inline BB128 bb128_decrement(BB128 bb)
{
    return bb128_from_u64_pair(
        bb.p[0] - 1,
        bb.p[0] == 0 ? bb.p[1] - 1 : bb.p[1]);
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

static inline BB128 bb128_from_square(uint8_t sq)
{
    if (sq < 63) {
        return bb128_from_u64_pair(1ULL << sq, 0);
    }

    return bb128_from_u64_pair(0, 1ULL << (sq - 63));
}

static inline uint8_t bb128_lsb(BB128 bb)
{
    if (bb.p[0] != 0) {
        return (uint8_t)__builtin_ctzll(bb.p[0]);
    }

    return (uint8_t)(63 + __builtin_ctzll(bb.p[1]));
}

static inline void bb256_set(
    BB256 *out,
    uint64_t p0,
    uint64_t p1,
    uint64_t p2,
    uint64_t p3)
{
    out->p[0] = p0;
    out->p[1] = p1;
    out->p[2] = p2;
    out->p[3] = p3;
}

static inline void bb256_from_bb128(BB256 *out, BB128 bb)
{
    bb256_set(out, bb.p[0], bb.p[1], bb.p[0], bb.p[1]);
}

static inline void bb256_and(const BB256 *lhs, const BB256 *rhs, BB256 *out)
{
    bb256_set(
        out,
        lhs->p[0] & rhs->p[0],
        lhs->p[1] & rhs->p[1],
        lhs->p[2] & rhs->p[2],
        lhs->p[3] & rhs->p[3]);
}

static inline void bb256_or(const BB256 *lhs, const BB256 *rhs, BB256 *out)
{
    bb256_set(
        out,
        lhs->p[0] | rhs->p[0],
        lhs->p[1] | rhs->p[1],
        lhs->p[2] | rhs->p[2],
        lhs->p[3] | rhs->p[3]);
}

static inline void bb256_xor(const BB256 *lhs, const BB256 *rhs, BB256 *out)
{
    bb256_set(
        out,
        lhs->p[0] ^ rhs->p[0],
        lhs->p[1] ^ rhs->p[1],
        lhs->p[2] ^ rhs->p[2],
        lhs->p[3] ^ rhs->p[3]);
}

static inline void bb256_byte_reverse(const BB256 *bb, BB256 *out)
{
    bb256_set(
        out,
        __builtin_bswap64(bb->p[1]),
        __builtin_bswap64(bb->p[0]),
        __builtin_bswap64(bb->p[3]),
        __builtin_bswap64(bb->p[2]));
}

static inline void bb256_unpack(const BB256 *hi_in, const BB256 *lo_in, BB256 *hi_out, BB256 *lo_out)
{
    bb256_set(hi_out, lo_in->p[1], hi_in->p[1], lo_in->p[3], hi_in->p[3]);
    bb256_set(lo_out, lo_in->p[0], hi_in->p[0], lo_in->p[2], hi_in->p[2]);
}

static inline void bb256_decrement_pair(
    const BB256 *hi_in,
    const BB256 *lo_in,
    BB256 *hi_out,
    BB256 *lo_out)
{
    bb256_set(
        hi_out,
        hi_in->p[0] + (lo_in->p[0] == 0 ? UINT64_MAX : 0),
        hi_in->p[1] + (lo_in->p[1] == 0 ? UINT64_MAX : 0),
        hi_in->p[2] + (lo_in->p[2] == 0 ? UINT64_MAX : 0),
        hi_in->p[3] + (lo_in->p[3] == 0 ? UINT64_MAX : 0));
    bb256_set(
        lo_out,
        lo_in->p[0] - 1,
        lo_in->p[1] - 1,
        lo_in->p[2] - 1,
        lo_in->p[3] - 1);
}

static inline void bb256_merge(const BB256 *bb, BB128 *out)
{
    *out = bb128_from_u64_pair(bb->p[0] | bb->p[2], bb->p[1] | bb->p[3]);
}

static inline uint32_t msb64(uint64_t x)
{
    return x == 0 ? 0u : 63u - (uint32_t)__builtin_clzll(x);
}

static inline BB128 lance_effect_color(const AttackersCtx *ctx, uint8_t color, uint8_t sq, BB128 occupied)
{
    const BB128 se = ctx->lance_step_effect[color][sq];

    if (color == 1) {
        if (sq < 63) {
            const uint64_t mask = se.p[0];
            const uint64_t em = occupied.p[0] & mask;
            const uint64_t t = em - 1;
            return bb128_from_u64_pair((em ^ t) & mask, 0);
        }

        const uint64_t mask = se.p[1];
        const uint64_t em = occupied.p[1] & mask;
        const uint64_t t = em - 1;
        return bb128_from_u64_pair(0, (em ^ t) & mask);
    }

    if (sq < 63) {
        const uint64_t mask = se.p[0];
        const uint64_t mocc = mask & occupied.p[0];
        const uint64_t up = UINT64_MAX << msb64(mocc | 1);
        return bb128_from_u64_pair(up & mask, 0);
    }

    const uint64_t mask = se.p[1];
    const uint64_t mocc = mask & occupied.p[1];
    const uint64_t up = UINT64_MAX << msb64(mocc | 1);
    return bb128_from_u64_pair(0, up & mask);
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
    const BB256 *mask_lo = &ctx->qugiy_bishop_mask[sq][0];
    const BB256 *mask_hi = &ctx->qugiy_bishop_mask[sq][1];
    BB256 occ2;
    BB256 rocc2;
    BB256 hi;
    BB256 lo;
    BB256 t1;
    BB256 t0;
    BB256 tmp;
    BB128 merged;

    bb256_from_bb128(&occ2, occupied);
    bb256_from_bb128(&rocc2, bb128_byte_reverse(occupied));
    bb256_unpack(&rocc2, &occ2, &hi, &lo);
    bb256_and(&hi, mask_hi, &hi);
    bb256_and(&lo, mask_lo, &lo);
    bb256_decrement_pair(&hi, &lo, &t1, &t0);
    bb256_xor(&t1, &hi, &t1);
    bb256_and(&t1, mask_hi, &t1);
    bb256_xor(&t0, &lo, &t0);
    bb256_and(&t0, mask_lo, &t0);
    bb256_unpack(&t1, &t0, &hi, &lo);
    bb256_byte_reverse(&hi, &tmp);
    bb256_or(&tmp, &lo, &tmp);
    bb256_merge(&tmp, &merged);

    return merged;
}

static inline BB128 ray_effect_dir(const AttackersCtx *ctx, uint8_t dir, uint8_t sq, BB128 occupied)
{
    switch (dir) {
    case DIRECT_U:
        return lance_effect_color(ctx, 0, sq, occupied);
    case DIRECT_D:
        return lance_effect_color(ctx, 1, sq, occupied);
    default: {
        uint8_t idx;
        bool reverse;
        BB128 mask;
        BB128 bb;

        switch (dir) {
        case DIRECT_RU:
            idx = 0;
            reverse = true;
            break;
        case DIRECT_R:
            idx = 1;
            reverse = true;
            break;
        case DIRECT_RD:
            idx = 2;
            reverse = true;
            break;
        case DIRECT_LU:
            idx = 3;
            reverse = false;
            break;
        case DIRECT_L:
            idx = 4;
            reverse = false;
            break;
        case DIRECT_LD:
        default:
            idx = 5;
            reverse = false;
            break;
        }

        mask = ctx->qugiy_step_effect[idx][sq];
        bb = reverse ? bb128_byte_reverse(occupied) : occupied;
        bb = bb128_and(bb, mask);
        bb = bb128_and(bb128_xor(bb, bb128_decrement(bb)), mask);
        return reverse ? bb128_byte_reverse(bb) : bb;
    }
    }
}

static inline int8_t direct_of(uint8_t sq1, uint8_t sq2)
{
    const int file1 = sq1 / 9;
    const int rank1 = sq1 % 9;
    const int file2 = sq2 / 9;
    const int rank2 = sq2 % 9;
    const int df = file2 - file1;
    const int dr = rank2 - rank1;

    if (df == 0) {
        if (dr < 0) {
            return DIRECT_U;
        }
        if (dr > 0) {
            return DIRECT_D;
        }
        return -1;
    }

    if (dr == 0) {
        return df < 0 ? DIRECT_R : DIRECT_L;
    }

    if ((df * df) == (dr * dr)) {
        if (df < 0 && dr < 0) {
            return DIRECT_RU;
        }
        if (df < 0 && dr > 0) {
            return DIRECT_RD;
        }
        if (df > 0 && dr < 0) {
            return DIRECT_LU;
        }
        if (df > 0 && dr > 0) {
            return DIRECT_LD;
        }
    }

    return -1;
}

static inline int32_t see_piece_value_raw(uint8_t pt)
{
    switch (pt) {
    case PT_PAWN:
        return 90;
    case PT_LANCE:
        return 315;
    case PT_KNIGHT:
        return 405;
    case PT_SILVER:
        return 495;
    case PT_GOLD:
    case PT_PRO_PAWN:
    case PT_PRO_LANCE:
    case PT_PRO_KNIGHT:
    case PT_PRO_SILVER:
        return 540;
    case PT_BISHOP:
        return 855;
    case PT_HORSE:
        return 945;
    case PT_ROOK:
        return 990;
    case PT_DRAGON:
        return 1395;
    case PT_KING:
        return 15000;
    default:
        return 0;
    }
}

static inline __m128i bb128_load(const BB128 *bb)
{
    return _mm_load_si128((const __m128i *)bb);
}

static inline BB128 compute_near_attackers(const AttackersCtx *ctx, uint8_t sq)
{
    const __m128i pawn = bb128_load(&ctx->by_type[PT_PAWN]);
    const __m128i knight = bb128_load(&ctx->by_type[PT_KNIGHT]);
    const __m128i hdk = bb128_load(ctx->hdk_bb);
    const __m128i golds = bb128_load(ctx->golds_bb);
    const __m128i black = bb128_load(&ctx->by_color[0]);
    const __m128i white = bb128_load(&ctx->by_color[1]);
    const __m128i silver_hdk = _mm_or_si128(bb128_load(&ctx->by_type[PT_SILVER]), hdk);
    const __m128i golds_hdk = _mm_or_si128(golds, hdk);
    const __m128i black_att = _mm_and_si128(
        _mm_or_si128(
            _mm_or_si128(
                _mm_and_si128(bb128_load(&ctx->pawn_effect[1][sq]), pawn),
                _mm_and_si128(bb128_load(&ctx->knight_effect[1][sq]), knight)),
            _mm_or_si128(
                _mm_and_si128(bb128_load(&ctx->silver_effect[1][sq]), silver_hdk),
                _mm_and_si128(bb128_load(&ctx->gold_effect[1][sq]), golds_hdk))),
        black);
    const __m128i white_att = _mm_and_si128(
        _mm_or_si128(
            _mm_or_si128(
                _mm_and_si128(bb128_load(&ctx->pawn_effect[0][sq]), pawn),
                _mm_and_si128(bb128_load(&ctx->knight_effect[0][sq]), knight)),
            _mm_or_si128(
                _mm_and_si128(bb128_load(&ctx->silver_effect[0][sq]), silver_hdk),
                _mm_and_si128(bb128_load(&ctx->gold_effect[0][sq]), golds_hdk))),
        white);
    BB128 out;
    _mm_store_si128((__m128i *)&out, _mm_or_si128(black_att, white_att));
    return out;
}

static inline BB128 attackers_to_occ_impl(const AttackersCtx *ctx, uint8_t sq, BB128 occupied)
{
    const BB128 near = compute_near_attackers(ctx, sq);
    const BB128 bishop = bb128_and(bishop_effect(ctx, sq, occupied), *ctx->bishop_horse_bb);
    const BB128 rook_eff = rook_effect(ctx, sq, occupied);
    const BB128 lances = bb128_or(
        bb128_and(ctx->lance_step_effect[1][sq], bb128_and(ctx->by_type[PT_LANCE], ctx->by_color[0])),
        bb128_and(ctx->lance_step_effect[0][sq], bb128_and(ctx->by_type[PT_LANCE], ctx->by_color[1])));
    const BB128 rook_lance = bb128_and(rook_eff, bb128_or(*ctx->rook_dragon_bb, lances));

    return bb128_or(bb128_or(near, bishop), rook_lance);
}

static inline void least_valuable_attacker(
    const AttackersCtx *ctx,
    BB128 attackers,
    uint8_t stm,
    uint8_t *sq_out,
    int32_t *value_out)
{
    BB128 bb = bb128_and(attackers, ctx->by_color[stm]);

    bb = bb128_and(bb, ctx->by_type[PT_PAWN]);
    if (!bb128_is_empty(bb)) {
        *sq_out = bb128_lsb(bb);
        *value_out = see_piece_value_raw(PT_PAWN);
        return;
    }

    bb = bb128_and(attackers, ctx->by_color[stm]);
    bb = bb128_and(bb, ctx->by_type[PT_LANCE]);
    if (!bb128_is_empty(bb)) {
        *sq_out = bb128_lsb(bb);
        *value_out = see_piece_value_raw(PT_LANCE);
        return;
    }

    bb = bb128_and(attackers, ctx->by_color[stm]);
    bb = bb128_and(bb, ctx->by_type[PT_KNIGHT]);
    if (!bb128_is_empty(bb)) {
        *sq_out = bb128_lsb(bb);
        *value_out = see_piece_value_raw(PT_KNIGHT);
        return;
    }

    bb = bb128_and(attackers, ctx->by_color[stm]);
    bb = bb128_and(bb, ctx->by_type[PT_SILVER]);
    if (!bb128_is_empty(bb)) {
        *sq_out = bb128_lsb(bb);
        *value_out = see_piece_value_raw(PT_SILVER);
        return;
    }

    bb = bb128_and(attackers, ctx->by_color[stm]);
    bb = bb128_and(
        bb,
        bb128_or(
            bb128_or(ctx->by_type[PT_GOLD], ctx->by_type[PT_PRO_PAWN]),
            bb128_or(
                bb128_or(ctx->by_type[PT_PRO_LANCE], ctx->by_type[PT_PRO_KNIGHT]),
                ctx->by_type[PT_PRO_SILVER])));
    if (!bb128_is_empty(bb)) {
        *sq_out = bb128_lsb(bb);
        *value_out = see_piece_value_raw(PT_GOLD);
        return;
    }

    bb = bb128_and(attackers, ctx->by_color[stm]);
    bb = bb128_and(bb, ctx->by_type[PT_BISHOP]);
    if (!bb128_is_empty(bb)) {
        *sq_out = bb128_lsb(bb);
        *value_out = see_piece_value_raw(PT_BISHOP);
        return;
    }

    bb = bb128_and(attackers, ctx->by_color[stm]);
    bb = bb128_and(bb, ctx->by_type[PT_ROOK]);
    if (!bb128_is_empty(bb)) {
        *sq_out = bb128_lsb(bb);
        *value_out = see_piece_value_raw(PT_ROOK);
        return;
    }

    bb = bb128_and(attackers, ctx->by_color[stm]);
    bb = bb128_and(bb, ctx->by_type[PT_HORSE]);
    if (!bb128_is_empty(bb)) {
        *sq_out = bb128_lsb(bb);
        *value_out = see_piece_value_raw(PT_HORSE);
        return;
    }

    bb = bb128_and(attackers, ctx->by_color[stm]);
    bb = bb128_and(bb, ctx->by_type[PT_DRAGON]);
    if (!bb128_is_empty(bb)) {
        *sq_out = bb128_lsb(bb);
        *value_out = see_piece_value_raw(PT_DRAGON);
        return;
    }

    bb = bb128_and(attackers, ctx->by_color[stm]);
    bb = bb128_and(bb, ctx->by_type[PT_KING]);
    *sq_out = bb128_lsb(bb);
    *value_out = see_piece_value_raw(PT_KING);
}

__attribute__((noinline, visibility("hidden")))
void attackers_to_occ_sse2(
    const AttackersCtx * __restrict ctx,
    const BB128 * __restrict occupied,
    uint8_t sq,
    BB128 * __restrict out)
{
    *out = attackers_to_occ_impl(ctx, sq, *occupied);
}

__attribute__((noinline, visibility("hidden")))
bool see_ge_sse2(
    const AttackersCtx * __restrict ctx,
    const BB128 * __restrict occupied,
    uint8_t side_to_move,
    uint8_t from_sq,
    uint8_t to_sq,
    uint8_t from_pt,
    uint8_t captured_pt,
    uint8_t drop_pt,
    const BB128 * __restrict blockers_for_king,
    const BB128 * __restrict pinners,
    int32_t threshold)
{
    const bool is_drop = drop_pt != 0;
    const int32_t captured_value = is_drop ? 0 : see_piece_value_raw(captured_pt);
    const int32_t from_value = see_piece_value_raw(is_drop ? drop_pt : from_pt);
    int32_t swap = captured_value - threshold;
    BB128 occ = *occupied;
    uint8_t stm = side_to_move;
    BB128 attackers;
    int32_t res = 1;

    if (swap < 0) {
        return false;
    }

    swap = from_value - swap;
    if (swap <= 0) {
        return true;
    }

    attackers = attackers_to_occ_impl(ctx, to_sq, occ);

    for (;;) {
        uint8_t attacker_sq;
        int32_t attacker_value;
        int8_t dir;

        stm ^= 1;
        attackers = bb128_and(attackers, occ);

        BB128 stm_attackers = bb128_and(attackers, ctx->by_color[stm]);
        if (bb128_is_empty(stm_attackers)) {
            break;
        }

        if (!bb128_is_empty(bb128_and(pinners[stm], occ))) {
            stm_attackers = bb128_andnot(stm_attackers, blockers_for_king[stm]);
            if (bb128_is_empty(stm_attackers)) {
                break;
            }
        }

        res ^= 1;
        least_valuable_attacker(ctx, stm_attackers, stm, &attacker_sq, &attacker_value);

        swap = attacker_value - swap;
        if (swap < res) {
            break;
        }

        if (attacker_value == see_piece_value_raw(PT_KING)) {
            return !bb128_is_empty(bb128_and(attackers, ctx->by_color[stm ^ 1]))
                ? ((res ^ 1) != 0)
                : (res != 0);
        }

        occ = bb128_xor(occ, bb128_from_square(attacker_sq));
        dir = direct_of(to_sq, attacker_sq);
        if (dir >= 0) {
            const BB128 ray = ray_effect_dir(ctx, (uint8_t)dir, to_sq, occ);
            BB128 extras;

            switch (dir) {
            case DIRECT_RU:
            case DIRECT_RD:
            case DIRECT_LU:
            case DIRECT_LD:
                extras = bb128_and(ray, *ctx->bishop_horse_bb);
                break;
            case DIRECT_U:
                extras = bb128_and(
                    ray,
                    bb128_or(*ctx->rook_dragon_bb, bb128_and(ctx->by_type[PT_LANCE], ctx->by_color[1])));
                break;
            case DIRECT_D:
                extras = bb128_and(
                    ray,
                    bb128_or(*ctx->rook_dragon_bb, bb128_and(ctx->by_type[PT_LANCE], ctx->by_color[0])));
                break;
            case DIRECT_L:
            case DIRECT_R:
            default:
                extras = bb128_and(ray, *ctx->rook_dragon_bb);
                break;
            }

            attackers = bb128_or(attackers, extras);
        }
    }

    (void)from_sq;
    return res != 0;
}
