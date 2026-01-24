//! NNUEネットワーク全体の構造と評価関数
//!
//! 以下のアーキテクチャをサポート:
//! - **HalfKP**: classic NNUE（水匠/tanuki互換）
//! - **HalfKA_hm^**: nnue-pytorch互換（Half-Mirror + Factorization）
//!
//! 評価値計算フロー:
//! - `FeatureTransformer` で特徴量を 512 次元に変換
//! - `AffineTransform` + `ClippedReLU` を 2 層適用して 32→32 と圧縮
//! - 出力層（32→1）で整数スコアを得て `FV_SCALE` でスケーリングし `Value` に変換
//! - グローバルな `NETWORK` にロードし、`evaluate` から利用する

use super::accumulator_layer_stacks::AccumulatorStackLayerStacks;
use super::accumulator_stack_variant::AccumulatorStackVariant;
use super::activation::detect_activation_from_arch;
use super::constants::{MAX_ARCH_LEN, NNUE_VERSION, NNUE_VERSION_HALFKA};
use super::network_halfka::{
    AccumulatorHalfKA, AccumulatorStackHalfKA, HalfKA1024CReLU, HalfKA1024Pairwise,
    HalfKA1024SCReLU, HalfKA1024_8_32CReLU, HalfKA1024_8_32SCReLU, HalfKA256CReLU,
    HalfKA256Pairwise, HalfKA256SCReLU, HalfKA512CReLU, HalfKA512Pairwise, HalfKA512SCReLU,
};
use super::network_halfkp::{
    AccumulatorHalfKP, AccumulatorStackHalfKP, HalfKP256CReLU, HalfKP256Pairwise, HalfKP256SCReLU,
    HalfKP512CReLU, HalfKP512Pairwise, HalfKP512SCReLU,
};
use super::network_layer_stacks::NetworkLayerStacks;
#[cfg(not(feature = "tournament"))]
use crate::eval::material;
use crate::position::Position;
use crate::types::Value;
use std::fs::File;
use std::io::{self, BufReader, Cursor, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::OnceLock;

/// グローバルなNNUEネットワーク（HalfKPまたはHalfKA_hm^）
static NETWORK: OnceLock<NNUENetwork> = OnceLock::new();

/// FV_SCALE のグローバルオーバーライド設定
///
/// 0 = 自動判定（Network 構造体の fv_scale を使用）
/// 1以上 = 指定値でオーバーライド
///
/// YaneuraOuと同様にエンジンオプションで設定可能。
/// 評価関数によって異なる値が必要な場合に使用。
static FV_SCALE_OVERRIDE: AtomicI32 = AtomicI32::new(0);

/// FV_SCALE オーバーライドを取得
///
/// 戻り値:
/// - `Some(value)`: オーバーライド値が設定されている
/// - `None`: 自動判定を使用（Network の fv_scale を使用）
pub fn get_fv_scale_override() -> Option<i32> {
    let value = FV_SCALE_OVERRIDE.load(Ordering::Relaxed);
    if value > 0 {
        Some(value)
    } else {
        None
    }
}

/// FV_SCALE オーバーライドを設定
///
/// 引数:
/// - `value`: 設定値（0 = 自動判定、1以上 = オーバーライド）
pub fn set_fv_scale_override(value: i32) {
    FV_SCALE_OVERRIDE.store(value.max(0), Ordering::Relaxed);
}

// =============================================================================
// NNUENetwork - アーキテクチャを抽象化するenum
// =============================================================================

/// NNUEネットワーク（HalfKPまたはHalfKA_hm^をラップ）
///
/// const generics 版の統一実装を使用。各バリアントは活性化関数ごとに分かれる。
///
/// # サポートするアーキテクチャ
///
/// - HalfKP 256x2-32-32 (CReLU/SCReLU)
/// - HalfKP 512x2-8-96 (CReLU/SCReLU)
/// - HalfKA 256x2-32-32 (CReLU/SCReLU)
/// - HalfKA 512x2-8-96 (CReLU/SCReLU)
/// - HalfKA 1024x2-8-96 (CReLU/SCReLU)
/// - HalfKA 1024x2-8-32 (CReLU/SCReLU)
/// - LayerStacks 1536x2 + 9バケット
pub enum NNUENetwork {
    /// HalfKP 256x2-32-32 CReLU (const generics版)
    HalfKP256CReLU(Box<HalfKP256CReLU>),
    /// HalfKP 256x2-32-32 SCReLU (const generics版)
    HalfKP256SCReLU(Box<HalfKP256SCReLU>),
    /// HalfKP 256/2x2-32-32 PairwiseCReLU (const generics版)
    HalfKP256Pairwise(Box<HalfKP256Pairwise>),
    /// HalfKP 512x2-8-96 CReLU (const generics版)
    HalfKP512CReLU(Box<HalfKP512CReLU>),
    /// HalfKP 512x2-8-96 SCReLU (const generics版)
    HalfKP512SCReLU(Box<HalfKP512SCReLU>),
    /// HalfKP 512/2x2-8-96 PairwiseCReLU (const generics版)
    HalfKP512Pairwise(Box<HalfKP512Pairwise>),
    /// HalfKA_hm^ 256x2-32-32 CReLU (const generics版)
    HalfKA256CReLU(Box<HalfKA256CReLU>),
    /// HalfKA_hm^ 256x2-32-32 SCReLU (const generics版)
    HalfKA256SCReLU(Box<HalfKA256SCReLU>),
    /// HalfKA_hm^ 256/2x2-32-32 PairwiseCReLU (const generics版)
    HalfKA256Pairwise(Box<HalfKA256Pairwise>),
    /// HalfKA_hm^ 512x2-8-96 CReLU (const generics版)
    HalfKA512CReLU(Box<HalfKA512CReLU>),
    /// HalfKA_hm^ 512x2-8-96 SCReLU (const generics版)
    HalfKA512SCReLU(Box<HalfKA512SCReLU>),
    /// HalfKA_hm^ 512/2x2-8-96 PairwiseCReLU (const generics版)
    HalfKA512Pairwise(Box<HalfKA512Pairwise>),
    /// HalfKA_hm^ 1024x2-8-96 CReLU (const generics版)
    HalfKA1024CReLU(Box<HalfKA1024CReLU>),
    /// HalfKA_hm^ 1024x2-8-96 SCReLU (const generics版)
    HalfKA1024SCReLU(Box<HalfKA1024SCReLU>),
    /// HalfKA_hm^ 1024/2x2-8-96 PairwiseCReLU (const generics版)
    HalfKA1024Pairwise(Box<HalfKA1024Pairwise>),
    /// HalfKA_hm^ 1024x2-8-32 CReLU (const generics版)
    HalfKA1024_8_32CReLU(Box<HalfKA1024_8_32CReLU>),
    /// HalfKA_hm^ 1024x2-8-32 SCReLU (const generics版)
    HalfKA1024_8_32SCReLU(Box<HalfKA1024_8_32SCReLU>),
    /// LayerStacks（1536次元 + 9バケット）
    LayerStacks(Box<NetworkLayerStacks>),
}

impl NNUENetwork {
    /// HalfKP でサポートされているアーキテクチャ一覧
    ///
    /// 新しいバリアント追加時は `architecture_spec()` と合わせて更新すること。
    pub fn supported_halfkp_specs() -> &'static str {
        "256x2-32-32, 512x2-8-96"
    }

    /// HalfKA でサポートされているアーキテクチャ一覧
    ///
    /// 新しいバリアント追加時は `architecture_spec()` と合わせて更新すること。
    /// 注: LayerStacks は実験段階のため含まない。
    pub fn supported_halfka_specs() -> &'static str {
        "256x2-32-32, 512x2-8-96, 1024x2-8-96, 1024x2-8-32"
    }

    /// ファイルから読み込み（バージョン自動判別）
    pub fn load<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);
        Self::read(&mut reader)
    }

    /// リーダーから読み込み（バージョン自動判別）
    pub fn read<R: Read + Seek>(reader: &mut R) -> io::Result<Self> {
        // バージョンを読み取り
        let mut buf4 = [0u8; 4];
        reader.read_exact(&mut buf4)?;
        let version = u32::from_le_bytes(buf4);

        // nnue-pytorch は NNUE_VERSION (0x7AF32F16) を使用するが、
        // アーキテクチャ文字列が "HalfKA" を含む場合は HalfKA_hm^ として扱う。
        // NNUE_VERSION_HALFKA (0x7AF32F20) も HalfKA_hm^ として扱う。
        match version {
            NNUE_VERSION | NNUE_VERSION_HALFKA => {
                // アーキテクチャ文字列を先に読んで判別する

                // ハッシュを読み飛ばし
                reader.read_exact(&mut buf4)?;

                // アーキテクチャ文字列長を読み取り
                reader.read_exact(&mut buf4)?;
                let arch_len = u32::from_le_bytes(buf4) as usize;
                if arch_len == 0 || arch_len > MAX_ARCH_LEN {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("Invalid arch string length: {arch_len}"),
                    ));
                }

                // アーキテクチャ文字列を読み取り
                let mut arch = vec![0u8; arch_len];
                reader.read_exact(&mut arch)?;
                let arch_str = String::from_utf8_lossy(&arch);
                // 実ファイルの例:
                // - Features=HalfKP[125388->256x2],fv_scale=16,qa=127,qb=64,scale=508
                // - Features=HalfKA_hm[73305->256x2],fv_scale=16,qa=127,qb=64,scale=508
                // - Features=HalfKA_hm[73305->512x2],fv_scale=5,qa=127,qb=64,scale=1600
                // - Features=HalfKA_hm[73305->1024x2],fv_scale=5,qa=127,qb=64,scale=1600

                // 位置を戻して全体を読み込み
                reader.seek(SeekFrom::Start(0))?;

                // 活性化関数を検出 ("CReLU", "SCReLU", "PairwiseCReLU")
                let activation = detect_activation_from_arch(&arch_str);

                // アーキテクチャを判別
                // HalfKA_hm 系の判定（アーキテクチャ文字列に "HalfKA" を含む）
                if arch_str.contains("HalfKA") {
                    // HalfKA_hm^ には複数のアーキテクチャがある:
                    // - LayerStacks (1536次元 + 9バケット)
                    // - HalfKA512 (512x2-8-96)
                    // - HalfKA1024 (1024x2-8-96)
                    if arch_str.contains("->1536x2]") || arch_str.contains("LayerStacks") {
                        // LayerStacks (1536次元)
                        let network = NetworkLayerStacks::read(reader)?;
                        Ok(Self::LayerStacks(Box::new(network)))
                    } else {
                        // L1, L2, L3 をパースして判定
                        let (l1, l2, l3) = Self::parse_arch_dimensions(&arch_str);
                        match (l1, l2, l3, activation) {
                            // 256x2-32-32
                            (256, 32, 32, "CReLU") => {
                                let network = HalfKA256CReLU::read(reader)?;
                                Ok(Self::HalfKA256CReLU(Box::new(network)))
                            }
                            (256, 32, 32, "SCReLU") => {
                                let network = HalfKA256SCReLU::read(reader)?;
                                Ok(Self::HalfKA256SCReLU(Box::new(network)))
                            }
                            (256, 32, 32, "PairwiseCReLU") => {
                                let network = HalfKA256Pairwise::read(reader)?;
                                Ok(Self::HalfKA256Pairwise(Box::new(network)))
                            }
                            // 512x2-8-96 CReLU
                            (512, 8, 96, "CReLU") => {
                                let network = HalfKA512CReLU::read(reader)?;
                                Ok(Self::HalfKA512CReLU(Box::new(network)))
                            }
                            // 512x2-8-96 SCReLU
                            (512, 8, 96, "SCReLU") => {
                                let network = HalfKA512SCReLU::read(reader)?;
                                Ok(Self::HalfKA512SCReLU(Box::new(network)))
                            }
                            // 512/2x2-8-96 PairwiseCReLU
                            (512, 8, 96, "PairwiseCReLU") => {
                                let network = HalfKA512Pairwise::read(reader)?;
                                Ok(Self::HalfKA512Pairwise(Box::new(network)))
                            }
                            // 1024x2-8-96
                            (1024, 8, 96, "CReLU") => {
                                let network = HalfKA1024CReLU::read(reader)?;
                                Ok(Self::HalfKA1024CReLU(Box::new(network)))
                            }
                            (1024, 8, 96, "SCReLU") => {
                                let network = HalfKA1024SCReLU::read(reader)?;
                                Ok(Self::HalfKA1024SCReLU(Box::new(network)))
                            }
                            (1024, 8, 96, "PairwiseCReLU") => {
                                let network = HalfKA1024Pairwise::read(reader)?;
                                Ok(Self::HalfKA1024Pairwise(Box::new(network)))
                            }
                            // 1024x2-8-32
                            (1024, 8, 32, "CReLU") => {
                                let network = HalfKA1024_8_32CReLU::read(reader)?;
                                Ok(Self::HalfKA1024_8_32CReLU(Box::new(network)))
                            }
                            (1024, 8, 32, "SCReLU") => {
                                let network = HalfKA1024_8_32SCReLU::read(reader)?;
                                Ok(Self::HalfKA1024_8_32SCReLU(Box::new(network)))
                            }
                            _ => {
                                // 旧形式ファイル検出（L2/L3 が 0 の場合）
                                if l2 == 0 || l3 == 0 {
                                    Err(io::Error::new(
                                        io::ErrorKind::InvalidData,
                                        format!(
                                            "HalfKA L1={l1} network missing L2/L3 dimensions in header. \
                                             This is an old bullet-shogi format that is no longer supported. \
                                             Please re-export the model with a newer version of bullet-shogi. \
                                             Architecture string: {arch_str}"
                                        ),
                                    ))
                                } else {
                                    // 未対応アーキテクチャ
                                    Err(io::Error::new(
                                        io::ErrorKind::Unsupported,
                                        format!(
                                            "Unsupported HalfKA architecture: {arch_str}. \
                                             Supported: {}. \
                                             Detected: L1={l1}, L2={l2}, L3={l3}, activation={activation}",
                                            Self::supported_halfka_specs()
                                        ),
                                    ))
                                }
                            }
                        }
                    }
                } else {
                    // HalfKP: L1をパースして活性化関数と組み合わせて判定
                    let l1 = Self::parse_halfkp_l1(&arch_str);
                    let (_, l2, l3) = Self::parse_arch_dimensions(&arch_str);
                    match (l1, l2, l3, activation) {
                        // 256x2-32-32 CReLU
                        (256, 32, 32, "CReLU") => {
                            let network = HalfKP256CReLU::read(reader)?;
                            Ok(Self::HalfKP256CReLU(Box::new(network)))
                        }
                        // 256x2-32-32 SCReLU
                        (256, 32, 32, "SCReLU") => {
                            let network = HalfKP256SCReLU::read(reader)?;
                            Ok(Self::HalfKP256SCReLU(Box::new(network)))
                        }
                        // 256x2-32-32 PairwiseCReLU
                        (256, 32, 32, "PairwiseCReLU") => {
                            let network = HalfKP256Pairwise::read(reader)?;
                            Ok(Self::HalfKP256Pairwise(Box::new(network)))
                        }
                        // 512x2-8-96 CReLU
                        (512, 8, 96, "CReLU") => {
                            let network = HalfKP512CReLU::read(reader)?;
                            Ok(Self::HalfKP512CReLU(Box::new(network)))
                        }
                        // 512x2-8-96 SCReLU
                        (512, 8, 96, "SCReLU") => {
                            let network = HalfKP512SCReLU::read(reader)?;
                            Ok(Self::HalfKP512SCReLU(Box::new(network)))
                        }
                        // 512x2-8-96 PairwiseCReLU
                        (512, 8, 96, "PairwiseCReLU") => {
                            let network = HalfKP512Pairwise::read(reader)?;
                            Ok(Self::HalfKP512Pairwise(Box::new(network)))
                        }
                        _ => {
                            // 旧形式ファイル検出（L1/L2/L3 が 0 の場合）
                            if l1 == 0 || l2 == 0 || l3 == 0 {
                                Err(io::Error::new(
                                    io::ErrorKind::InvalidData,
                                    format!(
                                        "HalfKP network missing L1/L2/L3 dimensions in header. \
                                         This is an old format that is no longer supported. \
                                         Please re-export the model with a newer version. \
                                         Detected: L1={l1}, L2={l2}, L3={l3}. \
                                         Architecture string: {arch_str}"
                                    ),
                                ))
                            } else {
                                // 未対応アーキテクチャ
                                Err(io::Error::new(
                                    io::ErrorKind::Unsupported,
                                    format!(
                                        "Unsupported HalfKP architecture: {arch_str}. \
                                         Supported: {}. \
                                         Detected: L1={l1}, L2={l2}, L3={l3}, activation={activation}",
                                        Self::supported_halfkp_specs()
                                    ),
                                ))
                            }
                        }
                    }
                }
            }
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Unknown NNUE version: {version:#x}. Expected {NNUE_VERSION:#x} (HalfKP) or {NNUE_VERSION_HALFKA:#x} (HalfKA_hm^)"
                ),
            )),
        }
    }

    /// アーキテクチャ文字列から L1, L2, L3 を抽出
    ///
    /// 戻り値: (L1, L2, L3)
    /// パース失敗時はデフォルト値 (0, 0, 0) を返す
    fn parse_arch_dimensions(arch_str: &str) -> (usize, usize, usize) {
        // L1: "->NNNx2]" または "->NNN/2x2]" (Pairwise) パターンを探す
        let l1 = if let Some(idx) = arch_str.find("x2]") {
            let before = &arch_str[..idx];
            if let Some(arrow_idx) = before.rfind("->") {
                let after_arrow = &before[arrow_idx + 2..];
                // Pairwise形式 "512/2" の場合は "/" で終端、通常形式なら全体が数値
                let num_str = if let Some(slash_idx) = after_arrow.find('/') {
                    &after_arrow[..slash_idx]
                } else {
                    after_arrow
                };
                num_str.parse::<usize>().unwrap_or(0)
            } else {
                0
            }
        } else {
            0
        };

        // L2, L3: AffineTransform[OUT<-IN] パターンを探す
        // 例: AffineTransform[8<-1024] → L2=8
        //     AffineTransform[96<-8] → L3=96
        let mut layers: Vec<(usize, usize)> = Vec::new();
        let pattern = "AffineTransform[";

        let mut search_start = 0;
        while let Some(start) = arch_str[search_start..].find(pattern) {
            let abs_start = search_start + start + pattern.len();
            if let Some(end) = arch_str[abs_start..].find(']') {
                let content = &arch_str[abs_start..abs_start + end];
                if let Some(arrow_idx) = content.find("<-") {
                    let out_str = &content[..arrow_idx];
                    let in_str = &content[arrow_idx + 2..];
                    if let (Ok(out), Ok(inp)) = (out_str.parse::<usize>(), in_str.parse::<usize>())
                    {
                        layers.push((out, inp));
                    }
                }
                search_start = abs_start + end;
            } else {
                break;
            }
        }

        // 1. まず bullet-shogi 形式 "l2=8,l3=96" を優先的にパース
        //    明示的に指定された値を尊重する
        let mut l2 = 0usize;
        let mut l3 = 0usize;
        for part in arch_str.split(',') {
            if let Some(val_str) = part.strip_prefix("l2=") {
                if let Ok(val) = val_str.parse::<usize>() {
                    l2 = val;
                }
            } else if let Some(val_str) = part.strip_prefix("l3=") {
                if let Ok(val) = val_str.parse::<usize>() {
                    l3 = val;
                }
            }
        }

        // 2. l2/l3 が取得できなかった場合、AffineTransform パターンでフォールバック
        //    nnue-pytorch のネストされた構造では、出力に近い順に並ぶ
        //    例: AffineTransform[1<-96](ClippedReLU[96](AffineTransform[96<-8](...)))
        //    パース結果: [1<-96], [96<-8], [8<-1024]
        //    逆順にして入力側から: [8<-1024] (L2), [96<-8] (L3), [1<-96] (output)
        if l2 == 0 || l3 == 0 {
            layers.reverse();
            if layers.len() >= 3 {
                if l2 == 0 {
                    l2 = layers[0].0;
                }
                if l3 == 0 {
                    l3 = layers[1].0;
                }
            }
        }

        (l1, l2, l3)
    }

    /// HalfKP アーキテクチャ文字列から L1 を抽出
    ///
    /// パース失敗時は 0 を返す
    fn parse_halfkp_l1(arch_str: &str) -> usize {
        // "->NNN" または "->NNN/2" (Pairwise) パターンを探す
        if let Some(idx) = arch_str.find("->") {
            let after = &arch_str[idx + 2..];
            let end = after.find(|c: char| !c.is_ascii_digit()).unwrap_or(after.len());
            let num_str = &after[..end];
            return num_str.parse().unwrap_or(0);
        }
        // "[NNNx2]" または "[NNN/2x2]" パターンを探す
        if let Some(idx) = arch_str.find("x2]") {
            let before = &arch_str[..idx];
            // Pairwise形式 "512/2" の場合
            if let Some(slash_idx) = before.rfind('/') {
                let num_part = &before[..slash_idx];
                if let Some(start) = num_part.rfind(|c: char| !c.is_ascii_digit()) {
                    let num_str = &num_part[start + 1..];
                    return num_str.parse().unwrap_or(0);
                }
            } else if let Some(start) = before.rfind(|c: char| !c.is_ascii_digit()) {
                let num_str = &before[start + 1..];
                return num_str.parse().unwrap_or(0);
            }
        }
        0
    }

    /// バイト列から読み込み（バージョン自動判別）
    pub fn from_bytes(bytes: &[u8]) -> io::Result<Self> {
        let mut cursor = Cursor::new(bytes);
        Self::read(&mut cursor)
    }

    /// 評価値を計算（LayerStacks用）
    ///
    /// # Panics
    ///
    /// LayerStacks 以外のアーキテクチャで呼び出された場合にパニックします。
    // TODO: Result<Value, EvaluationError> を返すように変更する
    pub fn evaluate_layer_stacks(
        &self,
        pos: &Position,
        acc: &super::accumulator_layer_stacks::AccumulatorLayerStacks,
    ) -> Value {
        match self {
            Self::LayerStacks(net) => net.evaluate(pos, acc),
            _ => unreachable!(
                "BUG: evaluate_layer_stacks() called on non-LayerStacks architecture: {:?}",
                self.architecture_name()
            ),
        }
    }

    /// 評価値を計算（HalfKA256用 - const generics版）
    ///
    /// # Panics
    ///
    /// HalfKA256CReLU/HalfKA256SCReLU 以外のアーキテクチャで呼び出された場合にパニックします。
    pub fn evaluate_halfka_256(&self, pos: &Position, acc: &AccumulatorHalfKA<256>) -> Value {
        match self {
            Self::HalfKA256CReLU(net) => net.evaluate(pos, acc),
            Self::HalfKA256SCReLU(net) => net.evaluate(pos, acc),
            Self::HalfKA256Pairwise(net) => net.evaluate(pos, acc),
            _ => unreachable!(
                "BUG: evaluate_halfka_256() called on non-HalfKA256 architecture: {:?}",
                self.architecture_name()
            ),
        }
    }

    /// 評価値を計算（HalfKA512用 - const generics版）
    ///
    /// # Panics
    ///
    /// HalfKA512CReLU/HalfKA512SCReLU/HalfKA512Pairwise 以外のアーキテクチャで呼び出された場合にパニックします。
    pub fn evaluate_halfka_512(&self, pos: &Position, acc: &AccumulatorHalfKA<512>) -> Value {
        match self {
            Self::HalfKA512CReLU(net) => net.evaluate(pos, acc),
            Self::HalfKA512SCReLU(net) => net.evaluate(pos, acc),
            Self::HalfKA512Pairwise(net) => net.evaluate(pos, acc),
            _ => unreachable!(
                "BUG: evaluate_halfka_512() called on non-HalfKA512 architecture: {:?}",
                self.architecture_name()
            ),
        }
    }

    /// 評価値を計算（HalfKA1024用 - const generics版）
    ///
    /// # Panics
    ///
    /// HalfKA1024CReLU/HalfKA1024SCReLU/HalfKA1024Pairwise 以外のアーキテクチャで呼び出された場合にパニックします。
    pub fn evaluate_halfka_1024(&self, pos: &Position, acc: &AccumulatorHalfKA<1024>) -> Value {
        match self {
            Self::HalfKA1024CReLU(net) => net.evaluate(pos, acc),
            Self::HalfKA1024SCReLU(net) => net.evaluate(pos, acc),
            Self::HalfKA1024Pairwise(net) => net.evaluate(pos, acc),
            _ => unreachable!(
                "BUG: evaluate_halfka_1024() called on non-HalfKA1024 architecture: {:?}",
                self.architecture_name()
            ),
        }
    }

    /// 評価値を計算（HalfKA1024_8_32用 - const generics版）
    ///
    /// # Panics
    ///
    /// HalfKA1024_8_32CReLU/HalfKA1024_8_32SCReLU 以外のアーキテクチャで呼び出された場合にパニックします。
    pub fn evaluate_halfka_1024_8_32(
        &self,
        pos: &Position,
        acc: &AccumulatorHalfKA<1024>,
    ) -> Value {
        match self {
            Self::HalfKA1024_8_32CReLU(net) => net.evaluate(pos, acc),
            Self::HalfKA1024_8_32SCReLU(net) => net.evaluate(pos, acc),
            _ => unreachable!(
                "BUG: evaluate_halfka_1024_8_32() called on non-HalfKA1024_8_32 architecture: {:?}",
                self.architecture_name()
            ),
        }
    }

    /// 評価値を計算（HalfKP256用 - const generics版）
    ///
    /// # Panics
    ///
    /// HalfKP256CReLU/HalfKP256SCReLU/HalfKP256Pairwise 以外のアーキテクチャで呼び出された場合にパニックします。
    pub fn evaluate_halfkp_256(&self, pos: &Position, acc: &AccumulatorHalfKP<256>) -> Value {
        match self {
            Self::HalfKP256CReLU(net) => net.evaluate(pos, acc),
            Self::HalfKP256SCReLU(net) => net.evaluate(pos, acc),
            Self::HalfKP256Pairwise(net) => net.evaluate(pos, acc),
            _ => unreachable!(
                "BUG: evaluate_halfkp_256() called on non-HalfKP256 architecture: {:?}",
                self.architecture_name()
            ),
        }
    }

    /// 評価値を計算（HalfKP512用 - const generics版）
    ///
    /// # Panics
    ///
    /// HalfKP512CReLU/HalfKP512SCReLU/HalfKP512Pairwise 以外のアーキテクチャで呼び出された場合にパニックします。
    pub fn evaluate_halfkp_512(&self, pos: &Position, acc: &AccumulatorHalfKP<512>) -> Value {
        match self {
            Self::HalfKP512CReLU(net) => net.evaluate(pos, acc),
            Self::HalfKP512SCReLU(net) => net.evaluate(pos, acc),
            Self::HalfKP512Pairwise(net) => net.evaluate(pos, acc),
            _ => unreachable!(
                "BUG: evaluate_halfkp_512() called on non-HalfKP512 architecture: {:?}",
                self.architecture_name()
            ),
        }
    }

    /// LayerStacks アーキテクチャかどうか
    pub fn is_layer_stacks(&self) -> bool {
        matches!(self, Self::LayerStacks(_))
    }

    /// HalfKA256 アーキテクチャかどうか
    pub fn is_halfka_256(&self) -> bool {
        matches!(
            self,
            Self::HalfKA256CReLU(_) | Self::HalfKA256SCReLU(_) | Self::HalfKA256Pairwise(_)
        )
    }

    /// HalfKA512 アーキテクチャかどうか
    pub fn is_halfka_512(&self) -> bool {
        matches!(
            self,
            Self::HalfKA512CReLU(_) | Self::HalfKA512SCReLU(_) | Self::HalfKA512Pairwise(_)
        )
    }

    /// HalfKA1024 アーキテクチャかどうか
    pub fn is_halfka_1024(&self) -> bool {
        matches!(
            self,
            Self::HalfKA1024CReLU(_) | Self::HalfKA1024SCReLU(_) | Self::HalfKA1024Pairwise(_)
        )
    }

    /// HalfKA1024_8_32 アーキテクチャかどうか
    pub fn is_halfka_1024_8_32(&self) -> bool {
        matches!(self, Self::HalfKA1024_8_32CReLU(_) | Self::HalfKA1024_8_32SCReLU(_))
    }

    /// アーキテクチャ名を取得（例: "HalfKP256CReLU"）
    pub fn architecture_name(&self) -> &'static str {
        match self {
            Self::HalfKP256CReLU(_) => "HalfKP256CReLU",
            Self::HalfKP256SCReLU(_) => "HalfKP256SCReLU",
            Self::HalfKP256Pairwise(_) => "HalfKP256Pairwise",
            Self::HalfKP512CReLU(_) => "HalfKP512CReLU",
            Self::HalfKP512SCReLU(_) => "HalfKP512SCReLU",
            Self::HalfKP512Pairwise(_) => "HalfKP512Pairwise",
            Self::HalfKA256CReLU(_) => "HalfKA256CReLU",
            Self::HalfKA256SCReLU(_) => "HalfKA256SCReLU",
            Self::HalfKA256Pairwise(_) => "HalfKA256Pairwise",
            Self::HalfKA512CReLU(_) => "HalfKA512CReLU",
            Self::HalfKA512SCReLU(_) => "HalfKA512SCReLU",
            Self::HalfKA512Pairwise(_) => "HalfKA512Pairwise",
            Self::HalfKA1024CReLU(_) => "HalfKA1024CReLU",
            Self::HalfKA1024SCReLU(_) => "HalfKA1024SCReLU",
            Self::HalfKA1024Pairwise(_) => "HalfKA1024Pairwise",
            Self::HalfKA1024_8_32CReLU(_) => "HalfKA1024_8_32CReLU",
            Self::HalfKA1024_8_32SCReLU(_) => "HalfKA1024_8_32SCReLU",
            Self::LayerStacks(_) => "LayerStacks",
        }
    }

    /// アーキテクチャ仕様を取得（例: "256x2-32-32"）
    ///
    /// `supported_halfkp_specs()` / `supported_halfka_specs()` と整合性を保つこと。
    /// 新しいバリアント追加時はここも更新が必要（exhaustive matchで警告される）。
    pub fn architecture_spec(&self) -> &'static str {
        match self {
            Self::HalfKP256CReLU(_) | Self::HalfKP256SCReLU(_) | Self::HalfKP256Pairwise(_) => {
                "256x2-32-32"
            }
            Self::HalfKP512CReLU(_) | Self::HalfKP512SCReLU(_) | Self::HalfKP512Pairwise(_) => {
                "512x2-8-96"
            }
            Self::HalfKA256CReLU(_) | Self::HalfKA256SCReLU(_) | Self::HalfKA256Pairwise(_) => {
                "256x2-32-32"
            }
            Self::HalfKA512CReLU(_) | Self::HalfKA512SCReLU(_) | Self::HalfKA512Pairwise(_) => {
                "512x2-8-96"
            }
            Self::HalfKA1024CReLU(_) | Self::HalfKA1024SCReLU(_) | Self::HalfKA1024Pairwise(_) => {
                "1024x2-8-96"
            }
            Self::HalfKA1024_8_32CReLU(_) | Self::HalfKA1024_8_32SCReLU(_) => "1024x2-8-32",
            Self::LayerStacks(_) => "LayerStacks(1536x2)",
        }
    }

    /// 差分計算を使わずにAccumulatorを計算（LayerStacks用）
    pub fn refresh_accumulator_layer_stacks(
        &self,
        pos: &Position,
        acc: &mut super::accumulator_layer_stacks::AccumulatorLayerStacks,
    ) {
        match self {
            Self::LayerStacks(net) => net.refresh_accumulator(pos, acc),
            _ => panic!("This method is only for LayerStacks architecture."),
        }
    }

    /// 差分計算を使わずにAccumulatorを計算（HalfKA256用 - const generics版）
    pub fn refresh_accumulator_halfka_256(&self, pos: &Position, acc: &mut AccumulatorHalfKA<256>) {
        match self {
            Self::HalfKA256CReLU(net) => net.refresh_accumulator(pos, acc),
            Self::HalfKA256SCReLU(net) => net.refresh_accumulator(pos, acc),
            Self::HalfKA256Pairwise(net) => net.refresh_accumulator(pos, acc),
            _ => panic!("This method is only for HalfKA256 architecture."),
        }
    }

    /// 差分計算を使わずにAccumulatorを計算（HalfKA512用 - const generics版）
    pub fn refresh_accumulator_halfka_512(&self, pos: &Position, acc: &mut AccumulatorHalfKA<512>) {
        match self {
            Self::HalfKA512CReLU(net) => net.refresh_accumulator(pos, acc),
            Self::HalfKA512SCReLU(net) => net.refresh_accumulator(pos, acc),
            Self::HalfKA512Pairwise(net) => net.refresh_accumulator(pos, acc),
            _ => panic!("This method is only for HalfKA512 architecture."),
        }
    }

    /// 差分計算を使わずにAccumulatorを計算（HalfKA1024用 - const generics版）
    pub fn refresh_accumulator_halfka_1024(
        &self,
        pos: &Position,
        acc: &mut AccumulatorHalfKA<1024>,
    ) {
        match self {
            Self::HalfKA1024CReLU(net) => net.refresh_accumulator(pos, acc),
            Self::HalfKA1024SCReLU(net) => net.refresh_accumulator(pos, acc),
            Self::HalfKA1024Pairwise(net) => net.refresh_accumulator(pos, acc),
            _ => panic!("This method is only for HalfKA1024 architecture."),
        }
    }

    /// 差分計算を使わずにAccumulatorを計算（HalfKA1024_8_32用 - const generics版）
    pub fn refresh_accumulator_halfka_1024_8_32(
        &self,
        pos: &Position,
        acc: &mut AccumulatorHalfKA<1024>,
    ) {
        match self {
            Self::HalfKA1024_8_32CReLU(net) => net.refresh_accumulator(pos, acc),
            Self::HalfKA1024_8_32SCReLU(net) => net.refresh_accumulator(pos, acc),
            _ => panic!("This method is only for HalfKA1024_8_32 architecture."),
        }
    }

    /// 差分計算を使わずにAccumulatorを計算（HalfKP256用 - const generics版）
    pub fn refresh_accumulator_halfkp_256(&self, pos: &Position, acc: &mut AccumulatorHalfKP<256>) {
        match self {
            Self::HalfKP256CReLU(net) => net.refresh_accumulator(pos, acc),
            Self::HalfKP256SCReLU(net) => net.refresh_accumulator(pos, acc),
            Self::HalfKP256Pairwise(net) => net.refresh_accumulator(pos, acc),
            _ => panic!("This method is only for HalfKP256 architecture."),
        }
    }

    /// 差分計算を使わずにAccumulatorを計算（HalfKP512用 - const generics版）
    pub fn refresh_accumulator_halfkp_512(&self, pos: &Position, acc: &mut AccumulatorHalfKP<512>) {
        match self {
            Self::HalfKP512CReLU(net) => net.refresh_accumulator(pos, acc),
            Self::HalfKP512SCReLU(net) => net.refresh_accumulator(pos, acc),
            Self::HalfKP512Pairwise(net) => net.refresh_accumulator(pos, acc),
            _ => panic!("This method is only for HalfKP512 architecture."),
        }
    }

    /// 差分計算でAccumulatorを更新（LayerStacks用）
    pub fn update_accumulator_layer_stacks(
        &self,
        pos: &Position,
        dirty_piece: &super::accumulator::DirtyPiece,
        acc: &mut super::accumulator_layer_stacks::AccumulatorLayerStacks,
        prev_acc: &super::accumulator_layer_stacks::AccumulatorLayerStacks,
    ) {
        match self {
            Self::LayerStacks(net) => net.update_accumulator(pos, dirty_piece, acc, prev_acc),
            _ => panic!("This method is only for LayerStacks architecture."),
        }
    }

    /// 差分計算でAccumulatorを更新（HalfKA256用 - const generics版）
    pub fn update_accumulator_halfka_256(
        &self,
        pos: &Position,
        dirty_piece: &super::accumulator::DirtyPiece,
        acc: &mut AccumulatorHalfKA<256>,
        prev_acc: &AccumulatorHalfKA<256>,
    ) {
        match self {
            Self::HalfKA256CReLU(net) => net.update_accumulator(pos, dirty_piece, acc, prev_acc),
            Self::HalfKA256SCReLU(net) => net.update_accumulator(pos, dirty_piece, acc, prev_acc),
            Self::HalfKA256Pairwise(net) => net.update_accumulator(pos, dirty_piece, acc, prev_acc),
            _ => panic!("This method is only for HalfKA256 architecture."),
        }
    }

    /// 差分計算でAccumulatorを更新（HalfKA512用 - const generics版）
    pub fn update_accumulator_halfka_512(
        &self,
        pos: &Position,
        dirty_piece: &super::accumulator::DirtyPiece,
        acc: &mut AccumulatorHalfKA<512>,
        prev_acc: &AccumulatorHalfKA<512>,
    ) {
        match self {
            Self::HalfKA512CReLU(net) => net.update_accumulator(pos, dirty_piece, acc, prev_acc),
            Self::HalfKA512SCReLU(net) => net.update_accumulator(pos, dirty_piece, acc, prev_acc),
            Self::HalfKA512Pairwise(net) => net.update_accumulator(pos, dirty_piece, acc, prev_acc),
            _ => panic!("This method is only for HalfKA512 architecture."),
        }
    }

    /// 差分計算でAccumulatorを更新（HalfKA1024用 - const generics版）
    pub fn update_accumulator_halfka_1024(
        &self,
        pos: &Position,
        dirty_piece: &super::accumulator::DirtyPiece,
        acc: &mut AccumulatorHalfKA<1024>,
        prev_acc: &AccumulatorHalfKA<1024>,
    ) {
        match self {
            Self::HalfKA1024CReLU(net) => net.update_accumulator(pos, dirty_piece, acc, prev_acc),
            Self::HalfKA1024SCReLU(net) => net.update_accumulator(pos, dirty_piece, acc, prev_acc),
            Self::HalfKA1024Pairwise(net) => {
                net.update_accumulator(pos, dirty_piece, acc, prev_acc)
            }
            _ => panic!("This method is only for HalfKA1024 architecture."),
        }
    }

    /// 差分計算でAccumulatorを更新（HalfKA1024_8_32用 - const generics版）
    pub fn update_accumulator_halfka_1024_8_32(
        &self,
        pos: &Position,
        dirty_piece: &super::accumulator::DirtyPiece,
        acc: &mut AccumulatorHalfKA<1024>,
        prev_acc: &AccumulatorHalfKA<1024>,
    ) {
        match self {
            Self::HalfKA1024_8_32CReLU(net) => {
                net.update_accumulator(pos, dirty_piece, acc, prev_acc)
            }
            Self::HalfKA1024_8_32SCReLU(net) => {
                net.update_accumulator(pos, dirty_piece, acc, prev_acc)
            }
            _ => panic!("This method is only for HalfKA1024_8_32 architecture."),
        }
    }

    /// 差分計算でAccumulatorを更新（HalfKP256用 - const generics版）
    pub fn update_accumulator_halfkp_256(
        &self,
        pos: &Position,
        dirty_piece: &super::accumulator::DirtyPiece,
        acc: &mut AccumulatorHalfKP<256>,
        prev_acc: &AccumulatorHalfKP<256>,
    ) {
        match self {
            Self::HalfKP256CReLU(net) => net.update_accumulator(pos, dirty_piece, acc, prev_acc),
            Self::HalfKP256SCReLU(net) => net.update_accumulator(pos, dirty_piece, acc, prev_acc),
            Self::HalfKP256Pairwise(net) => net.update_accumulator(pos, dirty_piece, acc, prev_acc),
            _ => panic!("This method is only for HalfKP256 architecture."),
        }
    }

    /// 差分計算でAccumulatorを更新（HalfKP512用 - const generics版）
    pub fn update_accumulator_halfkp_512(
        &self,
        pos: &Position,
        dirty_piece: &super::accumulator::DirtyPiece,
        acc: &mut AccumulatorHalfKP<512>,
        prev_acc: &AccumulatorHalfKP<512>,
    ) {
        match self {
            Self::HalfKP512CReLU(net) => net.update_accumulator(pos, dirty_piece, acc, prev_acc),
            Self::HalfKP512SCReLU(net) => net.update_accumulator(pos, dirty_piece, acc, prev_acc),
            Self::HalfKP512Pairwise(net) => net.update_accumulator(pos, dirty_piece, acc, prev_acc),
            _ => panic!("This method is only for HalfKP512 architecture."),
        }
    }

    /// 複数手分の差分を適用してアキュムレータを更新（LayerStacks用）
    pub fn forward_update_incremental_layer_stacks(
        &self,
        pos: &Position,
        stack: &mut AccumulatorStackLayerStacks,
        source_idx: usize,
    ) -> bool {
        match self {
            Self::LayerStacks(net) => net.forward_update_incremental(pos, stack, source_idx),
            _ => panic!("This method is only for LayerStacks architecture."),
        }
    }

    /// 複数手分の差分を適用してアキュムレータを更新（HalfKA256用 - const generics版）
    pub fn forward_update_incremental_halfka_256(
        &self,
        pos: &Position,
        stack: &mut AccumulatorStackHalfKA<256>,
        source_idx: usize,
    ) -> bool {
        match self {
            Self::HalfKA256CReLU(net) => net.forward_update_incremental(pos, stack, source_idx),
            Self::HalfKA256SCReLU(net) => net.forward_update_incremental(pos, stack, source_idx),
            Self::HalfKA256Pairwise(net) => net.forward_update_incremental(pos, stack, source_idx),
            _ => panic!("This method is only for HalfKA256 architecture."),
        }
    }

    /// 複数手分の差分を適用してアキュムレータを更新（HalfKA512用 - const generics版）
    pub fn forward_update_incremental_halfka_512(
        &self,
        pos: &Position,
        stack: &mut AccumulatorStackHalfKA<512>,
        source_idx: usize,
    ) -> bool {
        match self {
            Self::HalfKA512CReLU(net) => net.forward_update_incremental(pos, stack, source_idx),
            Self::HalfKA512SCReLU(net) => net.forward_update_incremental(pos, stack, source_idx),
            Self::HalfKA512Pairwise(net) => net.forward_update_incremental(pos, stack, source_idx),
            _ => panic!("This method is only for HalfKA512 architecture."),
        }
    }

    /// 複数手分の差分を適用してアキュムレータを更新（HalfKA1024用 - const generics版）
    pub fn forward_update_incremental_halfka_1024(
        &self,
        pos: &Position,
        stack: &mut AccumulatorStackHalfKA<1024>,
        source_idx: usize,
    ) -> bool {
        match self {
            Self::HalfKA1024CReLU(net) => net.forward_update_incremental(pos, stack, source_idx),
            Self::HalfKA1024SCReLU(net) => net.forward_update_incremental(pos, stack, source_idx),
            Self::HalfKA1024Pairwise(net) => net.forward_update_incremental(pos, stack, source_idx),
            _ => panic!("This method is only for HalfKA1024 architecture."),
        }
    }

    /// 複数手分の差分を適用してアキュムレータを更新（HalfKA1024_8_32用 - const generics版）
    pub fn forward_update_incremental_halfka_1024_8_32(
        &self,
        pos: &Position,
        stack: &mut AccumulatorStackHalfKA<1024>,
        source_idx: usize,
    ) -> bool {
        match self {
            Self::HalfKA1024_8_32CReLU(net) => {
                net.forward_update_incremental(pos, stack, source_idx)
            }
            Self::HalfKA1024_8_32SCReLU(net) => {
                net.forward_update_incremental(pos, stack, source_idx)
            }
            _ => panic!("This method is only for HalfKA1024_8_32 architecture."),
        }
    }

    /// 複数手分の差分を適用してアキュムレータを更新（HalfKP256用 - const generics版）
    pub fn forward_update_incremental_halfkp_256(
        &self,
        pos: &Position,
        stack: &mut AccumulatorStackHalfKP<256>,
        source_idx: usize,
    ) -> bool {
        match self {
            Self::HalfKP256CReLU(net) => net.forward_update_incremental(pos, stack, source_idx),
            Self::HalfKP256SCReLU(net) => net.forward_update_incremental(pos, stack, source_idx),
            Self::HalfKP256Pairwise(net) => net.forward_update_incremental(pos, stack, source_idx),
            _ => panic!("This method is only for HalfKP256 architecture."),
        }
    }

    /// 複数手分の差分を適用してアキュムレータを更新（HalfKP512用 - const generics版）
    pub fn forward_update_incremental_halfkp_512(
        &self,
        pos: &Position,
        stack: &mut AccumulatorStackHalfKP<512>,
        source_idx: usize,
    ) -> bool {
        match self {
            Self::HalfKP512CReLU(net) => net.forward_update_incremental(pos, stack, source_idx),
            Self::HalfKP512SCReLU(net) => net.forward_update_incremental(pos, stack, source_idx),
            Self::HalfKP512Pairwise(net) => net.forward_update_incremental(pos, stack, source_idx),
            _ => panic!("This method is only for HalfKP512 architecture."),
        }
    }

    /// HalfKA256 用の新しいアキュムレータを作成
    pub fn new_accumulator_halfka_256(&self) -> AccumulatorHalfKA<256> {
        match self {
            Self::HalfKA256CReLU(net) => net.new_accumulator(),
            Self::HalfKA256SCReLU(net) => net.new_accumulator(),
            _ => panic!("This method is only for HalfKA256 architecture."),
        }
    }

    /// HalfKA256 用の新しいアキュムレータスタックを作成
    pub fn new_accumulator_stack_halfka_256(&self) -> AccumulatorStackHalfKA<256> {
        match self {
            Self::HalfKA256CReLU(net) => net.new_accumulator_stack(),
            Self::HalfKA256SCReLU(net) => net.new_accumulator_stack(),
            _ => panic!("This method is only for HalfKA256 architecture."),
        }
    }

    /// HalfKA512 用の新しいアキュムレータを作成
    pub fn new_accumulator_halfka_512(&self) -> AccumulatorHalfKA<512> {
        match self {
            Self::HalfKA512CReLU(net) => net.new_accumulator(),
            Self::HalfKA512SCReLU(net) => net.new_accumulator(),
            _ => panic!("This method is only for HalfKA512 architecture."),
        }
    }

    /// HalfKA512 用の新しいアキュムレータスタックを作成
    pub fn new_accumulator_stack_halfka_512(&self) -> AccumulatorStackHalfKA<512> {
        match self {
            Self::HalfKA512CReLU(net) => net.new_accumulator_stack(),
            Self::HalfKA512SCReLU(net) => net.new_accumulator_stack(),
            _ => panic!("This method is only for HalfKA512 architecture."),
        }
    }

    /// HalfKA1024 用の新しいアキュムレータを作成
    pub fn new_accumulator_halfka_1024(&self) -> AccumulatorHalfKA<1024> {
        match self {
            Self::HalfKA1024CReLU(net) => net.new_accumulator(),
            Self::HalfKA1024SCReLU(net) => net.new_accumulator(),
            _ => panic!("This method is only for HalfKA1024 architecture."),
        }
    }

    /// HalfKA1024 用の新しいアキュムレータスタックを作成
    pub fn new_accumulator_stack_halfka_1024(&self) -> AccumulatorStackHalfKA<1024> {
        match self {
            Self::HalfKA1024CReLU(net) => net.new_accumulator_stack(),
            Self::HalfKA1024SCReLU(net) => net.new_accumulator_stack(),
            _ => panic!("This method is only for HalfKA1024 architecture."),
        }
    }

    /// HalfKA1024_8_32 用の新しいアキュムレータを作成
    pub fn new_accumulator_halfka_1024_8_32(&self) -> AccumulatorHalfKA<1024> {
        match self {
            Self::HalfKA1024_8_32CReLU(net) => net.new_accumulator(),
            Self::HalfKA1024_8_32SCReLU(net) => net.new_accumulator(),
            _ => panic!("This method is only for HalfKA1024_8_32 architecture."),
        }
    }

    /// HalfKA1024_8_32 用の新しいアキュムレータスタックを作成
    pub fn new_accumulator_stack_halfka_1024_8_32(&self) -> AccumulatorStackHalfKA<1024> {
        match self {
            Self::HalfKA1024_8_32CReLU(net) => net.new_accumulator_stack(),
            Self::HalfKA1024_8_32SCReLU(net) => net.new_accumulator_stack(),
            _ => panic!("This method is only for HalfKA1024_8_32 architecture."),
        }
    }

    /// HalfKP256 用の新しいアキュムレータを作成
    pub fn new_accumulator_halfkp_256(&self) -> AccumulatorHalfKP<256> {
        match self {
            Self::HalfKP256CReLU(net) => net.new_accumulator(),
            Self::HalfKP256SCReLU(net) => net.new_accumulator(),
            _ => panic!("This method is only for HalfKP256 architecture."),
        }
    }

    /// HalfKP256 用の新しいアキュムレータスタックを作成
    pub fn new_accumulator_stack_halfkp_256(&self) -> AccumulatorStackHalfKP<256> {
        match self {
            Self::HalfKP256CReLU(net) => net.new_accumulator_stack(),
            Self::HalfKP256SCReLU(net) => net.new_accumulator_stack(),
            _ => panic!("This method is only for HalfKP256 architecture."),
        }
    }

    /// HalfKP512 用の新しいアキュムレータを作成
    pub fn new_accumulator_halfkp_512(&self) -> AccumulatorHalfKP<512> {
        match self {
            Self::HalfKP512CReLU(net) => net.new_accumulator(),
            Self::HalfKP512SCReLU(net) => net.new_accumulator(),
            _ => panic!("This method is only for HalfKP512 architecture."),
        }
    }

    /// HalfKP512 用の新しいアキュムレータスタックを作成
    pub fn new_accumulator_stack_halfkp_512(&self) -> AccumulatorStackHalfKP<512> {
        match self {
            Self::HalfKP512CReLU(net) => net.new_accumulator_stack(),
            Self::HalfKP512SCReLU(net) => net.new_accumulator_stack(),
            _ => panic!("This method is only for HalfKP512 architecture."),
        }
    }
}

