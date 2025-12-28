//! 学習可能なNNUEネットワーク
//!
//! HalfKP 256x2-32-32 アーキテクチャをf32で実装し、
//! 順伝播・逆伝播をサポートする。

use byteorder::{LittleEndian, WriteBytesExt};
use rand::Rng;
use std::io::{self, Write};

/// ネットワーク定数
pub const FE_END: usize = 1548;
pub const HALFKP_DIMENSIONS: usize = 81 * FE_END;
pub const TRANSFORMED_DIMENSIONS: usize = 256;
pub const HIDDEN1_DIMENSIONS: usize = 32;
pub const HIDDEN2_DIMENSIONS: usize = 32;
pub const OUTPUT_DIMENSIONS: usize = 1;
pub const FV_SCALE: f32 = 24.0;

/// NNUEバージョン（YaneuraOu互換）
pub const NNUE_VERSION: u32 = 0x7AF32F16;

/// 学習可能なアフィン変換層
#[derive(Clone)]
pub struct TrainableAffine<const INPUT: usize, const OUTPUT: usize> {
    /// 重み [OUTPUT][INPUT]
    pub weights: Vec<f32>,
    /// バイアス [OUTPUT]
    pub biases: Vec<f32>,
    /// 重みの勾配
    pub weight_grads: Vec<f32>,
    /// バイアスの勾配
    pub bias_grads: Vec<f32>,
}

impl<const INPUT: usize, const OUTPUT: usize> TrainableAffine<INPUT, OUTPUT> {
    /// 新しい層を作成（ゼロ初期化）
    pub fn new() -> Self {
        Self {
            weights: vec![0.0; OUTPUT * INPUT],
            biases: vec![0.0; OUTPUT],
            weight_grads: vec![0.0; OUTPUT * INPUT],
            bias_grads: vec![0.0; OUTPUT],
        }
    }

    /// He初期化
    pub fn init_he<R: Rng>(&mut self, rng: &mut R) {
        let std_dev = (2.0 / INPUT as f32).sqrt();
        for w in &mut self.weights {
            *w = rng.random::<f32>() * 2.0 * std_dev - std_dev;
        }
        for b in &mut self.biases {
            *b = 0.0;
        }
    }

    /// 順伝播
    pub fn forward(&self, input: &[f32], output: &mut [f32]) {
        debug_assert_eq!(input.len(), INPUT);
        debug_assert_eq!(output.len(), OUTPUT);

        for (j, out) in output.iter_mut().enumerate() {
            let mut sum = self.biases[j];
            for (i, &inp) in input.iter().enumerate() {
                sum += self.weights[j * INPUT + i] * inp;
            }
            *out = sum;
        }
    }

    /// 逆伝播（出力勾配から入力勾配と重み勾配を計算）
    pub fn backward(&mut self, input: &[f32], output_grad: &[f32], input_grad: &mut [f32]) {
        debug_assert_eq!(input.len(), INPUT);
        debug_assert_eq!(output_grad.len(), OUTPUT);
        debug_assert_eq!(input_grad.len(), INPUT);

        // 入力勾配をゼロ初期化
        input_grad.fill(0.0);

        for (j, &grad) in output_grad.iter().enumerate() {
            // バイアス勾配を累積
            self.bias_grads[j] += grad;

            for i in 0..INPUT {
                // 重み勾配を累積
                self.weight_grads[j * INPUT + i] += grad * input[i];
                // 入力勾配を累積
                input_grad[i] += grad * self.weights[j * INPUT + i];
            }
        }
    }

    /// 勾配をゼロにリセット
    pub fn zero_grad(&mut self) {
        self.weight_grads.fill(0.0);
        self.bias_grads.fill(0.0);
    }

    /// パラメータ数
    pub fn param_count(&self) -> usize {
        OUTPUT * INPUT + OUTPUT
    }
}

