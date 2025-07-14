# Rust/WASM AI エンジン実装計画

## 現状分析

### 1. 現在のAI実装（TypeScript）

#### 構成
- `packages/core/src/ai/` - AI実装のコア
  - `engine.ts` - AIエンジンのメインクラス
  - `evaluation.ts` - 局面評価関数
  - `search.ts` - 探索アルゴリズム（反復深化、α-β探索）
  - `openingData.ts` - 定跡データ（TypeScript）
  - `openingBookInterface.ts` - 定跡インターフェース

#### 特徴
- **探索深度**: 2-8（難易度による）
- **時間制限**: 1-30秒
- **評価関数**: 駒価値 + 位置評価 + 王の安全性 + 機動力 + 連携
- **データ構造**: オブジェクト型（`Record<string, Piece | null>`）
- **ハッシュ**: `JSON.stringify`（非効率）

#### 問題点
- パフォーマンス（1秒あたり数万局面程度）
- 非効率なデータ構造
- GCによる遅延
- 基本的な最適化技術の欠如

### 2. 既存のRust実装

#### 構成
- `packages/rust-core/` - 定跡データベースとWebRTC
  - 定跡読み込み機能のみ
  - AIエンジンは未実装

## 実装計画

### Phase 1: Rust AI基盤構築（2週間）

#### 1.1 プロジェクト構造の整理
- `packages/rust-core/src/ai/` ディレクトリ作成
- モジュール分割：
  - `board.rs` - ビットボード表現
  - `move_gen.rs` - 合法手生成
  - `evaluation.rs` - 局面評価
  - `search.rs` - 探索エンジン
  - `types.rs` - 共通型定義

#### 1.2 基本データ構造の実装
```rust
// ビットボード表現
pub struct BitBoard {
    black_pieces: [u128; 7], // 各駒種（玉、飛、角、金、銀、桂、香、歩）
    white_pieces: [u128; 7],
    hands: [u8; 14],         // 持ち駒
}

// 移動表現
pub struct Move {
    from: Option<Square>,    // None = 駒打ち
    to: Square,
    piece_type: PieceType,
    promote: bool,
    capture: Option<PieceType>,
}
```

#### 1.3 合法手生成の実装
- ビットボードベースの高速化
- 駒ごとの移動パターン
- 王手判定の最適化

### Phase 2: 高性能評価関数の実装（3-4週間）

#### 2.1 評価関数の選択と設計

##### 選択肢の検討
1. **従来型評価関数（非推奨）**
   - 現在の実装のような手作り評価関数
   - 棋力に限界あり（アマ二段程度）

2. **KPP/KPPT評価関数**
   - 3駒関係を評価
   - 実装は比較的シンプル
   - 棋力：アマ四段〜五段程度

3. **NNUE評価関数（推奨）**
   - 効率的に更新可能なニューラルネットワーク
   - CPUで高速動作
   - 棋力：プロレベル

##### 実装方針：段階的アプローチ
1. **Phase 2a**: シンプルなKPP実装（1週間）
   - 基本的な3駒関係評価
   - 学習済みパラメータの利用検討
   
2. **Phase 2b**: NNUE実装（2-3週間）
   - halfKP型の実装
   - 差分計算の実装
   - SIMD最適化

#### 2.2 KPP評価関数の実装（Phase 2a）

```rust
// KPP評価関数の基本構造
pub struct KPPEvaluator {
    // King-Piece-Piece の評価値テーブル
    // 玉位置(81) × 駒1(約1500) × 駒2(約1500)
    kpp_table: Vec<i16>,  // 約360MB
    
    // King-Piece の評価値テーブル
    kp_table: Vec<i16>,   // 約240KB
}

impl KPPEvaluator {
    pub fn evaluate(&self, position: &Position) -> i32 {
        // 3駒関係の評価値を累積
        // 差分計算で高速化
    }
}
```

#### 2.3 NNUE評価関数の実装（Phase 2b）

##### 2.3.1 アーキテクチャ
- **標準構成**: halfKP_256x2-32-32
  - 入力層：halfKP特徴量（約80,000次元）
  - 中間層1：256ユニット×2（先手視点・後手視点）
  - 中間層2：32ユニット
  - 中間層3：32ユニット
  - 出力層：1（評価値）