// =============================================================================
// arch_str メタデータパース
// =============================================================================

/// arch_str から fv_scale を抽出
///
/// bullet-shogi で学習したモデルは arch_str に "fv_scale=N" を含む。
/// 例: "Features=HalfKA_hm^[73305->256x2]-SCReLU,fv_scale=13,qa=127,qb=64,scale=600"
///
/// 戻り値:
/// - `Some(N)`: fv_scale=N が見つかり、妥当な範囲（1〜128）内の場合
/// - `None`: fv_scale が見つからない、またはパース失敗、または範囲外
///
/// 範囲外の値（0, 負数, 128超）は None を返し、フォールバック値が使用される。
/// これによりゼロ除算や不正な評価値スケーリングを防止する。
pub fn parse_fv_scale_from_arch(arch_str: &str) -> Option<i32> {
    /// fv_scale の許容最小値（ゼロ除算防止）
    const FV_SCALE_MIN: i32 = 1;
    /// fv_scale の許容最大値（実用的な上限）
    const FV_SCALE_MAX: i32 = 128;

    for part in arch_str.split(',') {
        if let Some(value) = part.strip_prefix("fv_scale=") {
            if let Ok(scale) = value.parse::<i32>() {
                // 妥当な範囲内のみ受け入れる
                if (FV_SCALE_MIN..=FV_SCALE_MAX).contains(&scale) {
                    return Some(scale);
                }
            }
            // fv_scale= が見つかったがパース失敗または範囲外の場合は None
            return None;
        }
    }
    None
}

