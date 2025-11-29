//! NNUEネットワーク全体の構造と評価関数
//!
//! HalfKP 256x2-32-32 アーキテクチャを想定した NNUE ネットワークを表現する。
//! - `FeatureTransformer` で HalfKP 特徴量を 512 次元に変換
//! - `AffineTransform` + `ClippedReLU` を 2 層適用して 32→32 と圧縮
//! - 出力層（32→1）で整数スコアを得て `FV_SCALE` でスケーリングし `Value` に変換
//! - グローバルな `NETWORK` にロードし、`evaluate` から利用する

use super::accumulator::Accumulator;
use super::constants::{
    FV_SCALE, HIDDEN1_DIMENSIONS, HIDDEN2_DIMENSIONS, NNUE_VERSION, OUTPUT_DIMENSIONS,
    TRANSFORMED_FEATURE_DIMENSIONS,
};
use super::feature_transformer::FeatureTransformer;
use super::layers::{AffineTransform, ClippedReLU};
use crate::position::Position;
use crate::types::Value;
use std::fs::File;
use std::io::{self, Read};
use std::mem;
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

/// NNUEが初期化されているか
#[allow(dead_code)]
pub fn is_nnue_initialized() -> bool {
    NETWORK.get().is_some()
}

/// 局面を評価
///
/// NNUEが初期化されていない場合は駒得評価にフォールバック。
/// StateInfo に保持した Accumulator を差分更新し、計算済みなら再利用する。
pub fn evaluate(pos: &mut Position) -> Value {
    if let Some(network) = NETWORK.get() {
        // Accumulator を一時的に取り出して更新し、計算済みのものを StateInfo に書き戻す。
        let (mut acc, prev_acc_ptr) = {
            let state = pos.state_mut();
            let acc = mem::replace(&mut state.accumulator, Box::new(Accumulator::new()));
            // 生ポインタで保持し、pos の可変借用を解放した後に参照する
            let prev_acc = state.previous.as_ref().map(|s| &*s.accumulator as *const Accumulator);
            (acc, prev_acc)
        };

        if !acc.computed_accumulation {
            let mut updated = false;
            if let Some(prev_acc_ptr) = prev_acc_ptr {
                // SAFETY: prev_acc_ptr は state.previous の生存期間内にのみ使用する。
                let prev_acc = unsafe { &*prev_acc_ptr };
                if prev_acc.computed_accumulation {
                    updated =
                        network.feature_transformer.update_accumulator(pos, &mut acc, prev_acc);
                }
            }

            if !updated {
                network.feature_transformer.refresh_accumulator(pos, &mut acc);
            }
        }

        // 計算済みの Accumulator を StateInfo に書き戻す
        {
            let state = pos.state_mut();
            state.accumulator = acc;
        }

        // 不変借用で評価
        let acc_ref = {
            let state = pos.state();
            &state.accumulator
        };
        network.evaluate(pos, acc_ref)
    } else {
        // フォールバック: 簡易駒得評価
        evaluate_material(pos)
    }
}

/// 簡易駒得評価（NNUEが使えない場合のフォールバック）
fn evaluate_material(pos: &Position) -> Value {
    use crate::types::PieceType;

    let mut score = 0i32;

    // 駒の価値（単位: 1歩 = 100）
    const PIECE_VALUES: [i32; 15] = [
        0,    // None
        100,  // Pawn
        300,  // Lance
        350,  // Knight
        400,  // Silver
        500,  // Gold
        800,  // Bishop
        1000, // Rook
        500,  // King (not used)
        400,  // ProPawn
        400,  // ProLance
        400,  // ProKnight
        400,  // ProSilver
        1100, // Horse
        1300, // Dragon
    ];

    // 盤上の駒
    for sq in pos.occupied().iter() {
        let pc = pos.piece_on(sq);
        if pc.is_none() {
            continue;
        }

        let pt = pc.piece_type();
        let value = PIECE_VALUES[pt as usize];

        if pc.color() == pos.side_to_move() {
            score += value;
        } else {
            score -= value;
        }
    }

    // 手駒
    for pt in [
        PieceType::Pawn,
        PieceType::Lance,
        PieceType::Knight,
        PieceType::Silver,
        PieceType::Gold,
        PieceType::Bishop,
        PieceType::Rook,
    ] {
        let my_hand = pos.hand(pos.side_to_move()).count(pt) as i32;
        let opp_hand = pos.hand(!pos.side_to_move()).count(pt) as i32;

        let value = PIECE_VALUES[pt as usize];
        score += value * my_hand;
        score -= value * opp_hand;
    }

    Value::new(score)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::position::SFEN_HIRATE;

    #[test]
    fn test_evaluate_material_hirate() {
        let mut pos = Position::new();
        pos.set_sfen(SFEN_HIRATE).unwrap();

        let value = evaluate_material(&pos);

        // 初期局面は互角
        assert_eq!(value, Value::ZERO);
    }

    #[test]
    fn test_evaluate_fallback() {
        let mut pos = Position::new();
        pos.set_sfen(SFEN_HIRATE).unwrap();

        // NNUEが初期化されていない場合はフォールバック
        let value = evaluate(&mut pos);

        // フォールバック評価が動作することを確認
        assert!(value.raw().abs() < 1000);
    }
}
