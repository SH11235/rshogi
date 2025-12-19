//! NNUEネットワーク全体の構造と評価関数
//!
//! HalfKP 256x2-32-32 アーキテクチャを想定した NNUE ネットワークを表現する。
//! - `FeatureTransformer` で HalfKP 特徴量を 512 次元に変換
//! - `AffineTransform` + `ClippedReLU` を 2 層適用して 32→32 と圧縮
//! - 出力層（32→1）で整数スコアを得て `FV_SCALE` でスケーリングし `Value` に変換
//! - グローバルな `NETWORK` にロードし、`evaluate` から利用する

use super::accumulator::{Accumulator, AccumulatorStack};
use super::constants::{
    FV_SCALE, HIDDEN1_DIMENSIONS, HIDDEN2_DIMENSIONS, NNUE_VERSION, OUTPUT_DIMENSIONS,
    TRANSFORMED_FEATURE_DIMENSIONS,
};
use super::feature_transformer::FeatureTransformer;
use super::layers::{AffineTransform, ClippedReLU};
use crate::eval::material;
use crate::position::Position;
use crate::types::Value;
use std::fs::File;
use std::io::{self, Cursor, Read};
use std::path::Path;
use std::sync::OnceLock;

/// グローバルなNNUEネットワーク
static NETWORK: OnceLock<Network> = OnceLock::new();

/// NNUEネットワーク全体
pub struct Network {
    /// 特徴量変換器
    pub feature_transformer: FeatureTransformer,
    /// 隠れ層1: 512 → 32
    pub hidden1: AffineTransform<{ TRANSFORMED_FEATURE_DIMENSIONS * 2 }, HIDDEN1_DIMENSIONS>,
    /// 隠れ層2: 32 → 32
    pub hidden2: AffineTransform<HIDDEN1_DIMENSIONS, HIDDEN2_DIMENSIONS>,
    /// 出力層: 32 → 1
    pub output: AffineTransform<HIDDEN2_DIMENSIONS, OUTPUT_DIMENSIONS>,
}

impl Network {
    /// ファイルから読み込み
    pub fn load<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let mut file = File::open(path)?;
        Self::read(&mut file)
    }

    /// リーダーから読み込み
    pub fn read<R: Read>(reader: &mut R) -> io::Result<Self> {
        // ヘッダを読み込み
        let mut buf4 = [0u8; 4];
        reader.read_exact(&mut buf4)?;
        let version = u32::from_le_bytes(buf4);

        if version != NNUE_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Invalid NNUE version: {version:#x}, expected {NNUE_VERSION:#x}"),
            ));
        }

        // 構造ハッシュを読み込み（検証はスキップ）
        reader.read_exact(&mut buf4)?;
        let _hash = u32::from_le_bytes(buf4);

        // アーキテクチャ文字列を読み込み
        reader.read_exact(&mut buf4)?;
        let arch_len = u32::from_le_bytes(buf4) as usize;
        let mut arch = vec![0u8; arch_len];
        reader.read_exact(&mut arch)?;

        // パラメータを読み込み
        let feature_transformer = FeatureTransformer::read(reader)?;
        let hidden1 = AffineTransform::read(reader)?;
        let hidden2 = AffineTransform::read(reader)?;
        let output = AffineTransform::read(reader)?;

        Ok(Self {
            feature_transformer,
            hidden1,
            hidden2,
            output,
        })
    }

    /// 評価値を計算
    pub fn evaluate(&self, pos: &Position, acc: &Accumulator) -> Value {
        // 変換済み特徴量
        let mut transformed = [0u8; TRANSFORMED_FEATURE_DIMENSIONS * 2];
        self.feature_transformer.transform(acc, pos.side_to_move(), &mut transformed);

        // 入力密度の計測（diagnosticsフィーチャー有効時のみ）
        //
        // 計測結果（2025-12-18）:
        //   - hidden1層への入力密度: 約39-42%（安定して~40%）
        //   - サンプル数: 16,900,000+ evaluations
        //   - 結論: 密度40%はスパース最適化には高すぎる。密な行列積方式が正しい選択。
        //
        // 計測コマンド:
        //   RUSTFLAGS="-C target-cpu=native" cargo build -p tools --bin benchmark --release --features engine-core/diagnostics
        //   ./target/release/benchmark --internal --threads 1 --limit-type movetime --limit 10000 --nnue-file path/to/nn.bin
        #[cfg(feature = "diagnostics")]
        {
            use std::sync::atomic::{AtomicU64, Ordering};
            static CALL_COUNT: AtomicU64 = AtomicU64::new(0);
            static TOTAL_NONZERO: AtomicU64 = AtomicU64::new(0);
            static TOTAL_ELEMENTS: AtomicU64 = AtomicU64::new(0);

            let nonzero = transformed.iter().filter(|&&x| x != 0).count() as u64;
            let elements = transformed.len() as u64;

            TOTAL_NONZERO.fetch_add(nonzero, Ordering::Relaxed);
            TOTAL_ELEMENTS.fetch_add(elements, Ordering::Relaxed);
            let count = CALL_COUNT.fetch_add(1, Ordering::Relaxed) + 1;

            // 100000回ごとにログ出力
            if count.is_multiple_of(100000) {
                let total_nz = TOTAL_NONZERO.load(Ordering::Relaxed);
                let total_el = TOTAL_ELEMENTS.load(Ordering::Relaxed);
                let density = total_nz as f64 / total_el as f64 * 100.0;
                eprintln!(
                    "[NNUE density] hidden1 input: {total_nz}/{total_el} nonzero ({density:.1}%) over {count} evals"
                );
            }
        }

        // 隠れ層1
        let mut hidden1_out = [0i32; HIDDEN1_DIMENSIONS];
        self.hidden1.propagate(&transformed, &mut hidden1_out);

        let mut hidden1_relu = [0u8; HIDDEN1_DIMENSIONS];
        ClippedReLU::propagate(&hidden1_out, &mut hidden1_relu);

        // 隠れ層2
        let mut hidden2_out = [0i32; HIDDEN2_DIMENSIONS];
        self.hidden2.propagate(&hidden1_relu, &mut hidden2_out);

        let mut hidden2_relu = [0u8; HIDDEN2_DIMENSIONS];
        ClippedReLU::propagate(&hidden2_out, &mut hidden2_relu);

        // 出力層
        let mut output = [0i32; OUTPUT_DIMENSIONS];
        self.output.propagate(&hidden2_relu, &mut output);

        // スケーリング
        Value::new(output[0] / FV_SCALE)
    }
}