/// NNUEを初期化（バージョン自動判別）
pub fn init_nnue<P: AsRef<Path>>(path: P) -> io::Result<()> {
    let network = NNUENetwork::load(path)?;
    NETWORK
        .set(network)
        .map_err(|_| io::Error::new(io::ErrorKind::AlreadyExists, "NNUE already initialized"))
}

/// バイト列からNNUEを初期化（バージョン自動判別）
pub fn init_nnue_from_bytes(bytes: &[u8]) -> io::Result<()> {
    let network = NNUENetwork::from_bytes(bytes)?;
    NETWORK
        .set(network)
        .map_err(|_| io::Error::new(io::ErrorKind::AlreadyExists, "NNUE already initialized"))
}

/// NNUEが初期化済みかどうか
pub fn is_nnue_initialized() -> bool {
    NETWORK.get().is_some()
}

/// NNUEネットワークへの参照を取得（初期化されていない場合はNone）
///
/// AccumulatorStackVariant の初期化・更新に使用。
pub fn get_network() -> Option<&'static NNUENetwork> {
    NETWORK.get()
}

// =============================================================================
// 内部ヘルパー関数（ロジック集約用）
// =============================================================================

/// LayerStacks アキュムレータを更新して評価（内部実装）
///
/// `evaluate_layer_stacks` と `evaluate_dispatch` から呼び出される共通ロジック。
/// network は既に取得済みで、アーキテクチャチェックも完了していることが前提。
#[inline]
fn update_and_evaluate_layer_stacks(
    network: &NNUENetwork,
    pos: &Position,
    stack: &mut AccumulatorStackLayerStacks,
) -> Value {
    // アキュムレータの更新
    let current_entry = stack.current();
    if !current_entry.accumulator.computed_accumulation {
        let mut updated = false;

        // 1. 直前局面で差分更新を試行
        if let Some(prev_idx) = current_entry.previous {
            let prev_computed = stack.entry_at(prev_idx).accumulator.computed_accumulation;
            if prev_computed {
                let dirty_piece = stack.current().dirty_piece;
                let (prev_acc, current_acc) = stack.get_prev_and_current_accumulators(prev_idx);
                network.update_accumulator_layer_stacks(pos, &dirty_piece, current_acc, prev_acc);
                updated = true;
            }
        }

        // 2. 失敗なら祖先探索 + 複数手差分更新を試行
        if !updated {
            if let Some((source_idx, _depth)) = stack.find_usable_accumulator() {
                updated = network.forward_update_incremental_layer_stacks(pos, stack, source_idx);
            }
        }

        // 3. それでも失敗なら全計算
        if !updated {
            let acc = &mut stack.current_mut().accumulator;
            network.refresh_accumulator_layer_stacks(pos, acc);
        }
    }

    // 評価
    let acc_ref = &stack.current().accumulator;
    network.evaluate_layer_stacks(pos, acc_ref)
}

