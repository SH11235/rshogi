# SEE Integration Testing Framework - Quick Start

## 概要
探索統合テスト基盤が整備されました。この基盤により、SEE最適化の効果を定量的に測定できます。

## ファイル構成

```
tests/
├── tactical_positions.yaml      # 戦術局面データベース
└── test_search_integration.rs   # 統合テストスイート

benches/
└── see_integration_bench.rs     # パフォーマンスベンチマーク

docs/
└── see_integration_testing.md   # 詳細ドキュメント
```

## 基本的な使い方

### 1. ベースライン測定（最適化前）

```bash
# 現在の性能を記録
cargo bench --bench see_integration_bench -- --save-baseline baseline_main
```

### 2. 最適化実装

最適化を実装...

### 3. 効果測定

```bash
# ベースラインと比較
cargo bench --bench see_integration_bench -- --baseline baseline_main
```

### 4. 正確性確認

```bash
# 統合テストで動作確認
cargo test --test test_search_integration -- --nocapture
```

## 測定項目

### パフォーマンス指標
- **SEE計算時間**: < 200ns（ピン検出付きで < 250ns）
- **探索ノード数**: ベースラインからの改善率
- **Beta cutoff率**: > 65%
- **First move cutoff率**: > 35%

### 正確性指標
- 評価値の一致
- PV（主要変化）の安定性
- 戦術局面での最善手発見

## 次のステップ

1. **X-ray部分ピン更新の実装**
   - `update_pins_after_capture()`メソッドの追加
   - 差分更新による高速化

2. **ビットボード最適化**
   - pop_lsb()呼び出しの削減
   - キャッシュの活用

3. **早期終了の強化**
   - 閾値ベースの枝刈り
   - 余剰価値テーブルの導入

詳細は `docs/see_integration_testing.md` を参照してください。