# Phase 2: NNUE実装 - 詳細設計書

> **親ドキュメント**: [Rust将棋AI実装要件書](./rust-shogi-ai-requirements.md)  
> **該当セクション**: 9. 開発マイルストーン - Phase 2: NNUE実装（3週間）  
> **前提条件**: [Phase 1: 基盤実装](./phase1-foundation-design.md) の完了

## 1. 概要

Phase 2では、NNUE（Efficiently Updatable Neural Network）評価関数を実装します。NNUEは差分計算により高速に動作するニューラルネットワークベースの評価関数で、現代の将棋AIの強さの核心技術です。

### 1.1 目標
- HalfKP（256×2-32-32）ネットワーク構造の実装
- 差分計算による高速な評価値更新
- SIMD命令による最適化（WASM: simd128、ネイティブ: AVX2）
- 既存の学習済みネットワークの読み込み

### 1.2 成果物
- `nnue/mod.rs`: NNUEモジュールのエントリポイント
- `nnue/features.rs`: HalfKP特徴量抽出
- `nnue/network.rs`: ニューラルネットワーク構造
- `nnue/accumulator.rs`: 差分計算用アキュムレータ
- `nnue/weights.rs`: 重みパラメータ管理
- パフォーマンステストスイート

## 2. NNUEアーキテクチャ

### 2.1 ネットワーク構造

```
入力層（HalfKP特徴量）
    ↓
特徴変換層（256×2）← 差分計算で高速化
    ↓
隠れ層1（32ユニット、ClippedReLU）
    ↓
隠れ層2（32ユニット、ClippedReLU）
    ↓
出力層（1、評価値）
```

### 2.2 HalfKP特徴量

HalfKPは「自玉の位置」と「各駒（玉を除く）」の組み合わせで特徴量を構成します。

```rust
/// HalfKP特徴量の定義
pub mod halfkp {
    /// 駒の種類と位置をエンコード
    #[derive(Clone, Copy, Debug)]
    pub struct BonaPiece(u16);
    
    impl BonaPiece {
        /// 盤上の駒
        pub fn from_board(piece: Piece, sq: Square) -> Self {
            let mut index = 0u16;
            
            // 駒種のオフセット
            let piece_offset = match (piece.piece_type, piece.promoted) {
                (PieceType::Pawn, false) => 0,
                (PieceType::Lance, false) => 81,
                (PieceType::Knight, false) => 162,
                (PieceType::Silver, false) => 243,
                (PieceType::Gold, false) => 324,
                (PieceType::Bishop, false) => 405,
                (PieceType::Rook, false) => 486,
                (PieceType::King, false) => unreachable!(), // 玉は含まない
                (PieceType::Pawn, true) => 567,   // と金
                (PieceType::Lance, true) => 648,  // 成香
                (PieceType::Knight, true) => 729, // 成桂
                (PieceType::Silver, true) => 810, // 成銀
                (PieceType::Bishop, true) => 891, // 馬
                (PieceType::Rook, true) => 972,   // 龍
                _ => unreachable!(),
            };
            
            // 色による調整（後手の駒は別扱い）
            let color_offset = if piece.color == Color::White { 1053 } else { 0 };
            
            index = piece_offset + sq.0 as u16 + color_offset;
            BonaPiece(index)
        }
        
        /// 持ち駒
        pub fn from_hand(piece_type: PieceType, color: Color, count: u8) -> Self {
            let base = 2106; // 盤上の駒の後
            
            let piece_offset = match piece_type {
                PieceType::Pawn => 0,
                PieceType::Lance => 19,
                PieceType::Knight => 38,
                PieceType::Silver => 57,
                PieceType::Gold => 76,
                PieceType::Bishop => 95,
                PieceType::Rook => 114,
                _ => unreachable!(),
            };
            
            let color_offset = if color == Color::White { 133 } else { 0 };
            
            let index = base + piece_offset + (count - 1) as u16 + color_offset;
            BonaPiece(index)
        }
        
        pub fn index(self) -> usize { self.0 as usize }
    }
    
    /// 特徴量の総数
    pub const FE_END: usize = 2106 + 266; // 盤上 + 持ち駒
    
    /// HalfKP特徴量のインデックス計算
    pub fn halfkp_index(king_sq: Square, piece: BonaPiece) -> usize {
        king_sq.0 as usize * FE_END + piece.index()
    }
}
```