/// HalfKA256 アキュムレータを更新して評価（内部実装）
#[inline]
fn update_and_evaluate_halfka_256(
    network: &NNUENetwork,
    pos: &Position,
    stack: &mut AccumulatorStackHalfKA<256>,
) -> Value {
    // アキュムレータの更新
    let current_entry = stack.current();
    if !current_entry.accumulator.computed_accumulation {
        let mut updated = false;

        // 1. 直前局面で差分更新を試行
        if let Some(prev_idx) = current_entry.previous {
            let prev_computed = stack.entry_at(prev_idx).accumulator.computed_accumulation;
            if prev_computed {
                let dirty_piece = stack.current().dirty_piece;
                let (prev_acc, current_acc) = stack.get_prev_and_current_accumulators(prev_idx);
                network.update_accumulator_halfka_256(pos, &dirty_piece, current_acc, prev_acc);
                updated = true;
            }
        }

        // 2. 失敗なら祖先探索 + 複数手差分更新を試行
        if !updated {
            if let Some((source_idx, _depth)) = stack.find_usable_accumulator() {
                updated = network.forward_update_incremental_halfka_256(pos, stack, source_idx);
            }
        }

        // 3. それでも失敗なら全計算
        if !updated {
            let acc = &mut stack.current_mut().accumulator;
            network.refresh_accumulator_halfka_256(pos, acc);
        }
    }

    // 評価
    let acc_ref = &stack.current().accumulator;
    network.evaluate_halfka_256(pos, acc_ref)
}

