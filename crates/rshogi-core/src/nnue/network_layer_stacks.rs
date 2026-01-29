//! NetworkLayerStacks - LayerStacksアーキテクチャのNNUEネットワーク
//!
//! HalfKA_hm^ 特徴量 + LayerStacks 構造の NNUE を実装する。
//! nnue-pytorch で学習したファイルを読み込み、評価を行う。
//!
//! ## アーキテクチャ
//!
//! ```text
//! Feature Transformer (HalfKA_hm^): 73,305 → 1536 (各視点)
//! 視点結合: 両視点を連結 → 3072
//! SqrClippedReLU: 3072 → 1536
//! LayerStacks (両玉の相対段ベースの9バケット選択後):
//!   L1: 1536 → 16
//!   SqrReLU + concat: 30
//!   L2: 30 → 32
//!   Output: 32 → 1 + skip
//! ```
//!
//! ## バケット選択
//!
//! 両玉の相対段（0-8）に基づいて9個のバケットから1つを選択：
//! - 味方玉の段: 0-2 → 0, 3-5 → 3, 6-8 → 6
//! - 相手玉の段: 0-2 → 0, 3-5 → 1, 6-8 → 2
//! - bucket = f_index + e_index (0-8)

use super::accumulator_layer_stacks::{AccumulatorLayerStacks, AccumulatorStackLayerStacks};
use super::constants::{MAX_ARCH_LEN, NNUE_PYTORCH_L1, NNUE_VERSION, NNUE_VERSION_HALFKA};
use super::feature_transformer_layer_stacks::FeatureTransformerLayerStacks;
use super::layer_stacks::{compute_bucket_index, sqr_clipped_relu_transform, LayerStacks};
use crate::position::Position;
use crate::types::{Color, Value};
#[cfg(feature = "diagnostics")]
use log::info;
use std::fs::File;
use std::io::{self, BufReader, Cursor, Read, Seek};
use std::path::Path;

/// LayerStacksアーキテクチャのNNUEネットワーク
///
/// HalfKA_hm^ 特徴量（73,305次元）+ 1536次元 Feature Transformer + 9バケット LayerStacks
pub struct NetworkLayerStacks {
    /// Feature Transformer (73,305 → 1536)
    pub feature_transformer: FeatureTransformerLayerStacks,
    /// LayerStacks (9バケット)
    pub layer_stacks: LayerStacks,
    /// FV_SCALE (評価値のスケーリング係数)
    /// bullet-shogi: 16 (8128/508), nnue-pytorch: 600
    fv_scale: i32,
}

