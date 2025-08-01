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

#### 1. ~~**エンジンコントローラー（最優先）**~~ ✅ 完了
**ファイル**: `crates/engine-core/src/engine/controller.rs`

**実施内容**:
- 旧モジュールの使用を削除し、統合searcherを使用：
  ```rust
  use crate::search::unified::{UnifiedSearcher};
  ```
- 各エンジンタイプに対応する統合searcherの型エイリアスを定義
- `Engine::search()`メソッドを統合searcherで実装

#### 2. ~~**SearchStackの依存関係問題**~~ ✅ 完了
**影響ファイル**: 
- `crates/engine-core/src/movegen/move_picker.rs`
- `src/ai/test_move_picker_integration.rs`
- `src/ai/test_move_picker_comprehensive.rs`

**実施内容**:
- `SearchStack`を`search/types.rs`に移動し、共通型として定義
- `search_enhanced`からは後方互換性のために再エクスポート
- `MovePicker`と関連テストファイルのimportを更新
- これにより、統合searcherと`MovePicker`の両方から`SearchStack`を使用可能に

#### 3. ~~**ベンチマーク・テストの更新**~~ ✅ 完了
以下のファイルを統合searcherに更新完了：

| ファイル | 変更内容 |
|---------|---------|
| `src/benchmark.rs` | `UnifiedSearcher<MaterialEvaluator, true, false, 8>`に更新 |
| `tests/test_search_integration.rs` | `UnifiedSearcher<MaterialEvaluator, true, true, 16>`に更新 |
| `benches/search_benchmarks.rs` | 基本・拡張両方の設定で統合searcherを使用 |
| `benches/see_integration_bench.rs` | `UnifiedSearcher<MaterialEvaluator, true, true, TT_SIZE>`に更新 |

**実施内容**:
- 旧searcherのimportを削除
- 統合searcherの適切な型パラメータで設定
- `SearchLimitsBuilder`を使用したAPI更新
- 不要な`Arc`ラッパーを削除

#### 4. ~~**性能関連のTODO**~~ ✅ 完了
1. **TimeControl::Infiniteでの性能問題** ✅ 修正済み
   - 場所: `unified/mod.rs:136`
   - 解決: 深さ制限がある場合もTimeManagerを作成
   - 結果: depth 5の探索が25秒 → 1.9秒に改善

2. **イベントポーリング間隔** ✅ 修正済み
   - 場所: `unified/core/mod.rs:49`
   - 解決: 深さ制限のみの探索では64ノードごとにチェック
   - 結果: レスポンシブな終了を維持しつつ高速化

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

## 追加の技術的考察

### キラームーブ管理の二重実装
現在、キラームーブ管理が2箇所で実装されています：

1. **SearchStack（MovePicker用）**
   - `search/types.rs`で定義
   - `MovePicker`で使用
   - 追加の探索情報も保持（static_eval、current_move等）

2. **MoveOrdering（統合searcher用）**
   - `search/unified/ordering/mod.rs`で実装
   - 統合searcherで独立して使用
   - キラームーブのみに特化

この二重実装は機能的には問題ありませんが、以下の点で改善の余地があります：
- コードの重複
- SearchStackの追加情報（static_eval等）が統合searcherで未活用
- 将来的なメンテナンスの複雑化

ただし、現状では両実装とも正しく動作しており、統合は優先度低と判断します。

## まとめ
統合エンジンの基本実装は完了していますが、実際の使用箇所の移行が未完了です。特に`controller.rs`の更新が最優先事項であり、これにより統合エンジンが実際に使用されるようになります。