/// HalfKA512 アキュムレータを更新して評価（内部実装）
#[inline]
fn update_and_evaluate_halfka_512(
    network: &NNUENetwork,
    pos: &Position,
    stack: &mut AccumulatorStackHalfKA<512>,
) -> Value {
    // アキュムレータの更新
    let current_entry = stack.current();
    if !current_entry.accumulator.computed_accumulation {
        let mut updated = false;

        // 1. 直前局面で差分更新を試行
        if let Some(prev_idx) = current_entry.previous {
            let prev_computed = stack.entry_at(prev_idx).accumulator.computed_accumulation;
            if prev_computed {
                let dirty_piece = stack.current().dirty_piece;
                let (prev_acc, current_acc) = stack.get_prev_and_current_accumulators(prev_idx);
                network.update_accumulator_halfka_512(pos, &dirty_piece, current_acc, prev_acc);
                updated = true;
            }
        }

        // 2. 失敗なら祖先探索 + 複数手差分更新を試行
        if !updated {
            if let Some((source_idx, _depth)) = stack.find_usable_accumulator() {
                updated = network.forward_update_incremental_halfka_512(pos, stack, source_idx);
            }
        }

        // 3. それでも失敗なら全計算
        if !updated {
            let acc = &mut stack.current_mut().accumulator;
            network.refresh_accumulator_halfka_512(pos, acc);
        }
    }

    // 評価
    let acc_ref = &stack.current().accumulator;
    network.evaluate_halfka_512(pos, acc_ref)
}

