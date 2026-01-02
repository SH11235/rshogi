//! オプティマイザ
//!
//! Adam, SGD等の最適化アルゴリズムを実装する。

use super::network::TrainableNetwork;

/// オプティマイザ trait
pub trait Optimizer {
    /// パラメータを更新
    fn step(&mut self, network: &mut TrainableNetwork);

    /// 学習率を設定
    fn set_lr(&mut self, lr: f32);

    /// 現在の学習率を取得
    fn get_lr(&self) -> f32;
}

/// Adam オプティマイザ
pub struct Adam {
    /// 学習率
    pub lr: f32,
    /// β1（一次モーメントの減衰率）
    pub beta1: f32,
    /// β2（二次モーメントの減衰率）
    pub beta2: f32,
    /// 数値安定性のための小さな値
    pub epsilon: f32,
    /// 重み減衰
    pub weight_decay: f32,

    /// ステップ数
    t: usize,
    /// 一次モーメント（FT重み）
    m_ft_weights: Vec<f32>,
    /// 二次モーメント（FT重み）
    v_ft_weights: Vec<f32>,
    /// 一次モーメント（FTバイアス）
    m_ft_biases: Vec<f32>,
    /// 二次モーメント（FTバイアス）
    v_ft_biases: Vec<f32>,
    /// 一次モーメント（Hidden1重み）
    m_h1_weights: Vec<f32>,
    /// 二次モーメント（Hidden1重み）
    v_h1_weights: Vec<f32>,
    /// 一次モーメント（Hidden1バイアス）
    m_h1_biases: Vec<f32>,
    /// 二次モーメント（Hidden1バイアス）
    v_h1_biases: Vec<f32>,
    /// 一次モーメント（Hidden2重み）
    m_h2_weights: Vec<f32>,
    /// 二次モーメント（Hidden2重み）
    v_h2_weights: Vec<f32>,
    /// 一次モーメント（Hidden2バイアス）
    m_h2_biases: Vec<f32>,
    /// 二次モーメント（Hidden2バイアス）
    v_h2_biases: Vec<f32>,
    /// 一次モーメント（出力重み）
    m_out_weights: Vec<f32>,
    /// 二次モーメント（出力重み）
    v_out_weights: Vec<f32>,
    /// 一次モーメント（出力バイアス）
    m_out_biases: Vec<f32>,
    /// 二次モーメント（出力バイアス）
    v_out_biases: Vec<f32>,
}

impl Adam {
    /// 新しいAdamオプティマイザを作成
    pub fn new(network: &TrainableNetwork, lr: f32) -> Self {
        Self {
            lr,
            beta1: 0.9,
            beta2: 0.999,
            epsilon: 1e-8,
            weight_decay: 0.0,
            t: 0,
            m_ft_weights: vec![0.0; network.ft_black.weights.len()],
            v_ft_weights: vec![0.0; network.ft_black.weights.len()],
            m_ft_biases: vec![0.0; network.ft_black.biases.len()],
            v_ft_biases: vec![0.0; network.ft_black.biases.len()],
            m_h1_weights: vec![0.0; network.hidden1.weights.len()],
            v_h1_weights: vec![0.0; network.hidden1.weights.len()],
            m_h1_biases: vec![0.0; network.hidden1.biases.len()],
            v_h1_biases: vec![0.0; network.hidden1.biases.len()],
            m_h2_weights: vec![0.0; network.hidden2.weights.len()],
            v_h2_weights: vec![0.0; network.hidden2.weights.len()],
            m_h2_biases: vec![0.0; network.hidden2.biases.len()],
            v_h2_biases: vec![0.0; network.hidden2.biases.len()],
            m_out_weights: vec![0.0; network.output.weights.len()],
            v_out_weights: vec![0.0; network.output.weights.len()],
            m_out_biases: vec![0.0; network.output.biases.len()],
            v_out_biases: vec![0.0; network.output.biases.len()],
        }
    }

    /// デフォルトパラメータを設定
    pub fn with_weight_decay(mut self, wd: f32) -> Self {
        self.weight_decay = wd;
        self
    }

    /// β1を設定
    pub fn with_beta1(mut self, beta1: f32) -> Self {
        self.beta1 = beta1;
        self
    }