impl<const INPUT: usize, const OUTPUT: usize> Default for TrainableAffine<INPUT, OUTPUT> {
    fn default() -> Self {
        Self::new()
    }
}

/// 学習可能なFeatureTransformer
#[derive(Clone)]
pub struct TrainableFeatureTransformer {
    /// 重み [HALFKP_DIMENSIONS][TRANSFORMED_DIMENSIONS]
    pub weights: Vec<f32>,
    /// バイアス [TRANSFORMED_DIMENSIONS]
    pub biases: Vec<f32>,
    /// 重みの勾配
    pub weight_grads: Vec<f32>,
    /// バイアスの勾配
    pub bias_grads: Vec<f32>,
}

impl TrainableFeatureTransformer {
    /// 新しいFeatureTransformerを作成
    pub fn new() -> Self {
        Self {
            weights: vec![0.0; HALFKP_DIMENSIONS * TRANSFORMED_DIMENSIONS],
            biases: vec![0.0; TRANSFORMED_DIMENSIONS],
            weight_grads: vec![0.0; HALFKP_DIMENSIONS * TRANSFORMED_DIMENSIONS],
            bias_grads: vec![0.0; TRANSFORMED_DIMENSIONS],
        }
    }

    /// 小さい値で初期化
    pub fn init_small<R: Rng>(&mut self, rng: &mut R) {
        let std_dev = 0.01;
        for w in &mut self.weights {
            *w = rng.random::<f32>() * 2.0 * std_dev - std_dev;
        }
        for b in &mut self.biases {
            *b = 0.0;
        }
    }

    /// 順伝播（スパース入力）
    ///
    /// active_features: アクティブな特徴量のインデックスリスト
    /// output: 変換後の出力 [TRANSFORMED_DIMENSIONS]
    pub fn forward(&self, active_features: &[usize], output: &mut [f32]) {
        debug_assert_eq!(output.len(), TRANSFORMED_DIMENSIONS);

        // バイアスで初期化
        output.copy_from_slice(&self.biases);

        // アクティブな特徴量の重みを加算
        for &idx in active_features {
            if idx >= HALFKP_DIMENSIONS {
                continue;
            }
            let offset = idx * TRANSFORMED_DIMENSIONS;
            for (i, out) in output.iter_mut().enumerate() {
                *out += self.weights[offset + i];
            }
        }
    }

    /// 逆伝播
    pub fn backward(&mut self, active_features: &[usize], output_grad: &[f32]) {
        debug_assert_eq!(output_grad.len(), TRANSFORMED_DIMENSIONS);

        // バイアス勾配を累積
        for (i, &grad) in output_grad.iter().enumerate() {
            self.bias_grads[i] += grad;
        }

        // アクティブな特徴量の重み勾配を累積
        for &idx in active_features {
            if idx >= HALFKP_DIMENSIONS {
                continue;
            }
            let offset = idx * TRANSFORMED_DIMENSIONS;
            for (i, &grad) in output_grad.iter().enumerate() {
                self.weight_grads[offset + i] += grad;
            }
        }
    }

    /// 勾配をゼロにリセット
    pub fn zero_grad(&mut self) {
        self.weight_grads.fill(0.0);
        self.bias_grads.fill(0.0);
    }

    /// パラメータ数
    pub fn param_count(&self) -> usize {
        HALFKP_DIMENSIONS * TRANSFORMED_DIMENSIONS + TRANSFORMED_DIMENSIONS
    }
}

impl Default for TrainableFeatureTransformer {
    fn default() -> Self {
        Self::new()
    }
}

/// ClippedReLU（0-127にクリップ）
#[inline]
pub fn clipped_relu(x: f32) -> f32 {
    x.clamp(0.0, 127.0)
}

/// ClippedReLUの勾配
#[inline]
pub fn clipped_relu_grad(x: f32) -> f32 {
    if x > 0.0 && x < 127.0 {
        1.0
    } else {
        0.0
    }
}

