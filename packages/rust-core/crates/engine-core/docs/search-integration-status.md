# 探索エンジン統合状況レポート

## 概要
`search_basic`と`search_enhanced`を`unified`探索エンジンに統合するタスクの実施状況と、未実装・要修正箇所の調査結果をまとめます。

調査日: 2025-08-01

## 統合の現状

### ✅ 完了済み
1. **統合探索エンジンの基本実装**
   - `crates/engine-core/src/search/unified/` に統合エンジン実装完了
   - コンパイル時機能設定（`USE_TT`, `USE_PRUNING`, `TT_SIZE_MB`）実装済み
   - 型エイリアス定義済み：
     - `BasicSearcher` = `UnifiedSearcher<MaterialEvaluator, true, false, 8>`
     - `EnhancedSearcher<E>` = `UnifiedSearcher<E, true, true, 16>`

2. **機能の統合**
   - アルファベータ探索
   - 反復深化
   - 静止探索（Quiescence Search）
   - トランスポジションテーブル
   - ヌルムーブ枝刈り
   - Late Move Reduction (LMR)
   - キラームーブ（独自実装）
   - ムーブオーダリング

### ❌ 未実装・要修正箇所

#### 1. **エンジンコントローラー（最優先）**
**ファイル**: `crates/engine-core/src/engine/controller.rs`

**問題点**:
- 依然として旧モジュールを使用：
  ```rust
  use crate::search::search_basic::Searcher;
  use crate::search::search_enhanced::EnhancedSearcher;
  ```
- `Engine::search()`メソッドが旧searcherを使用して探索を実行

**必要な対応**:
- `UnifiedSearcher`を使用するように修正
- エンジンタイプに応じた適切な型パラメータの設定

#### 2. **SearchStackの依存関係問題**
**影響ファイル**: 
- `crates/engine-core/src/movegen/move_picker.rs`
- `src/ai/test_move_picker_integration.rs`
- `src/ai/test_move_picker_comprehensive.rs`

**問題点**:
- `MovePicker`が`search_enhanced::SearchStack`に依存
- 統合エンジンは`SearchStack`を使用せず、独自の`MoveOrdering`でキラームーブを管理

**必要な対応**:
- オプション1: `SearchStack`を共通モジュールに移動
- オプション2: `MovePicker`を統合エンジンの`MoveOrdering`に合わせて修正
- オプション3: `SearchStack`のインターフェースを簡素化

#### 3. **ベンチマーク・テストの更新**
以下のファイルが旧searcherを使用：

| ファイル | 使用している旧モジュール |
|---------|---------------------|
| `benches/search_benchmarks.rs` | `search_basic::Searcher`, `search_enhanced::EnhancedSearcher` |
| `tests/test_search_integration.rs` | `search_enhanced::EnhancedSearcher` |
| `src/benchmark.rs` | `search_basic::Searcher` |
| `benches/see_integration_bench.rs` | `search_enhanced::EnhancedSearcher` |

#### 4. **性能関連のTODO**
1. **TimeControl::Infiniteでの性能問題**
   - 場所: `unified/mod.rs:133-136`
   - 問題: `TimeManager`が作成されない場合、深さ制限のみの探索が遅い
   - 例: depth 5の探索に25秒かかる

2. **イベントポーリング間隔**
   - 場所: `unified/core/mod.rs:43-47`
   - 問題: `TimeManager`無しでのポーリング間隔が1024ノード固定
   - 影響: 停止条件のチェック頻度が低い

## 統合の影響範囲

### 直接的な影響
- エンジンコントローラー
- ベンチマークスイート
- 統合テスト
- ムーブピッカー

### 間接的な影響
- USIプロトコル実装（変更不要）
- 評価関数インターフェース（変更不要）
- 時間管理モジュール（変更不要）

## 推奨される対応順序

1. **Phase 1: コントローラーの更新**（最優先）
   - `controller.rs`を`UnifiedSearcher`使用に更新
   - 各エンジンタイプに対応する統合エンジンの設定

2. **Phase 2: SearchStack問題の解決**
   - `SearchStack`の扱いを決定
   - `MovePicker`の修正または共通化

3. **Phase 3: テスト・ベンチマークの更新**
   - 各テストファイルを統合エンジン使用に更新
   - ベンチマークの比較可能性を維持

4. **Phase 4: 性能最適化**
   - TimeControl::Infiniteでの性能改善
   - イベントポーリングの最適化

## 技術的な注意点

1. **後方互換性**
   - `search_enhanced::SearchStack`は「後方互換性のための再エクスポート」として残されている
   - 完全な削除は依存関係の解決後に実施

2. **型パラメータ**
   - 統合エンジンはconst genericsを使用
   - コンパイル時に機能が決定されるため、実行時オーバーヘッドなし

3. **メモリ使用量**
   - BasicSearcher: ~20MB（TT 8MB）
   - EnhancedSearcher: ~36MB（TT 16MB）

## まとめ
統合エンジンの基本実装は完了していますが、実際の使用箇所の移行が未完了です。特に`controller.rs`の更新が最優先事項であり、これにより統合エンジンが実際に使用されるようになります。