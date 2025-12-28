//! 学習ループ
//!
//! エポック単位での学習を管理する。

use super::dataset::TrainingDataset;
use super::network::{TrainableNetwork, FV_SCALE};
use super::optimizer::Optimizer;
use indicatif::{ProgressBar, ProgressStyle};
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// 学習設定
pub struct TrainConfig {
    /// バッチサイズ
    pub batch_size: usize,
    /// エポック数
    pub epochs: usize,
    /// 学習率
    pub learning_rate: f32,
    /// 重み減衰
    pub weight_decay: f32,
    /// シード値
    pub seed: u64,
    /// 損失関数の種類
    pub loss_type: LossType,
    /// 評価値のスケーリング（勝率変換用）
    pub eval_scale: f32,
    /// チェックポイント保存間隔（エポック単位）
    pub checkpoint_interval: usize,
    /// 出力ディレクトリ
    pub output_dir: String,
}

impl Default for TrainConfig {
    fn default() -> Self {
        Self {
            batch_size: 16384,
            epochs: 100,
            learning_rate: 0.001,
            weight_decay: 0.0,
            seed: 42,
            loss_type: LossType::Mse,
            eval_scale: 600.0, // 勝率50%が0cpの場合のスケール
            checkpoint_interval: 10,
            output_dir: ".".to_string(),
        }
    }
}

/// 損失関数の種類
#[derive(Clone, Copy, Debug)]
pub enum LossType {
    /// 平均二乗誤差
    Mse,
    /// シグモイド交差エントロピー（勝率ベース）
    SigmoidCrossEntropy,
}

/// トレーナー
pub struct Trainer {
    config: TrainConfig,
    network: TrainableNetwork,
    rng: ChaCha8Rng,
    interrupted: Arc<AtomicBool>,
}

impl Trainer {
    /// 新しいトレーナーを作成
    pub fn new(config: TrainConfig) -> Self {
        let mut rng = ChaCha8Rng::seed_from_u64(config.seed);
        let mut network = TrainableNetwork::new();
        network.init_random(&mut rng);

        Self {
            config,
            network,
            rng,
            interrupted: Arc::new(AtomicBool::new(false)),
        }
    }

    /// 中断フラグを取得
    pub fn interrupted(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.interrupted)
    }

    /// 学習を実行
    pub fn train<O: Optimizer>(&mut self, dataset: &mut TrainingDataset, optimizer: &mut O) {
        eprintln!("Training with {} samples", dataset.len());
        eprintln!("  Batch size: {}", self.config.batch_size);
        eprintln!("  Epochs: {}", self.config.epochs);
        eprintln!("  Learning rate: {}", self.config.learning_rate);
        eprintln!("  Parameters: {}", self.network.param_count());

        for epoch in 0..self.config.epochs {
            if self.interrupted.load(Ordering::SeqCst) {
                eprintln!("\nInterrupted at epoch {epoch}");
                break;
            }

            // シャッフル
            dataset.shuffle(&mut self.rng);

            // エポックの学習
            let (avg_loss, samples_processed) = self.train_epoch(dataset, optimizer, epoch);

            eprintln!(
                "Epoch {}/{}: loss={:.6}, samples={}",
                epoch + 1,
                self.config.epochs,
                avg_loss,
                samples_processed
            );

            // チェックポイント保存
            if (epoch + 1) % self.config.checkpoint_interval == 0 {
                let path = format!("{}/nnue_epoch_{}.bin", self.config.output_dir, epoch + 1);
                if let Err(e) = self.save_model(&path) {
                    eprintln!("Failed to save checkpoint: {e}");
                } else {
                    eprintln!("Saved checkpoint: {path}");
                }
            }
        }

        // 最終モデルを保存
        let final_path = format!("{}/nnue_final.bin", self.config.output_dir);
        if let Err(e) = self.save_model(&final_path) {
            eprintln!("Failed to save final model: {e}");
        } else {
            eprintln!("Saved final model: {final_path}");
        }
    }

    /// 1エポックの学習
    fn train_epoch<O: Optimizer>(
        &mut self,
        dataset: &TrainingDataset,
        optimizer: &mut O,
        epoch: usize,
    ) -> (f32, usize) {
        let num_batches = dataset.len().div_ceil(self.config.batch_size);

        let progress = ProgressBar::new(num_batches as u64);
        progress.set_style(
            ProgressStyle::default_bar()
                .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} loss:{msg}")
                .expect("valid template"),
        );

        let mut total_loss = 0.0;
        let mut total_samples = 0;

        for (batch_idx, batch) in dataset.batches(self.config.batch_size).enumerate() {
            if self.interrupted.load(Ordering::SeqCst) {
                break;
            }

            // 勾配をゼロにリセット
            self.network.zero_grad();

            // バッチの損失を計算
            let batch_loss = self.compute_batch_loss(&batch);

            // オプティマイザでパラメータを更新
            optimizer.step(&mut self.network);

            total_loss += batch_loss * batch.samples.len() as f32;
            total_samples += batch.samples.len();

            // 進捗表示
            if batch_idx % 10 == 0 {
                let avg_loss = total_loss / total_samples as f32;
                progress.set_message(format!("{avg_loss:.6}"));
            }
            progress.inc(1);
        }

        progress.finish();
        let _ = epoch; // 使用済みマーク

        let avg_loss = if total_samples > 0 {
            total_loss / total_samples as f32
        } else {
            0.0
        };

        (avg_loss, total_samples)
    }

    /// バッチの損失を計算（勾配も累積）
    fn compute_batch_loss(&mut self, batch: &super::dataset::TrainingBatch) -> f32 {
        let mut total_loss = 0.0;

        for sample in &batch.samples {
            // 順伝播
            let (output, cache) = self.network.forward(
                &sample.black_features,
                &sample.white_features,
                sample.side_to_move,
            );

            // スケーリング後の評価値
            let predicted = output / FV_SCALE;
            let target = sample.target_score / FV_SCALE;

            // 損失計算
            let (loss, grad) = match self.config.loss_type {
                LossType::Mse => {
                    let diff = predicted - target;
                    (diff * diff, 2.0 * diff / FV_SCALE)
                }
                LossType::SigmoidCrossEntropy => {
                    // 勝率への変換
                    let pred_wr = sigmoid(predicted / self.config.eval_scale);
                    let target_wr = sigmoid(target / self.config.eval_scale);

                    // バイナリ交差エントロピー
                    let loss = -target_wr * pred_wr.ln() - (1.0 - target_wr) * (1.0 - pred_wr).ln();

                    // 勾配: d(loss)/d(output)
                    let grad = (pred_wr - target_wr) / (self.config.eval_scale * FV_SCALE);

                    (loss, grad)
                }
            };

            total_loss += loss;

            // 逆伝播
            self.network.backward(
                &sample.black_features,
                &sample.white_features,
                sample.side_to_move,
                &cache,
                grad,
            );
        }

        total_loss / batch.samples.len() as f32
    }

    /// モデルを保存
    pub fn save_model<P: AsRef<Path>>(&self, path: P) -> std::io::Result<()> {
        let file = File::create(path)?;
        let mut writer = BufWriter::new(file);
        self.network.save(&mut writer)?;
        Ok(())
    }

    /// ネットワークへの参照を取得
    pub fn network(&self) -> &TrainableNetwork {
        &self.network
    }
}

/// シグモイド関数
#[inline]
fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sigmoid() {
        assert!((sigmoid(0.0) - 0.5).abs() < 1e-6);
        assert!(sigmoid(100.0) > 0.99);
        assert!(sigmoid(-100.0) < 0.01);
    }
}
