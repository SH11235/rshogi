# Rust将棋AI実装要件書

## 1. プロジェクト概要

### 目的
既存のTypeScript実装を高性能なRust/WASM実装に置き換え、プロ級の棋力を持つ将棋AIエンジンを開発する。

  コア技術
  - αβ探索 + NNUE評価関数（HalfKP 256×2-32-32構造）
  - Lazy SMPによる並列探索（最大16スレッド）
  - 100万局面/秒以上の探索速度目標

  各エンジンから採用した要素
  - Apery: Magic Bitboard、履歴ヒューリスティック
  - Fairy-Stockfish: ProbCut、動的評価切り替え
  - やねうら王: NNUE実装詳細、教師データ生成手法
  - nshogi: メモリプール、詰み探索統合


### 基本方針
- **コアアルゴリズム**: αβ探索 + NNUE評価関数
- **実装言語**: Rust（WASM出力）
- **目標棋力**: アマ六段〜プロレベル
- **パフォーマンス目標**: 150-200万局面/秒（4コア時）

## 2. アーキテクチャ要件

### 2.1 全体構成
```
packages/rust-core/
├── src/
│   ├── ai/
│   │   ├── mod.rs          # AIモジュールのエントリポイント
│   │   ├── board.rs        # ビットボード実装
│   │   ├── movegen.rs      # 合法手生成
│   │   ├── search.rs       # 探索エンジン
│   │   ├── evaluate.rs     # 評価関数インターフェース
│   │   ├── nnue/           # NNUE評価関数実装
│   │   ├── tt.rs           # 置換表
│   │   ├── thread.rs       # 並列探索管理
│   │   └── time_mgmt.rs    # 時間管理
│   └── wasm_bindings.rs    # WASMインターフェース
└── tests/                   # テストスイート
```

### 2.2 データ構造

#### ビットボード表現
```rust
pub struct BitBoard {
    // 駒種別のビットボード（先手・後手）
    pieces: [[u128; 8]; 2],  // 玉、飛、角、金、銀、桂、香、歩
    
    // 持ち駒
    hands: [[u8; 7]; 2],     // 各駒種の枚数（玉を除く）
    
    // その他の状態
    side_to_move: Color,
    ply: u16,
}
```

#### 移動表現
```rust
pub struct Move {
    from: Option<Square>,     // None = 駒打ち
    to: Square,
    piece_type: PieceType,
    promote: bool,
    capture: Option<PieceType>,
}
```

## 3. 探索エンジン要件

### 3.1 基本アルゴリズム
- **メイン探索**: PVS（Principal Variation Search）
- **並列化**: Lazy SMP（4-16スレッド対応）
- **時間管理**: 動的時間配分

### 3.2 探索技術

#### 必須実装
1. **Null Move Pruning**
   - 動的深さ削減: R = 3 + depth/6
   - 検証探索による安全性確保

2. **Late Move Reductions (LMR)**
   - 履歴情報に基づく動的削減
   - PVノード・カットノードでの調整

3. **Aspiration Windows**
   - 初期窓幅: 17センチポーン
   - 動的拡張アルゴリズム

4. **Futility Pruning**
   - 深さベースのマージン計算
   - 静的評価による枝刈り

5. **History Heuristics**
   - Butterfly History
   - Capture History  
   - Continuation History（1,2,4,6手前）

#### 推奨実装
1. **Singular Extension**
   - TTムーブの特異性検証
   - 1-2手の探索延長

2. **ProbCut**
   - 浅い探索での早期カット
   - 統計的閾値による判定

3. **Multi-Cut**
   - 複数の手が失敗時の早期終了

### 3.3 置換表
```rust
pub struct TTEntry {
    key: u32,           // ハッシュキーの一部（上位32ビット）
    best_move: Move,    // 最善手
    value: Value,       // 評価値
    eval: Value,        // 静的評価値
    depth: Depth,       // 探索深さ
    bound: Bound,       // EXACT/UPPER/LOWER
    generation: u8,     // 世代管理
}

// クラスター設計（キャッシュライン考慮）
pub struct TTCluster {
    entries: [TTEntry; CLUSTER_SIZE], // CLUSTER_SIZE = 3
}
```

## 4. NNUE評価関数要件

### 4.1 ネットワーク構造
```
入力層: HalfKP特徴量（約40,000次元）
  ↓
特徴変換層: 256×2（先手視点・後手視点）
  ↓
隠れ層1: 32ユニット（ClippedReLU）
  ↓
隠れ層2: 32ユニット（ClippedReLU）
  ↓
出力層: 1（評価値）
```

### 4.2 実装詳細

#### 特徴量抽出
```rust
pub struct HalfKPIndex {
    king_sq: Square,
    piece: Piece,
    piece_sq: Square,
}

impl HalfKPIndex {
    pub fn index(&self) -> usize {
        self.king_sq as usize * FE_END + 
        encode_piece(self.piece, self.piece_sq)
    }
}
```

#### 差分計算
```rust
pub struct Accumulator {
    // 特徴変換層の出力
    accumulation: [[i16; 256]; 2], // [先手視点, 後手視点]
    
    // 計算済みフラグ
    computed: [bool; 2],
}

impl Accumulator {
    pub fn update(&mut self, removed: &[usize], added: &[usize]) {
        // 差分更新（SIMD使用）
        #[cfg(target_arch = "x86_64")]
        unsafe {
            self.update_avx2(removed, added);
        }
    }
}
```