    /// β2を設定
    pub fn with_beta2(mut self, beta2: f32) -> Self {
        self.beta2 = beta2;
        self
    }
}

/// Adamの更新式を適用（借用エラー回避のため独立関数として実装）
fn adam_update_params(
    params: &mut [f32],
    grads: &[f32],
    m: &mut [f32],
    v: &mut [f32],
    beta1: f32,
    beta2: f32,
    epsilon: f32,
    weight_decay: f32,
    lr_t: f32,
) {
    for i in 0..params.len() {
        let g = grads[i] + weight_decay * params[i];

        // モーメントの更新
        m[i] = beta1 * m[i] + (1.0 - beta1) * g;
        v[i] = beta2 * v[i] + (1.0 - beta2) * g * g;

        // パラメータの更新
        params[i] -= lr_t * m[i] / (v[i].sqrt() + epsilon);
    }
}

impl Optimizer for Adam {
    fn set_lr(&mut self, lr: f32) {
        self.lr = lr;
    }

    fn get_lr(&self) -> f32 {
        self.lr
    }

    fn step(&mut self, network: &mut TrainableNetwork) {
        self.t += 1;

        // バイアス補正付き学習率
        let lr_t = self.lr * (1.0 - self.beta2.powi(self.t as i32)).sqrt()
            / (1.0 - self.beta1.powi(self.t as i32));

        // FeatureTransformer
        adam_update_params(
            &mut network.ft_black.weights,
            &network.ft_black.weight_grads,
            &mut self.m_ft_weights,
            &mut self.v_ft_weights,
            self.beta1,
            self.beta2,
            self.epsilon,
            self.weight_decay,
            lr_t,
        );
        adam_update_params(
            &mut network.ft_black.biases,
            &network.ft_black.bias_grads,
            &mut self.m_ft_biases,
            &mut self.v_ft_biases,
            self.beta1,
            self.beta2,
            self.epsilon,
            self.weight_decay,
            lr_t,
        );

        // Hidden1
        adam_update_params(
            &mut network.hidden1.weights,
            &network.hidden1.weight_grads,
            &mut self.m_h1_weights,
            &mut self.v_h1_weights,
            self.beta1,
            self.beta2,
            self.epsilon,
            self.weight_decay,
            lr_t,
        );
        adam_update_params(
            &mut network.hidden1.biases,
            &network.hidden1.bias_grads,
            &mut self.m_h1_biases,
            &mut self.v_h1_biases,
            self.beta1,
            self.beta2,
            self.epsilon,
            self.weight_decay,
            lr_t,
        );

        // Hidden2
        adam_update_params(
            &mut network.hidden2.weights,
            &network.hidden2.weight_grads,
            &mut self.m_h2_weights,
            &mut self.v_h2_weights,
            self.beta1,
            self.beta2,
            self.epsilon,
            self.weight_decay,
            lr_t,
        );
        adam_update_params(
            &mut network.hidden2.biases,
            &network.hidden2.bias_grads,
            &mut self.m_h2_biases,
            &mut self.v_h2_biases,
            self.beta1,
            self.beta2,
            self.epsilon,
            self.weight_decay,
            lr_t,
        );

        // Output
        adam_update_params(
            &mut network.output.weights,
            &network.output.weight_grads,
            &mut self.m_out_weights,
            &mut self.v_out_weights,
            self.beta1,
            self.beta2,
            self.epsilon,
            self.weight_decay,
            lr_t,
        );
        adam_update_params(
            &mut network.output.biases,
            &network.output.bias_grads,
            &mut self.m_out_biases,
            &mut self.v_out_biases,
            self.beta1,
            self.beta2,
            self.epsilon,
            self.weight_decay,
            lr_t,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_adam_step() {
        let mut network = TrainableNetwork::new();
        let mut optimizer = Adam::new(&network, 0.001);

        // ダミーの勾配を設定
        network.ft_black.weight_grads[0] = 1.0;
        network.hidden1.weight_grads[0] = 1.0;

        // 初期値を記録
        let ft_before = network.ft_black.weights[0];
        let h1_before = network.hidden1.weights[0];

        // ステップを実行
        optimizer.step(&mut network);

        // 値が更新されていることを確認
        assert_ne!(network.ft_black.weights[0], ft_before);
        assert_ne!(network.hidden1.weights[0], h1_before);
    }
}