/// 学習可能なNNUEネットワーク
#[derive(Clone)]
pub struct TrainableNetwork {
    /// 特徴量変換器（先手視点）
    pub ft_black: TrainableFeatureTransformer,
    /// 特徴量変換器（後手視点、重みは共有）
    /// 実際は ft_black と同じ重みを使用するが、勾配累積のため別インスタンス
    /// 学習時はft_blackの重みを使い、勾配を両方から累積する
    /// 隠れ層1: 512 -> 32
    pub hidden1: TrainableAffine<{ TRANSFORMED_DIMENSIONS * 2 }, HIDDEN1_DIMENSIONS>,
    /// 隠れ層2: 32 -> 32
    pub hidden2: TrainableAffine<HIDDEN1_DIMENSIONS, HIDDEN2_DIMENSIONS>,
    /// 出力層: 32 -> 1
    pub output: TrainableAffine<HIDDEN2_DIMENSIONS, OUTPUT_DIMENSIONS>,
}

impl TrainableNetwork {
    /// 新しいネットワークを作成
    pub fn new() -> Self {
        Self {
            ft_black: TrainableFeatureTransformer::new(),
            hidden1: TrainableAffine::new(),
            hidden2: TrainableAffine::new(),
            output: TrainableAffine::new(),
        }
    }

    /// ランダム初期化
    pub fn init_random<R: Rng>(&mut self, rng: &mut R) {
        self.ft_black.init_small(rng);
        self.hidden1.init_he(rng);
        self.hidden2.init_he(rng);
        self.output.init_he(rng);
    }

    /// 順伝播
    ///
    /// # Arguments
    /// * `black_features` - 先手視点のアクティブ特徴量
    /// * `white_features` - 後手視点のアクティブ特徴量
    /// * `side_to_move` - 手番（0=先手, 1=後手）
    ///
    /// # Returns
    /// * 評価値（スケーリング前）
    /// * 中間値（逆伝播用）
    pub fn forward(
        &self,
        black_features: &[usize],
        white_features: &[usize],
        side_to_move: usize,
    ) -> (f32, ForwardCache) {
        let mut cache = ForwardCache::new();

        // FeatureTransformer
        self.ft_black.forward(black_features, &mut cache.ft_black_out);
        self.ft_black.forward(white_features, &mut cache.ft_white_out);

        // 視点に応じて連結順序を変える
        if side_to_move == 0 {
            // 先手番: [black, white]
            cache.ft_combined[..TRANSFORMED_DIMENSIONS].copy_from_slice(&cache.ft_black_out);
            cache.ft_combined[TRANSFORMED_DIMENSIONS..].copy_from_slice(&cache.ft_white_out);
        } else {
            // 後手番: [white, black]
            cache.ft_combined[..TRANSFORMED_DIMENSIONS].copy_from_slice(&cache.ft_white_out);
            cache.ft_combined[TRANSFORMED_DIMENSIONS..].copy_from_slice(&cache.ft_black_out);
        }

        // ClippedReLU
        for (i, v) in cache.ft_combined.iter().enumerate() {
            cache.ft_relu[i] = clipped_relu(*v);
        }

        // Hidden1
        self.hidden1.forward(&cache.ft_relu, &mut cache.h1_out);
        for (i, v) in cache.h1_out.iter().enumerate() {
            cache.h1_relu[i] = clipped_relu(*v);
        }

        // Hidden2
        self.hidden2.forward(&cache.h1_relu, &mut cache.h2_out);
        for (i, v) in cache.h2_out.iter().enumerate() {
            cache.h2_relu[i] = clipped_relu(*v);
        }

        // Output
        self.output.forward(&cache.h2_relu, &mut cache.out);

        (cache.out[0], cache)
    }