### 4.3 量子化と最適化
- **重み量子化**: int16（特徴変換層）、int8（隠れ層）
- **SIMD最適化**: 
  - WASM: 128bit SIMD（simd128）
  - ネイティブ: AVX2必須、AVX512はオプション
- **メモリアライメント**: 32バイト境界

## 5. 高速化要件

### 5.1 合法手生成
- **Magic Bitboard**: 飛車・角の利き
- **事前計算テーブル**: その他の駒
- **ピン情報のキャッシュ**

### 5.2 並列処理
- **Lazy SMP**: 最大16スレッド
- **ロックフリー置換表**: CAS操作
- **スレッドローカル履歴**: 競合回避

### 5.3 メモリ最適化
- **メモリプール**: 頻繁な割り当ての回避
- **キャッシュ考慮**: ホットデータの局所化
- **プリフェッチ**: 置換表アクセス

## 6. 教師データと学習

### 6.1 教師データ生成
```bash
# 自己対戦による生成
# - 探索深度: 12以上
# - 評価値フィルタ: -800 ≤ cp ≤ 800
# - 序盤ランダム: 10-20手
# - シャッフル深度: 2以上（重複削減）
# - 目標: 1億局面以上
```

### 6.2 学習パイプライン
1. **既存ネットワークからの転移学習**
   - やねうら王互換フォーマット対応
   - 初期値として利用

2. **増分学習**
   - 新規教師データでの追加学習
   - 過学習防止（L2正則化）

3. **評価とテスト**
   - SPRT（Sequential Probability Ratio Test）
   - 目標: +10 Elo以上で採用

## 7. インターフェース要件

### 7.1 WASM API
```rust
#[wasm_bindgen]
pub struct AIEngine {
    internal: InternalEngine,
}

#[wasm_bindgen]
impl AIEngine {
    pub fn new(config: JsValue) -> Result<AIEngine, JsValue>;
    
    pub fn search(
        &mut self,
        position: JsValue,  // JSON形式の局面
        time_limit: u32,    // ミリ秒
        options: JsValue,   // 探索オプション
    ) -> JsValue;          // 最善手と評価値
    
    pub fn evaluate(&self, position: JsValue) -> JsValue;
    
    pub fn stop(&mut self);
    
    pub fn ponder_hit(&mut self);  // 先読みが的中した場合
}
```

### 7.2 既存TypeScriptとの互換性
- 現行のAIEngineInterfaceを完全実装
- JSON形式での局面・手のやり取り
- 非同期処理のサポート

## 8. 品質要件

### 8.1 パフォーマンス
- **探索速度**: 150-200万NPS（4コア時）
- **評価関数**: 0.8μs/局面以下
- **メモリ使用**: 100MB以下（置換表除く）

### 8.2 正確性
- **合法手生成**: 100%正確
- **千日手判定**: 完全実装
- **詰み探索**: 3手詰め必須、5手詰め推奨

### 8.3 テスト
- **単体テスト**: 各モジュール90%以上カバレッジ
- **統合テスト**: perftテスト必須
- **回帰テスト**: ベンチマーク位置での一貫性

## 9. 開発マイルストーン

### Phase 1: 基盤実装（3週間）
- ビットボード実装
- 合法手生成
- 基本的なαβ探索（単一スレッド）
- 簡易評価関数

### Phase 2: NNUE実装と統合（3週間）
- NNUE評価関数（HalfKP）
- 差分計算
- SIMD最適化（WASM simd128対応）
- 既存ネットワーク読み込み
- 単一スレッドでのElo測定

### Phase 3: 並列化と探索強化（2週間）
- 並列探索（Lazy SMP）
- ロックフリー置換表
- 各種枝刈り実装の調整
- 時間管理システム

### Phase 4: 最適化と品質保証（2週間）
- WASM統合とAPI実装
- パフォーマンスチューニング
- 教師データ生成
- 包括的テストとベンチマーク

## 10. リスクと対策

### 技術的リスク
1. **WASM性能**: ネイティブ比70-80%を想定
   - 対策: 重要部分のWebWorker分離

2. **メモリ制限**: ブラウザの制約
   - 対策: 動的メモリ管理、設定可能な置換表

3. **SIMD対応**: ブラウザ依存
   - 対策: フォールバック実装

### 開発リスク
1. **NNUE学習**: 計算リソース不足
   - 対策: 既存ネットワークの活用

2. **デバッグ困難**: WASM環境
   - 対策: ネイティブビルドでのテスト

## 11. 参考実装
- やねうら王（NNUE実装の参考）
- Apery Rust（Rust実装の参考）
- Fairy-Stockfish（探索技術の参考）
- tanuki-シリーズ（学習済みNNUE）

## 12. 将来的な拡張

### SFNN（Stockfish Neural Network）の検討
- **概要**: HalfKPより入力圧縮を弱め、中間層を拡張したアーキテクチャ
- **メリット**: +15-30 Elo向上の可能性
- **デメリット**: 推論速度0.6-0.75倍、ファイルサイズ3-6MB
- **導入基準**: 
  - HalfKP実装で基準性能を確立後
  - SPRT A/Bテストで速度低下25%以内かつElo+20以上
  - ネイティブ環境向けオプションとして提供
