//! ネットワーク層の実装
//!
//! - `AffineTransform`: 全結合アフィン変換層（入力×重み + バイアス）
//! - `ClippedReLU`: 整数スケーリング付きのクリップ付き ReLU 層

use super::constants::WEIGHT_SCALE_BITS;
use std::io::{self, Read};

/// パディング済み入力次元（SIMDアライメント用）
const fn padded_input(input_dim: usize) -> usize {
    input_dim.div_ceil(32) * 32
}

/// アフィン変換層
pub struct AffineTransform<const INPUT_DIM: usize, const OUTPUT_DIM: usize> {
    /// バイアス
    pub biases: [i32; OUTPUT_DIM],
    /// 重み（転置形式で保持）
    pub weights: Box<[i8]>,
}

impl<const INPUT_DIM: usize, const OUTPUT_DIM: usize> AffineTransform<INPUT_DIM, OUTPUT_DIM> {
    const PADDED_INPUT: usize = padded_input(INPUT_DIM);

    /// ファイルから読み込み
    pub fn read<R: Read>(reader: &mut R) -> io::Result<Self> {
        // バイアスを読み込み
        let mut biases = [0i32; OUTPUT_DIM];
        let mut buf4 = [0u8; 4];
        for bias in biases.iter_mut() {
            reader.read_exact(&mut buf4)?;
            *bias = i32::from_le_bytes(buf4);
        }

        // 重みを読み込み
        let weight_size = OUTPUT_DIM * Self::PADDED_INPUT;
        let mut weights = vec![0i8; weight_size].into_boxed_slice();
        let mut buf1 = [0u8; 1];
        for weight in weights.iter_mut() {
            reader.read_exact(&mut buf1)?;
            *weight = buf1[0] as i8;
        }

        Ok(Self { biases, weights })
    }

    /// 順伝播
    pub fn propagate(&self, input: &[u8], output: &mut [i32; OUTPUT_DIM]) {
        // バイアスで初期化
        output.copy_from_slice(&self.biases);

        // 行列×ベクトル
        for (i, &in_byte) in input.iter().enumerate().take(INPUT_DIM) {
            if in_byte != 0 {
                let in_val = in_byte as i32;
                for (j, out) in output.iter_mut().enumerate() {
                    let weight_idx = j * Self::PADDED_INPUT + i;
                    *out += self.weights[weight_idx] as i32 * in_val;
                }
            }
        }
    }
}

/// ClippedReLU層
/// 入力: i32、出力: u8（0-127にクランプ）
pub struct ClippedReLU<const DIM: usize>;

impl<const DIM: usize> ClippedReLU<DIM> {
    /// 順伝播
    pub fn propagate(input: &[i32; DIM], output: &mut [u8; DIM]) {
        for i in 0..DIM {
            let shifted = input[i] >> WEIGHT_SCALE_BITS;
            output[i] = shifted.clamp(0, 127) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_affine_transform_propagate() {
        // 小さいテスト用の変換
        let transform: AffineTransform<4, 2> = AffineTransform {
            biases: [10, 20],
            weights: vec![
                1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 3, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0,
            ]
            .into_boxed_slice(),
        };

        let input = [1u8, 2, 0, 0];
        let mut output = [0i32; 2];

        transform.propagate(&input, &mut output);

        // output[0] = 10 + 1*1 + 2*2 = 15
        // output[1] = 20 + 1*3 + 2*4 = 31
        assert_eq!(output[0], 15);
        assert_eq!(output[1], 31);
    }

    #[test]
    fn test_clipped_relu() {
        let input = [0i32, 64, 128, -64, 256];
        let mut output = [0u8; 5];

        // WEIGHT_SCALE_BITS = 6 なので、64 >> 6 = 1, 128 >> 6 = 2, etc.
        ClippedReLU::propagate(&input, &mut output);

        assert_eq!(output[0], 0); // 0 >> 6 = 0
        assert_eq!(output[1], 1); // 64 >> 6 = 1
        assert_eq!(output[2], 2); // 128 >> 6 = 2
        assert_eq!(output[3], 0); // -64 >> 6 = -1, clamped to 0
        assert_eq!(output[4], 4); // 256 >> 6 = 4
    }
}