/// HalfKA1024 アキュムレータを更新して評価（内部実装）
#[inline]
fn update_and_evaluate_halfka_1024(
    network: &NNUENetwork,
    pos: &Position,
    stack: &mut AccumulatorStackHalfKA<1024>,
) -> Value {
    // アキュムレータの更新
    let current_entry = stack.current();
    if !current_entry.accumulator.computed_accumulation {
        let mut updated = false;

        // 1. 直前局面で差分更新を試行
        if let Some(prev_idx) = current_entry.previous {
            let prev_computed = stack.entry_at(prev_idx).accumulator.computed_accumulation;
            if prev_computed {
                let dirty_piece = stack.current().dirty_piece;
                let (prev_acc, current_acc) = stack.get_prev_and_current_accumulators(prev_idx);
                network.update_accumulator_halfka_1024(pos, &dirty_piece, current_acc, prev_acc);
                updated = true;
            }
        }

        // 2. 失敗なら祖先探索 + 複数手差分更新を試行
        if !updated {
            if let Some((source_idx, _depth)) = stack.find_usable_accumulator() {
                updated = network.forward_update_incremental_halfka_1024(pos, stack, source_idx);
            }
        }

        // 3. それでも失敗なら全計算
        if !updated {
            let acc = &mut stack.current_mut().accumulator;
            network.refresh_accumulator_halfka_1024(pos, acc);
        }
    }

    // 評価
    let acc_ref = &stack.current().accumulator;
    network.evaluate_halfka_1024(pos, acc_ref)
}