## 3. ニューラルネットワーク実装

### 3.1 ネットワーク構造体

```rust
use std::arch::x86_64::*;

/// NNUE評価関数
pub struct NNUEEvaluator {
    feature_transformer: FeatureTransformer,
    network: Network,
}

/// 特徴変換層
pub struct FeatureTransformer {
    weights: AlignedVector<i16>, // [INPUT_DIM][256]
    biases: AlignedVector<i32>,  // [256]
}

/// ニューラルネットワーク本体
pub struct Network {
    hidden1_weights: AlignedVector<i8>, // [512][32]
    hidden1_biases: AlignedVector<i32>, // [32]
    hidden2_weights: AlignedVector<i8>, // [32][32]
    hidden2_biases: AlignedVector<i32>, // [32]
    output_weights: AlignedVector<i8>,  // [32][1]
    output_bias: i32,
}

/// アライメント保証付きベクター（SIMD用）
#[repr(align(32))]
pub struct AlignedVector<T> {
    data: Vec<T>,
}

impl<T: Clone> AlignedVector<T> {
    pub fn new(size: usize, init: T) -> Self {
        let mut data = Vec::with_capacity(size);
        data.resize(size, init);
        AlignedVector { data }
    }
    
    pub fn as_ptr(&self) -> *const T {
        self.data.as_ptr()
    }
    
    pub fn as_mut_ptr(&mut self) -> *mut T {
        self.data.as_mut_ptr()
    }
}
```

### 3.2 推論実装

```rust
impl NNUEEvaluator {
    /// 局面を評価
    pub fn evaluate(&self, pos: &Position, accumulator: &Accumulator) -> i32 {
        // 手番に応じたアキュムレータを選択
        let (acc_us, acc_them) = if pos.side_to_move == Color::Black {
            (&accumulator.black, &accumulator.white)
        } else {
            (&accumulator.white, &accumulator.black)
        };
        
        // ネットワークの推論
        let output = self.network.propagate(acc_us, acc_them);
        
        // 評価値のスケーリング（FV_SCALEは定数）
        (output * FV_SCALE) >> 16
    }
}

impl Network {
    /// ネットワークの順伝播
    pub fn propagate(&self, acc_us: &[i16; 256], acc_them: &[i16; 256]) -> i32 {
        // 入力層の結合（512次元）
        let mut input = [0i8; 512];
        self.transform_features(acc_us, acc_them, &mut input);
        
        // 隠れ層1
        let mut hidden1 = [0i32; 32];
        self.affine_propagate::<512, 32>(
            &input,
            &self.hidden1_weights,
            &self.hidden1_biases,
            &mut hidden1,
        );
        
        // ClippedReLU活性化
        let mut hidden1_out = [0i8; 32];
        self.clipped_relu::<32>(&hidden1, &mut hidden1_out);
        
        // 隠れ層2
        let mut hidden2 = [0i32; 32];
        self.affine_propagate::<32, 32>(
            &hidden1_out,
            &self.hidden2_weights,
            &self.hidden2_biases,
            &mut hidden2,
        );
        
        // ClippedReLU活性化
        let mut hidden2_out = [0i8; 32];
        self.clipped_relu::<32>(&hidden2, &mut hidden2_out);
        
        // 出力層
        let mut output = self.output_bias;
        for i in 0..32 {
            output += hidden2_out[i] as i32 * self.output_weights.data[i] as i32;
        }
        
        output
    }
    
    /// 特徴量の変換（量子化）
    fn transform_features(&self, us: &[i16; 256], them: &[i16; 256], output: &mut [i8; 512]) {
        for i in 0..256 {
            // 先手視点の特徴
            output[i] = clamp(us[i] >> SHIFT, -127, 127) as i8;
            // 後手視点の特徴
            output[i + 256] = clamp(them[i] >> SHIFT, -127, 127) as i8;
        }
    }
}
```

### 3.3 SIMD最適化