##### 2.3.2 実装の要点
```rust
pub struct NNUEEvaluator {
    // ネットワーク構造
    feature_transformer: HalfKPFeatureTransformer,
    hidden_layers: Vec<Layer>,
    
    // 差分更新用のキャッシュ
    accumulator: Accumulator,
}

// halfKP特徴量の抽出
pub struct HalfKPFeatureTransformer {
    weights: Vec<i16>,  // 約40MB
    biases: Vec<i32>,
}

// 差分計算の最適化
impl NNUEEvaluator {
    pub fn update_incremental(&mut self, move: &Move) {
        // 移動した駒に関連する特徴量のみ更新
        // SIMD命令を使用した高速化
    }
}
```

##### 2.3.3 SIMD最適化
```rust
#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

unsafe fn update_accumulator_avx2(
    accumulator: &mut [i32],
    weights: &[i16],
    indices: &[usize]
) {
    // AVX2命令を使用した並列処理
    // VPADDW, VPSUBW などの活用
}
```

#### 2.4 学習済みパラメータの活用

##### 選択肢
1. **新規学習**
   - 大量の棋譜データが必要（数億局面）
   - 計算リソースが膨大（GPU必須）
   - 時間がかかる（数週間〜数ヶ月）

2. **既存パラメータの利用（推奨）**
   - オープンソースの学習済みモデル
   - 互換性のあるフォーマットに変換
   - ライセンスの確認が必要

##### 利用可能なリソース
- **tanuki-**シリーズ：最強クラスの無料NNUE
- **やねうら王**の学習済みパラメータ（ライセンス要確認）
- **独自フォーマットへの変換ツール**の実装

#### 2.5 評価関数のテストとチューニング

##### 性能指標
- **計算速度**: 1局面あたり1μs以下（100万局面/秒）
- **メモリ使用量**: 100MB以下
- **精度**: 既存の強豪エンジンとの一致率

##### ベンチマーク
```rust
#[cfg(test)]
mod benchmarks {
    use criterion::{black_box, criterion_group, Criterion};
    
    fn bench_evaluate(c: &mut Criterion) {
        c.bench_function("nnue_evaluate", |b| {
            b.iter(|| {
                evaluator.evaluate(black_box(&position))
            });
        });
    }
}

### Phase 3: 探索エンジンの実装（2週間）

#### 3.1 基本探索
- α-β探索
- 反復深化
- 置換表（Zobristハッシュ）
- 手の並び替え（ムーブオーダリング）

#### 3.2 高度な探索技術
- Null Move Pruning
- Late Move Reduction
- Killer Move Heuristic
- History Heuristic
- Futility Pruning

### Phase 4: WASM統合（1週間）

#### 4.1 WASM バインディング
```rust
#[wasm_bindgen]
pub struct AIEngine {
    engine: InternalEngine,
}

#[wasm_bindgen]
impl AIEngine {
    #[wasm_bindgen(constructor)]
    pub fn new(difficulty: &str) -> Self;
    
    pub fn calculate_best_move(
        &mut self, 
        board: &str,      // JSON形式
        hands: &str,      // JSON形式
        current_player: &str,
        move_history: &str
    ) -> String;         // JSON形式のMove
    