/// NNUEを初期化
pub fn init_nnue<P: AsRef<Path>>(path: P) -> io::Result<()> {
    let network = Network::load(path)?;
    NETWORK
        .set(network)
        .map_err(|_| io::Error::new(io::ErrorKind::AlreadyExists, "NNUE already initialized"))
}

/// バイト列からNNUEを初期化
pub fn init_nnue_from_bytes(bytes: &[u8]) -> io::Result<()> {
    let mut cursor = Cursor::new(bytes);
    let network = Network::read(&mut cursor)?;
    NETWORK
        .set(network)
        .map_err(|_| io::Error::new(io::ErrorKind::AlreadyExists, "NNUE already initialized"))
}

/// 局面を評価
///
/// NNUEが初期化されていない場合は駒得評価にフォールバック。
/// AccumulatorStack を使って差分更新し、計算済みなら再利用する。
///
/// 遅延評価パターン:
/// 1. 直前局面で差分更新を試行
/// 2. 失敗なら祖先探索 + 複数手差分更新を試行
/// 3. それでも失敗なら全計算
pub fn evaluate(pos: &Position, stack: &mut AccumulatorStack) -> Value {
    if let Some(network) = NETWORK.get() {
        // 差分更新の成功率計測（diagnosticsフィーチャー有効時のみ）
        // 0=cached, 1=diff_success, 2=no_prev, 3=prev_not_computed, 4=update_failed,
        // 5=refresh, 6=ancestor_success
        #[cfg(feature = "diagnostics")]
        let mut diff_update_result: u8 = 0;

        // AccumulatorStack 上の Accumulator をインプレースで更新
        {
            let current_entry = stack.current();
            if !current_entry.accumulator.computed_accumulation {
                let mut updated = false;

                // 1. 直前局面で差分更新を試行
                if let Some(prev_idx) = current_entry.previous {
                    let prev_computed = stack.entry_at(prev_idx).accumulator.computed_accumulation;
                    if prev_computed {
                        // DirtyPieceをコピーして借用を解消
                        let dirty_piece = stack.current().dirty_piece;
                        // Note: clone() + copy_from_slice による二重コピーを避ける最適化を試みたが、
                        // NPSに改善が見られなかった。YaneuraOu の C++ 実装でも同様のパターン
                        // （値コピー + std::memcpy）を使用している。
                        let prev_acc = stack.entry_at(prev_idx).accumulator.clone();
                        let current_acc = &mut stack.current_mut().accumulator;
                        updated = network.feature_transformer.update_accumulator(
                            pos,
                            &dirty_piece,
                            current_acc,
                            &prev_acc,
                        );
                        #[cfg(feature = "diagnostics")]
                        {
                            diff_update_result = if updated { 1 } else { 4 };
                        }
                    } else {
                        #[cfg(feature = "diagnostics")]
                        {
                            diff_update_result = 3; // prev_not_computed
                        }
                    }
                } else {
                    #[cfg(feature = "diagnostics")]
                    {
                        diff_update_result = 2; // no_prev
                    }
                }

                // 2. 失敗なら祖先探索 + 複数手差分更新を試行
                if !updated {
                    if let Some((source_idx, _depth)) = stack.find_usable_accumulator() {
                        updated = network
                            .feature_transformer
                            .forward_update_incremental(pos, stack, source_idx);
                        #[cfg(feature = "diagnostics")]
                        if updated {
                            diff_update_result = 6; // ancestor_success
                        }
                    }
                }

                // 3. それでも失敗なら全計算
                if !updated {
                    let acc = &mut stack.current_mut().accumulator;
                    network.feature_transformer.refresh_accumulator(pos, acc);
                }
            }
            // else: cached (diff_update_result = 0)
        }

        // 差分更新の成功率をログ出力（diagnosticsフィーチャー有効時のみ）
        #[cfg(feature = "diagnostics")]
        {
            use std::sync::atomic::{AtomicU64, Ordering};
            static TOTAL_EVALS: AtomicU64 = AtomicU64::new(0);
            static CACHED: AtomicU64 = AtomicU64::new(0);
            static DIFF_SUCCESS: AtomicU64 = AtomicU64::new(0);
            static ANCESTOR_SUCCESS: AtomicU64 = AtomicU64::new(0);
            static NO_PREV: AtomicU64 = AtomicU64::new(0);
            static PREV_NOT_COMPUTED: AtomicU64 = AtomicU64::new(0);
            static UPDATE_FAILED: AtomicU64 = AtomicU64::new(0);

            match diff_update_result {
                0 => {
                    CACHED.fetch_add(1, Ordering::Relaxed);
                }
                1 => {
                    DIFF_SUCCESS.fetch_add(1, Ordering::Relaxed);
                }
                2 => {
                    NO_PREV.fetch_add(1, Ordering::Relaxed);
                }
                3 => {
                    PREV_NOT_COMPUTED.fetch_add(1, Ordering::Relaxed);
                }
                4 | 5 => {
                    UPDATE_FAILED.fetch_add(1, Ordering::Relaxed);
                }
                6 => {
                    ANCESTOR_SUCCESS.fetch_add(1, Ordering::Relaxed);
                }
                _ => {}
            }

            let count = TOTAL_EVALS.fetch_add(1, Ordering::Relaxed) + 1;

            // 100000回ごとにログ出力
            if count.is_multiple_of(100000) {
                let cached = CACHED.load(Ordering::Relaxed);
                let diff_ok = DIFF_SUCCESS.load(Ordering::Relaxed);
                let ancestor_ok = ANCESTOR_SUCCESS.load(Ordering::Relaxed);
                let no_prev = NO_PREV.load(Ordering::Relaxed);
                let prev_nc = PREV_NOT_COMPUTED.load(Ordering::Relaxed);
                let upd_fail = UPDATE_FAILED.load(Ordering::Relaxed);

                let need_compute = count - cached;
                let total_diff_ok = diff_ok + ancestor_ok;
                let diff_rate = if need_compute > 0 {
                    total_diff_ok as f64 / need_compute as f64 * 100.0
                } else {
                    0.0
                };
                // refresh = 全計算が必要だった回数 = 計算が必要な回数 - 差分更新成功回数
                let refresh_count = need_compute - total_diff_ok;
                let refresh_rate = if need_compute > 0 {
                    refresh_count as f64 / need_compute as f64 * 100.0
                } else {
                    0.0
                };

                eprintln!(
                    "[NNUE diff] total={count} cached={cached} | need_compute={need_compute} diff_ok={total_diff_ok}({diff_rate:.1}%) refresh={refresh_rate:.1}% | direct={diff_ok} ancestor={ancestor_ok} no_prev={no_prev} prev_nc={prev_nc} upd_fail={upd_fail}"
                );
            }
        }

        // 不変借用で評価
        let acc_ref = &stack.current().accumulator;
        network.evaluate(pos, acc_ref)
    } else {
        // フォールバック: Material評価
        material::evaluate_material(pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::position::SFEN_HIRATE;

    #[test]
    fn test_evaluate_fallback() {
        let mut pos = Position::new();
        pos.set_sfen(SFEN_HIRATE).unwrap();
        let mut stack = AccumulatorStack::new();

        // NNUEが初期化されていない場合はフォールバック
        let value = evaluate(&pos, &mut stack);

        // フォールバック評価が動作することを確認
        assert!(value.raw().abs() < 1000);
    }

    #[test]
    fn test_accumulator_cached_after_evaluate() {
        // AccumulatorStack を使った評価キャッシュのテスト。
        // 評価後に AccumulatorStack の Accumulator が computed_accumulation = true で残り、
        // 再度 evaluate を呼んでもフラグが維持されることを確認する。

        let mut pos = Position::new();
        pos.set_sfen(SFEN_HIRATE).unwrap();
        let mut stack = AccumulatorStack::new();

        // 手動で accumulator を計算済みにする
        stack.current_mut().accumulator.computed_accumulation = true;

        // 1回目の evaluate: computed_accumulation が true のままならそのまま評価する
        let value1 = evaluate(&pos, &mut stack);
        assert!(stack.current().accumulator.computed_accumulation);

        // 2回目もフラグが維持されていることを確認
        let value2 = evaluate(&pos, &mut stack);
        assert!(stack.current().accumulator.computed_accumulation);

        // フォールバックの駒得評価は手番に依存して符号が変わる可能性があるが、
        // ここでは「計算が成功し、フラグが維持された」ことのみ検証する。
        let _ = (value1, value2);
    }
}