    /// 逆伝播
    pub fn backward(
        &mut self,
        black_features: &[usize],
        white_features: &[usize],
        side_to_move: usize,
        cache: &ForwardCache,
        output_grad: f32,
    ) {
        // Output層の逆伝播
        let out_grad = [output_grad];
        let mut h2_relu_grad = [0.0f32; HIDDEN2_DIMENSIONS];
        self.output.backward(&cache.h2_relu, &out_grad, &mut h2_relu_grad);

        // Hidden2のClippedReLU逆伝播
        let mut h2_out_grad = [0.0f32; HIDDEN2_DIMENSIONS];
        for i in 0..HIDDEN2_DIMENSIONS {
            h2_out_grad[i] = h2_relu_grad[i] * clipped_relu_grad(cache.h2_out[i]);
        }

        // Hidden2層の逆伝播
        let mut h1_relu_grad = [0.0f32; HIDDEN1_DIMENSIONS];
        self.hidden2.backward(&cache.h1_relu, &h2_out_grad, &mut h1_relu_grad);

        // Hidden1のClippedReLU逆伝播
        let mut h1_out_grad = [0.0f32; HIDDEN1_DIMENSIONS];
        for i in 0..HIDDEN1_DIMENSIONS {
            h1_out_grad[i] = h1_relu_grad[i] * clipped_relu_grad(cache.h1_out[i]);
        }

        // Hidden1層の逆伝播
        let mut ft_relu_grad = [0.0f32; TRANSFORMED_DIMENSIONS * 2];
        self.hidden1.backward(&cache.ft_relu, &h1_out_grad, &mut ft_relu_grad);

        // FeatureTransformerのClippedReLU逆伝播
        let mut ft_combined_grad = [0.0f32; TRANSFORMED_DIMENSIONS * 2];
        for i in 0..TRANSFORMED_DIMENSIONS * 2 {
            ft_combined_grad[i] = ft_relu_grad[i] * clipped_relu_grad(cache.ft_combined[i]);
        }

        // 視点に応じて勾配を分離
        let (ft_black_grad, ft_white_grad) = if side_to_move == 0 {
            (
                &ft_combined_grad[..TRANSFORMED_DIMENSIONS],
                &ft_combined_grad[TRANSFORMED_DIMENSIONS..],
            )
        } else {
            (
                &ft_combined_grad[TRANSFORMED_DIMENSIONS..],
                &ft_combined_grad[..TRANSFORMED_DIMENSIONS],
            )
        };

        // FeatureTransformerの逆伝播（重みは共有なので両方の勾配を累積）
        self.ft_black.backward(black_features, ft_black_grad);
        self.ft_black.backward(white_features, ft_white_grad);
    }

    /// 勾配をゼロにリセット
    pub fn zero_grad(&mut self) {
        self.ft_black.zero_grad();
        self.hidden1.zero_grad();
        self.hidden2.zero_grad();
        self.output.zero_grad();
    }

    /// パラメータ数
    pub fn param_count(&self) -> usize {
        self.ft_black.param_count()
            + self.hidden1.param_count()
            + self.hidden2.param_count()
            + self.output.param_count()
    }

    /// YaneuraOu形式でモデルを保存
    pub fn save<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        // ヘッダ
        writer.write_u32::<LittleEndian>(NNUE_VERSION)?;

        // 構造ハッシュ（ダミー）
        writer.write_u32::<LittleEndian>(0)?;

        // アーキテクチャ文字列
        let arch_str = b"HalfKP(Friend)[256x2->32->32->1]";
        writer.write_u32::<LittleEndian>(arch_str.len() as u32)?;
        writer.write_all(arch_str)?;

        // FeatureTransformer
        // バイアス [256] as i16
        for &b in &self.ft_black.biases {
            writer.write_i16::<LittleEndian>(b.round() as i16)?;
        }
        // 重み [HALFKP][256] as i16
        for &w in &self.ft_black.weights {
            writer.write_i16::<LittleEndian>(w.round() as i16)?;
        }