```rust
#[cfg(target_arch = "x86_64")]
impl Network {
    /// AVX2を使用したアフィン変換
    unsafe fn affine_propagate_avx2<const IN: usize, const OUT: usize>(
        &self,
        input: &[i8; IN],
        weights: &AlignedVector<i8>,
        biases: &[i32; OUT],
        output: &mut [i32; OUT],
    ) {
        // バイアスをコピー
        output.copy_from_slice(biases);
        
        // 重みを適用
        for i in 0..OUT {
            let mut sum = _mm256_setzero_si256();
            
            // 32要素ずつ処理
            for j in (0..IN).step_by(32) {
                // 入力をロード
                let input_vec = _mm256_loadu_si256(
                    input.as_ptr().add(j) as *const __m256i
                );
                
                // 重みをロード
                let weight_vec = _mm256_load_si256(
                    weights.as_ptr().add(i * IN + j) as *const __m256i
                );
                
                // 8ビット整数の積和演算
                let product = _mm256_maddubs_epi16(input_vec, weight_vec);
                
                // 32ビットに拡張して累積
                let product_lo = _mm256_cvtepi16_epi32(_mm256_castsi256_si128(product));
                let product_hi = _mm256_cvtepi16_epi32(
                    _mm256_extracti128_si256(product, 1)
                );
                
                sum = _mm256_add_epi32(sum, product_lo);
                sum = _mm256_add_epi32(sum, product_hi);
            }
            
            // 水平加算
            let sum_array: [i32; 8] = std::mem::transmute(sum);
            output[i] += sum_array.iter().sum::<i32>();
        }
    }
    
    /// ClippedReLU活性化関数（AVX2版）
    unsafe fn clipped_relu_avx2<const N: usize>(
        &self,
        input: &[i32; N],
        output: &mut [i8; N],
    ) {
        let zero = _mm256_setzero_si256();
        let max_val = _mm256_set1_epi32(127);
        
        for i in (0..N).step_by(8) {
            // 入力をロード
            let val = _mm256_loadu_si256(
                input.as_ptr().add(i) as *const __m256i
            );
            
            // ClippedReLU: max(0, min(x, 127))
            let clipped = _mm256_min_epi32(_mm256_max_epi32(val, zero), max_val);
            
            // 8ビットに変換
            let packed = _mm256_packs_epi32(clipped, clipped);
            let packed = _mm256_packs_epi16(packed, packed);
            
            // 結果を保存
            let result = _mm256_extract_epi64(packed, 0);
            *(output.as_mut_ptr().add(i) as *mut i64) = result;
        }
    }
}
```

## 4. 差分計算（Accumulator）

### 4.1 アキュムレータ構造

```rust
/// 差分計算用アキュムレータ
pub struct Accumulator {
    /// 先手視点の特徴変換結果
    black: AlignedVector<i16>, // [256]
    /// 後手視点の特徴変換結果
    white: AlignedVector<i16>, // [256]
    /// 計算済みフラグ
    computed_black: bool,
    computed_white: bool,
}

impl Accumulator {
    pub fn new() -> Self {
        Accumulator {
            black: AlignedVector::new(256, 0),
            white: AlignedVector::new(256, 0),
            computed_black: false,
            computed_white: false,
        }
    }
    
    /// 全計算（初期化時）
    pub fn refresh(&mut self, pos: &Position, evaluator: &NNUEEvaluator) {
        self.computed_black = false;
        self.computed_white = false;
        
        // 先手視点
        if let Some(king_sq) = pos.king_square(Color::Black) {
            self.refresh_side(pos, king_sq, Color::Black, evaluator);
            self.computed_black = true;
        }
        
        // 後手視点
        if let Some(king_sq) = pos.king_square(Color::White) {
            let king_sq = king_sq.flip(); // 後手視点に変換
            self.refresh_side(pos, king_sq, Color::White, evaluator);
            self.computed_white = true;
        }
    }
    
    /// 片側の全計算
    fn refresh_side(
        &mut self,
        pos: &Position,
        king_sq: Square,
        perspective: Color,
        evaluator: &NNUEEvaluator,
    ) {
        let accumulator = if perspective == Color::Black {
            &mut self.black
        } else {
            &mut self.white
        };
        
        // バイアスで初期化
        accumulator.data.copy_from_slice(&evaluator.feature_transformer.biases.data);
        
        // アクティブな特徴を収集
        let mut active_features = Vec::with_capacity(32);
        
        // 盤上の駒
        for color in 0..2 {
            for piece_type in 0..8 {
                if piece_type == PieceType::King as usize {
                    continue;
                }
                
                let mut bb = pos.piece_bb[color][piece_type];
                while let Some(sq) = bb.pop_lsb() {
                    let piece = Piece::new(
                        unsafe { std::mem::transmute(piece_type as u8) },
                        unsafe { std::mem::transmute(color as u8) },
                    );
                    
                    let bona_piece = if perspective == Color::Black {
                        BonaPiece::from_board(piece, sq)
                    } else {
                        BonaPiece::from_board(piece.flip(), sq.flip())
                    };
                    
                    let index = halfkp::halfkp_index(king_sq, bona_piece);
                    active_features.push(index);
                }
            }
        }
        
        // 持ち駒
        for color in 0..2 {
            for piece_type in 0..7 {
                let count = pos.hands[color][piece_type];
                if count > 0 {
                    let pt = unsafe { std::mem::transmute(piece_type as u8) };
                    let c = unsafe { std::mem::transmute(color as u8) };
                    
                    let bona_piece = if perspective == Color::Black {
                        BonaPiece::from_hand(pt, c, count)
                    } else {
                        BonaPiece::from_hand(pt, c.opposite(), count)
                    };
                    
                    let index = halfkp::halfkp_index(king_sq, bona_piece);
                    active_features.push(index);
                }
            }
        }
        
        // 特徴を適用
        self.apply_features(
            accumulator,
            &active_features,
            &evaluator.feature_transformer.weights,
        );
    }
}
```

