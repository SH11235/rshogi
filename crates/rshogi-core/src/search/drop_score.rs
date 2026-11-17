use crate::types::Square;

type Rows<'a> = [&'a [i16; Square::NUM]; 6];

#[inline]
pub(super) fn sum(rows: Rows<'_>, out: &mut [i32; Square::NUM]) {
    #[cfg(all(
        target_arch = "x86_64",
        target_feature = "avx512f",
        target_feature = "avx512bw"
    ))]
    sum_avx512(rows, out);
    #[cfg(all(
        target_arch = "x86_64",
        target_feature = "avx2",
        not(all(target_feature = "avx512f", target_feature = "avx512bw"))
    ))]
    sum_avx2(rows, out);
    #[cfg(not(all(target_arch = "x86_64", target_feature = "avx2")))]
    sum_scalar(rows, out);
}

#[cfg(any(test, not(all(target_arch = "x86_64", target_feature = "avx2"))))]
fn sum_scalar(rows: Rows<'_>, out: &mut [i32; Square::NUM]) {
    for (i, value) in out.iter_mut().enumerate() {
        *value = 2 * i32::from(rows[0][i]);
        for row in &rows[1..] {
            *value += i32::from(row[i]);
        }
    }
}

#[cfg(all(
    target_arch = "x86_64",
    target_feature = "avx2",
    any(
        test,
        not(all(target_feature = "avx512f", target_feature = "avx512bw"))
    )
))]
#[inline]
fn sum_avx2(rows: Rows<'_>, out: &mut [i32; Square::NUM]) {
    use std::arch::x86_64::*;
    for i in (0..80).step_by(8) {
        // SAFETY: AVX2 は build 時に有効。各 load は行内の 8 個の i16、store は出力内の 8 個の i32 に収まる。
        unsafe {
            let mut value = _mm256_slli_epi32::<1>(_mm256_cvtepi16_epi32(_mm_loadu_si128(
                rows[0].as_ptr().add(i).cast(),
            )));
            for row in &rows[1..] {
                value = _mm256_add_epi32(
                    value,
                    _mm256_cvtepi16_epi32(_mm_loadu_si128(row.as_ptr().add(i).cast())),
                );
            }
            _mm256_storeu_si256(out.as_mut_ptr().add(i).cast(), value);
        }
    }
    out[80] =
        2 * i32::from(rows[0][80]) + rows[1..].iter().map(|row| i32::from(row[80])).sum::<i32>();
}

#[cfg(all(
    target_arch = "x86_64",
    target_feature = "avx512f",
    target_feature = "avx512bw"
))]
#[inline]
fn sum_avx512(rows: Rows<'_>, out: &mut [i32; Square::NUM]) {
    use std::arch::x86_64::*;
    for i in (0..80).step_by(16) {
        // SAFETY: AVX-512F/BW は build 時に有効。各 load は行内の 16 個の i16、store は出力内の 16 個の i32 に収まる。
        unsafe {
            let mut value = _mm512_slli_epi32::<1>(_mm512_cvtepi16_epi32(_mm256_loadu_si256(
                rows[0].as_ptr().add(i).cast(),
            )));
            for row in &rows[1..] {
                value = _mm512_add_epi32(
                    value,
                    _mm512_cvtepi16_epi32(_mm256_loadu_si256(row.as_ptr().add(i).cast())),
                );
            }
            _mm512_storeu_si512(out.as_mut_ptr().add(i).cast(), value);
        }
    }
    out[80] =
        2 * i32::from(rows[0][80]) + rows[1..].iter().map(|row| i32::from(row[80])).sum::<i32>();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_match_scalar_at_every_lane() {
        use rand::{Rng, SeedableRng};
        use rand_xoshiro::Xoshiro256PlusPlus;

        let mut rng = Xoshiro256PlusPlus::seed_from_u64(0xD0_81);
        for trial in 0..128 {
            let rows: [[i16; Square::NUM]; 6] = std::array::from_fn(|r| {
                std::array::from_fn(|i| match trial {
                    0 => i16::MIN,
                    1 => i16::MAX,
                    2 => {
                        if (r + i) % 2 == 0 {
                            i16::MIN
                        } else {
                            i16::MAX
                        }
                    }
                    3 => (r * 1000 + i * 7) as i16,
                    _ => rng.random(),
                })
            });
            let refs = rows.each_ref();
            let mut expected = [0; Square::NUM];
            sum_scalar(refs, &mut expected);
            let mut actual = [0; Square::NUM];
            sum(refs, &mut actual);
            assert_eq!(actual, expected, "trial={trial}");
            #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
            {
                sum_avx2(refs, &mut actual);
                assert_eq!(actual, expected, "AVX2 trial={trial}");
            }
            #[cfg(all(
                target_arch = "x86_64",
                target_feature = "avx512f",
                target_feature = "avx512bw"
            ))]
            {
                sum_avx512(refs, &mut actual);
                assert_eq!(actual, expected, "AVX-512 trial={trial}");
            }
        }
    }
}