        // Hidden1
        for &b in &self.hidden1.biases {
            writer.write_i32::<LittleEndian>(b.round() as i32)?;
        }
        for &w in &self.hidden1.weights {
            writer.write_i8(w.round().clamp(-128.0, 127.0) as i8)?;
        }

        // Hidden2
        for &b in &self.hidden2.biases {
            writer.write_i32::<LittleEndian>(b.round() as i32)?;
        }
        for &w in &self.hidden2.weights {
            writer.write_i8(w.round().clamp(-128.0, 127.0) as i8)?;
        }

        // Output
        for &b in &self.output.biases {
            writer.write_i32::<LittleEndian>(b.round() as i32)?;
        }
        for &w in &self.output.weights {
            writer.write_i8(w.round().clamp(-128.0, 127.0) as i8)?;
        }

        Ok(())
    }
}

impl Default for TrainableNetwork {
    fn default() -> Self {
        Self::new()
    }
}

/// 順伝播時の中間値キャッシュ
pub struct ForwardCache {
    pub ft_black_out: [f32; TRANSFORMED_DIMENSIONS],
    pub ft_white_out: [f32; TRANSFORMED_DIMENSIONS],
    pub ft_combined: [f32; TRANSFORMED_DIMENSIONS * 2],
    pub ft_relu: [f32; TRANSFORMED_DIMENSIONS * 2],
    pub h1_out: [f32; HIDDEN1_DIMENSIONS],
    pub h1_relu: [f32; HIDDEN1_DIMENSIONS],
    pub h2_out: [f32; HIDDEN2_DIMENSIONS],
    pub h2_relu: [f32; HIDDEN2_DIMENSIONS],
    pub out: [f32; OUTPUT_DIMENSIONS],
}

impl ForwardCache {
    pub fn new() -> Self {
        Self {
            ft_black_out: [0.0; TRANSFORMED_DIMENSIONS],
            ft_white_out: [0.0; TRANSFORMED_DIMENSIONS],
            ft_combined: [0.0; TRANSFORMED_DIMENSIONS * 2],
            ft_relu: [0.0; TRANSFORMED_DIMENSIONS * 2],
            h1_out: [0.0; HIDDEN1_DIMENSIONS],
            h1_relu: [0.0; HIDDEN1_DIMENSIONS],
            h2_out: [0.0; HIDDEN2_DIMENSIONS],
            h2_relu: [0.0; HIDDEN2_DIMENSIONS],
            out: [0.0; OUTPUT_DIMENSIONS],
        }
    }
}

impl Default for ForwardCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trainable_affine_forward() {
        let mut layer: TrainableAffine<4, 2> = TrainableAffine::new();
        layer.weights = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
        layer.biases = vec![1.0, 2.0];

        let input = [1.0, 2.0, 3.0, 4.0];
        let mut output = [0.0; 2];
        layer.forward(&input, &mut output);

        // output[0] = 1 + 1*1 + 2*2 + 3*3 + 4*4 = 1 + 1 + 4 + 9 + 16 = 31
        // output[1] = 2 + 1*5 + 2*6 + 3*7 + 4*8 = 2 + 5 + 12 + 21 + 32 = 72
        assert!((output[0] - 31.0).abs() < 1e-5);
        assert!((output[1] - 72.0).abs() < 1e-5);
    }

    #[test]
    fn test_clipped_relu() {
        assert_eq!(clipped_relu(-10.0), 0.0);
        assert_eq!(clipped_relu(0.0), 0.0);
        assert_eq!(clipped_relu(50.0), 50.0);
        assert_eq!(clipped_relu(127.0), 127.0);
        assert_eq!(clipped_relu(200.0), 127.0);
    }

    #[test]
    fn test_network_forward() {
        let network = TrainableNetwork::new();

        // 空の特徴量でテスト
        let (output, _cache) = network.forward(&[], &[], 0);
        assert!(output.is_finite());
    }
}