### 4.2 差分更新

```rust
/// 差分更新のための変更情報
pub struct AccumulatorUpdate {
    /// 削除される特徴のインデックス
    removed: Vec<usize>,
    /// 追加される特徴のインデックス
    added: Vec<usize>,
}

impl Accumulator {
    /// 差分更新
    pub fn update(
        &mut self,
        update: &AccumulatorUpdate,
        perspective: Color,
        evaluator: &NNUEEvaluator,
    ) {
        let accumulator = if perspective == Color::Black {
            &mut self.black
        } else {
            &mut self.white
        };
        
        let weights = &evaluator.feature_transformer.weights;
        
        // SIMD版またはスカラー版を選択
        #[cfg(target_arch = "x86_64")]
        unsafe {
            if is_x86_feature_detected!("avx2") {
                self.update_avx2(accumulator, &update.removed, &update.added, weights);
                return;
            }
        }
        
        self.update_scalar(accumulator, &update.removed, &update.added, weights);
    }
    
    /// スカラー版の差分更新
    fn update_scalar(
        &self,
        accumulator: &mut AlignedVector<i16>,
        removed: &[usize],
        added: &[usize],
        weights: &AlignedVector<i16>,
    ) {
        // 削除
        for &index in removed {
            let offset = index * 256;
            for i in 0..256 {
                accumulator.data[i] -= weights.data[offset + i];
            }
        }
        
        // 追加
        for &index in added {
            let offset = index * 256;
            for i in 0..256 {
                accumulator.data[i] += weights.data[offset + i];
            }
        }
    }
    
    /// AVX2版の差分更新
    #[cfg(target_arch = "x86_64")]
    unsafe fn update_avx2(
        &self,
        accumulator: &mut AlignedVector<i16>,
        removed: &[usize],
        added: &[usize],
        weights: &AlignedVector<i16>,
    ) {
        // 16要素（32バイト）ずつ処理
        for i in (0..256).step_by(16) {
            let mut acc = _mm256_load_si256(
                accumulator.as_ptr().add(i) as *const __m256i
            );
            
            // 削除
            for &index in removed {
                let offset = index * 256 + i;
                let weight = _mm256_load_si256(
                    weights.as_ptr().add(offset) as *const __m256i
                );
                acc = _mm256_sub_epi16(acc, weight);
            }
            
            // 追加
            for &index in added {
                let offset = index * 256 + i;
                let weight = _mm256_load_si256(
                    weights.as_ptr().add(offset) as *const __m256i
                );
                acc = _mm256_add_epi16(acc, weight);
            }
            
            _mm256_store_si256(
                accumulator.as_mut_ptr().add(i) as *mut __m256i,
                acc,
            );
        }
    }
}
```

## 5. 重みファイルの読み込み

### 5.1 ファイルフォーマット

