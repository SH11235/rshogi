/*
 * SSE2-only attackers_to_occ 近接駒 kernel
 *
 * ビルドフラグ: -O3 -msse2 -mno-avx -mno-avx2 -fno-lto
 */

#include <emmintrin.h>  /* SSE2 */
#include <stdint.h>

typedef struct __attribute__((aligned(16))) {
    uint64_t p[2];
} BB128;

/*
 * 近接駒効きの lookup table 群と Position フィールドをまとめた構造体。
 * Rust 側で構築し、C kernel に渡す。
 */
typedef struct {
    /* 近接駒効き lookup table の base ポインタ */
    const BB128 (*pawn_effect)[81];    /* [2][81] — [color][sq] */
    const BB128 (*knight_effect)[81];
    const BB128 (*silver_effect)[81];
    const BB128 (*gold_effect)[81];

    /* Position フィールドへのポインタ */
    const BB128 *by_type;   /* [PieceType::NUM + 1] */
    const BB128 *by_color;  /* [Color::NUM] = [2] */
    const BB128 *golds_bb;
    const BB128 *hdk_bb;
} NearCtx;

/* Piece type indices (must match rshogi-core PieceType enum) */
#define PT_PAWN   1
#define PT_KNIGHT 3
#define PT_SILVER 4

__attribute__((noinline, visibility("hidden")))
void attackers_near_pieces_sse2(
    const NearCtx * __restrict ctx,
    uint8_t sq,
    BB128 * __restrict out)
{
    const __m128i pawn   = _mm_load_si128((const __m128i *)&ctx->by_type[PT_PAWN]);
    const __m128i knight = _mm_load_si128((const __m128i *)&ctx->by_type[PT_KNIGHT]);
    const __m128i hdk    = _mm_load_si128((const __m128i *)ctx->hdk_bb);
    const __m128i golds  = _mm_load_si128((const __m128i *)ctx->golds_bb);
    const __m128i black  = _mm_load_si128((const __m128i *)&ctx->by_color[0]);
    const __m128i white  = _mm_load_si128((const __m128i *)&ctx->by_color[1]);

    const __m128i silver_hdk = _mm_or_si128(
        _mm_load_si128((const __m128i *)&ctx->by_type[PT_SILVER]), hdk);
    const __m128i golds_hdk = _mm_or_si128(golds, hdk);

    /* 先手の攻め駒 (White 方向の effect で逆引き → Black でフィルタ) */
    const __m128i black_att = _mm_and_si128(
        _mm_or_si128(
            _mm_or_si128(
                _mm_and_si128(_mm_load_si128((const __m128i *)&ctx->pawn_effect[1][sq]), pawn),
                _mm_and_si128(_mm_load_si128((const __m128i *)&ctx->knight_effect[1][sq]), knight)
            ),
            _mm_or_si128(
                _mm_and_si128(_mm_load_si128((const __m128i *)&ctx->silver_effect[1][sq]), silver_hdk),
                _mm_and_si128(_mm_load_si128((const __m128i *)&ctx->gold_effect[1][sq]), golds_hdk)
            )
        ),
        black
    );

    /* 後手の攻め駒 (Black 方向の effect で逆引き → White でフィルタ) */
    const __m128i white_att = _mm_and_si128(
        _mm_or_si128(
            _mm_or_si128(
                _mm_and_si128(_mm_load_si128((const __m128i *)&ctx->pawn_effect[0][sq]), pawn),
                _mm_and_si128(_mm_load_si128((const __m128i *)&ctx->knight_effect[0][sq]), knight)
            ),
            _mm_or_si128(
                _mm_and_si128(_mm_load_si128((const __m128i *)&ctx->silver_effect[0][sq]), silver_hdk),
                _mm_and_si128(_mm_load_si128((const __m128i *)&ctx->gold_effect[0][sq]), golds_hdk)
            )
        ),
        white
    );

    _mm_store_si128((__m128i *)out, _mm_or_si128(black_att, white_att));
}
