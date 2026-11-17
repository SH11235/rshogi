use super::ExtMoveBuffer;
#[cfg(any(target_arch = "x86_64", test))]
use super::MAX_MOVES;
use crate::bitboard::Bitboard;
#[cfg(target_arch = "x86_64")]
use crate::types::Square;
use crate::types::{Move, Piece, PieceType};

impl ExtMoveBuffer {
    #[inline]
    pub(in crate::movegen) fn splat_drops(
        &mut self,
        targets: Bitboard,
        drops: &[(PieceType, Piece)],
    ) {
        if targets.is_empty() || drops.is_empty() {
            return;
        }
        #[cfg(target_arch = "x86_64")]
        if std::is_x86_feature_detected!("avx512f")
            && std::is_x86_feature_detected!("bmi2")
            && drops.len() <= 6
            && targets.count() as usize * drops.len() <= MAX_MOVES - self.len
        {
            // SAFETY: CPU 機能を確認済みで、全出力がバッファ内に収まる。
            unsafe { self.splat_drops_avx512(targets, drops) };
            return;
        }
        self.splat_drops_scalar(targets, drops);
    }

    #[inline]
    fn splat_drops_scalar(&mut self, targets: Bitboard, drops: &[(PieceType, Piece)]) {
        for to in targets.iter() {
            for &(pt, pc) in drops {
                self.push_move(Move::new_drop_with_piece(pt, to, pc));
            }
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx512f,bmi2")]
    unsafe fn splat_drops_avx512(&mut self, targets: Bitboard, drops: &[(PieceType, Piece)]) {
        let mut bases = [0u64; 8];
        for (base, &(pt, pc)) in bases.iter_mut().zip(drops) {
            *base = Move::new_drop_with_piece(pt, Square::SQ_11, pc).raw32() as u64;
        }
        // SAFETY: 呼出し側が CPU 機能と出力容量を確認し、N は drops の要素数と一致する。
        unsafe {
            match drops.len() {
                1 => self.splat_drops_n::<1>(targets, &bases),
                2 => self.splat_drops_n::<2>(targets, &bases),
                3 => self.splat_drops_n::<3>(targets, &bases),
                4 => self.splat_drops_n::<4>(targets, &bases),
                5 => self.splat_drops_n::<5>(targets, &bases),
                6 => self.splat_drops_n::<6>(targets, &bases),
                _ => unreachable!(),
            }
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx512f,bmi2")]
    unsafe fn splat_drops_n<const N: usize>(&mut self, targets: Bitboard, bases: &[u64; 8]) {
        use std::arch::x86_64::*;

        const fn indices<const N: usize>(piece: bool) -> [[i64; 8]; N] {
            let mut result = [[0; 8]; N];
            let mut i = 0;
            while i < N * 8 {
                result[i / 8][i % 8] = if piece {
                    (i % N) as i64
                } else {
                    (i / N) as i64
                };
                i += 1;
            }
            result
        }
        const fn deposit_mask<const N: usize>() -> u64 {
            let mut mask = 0;
            let mut i = 0;
            while i < 8 {
                mask |= 1 << (i * N);
                i += 1;
            }
            mask
        }

        // SAFETY: 各 load は8要素の配列内。Move は u32、repr(C) の ExtMove はその後に i32 を持つ。
        // lane 上位32bitを0に保ち、capacity 確認済みの領域に選択された lane だけを書く。
        unsafe {
            let pieces = _mm512_loadu_si512(bases.as_ptr().cast());
            let piece_indices = const { indices::<N>(true) };
            let square_indices = const { indices::<N>(false) };
            let templates: [__m512i; N] = std::array::from_fn(|i| {
                let pc = _mm512_permutexvar_epi64(
                    _mm512_loadu_si512(piece_indices[i].as_ptr().cast()),
                    pieces,
                );
                _mm512_or_si512(pc, _mm512_loadu_si512(square_indices[i].as_ptr().cast()))
            });
            // p0 の bit63 は未使用。p1 の bit0 は升63なので、語ごとに基点を変える。
            for (mut bits, mut square) in [(targets.p0(), 0i64), (targets.p1(), 63)] {
                while bits != 0 {
                    let skip = bits.trailing_zeros() & !7;
                    bits >>= skip;
                    square += skip as i64;
                    // 各升の bit を N 駒種分へ複製し、升順のまま64bit laneを圧縮する。
                    let mask =
                        _pdep_u64(bits & 255, const { deposit_mask::<N>() }) * ((1 << N) - 1);
                    let to = _mm512_set1_epi64(square);
                    for (i, &template) in templates.iter().enumerate() {
                        let selected = (mask >> (i * 8)) as u8;
                        let moves = _mm512_add_epi64(template, to);
                        _mm512_mask_compressstoreu_epi64(
                            self.buf.as_mut_ptr().add(self.len).cast(),
                            selected,
                            moves,
                        );
                        self.len += selected.count_ones() as usize;
                    }
                    bits >>= 8;
                    square += 8;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::movegen::ExtMove;
    use crate::types::Color;

    fn entries(buffer: &ExtMoveBuffer) -> Vec<(u32, i32)> {
        buffer.iter().map(|ext| (ext.mv.raw32(), ext.value)).collect()
    }

    fn compare(targets: Bitboard, drops: &[(PieceType, Piece)], prefix: usize, avx512: bool) {
        let mut expected = ExtMoveBuffer::new();
        let mut actual = ExtMoveBuffer::new();
        for i in 0..MAX_MOVES {
            let entry = ExtMove::new(Move::NULL, i as i32 + 1);
            expected.push(entry);
            actual.push(entry);
        }
        expected.set_len(prefix);
        actual.set_len(prefix);
        expected.splat_drops_scalar(targets, drops);
        if avx512 {
            #[cfg(target_arch = "x86_64")]
            {
                assert!(std::is_x86_feature_detected!("avx512f"));
                assert!(std::is_x86_feature_detected!("bmi2"));
                assert!((1..=6).contains(&drops.len()));
                assert!(targets.count() as usize * drops.len() <= MAX_MOVES - prefix);
                // SAFETY: CPU 機能・駒種数・全出力の容量を上で確認済み。
                unsafe { actual.splat_drops_avx512(targets, drops) };
            }
        } else {
            actual.splat_drops(targets, drops);
        }
        assert_eq!(entries(&actual), entries(&expected), "{targets:?} {drops:?}");
        for i in actual.len..MAX_MOVES {
            // SAFETY: テスト冒頭で末尾まで初期化し、生成後の長さ以降は上書きされない契約。
            let tail = unsafe { actual.buf[i].assume_init_ref() };
            assert_eq!((tail.mv, tail.value), (Move::NULL, i as i32 + 1));
        }
    }

    fn compare_masks(avx512: bool) {
        let types = [
            PieceType::Knight,
            PieceType::Lance,
            PieceType::Silver,
            PieceType::Gold,
            PieceType::Bishop,
            PieceType::Rook,
        ];
        for us in [Color::Black, Color::White] {
            let drops: Vec<_> = types.iter().map(|&pt| (pt, Piece::make(us, pt))).collect();
            for n in 1..=6 {
                for start in (0..63).step_by(8).chain([63, 71, 79]) {
                    for mask in 0..=255u64 {
                        let targets = if start < 63 {
                            Bitboard::new(mask << start, 0)
                        } else {
                            Bitboard::new(0, mask << (start - 63))
                        } & Bitboard::ALL;
                        compare(targets, &drops[..n], 3, avx512);
                    }
                }
            }
            for hand in 1..64 {
                let subset: Vec<_> = drops
                    .iter()
                    .enumerate()
                    .filter_map(|(i, &drop)| (hand & (1 << i) != 0).then_some(drop))
                    .collect();
                for targets in [
                    Bitboard::ALL,
                    Bitboard::new(0x4000_0000_0001_0001, 0x2_0002),
                    Bitboard::new(0x5555_5555_5555_5555, 0x1_5555),
                    Bitboard::EMPTY,
                ] {
                    let count = targets.count() as usize * subset.len();
                    compare(targets, &subset, MAX_MOVES - count, avx512);
                }
            }
        }
    }

    #[test]
    fn splat_drops_preserves_order_values_and_tail() {
        compare_masks(false);
    }

    #[test]
    fn empty_drops_leave_a_full_buffer_unchanged() {
        compare(Bitboard::ALL, &[], MAX_MOVES, false);
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "ExtMoveBuffer overflow")]
    fn insufficient_capacity_keeps_scalar_overflow_check() {
        let mut buffer = ExtMoveBuffer::new();
        for _ in 0..MAX_MOVES {
            buffer.push_move(Move::NULL);
        }
        buffer.splat_drops(
            Bitboard::from_square(crate::types::Square::SQ_11),
            &[(PieceType::Gold, Piece::make(Color::Black, PieceType::Gold))],
        );
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn avx512_splat_drops_matches_scalar_lane_order() {
        if !std::is_x86_feature_detected!("avx512f") || !std::is_x86_feature_detected!("bmi2") {
            eprintln!("AVX-512F/BMI2 非対応のため SIMD の実行比較をスキップ");
            return;
        }
        assert_eq!(std::mem::size_of::<ExtMove>(), 8);
        assert_eq!(std::mem::offset_of!(ExtMove, mv), 0);
        assert_eq!(std::mem::offset_of!(ExtMove, value), 4);
        compare_masks(true);
    }
}