```rust
/// NNUEファイルヘッダー
#[repr(C, packed)]
pub struct NNUEHeader {
    magic: [u8; 4],    // "NNUE"
    version: u32,      // バージョン番号
    architecture: u32, // アーキテクチャID
    size: u32,         // ファイルサイズ
}

/// 重みファイルリーダー
pub struct WeightReader {
    data: Vec<u8>,
    offset: usize,
}

impl WeightReader {
    pub fn from_file(path: &str) -> Result<Self, std::io::Error> {
        let data = std::fs::read(path)?;
        Ok(WeightReader { data, offset: 0 })
    }
    
    pub fn read_header(&mut self) -> Result<NNUEHeader, &'static str> {
        if self.data.len() < std::mem::size_of::<NNUEHeader>() {
            return Err("File too small");
        }
        
        let header: NNUEHeader = unsafe {
            std::ptr::read_unaligned(self.data.as_ptr() as *const NNUEHeader)
        };
        
        if &header.magic != b"NNUE" {
            return Err("Invalid magic number");
        }
        
        self.offset = std::mem::size_of::<NNUEHeader>();
        Ok(header)
    }
    
    pub fn read_weights<T: Copy>(&mut self, count: usize) -> Result<Vec<T>, &'static str> {
        let size = count * std::mem::size_of::<T>();
        if self.offset + size > self.data.len() {
            return Err("Not enough data");
        }
        
        let mut result = Vec::with_capacity(count);
        unsafe {
            let ptr = self.data.as_ptr().add(self.offset) as *const T;
            for i in 0..count {
                result.push(*ptr.add(i));
            }
        }
        
        self.offset += size;
        Ok(result)
    }
}
```

### 5.2 やねうら王形式の互換性

```rust
/// やねうら王形式のNNUEファイル読み込み
pub fn load_yaneuraou_nnue(path: &str) -> Result<NNUEEvaluator, Box<dyn std::error::Error>> {
    let mut reader = WeightReader::from_file(path)?;
    let header = reader.read_header()?;
    
    // バージョンチェック
    const YANEURAOU_HALFKP_256X2_32_32: u32 = 0x7AF32F16;
    if header.architecture != YANEURAOU_HALFKP_256X2_32_32 {
        return Err("Unsupported architecture".into());
    }
    
    // 特徴変換層の重み
    let ft_weights = reader.read_weights::<i16>(halfkp::FE_END * 256)?;
    let ft_biases = reader.read_weights::<i32>(256)?;
    
    // 隠れ層1
    let hidden1_weights = reader.read_weights::<i8>(512 * 32)?;
    let hidden1_biases = reader.read_weights::<i32>(32)?;
    
    // 隠れ層2
    let hidden2_weights = reader.read_weights::<i8>(32 * 32)?;
    let hidden2_biases = reader.read_weights::<i32>(32)?;
    
    // 出力層
    let output_weights = reader.read_weights::<i8>(32)?;
    let output_bias = reader.read_weights::<i32>(1)?[0];
    
    // 評価関数を構築
    Ok(NNUEEvaluator {
        feature_transformer: FeatureTransformer {
            weights: AlignedVector { data: ft_weights },
            biases: AlignedVector { data: ft_biases },
        },
        network: Network {
            hidden1_weights: AlignedVector { data: hidden1_weights },
            hidden1_biases: AlignedVector { data: hidden1_biases },
            hidden2_weights: AlignedVector { data: hidden2_weights },
            hidden2_biases: AlignedVector { data: hidden2_biases },
            output_weights: AlignedVector { data: output_weights },
            output_bias,
        },
    })
}
```

## 6. 統合と最適化

### 6.1 Phase 1との統合

