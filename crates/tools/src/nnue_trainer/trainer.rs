//! 学習ループ
//!
//! エポック単位での学習を管理する。

use super::dataset::TrainingDataset;
use super::network::TrainableNetwork;
use super::optimizer::Optimizer;
use indicatif::{ProgressBar, ProgressStyle};
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;
use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// 学習設定
pub struct TrainConfig {
    /// バッチサイズ
    pub batch_size: usize,
    /// エポック数
    pub epochs: usize,
    /// 学習率（eta1と同義、後方互換性のため残す）
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
    /// FV_SCALE: 評価値のスケーリング係数
    ///
    /// NNUEの出力を最終的な評価値に変換する係数。
    /// 推論時の engine-core 側 FV_SCALE と同じ値を使用する必要がある。
    /// 訓練時の選択によって値が決まる（例: 水匠5=24, YaneuraOuデフォルト=16）。
    pub fv_scale: f32,

    // === 学習率スケジューリング (eta1/eta2/eta3方式) ===
    /// eta1: 初期学習率
    pub eta1: f32,
    /// eta2: 中間学習率
    pub eta2: f32,
    /// eta3: 最終学習率
    pub eta3: f32,
    /// eta1_epoch: eta1→eta2への遷移が完了するエポック (0の場合はeta1固定)
    pub eta1_epoch: usize,
    /// eta2_epoch: eta2→eta3への遷移が完了するエポック (0の場合はeta2固定)
    pub eta2_epoch: usize,

    // === Newbobスケジューリング ===
    /// Newbob decay: 検証損失が改善しない場合の学習率減衰率 (1.0の場合は無効)
    pub newbob_decay: f32,
    /// Newbob trials: 最大試行回数
    pub newbob_num_trials: usize,

    // === リジューム ===
    /// 既存モデルからのリジュームパス
    pub resume_path: Option<String>,
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
            fv_scale: 24.0, // HalfKP (水匠5互換) のデフォルト値
            // 学習率スケジューリング（デフォルトは単一学習率）
            eta1: 0.001,
            eta2: 0.001,
            eta3: 0.001,
            eta1_epoch: 0,
            eta2_epoch: 0,
            // Newbob（デフォルトは無効）
            newbob_decay: 1.0,
            newbob_num_trials: 2,
            // リジューム（デフォルトはなし）
            resume_path: None,
        }
    }
}

/// 学習率スケジューラ
pub struct LearningRateScheduler {
    eta1: f32,
    eta2: f32,
    eta3: f32,
    eta1_epoch: usize,
    eta2_epoch: usize,
}

impl LearningRateScheduler {
    /// 新しいスケジューラを作成
    ///
    /// # パラメータ
    /// - `eta1`: 初期学習率
    /// - `eta2`: 中間学習率（eta1_epoch時点）
    /// - `eta3`: 最終学習率（eta2_epoch時点）
    /// - `eta1_epoch`: eta1→eta2遷移完了エポック（0の場合はeta1固定）
    /// - `eta2_epoch`: eta2→eta3遷移完了エポック（0の場合はeta2固定）
    ///
    /// # 注意
    /// eta1_epoch > 0 かつ eta2_epoch > 0 の場合、eta1_epoch <= eta2_epoch でなければ警告を出力
    pub fn new(eta1: f32, eta2: f32, eta3: f32, eta1_epoch: usize, eta2_epoch: usize) -> Self {
        // バリデーション: eta1_epochとeta2_epochの関係チェック
        if eta1_epoch > 0 && eta2_epoch > 0 && eta1_epoch > eta2_epoch {
            eprintln!(
                "Warning: eta1_epoch ({eta1_epoch}) > eta2_epoch ({eta2_epoch}). \
                 This may cause unexpected learning rate behavior."
            );
        }
        Self {
            eta1,
            eta2,
            eta3,
            eta1_epoch,
            eta2_epoch,
        }
    }

    /// 単一学習率のスケジューラを作成
    pub fn constant(lr: f32) -> Self {
        Self::new(lr, lr, lr, 0, 0)
    }