/// HalfKA1024_8_32 アキュムレータを更新して評価（内部実装）
#[inline]
fn update_and_evaluate_halfka_1024_8_32(
    network: &NNUENetwork,
    pos: &Position,
    stack: &mut AccumulatorStackHalfKA<1024>,
) -> Value {
    // アキュムレータの更新
    let current_entry = stack.current();
    if !current_entry.accumulator.computed_accumulation {
        let mut updated = false;

        // 1. 直前局面で差分更新を試行
        if let Some(prev_idx) = current_entry.previous {
            let prev_computed = stack.entry_at(prev_idx).accumulator.computed_accumulation;
            if prev_computed {
                let dirty_piece = stack.current().dirty_piece;
                let (prev_acc, current_acc) = stack.get_prev_and_current_accumulators(prev_idx);
                network.update_accumulator_halfka_1024_8_32(
                    pos,
                    &dirty_piece,
                    current_acc,
                    prev_acc,
                );
                updated = true;
            }
        }

        // 2. 失敗なら祖先探索 + 複数手差分更新を試行
        if !updated {
            if let Some((source_idx, _depth)) = stack.find_usable_accumulator() {
                updated =
                    network.forward_update_incremental_halfka_1024_8_32(pos, stack, source_idx);
            }
        }

        // 3. それでも失敗なら全計算
        if !updated {
            let acc = &mut stack.current_mut().accumulator;
            network.refresh_accumulator_halfka_1024_8_32(pos, acc);
        }
    }

    // 評価
    let acc_ref = &stack.current().accumulator;
    network.evaluate_halfka_1024_8_32(pos, acc_ref)
}

/// HalfKP256 アキュムレータを更新して評価（内部実装）
#[inline]
fn update_and_evaluate_halfkp_256(
    network: &NNUENetwork,
    pos: &Position,
    stack: &mut AccumulatorStackHalfKP<256>,
) -> Value {
    // アキュムレータの更新
    let current_entry = stack.current();
    if !current_entry.accumulator.computed_accumulation {
        let mut updated = false;

        // 1. 直前局面で差分更新を試行
        if let Some(prev_idx) = current_entry.previous {
            let prev_computed = stack.entry_at(prev_idx).accumulator.computed_accumulation;
            if prev_computed {
                let dirty_piece = stack.current().dirty_piece;
                let (prev_acc, current_acc) = stack.get_prev_and_current_accumulators(prev_idx);
                network.update_accumulator_halfkp_256(pos, &dirty_piece, current_acc, prev_acc);
                updated = true;
            }
        }

        // 2. 失敗なら祖先探索 + 複数手差分更新を試行
        if !updated {
            if let Some((source_idx, _depth)) = stack.find_usable_accumulator() {
                updated = network.forward_update_incremental_halfkp_256(pos, stack, source_idx);
            }
        }

        // 3. それでも失敗なら全計算
        if !updated {
            let acc = &mut stack.current_mut().accumulator;
            network.refresh_accumulator_halfkp_256(pos, acc);
        }
    }

    // 評価
    let acc_ref = &stack.current().accumulator;
    network.evaluate_halfkp_256(pos, acc_ref)
}

/// HalfKP512 アキュムレータを更新して評価（内部実装）
#[inline]
fn update_and_evaluate_halfkp_512(
    network: &NNUENetwork,
    pos: &Position,
    stack: &mut AccumulatorStackHalfKP<512>,
) -> Value {
    // アキュムレータの更新
    let current_entry = stack.current();
    if !current_entry.accumulator.computed_accumulation {
        let mut updated = false;

        // 1. 直前局面で差分更新を試行
        if let Some(prev_idx) = current_entry.previous {
            let prev_computed = stack.entry_at(prev_idx).accumulator.computed_accumulation;
            if prev_computed {
                let dirty_piece = stack.current().dirty_piece;
                let (prev_acc, current_acc) = stack.get_prev_and_current_accumulators(prev_idx);
                network.update_accumulator_halfkp_512(pos, &dirty_piece, current_acc, prev_acc);
                updated = true;
            }
        }

        // 2. 失敗なら祖先探索 + 複数手差分更新を試行
        if !updated {
            if let Some((source_idx, _depth)) = stack.find_usable_accumulator() {
                updated = network.forward_update_incremental_halfkp_512(pos, stack, source_idx);
            }
        }

        // 3. それでも失敗なら全計算
        if !updated {
            let acc = &mut stack.current_mut().accumulator;
            network.refresh_accumulator_halfkp_512(pos, acc);
        }
    }

    // 評価
    let acc_ref = &stack.current().accumulator;
    network.evaluate_halfkp_512(pos, acc_ref)
}

/// ロードされたNNUEがLayerStacksアーキテクチャかどうか
pub fn is_layer_stacks_loaded() -> bool {
    NETWORK.get().map(|n| n.is_layer_stacks()).unwrap_or(false)
}

/// ロードされたNNUEがHalfKA256アーキテクチャかどうか
pub fn is_halfka_256_loaded() -> bool {
    NETWORK.get().map(|n| n.is_halfka_256()).unwrap_or(false)
}

/// ロードされたNNUEがHalfKA512アーキテクチャかどうか
pub fn is_halfka_512_loaded() -> bool {
    NETWORK.get().map(|n| n.is_halfka_512()).unwrap_or(false)
}

/// ロードされたNNUEがHalfKA1024アーキテクチャかどうか
pub fn is_halfka_1024_loaded() -> bool {
    NETWORK.get().map(|n| n.is_halfka_1024()).unwrap_or(false)
}

/// 局面を評価（LayerStacks用）
///
/// AccumulatorStackLayerStacks を使って差分更新し、計算済みなら再利用する。
///
/// # フォールバック動作
/// - 通常ビルド: NNUEが初期化されていない場合は駒得評価にフォールバック
/// - tournamentビルド: NNUEが初期化されていない場合はパニック
pub fn evaluate_layer_stacks(pos: &Position, stack: &mut AccumulatorStackLayerStacks) -> Value {
    // tournamentビルド: NNUEが必須（フォールバックなし）
    #[cfg(feature = "tournament")]
    let network = NETWORK.get().expect(
        "NNUE network is not initialized. \
         Tournament build requires NNUE to be loaded before evaluation. \
         Call init_nnue() or init_nnue_from_bytes() first.",
    );

    // 通常ビルド: NNUEがなければMaterial評価にフォールバック
    #[cfg(not(feature = "tournament"))]
    let Some(network) = NETWORK.get() else {
        return material::evaluate_material(pos);
    };

    // LayerStacks 以外はエラー
    if !network.is_layer_stacks() {
        panic!("Non-LayerStacks architecture detected. Use evaluate() with AccumulatorStack.");
    }

    // 内部ヘルパー関数を呼び出し
    update_and_evaluate_layer_stacks(network, pos, stack)
}