```rust
/// 評価関数トレイト（Phase 1で定義）
pub trait Evaluator {
    fn evaluate(&self, pos: &Position) -> i32;
}

/// NNUE評価関数のラッパー
pub struct NNUEEvaluatorWrapper {
    evaluator: NNUEEvaluator,
    accumulator_stack: Vec<Accumulator>,
}

impl NNUEEvaluatorWrapper {
    pub fn new(weights_path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let evaluator = load_yaneuraou_nnue(weights_path)?;
        Ok(NNUEEvaluatorWrapper {
            evaluator,
            accumulator_stack: vec![Accumulator::new()],
        })
    }
    
    /// 手を進める際のアキュムレータ更新
    pub fn do_move(&mut self, pos: &Position, mv: Move) {
        // 新しいアキュムレータを作成
        let mut new_acc = self.accumulator_stack.last().unwrap().clone();
        
        // 差分更新を計算
        let update = calculate_update(pos, mv);
        
        // 両視点を更新
        new_acc.update(&update, Color::Black, &self.evaluator);
        new_acc.update(&update, Color::White, &self.evaluator);
        
        self.accumulator_stack.push(new_acc);
    }
    
    /// 手を戻す
    pub fn undo_move(&mut self) {
        self.accumulator_stack.pop();
    }
}

impl Evaluator for NNUEEvaluatorWrapper {
    fn evaluate(&self, pos: &Position) -> i32 {
        let accumulator = self.accumulator_stack.last().unwrap();
        self.evaluator.evaluate(pos, accumulator)
    }
}
```

### 6.2 キャッシュ最適化

```rust
/// 評価値キャッシュ
pub struct EvalCache {
    entries: Vec<EvalCacheEntry>,
    size_mask: usize,
}

#[derive(Clone, Copy)]
struct EvalCacheEntry {
    hash: u64,
    eval: i32,
}

impl EvalCache {
    pub fn new(size_mb: usize) -> Self {
        let size = (size_mb * 1024 * 1024) / std::mem::size_of::<EvalCacheEntry>();
        let size = size.next_power_of_two();
        
        EvalCache {
            entries: vec![EvalCacheEntry { hash: 0, eval: 0 }; size],
            size_mask: size - 1,
        }
    }
    
    pub fn probe(&self, hash: u64) -> Option<i32> {
        let index = (hash as usize) & self.size_mask;
        let entry = &self.entries[index];
        
        if entry.hash == hash {
            Some(entry.eval)
        } else {
            None
        }
    }
    
    pub fn store(&mut self, hash: u64, eval: i32) {
        let index = (hash as usize) & self.size_mask;
        self.entries[index] = EvalCacheEntry { hash, eval };
    }
}
```

## 7. テスト計画

### 7.1 単体テスト

```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_halfkp_index() {
        // 5九玉に対する5五歩のインデックス
        let king_sq = Square::new(4, 8);
        let pawn = BonaPiece::from_board(
            Piece::new(PieceType::Pawn, Color::Black),
            Square::new(4, 4),
        );
        
        let index = halfkp::halfkp_index(king_sq, pawn);
        assert_eq!(index, 76 * halfkp::FE_END + 40);
    }
    
    #[test]
    fn test_accumulator_refresh() {
        let pos = Position::startpos();
        let evaluator = create_test_evaluator();
        let mut acc = Accumulator::new();
        
        acc.refresh(&pos, &evaluator);
        
        // 初期局面では特定の特徴が有効
        assert!(acc.computed_black);
        assert!(acc.computed_white);
    }
    
    #[test]
    fn test_differential_update() {
        let mut pos = Position::startpos();
        let evaluator = create_test_evaluator();
        let mut acc = Accumulator::new();
        
        // 初期計算
        acc.refresh(&pos, &evaluator);
        let initial_black = acc.black.data.clone();
        
        // 7六歩
        let mv = Move::normal(Square::new(7, 6), Square::new(7, 5), false);
        pos.do_move(mv);
        
        // 差分更新
        let update = calculate_update(&pos, mv);
        acc.update(&update, Color::Black, &evaluator);
        
        // 全計算で検証
        let mut acc_full = Accumulator::new();
        acc_full.refresh(&pos, &evaluator);
        
        assert_eq!(acc.black.data, acc_full.black.data);
    }
}
```

### 7.2 パフォーマンステスト