    /// エポックに応じた学習率を計算
    ///
    /// YaneuraOuの実装に基づく：
    /// - epoch < eta1_epoch: eta1 → eta2 を線形補間
    /// - eta1_epoch <= epoch < eta2_epoch: eta2 → eta3 を線形補間
    /// - epoch >= eta2_epoch: eta3
    pub fn get_lr(&self, epoch: usize) -> f32 {
        if self.eta1_epoch == 0 {
            // eta1_epoch == 0 の場合は eta1 固定
            self.eta1
        } else if epoch < self.eta1_epoch {
            // eta1 → eta2 を線形補間
            let t = epoch as f32 / self.eta1_epoch as f32;
            self.eta1 + (self.eta2 - self.eta1) * t
        } else if self.eta2_epoch == 0 {
            // eta2_epoch == 0 の場合は eta2 固定
            self.eta2
        } else if epoch < self.eta2_epoch {
            // eta2 → eta3 を線形補間
            let t = (epoch - self.eta1_epoch) as f32 / (self.eta2_epoch - self.eta1_epoch) as f32;
            self.eta2 + (self.eta3 - self.eta2) * t
        } else {
            self.eta3
        }
    }
}

/// Newbobスケジューラの状態
///
/// YaneuraOuのNewbob実装に基づく：
/// - 検証損失が改善 → best更新、trials リセット
/// - 検証損失が悪化 → 最良モデルをリストア、学習率を減衰
/// - trialsが0になったら収束として学習終了
///
/// 参考: YaneuraOu/source/learn/learner.cpp (2166-2200行付近)
pub struct NewbobState {
    /// 現在のスケール
    pub scale: f32,
    /// 減衰率
    decay: f32,
    /// 最大試行回数
    max_trials: usize,
    /// 残り試行回数
    trials_left: usize,
    /// 最良の検証損失
    best_loss: f32,
    /// 最良モデルのパス
    best_model_path: Option<String>,
}

impl NewbobState {
    /// 新しいNewbob状態を作成
    pub fn new(decay: f32, num_trials: usize) -> Self {
        Self {
            scale: 1.0,
            decay,
            max_trials: num_trials,
            trials_left: num_trials,
            best_loss: f32::MAX,
            best_model_path: None,
        }
    }

    /// Newbobが有効かどうか
    pub fn is_enabled(&self) -> bool {
        self.decay < 1.0
    }

    /// 検証損失に基づいて更新
    ///
    /// Returns: (accepted, converged, should_restore)
    /// - accepted: 損失が改善したか
    /// - converged: 収束したか（試行回数が尽きた）
    /// - should_restore: 最良モデルへのリストアが必要か
    pub fn update(&mut self, current_loss: f32, model_path: &str) -> (bool, bool, bool) {
        if !self.is_enabled() {
            return (true, false, false);
        }

        if current_loss < self.best_loss {
            // 改善した
            eprintln!("  Newbob: loss {current_loss:.6} < best {:.6}, accepted", self.best_loss);
            self.best_loss = current_loss;
            self.best_model_path = Some(model_path.to_string());
            self.trials_left = self.max_trials; // 試行回数をリセット
            (true, false, false)
        } else {
            // 改善しなかった
            eprintln!("  Newbob: loss {current_loss:.6} >= best {:.6}, rejected", self.best_loss);

            self.trials_left = self.trials_left.saturating_sub(1);

            if self.trials_left == 0 {
                eprintln!("  Newbob: converged");
                return (false, true, false);
            }

            // 学習率を減衰
            self.scale *= self.decay;
            eprintln!(
                "  Newbob: reducing scale to {:.4} ({} trials left)",
                self.scale, self.trials_left
            );

            // 最良モデルがあればリストアが必要
            let should_restore = self.best_model_path.is_some();
            (false, false, should_restore)
        }
    }