    pub fn evaluate_position(
        &self,
        board: &str,
        hands: &str,
        player: &str
    ) -> String;         // JSON形式のEvaluation
}
```

#### 4.2 TypeScriptインターフェース
- 既存のAIEngineInterfaceを維持
- 内部実装をWASMに置き換え
- JSON⇔内部表現の変換層

### Phase 5: 既存コードの整理（3日）

#### 5.1 削除対象ファイル
- `packages/core/src/ai/engine.ts` の実装部分（インターフェースは保持）
- `packages/core/src/ai/evaluation.ts`
- `packages/core/src/ai/search.ts`

#### 5.2 保持・修正対象
- `packages/core/src/ai/openingBookInterface.ts` （インターフェース）
- `packages/web/src/services/ai/aiService.ts` （軽微な修正）
- `packages/web/src/workers/aiWorker.ts` （WASM呼び出しに変更）
- `packages/core/src/types/ai.ts` （型定義）

### Phase 6: テストとチューニング（1週間）

#### 6.1 パフォーマンステスト
- 1秒あたりの探索局面数（NPS: Nodes Per Second）
- メモリ使用量
- 探索深度と時間の関係

#### 6.2 棋力テスト
- 定跡局面での応手
- 中盤での形勢判断
- 終盤での詰み探索
- 既存エンジンとの対戦

## 期待される成果

### パフォーマンス向上
- **現在**: 約5万局面/秒
- **KPP実装時**: 50万局面/秒以上（10倍）
- **NNUE実装時**: 100万局面/秒以上（20倍）

### 棋力向上
- **現在**: アマ初段〜二段程度
- **KPP実装時**: アマ四段〜五段程度
- **NNUE実装時**: アマ六段〜プロレベル

### その他の改善
- メモリ効率の向上（GCなし）
- 予測可能な応答時間
- より深い探索深度

## 実装の優先順位

1. **MVP（最小実装）** - 5週間
   - Phase 1: 基本的なビットボード実装（2週間）
   - Phase 2a: KPP評価関数の実装（1週間）
   - Phase 3: 基本的なα-β探索（2週間）
   - Phase 4: WASM統合（1週間）
   
   **MVP達成時の棋力**: アマ四段程度

2. **強化フェーズ** - 3-4週間
   - Phase 2b: NNUE評価関数の実装（2-3週間）
   - 高度な探索技術の追加（1週間）
   - パフォーマンスチューニング
   
   **強化フェーズ後の棋力**: アマ六段〜プロレベル

3. **拡張フェーズ** - 2週間
   - 詰み探索の強化
   - 学習済みパラメータの最適化
   - UIへの詳細情報表示（評価値グラフ、読み筋表示）
   - マルチスレッド対応の検討

## リスクと対策

### 1. 実装工数
**リスク**: 予定より長引く可能性（特にデバッグ）
**対策**: 
- 段階的リリース
- MVPの明確な定義
- 既存実装との並行運用

### 2. 互換性
**リスク**: 既存インターフェースとの不整合
**対策**: 
- アダプターパターンで段階的移行
- 十分なテストカバレッジ

### 3. デバッグの困難さ
**リスク**: Rust/WASMのデバッグは困難
**対策**: 
- 充実したログ出力
- 単体テストの充実
- ベンチマークテスト

## 成功指標

1. **パフォーマンス**
   - 1秒で100万局面以上の探索
   - 応答時間の安定性（±10%以内）

2. **棋力**
   - 既存実装に100戦して90勝以上
   - アマ三段相当のベンチマークをクリア

3. **保守性**
   - 既存インターフェースの完全互換
   - ドキュメントの充実
   - テストカバレッジ80%以上

### 4. 評価関数の複雑性
**リスク**: NNUE実装は高度な技術を要求
**対策**:
- 段階的実装（KPP → NNUE）
- 既存実装の参考（やねうら王のソースコード）
- コミュニティサポートの活用

### 5. 学習済みパラメータ
**リスク**: 適切なパラメータの入手・変換
**対策**:
- オープンソースモデルの調査
- フォーマット変換ツールの開発
- 独自学習環境の構築（将来的に）

## 参考資料

### 技術文献
- [次世代の将棋思考エンジン、NNUE関数を学ぼう（Qhapaq）](https://qhapaq.hatenablog.com/entry/2018/06/02/221612)
- [NNUE評価関数、新しい時代の夜明け（やねうら王）](https://yaneuraou.yaneu.com/2022/06/09/nnue-eval-function-the-dawn-of-a-new-era/)
- [nnue-pytorch（将棋AI評価関数学習器）](https://github.com/nodchip/nnue-pytorch)

### 実装参考
- [やねうら王 GitHub](https://github.com/yaneurao/YaneuraOu)
- [Apery](https://github.com/HiraokaTakuya/apery_rust)
- [tanuki- NNUE評価関数](https://github.com/nodchip/tanuki-)

### 評価関数データ
- tanuki- シリーズ「Hao」「Li」（最強の無料NNUE）
- やねうら王付属の評価関数（ライセンス要確認）

## 次のステップ

1. このドキュメントのレビューと承認
2. Phase 1の詳細設計
   - ビットボード表現の具体的な実装方針
   - Rust側のインターフェース設計
3. 開発環境の準備
   - Rustツールチェーン（nightly版推奨）
   - ベンチマーク環境
   - SIMD対応の確認
4. 評価関数パラメータの調査
   - 利用可能な学習済みモデルのライセンス確認
   - フォーマット仕様の調査
5. 実装開始

---

*最終更新日: 2025年1月*