/// アーキテクチャに応じて適切な評価関数を呼び出す
///
/// AccumulatorStackVariant を受け取り、内部のバリアントに応じて
/// 適切な評価関数を呼び出す。
///
/// # フォールバック動作
/// - 通常ビルド: NNUEが初期化されていない場合は駒得評価にフォールバック
/// - tournamentビルド: NNUEが初期化されていない場合はパニック
pub fn evaluate_dispatch(pos: &Position, stack: &mut AccumulatorStackVariant) -> Value {
    // tournamentビルド: NNUEが必須（フォールバックなし）
    #[cfg(feature = "tournament")]
    let network = NETWORK.get().expect(
        "NNUE network is not initialized. \
         Tournament build requires NNUE to be loaded before evaluation. \
         Call init_nnue() or init_nnue_from_bytes() first.",
    );

    // 通常ビルド: NNUEがなければMaterial評価にフォールバック
    #[cfg(not(feature = "tournament"))]
    let Some(network) = NETWORK.get() else {
        return material::evaluate_material(pos);
    };

    // バリアントに応じて適切な評価関数を呼び出し
    match stack {
        AccumulatorStackVariant::LayerStacks(s) => {
            update_and_evaluate_layer_stacks(network, pos, s)
        }
        AccumulatorStackVariant::HalfKA256(s) => update_and_evaluate_halfka_256(network, pos, s),
        AccumulatorStackVariant::HalfKA512(s) => update_and_evaluate_halfka_512(network, pos, s),
        AccumulatorStackVariant::HalfKA1024(s) => update_and_evaluate_halfka_1024(network, pos, s),
        AccumulatorStackVariant::HalfKA1024_8_32(s) => {
            update_and_evaluate_halfka_1024_8_32(network, pos, s)
        }
        AccumulatorStackVariant::HalfKP256(s) => update_and_evaluate_halfkp_256(network, pos, s),
        AccumulatorStackVariant::HalfKP512(s) => update_and_evaluate_halfkp_512(network, pos, s),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::position::SFEN_HIRATE;

    /// NNUEが初期化されていない場合のフォールバック動作をテスト
    /// tournamentビルドではフォールバックがないため、このテストはスキップ
    #[test]
    #[cfg(not(feature = "tournament"))]
    fn test_evaluate_fallback() {
        let mut pos = Position::new();
        pos.set_sfen(SFEN_HIRATE).unwrap();
        let mut stack = AccumulatorStackVariant::new_default();

        // NNUEが初期化されていない場合はフォールバック
        let value = evaluate_dispatch(&pos, &mut stack);

        // フォールバック評価が動作することを確認
        assert!(value.raw().abs() < 1000);
    }

    /// AccumulatorStackVariant を使った評価のテスト
    /// NNUEが未初期化でもフォールバックで評価が動作することを確認
    #[test]
    #[cfg(not(feature = "tournament"))]
    fn test_accumulator_stack_variant_fallback() {
        let mut pos = Position::new();
        pos.set_sfen(SFEN_HIRATE).unwrap();
        let mut stack = AccumulatorStackVariant::new_default();

        // 1回目の evaluate: NNUEが未初期化なのでフォールバック評価
        let value1 = evaluate_dispatch(&pos, &mut stack);

        // 2回目も動作することを確認
        let value2 = evaluate_dispatch(&pos, &mut stack);

        // フォールバックの駒得評価は手番に依存して符号が変わる可能性があるが、
        // ここでは「評価が成功した」ことのみ検証する。
        let _ = (value1, value2);
    }

    /// NNUENetwork のアーキテクチャ自動検出テスト
    ///
    /// 外部NNUEファイルが必要なため通常はスキップ。
    /// 実行方法: `NNUE_TEST_FILE=/path/to/file.nnue cargo test test_nnue_network_auto_detect_layer_stacks -- --ignored`
    ///
    /// テスト結果 (epoch82.nnue):
    /// - LayerStacks として正しく認識される
    /// - 評価値: 0 (学習初期のモデル)
    #[test]
    #[ignore]
    fn test_nnue_network_auto_detect_layer_stacks() {
        let path = std::env::var("NNUE_TEST_FILE")
            .unwrap_or_else(|_| "/path/to/your/layer_stacks.nnue".to_string());
        let network = match NNUENetwork::load(path) {
            Ok(n) => n,
            Err(e) => {
                eprintln!("Skipping test: {e}");
                return;
            }
        };

        // LayerStacks として認識されることを確認
        assert!(network.is_layer_stacks(), "epoch82.nnue should be detected as LayerStacks");
        assert_eq!(network.architecture_name(), "LayerStacks");

        // LayerStacks 用の評価が動作することを確認
        let mut pos = crate::position::Position::new();
        pos.set_sfen(SFEN_HIRATE).unwrap();

        let mut acc = crate::nnue::AccumulatorLayerStacks::new();
        network.refresh_accumulator_layer_stacks(&pos, &mut acc);

        let value = network.evaluate_layer_stacks(&pos, &acc);
        eprintln!("LayerStacks evaluate: {}", value.raw());

        // 評価値が妥当な範囲内
        assert!(value.raw().abs() < 1000);
    }

    /// parse_fv_scale_from_arch のユニットテスト
    #[test]
    fn test_parse_fv_scale_from_arch() {
        // bullet-shogi 形式の arch_str
        assert_eq!(
            parse_fv_scale_from_arch(
                "Features=HalfKA_hm^[73305->256x2]-SCReLU,fv_scale=13,qa=127,qb=64,scale=600"
            ),
            Some(13)
        );
        assert_eq!(
            parse_fv_scale_from_arch(
                "Features=HalfKA_hm^[73305->512x2]-SCReLU,fv_scale=20,qa=127,qb=64,scale=400"
            ),
            Some(20)
        );
        assert_eq!(
            parse_fv_scale_from_arch(
                "Features=HalfKA_hm^[73305->1024x2]-SCReLU,fv_scale=16,qa=127,qb=64,scale=508"
            ),
            Some(16)
        );

        // fv_scale が含まれていない従来形式
        assert_eq!(parse_fv_scale_from_arch("Features=HalfKP[125388->256x2]"), None);
        assert_eq!(parse_fv_scale_from_arch("Features=HalfKA_hm^[73305->512x2]"), None);

        // 空文字列
        assert_eq!(parse_fv_scale_from_arch(""), None);

        // 不正な fv_scale 値（文字列）
        assert_eq!(
            parse_fv_scale_from_arch("Features=HalfKA_hm^[73305->256x2],fv_scale=abc"),
            None
        );
    }

    /// parse_fv_scale_from_arch の境界値・エラーケーステスト
    #[test]
    fn test_parse_fv_scale_edge_cases() {
        // 境界値（許容範囲内）
        assert_eq!(parse_fv_scale_from_arch("fv_scale=1"), Some(1));
        assert_eq!(parse_fv_scale_from_arch("fv_scale=128"), Some(128));
        assert_eq!(parse_fv_scale_from_arch("fv_scale=64"), Some(64));

        // 境界値（範囲外 - ゼロ除算防止）
        assert_eq!(parse_fv_scale_from_arch("fv_scale=0"), None);
        assert_eq!(parse_fv_scale_from_arch("fv_scale=129"), None);

        // 不正な値（負数）
        assert_eq!(parse_fv_scale_from_arch("fv_scale=-1"), None);
        assert_eq!(parse_fv_scale_from_arch("fv_scale=-100"), None);

        // 不正な値（極端に大きい値）
        assert_eq!(parse_fv_scale_from_arch("fv_scale=99999"), None);
        assert_eq!(parse_fv_scale_from_arch("fv_scale=2147483647"), None);

        // ホワイトスペースを含む（パース失敗を期待）
        assert_eq!(parse_fv_scale_from_arch("fv_scale= 16"), None);
        assert_eq!(parse_fv_scale_from_arch("fv_scale=16 "), None);

        // 複数の fv_scale がある場合（最初のものが使用される）
        assert_eq!(parse_fv_scale_from_arch("fv_scale=10,fv_scale=20"), Some(10));

        // fv_scale= の後に何もない
        assert_eq!(parse_fv_scale_from_arch("fv_scale="), None);

        // 小数点を含む（パース失敗を期待）
        assert_eq!(parse_fv_scale_from_arch("fv_scale=16.5"), None);

        // プレフィックスが部分一致する場合（マッチしない）
        assert_eq!(parse_fv_scale_from_arch("my_fv_scale=16"), None);
        assert_eq!(parse_fv_scale_from_arch("fv_scale_v2=16"), None);
    }

    /// parse_arch_dimensions のユニットテスト
    #[test]
    fn test_parse_arch_dimensions() {
        // nnue-pytorch 形式 (ネスト構造、出力→入力の順)
        // 実際のファイル例: "Network=AffineTransform[1<-96](ClippedReLU[96](AffineTransform[96<-8](...)))"
        let arch = "Features=HalfKA_hm[73305->512x2],Network=AffineTransform[1<-96](ClippedReLU[96](AffineTransform[96<-8](ClippedReLU[8](AffineTransform[8<-1024](InputSlice[1024(0:1024)])))))";
        assert_eq!(NNUENetwork::parse_arch_dimensions(arch), (512, 8, 96));

        // nnue-pytorch 形式 (1024次元)
        let arch = "Features=HalfKA_hm[73305->1024x2],Network=AffineTransform[1<-96](ClippedReLU[96](AffineTransform[96<-8](ClippedReLU[8](AffineTransform[8<-2048](InputSlice[2048(0:2048)])))))";
        assert_eq!(NNUENetwork::parse_arch_dimensions(arch), (1024, 8, 96));

        // bullet-shogi 形式 (l2=, l3= パターン)
        let arch = "Features=HalfKA_hm^[73305->512x2]-SCReLU,fv_scale=13,l2=8,l3=96,qa=127,qb=64";
        assert_eq!(NNUENetwork::parse_arch_dimensions(arch), (512, 8, 96));

        // bullet-shogi 形式 (1024次元)
        let arch = "Features=HalfKA_hm^[73305->1024x2]-SCReLU,fv_scale=16,l2=8,l3=96,qa=127,qb=64";
        assert_eq!(NNUENetwork::parse_arch_dimensions(arch), (1024, 8, 96));

        // bullet-shogi 形式 (256次元, 32-32)
        let arch = "Features=HalfKA_hm^[73305->256x2]-SCReLU,fv_scale=13,l2=32,l3=32,qa=127,qb=64";
        assert_eq!(NNUENetwork::parse_arch_dimensions(arch), (256, 32, 32));

        // L1のみ取得できる場合 (L2/L3 は 0)
        let arch = "Features=HalfKP[125388->256x2]";
        assert_eq!(NNUENetwork::parse_arch_dimensions(arch), (256, 0, 0));

        // Pairwise 形式 (512/2x2 = 出力512、Pairwise乗算で256に縮小)
        let arch = "Features=HalfKA_hm[73305->512/2x2]-Pairwise,fv_scale=10,l1_input=512,l2=8,l3=96,qa=255,qb=64,scale=1600,pairwise=true";
        assert_eq!(NNUENetwork::parse_arch_dimensions(arch), (512, 8, 96));

        // Pairwise 形式 (256/2x2)
        let arch = "Features=HalfKA_hm[73305->256/2x2]-Pairwise,fv_scale=10,l1_input=256,l2=32,l3=32,qa=255,qb=64";
        assert_eq!(NNUENetwork::parse_arch_dimensions(arch), (256, 32, 32));

        // 何も取得できない場合
        assert_eq!(NNUENetwork::parse_arch_dimensions("unknown"), (0, 0, 0));
        assert_eq!(NNUENetwork::parse_arch_dimensions(""), (0, 0, 0));
    }
}