    /// 最良モデルのパスを取得
    pub fn best_model_path(&self) -> Option<&str> {
        self.best_model_path.as_deref()
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
    lr_scheduler: LearningRateScheduler,
    newbob_state: NewbobState,
}

impl Trainer {
    /// 新しいトレーナーを作成
    pub fn new(config: TrainConfig) -> std::io::Result<Self> {
        let mut rng = ChaCha8Rng::seed_from_u64(config.seed);

        // ネットワークの初期化（リジュームまたはランダム）
        let network = if let Some(ref path) = config.resume_path {
            eprintln!("Resuming from: {path}");
            let file = File::open(path)?;
            let mut reader = BufReader::new(file);
            TrainableNetwork::load(&mut reader)?
        } else {
            let mut network = TrainableNetwork::new();
            network.init_random(&mut rng);
            network
        };

        // 学習率スケジューラの作成
        let lr_scheduler = if config.eta1_epoch > 0 || config.eta2_epoch > 0 {
            LearningRateScheduler::new(
                config.eta1,
                config.eta2,
                config.eta3,
                config.eta1_epoch,
                config.eta2_epoch,
            )
        } else {
            LearningRateScheduler::constant(config.learning_rate)
        };

        // Newbob状態の作成
        let newbob_state = NewbobState::new(config.newbob_decay, config.newbob_num_trials);

        Ok(Self {
            config,
            network,
            rng,
            interrupted: Arc::new(AtomicBool::new(false)),
            lr_scheduler,
            newbob_state,
        })
    }

    /// 中断フラグを取得
    pub fn interrupted(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.interrupted)
    }