```rust
#[cfg(test)]
mod bench {
    use super::*;
    use test::Bencher;
    
    #[bench]
    fn bench_evaluate(b: &mut Bencher) {
        let pos = Position::startpos();
        let evaluator = create_test_evaluator();
        let mut acc = Accumulator::new();
        acc.refresh(&pos, &evaluator);
        
        b.iter(|| {
            test::black_box(evaluator.evaluate(&pos, &acc));
        });
    }
    
    #[bench]
    fn bench_refresh_accumulator(b: &mut Bencher) {
        let pos = Position::startpos();
        let evaluator = create_test_evaluator();
        let mut acc = Accumulator::new();
        
        b.iter(|| {
            acc.refresh(&pos, &evaluator);
            test::black_box(&acc);
        });
    }
    
    #[bench]
    fn bench_update_accumulator(b: &mut Bencher) {
        let pos = Position::startpos();
        let evaluator = create_test_evaluator();
        let mut acc = Accumulator::new();
        acc.refresh(&pos, &evaluator);
        
        let mv = Move::normal(Square::new(7, 6), Square::new(7, 5), false);
        let update = calculate_update(&pos, mv);
        
        b.iter(|| {
            acc.update(&update, Color::Black, &evaluator);
            test::black_box(&acc);
        });
    }
}
```

### 7.3 互換性テスト

```rust
#[test]
fn test_yaneuraou_compatibility() {
    // やねうら王の評価関数ファイルを読み込み
    let evaluator = load_yaneuraou_nnue("test_data/nn.bin").unwrap();
    
    // 既知の局面での評価値を確認
    let test_positions = vec![
        ("startpos", 0, 50),  // 初期局面は±50以内
        ("4k4/9/9/9/9/9/9/9/4K4 b - 1", 0, 100), // 裸玉
    ];
    
    for (sfen, expected, tolerance) in test_positions {
        let pos = Position::from_sfen(sfen).unwrap();
        let mut acc = Accumulator::new();
        acc.refresh(&pos, &evaluator);
        
        let eval = evaluator.evaluate(&pos, &acc);
        assert!(
            (eval - expected).abs() <= tolerance,
            "Position {} evaluated to {}, expected {}±{}",
            sfen, eval, expected, tolerance
        );
    }
}
```

## 8. 実装スケジュール

### Week 1: 基本構造とネットワーク
- Day 1-2: HalfKP特徴量の実装
- Day 3-4: ニューラルネットワーク構造
- Day 5-6: 基本的な推論実装
- Day 7: 単体テスト

### Week 2: 差分計算とSIMD
- Day 1-2: アキュムレータの実装
- Day 3-4: 差分更新アルゴリズム
- Day 5: WASM simd128最適化
- Day 6: ネイティブAVX2最適化
- Day 7: パフォーマンステスト

### Week 3: 統合と最適化
- Day 1-2: 重みファイル読み込み
- Day 3: Phase 1との統合
- Day 4: 評価値キャッシュ
- Day 5-6: 全体テストとデバッグ
- Day 7: ドキュメント整備

## 9. 成功基準

### 機能要件
- [ ] HalfKP特徴量の正確な抽出
- [ ] 差分計算による高速更新
- [ ] やねうら王形式のNNUEファイル対応
- [ ] 評価値の妥当性（既存エンジンとの比較）

### 性能要件
- [ ] 評価速度: 100万局面/秒以上
- [ ] 差分更新: 10μs以下
- [ ] メモリ使用量: 50MB以下（重みデータ含む）

### 品質要件
- [ ] WASM simd128対応（フォールバックあり）
- [ ] ネイティブAVX2対応（フォールバックあり）
- [ ] テストカバレッジ: 85%以上
- [ ] ベンチマークの安定性

## 10. リスクと対策

### 技術的リスク
1. **SIMD命令の互換性**
   - 対策: 実行時の機能検出とフォールバック
   - WASM/ネイティブ別の実装
   - スカラー版の並行実装

2. **差分計算の正確性**
   - 対策: 全計算との比較テスト
   - デバッグビルドでの検証モード

3. **重みファイルの互換性**
   - 対策: 複数フォーマット対応
   - バージョン管理

### 性能リスク
1. **目標性能の未達**
   - 対策: プロファイリングによる最適化
   - キャッシュ効率の改善

2. **メモリ帯域の制約**
   - 対策: データレイアウトの最適化
   - プリフェッチの活用

## 11. Phase 3への準備

Phase 2完了時に、以下が準備されている必要があります：

1. **高速な評価関数**: 探索の深さを支える基盤
2. **差分計算インフラ**: 探索中の効率的な評価
3. **SIMD最適化の知見**: Phase 3での活用
4. **性能測定基盤**: 継続的な最適化

これらにより、Phase 3での高度な探索技術の実装が可能になります。