impl NetworkLayerStacks {
    /// ファイルから読み込み
    pub fn load<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);
        Self::read(&mut reader)
    }

    /// リーダーから読み込み
    pub fn read<R: Read + Seek>(reader: &mut R) -> io::Result<Self> {
        // ヘッダを読み込み
        let mut buf4 = [0u8; 4];
        reader.read_exact(&mut buf4)?;
        let version = u32::from_le_bytes(buf4);

        // bullet-shogi は NNUE_VERSION (0x7AF32F16) を使用することがある
        if version != NNUE_VERSION_HALFKA && version != NNUE_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Invalid NNUE version for LayerStacks: {version:#x}, expected {NNUE_VERSION_HALFKA:#x} or {NNUE_VERSION:#x}"
                ),
            ));
        }

        // 構造ハッシュを読み込み
        reader.read_exact(&mut buf4)?;
        let _hash = u32::from_le_bytes(buf4);

        // アーキテクチャ文字列を読み込み
        reader.read_exact(&mut buf4)?;
        let arch_len = u32::from_le_bytes(buf4) as usize;
        if arch_len == 0 || arch_len > MAX_ARCH_LEN {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Invalid arch string length: {arch_len} (max: {MAX_ARCH_LEN})"),
            ));
        }
        let mut arch = vec![0u8; arch_len];
        reader.read_exact(&mut arch)?;

        // アーキテクチャ文字列を解析
        let arch_str = String::from_utf8_lossy(&arch);

        // Factorizedモデルの検出
        if arch_str.contains("Factorizer") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Unsupported model format: factorized (non-coalesced) model detected.\n\
                     This engine only supports coalesced models.\n\n\
                     To fix: Re-export the model using nnue-pytorch serialize.py:\n\
                       python serialize.py model.ckpt output.nnue\n\n\
                     Architecture string: {arch_str}"
                ),
            ));
        }

        // bullet-shogi 形式の検出
        // bullet-shogi は "-LayerStack" (単数形) と "l0=", "buckets=" パラメータを使用
        // nnue-pytorch は "LayerStacks" (複数形) を使用し、FT hash を含む
        let is_bullet_shogi_format = arch_str.contains("-LayerStack,") || arch_str.contains("l0=");

        // fv_scale をパース
        // bullet-shogi: "fv_scale=16" が arch_str に含まれる
        // nnue-pytorch: fv_scale が含まれない場合は 600 (デフォルト)
        let fv_scale = parse_fv_scale(&arch_str);

        let (feature_transformer, layer_stacks) = if is_bullet_shogi_format {
            // bullet-shogi 形式: FT hash なし、非圧縮、LayerStacks も独自形式
            // FT 重みは [output_dim][input_dim] で保存されているため転置が必要
            let ft = FeatureTransformerLayerStacks::read_bullet_shogi(reader)?;
            let ls = LayerStacks::read_bullet_shogi(reader)?;
            (ft, ls)
        } else {
            // nnue-pytorch 形式: FT hash あり、LEB128 圧縮
            reader.read_exact(&mut buf4)?;
            let _ft_hash = u32::from_le_bytes(buf4);
            let ft = FeatureTransformerLayerStacks::read_leb128(reader)?;
            let ls = LayerStacks::read(reader)?;
            (ft, ls)
        };

        // EOF検証: 余りデータがないことを確認
        // factorizedモデル（非coalesced）を誤って読んだ場合、
        // 余りデータが発生する可能性がある。
        // bullet-shogi は 64 バイト境界までパディング ("bullet" 文字列) を追加するため、
        // bullet-shogi 形式の場合は最大 63 バイトの余りを許容する。
        let mut probe = [0u8; 64];
        match reader.read(&mut probe) {
            Ok(0) => {
                // EOF到達 - 正常（coalesce済みモデル）
            }
            Ok(n) if is_bullet_shogi_format && n < 64 => {
                // bullet-shogi のパディング (最大 63 バイト) - 正常
            }
            Ok(_) => {
                // 余りデータあり - おそらくfactorizedモデル
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "NNUE file has unexpected trailing data.\n\
                     This likely indicates a factorized (non-coalesced) model.\n\
                     This engine only supports coalesced models.\n\n\
                     To fix: Re-export the model using nnue-pytorch serialize.py:\n\
                       python serialize.py model.ckpt output.nnue\n\n\
                     The serialize.py script automatically coalesces factor weights.",
                ));
            }
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                // EOF - 正常
            }
            Err(e) => {
                // その他のIOエラー
                return Err(e);
            }
        }

        // 診断ログを出力
        #[cfg(feature = "diagnostics")]
        {
            Self::log_load_diagnostics(&feature_transformer, &layer_stacks, fv_scale);
        }

        Ok(Self {
            feature_transformer,
            layer_stacks,
            fv_scale,
        })
    }

    /// 読み込み時の診断ログを出力
    #[cfg(feature = "diagnostics")]
    fn log_load_diagnostics(ft: &FeatureTransformerLayerStacks, ls: &LayerStacks, fv_scale: i32) {
        // FT統計
        let bias_sum: i64 = ft.biases.0.iter().map(|&x| x as i64).sum();
        let weight_min = ft.weights.iter().copied().min().unwrap_or(0);
        let weight_max = ft.weights.iter().copied().max().unwrap_or(0);
        let weight_nonzero: usize = ft.weights.iter().filter(|&&x| x != 0).count();
        let weight_total = ft.weights.len();

        info!("[NNUE Load] fv_scale: {fv_scale}");
        info!("[NNUE Load] FT bias sum: {bias_sum}");
        info!("[NNUE Load] FT weight: min={weight_min}, max={weight_max}");
        info!(
            "[NNUE Load] FT weight nonzero: {weight_nonzero}/{weight_total} ({:.2}%)",
            weight_nonzero as f64 / weight_total as f64 * 100.0
        );

        // LayerStacks bucket0 の l1_biases
        let l1_biases = &ls.buckets[0].l1_biases;
        info!("[NNUE Load] LayerStacks bucket0 l1_biases: {l1_biases:?}");
    }

    /// バイト列から読み込み
    pub fn from_bytes(bytes: &[u8]) -> io::Result<Self> {
        let mut cursor = Cursor::new(bytes);
        Self::read(&mut cursor)
    }

    /// 評価値を計算
    pub fn evaluate(&self, pos: &Position, acc: &AccumulatorLayerStacks) -> Value {
        let side_to_move = pos.side_to_move();

        // SqrClippedReLU変換
        let (us_acc, them_acc) = if side_to_move == Color::Black {
            (acc.get(Color::Black as usize), acc.get(Color::White as usize))
        } else {
            (acc.get(Color::White as usize), acc.get(Color::Black as usize))
        };

        let mut transformed = [0u8; NNUE_PYTORCH_L1];
        sqr_clipped_relu_transform(us_acc, them_acc, &mut transformed);

        // バケットインデックスを計算（両玉の段に基づく）
        let f_king = pos.king_square(side_to_move);
        let e_king = pos.king_square(!side_to_move);
        let (f_rank, e_rank) =
            crate::nnue::layer_stacks::compute_king_ranks(side_to_move, f_king, e_king);
        let bucket_index = compute_bucket_index(f_rank, e_rank);

        // LayerStacks で評価 (raw score を fv_scale で割る)
        let raw_score = self.layer_stacks.evaluate_raw(bucket_index, &transformed);
        let score = raw_score / self.fv_scale;

        Value::new(score)
    }

    /// 評価値を計算（詳細診断ログ付き）
    ///
    /// Python (nnue-pytorch) との比較検証用。
    /// 各中間値をログ出力する。
    #[cfg(feature = "diagnostics")]
    pub fn evaluate_with_diagnostics(&self, pos: &Position, acc: &AccumulatorLayerStacks) -> Value {
        use log::info;

        let side_to_move = pos.side_to_move();

        // アキュムレータの統計
        let (us_acc, them_acc) = if side_to_move == Color::Black {
            (acc.get(Color::Black as usize), acc.get(Color::White as usize))
        } else {
            (acc.get(Color::White as usize), acc.get(Color::Black as usize))
        };

        // us_acc の統計
        let us_min = us_acc.iter().copied().min().unwrap_or(0);
        let us_max = us_acc.iter().copied().max().unwrap_or(0);
        let us_first_half_positive: usize = us_acc[0..768].iter().filter(|&&x| x > 0).count();
        let us_second_half_positive: usize = us_acc[768..1536].iter().filter(|&&x| x > 0).count();

        info!("[NNUE Eval] us_acc: min={us_min}, max={us_max}");
        info!(
            "[NNUE Eval] us_acc positive: first_half={us_first_half_positive}/768, second_half={us_second_half_positive}/768"
        );
        info!("[NNUE Eval] us_acc first 16: {:?}", &us_acc[0..16]);

        // SqrClippedReLU変換
        let mut transformed = [0u8; NNUE_PYTORCH_L1];
        sqr_clipped_relu_transform(us_acc, them_acc, &mut transformed);

        let transformed_nonzero: usize = transformed.iter().filter(|&&x| x > 0).count();
        let transformed_sum: u64 = transformed.iter().map(|&x| x as u64).sum();
        info!("[NNUE Eval] transformed: nonzero={transformed_nonzero}/1536, sum={transformed_sum}");
        info!("[NNUE Eval] transformed first 32: {:?}", &transformed[0..32]);

        // バケットインデックスを計算（両玉の段に基づく）
        let f_king = pos.king_square(side_to_move);
        let e_king = pos.king_square(!side_to_move);
        let (f_rank, e_rank) =
            crate::nnue::layer_stacks::compute_king_ranks(side_to_move, f_king, e_king);
        let bucket_index = compute_bucket_index(f_rank, e_rank);
        info!(
            "[NNUE Eval] f_king_rank={f_rank}, e_king_rank={e_rank}, bucket_index={bucket_index}"
        );

        // LayerStacks で評価（詳細ログ付き）
        let (raw_score, l1_out, l1_skip) =
            self.layer_stacks.evaluate_raw_with_diagnostics(bucket_index, &transformed);

        info!("[NNUE Eval] l1_out (16 elements): {l1_out:?}");
        info!("[NNUE Eval] l1_skip: {l1_skip}");
        info!("[NNUE Eval] raw_score (with skip): {raw_score}");
        info!("[NNUE Eval] fv_scale: {}", self.fv_scale);

        let score = raw_score / self.fv_scale;
        let score_float = raw_score as f64 / self.fv_scale as f64;
        info!("[NNUE Eval] score: {score} (float: {score_float:.4})");

        Value::new(score)
    }

    /// 差分計算を使わずにAccumulatorを計算
    pub fn refresh_accumulator(&self, pos: &Position, acc: &mut AccumulatorLayerStacks) {
        self.feature_transformer.refresh_accumulator(pos, acc);
    }

    /// 差分計算でAccumulatorを更新
    pub fn update_accumulator(
        &self,
        pos: &Position,
        dirty_piece: &super::accumulator::DirtyPiece,
        acc: &mut AccumulatorLayerStacks,
        prev_acc: &AccumulatorLayerStacks,
    ) {
        self.feature_transformer.update_accumulator(pos, dirty_piece, acc, prev_acc);
    }

    /// 複数手分の差分を適用してアキュムレータを更新
    pub fn forward_update_incremental(
        &self,
        pos: &Position,
        stack: &mut AccumulatorStackLayerStacks,
        source_idx: usize,
    ) -> bool {
        self.feature_transformer.forward_update_incremental(pos, stack, source_idx)
    }
}