    /// 学習を実行
    pub fn train<O: Optimizer>(
        &mut self,
        dataset: &mut TrainingDataset,
        validation: Option<&TrainingDataset>,
        optimizer: &mut O,
    ) {
        eprintln!("Training with {} samples", dataset.len());
        if let Some(val) = validation {
            eprintln!("Validation with {} samples", val.len());
        }
        eprintln!("  Batch size: {}", self.config.batch_size);
        eprintln!("  Epochs: {}", self.config.epochs);
        eprintln!("  Parameters: {}", self.network.param_count());

        // 学習率スケジューリング情報の表示
        if self.config.eta1_epoch > 0 || self.config.eta2_epoch > 0 {
            eprintln!(
                "  LR schedule: eta1={} (epoch 0-{}), eta2={} (epoch {}-{}), eta3={}",
                self.config.eta1,
                self.config.eta1_epoch,
                self.config.eta2,
                self.config.eta1_epoch,
                self.config.eta2_epoch,
                self.config.eta3
            );
        } else {
            eprintln!("  Learning rate: {}", self.config.learning_rate);
        }

        // Newbob情報の表示
        if self.newbob_state.is_enabled() {
            eprintln!(
                "  Newbob: decay={}, trials={}",
                self.config.newbob_decay, self.config.newbob_num_trials
            );
        }

        for epoch in 0..self.config.epochs {
            if self.interrupted.load(Ordering::SeqCst) {
                eprintln!("\nInterrupted at epoch {epoch}");
                break;
            }

            // 学習率の更新
            let base_lr = self.lr_scheduler.get_lr(epoch);
            let effective_lr = base_lr * self.newbob_state.scale;
            optimizer.set_lr(effective_lr);

            // シャッフル
            dataset.shuffle(&mut self.rng);

            // エポックの学習
            let (train_loss, samples_processed) = self.train_epoch(dataset, optimizer, epoch);

            // 検証損失の計算
            let val_loss = validation.map(|val| self.compute_validation_loss(val));

            // ログ出力
            if let Some(vl) = val_loss {
                eprintln!(
                    "Epoch {}/{}: lr={:.6}, train_loss={:.6}, val_loss={:.6}, samples={}",
                    epoch + 1,
                    self.config.epochs,
                    effective_lr,
                    train_loss,
                    vl,
                    samples_processed
                );
            } else {
                eprintln!(
                    "Epoch {}/{}: lr={:.6}, loss={:.6}, samples={}",
                    epoch + 1,
                    self.config.epochs,
                    effective_lr,
                    train_loss,
                    samples_processed
                );
            }

            // チェックポイント保存
            if (epoch + 1) % self.config.checkpoint_interval == 0 {
                let path = format!("{}/nnue_epoch_{}.bin", self.config.output_dir, epoch + 1);
                if let Err(e) = self.save_model(&path) {
                    eprintln!("Failed to save checkpoint: {e}");
                } else {
                    eprintln!("Saved checkpoint: {path}");

                    // Newbobの更新（検証損失がある場合）
                    if let Some(vl) = val_loss {
                        let (_, converged, should_restore) = self.newbob_state.update(vl, &path);

                        // 損失が悪化した場合、最良モデルをリストア
                        if should_restore {
                            if let Some(best_path) = self.newbob_state.best_model_path() {
                                let best_path = best_path.to_string(); // Clone to avoid borrow issue
                                eprintln!("  Restoring parameters from {best_path}");
                                match self.restore_model(&best_path) {
                                    Ok(()) => eprintln!("  Restored successfully"),
                                    Err(e) => eprintln!("  Warning: failed to restore: {e}"),
                                }
                            }
                        }

                        if converged {
                            eprintln!("Newbob converged, stopping training");
                            break;
                        }
                    }
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

        // Newbobが有効で最良モデルがある場合、それを報告
        if let Some(best_path) = self.newbob_state.best_model_path() {
            eprintln!("Best model (by validation loss): {best_path}");
        }
    }

    /// 検証損失を計算
    fn compute_validation_loss(&self, validation: &TrainingDataset) -> f32 {
        let mut total_loss = 0.0;
        let mut total_samples = 0;

        for batch in validation.batches(self.config.batch_size) {
            for sample in &batch.samples {
                let (output, _) = self.network.forward(
                    &sample.black_features,
                    &sample.white_features,
                    sample.side_to_move,
                );

                let predicted = output / self.config.fv_scale;
                let target = sample.target_score / self.config.fv_scale;

                let loss = match self.config.loss_type {
                    LossType::Mse => {
                        let diff = predicted - target;
                        diff * diff
                    }
                    LossType::SigmoidCrossEntropy => {
                        const EPS: f32 = 1e-7;
                        let pred_wr =
                            sigmoid(predicted / self.config.eval_scale).clamp(EPS, 1.0 - EPS);
                        let target_wr =
                            sigmoid(target / self.config.eval_scale).clamp(EPS, 1.0 - EPS);
                        -target_wr * pred_wr.ln() - (1.0 - target_wr) * (1.0 - pred_wr).ln()
                    }
                };

                total_loss += loss;
                total_samples += 1;
            }
        }

        if total_samples > 0 {
            total_loss / total_samples as f32
        } else {
            0.0
        }
    }

    /// 1エポックの学習
    fn train_epoch<O: Optimizer>(
        &mut self,
        dataset: &TrainingDataset,
        optimizer: &mut O,
        _epoch: usize,
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
        let batch_size = batch.samples.len() as f32;

        for sample in &batch.samples {
            // 順伝播
            let (output, cache) = self.network.forward(
                &sample.black_features,
                &sample.white_features,
                sample.side_to_move,
            );

            // スケーリング後の評価値
            let fv_scale = self.config.fv_scale;
            let predicted = output / fv_scale;
            let target = sample.target_score / fv_scale;

            // 損失計算
            let (loss, grad) = match self.config.loss_type {
                LossType::Mse => {
                    let diff = predicted - target;
                    (diff * diff, 2.0 * diff / fv_scale)
                }
                LossType::SigmoidCrossEntropy => {
                    // 勝率への変換（数値安定性のためクランプ）
                    const EPS: f32 = 1e-7;
                    let pred_wr = sigmoid(predicted / self.config.eval_scale).clamp(EPS, 1.0 - EPS);
                    let target_wr = sigmoid(target / self.config.eval_scale).clamp(EPS, 1.0 - EPS);

                    // バイナリ交差エントロピー
                    let loss = -target_wr * pred_wr.ln() - (1.0 - target_wr) * (1.0 - pred_wr).ln();

                    // 勾配: d(loss)/d(output)
                    let grad = (pred_wr - target_wr) / (self.config.eval_scale * fv_scale);

                    (loss, grad)
                }
            };

            total_loss += loss;

            // 逆伝播（勾配をバッチサイズで正規化）
            self.network.backward(
                &sample.black_features,
                &sample.white_features,
                sample.side_to_move,
                &cache,
                grad / batch_size,
            );
        }

        total_loss / batch_size
    }

    /// モデルを保存
    pub fn save_model<P: AsRef<Path>>(&self, path: P) -> std::io::Result<()> {
        let file = File::create(path)?;
        let mut writer = BufWriter::new(file);
        self.network.save(&mut writer)?;
        Ok(())
    }

    /// モデルをリストア（Newbobでの最良モデル復元用）
    ///
    /// YaneuraOuのRestoreParameters相当の機能
    fn restore_model(&mut self, path: &str) -> std::io::Result<()> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);
        self.network = TrainableNetwork::load(&mut reader)?;
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

    #[test]
    fn test_lr_scheduler_constant() {
        let scheduler = LearningRateScheduler::constant(0.001);
        assert!((scheduler.get_lr(0) - 0.001).abs() < 1e-9);
        assert!((scheduler.get_lr(50) - 0.001).abs() < 1e-9);
        assert!((scheduler.get_lr(100) - 0.001).abs() < 1e-9);
    }

    #[test]
    fn test_lr_scheduler_two_phase() {
        // eta1 = 0.01, eta2 = 0.001 @ epoch 100
        let scheduler = LearningRateScheduler::new(0.01, 0.001, 0.001, 100, 0);

        // epoch 0: eta1
        assert!((scheduler.get_lr(0) - 0.01).abs() < 1e-6);

        // epoch 50: 中間値
        let expected = 0.01 + (0.001 - 0.01) * 0.5;
        assert!((scheduler.get_lr(50) - expected).abs() < 1e-6);

        // epoch 100: eta2
        assert!((scheduler.get_lr(100) - 0.001).abs() < 1e-6);

        // epoch 200: eta2固定（eta2_epoch == 0）
        assert!((scheduler.get_lr(200) - 0.001).abs() < 1e-6);
    }

    #[test]
    fn test_lr_scheduler_three_phase() {
        // eta1 = 0.01 (epoch 0-100), eta2 = 0.001 (epoch 100-200), eta3 = 0.0001
        let scheduler = LearningRateScheduler::new(0.01, 0.001, 0.0001, 100, 200);

        // epoch 0: eta1
        assert!((scheduler.get_lr(0) - 0.01).abs() < 1e-6);

        // epoch 100: eta2
        assert!((scheduler.get_lr(100) - 0.001).abs() < 1e-6);

        // epoch 150: eta2 → eta3 中間
        let expected = 0.001 + (0.0001 - 0.001) * 0.5;
        assert!((scheduler.get_lr(150) - expected).abs() < 1e-6);

        // epoch 200: eta3
        assert!((scheduler.get_lr(200) - 0.0001).abs() < 1e-6);

        // epoch 300: eta3固定
        assert!((scheduler.get_lr(300) - 0.0001).abs() < 1e-6);
    }

    #[test]
    fn test_newbob_disabled() {
        // decay = 1.0 の場合は無効
        let mut newbob = NewbobState::new(1.0, 2);
        assert!(!newbob.is_enabled());

        let (accepted, converged, should_restore) = newbob.update(1.0, "/tmp/model.bin");
        assert!(accepted);
        assert!(!converged);
        assert!(!should_restore);
    }

    #[test]
    fn test_newbob_improvement() {
        let mut newbob = NewbobState::new(0.5, 3);
        assert!(newbob.is_enabled());

        // 最初の改善
        let (accepted, converged, should_restore) = newbob.update(0.5, "/tmp/model1.bin");
        assert!(accepted);
        assert!(!converged);
        assert!(!should_restore);
        assert!((newbob.scale - 1.0).abs() < 1e-6);
        assert_eq!(newbob.best_model_path(), Some("/tmp/model1.bin"));

        // 2回目の改善
        let (accepted, converged, should_restore) = newbob.update(0.4, "/tmp/model2.bin");
        assert!(accepted);
        assert!(!converged);
        assert!(!should_restore);
        assert_eq!(newbob.best_model_path(), Some("/tmp/model2.bin"));
    }

    #[test]
    fn test_newbob_no_improvement() {
        let mut newbob = NewbobState::new(0.5, 2);

        // 初期値を設定
        newbob.update(0.5, "/tmp/model1.bin");

        // 改善なし（1回目）→ リストアが必要
        let (accepted, converged, should_restore) = newbob.update(0.6, "/tmp/model2.bin");
        assert!(!accepted);
        assert!(!converged);
        assert!(should_restore); // 最良モデルへのリストアが必要
        assert!((newbob.scale - 0.5).abs() < 1e-6);

        // 改善なし（2回目）→ 収束
        let (accepted, converged, _should_restore) = newbob.update(0.7, "/tmp/model3.bin");
        assert!(!accepted);
        assert!(converged);
    }
}