/// arch_str から fv_scale をパース
///
/// bullet-shogi 形式: "fv_scale=16" のようなパラメータが含まれる
/// nnue-pytorch 形式: fv_scale が含まれない場合は 600 (NNUE_PYTORCH_NNUE2SCORE)
fn parse_fv_scale(arch_str: &str) -> i32 {
    use super::constants::NNUE_PYTORCH_NNUE2SCORE;

    if let Some(start) = arch_str.find("fv_scale=") {
        let rest = &arch_str[start + 9..];
        let end = rest.find(',').unwrap_or(rest.len());
        rest[..end].parse::<i32>().unwrap_or(NNUE_PYTORCH_NNUE2SCORE)
    } else {
        NNUE_PYTORCH_NNUE2SCORE
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nnue::constants::NNUE_PYTORCH_NNUE2SCORE;
    use crate::position::{Position, SFEN_HIRATE};

    #[test]
    fn test_network_dimensions() {
        assert_eq!(NNUE_PYTORCH_L1, 1536);
        assert_eq!(NNUE_PYTORCH_NNUE2SCORE, 600);
    }

    /// LayerStacks NNUEファイルの読み込みと評価テスト
    ///
    /// このテストは外部NNUEファイルが必要なため通常はスキップ。
    /// 実行方法: `cargo test test_load_layer_stacks_file -- --ignored`
    ///
    /// テスト結果 (epoch82.nnue):
    /// - FT bias sum: -1
    /// - FT weight nonzero: 2,143,627
    /// - L1 bias (bucket 0): [-15, 57, -182, -97, -202, -55, 120, 1, 87, -133, -16, 44, -27, -37, -201, -186]
    /// - Initial position score: 0 (epoch82は学習初期のため)
    #[test]
    #[ignore]
    fn test_load_layer_stacks_file() {
        use crate::nnue::layer_stacks::{compute_bucket_index, sqr_clipped_relu_transform};

        // テスト用NNUEファイルのパスを設定してください
        let path = std::env::var("NNUE_TEST_FILE")
            .unwrap_or_else(|_| "/path/to/your/layer_stacks.nnue".to_string());

        let network = match NetworkLayerStacks::load(path) {
            Ok(n) => n,
            Err(e) => {
                eprintln!("Skipping test: {e}");
                return;
            }
        };

        // Feature Transformer のバイアスが読み込まれていることを確認
        let bias_sum: i64 = network.feature_transformer.biases.0.iter().map(|&x| x as i64).sum();
        eprintln!("FT bias sum: {bias_sum}");

        // Feature Transformer の重みの一部を確認
        let weight_sample: Vec<i16> = network.feature_transformer.weights[0..10].to_vec();
        eprintln!("FT weight sample (first 10): {weight_sample:?}");

        // 異なるオフセットで重みを確認
        let weight_total = network.feature_transformer.weights.len();
        let weight_nonzero: usize =
            network.feature_transformer.weights.iter().filter(|&&x| x != 0).count();
        eprintln!("FT weight total: {weight_total}, nonzero: {weight_nonzero}");

        // 中間位置の重みをサンプル
        let mid_offset = weight_total / 2;
        let weight_mid_sample: Vec<i16> =
            network.feature_transformer.weights[mid_offset..mid_offset + 10].to_vec();
        eprintln!("FT weight sample (mid): {weight_mid_sample:?}");

        // 最初のnonzero重みの位置を探す
        let first_nonzero_pos = network.feature_transformer.weights.iter().position(|&x| x != 0);
        if let Some(pos) = first_nonzero_pos {
            let sample_end = (pos + 10).min(weight_total);
            let first_nonzero_sample: Vec<i16> =
                network.feature_transformer.weights[pos..sample_end].to_vec();
            eprintln!("First nonzero at position {pos}, sample: {first_nonzero_sample:?}");
            // 特徴インデックスを計算 (weight layout: [feature_index][output_dim])
            let feature_idx = pos / NNUE_PYTORCH_L1;
            eprintln!("  -> Feature index: {feature_idx}");
        }

        // LayerStacks の重みの一部を確認
        let l1_bias_sample: Vec<i32> = network.layer_stacks.buckets[0].l1_biases.to_vec();
        eprintln!("L1 bias (bucket 0): {l1_bias_sample:?}");

        // 初期局面を評価
        let mut pos = Position::new();
        pos.set_sfen(SFEN_HIRATE).unwrap();

        // アクティブ特徴量を確認
        use crate::nnue::features::{FeatureSet, HalfKA_hm_FeatureSet};
        use crate::types::Color;
        let active_black = HalfKA_hm_FeatureSet::collect_active_indices(&pos, Color::Black);
        eprintln!("Active features for Black: {} features", active_black.len());
        let first_5: Vec<usize> = active_black.iter().take(5).copied().collect();
        eprintln!("  First 5 indices: {first_5:?}");

        // 最初のアクティブ特徴量の重みを確認
        if let Some(&first_idx) = active_black.iter().next() {
            let offset = first_idx * NNUE_PYTORCH_L1;
            eprintln!("  Weight offset for feature {first_idx}: {offset}");
            if offset + 10 <= weight_total {
                let active_weight_sample: Vec<i16> =
                    network.feature_transformer.weights[offset..offset + 10].to_vec();
                eprintln!("  Weight sample for first active feature: {active_weight_sample:?}");
            }
        }

        let mut acc = AccumulatorLayerStacks::new();
        network.refresh_accumulator(&pos, &mut acc);

        // Accumulatorの値を確認
        let black_acc = acc.get(0);
        let white_acc = acc.get(1);
        let black_acc_sum: i64 = black_acc.iter().map(|&x| x as i64).sum();
        let white_acc_sum: i64 = white_acc.iter().map(|&x| x as i64).sum();
        eprintln!("Black acc sum: {black_acc_sum}, White acc sum: {white_acc_sum}");
        eprintln!("Black acc sample (first 10): {:?}", &black_acc[0..10]);

        // アキュムレータの統計
        let black_min = black_acc.iter().copied().min().unwrap_or(0);
        let black_max = black_acc.iter().copied().max().unwrap_or(0);
        let black_positive: usize = black_acc.iter().filter(|&&x| x > 0).count();
        eprintln!("Black acc: min={black_min}, max={black_max}, positive={black_positive}/1536");

        // 前半768と後半768の統計（SqrClippedReLUでペア乗算される）
        let first_half = &black_acc[0..768];
        let second_half = &black_acc[768..1536];
        let first_positive: usize = first_half.iter().filter(|&&x| x > 0).count();
        let second_positive: usize = second_half.iter().filter(|&&x| x > 0).count();
        eprintln!("First half positive: {first_positive}/768, Second half positive: {second_positive}/768");

        // ペア乗算で非ゼロになるペアの数
        let mut pairs_both_positive = 0usize;
        for i in 0..768 {
            if first_half[i] > 0 && second_half[i] > 0 {
                pairs_both_positive += 1;
            }
        }
        eprintln!("Pairs where both halves > 0: {pairs_both_positive}/768");

        // SqrClippedReLU変換後の値を確認
        let mut transformed = [0u8; NNUE_PYTORCH_L1];
        sqr_clipped_relu_transform(black_acc, white_acc, &mut transformed);
        let transformed_sum: u64 = transformed.iter().map(|&x| x as u64).sum();
        let transformed_nonzero: usize = transformed.iter().filter(|&&x| x > 0).count();
        eprintln!("Transformed sum: {transformed_sum}, nonzero count: {transformed_nonzero}");
        eprintln!("Transformed sample (first 20): {:?}", &transformed[0..20]);

        // バケットインデックスを計算（玉の段に基づく）
        let side_to_move = pos.side_to_move();
        let f_king = pos.king_square(side_to_move);
        let e_king = pos.king_square(!side_to_move);
        let (f_rank, e_rank) =
            crate::nnue::layer_stacks::compute_king_ranks(side_to_move, f_king, e_king);
        let bucket_index = compute_bucket_index(f_rank, e_rank);
        eprintln!("King ranks: f={f_rank}, e={e_rank}, bucket index: {bucket_index}");

        // LayerStacks の生スコアを計算
        let raw_score = network.layer_stacks.evaluate_raw(bucket_index, &transformed);
        eprintln!("Raw score (before /600): {raw_score}");

        // 評価値を計算
        let value = network.evaluate(&pos, &acc);
        eprintln!("Initial position score: {}", value.raw());

        // 評価値が妥当な範囲内であることを確認（-1000〜1000）
        assert!(value.raw().abs() < 1000, "Score {} is out of expected range", value.raw());

        // 様々な局面での評価値を確認
        eprintln!("\n=== Various positions ===");
        let test_positions = [
            ("初期局面", "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1"),
            ("後手1歩得", "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPP1/1B5R1/LNSGKGSNL b p 1"),
            ("先手1歩得", "lnsgkgsnl/1r5b1/pppppppp1/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b P 1"),
            ("後手飛車落ち", "lnsgkgsnl/7b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1"),
            ("先手角得", "lnsgkgsnl/1r7/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b B 1"),
        ];

        for (name, sfen) in test_positions {
            pos.set_sfen(sfen).unwrap();
            network.refresh_accumulator(&pos, &mut acc);

            // raw score（/600前）を計算
            let (us_acc, them_acc) = (acc.get(0), acc.get(1));
            let mut transformed = [0u8; NNUE_PYTORCH_L1];
            sqr_clipped_relu_transform(us_acc, them_acc, &mut transformed);
            let stm = pos.side_to_move();
            let f_k = pos.king_square(stm);
            let e_k = pos.king_square(!stm);
            let (f_r, e_r) = crate::nnue::layer_stacks::compute_king_ranks(stm, f_k, e_k);
            let bucket_idx = compute_bucket_index(f_r, e_r);
            let raw = network.layer_stacks.evaluate_raw(bucket_idx, &transformed);

            let val = network.evaluate(&pos, &acc);
            eprintln!("{:15}: {:6} (raw: {:6})", name, val.raw(), raw);
        }

        // ファイル読み込みの検証
        // - FT bias/weight の読み込みが正しい
        // - LayerStacks の読み込みが正しい
        // - 評価値計算が動作する
        //
        // 注意: epoch82.nnue は学習途中のモデルなので評価値が小さい
        // Raw score: -51 → /600 = 0
        // これは正常な動作
    }
}
